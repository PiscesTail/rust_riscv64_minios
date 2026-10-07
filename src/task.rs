//! 任务管理与「轮转（Round-Robin）抢占式调度器」。
//!
//! 这是本项目在 trap + 时钟中断之上迈出的下一步：让 CPU 能在多个
//! **内核线程** 之间来回切换，营造出“同时运行”的假象。
//!
//! ## 核心思路（务必结合 `trap.rs` 一起看）
//!
//! 每个任务都有自己的 **内核栈**，并且约定：任务一旦陷入，它的
//! `TrapContext` 就保存在自己内核栈的某个位置。调度器要做的事非常简单：
//!
//! ```text
//!   时钟中断 -> trap_handler -> on_timer_tick
//!                                   │ 时间片用完？
//!                        否 ────────┘──────── 是
//!                        │                     │
//!              返回当前任务现场          1. 记下当前任务的现场地址
//!                        │              2. 挑出下一个任务
//!                        │              3. 返回下一个任务的现场地址
//!                        ▼                     ▼
//!                     __restore ------------> __restore  (从新现场 sret)
//! ```
//!
//! 关键点：**“保存现场”由 `__alltraps` 自动完成**，调度器只需要把
//! 这个现场地址记在任务元数据里，下次切回来时再交还给 `__restore` 即可。
//!
//! ## 为什么任务栈要静态分配？
//!
//! 内核还没有堆分配器（allocator），所以任务栈只能放在静态内存里。
//! 这里用 `[Stack; MAX_TASKS]` 一次性预留，简单、可靠，也方便初学者建立
//! “栈就是一块内存”的直觉。等以后实现了页帧分配器，就可以动态创建栈了。

use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sbi;
use crate::trap;

/// 最多同时支持的任务数（固定数组，无需堆分配）。
pub const MAX_TASKS: usize = 4;

/// 每个任务的内核栈大小（16 KiB）。QEMU 内存 128M，绰绰有余。
const STACK_SIZE: usize = 16 * 1024;

/// 每个任务一次连续运行的时间片（单位：时钟 tick）。
/// 设为 1 表示「每来一次时钟中断就换人」，调度效果最直观。
const TASK_SLICE: usize = 1;

/// 演示用：发生多少次任务切换后关机，方便 `make run` 自动结束。
const SWITCH_LIMIT: usize = 8;

/// 任务状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// 就绪，等待被调度。
    Ready,
    /// 正在运行（同一时刻只会有一个）。
    Running,
    /// 已退出，不再参与调度。
    Exited,
}

/// 任务栈：用 `repr(align(16))` 保证栈顶满足 RISC-V ABI 的 16 字节对齐要求。
#[repr(align(16))]
struct Stack([u8; STACK_SIZE]);

/// 任务的元数据（不含栈本身，栈单独存放在 `TaskManager::stacks`）。
///
/// 实现 `Copy` 是为了在调度时能方便地“复制一份”来读取字段，
/// 从而避开 Rust 借用检查器的同时可变借用冲突。
#[derive(Clone, Copy)]
struct TaskMeta {
    /// 任务编号（= 数组下标）。
    id: usize,
    /// 便于打印的任务名。
    name: &'static str,
    /// 入口函数地址（仅用于展示，调度并不需要它）。
    entry: usize,
    /// 当前状态。
    state: TaskState,
    /// 本时间片还剩多少 tick。
    ticks_left: usize,
    /// 最近一次陷入时 `TrapContext` 在任务栈上的地址。
    /// 调度器切走/切回任务全靠它。
    trap_cx_ptr: *mut trap::TrapContext,
}

/// 全局唯一的任务管理器。
struct TaskManager {
    /// 所有任务的内核栈。放在最前面是为了对齐；`Stack` 自带 16 字节对齐。
    stacks: [Stack; MAX_TASKS],
    /// 每个任务的元数据；未使用的位置为 `None`。
    tasks: [Option<TaskMeta>; MAX_TASKS],
    /// 已创建的任务数量。
    num: usize,
    /// 当前正在运行的任务下标。
    current: usize,
}

impl TaskManager {
    /// 常量构造：整个结构体全零，会被放进 `.bss`，由 `_start` 统一清零。
    const fn new() -> Self {
        Self {
            // 内联常量表达式 `const { ... }` 保证在静态存储里原地初始化，
            // 不会在栈上生成 4 * 16 KiB 的临时数组。
            stacks: [const { Stack([0; STACK_SIZE]) }; MAX_TASKS],
            tasks: [None; MAX_TASKS],
            num: 0,
            current: 0,
        }
    }

    /// 返回第 `i` 个任务栈的栈顶地址（向下取整到 16 字节对齐）。
    fn stack_top(&self, i: usize) -> usize {
        let base = self.stacks[i].0.as_ptr() as usize;
        (base + STACK_SIZE) & !0xf
    }

    /// 创建一个新任务：在它自己的栈顶写入初始 `TrapContext`。
    fn spawn(&mut self, name: &'static str, entry: usize) {
        assert!(self.num < MAX_TASKS, "too many tasks (max = {})", MAX_TASKS);

        let id = self.num;
        let top = self.stack_top(id);
        // 把初始现场放在栈顶下方 sizeof(TrapContext) 处，
        // 这样 __restore 执行完 `addi sp, sp, 34*8` 后 sp 正好等于栈顶。
        let ctx_ptr = (top - core::mem::size_of::<trap::TrapContext>()) as *mut trap::TrapContext;

        unsafe {
            // 直接写入这块内存，构造任务首次运行所需的寄存器现场。
            ctx_ptr.write(trap::TrapContext::init(entry));
        }

        self.tasks[id] = Some(TaskMeta {
            id,
            name,
            entry,
            state: TaskState::Ready,
            ticks_left: TASK_SLICE,
            trap_cx_ptr: ctx_ptr,
        });
        self.num += 1;
        crate::println!(
            "[task {}] '{}' ready, entry=0x{:x}, stack_top=0x{:x}",
            id,
            name,
            entry,
            top
        );
    }

    /// 轮转选择下一个可运行任务：从 current 的下一个开始，绕一圈找 Ready/Running。
    fn pick_next(&self) -> usize {
        let mut next = self.current;
        for _ in 0..self.num {
            next = (next + 1) % self.num;
            if let Some(t) = self.tasks[next] {
                if t.state != TaskState::Exited {
                    return next;
                }
            }
        }
        // 没有别的任务可用，就继续跑当前的。
        self.current
    }
}

/// 把 `UnsafeCell` 包一层并声明 `Sync`，以便放进 `static`。
///
/// # 安全性说明
/// 裸机单 hart 运行，且调度器只在 trap 处理期间（硬件已关闭中断）访问它；
/// `spawn`/`run` 也都在开中断之前调用。因此不存在并发访问。
struct ManagerCell(UnsafeCell<TaskManager>);
unsafe impl Sync for ManagerCell {}

static MANAGER: ManagerCell = ManagerCell(UnsafeCell::new(TaskManager::new()));

/// 取得全局任务管理器的可变引用。
///
/// # Safety 调用约定
/// 只能在「单线程且中断关闭」的上下文中调用（trap 处理期间，或启动阶段）。
#[inline]
fn manager() -> &'static mut TaskManager {
    unsafe { &mut *MANAGER.0.get() }
}

/// 已经发生的任务切换次数（仅用于演示打印与自动关机）。
static SWITCHES: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// 对外接口
// ---------------------------------------------------------------------------

/// 创建一个内核线程任务。必须在开启调度之前调用。
pub fn spawn(name: &'static str, entry: extern "C" fn() -> !) {
    manager().spawn(name, entry as usize);
}

/// 启动第一个任务并从此进入调度循环，永不返回。
///
/// 实现方式是直接“恢复”任务 0 的初始现场：`__restore` 会把 sp 切到它的
/// 内核栈，并 `sret` 跳到入口函数；由于初始 `sstatus` 里 SPIE=1，
/// 任务一开始运行就是开中断的，随时可能被时钟中断抢占。
pub fn run() -> ! {
    let m: &mut TaskManager = manager();
    assert!(m.num > 0, "no task to run");

    m.current = 0;
    m.tasks[0].as_mut().unwrap().state = TaskState::Running;

    let first: TaskMeta = m.tasks[0].unwrap();
    crate::println!(
        "[sched  0] boot -> {} (id={}, entry=0x{:x})   (first task)",
        first.name,
        first.id,
        first.entry
    );
    unsafe { trap::restore_to(first.trap_cx_ptr as usize) }
}

/// 时钟中断回调：消耗时间片，必要时切换到下一个任务。
///
/// 参数 `cx_ptr` 是本次陷入时保存的当前任务现场地址；
/// 返回值是本次 `sret` 应当恢复的现场地址（可能已换人）。
pub fn on_timer_tick(cx_ptr: *mut trap::TrapContext) -> *mut trap::TrapContext {
    let m = manager();
    let cur = m.current;

    // 1) 记下当前任务的现场，将来切回它时才能从这里继续。
    m.tasks[cur].as_mut().unwrap().trap_cx_ptr = cx_ptr;

    // 2) 消耗时间片。
    {
        let t = m.tasks[cur].as_mut().unwrap();
        if t.ticks_left > 0 {
            t.ticks_left -= 1;
        }
        if t.ticks_left > 0 {
            // 时间片还没用完，继续运行当前任务，现场不变。
            return cx_ptr;
        }
        // 用完就续满，供下一次轮到它时使用。
        t.ticks_left = TASK_SLICE;
    }

    // 3) 选择下一个任务。
    let next = m.pick_next();
    if next == cur {
        // 只有自己，无需切换。
        return cx_ptr;
    }

    // 4) 更新状态并完成切换（本质上只是“返回下一个任务的现场地址”）。
    let cur_name = m.tasks[cur].unwrap().name;
    m.tasks[cur].as_mut().unwrap().state = TaskState::Ready;
    let next_meta = m.tasks[next].unwrap();
    m.tasks[next].as_mut().unwrap().state = TaskState::Running;
    m.current = next;

    let n = SWITCHES.fetch_add(1, Ordering::Relaxed) + 1;
    crate::println!("[sched {:>2}] {} -> {}", n, cur_name, next_meta.name);

    if n >= SWITCH_LIMIT {
        crate::println!("[OK] Round-robin preemptive scheduling works. Shutting down ...");
        sbi::shutdown();
    }

    next_meta.trap_cx_ptr
}

/// 让当前任务主动让出 CPU（协作式调度）。
///
/// 本项目用的是时钟中断抢占，所以暂时没有任务调用它；保留此函数是为了
/// 让你看到“协作式让权”的实现方式——制造一次 S 态软件中断即可。
#[allow(dead_code)]
pub fn yield_now() {
    const S_SOFTWARE_INTERRUPT: usize = 1;
    unsafe {
        asm!("csrw sip, {}", in(reg) (1 << S_SOFTWARE_INTERRUPT), options(nomem, nostack));
    }
}

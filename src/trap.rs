//! Trap（异常 / 中断）处理基础设施。
//!
//! RISC-V 在陷入时硬件只做最少的事：把返回地址存入 `sepc`、原因存入 `scause`、
//! 附加值存入 `stval`，然后跳到 `stvec` 指向的地址。寄存器现场的保存/恢复
//! 必须由软件完成——这正是本模块的核心：
//!
//! ```text
//!   [S 态代码] --trap--> __alltraps (汇编) --call--> trap_handler (Rust)
//!        ▲                                                  │
//!        └──────────────── sret ──── __restore ◄────────────┘
//! ```
//!
//! ## 与调度器的衔接（本项目新增）
//!
//! `trap_handler` 不再直接返回 `()`，而是返回一个 **`TrapContext` 指针**：
//! “这次 `sret` 应该从哪份寄存器现场恢复”。平时它就是当前任务的现场地址；
//! 当时钟中断触发任务切换时，它会变成 **下一个任务** 的现场地址。
//! 汇编里的 `__restore` 会用这个返回值重设 `sp` 并从对应现场恢复，于是
//! 一次普通的 trap 返回，就变成了任务切换。这就是“抢占式调度”的实现原理。

use core::arch::{asm, global_asm};

use crate::sbi;

/// 陷入时保存在内核栈上的寄存器现场。
///
/// 字段顺序/偏移必须与下方 `__alltraps` 汇编严格一致：
/// `x[0..32]` 各占 8 字节，随后是 `sstatus`、`sepc`，共 34 * 8 = 272 字节。
///
/// 说明：
/// - `x[0]` 恒为 0，汇编里也跳过它，但保留槽位可以让下标 = 寄存器号，方便阅读。
/// - `x[2] = sp` 也不显式保存/恢复，而是靠汇编里的 `addi sp, sp, ±34*8` 隐式处理。
#[repr(C)]
pub struct TrapContext {
    /// 32 个通用寄存器 x0 ~ x31。
    pub x: [usize; 32],
    /// 陷入时的 `sstatus`，决定 `sret` 后回到哪个特权级、是否开中断。
    pub sstatus: usize,
    /// 陷入时的 `sepc`，`sret` 会跳回这里。
    pub sepc: usize,
}

// 编译期断言：确保 Rust 结构体布局与汇编里的固定偏移一致。
// 一旦有人不小心改了字段，编译就会失败，而不是运行期出现诡异 bug。
const _: () = assert!(core::mem::size_of::<TrapContext>() == 34 * 8);

impl TrapContext {
    /// 为 **新任务** 构造一份初始现场。
    ///
    /// 当调度器第一次切到这个任务时，`__restore` 会加载这份现场并 `sret`，
    /// 效果等价于“从 `entry` 函数的第一条指令开始执行，且处于 S 态、中断开启”。
    ///
    /// `sstatus` 关键位（RV64 S 态）：
    /// - `SPP`  (bit 8)：`sret` 后回到的特权级。1 = S 态，0 = U 态。内核线程填 1。
    /// - `SPIE` (bit 5)：`sret` 会把 `SIE ← SPIE`，所以填 1 表示“返回后开中断”。
    /// - `SIE`  (bit 1) ：当前是否开中断。构造时先清 0，等 `sret` 再打开。
    ///
    /// 注意：`x[2]`（sp）不需要在这里设置。因为 `__restore` 会把 `sp` 设为
    /// 这份现场的首地址，再执行 `addi sp, sp, 34*8` 得到栈顶——所以只要把
    /// 初始现场放在任务栈顶下方 272 字节处，任务启动时 `sp` 自然就是栈顶。
    pub fn init(entry: usize) -> Self {
        const SPP: usize = 1 << 8;
        const SPIE: usize = 1 << 5;
        const SIE: usize = 1 << 1;

        let mut sstatus: usize;
        // 读取当前 sstatus，保留固件设置的其他位，只覆盖我们关心的几位。
        unsafe { asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack)) };
        sstatus = (sstatus & !(SPP | SIE)) | SPP | SPIE;

        Self {
            x: [0; 32],
            sstatus,
            sepc: entry,
        }
    }
}

// trap 入口：保存全部通用寄存器 + `sstatus`/`sepc`，调用 Rust 处理函数，
// 然后按返回值恢复现场并 `sret`（可能切换到别的任务）。
global_asm!(
    r#"
    .section .text
    .align 4                 # stvec 直接模式要求入口 4 字节对齐
    .globl __alltraps
__alltraps:
    # 在当前内核栈上向下开辟 34*8 = 272 字节，用来放 TrapContext。
    addi sp, sp, -34*8

    # 保存 x1、x3..x31（x0 恒为 0，跳过；x2=sp 稍后单独算）
    sd   x1,   1*8(sp)
    sd   x3,   3*8(sp)
    sd   x4,   4*8(sp)
    sd   x5,   5*8(sp)
    sd   x6,   6*8(sp)
    sd   x7,   7*8(sp)
    sd   x8,   8*8(sp)
    sd   x9,   9*8(sp)
    sd   x10, 10*8(sp)
    sd   x11, 11*8(sp)
    sd   x12, 12*8(sp)
    sd   x13, 13*8(sp)
    sd   x14, 14*8(sp)
    sd   x15, 15*8(sp)
    sd   x16, 16*8(sp)
    sd   x17, 17*8(sp)
    sd   x18, 18*8(sp)
    sd   x19, 19*8(sp)
    sd   x20, 20*8(sp)
    sd   x21, 21*8(sp)
    sd   x22, 22*8(sp)
    sd   x23, 23*8(sp)
    sd   x24, 24*8(sp)
    sd   x25, 25*8(sp)
    sd   x26, 26*8(sp)
    sd   x27, 27*8(sp)
    sd   x28, 28*8(sp)
    sd   x29, 29*8(sp)
    sd   x30, 30*8(sp)
    sd   x31, 31*8(sp)

    # 陷入前的 sp 等于「当前 sp + 272」，补存进 x2 槽位，方便调试查看。
    addi t0, sp, 34*8
    sd   t0,   2*8(sp)

    # 保存控制状态寄存器
    csrr t0, sstatus
    sd   t0, 32*8(sp)
    csrr t0, sepc
    sd   t0, 33*8(sp)

    # a0 = &mut TrapContext，交给 Rust 处理
    mv   a0, sp
    call trap_handler

    # trap_handler 的返回值 a0 = 这次要恢复的 TrapContext 指针。
    # 普通中断时它就是当前任务的现场；发生调度时它是下一个任务的现场。
    j    __restore

    # ------------------------------------------------------------------
    # __restore：从 a0 指向的 TrapContext 恢复现场并 sret，永不返回。
    # __alltraps 会跳到这里；任务首次启动时也会直接调用它。
    # ------------------------------------------------------------------
    .globl __restore
__restore:
    mv   sp, a0              # 把栈切到要恢复的现场所在位置

    # 先恢复 sstatus / sepc：sstatus 决定 sret 后的特权级与中断开关
    ld   t0, 32*8(sp)
    csrw sstatus, t0
    ld   t0, 33*8(sp)
    csrw sepc, t0

    # 恢复 x1、x3..x31（x2 由末尾的 addi 自然还原）
    ld   x1,   1*8(sp)
    ld   x3,   3*8(sp)
    ld   x4,   4*8(sp)
    ld   x5,   5*8(sp)
    ld   x6,   6*8(sp)
    ld   x7,   7*8(sp)
    ld   x8,   8*8(sp)
    ld   x9,   9*8(sp)
    ld   x10, 10*8(sp)
    ld   x11, 11*8(sp)
    ld   x12, 12*8(sp)
    ld   x13, 13*8(sp)
    ld   x14, 14*8(sp)
    ld   x15, 15*8(sp)
    ld   x16, 16*8(sp)
    ld   x17, 17*8(sp)
    ld   x18, 18*8(sp)
    ld   x19, 19*8(sp)
    ld   x20, 20*8(sp)
    ld   x21, 21*8(sp)
    ld   x22, 22*8(sp)
    ld   x23, 23*8(sp)
    ld   x24, 24*8(sp)
    ld   x25, 25*8(sp)
    ld   x26, 26*8(sp)
    ld   x27, 27*8(sp)
    ld   x28, 28*8(sp)
    ld   x29, 29*8(sp)
    ld   x30, 30*8(sp)
    ld   x31, 31*8(sp)

    addi sp, sp, 34*8
    sret
    "#
);

extern "C" {
    fn __alltraps();
    fn __restore();
}

/// 从一份 TrapContext 恢复并 `sret`，永不返回。
///
/// 调度器第一次启动任务时会调用它：`ctx` 指向任务栈顶下方 272 字节处的
/// 初始现场，`__restore` 会把 sp 切到那个栈，然后 `sret` 跳到任务入口。
///
/// # Safety
/// `ctx` 必须指向一份合法、可长期驻留的 `TrapContext`。
pub unsafe fn restore_to(ctx: usize) -> ! {
    // 用 `call` 跳过去；__restore 内部会立刻把 sp 换成任务栈，
    // 因此 `call` push 到旧栈上的返回地址会被彻底丢弃，不会造成问题。
    asm!("call {}", sym __restore, in("a0") ctx, options(noreturn));
}

// --- scause 编码：最高位为 1 表示中断，为 0 表示异常；低位是具体原因号。---
const INTERRUPT_FLAG: usize = 1 << (usize::BITS as usize - 1);
const SUPERVISOR_TIMER_INTERRUPT: usize = 5;

/// 时钟中断间隔（`rdtime` ticks）。QEMU virt 上约 10 MHz，
/// 10_000_000 ticks ≈ 1 秒。调小它会切换得更频繁。
const TIMER_INTERVAL: u64 = 10_000_000;

/// 安装 trap 入口（写 `stvec`，直接模式）。
pub fn init() {
    let entry = __alltraps as *const () as usize;
    unsafe { asm!("csrw stvec, {}", in(reg) entry, options(nomem, nostack)) };
}

/// 安排首次时钟中断。之后每次中断都会在 `trap_handler` 里自动续期。
pub fn start_timer() {
    set_next_timer();
}

/// 开启 S 态时钟中断（`sie.STIE`）。
pub fn enable_timer_interrupt() {
    const SIE_STIE: usize = 1 << 5;
    unsafe { asm!("csrs sie, {}", in(reg) SIE_STIE, options(nomem, nostack)) };
}

/// 开启 S 态全局中断（`sstatus.SIE`）。
///
/// 本项目中任务的首次启动由 `sret` 打开中断，所以这里暂时用不到；
/// 保留它方便你理解 `sstatus.SIE` 与 `sie.STIE` 的分工。
#[allow(dead_code)]
pub fn enable_global_interrupt() {
    const SSTATUS_SIE: usize = 1 << 1;
    unsafe { asm!("csrs sstatus, {}", in(reg) SSTATUS_SIE, options(nomem, nostack)) };
}

/// 把下一次时钟中断安排在「当前时间 + TIMER_INTERVAL」。
fn set_next_timer() {
    let ret = sbi::set_timer(sbi::time() + TIMER_INTERVAL);
    if !ret.is_ok() {
        panic!("SBI set_timer failed: error={}", ret.error);
    }
}

/// trap 分发。`cx` 是完整寄存器现场，可读写并据此改变返回行为。
///
/// 返回值为“这次 `sret` 要恢复的现场地址”，详见模块头部说明与 `__restore`。
#[no_mangle]
pub extern "C" fn trap_handler(cx: &mut TrapContext) -> *mut TrapContext {
    let scause: usize;
    let stval: usize;
    let sepc: usize;
    unsafe {
        asm!("csrr {}, scause", out(reg) scause, options(nomem, nostack));
        asm!("csrr {}, stval",  out(reg) stval,  options(nomem, nostack));
        asm!("csrr {}, sepc",   out(reg) sepc,   options(nomem, nostack));
    }

    if scause == (INTERRUPT_FLAG | SUPERVISOR_TIMER_INTERRUPT) {
        // S 态无法直接清除 STIP：通过安排下一次定时来应答本次中断。
        set_next_timer();

        // 把当前任务的现场地址交给调度器；它可能决定切换任务，
        // 并返回“下一个任务”的现场地址（不切换则原样返回）。
        crate::task::on_timer_tick(cx as *mut TrapContext)
    } else if (scause & INTERRUPT_FLAG) != 0 {
        panic!(
            "unhandled interrupt: scause={:#x}, stval={:#x}, sepc={:#x}",
            scause, stval, sepc
        );
    } else {
        // 异常：`sepc` 指向出错指令，`stval` 依异常类型含义不同。
        panic!(
            "unhandled exception: scause={:#x}, stval={:#x}, sepc={:#x}",
            scause, stval, sepc
        );
    }
}

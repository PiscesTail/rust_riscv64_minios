//! MiniOS —— 一个运行在 RISC-V64 (S 态) 的极简内核。
//!
//! 启动流程：
//!   QEMU → OpenSBI (M 态) → 跳转到 0x8020_0000 的 _start (S 态) → rust_main
//!
//! 目前功能：
//!   1. 通过 SBI 控制台打印字符串；
//!   2. 安装 trap 入口，开启 S 态时钟中断；
//!   3. **创建多个内核线程，并用时钟中断做抢占式轮转调度**（本模块 + `task.rs`）。

#![no_std]
#![no_main]

mod sbi;
mod task;
mod trap;

use core::arch::global_asm;
use core::fmt;
use core::panic::PanicInfo;

// 入口汇编：清 bss、设置内核栈，然后进入 Rust 世界。
// 进入 _start 时（由 OpenSBI 约定）：
//   a0 = hartid
//   a1 = DTB 地址
global_asm!(
    r#"
    .section .text.entry
    .globl _start
_start:
    # 1) 清零 .bss（含所有未初始化的全局变量、任务栈等）。
    la     t0, _bss_start
    la     t1, _bss_end
1:  bgeu   t0, t1, 2f
    sd     zero, 0(t0)
    addi   t0, t0, 8
    j      1b
2:
    # 2) 设置内核栈（linker.ld 里 _stack_top 之前预留了 32 KiB）。
    la     sp, _stack_top
    # 3) 进入 Rust。
    call   rust_main
3:  wfi
    j      3b
    "#
);

/// 基于 SBI 控制台的极简字符输出器。
pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            sbi::console_putchar(b);
        }
        Ok(())
    }
}

/// 打印到控制台（不带换行）。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::Console, $($arg)*);
    }};
}

/// 打印到控制台（带换行）。
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::print!($($arg)*);
        $crate::print!("\n");
    }};
}

/// 忙等待约 `ticks` 个 `rdtime` 周期（QEMU virt 上约 10 tick = 1 微秒）。
///
/// 这里故意不用 `sleep`（内核还没有这种系统调用），只是空转消耗时间，
/// 好让每个任务在被抢占前都能打印一些内容。
fn busy_wait(ticks: u64) {
    let start = sbi::time();
    while sbi::time().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// 演示用的三个内核线程。
//
// 它们都是 `extern "C" fn() -> !`，由 `task::spawn` 安排到各自的内核栈上运行。
// 函数里的死循环会一直跑下去，直到被时钟中断抢占并切换；这正是抢占式调度的
// 直观体现——任务自己并不需要“让出 CPU”。
// ---------------------------------------------------------------------------

extern "C" fn task_alpha() -> ! {
    let mut n = 0u64;
    loop {
        n += 1;
        crate::println!("        [alpha] heartbeat #{}", n);
        busy_wait(3_000_000); // ≈ 0.3 秒
    }
}

extern "C" fn task_beta() -> ! {
    let mut n = 0u64;
    loop {
        n += 1;
        crate::println!("        [ beta] heartbeat #{}", n);
        busy_wait(3_000_000);
    }
}

extern "C" fn task_gamma() -> ! {
    let mut n = 0u64;
    loop {
        n += 1;
        crate::println!("        [gamma] heartbeat #{}", n);
        busy_wait(3_000_000);
    }
}

/// Rust 内核入口（由 _start 调用）。
///
/// a0/a1 恰好对应两个参数：hartid 与 DTB 地址。
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb: usize) -> ! {
    println!("==============================================");
    println!("  MiniOS | RISC-V64 + Rust | OpenSBI S-Mode");
    println!("==============================================");
    println!("[OK] Running on hart      = {}", hartid);
    println!("[OK] DTB address          = 0x{:x}", dtb);
    println!("[OK] Printing via SBI console works.");

    // 1) 安装 trap 入口（必须在开中断之前）。
    trap::init();
    println!("[OK] stvec installed, traps enabled.");

    // 2) 创建三个内核线程（此时还没开始调度，只是准备好各自的栈与现场）。
    task::spawn("alpha", task_alpha);
    task::spawn("beta", task_beta);
    task::spawn("gamma", task_gamma);
    println!("[OK] 3 kernel threads spawned.");

    // 3) 安排第一次时钟中断，并打开 S 态时钟中断源。
    //    注意：全局中断（sstatus.SIE）由任务的首次 `sret` 打开，
    //    这样能避免在启动阶段误陷入。
    trap::start_timer();
    trap::enable_timer_interrupt();
    println!("[OK] Supervisor timer armed. Starting scheduler ...");
    println!("----------------------------------------------");

    // 4) 进入调度：启动第一个任务，之后由时钟中断驱动轮转。
    task::run()
}

/// 极简 panic 处理：打印信息后优雅关机。
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("[PANIC] {}", info);
    sbi::shutdown()
}

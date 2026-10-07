//! SBI (Supervisor Binary Interface) 封装。
//!
//! MiniOS 作为 OpenSBI (M 态) 加载的 payload 运行在 S 态，
//! 一切特权操作（打印、定时器、关机）都通过 `ecall` 陷入 M 态完成。
//!
//! 这里同时包含两套调用约定：
//! - **Legacy**（v0.1）：功能号直接放在 `a7`，无返回值约定（仅 console_putchar）。
//! - **v0.2+**：`a7 = EID`（扩展 ID）、`a6 = FID`（功能 ID），
//!   返回 `a0 = error`、`a1 = value`。

use core::arch::asm;

/// SBI v0.2 调用的返回值：`error` 为 0 表示成功，`value` 为扩展自定义数据。
#[derive(Debug, Clone, Copy)]
pub struct SbiRet {
    pub error: isize,
    pub value: usize,
}

impl SbiRet {
    #[inline]
    pub fn is_ok(&self) -> bool {
        self.error == 0
    }
}

/// SBI v0.2+ 通用 ecall。
#[inline(always)]
unsafe fn sbi_call(eid: usize, fid: usize, a0: usize, a1: usize, a2: usize) -> SbiRet {
    let mut a0 = a0;
    let mut a1 = a1;
    // SBI 约定：除 a0/a1 外所有寄存器在 ecall 前后保持不变，
    // 因此这里无需 clobber_abi，只需声明不会使用栈。
    asm!(
        "ecall",
        inlateout("a0") a0,
        inlateout("a1") a1,
        in("a2") a2,
        in("a6") fid,
        in("a7") eid,
        options(nostack),
    );
    SbiRet {
        error: a0 as isize,
        value: a1,
    }
}

/// Legacy ecall：功能号放在 `a7`，无返回值。
#[inline(always)]
unsafe fn sbi_legacy(eid: usize, a0: usize) {
    asm!(
        "ecall",
        inlateout("a0") a0 => _,
        in("a7") eid,
        options(nostack),
    );
}

// ---------------------------------------------------------------------------
// Legacy Console 扩展（EID = 0x01）：向 OpenSBI 调试控制台输出一个字符。
// 之所以暂时保留它而不是直接驱动 16550 UART，是为了让内核在早期启动阶段
// 就能打印；等有 MMIO/UART 驱动后再替换。
// ---------------------------------------------------------------------------
pub fn console_putchar(c: u8) {
    const LEGACY_CONSOLE_PUTCHAR: usize = 0x01;
    unsafe { sbi_legacy(LEGACY_CONSOLE_PUTCHAR, c as usize) };
}

// ---------------------------------------------------------------------------
// Time 扩展（EID = "TIME"）：设置下一次时钟中断的绝对时刻（单位：rdtime ticks）。
// ---------------------------------------------------------------------------
const SBI_EXT_TIME: usize = 0x5449_4D45; // "TIME"
const SBI_EXT_TIME_SET_TIMER: usize = 0;

/// 请求在绝对时刻 `stime` 触发一次 S 态时钟中断。
pub fn set_timer(stime: u64) -> SbiRet {
    unsafe { sbi_call(SBI_EXT_TIME, SBI_EXT_TIME_SET_TIMER, stime as usize, 0, 0) }
}

/// 读取 `time` CSR（硬件计时器，QEMU virt 上频率约 10 MHz）。
#[inline(always)]
pub fn time() -> u64 {
    let t: u64;
    unsafe { asm!("rdtime {}", out(reg) t, options(nomem, nostack)) };
    t
}

// ---------------------------------------------------------------------------
// Base 扩展（EID = 0x10）：探测固件是否支持某个扩展。
// ---------------------------------------------------------------------------
const SBI_EXT_BASE: usize = 0x10;
const SBI_EXT_BASE_PROBE_EXT: usize = 3;

/// 返回非 0 表示固件支持扩展 `eid`。
pub fn probe_extension(eid: usize) -> usize {
    unsafe { sbi_call(SBI_EXT_BASE, SBI_EXT_BASE_PROBE_EXT, eid, 0, 0).value }
}

// ---------------------------------------------------------------------------
// System Reset 扩展（EID = "SRST"）：优雅关机 / 复位。
// ---------------------------------------------------------------------------
const SBI_EXT_SRST: usize = 0x5352_5354; // "SRST"
const SBI_EXT_SRST_RESET: usize = 0;
const RESET_TYPE_SHUTDOWN: usize = 0;
const RESET_REASON_NO_REASON: usize = 0;

/// 请求整个系统关机并退出 QEMU；若固件不支持则挂起 CPU。
pub fn shutdown() -> ! {
    // 先探测扩展，避免在不支持 SRST 的老固件上瞎调用。
    if probe_extension(SBI_EXT_SRST) != 0 {
        unsafe {
            sbi_call(
                SBI_EXT_SRST,
                SBI_EXT_SRST_RESET,
                RESET_TYPE_SHUTDOWN,
                RESET_REASON_NO_REASON,
                0,
            );
        }
    }

    // 走到这里说明关机失败，直接停住 CPU，避免继续执行未定义代码。
    loop {
        unsafe { asm!("wfi", options(nomem, nostack)) }
    }
}

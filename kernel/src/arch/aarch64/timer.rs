//! ARM Generic Timer driver (virtual timer / CNTV).
//!
//! Slice 2.4 uses the EL1 virtual timer as the preemption tick source. It
//! delivers PPI 27 to the GICv3 redistributor; we configure it for a fixed
//! tick frequency, then re-arm `CNTV_TVAL_EL0` from the interrupt handler.
//!
//! Reference: ARM ARM (DDI 0487) D11 "The Generic Timer in AArch64 state".
//!
//! Why the virtual timer (CNTV) rather than the physical (CNTP)?
//! - On QEMU `-M virt` we run at EL1 without an EL2 hypervisor, so CNTP
//!   would work fine — but the virtual timer is the conventional choice
//!   for guest kernels (it stays correct under live migration / future
//!   hypervisor introspection) and gates on the same PPI mechanism.

use core::sync::atomic::{AtomicU64, Ordering};

/// PPI INTID for the EL1 virtual timer on ARM v8-A. Same value on every
/// implementation that exposes a Generic Timer.
pub const INTID_VIRT_TIMER: u32 = 27;

/// Counter ticks per scheduler tick, captured by `init` for the IRQ
/// handler's re-arm path.
static PERIOD_TICKS: AtomicU64 = AtomicU64::new(0);

/// Read the counter frequency in Hz from `CNTFRQ_EL0`.
pub fn frequency_hz() -> u64 {
    let freq: u64;
    // SAFETY: `CNTFRQ_EL0` is unconditionally readable at EL1.
    unsafe {
        core::arch::asm!(
            "mrs {0}, CNTFRQ_EL0",
            out(reg) freq,
            options(nomem, nostack, preserves_flags),
        );
    }
    freq
}

/// Initialize the virtual timer to fire at `tick_hz` Hz.
///
/// Programs `CNTV_TVAL_EL0` with one period worth of counter ticks and
/// enables the timer via `CNTV_CTL_EL0.ENABLE`. Does *not* unmask the timer
/// at the GIC — call [`gic::enable_ppi`](super::gic::enable_ppi) for that.
///
/// SAFETY: callable only during single-threaded boot, with IRQs masked. The
/// timer immediately starts counting down; ensure the GIC is initialized
/// (so IRQs can later be unmasked at EL0) before unmasking PSTATE.DAIF.I.
pub unsafe fn init(tick_hz: u64) {
    let freq = frequency_hz();
    assert!(freq > 0, "CNTFRQ_EL0 reads zero — timer firmware bug?");
    assert!(tick_hz > 0, "tick_hz must be > 0");
    let period = freq / tick_hz;
    PERIOD_TICKS.store(period, Ordering::Release);

    // SAFETY: writes to CNTV_TVAL_EL0 and CNTV_CTL_EL0 are EL1-permitted
    // and do not affect any other CPU.
    unsafe {
        core::arch::asm!(
            "msr CNTV_TVAL_EL0, {period}",
            "msr CNTV_CTL_EL0, {one}",
            "isb",
            period = in(reg) period,
            one = in(reg) 1u64, // ENABLE = 1, IMASK = 0, ISTATUS read-only.
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Re-arm the virtual timer for the next tick. Called from the timer ISR
/// after `clock::tick` runs.
///
/// SAFETY: only meaningful after [`init`]; called from IRQ context.
pub unsafe fn rearm() {
    let period = PERIOD_TICKS.load(Ordering::Acquire);
    // SAFETY: as in `init` — CNTV_TVAL_EL0 is EL1-writable.
    unsafe {
        core::arch::asm!(
            "msr CNTV_TVAL_EL0, {0}",
            in(reg) period,
            options(nomem, nostack, preserves_flags),
        );
    }
}

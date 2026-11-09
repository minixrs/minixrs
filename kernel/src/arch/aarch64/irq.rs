// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! aarch64 IRQ dispatcher.
//!
//! Two callers reach [`irq_dispatch`], which reads the pending INTID from the
//! GIC, dispatches by ID, then signals EOI:
//!
//! - the assembly entry stub `el0_64_irq_entry` (`interrupt.S`), through
//!   [`do_irq`], with the interrupted EL0 register frame pointer in x0;
//! - the kernel idle loop in `proc::sched::schedule_next` (slice 6.1, spec
//!   I11), which polls it at EL1 after a `wfi`, with `DAIF.I` still set. No
//!   exception is taken on that path, so there is no frame — which is why the
//!   dispatcher takes none.
//!
//! Two kinds of source are recognized. The EL1 virtual timer (PPI 27, slice
//! 2.4) drives `clock::tick`. A **hooked SPI** — a line a driver claimed with
//! `SYS_IRQCTL` (slice 6.1, spec I7) — is masked at the distributor, recorded
//! in its owner's `int_pending`, and delivered as a `NOTIFY` from `HARDWARE`;
//! the driver unmasks it again with `IRQ_ENABLE` once it has quiesced the
//! device. Anything else is a bug and says so — and if it is an SPI it is
//! masked too, so a line nobody owns cannot storm.

use minixrs_kernel_shared::irq::FIRST_SPI;

use crate::arch::ArchRegisterFrame;
use crate::arch::aarch64::gic;
use crate::arch::aarch64::timer;
use crate::clock;
use crate::uart::Uart;

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

/// Set for the whole of [`irq_dispatch`]: from before the `ack` until after the
/// matching `eoi` (or the spurious early return). While it is set an interrupt
/// may be acknowledged and not yet EOI'd, which masks it at the GIC — so
/// `proc::sched::schedule_next` asserts it clear before it would idle, turning
/// a `wfi` that can never wake into a loud panic. Single-CPU, DAIF.I-masked:
/// `Relaxed` is enough.
static IN_DISPATCH: AtomicBool = AtomicBool::new(false);

/// True while [`irq_dispatch`] is running — an interrupt may be acknowledged
/// and not yet EOI'd. See [`IN_DISPATCH`].
pub(crate) fn in_dispatch() -> bool {
    IN_DISPATCH.load(Ordering::Relaxed)
}

/// IRQ dispatch entry — called from `interrupt.S`'s `el0_64_irq_entry`.
///
/// The frame pointer is the same one `tpidr_el1` points at, i.e. the
/// register save area for the *currently-running* process at IRQ-entry
/// time. `clock::tick` may rewrite `tpidr_el1` via `sched::reschedule` to
/// switch contexts before the assembly tail returns to EL0.
#[unsafe(no_mangle)]
pub extern "C" fn do_irq(_frame: &mut ArchRegisterFrame) {
    // SAFETY: IRQ context — single async writer; the assembly stub guards
    // against re-entry by leaving PSTATE.DAIF.I set until eret restores
    // the saved SPSR. No PROC_TABLE borrow is live on IRQ entry.
    unsafe { irq_dispatch() }
}

/// Acknowledge, dispatch and EOI one pending interrupt.
///
/// The whole of what `do_irq` did before slice 6.1, split out so the idle
/// loop can call it without an exception frame.
///
/// SAFETY: caller must be in one of the two contexts [`gic::ack`] names —
/// IRQ context, or the idle poll at EL1 with `DAIF.I` set — and must hold no
/// reference into `PROC_TABLE` or the run queue, since `clock::tick` borrows
/// both.
pub(crate) unsafe fn irq_dispatch() {
    // Bracketing the body in one place covers every return path inside it —
    // the spurious early return as well as the EOI'd tail.
    IN_DISPATCH.store(true, Ordering::Relaxed);
    // SAFETY: forwarded — the caller's contract is this function's.
    unsafe { dispatch_one() };
    IN_DISPATCH.store(false, Ordering::Relaxed);
}

/// The body of [`irq_dispatch`], which brackets it with [`IN_DISPATCH`].
///
/// SAFETY: as [`irq_dispatch`].
unsafe fn dispatch_one() {
    // SAFETY: forwarded — the caller is in IRQ context or the idle poll, both
    // DAIF.I-masked and single-threaded, which is `ack`'s contract.
    let intid = unsafe { gic::ack() };

    if intid == gic::INTID_SPURIOUS {
        // No active interrupt (spurious). Nothing to EOI.
        return;
    }

    match intid {
        timer::INTID_VIRT_TIMER => {
            // SAFETY: forwarded — no other PROC_TABLE borrows live.
            unsafe { clock::tick() };
            // SAFETY: same — timer module is single-CPU.
            unsafe { timer::rearm() };
        }
        other => {
            if let Some(hook) = crate::irq_hooks::lookup(other) {
                // Spec I7: mask before EOI. The line is level-triggered and
                // stays asserted until the driver quiesces the device, which
                // it cannot do until it runs; EOI on the unmasked line would
                // re-enter here at once, forever. Masked, it is taken exactly
                // once per `IRQ_ENABLE`.
                // SAFETY: IRQ context or the idle poll, both single-threaded
                // with DAIF.I set — `mask_spi`'s contract; a hooked INTID
                // passed `decode`'s SPI range check when it was claimed.
                unsafe { gic::mask_spi(other) };
                // Only unblocks the owner — never schedules — so nothing
                // here can reach `schedule_next` with this interrupt un-EOI'd.
                crate::ipc::fire_irq(hook.owner, hook.notify_id);
            } else {
                // The prefix `do_irq: unexpected INTID` is in
                // tests/qemu-boot.forbidden — keep it byte-for-byte.
                let _ = writeln!(Uart::new(), "do_irq: unexpected INTID {other}");
                if (FIRST_SPI..gic::nr_intids()).contains(&other) {
                    // A line nobody owns must not be able to storm either.
                    // SAFETY: as the hooked arm's `mask_spi`; the range check
                    // is `mask_spi`'s own assertion, made here so a stray
                    // non-SPI INTID logs instead of panicking.
                    unsafe { gic::mask_spi(other) };
                }
            }
        }
    }

    // SAFETY: matches the `ack` above; the GIC enforces the pairing. Last, after
    // any `mask_spi` (I7).
    unsafe { gic::eoi(intid) };
}

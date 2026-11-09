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
//! Slice 2.4 recognizes exactly one source: the EL1 virtual timer
//! (PPI 27). Any other INTID is a bug — there is no other configured
//! interrupt source.

use crate::arch::ArchRegisterFrame;
use crate::arch::aarch64::gic;
use crate::arch::aarch64::timer;
use crate::clock;
use crate::uart::Uart;

use core::fmt::Write;

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
            let _ = writeln!(
                Uart::new(),
                "do_irq: unexpected INTID {other} (only PPI 27 is configured)",
            );
        }
    }

    // SAFETY: matches the `ack` above; the GIC enforces the pairing.
    unsafe { gic::eoi(intid) };
}

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! aarch64 IRQ dispatcher.
//!
//! Called from the assembly entry stub `el0_64_irq_entry` (`interrupt.S`)
//! with the interrupted EL0 register frame pointer in x0. We read the
//! pending INTID from the GIC, dispatch by ID, then signal EOI.
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
    // the saved SPSR.
    let intid = unsafe { gic::ack() };

    if intid == gic::INTID_SPURIOUS {
        // No active interrupt (spurious). Nothing to EOI.
        return;
    }

    match intid {
        timer::INTID_VIRT_TIMER => {
            // SAFETY: IRQ context invariant — no other PROC_TABLE borrows live.
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

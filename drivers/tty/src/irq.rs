// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! TTY's interrupt line: the `SYS_IRQCTL` claim and its denial battery
//! (slice 6.1, spec I3–I6, I10).
//!
//! TTY claims the PL011's line, INTID 33 — the one entry the kernel puts on its
//! allow-list at boot — under [`TTY_IRQ_NOTIFY_ID`], then drives the call's
//! refusals from EL0: each row of [`CASES`] is valid in every respect but one,
//! and must come back with exactly the result code the spec's table names. It
//! ends with the one positive case, the I6 replacement: the same claim again
//! answers `OK` with the same hook id, which TTY adopts.
//!
//! Nothing fires yet. TTY leaves the PL011's own interrupt mask (`UARTIMSC`)
//! alone, so the claimed line stays quiet at the device.

// Only `main.rs`'s `not(test)`-gated paths reach this, so under `cargo test`
// (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use minixrs_kernel_shared::callnr::{IRQ_ENABLE, IRQ_REENABLE, IRQ_SETPOLICY};
use minixrs_kernel_shared::error::{EBUSY, EINVAL, EPERM, OK};
use minixrs_kernel_shared::irq::NR_IRQ_HOOKS;
use minixrs_server_rt::{diag_fmt, sys_irqctl};

use crate::pl011::PL011_INTID;

/// The bit TTY's interrupt notifications will carry in their pending bitmap.
pub const TTY_IRQ_NOTIFY_ID: i32 = 0;

/// The battery's last argument word: a literal, or "the slot after TTY's own
/// hook", which only exists once the claim has answered.
#[derive(Clone, Copy)]
enum Id {
    Lit(i32),
    /// `claimed + 1` — a free slot, since TTY is the only claimer.
    FreeSlot,
}

/// One refusal: `(request, vector, policy, id, expected result)`.
type Case = (i32, i32, i32, Id, i32);

const INTID: i32 = PL011_INTID as i32;

/// The denial battery, in order. Each row breaks exactly one rule.
const CASES: [Case; 11] = [
    // I4: a real SPI, but not on TTY's allow-list.
    (
        IRQ_SETPOLICY,
        INTID + 1,
        0,
        Id::Lit(TTY_IRQ_NOTIFY_ID),
        EPERM,
    ),
    // I3: PPI 27 is the kernel's timer; SGIs and PPIs are never claimable.
    (IRQ_SETPOLICY, 27, 0, Id::Lit(TTY_IRQ_NOTIFY_ID), EINVAL),
    // I3: INTID 1020 is past every distributor's implemented range.
    (IRQ_SETPOLICY, 1020, 0, Id::Lit(TTY_IRQ_NOTIFY_ID), EINVAL),
    // I10: the policy word must be 0; IRQ_REENABLE is defined and refused.
    (
        IRQ_SETPOLICY,
        INTID,
        IRQ_REENABLE,
        Id::Lit(TTY_IRQ_NOTIFY_ID),
        EINVAL,
    ),
    // I6: TTY's own claimed line under a second notify_id.
    (
        IRQ_SETPOLICY,
        INTID,
        0,
        Id::Lit(TTY_IRQ_NOTIFY_ID + 1),
        EBUSY,
    ),
    // I2: notify_id is a bit index into a u32, range-checked as signed.
    (IRQ_SETPOLICY, INTID, 0, Id::Lit(32), EINVAL),
    (IRQ_SETPOLICY, INTID, 0, Id::Lit(-1), EINVAL),
    // I2: hook ids are 1-based, so 0 never names one...
    (IRQ_ENABLE, 0, 0, Id::Lit(0), EINVAL),
    // ...nor does anything past the table...
    (IRQ_ENABLE, 0, 0, Id::Lit(NR_IRQ_HOOKS as i32 + 1), EINVAL),
    // ...nor a slot nobody holds.
    (IRQ_ENABLE, 0, 0, Id::FreeSlot, EINVAL),
    // I1: an unknown subcode.
    (99, 0, 0, Id::Lit(0), EINVAL),
];

/// Claim the PL011 line, run [`CASES`], then the I6 replacement. Returns the
/// hook id TTY holds, or the claim's negative errno. Every outcome other than
/// all-pass prints one `[diag tty] irq…FAIL` line.
pub fn claim_and_probe() -> i32 {
    let (rc, claimed) = sys_irqctl(IRQ_SETPOLICY, INTID, 0, TTY_IRQ_NOTIFY_ID);
    if rc != OK {
        diag_fmt(format_args!("irq FAIL claim rc={rc}"));
        return rc;
    }

    for (i, &(request, vector, policy, id, expected)) in CASES.iter().enumerate() {
        let id = match id {
            Id::Lit(v) => v,
            Id::FreeSlot => claimed + 1,
        };
        let (rc, _) = sys_irqctl(request, vector, policy, id);
        if rc != expected {
            diag_fmt(format_args!("irq.deny FAIL case={i} rc={rc}"));
            return claimed;
        }
    }

    // I6: a repeat claim by the same owner under the same notify_id replaces
    // its hook in place — same id, no second slot.
    let (rc, hook) = sys_irqctl(IRQ_SETPOLICY, INTID, 0, TTY_IRQ_NOTIFY_ID);
    if rc != OK || hook != claimed {
        diag_fmt(format_args!(
            "irq.deny FAIL case={} rc={rc} hook={hook}",
            CASES.len()
        ));
        return claimed;
    }
    diag_fmt(format_args!("irq.deny ok n={}", CASES.len() + 1));
    hook
}

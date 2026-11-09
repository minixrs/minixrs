// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_IRQCTL` — a driver claims, releases, masks and unmasks its interrupt
//! line (slice 6.1, spec I1–I6, I9, I10).
//!
//! MINIX 3's four subcodes at MINIX's values (`kernel/system/do_irqctl.c`):
//! `IRQ_SETPOLICY` claims a line and leaves it unmasked, `IRQ_RMPOLICY` masks
//! it and frees the hook, `IRQ_ENABLE` / `IRQ_DISABLE` unmask / mask it. There
//! is no "acknowledge" subcode: re-enabling the line *is* the acknowledgement,
//! because delivery masks it (I7, slice 6.1's delivery task).
//!
//! Request decoding and the hook table are pure logic in `kernel-shared::irq`,
//! host-tested there; this handler adds the two things only the kernel has —
//! the caller's privilege slot and the GIC.
//!
//! ## Two gates, and the allow-list has no opt-out (I4)
//!
//! Reaching here means the caller's `k_call_mask` admits `SYS_IRQCTL`, which
//! every boot server's does — too coarse to be the only gate. So
//! `IRQ_SETPOLICY` is also checked against `Priv::irqs`, the per-line
//! allow-list the kernel fills at boot, and that check is unconditional: an
//! empty list denies every line. MINIX's `CHECK_IRQ` flag, which a privileged
//! server may simply lack, has no counterpart.
//!
//! ## Why this is routed with the target-taking calls (I5)
//!
//! It acts only on the caller, but a shared privilege slot must be refused —
//! `int_pending` lives in the slot, and one bitmap cannot describe several
//! processes' interrupts — and the handler writes the slot's state, so it
//! needs `&mut Priv`. Exactly `SYS_SETGRANT`'s situation, and its arm.
//!
//! ## Message payload layout (offsets within `Message::payload`, I2)
//!
//! | offset | field                                              | direction |
//! |--------|----------------------------------------------------|-----------|
//! |  0..4  | subcode (`IRQ_*`)                                  | in        |
//! |  4..8  | vector — raw GIC INTID, `IRQ_SETPOLICY` only       | in        |
//! |  8..12 | policy — must be 0 (I10), `IRQ_SETPOLICY` only     | in        |
//! | 12..16 | `IRQ_SETPOLICY`: the caller's `notify_id`          | in        |
//! | 12..16 | the other three: the 1-based hook id               | in        |
//! | 12..16 | `IRQ_SETPOLICY` success: the 1-based hook id       | out       |
//!
//! `m_type` is `OK`, `EINVAL`, `EPERM`, `EBUSY`, or `ENOSPC` per the spec's
//! result-code table.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::{
    IRQCTL_HOOK_ID_OFF, IRQCTL_NOTIFY_ID_OFF, IRQCTL_POLICY_OFF, IRQCTL_REQUEST_OFF,
    IRQCTL_VECTOR_OFF,
};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EPERM, OK};
use minixrs_kernel_shared::irq::{IrqRequest, decode};
use minixrs_kernel_shared::message::Message;

use crate::arch::aarch64::gic;
use crate::irq_hooks;
use crate::proc::Priv;
use crate::uart::Uart;

/// Every claimed SPI's GIC priority: the value `main` gives the timer PPI.
/// `ICC_PMR_EL1` is `0xFF`, and a line whose priority is not numerically below
/// the mask never fires (I9).
const SPI_PRIORITY: u8 = 0x80;

/// Claims and releases traced in full before the trace goes quiet. A driver
/// claims its line once at startup, so a head carve-out catches every real
/// claim; enable/disable are steady-state and never traced here.
const IRQCTL_TRACE_HEAD: u64 = 6;
static IRQCTL_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_IRQCTL`. `caller_e` is the caller's own stored endpoint — the owner
/// recorded in, and checked against, the hook table.
pub(super) fn do_irqctl(
    caller_priv: &mut Priv,
    caller_nr: ProcNr,
    caller_e: Endpoint,
    msg: &mut Message,
) -> i32 {
    let req = match decode(
        read_i32(msg, IRQCTL_REQUEST_OFF),
        read_i32(msg, IRQCTL_VECTOR_OFF),
        read_i32(msg, IRQCTL_POLICY_OFF),
        // `IRQCTL_NOTIFY_ID_OFF == IRQCTL_HOOK_ID_OFF`; `decode` knows which one
        // it is from the subcode.
        read_i32(msg, IRQCTL_NOTIFY_ID_OFF),
        gic::nr_intids(),
    ) {
        Ok(r) => r,
        Err(e) => return e,
    };

    // I5: a shared slot has no single owning process. `proc_nr` naming *this*
    // caller is what proves the slot dedicated — the `do_setgrant` test.
    if caller_priv.proc_nr != Some(caller_nr) {
        return EPERM;
    }

    // SAFETY: a kernel call — single-threaded EL1, IRQs masked. The borrow is
    // used only within this function, and nothing called below re-borrows the
    // table (`gic::*` touches only the distributor).
    let hooks = unsafe { irq_hooks::table_mut() };

    match req {
        IrqRequest::SetPolicy { vector, notify_id } => {
            // I4: the allow-list, before anything changes — so a refused
            // replacement leaves the caller's old hook exactly as it was (I6).
            if !caller_priv.irqs.contains(&vector) {
                return EPERM;
            }
            let claim = match hooks.set_policy(caller_e, vector, notify_id) {
                Ok(c) => c,
                Err(e) => return e,
            };
            if let Some(old) = claim.moved_from {
                // The replacement moved the hook: the old line has no owner now.
                // SAFETY: kernel call, IRQs masked; `old` was an SPI when it was
                // claimed.
                unsafe { gic::mask_spi(old) };
            }
            // SAFETY: kernel call, IRQs masked; `vector` just passed `decode`'s
            // `FIRST_SPI..nr_intids()` check.
            unsafe { gic::enable_spi(vector, SPI_PRIORITY) };
            write_i32(msg, IRQCTL_HOOK_ID_OFF, claim.hook_id);
            trace(caller_nr, "setpolicy", vector, claim.hook_id);
            OK
        }
        IrqRequest::RmPolicy { hook_id } => {
            let h = match hooks.remove(caller_e, hook_id) {
                Ok(h) => h,
                Err(e) => return e,
            };
            // SAFETY: kernel call, IRQs masked; a hooked INTID is an SPI.
            unsafe { gic::mask_spi(h.intid) };
            trace(caller_nr, "rmpolicy", h.intid, hook_id);
            OK
        }
        IrqRequest::Enable { hook_id } => match hooks.owned(caller_e, hook_id) {
            Ok(h) => {
                // SAFETY: kernel call, IRQs masked; a hooked INTID is an SPI.
                unsafe { gic::unmask_spi(h.intid) };
                OK
            }
            Err(e) => e,
        },
        IrqRequest::Disable { hook_id } => match hooks.owned(caller_e, hook_id) {
            Ok(h) => {
                // SAFETY: kernel call, IRQs masked; a hooked INTID is an SPI.
                unsafe { gic::mask_spi(h.intid) };
                OK
            }
            Err(e) => e,
        },
    }
}

fn trace(caller_nr: ProcNr, what: &str, intid: u32, hook_id: i32) {
    let n = IRQCTL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= IRQCTL_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_IRQCTL] proc={} {what} intid={intid} hook={hook_id}",
            caller_nr.get(),
        );
    }
}

#[inline]
fn read_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        msg.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}

#[inline]
fn write_i32(msg: &mut Message, off: usize, v: i32) {
    msg.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

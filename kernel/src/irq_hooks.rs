// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The kernel's IRQ hook table (slice 6.1, spec I6/I8).
//!
//! One static [`HookTable`] — the pure, host-tested logic in
//! `kernel-shared::irq` — wrapped in the usual `UnsafeCell` newtype. It maps a
//! claimed SPI to the driver that owns it: `SYS_IRQCTL` writes it through
//! [`table_mut`], interrupt dispatch reads it through [`lookup`], and
//! `SYS_EXIT` / `SYS_EXEC` empty a process's share of it through
//! [`release_owner`].
//!
//! A line has at most one hook (QEMU `virt` gives every device its own SPI, so
//! there is no shared-interrupt chain to keep consistent), and the table holds
//! [`NR_IRQ_HOOKS`](minixrs_kernel_shared::irq::NR_IRQ_HOOKS) of them, found by
//! a linear scan.

use core::cell::UnsafeCell;

use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::irq::{HookTable, IrqHook};

use crate::arch::aarch64::gic;

#[repr(transparent)]
struct IrqHooks(UnsafeCell<HookTable>);
// SAFETY: single CPU, and every accessor runs at EL1 with IRQs masked — a
// kernel call (`do_irqctl`, `do_exit`, `do_exec`) or interrupt dispatch — so no
// two accesses can interleave. The accessors below hand out borrows that end
// before they return, except `table_mut`, whose caller carries the rule.
unsafe impl Sync for IrqHooks {}

static IRQ_HOOKS: IrqHooks = IrqHooks(UnsafeCell::new(HookTable::new()));

/// Borrow the hook table mutably, for `SYS_IRQCTL`'s handler.
///
/// SAFETY: single-threaded EL1 with IRQs masked, and the caller must not hold
/// the returned borrow across a call to [`lookup`] or [`release_owner`] (each
/// re-borrows the same static).
pub(crate) unsafe fn table_mut() -> &'static mut HookTable {
    // SAFETY: forwarded — the caller's contract above.
    unsafe { &mut *IRQ_HOOKS.0.get() }
}

/// The hook claiming `intid`, if any — copied out, so no borrow of the table
/// outlives the call. Read by `irq_dispatch`'s hooked-SPI arm.
pub(crate) fn lookup(intid: u32) -> Option<IrqHook> {
    // SAFETY: single-threaded EL1 with IRQs masked (the `Sync` invariant); the
    // shared borrow ends at the end of this expression.
    unsafe { (*IRQ_HOOKS.0.get()).by_intid(intid) }
}

/// Free every hook `owner` holds, masking each line at the distributor first so
/// a line nobody owns any more cannot fire (spec I8). Returns how many were
/// freed. Called by `SYS_EXIT` (a stale owner endpoint would otherwise be
/// notified, or its slot's next occupant) and `SYS_EXEC` (the new image holds
/// none of the old image's hook ids).
pub(crate) fn release_owner(owner: Endpoint) -> usize {
    // SAFETY: single-threaded EL1 with IRQs masked (the `Sync` invariant); the
    // borrow ends when `release_owner` returns, and the mask closure touches
    // only the GIC, never this table.
    let table = unsafe { &mut *IRQ_HOOKS.0.get() };
    table.release_owner(owner, |intid| {
        // SAFETY: a kernel call with IRQs masked; every hooked INTID passed
        // `decode`'s SPI range check when it was claimed.
        unsafe { gic::mask_spi(intid) }
    })
}

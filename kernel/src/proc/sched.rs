// `pick_proc`, `enqueue`, `dequeue`, the run queues, and per-priority
// scheduling all land in slice 2.4. Slice 2.3 only needs the single-current-
// proc tracking that lets the SVC entry path find the right register frame.
#![allow(dead_code)]

//! Cooperative scheduler — minimal slice-2.3 surface.
//!
//! Slice 2.3 runs exactly one EL0 task. `CURRENT_PROC_NR` records which slot
//! it occupies so the SVC entry path can find the right `ArchRegisterFrame`
//! to save into; `switch_to_user` parks `&mut Proc::regs` in `TPIDR_EL1`
//! and `eret`s into EL0 via the assembly stub in `trap.S`.
//!
//! Slice 2.4 replaces `switch_to_user`'s "pick the slot from
//! `CURRENT_PROC_NR`" shortcut with a real `pick_proc` over a priority-banded
//! run queue.

use core::sync::atomic::{AtomicI32, Ordering};

use minix4_kernel_shared::ProcNr;

use crate::arch::ArchRegisterFrame;
use crate::proc::Proc;
use crate::proc::table::proc_slot_mut;

/// Currently-running process number. `i32::MIN` is the "no process running"
/// sentinel — chosen because it can never collide with a real `ProcNr`
/// (kernel tasks are `-5..=-1`, user processes are `0..NR_PROCS`).
pub static CURRENT_PROC_NR: AtomicI32 = AtomicI32::new(i32::MIN);

const NO_PROC: i32 = i32::MIN;

/// Borrow the currently-running process's slot.
///
/// SAFETY: caller must respect the single-threaded-boot invariant on
/// `PROC_TABLE`. The returned `&'static mut Proc` must not alias any other
/// reference into the table.
pub unsafe fn current_proc_mut() -> Option<&'static mut Proc> {
    let n = CURRENT_PROC_NR.load(Ordering::Relaxed);
    if n == NO_PROC {
        return None;
    }
    // SAFETY: forwarded — caller holds the no-aliasing invariant.
    unsafe { proc_slot_mut(ProcNr::new(n)) }
}

unsafe extern "C" {
    /// Restores `tpidr_el1`-pointed `ArchRegisterFrame` and erets into EL0.
    /// Defined in `kernel/src/arch/aarch64/trap.S`.
    fn el1_return_to_user() -> !;
}

/// Switch to running `p` at EL0.
///
/// Parks `&mut p.regs` in `TPIDR_EL1` so the SVC entry path can locate it
/// later, then jumps to the assembly restore-and-`eret` stub. The first
/// call to this function (from `kmain`) transitions the kernel from EL1
/// boot context into EL0 user execution; subsequent re-entries from the
/// SVC tail are already on the same path.
///
/// SAFETY: `p` must be fully initialized — in particular `regs.elr_el1`
/// must point at a mapped EL0-executable page, `regs.sp_el0` at a mapped
/// EL0-writable page, and `regs.spsr_el1` must encode a sane EL0 mode
/// (M[3:0] = EL0t = 0). `TTBR0_EL1` must already be active and cover those
/// VAs. Must be called with interrupts masked; slice 2.4 will revisit when
/// preemption arrives.
pub unsafe fn switch_to_user(p: &mut Proc) -> ! {
    CURRENT_PROC_NR.store(p.nr.get(), Ordering::Relaxed);
    let regs_ptr: *mut ArchRegisterFrame = &mut p.regs;
    // SAFETY: TPIDR_EL1 is EL1-only and used only by our own SVC stub +
    // el1_return_to_user. No other code reads or writes it.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {0}",
            "isb",
            in(reg) regs_ptr,
            options(nomem, nostack, preserves_flags),
        );
    }
    // SAFETY: trap.S contract documented at `el1_return_to_user`. The
    // function does not return; the `-> !` upholds that.
    unsafe { el1_return_to_user() }
}

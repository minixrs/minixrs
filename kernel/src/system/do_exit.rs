// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_EXIT` — terminate a target process and free its slot.
//!
//! PM's kill and exit paths end here: once PM decides a process is done —
//! signal disposition terminate, or a voluntary `PM_EXIT` — it issues
//! `SYS_EXIT` on the target, exactly MINIX 3's `pm_exit → sys_clear →
//! clear_proc` shape (`kernel/system/do_clear.c`). Slice 4.6 completes the
//! 4.5 "exit-lite" into full teardown, in MINIX's order:
//!
//! - cancel the target's `SYS_SETALARM` timer (a stale `EARLIEST_ALARM`
//!   cached minimum is harmless — the next gated scan finds no due alarm);
//! - set `RTS_PROC_STOP` (dequeues via `rts_set` if the target was runnable);
//! - if it was blocked `SENDING`, unlink it from the destination's `caller_q`
//!   so a terminated proc's queued message is never delivered;
//! - wake every process blocked on the dead endpoint with `EDEADSRCDST`
//!   (MINIX `clear_ipc_refs`): senders queued on the dead proc's `caller_q`
//!   and receivers waiting on it specifically both resume with the error in
//!   `x0`, and a dedicated-priv proc's pending-notification bits are purged;
//! - tear down the address space: free every leaf frame ([`walk_leaves`]),
//!   invalidate the ASID's TLB entries, free the page-table tree
//!   ([`AddrSpace::destroy`]), and recycle the ASID;
//! - free the slot: bump the endpoint generation (so every stale endpoint
//!   now fails `okendpt` with `EDEADSRCDST`), zero the signal/IPC/timer
//!   state, and store `RTS_SLOT_FREE` for `SYS_FORK` to reuse.
//!
//! `SELF` (and the caller's own endpoint) are rejected: full teardown of the
//! *active* address space mid-kernel-call would pull the TTBR0 out from under
//! the running caller. PM — the sole intended holder — always names a third
//! party. MINIX's self-exit arm (`SIGABRT` for system procs) has no consumer
//! here yet.
//!
//! Target-taking (routed beside `SYS_VMCTL` in `kernel_call_dispatch`); trust
//! model identical to `do_vmctl` — the `k_call_mask` gate is the only check,
//! and PM is the intended sole holder. Takes `priv_table` for the
//! pending-notification purge.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field                 | direction |
//! |--------|-----------------------|-----------|
//! |  0..4  | target endpoint (i32) | in        |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, SELF, bump_generation};
use minixrs_kernel_shared::error::{EDEADSRCDST, EINVAL, OK};
use minixrs_kernel_shared::message::Message;

use crate::arch::aarch64::addrspace::{AddrSpace, walk_leaves};
use crate::arch::aarch64::{asid, mmu};
use crate::mm::{Frame, free_frame};
use crate::proc::bitmap::clear_sys_bit;
use crate::proc::flags::{MF_REPLY_PEND, RTS_PROC_STOP, RTS_RECEIVING, RTS_SENDING, RTS_SLOT_FREE};
use crate::proc::priv_struct::IPC_MAP_CHUNKS;
use crate::proc::table::{N_PROC_SLOTS, okendpt, proc_index};
use crate::proc::{HeapWindow, PageFaultState, Priv, Proc, sched};
use crate::uart::Uart;

/// Leading `SYS_EXIT` calls traced explicitly, plus an every-100th steady
/// sample — fork/exit churn makes exits steady-state as of 4.6, so a
/// head-only trace (the 4.5 shape) would go silent seconds into boot.
const EXIT_TRACE_HEAD: u64 = 6;
const EXIT_TRACE_EVERY: u64 = 100;
static EXIT_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_EXIT` — permanently stop the target, detach it from IPC, tear down
/// its address space, and free its slot.
pub(super) fn do_exit(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e: Endpoint = read_i32(msg, 0);

    if target_e == SELF {
        return EINVAL;
    }
    let target_idx = match okendpt(proc_table, target_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };
    // The SELF sentinel is rejected above, but the caller could also name its
    // own endpoint explicitly — same active-TTBR0 hazard, same answer.
    if proc_index(caller_nr) == Some(target_idx) {
        return EINVAL;
    }

    let (rts, sendto_e, nr, name0, dead_e, ttbr0_pa, dead_asid, dead_priv_id) = {
        let p = &mut proc_table[target_idx];
        let rts = p.rts_flags.load(Ordering::Relaxed);
        p.alarm_at = 0;
        let out = (
            rts, p.sendto_e, p.nr, p.name[0], p.endpoint, p.ttbr0_pa, p.asid, p.priv_id,
        );
        // SAFETY: single-threaded EL1 context; the exclusive `p` borrow ends
        // (NLL) as `rts_set` captures `nr` internally — it dequeues the target
        // if it was runnable.
        unsafe { sched::rts_set(p, RTS_PROC_STOP) };
        out
    };

    // A proc blocked SENDING sits on exactly one caller queue — the
    // destination named by its `sendto_e` (classic MINIX `clear_proc` walks
    // only that chain too). Unlink it so the dead proc's queued message is
    // never delivered.
    if rts & RTS_SENDING != 0 {
        unlink_from_caller_q(proc_table, target_idx, sendto_e);
    }

    unblock_dependents(proc_table, priv_table, target_idx, dead_e, nr, dead_priv_id);

    let freed_pages = teardown_addrspace(ttbr0_pa, dead_asid);

    free_slot(&mut proc_table[target_idx]);

    let n = EXIT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= EXIT_TRACE_HEAD || n.is_multiple_of(EXIT_TRACE_EVERY) {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_EXIT] target={} nr={} freed={freed_pages} result=0",
            name0 as char,
            nr.get(),
        );
    }
    OK
}

/// Wake every process blocked on the dead endpoint with `EDEADSRCDST`
/// (MINIX 3 `kernel/system.c` `clear_ipc_refs`).
///
/// A blocked proc's dependency is `sendto_e` when `RTS_SENDING` is set (the
/// SENDREC send-half case carries both bits — SENDING wins, matching MINIX's
/// `P_BLOCKEDON`), else `getfrom_e` when `RTS_RECEIVING`. Matching procs get
/// the error patched into their parked SVC frame's `x0` (the register the
/// trap restore path hands back — the MINIX `retreg` pattern), their queue
/// linkage cleared, and both IPC block bits unset; `rts_unset` enqueues any
/// that become runnable. `RECEIVE(ANY)` blockers are untouched (`ANY` never
/// equals a real endpoint).
///
/// When the dead proc *owns* a dedicated priv slot (`priv.proc_nr` names it),
/// its pending-notification footprint is purged: its sender bit is cleared
/// from every priv's `notify_pending`, its own map is zeroed, and the slot is
/// detached. A shared priv slot (the `USER_PRIV_ID` case — every forked child
/// aliases it) is deliberately left alone: its bitmap state belongs to all
/// its live procs collectively, and `USR_T` (SENDREC-only) procs can never
/// have set a notify bit anyway.
fn unblock_dependents(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    target_idx: usize,
    dead_e: Endpoint,
    dead_nr: ProcNr,
    dead_priv_id: Option<minixrs_kernel_shared::PrivId>,
) {
    for idx in 0..N_PROC_SLOTS {
        if idx == target_idx {
            continue;
        }
        let p = &mut proc_table[idx];
        let rts = p.rts_flags.load(Ordering::Relaxed);
        if rts & RTS_SLOT_FREE != 0 {
            continue;
        }
        let blocked_on = if rts & RTS_SENDING != 0 {
            p.sendto_e
        } else if rts & RTS_RECEIVING != 0 {
            p.getfrom_e
        } else {
            continue;
        };
        if blocked_on != dead_e {
            continue;
        }
        p.regs.x[0] = EDEADSRCDST as i64 as u64;
        p.q_link = None;
        p.sendto_e = NONE;
        p.getfrom_e = NONE;
        p.misc_flags &= !MF_REPLY_PEND;
        // SAFETY: single-threaded EL1 context; the exclusive `p` borrow ends
        // (NLL) as `rts_unset` captures `nr` internally — it enqueues the
        // proc if the IPC bits were its only block state.
        unsafe { sched::rts_unset(p, RTS_SENDING | RTS_RECEIVING) };
    }
    // Every sender the loop woke was on the dead proc's caller_q; the chain
    // is now meaningless, so drop the head.
    proc_table[target_idx].caller_q = None;

    if let Some(pid) = dead_priv_id {
        let pidx = pid.as_usize();
        if pidx < NR_SYS_PROCS && priv_table[pidx].proc_nr == Some(dead_nr) {
            for pv in priv_table.iter_mut() {
                clear_sys_bit(&mut pv.notify_pending, pid);
            }
            priv_table[pidx].notify_pending = [0; IPC_MAP_CHUNKS];
            priv_table[pidx].proc_nr = None;
        }
    }
}

/// Free every leaf frame in the dead proc's page-table tree, invalidate its
/// TLB entries, free the tree itself, and recycle the ASID. Returns the leaf
/// count (the trace's frame-leak canary). No-op for procs that never ran at
/// EL0 (`ttbr0_pa == 0`: kernel tasks, never-loaded boot servers).
///
/// Shared with `do_exec`, which reuses it to reclaim the *old* image's address
/// space after swapping in the new one — the same "not the active TTBR0"
/// invariant holds there (exec's target is never the running caller).
pub(super) fn teardown_addrspace(ttbr0_pa: u64, dead_asid: u8) -> u64 {
    if ttbr0_pa == 0 {
        return 0;
    }
    let mut freed: u64 = 0;
    // The leaf walk must precede `destroy` (which frees the tables the walk
    // reads). Leaf frames are exclusively the dead proc's: fork copies pages
    // rather than sharing them, and `VMCTL_PT_MAP` allocates fresh frames.
    let _ = walk_leaves(ttbr0_pa, &mut |_va, pa, _prot| {
        free_frame(Frame::from_addr(pa));
        freed += 1;
        Ok(())
    });
    // SAFETY: single-threaded EL1; the dead proc's AS is never the active
    // TTBR0 (self-exit is rejected), so invalidating its ASID cannot yank
    // translations out from under the running caller.
    unsafe { mmu::flush_tlb_asid(dead_asid) };
    AddrSpace { ttbr0_pa }.destroy();
    // SAFETY: this ASID's TLB entries were invalidated just above, so the
    // next `alloc_asid` may hand it to a fresh address space.
    unsafe { asid::free_asid(dead_asid) };
    freed
}

/// Reset the slot for reuse: bump the endpoint generation (every stale
/// endpoint now fails `okendpt`), zero all per-process state, and mark the
/// slot free. `nr` and the bumped `endpoint` survive — `SYS_FORK` consumes
/// the stored endpoint when it re-populates the slot.
fn free_slot(p: &mut Proc) {
    p.endpoint = bump_generation(p.endpoint);
    p.priv_id = None;
    p.misc_flags = 0;
    p.sig_pending = 0;
    p.alarm_at = 0;
    p.user_time = 0;
    p.sys_time = 0;
    p.caller_q = None;
    p.q_link = None;
    p.getfrom_e = NONE;
    p.sendto_e = NONE;
    p.deliver_msg_vir = 0;
    p.ttbr0_pa = 0;
    p.asid = 0;
    p.page_fault_state = PageFaultState::EMPTY;
    p.heap_window = HeapWindow::EMPTY;
    p.scheduler = NONE;
    p.next_ready = None;
    p.rts_flags.store(RTS_SLOT_FREE, Ordering::Relaxed);
}

/// Splice `target_idx` out of the `caller_q` chain of the proc named by
/// `sendto_e`. No-op if the destination is invalid or the target isn't on the
/// chain (both indicate the flags and queue already diverged; nothing to fix
/// here). All borrows are sequential single-slot index accesses.
fn unlink_from_caller_q(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    target_idx: usize,
    sendto_e: Endpoint,
) {
    use minixrs_kernel_shared::endpoint::endpoint_proc;
    let Some(dst_idx) = proc_index(endpoint_proc(sendto_e)) else {
        return;
    };
    let target_nr = proc_table[target_idx].nr;
    let target_link = proc_table[target_idx].q_link;

    if proc_table[dst_idx].caller_q == Some(target_nr) {
        proc_table[dst_idx].caller_q = target_link;
        proc_table[target_idx].q_link = None;
        return;
    }

    // Walk for the predecessor. Chains are Option<ProcNr>-linked and acyclic
    // (each proc is on at most one caller queue), so this terminates.
    let mut cur = proc_table[dst_idx].caller_q;
    while let Some(cur_nr) = cur {
        let Some(cur_idx) = proc_index(cur_nr) else {
            return;
        };
        if proc_table[cur_idx].q_link == Some(target_nr) {
            proc_table[cur_idx].q_link = target_link;
            proc_table[target_idx].q_link = None;
            return;
        }
        cur = proc_table[cur_idx].q_link;
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

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `RECEIVE` — synchronous receive.
//!
//! Translation of MINIX 3 `kernel/proc.c:977 mini_receive()`. Searches in
//! order: pending notifications (synthesized on the fly from the caller's
//! `priv.notify_pending` bitmap), then queued senders in `caller_q`. If
//! nothing matches, the caller blocks with `RTS_RECEIVING` and records
//! the source filter and user-buffer VA so a later [`mini_send`] can
//! deposit the message and unblock us.

use core::sync::atomic::Ordering;

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{ANY, Endpoint};
use minixrs_kernel_shared::error::{EBADSRCDST, ELOCKED, ENOTREADY, OK};
use minixrs_kernel_shared::ipc_const::RECEIVE;

use crate::ipc::deadlock::deadlock_check;
use crate::ipc::notify::build_notify_message;
use crate::proc::flags::{
    MF_DELIVERMSG, MF_REPLY_PEND, RTS_RECEIVING, RTS_SENDING,
};
use crate::proc::priv_struct::IPC_MAP_CHUNKS;
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Priv, Proc, sched};

/// Blocking discipline for [`mini_receive`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum RecvFlags {
    /// `RECEIVE` — block if no matching message is queued.
    Blocking,
    /// Non-blocking (no IPC primitive exposes this today, but the dispatch
    /// path passes it from the `RECEIVE` half of a SENDREC; symmetric to
    /// [`super::send::SendFlags::NonBlocking`]).
    #[allow(dead_code)]
    NonBlocking,
}

/// `RECEIVE` primitive.
pub fn mini_receive(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    src_e: Endpoint,
    user_msg_va: u64,
    flags: RecvFlags,
) -> i32 {
    let Some(caller_idx) = proc_index(caller_nr) else {
        return EBADSRCDST;
    };

    // Record buffer VA up-front. Even if we block here, a later deferred
    // SEND will find this and deposit the message at the right place when
    // the flush path runs.
    proc_table[caller_idx].deliver_msg_vir = user_msg_va;

    // SENDREC mid-flight: SEND half blocked, RECEIVE half just kicked in.
    // Skip pending-message checks and proceed straight to "block on
    // receive". MINIX 3 proc.c:1010.
    let caller_rts = proc_table[caller_idx].rts_flags.load(Ordering::Relaxed);
    let in_mid_sendrec = caller_rts & RTS_SENDING != 0;

    if !in_mid_sendrec {
        // 1. Pending notification?
        if let Some(sender_e) =
            take_pending_notification(proc_table, priv_table, caller_idx, src_e)
        {
            let caller = &mut proc_table[caller_idx];
            build_notify_message(caller, sender_e);
            caller.misc_flags |= MF_DELIVERMSG;
            return OK;
        }

        // 2. (Skip pending async — `mini_senda` is an ENOSYS stub.)

        // 3. Walk caller_q for a sender matching src_e (or ANY).
        if let Some(matched) = walk_caller_q(proc_table, caller_idx, src_e) {
            return matched;
        }
    }

    // Nothing to deliver.
    if flags == RecvFlags::NonBlocking {
        return ENOTREADY;
    }

    if deadlock_check(&*proc_table, RECEIVE, caller_nr, src_e) {
        return ELOCKED;
    }

    let caller = &mut proc_table[caller_idx];
    caller.getfrom_e = src_e;
    // SAFETY: single-threaded SVC invariant; no other borrow into
    // `proc_table` is live.
    unsafe { sched::rts_set(caller, RTS_RECEIVING) };
    OK
}

/// Look for the first pending notification matching `src_e` (or any, if
/// `src_e == ANY`) in `caller`'s `notify_pending` bitmap. Returns the
/// sender's endpoint and clears the bit on success.
fn take_pending_notification(
    proc_table: &[Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_idx: usize,
    src_e: Endpoint,
) -> Option<Endpoint> {
    let Some(caller_priv_id) = proc_table[caller_idx].priv_id else {
        return None;
    };
    let caller_priv_idx = caller_priv_id.as_usize();

    // Snapshot the caller's notify_pending bitmap so the lookup loop can
    // read other priv_table slots without holding a mutable borrow.
    let pending_snapshot: [u32; IPC_MAP_CHUNKS] =
        priv_table[caller_priv_idx].notify_pending;

    // TODO(slice 2.6+): when src_e != ANY, resolve priv_id directly
    // (endpoint → proc_index → priv_id) and test that single bit, rather
    // than scanning the whole bitmap. Current map is 64 bits so the walk
    // is fine, but cost grows with NR_SYS_PROCS.
    for chunk_idx in 0..pending_snapshot.len() {
        let mut chunk = pending_snapshot[chunk_idx];
        while chunk != 0 {
            let bit = chunk.trailing_zeros() as usize;
            let sender_priv_idx = chunk_idx * 32 + bit;
            if sender_priv_idx < NR_SYS_PROCS {
                if let Some(sender_nr) = priv_table[sender_priv_idx].proc_nr {
                    if let Some(sender_idx) = proc_index(sender_nr) {
                        let sender_e = proc_table[sender_idx].endpoint;
                        if src_e == ANY || src_e == sender_e {
                            // Clear bit and return.
                            priv_table[caller_priv_idx].notify_pending[chunk_idx] &=
                                !(1u32 << bit);
                            return Some(sender_e);
                        }
                    }
                }
            }
            chunk &= chunk - 1; // pop lowest set bit and try next
        }
    }
    None
}

/// Walk `caller_q` rooted at `proc_table[caller_idx]` for the first sender
/// matching `src_e` (or any, if `src_e == ANY`). On match, deliver the
/// sender's `send_msg` into the caller's `deliver_msg`, set `MF_DELIVERMSG`,
/// clear `MF_REPLY_PEND` (SENDREC completion), unblock the sender, splice
/// it out of the queue, and return `Some(OK)`. Returns `None` if nothing
/// matches.
fn walk_caller_q(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_idx: usize,
    src_e: Endpoint,
) -> Option<i32> {
    let mut prev: Option<usize> = None;
    let mut cur = proc_table[caller_idx].caller_q;

    while let Some(sender_nr) = cur {
        let sender_idx = proc_index(sender_nr).expect("caller_q entry in range");
        let sender_e = proc_table[sender_idx].endpoint;
        let next = proc_table[sender_idx].q_link;

        if src_e == ANY || src_e == sender_e {
            // Deliver: copy the buffered message into the caller's
            // deliver_msg (m_source was already stamped by mini_send).
            let msg = proc_table[sender_idx].send_msg;
            let caller = &mut proc_table[caller_idx];
            caller.deliver_msg = msg;
            caller.misc_flags |= MF_DELIVERMSG;
            caller.misc_flags &= !MF_REPLY_PEND;

            // Splice sender out of the queue.
            match prev {
                None => proc_table[caller_idx].caller_q = next,
                Some(p_idx) => proc_table[p_idx].q_link = next,
            }
            {
                let sender = &mut proc_table[sender_idx];
                sender.q_link = None;
                // SAFETY: single-threaded SVC invariant.
                unsafe { sched::rts_unset(sender, RTS_SENDING) };
            }
            return Some(OK);
        }

        prev = Some(sender_idx);
        cur = next;
    }
    None
}

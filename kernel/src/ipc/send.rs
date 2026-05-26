//! `SEND` / `SENDNB` — synchronous (and non-blocking) send.
//!
//! Translation of MINIX 3 `kernel/proc.c:880 mini_send()`. The blocking
//! variant either delivers immediately to a waiting receiver or queues
//! the caller on the destination's `caller_q` via the `q_link` chain.
//! The non-blocking variant returns `ENOTREADY` instead of queueing.

use minix4_kernel_shared::ProcNr;
use minix4_kernel_shared::com::NR_SYS_PROCS;
use minix4_kernel_shared::endpoint::{Endpoint, endpoint_proc};
use minix4_kernel_shared::error::{EBADSRCDST, ECALLDENIED, ELOCKED, ENOTREADY, OK};
use minix4_kernel_shared::ipc_const::SEND;

use crate::ipc::deadlock::deadlock_check;
use crate::ipc::message::copy_msg_from_user;
use crate::ipc::notify::will_receive;
use crate::proc::bitmap::get_sys_bit;
use crate::proc::flags::{MF_DELIVERMSG, MF_REPLY_PEND, RTS_RECEIVING, RTS_SENDING};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Priv, Proc, sched};

/// Blocking discipline for [`mini_send`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum SendFlags {
    /// `SEND` — block if the destination cannot accept the message now.
    Blocking,
    /// `SENDNB` — return [`ENOTREADY`] instead of blocking.
    NonBlocking,
}

/// `SEND` / `SENDNB` primitive.
pub fn mini_send(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    dst_e: Endpoint,
    user_msg_va: u64,
    flags: SendFlags,
) -> i32 {
    // TODO(phase 3+): okendpt-style (gen, slot) validation — stale
    // endpoints after slot recycle should return EDEADSRCDST. Phase 2
    // has no slot recycling, so `endpoint_proc` alone is sufficient.
    let dst_nr = endpoint_proc(dst_e);
    let Some(dst_idx) = proc_index(dst_nr) else {
        return EBADSRCDST;
    };
    let Some(caller_idx) = proc_index(caller_nr) else {
        return EBADSRCDST;
    };

    let caller_endpoint = proc_table[caller_idx].endpoint;
    let Some(caller_priv_id) = proc_table[caller_idx].priv_id else {
        return ECALLDENIED;
    };
    let Some(dst_priv_id) = proc_table[dst_idx].priv_id else {
        return ECALLDENIED;
    };

    // ipc_to bitmap: caller must be permitted to send to dst's priv slot.
    if !get_sys_bit(&priv_table[caller_priv_id.as_usize()].ipc_to, dst_priv_id) {
        return ECALLDENIED;
    }

    // Copy the outgoing message out of user memory. The sender cannot
    // spoof `m_source` — MINIX 3 stomps it with the kernel's view of the
    // caller's endpoint (proc.c:918).
    //
    // Note: copy precedes deadlock_check deliberately — a userspace bug
    // (bad VA) surfaces as EFAULT rather than masquerading as a
    // kernel-detected ELOCKED. Keep this ordering.
    let mut msg = match copy_msg_from_user(user_msg_va) {
        Ok(m) => m,
        Err(e) => return e,
    };
    msg.m_source = caller_endpoint;

    // Immediate-delivery path: destination is RECEIVE-blocked and would
    // accept us (or ANY).
    {
        let dst = &mut proc_table[dst_idx];
        if will_receive(dst, caller_endpoint) {
            dst.deliver_msg = msg;
            dst.misc_flags |= MF_DELIVERMSG;
            dst.misc_flags &= !MF_REPLY_PEND;
            // SAFETY: single-threaded SVC invariant; no other borrow into
            // `proc_table` is live.
            unsafe { sched::rts_unset(dst, RTS_RECEIVING) };
            return OK;
        }
    }

    if flags == SendFlags::NonBlocking {
        return ENOTREADY;
    }

    // Deadlock check before blocking. Reborrow as shared.
    if deadlock_check(&*proc_table, SEND, caller_nr, dst_e) {
        return ELOCKED;
    }

    // Block: stash msg in caller.send_msg, record sendto_e, mark
    // RTS_SENDING, append to dst.caller_q via q_link.
    {
        let caller = &mut proc_table[caller_idx];
        caller.send_msg = msg;
        caller.sendto_e = dst_e;
        caller.q_link = None;
        // SAFETY: single-threaded SVC invariant.
        unsafe { sched::rts_set(caller, RTS_SENDING) };
    }

    enqueue_on_caller_q(proc_table, dst_idx, caller_nr);
    OK
}

/// Append `nr` to the tail of the `caller_q` rooted at `proc_table[dst_idx]`.
fn enqueue_on_caller_q(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    dst_idx: usize,
    nr: ProcNr,
) {
    match proc_table[dst_idx].caller_q {
        None => {
            proc_table[dst_idx].caller_q = Some(nr);
        }
        Some(head) => {
            // Walk the chain to the tail (entry whose q_link is None).
            let mut cur = head;
            loop {
                let cur_idx = proc_index(cur).expect("caller_q entry in range");
                match proc_table[cur_idx].q_link {
                    Some(next) => cur = next,
                    None => {
                        proc_table[cur_idx].q_link = Some(nr);
                        return;
                    }
                }
            }
        }
    }
}


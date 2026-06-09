// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `NOTIFY` — non-blocking notification.
//!
//! Translation of MINIX 3 `kernel/proc.c:1132 mini_notify()`. Never
//! blocks the caller. If the destination is `RECEIVE`-blocked and would
//! accept a message from the caller (or `ANY`), the kernel synthesizes a
//! [`NOTIFY_MESSAGE`]-typed message directly into the destination's
//! `deliver_msg` and unblocks them. Otherwise it sets a bit in the
//! destination's `priv.notify_pending` bitmap so the next `RECEIVE` will
//! collect the deferred notification.

use core::sync::atomic::Ordering;

use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{ANY, Endpoint, endpoint_proc};
use minixrs_kernel_shared::error::{EBADSRCDST, ECALLDENIED, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_kernel_shared::message::Message;

use crate::proc::bitmap::{get_sys_bit, set_sys_bit};
use crate::proc::flags::{MF_DELIVERMSG, MF_REPLY_PEND, RTS_RECEIVING, RTS_SENDING};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Priv, Proc, sched};

/// MINIX 3 `WILLRECEIVE(dst, src_e)`. Returns true when `dst` is blocked
/// in `RECEIVE` (and not also in `SEND`, to dodge the SENDREC mid-flight
/// case) and its receive-from filter matches `src_e`.
pub(crate) fn will_receive(dst: &Proc, src_e: Endpoint) -> bool {
    let rts = dst.rts_flags.load(Ordering::Relaxed);
    if rts & RTS_RECEIVING == 0 || rts & RTS_SENDING != 0 {
        return false;
    }
    let getfrom = dst.getfrom_e;
    getfrom == ANY || getfrom == src_e
}

/// Build a notification message into `dst.deliver_msg`. Mirrors MINIX 3
/// `BuildNotifyMessage` (proc.h:99-115): zero the message, stamp
/// `m_type = NOTIFY_MESSAGE`, leave payload zero.
pub(crate) fn build_notify_message(dst: &mut Proc, source_e: Endpoint) {
    dst.deliver_msg = Message {
        m_source: source_e,
        m_type: NOTIFY_MESSAGE,
        payload: [0; 96],
    };
}

/// `NOTIFY` primitive. Returns `OK` on success or an IPC error code.
pub fn mini_notify(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: minixrs_kernel_shared::ProcNr,
    dst_e: Endpoint,
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

    // Caller's endpoint and (for the deferred bitmap path) caller's priv_id.
    let caller_endpoint = proc_table[caller_idx].endpoint;
    let Some(caller_priv_id) = proc_table[caller_idx].priv_id else {
        return ECALLDENIED;
    };
    let Some(dst_priv_id) = proc_table[dst_idx].priv_id else {
        // No priv slot on the destination ⇒ no notify_pending bitmap to set
        // and (per MINIX 3's semantics) no ipc target.
        return ECALLDENIED;
    };

    // Permission: caller.ipc_to must permit dst_priv_id.
    if !get_sys_bit(&priv_table[caller_priv_id.as_usize()].ipc_to, dst_priv_id) {
        return ECALLDENIED;
    }

    let dst = &mut proc_table[dst_idx];

    if will_receive(dst, caller_endpoint) && dst.misc_flags & MF_REPLY_PEND == 0 {
        // Immediate delivery — synthesize the notification and unblock.
        build_notify_message(dst, caller_endpoint);
        dst.misc_flags |= MF_DELIVERMSG;
        // SAFETY: single-threaded SVC/IRQ-masked invariant; no other
        // borrow into `proc_table` is live (we hold the exclusive `dst`
        // reference and let NLL end it at the end of this match arm).
        unsafe { sched::rts_unset(dst, RTS_RECEIVING) };
        return OK;
    }

    // Deferred — record in dst's notify_pending bitmap.
    set_sys_bit(
        &mut priv_table[dst_priv_id.as_usize()].notify_pending,
        caller_priv_id,
    );
    OK
}

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

use minixrs_kernel_shared::com::{CLOCK, NR_SYS_PROCS, PM_PROC_NR, SYSTEM};
use minixrs_kernel_shared::endpoint::{ANY, Endpoint};
use minixrs_kernel_shared::error::{EBADSRCDST, ECALLDENIED, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_kernel_shared::message::Message;

use crate::proc::bitmap::{get_sys_bit, set_sys_bit};
use crate::proc::flags::{MF_DELIVERMSG, MF_REPLY_PEND, RTS_RECEIVING, RTS_SENDING};
use crate::proc::table::{N_PROC_SLOTS, okendpt, proc_index};
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
    // Slots recycle as of slice 4.6 (SYS_EXIT frees + bumps the generation),
    // so the destination must pass full okendpt validation — a stale endpoint
    // returns EDEADSRCDST instead of reaching the slot's new occupant.
    let dst_idx = match okendpt(proc_table, dst_e) {
        Ok(idx) => idx,
        Err(e) => return e,
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

/// Kernel-originated alarm notification (slice 4.4). Deliver a `NOTIFY` from the
/// `CLOCK` kernel task to the proc at `owner_idx` whose one-shot `SYS_SETALARM`
/// timer just expired.
///
/// Modeled on [`mini_notify`]'s delivery half, with two differences: the source
/// is `CLOCK` rather than a user caller, and there is **no `ipc_to` permission
/// check** — the kernel originates the alarm, exactly as
/// [`super::send::mini_pf_send`] originates a page fault. (Routing through
/// `mini_notify` would in fact *fail*: `CLOCK`'s `ipc_to` bitmap is empty, since
/// only `SRV_T` slots get theirs filled at boot.)
///
/// Immediate delivery if the owner is `RECEIVE`-blocked and would accept from
/// `CLOCK`; otherwise the bit is recorded in the owner's `notify_pending`
/// against CLOCK's priv slot, so the owner's next `RECEIVE` synthesizes the
/// message via [`super::receive`]'s `take_pending_notification`.
pub(crate) fn deliver_alarm(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    owner_idx: usize,
) {
    // CLOCK is a boot-time kernel task: `proc::init` always populates its slot
    // (IMAGE index 2) and priv id, so a missing entry is a structural bug — halt
    // loudly rather than silently drop the alarm.
    let clock_idx = proc_index(CLOCK).expect("CLOCK in proc table");
    let clock_e = proc_table[clock_idx].endpoint;
    let clock_priv_id = proc_table[clock_idx]
        .priv_id
        .expect("CLOCK priv populated by proc::init");

    let owner = &mut proc_table[owner_idx];
    if will_receive(owner, clock_e) && owner.misc_flags & MF_REPLY_PEND == 0 {
        // Immediate delivery — synthesize the notification and unblock.
        build_notify_message(owner, clock_e);
        owner.misc_flags |= MF_DELIVERMSG;
        // SAFETY: single-threaded IRQ/clock context; the exclusive `owner`
        // borrow ends (NLL) as `rts_unset` captures `nr`, so no other
        // PROC_TABLE borrow aliases.
        unsafe { sched::rts_unset(owner, RTS_RECEIVING) };
        return;
    }

    // Deferred — record against CLOCK's priv slot in the owner's bitmap. An
    // owner with no priv slot has no bitmap, so the alarm is dropped; RS (the
    // only armer in slice 4.4) always has one, so this can't strand it.
    let Some(owner_priv_id) = owner.priv_id else {
        return;
    };
    set_sys_bit(
        &mut priv_table[owner_priv_id.as_usize()].notify_pending,
        clock_priv_id,
    );
}

/// Kernel-originated ksig notification (slice 4.5). Deliver a `NOTIFY` from
/// the `SYSTEM` kernel task to PM after `cause_sig` marks a target proc
/// signal-pending, so PM wakes and drains via `SYS_GETKSIG` / `SYS_ENDKSIG`.
///
/// Modeled on [`deliver_alarm`]: the source is a kernel task and there is
/// **no `ipc_to` permission check** — the kernel originates the notification,
/// and routing through [`mini_notify`] would in fact *fail* because SYSTEM's
/// `ipc_to` bitmap is empty (only `SRV_T` slots get theirs filled at boot).
///
/// Immediate delivery if PM is `RECEIVE`-blocked and would accept from
/// `SYSTEM`; otherwise the bit is recorded in PM's `notify_pending` against
/// SYSTEM's priv slot, so PM's next `RECEIVE` synthesizes the message via
/// [`super::receive`]'s `take_pending_notification`.
///
/// Takes the caller's exclusive table slices — never re-materializes the
/// statics (`kernel_call_sendrec` already holds them).
pub(crate) fn deliver_ksig(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
) {
    // SYSTEM and PM are boot-time slots: `proc::init` always populates them,
    // so a missing entry is a structural bug — halt loudly rather than
    // silently drop the notification.
    let system_idx = proc_index(SYSTEM).expect("SYSTEM in proc table");
    let system_e = proc_table[system_idx].endpoint;
    let system_priv_id = proc_table[system_idx]
        .priv_id
        .expect("SYSTEM priv populated by proc::init");
    let pm_idx = proc_index(PM_PROC_NR).expect("PM in proc table");

    let pm = &mut proc_table[pm_idx];
    if will_receive(pm, system_e) && pm.misc_flags & MF_REPLY_PEND == 0 {
        // Immediate delivery — synthesize the notification and unblock.
        build_notify_message(pm, system_e);
        pm.misc_flags |= MF_DELIVERMSG;
        // SAFETY: single-threaded EL1 context; the exclusive `pm` borrow ends
        // (NLL) as `rts_unset` captures `nr`, so no other PROC_TABLE borrow
        // aliases.
        unsafe { sched::rts_unset(pm, RTS_RECEIVING) };
        return;
    }

    // Deferred — record against SYSTEM's priv slot in PM's bitmap. PM's boot
    // priv slot always exists, so the notification can't be dropped.
    let pm_priv_id = pm.priv_id.expect("PM priv populated by init_boot_image");
    set_sys_bit(
        &mut priv_table[pm_priv_id.as_usize()].notify_pending,
        system_priv_id,
    );
}

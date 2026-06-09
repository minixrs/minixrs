// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SEND` / `SENDNB` — synchronous (and non-blocking) send.
//!
//! Translation of MINIX 3 `kernel/proc.c:880 mini_send()`. The blocking
//! variant either delivers immediately to a waiting receiver or queues
//! the caller on the destination's `caller_q` via the `q_link` chain.
//! The non-blocking variant returns `ENOTREADY` instead of queueing.

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::VM_PAGEFAULT;
use minixrs_kernel_shared::com::{NR_SYS_PROCS, VM_PROC_NR};
use minixrs_kernel_shared::endpoint::{Endpoint, endpoint_proc};
use minixrs_kernel_shared::error::{EBADSRCDST, ECALLDENIED, ELOCKED, ENOTREADY, OK};
use minixrs_kernel_shared::ipc_const::SEND;
use minixrs_kernel_shared::message::Message;

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

/// Kernel-originated SEND of a `VM_PAGEFAULT` message to the VM server on a
/// faulting process's behalf (slice 3.4).
///
/// Unlike [`mini_send`] there is no user buffer and no caller running an SVC:
/// the kernel constructs the message, and the *faulting* process plays the role
/// of the (already-blocked) sender. The faulter is expected to already carry
/// `RTS_PAGEFAULT` (set by `do_page_fault`); we additionally mark it
/// `RTS_SENDING` only when VM can't receive immediately, so that when VM later
/// picks it off the `caller_q` ([`super::receive::mini_receive`]'s
/// `walk_caller_q` clears `RTS_SENDING`) the lingering `RTS_PAGEFAULT` keeps it
/// blocked until `VMCTL_CLEAR_PAGEFAULT`.
///
/// No `ipc_to`/permission check — the send is kernel-originated, not a user
/// trap. No deadlock check either: the faulter is already blocked on a fault
/// and VM is its sink, so the SEND↔RECV cycle the detector hunts for can't form.
///
/// `m_source` identifies the faulter; the payload carries the fault address
/// (`0..8`, u64) and fault flags (`8..12`, u32) so VM needs no `GET_PAGEFAULT`
/// round-trip to resolve.
pub fn mini_pf_send(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    faulting_nr: ProcNr,
    far: u64,
    flags: u32,
) {
    // Both lookups are boot-time invariants: `do_page_fault` has already
    // blocked the faulter on RTS_PAGEFAULT before calling us, so a silent
    // bailout here would strand it blocked forever with no diagnostic. Halt
    // loudly instead (CLAUDE.md: hard assert for invariants that would
    // otherwise silently corrupt — here, liveness).
    let fault_idx = proc_index(faulting_nr).expect("mini_pf_send: faulter not in proc table");
    let vm_idx = proc_index(VM_PROC_NR).expect("mini_pf_send: VM server not in proc table");

    let fault_endpoint = proc_table[fault_idx].endpoint;
    let vm_endpoint = proc_table[vm_idx].endpoint;

    let mut msg = Message {
        m_source: fault_endpoint,
        m_type: VM_PAGEFAULT,
        payload: [0u8; 96],
    };
    msg.payload[0..8].copy_from_slice(&far.to_ne_bytes());
    msg.payload[8..12].copy_from_slice(&flags.to_ne_bytes());

    // Immediate delivery: VM is receive-blocked and would accept us.
    {
        let vm = &mut proc_table[vm_idx];
        if will_receive(vm, fault_endpoint) {
            vm.deliver_msg = msg;
            vm.misc_flags |= MF_DELIVERMSG;
            vm.misc_flags &= !MF_REPLY_PEND;
            // SAFETY: single-threaded EL1 page-fault context; no other borrow
            // into `proc_table` is live.
            unsafe { sched::rts_unset(vm, RTS_RECEIVING) };
            return;
        }
    }

    // VM busy: queue the faulter as a blocked sender on VM's caller_q.
    {
        let caller = &mut proc_table[fault_idx];
        caller.send_msg = msg;
        caller.sendto_e = vm_endpoint;
        caller.q_link = None;
        // RTS_PAGEFAULT is already set, so the faulter is already off the run
        // queue; rts_set just OR-s in RTS_SENDING (won't double-dequeue).
        // SAFETY: single-threaded EL1 page-fault context.
        unsafe { sched::rts_set(caller, RTS_SENDING) };
    }
    enqueue_on_caller_q(proc_table, vm_idx, faulting_nr);
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


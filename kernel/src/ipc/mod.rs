// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! IPC subsystem — the heart of the MINIX microkernel.
//!
//! `do_ipc` is the single SVC
//! entry point; it materializes mutable borrows of `PROC_TABLE` and
//! `PRIV_TABLE`, performs trap-mask gating, and dispatches to one of the
//! per-primitive handlers (`mini_send`, `mini_receive`, `mini_notify`,
//! `mini_senda`). The handlers operate on the explicit table slices —
//! they never touch the static directly — which keeps each primitive
//! testable in isolation and prevents the two-`&mut`-from-one-
//! `UnsafeCell` UB hazard that arises if each primitive re-borrows
//! individual slots.
//!
//! After every SVC, `el1_svc_tail` runs `schedule_next` to pick the
//! highest-priority runnable proc and flush any pending `MF_DELIVERMSG`
//! into its user buffer. That keeps the run queue honest when the caller
//! blocks (`rts_set` already dequeued it) and ensures a receiver that
//! got unblocked sees its message before resuming.

mod deadlock;
mod message;
mod notify;
mod receive;
mod send;
mod senda;

// User-buffer copy helpers — exposed to `crate::system` so the SYSTEM
// SENDREC fast path can read/write the request and reply without each
// caller dragging in `ipc::message` paths directly.
pub(crate) use message::{copy_msg_from_user, copy_msg_to_user};
pub(crate) use notify::deliver_ksig;

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::endpoint::{ANY, Endpoint};
use minixrs_kernel_shared::error::{EBADCALL, ECALLDENIED, EFAULT, ETRAPDENIED, OK};
use minixrs_kernel_shared::ipc_const::{NOTIFY, RECEIVE, SEND, SENDA, SENDNB, SENDREC};

use crate::arch::ArchRegisterFrame;
use crate::proc::flags::{MF_DELIVERMSG, MF_MSGFAILED, MF_REPLY_PEND};
use crate::proc::sched::{self, CURRENT_PROC_NR};
use crate::proc::table::{N_PROC_SLOTS, priv_table_mut_slice, proc_index, proc_table_mut_slice};
use crate::proc::{Priv, Proc};
use crate::uart::Uart;

use receive::RecvFlags;
use send::SendFlags;

/// Running total of IPC calls dispatched. Sampled at [`TRACE_EVERY`]
/// intervals for the boot-time observability trace.
static CALL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Cadence of the boot-time IPC trace. Stub C's synchronous kernel-call loop
/// dominates the count by orders of magnitude, so in practice this samples C —
/// every other caller is far too rare to hit the modulus. Making the cadence a
/// runtime knob (DS-driven, or load-scaled) has been considered repeatedly and
/// never been worth it: the per-handler head-carved `[ksys SYS_*]` traces are
/// what actually answer "did X happen", and they are unsampled.
const TRACE_EVERY: u64 = 100;

/// Number of leading SVCs to trace unconditionally, regardless of
/// [`TRACE_EVERY`]. Gives the early-boot output enough granularity to
/// show each stub's first IPC call — slice 2.6 added a third stub (C)
/// whose fast-path SENDRECs to SYSTEM outpace stubs A and B by orders
/// of magnitude, so without this aid the ping-pong looks like it
/// regressed even though A↔B are still cooperating fine. Slice 4.5
/// widened 12 → 24: six boot servers' startup SENDRECs (GET_WHOAMI +
/// DS publish + replies + RS's first heartbeat round) exceed the old
/// window. PM itself boots after stub C's kernel-call flood begins, so
/// its handshake still lands past any practical head window — since
/// slice 5.1 its `[diag pm] sef ready` line is the boot evidence instead
/// (it can only fire after PM's GET_WHOAMI succeeded).
const TRACE_HEAD: u64 = 24;

/// IPC dispatch. Called from `trap.S` immediately after the SVC entry
/// stub has saved the caller's registers into `frame`.
///
/// Argument convention (mirrors MINIX 3 `kernel/proc.c:609 do_ipc`):
///
/// | reg | meaning                          |
/// |-----|----------------------------------|
/// | x0  | source/destination endpoint      |
/// | x1  | IPC primitive number             |
/// | x2  | pointer to the user's `Message`  |
/// | x3  | (SENDA only) table length        |
///
/// The result is written back into `frame.x[0]` — the SVC restore path
/// puts it in the caller's `x0` on `eret`.
#[unsafe(no_mangle)]
pub extern "C" fn do_ipc(frame: &mut ArchRegisterFrame) {
    let src_dst_e: Endpoint = frame.x[0] as i32;
    let call_nr: i32 = frame.x[1] as i32;
    let user_msg_va: u64 = frame.x[2];
    let extra: u64 = frame.x[3];

    // SAFETY: SVC dispatch is single-threaded (DAIF.I masked at EL1); no
    // other code can write to PROC_TABLE/PRIV_TABLE before we return.
    let proc_table = unsafe { proc_table_mut_slice() };
    let priv_table = unsafe { priv_table_mut_slice() };

    let cur_raw = CURRENT_PROC_NR.load(Ordering::Relaxed);
    let caller_nr = ProcNr::new(cur_raw);

    let result = dispatch(
        proc_table,
        priv_table,
        caller_nr,
        call_nr,
        src_dst_e,
        user_msg_va,
        extra,
    );

    frame.x[0] = result as u64;

    // EFAULT is produced by nothing but the user-message copy helpers, so a
    // result-keyed trace here unambiguously names a bad user pointer and
    // covers all three immediate copy sites (`mini_send`, and the kernel-call
    // request/reply copies, whose errors propagate up through `dispatch`).
    // Uncounted, for the reason `trace_deliver_efault` documents.
    if result == EFAULT {
        let _ = writeln!(
            Uart::new(),
            "[efault] proc={} nr={} call={call_nr} va={user_msg_va:#x}",
            proc_name0(proc_table, caller_nr) as char,
            caller_nr.get()
        );
    }

    let n = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= TRACE_HEAD || n.is_multiple_of(TRACE_EVERY) {
        let mut uart = Uart::new();
        let _ = writeln!(
            uart,
            "[ipc {n}] caller={caller_nr} call={call_nr} target={src_dst_e:#x} result={result}",
        );
    }
}

/// First byte of a proc's name — the one-char id the `[as]` / `[pf]` / `[ksys]`
/// traces use. `?` when the slot is out of range or unnamed.
fn proc_name0(proc_table: &[Proc; N_PROC_SLOTS], nr: ProcNr) -> u8 {
    match proc_index(nr) {
        Some(idx) if proc_table[idx].name[0] != 0 => proc_table[idx].name[0],
        _ => b'?',
    }
}

fn dispatch(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    call_nr: i32,
    src_dst_e: Endpoint,
    user_msg_va: u64,
    extra: u64,
) -> i32 {
    // Permission gate: caller must have a priv slot whose trap_mask
    // permits this primitive. Mirrors MINIX 3 `do_sync_ipc:562-568`.
    let Some(caller_idx) = proc_index(caller_nr) else {
        return EBADCALL;
    };
    let Some(caller_priv_id) = proc_table[caller_idx].priv_id else {
        return ECALLDENIED;
    };
    let trap_mask = priv_table[caller_priv_id.as_usize()].trap_mask;
    // ANY-source endpoint is only legal as a RECEIVE filter.
    if src_dst_e == ANY && call_nr != RECEIVE {
        return EBADCALL;
    }
    // Match on call_nr first so unknown primitives return EBADCALL.
    // Each arm gates on its own trap_mask bit so a valid-but-denied
    // primitive returns ETRAPDENIED. Keeps error precedence stable as
    // new primitives are added (no risk of an out-of-range call_nr
    // sneaking past the range check and getting ETRAPDENIED from a
    // stale `1 << call_nr` shift).
    match call_nr {
        SEND => trap_gate(trap_mask, SEND, || {
            send::mini_send(
                proc_table,
                priv_table,
                caller_nr,
                src_dst_e,
                user_msg_va,
                SendFlags::Blocking,
            )
        }),
        RECEIVE => trap_gate(trap_mask, RECEIVE, || {
            receive::mini_receive(
                proc_table,
                priv_table,
                caller_nr,
                src_dst_e,
                user_msg_va,
                RecvFlags::Blocking,
            )
        }),
        SENDREC => trap_gate(trap_mask, SENDREC, || {
            // Fast-path: SENDREC to the SYSTEM endpoint never enters mini_send.
            // MINIX 3 `proc.c::do_ipc` does the same divert — SYSTEM has no
            // scheduler context, so a real send would block forever.
            if src_dst_e == crate::system::system_endpoint() {
                crate::system::kernel_call_sendrec(proc_table, priv_table, caller_nr, user_msg_va)
            } else {
                do_sendrec(proc_table, priv_table, caller_nr, src_dst_e, user_msg_va)
            }
        }),
        NOTIFY => trap_gate(trap_mask, NOTIFY, || {
            notify::mini_notify(proc_table, priv_table, caller_nr, src_dst_e)
        }),
        SENDNB => trap_gate(trap_mask, SENDNB, || {
            send::mini_send(
                proc_table,
                priv_table,
                caller_nr,
                src_dst_e,
                user_msg_va,
                SendFlags::NonBlocking,
            )
        }),
        SENDA => trap_gate(trap_mask, SENDA, || {
            senda::mini_senda(
                proc_table,
                priv_table,
                caller_nr,
                user_msg_va,
                extra as usize,
            )
        }),
        _ => EBADCALL,
    }
}

/// Permission gate: returns `ETRAPDENIED` if the caller's `trap_mask`
/// doesn't permit `call_nr`, otherwise runs `f` and returns its result.
///
/// The shift is done in `u32` to dodge two pitfalls: (a) `1u16 <<
/// SENDA(16)` panics in debug builds and wraps to 0 in release, and (b)
/// every caller funnels through the match in `dispatch`, so `call_nr`
/// is one of the IPC primitive constants — but the explicit u32 widen
/// makes the gate robust to future re-use.
///
/// TODO(slice 2.6+): widen `Priv::trap_mask` from `u16` to `u32` to
/// match MINIX 3's `unsigned int` so SENDA's bit (16) actually fits.
/// Until then SENDA is dispatcher-denied here even though
/// `mini_senda` would otherwise reply `ENOSYS` — see the note on
/// `senda::mini_senda`.
#[inline]
fn trap_gate(trap_mask: u16, call_nr: i32, f: impl FnOnce() -> i32) -> i32 {
    let bit = 1u32.wrapping_shl(call_nr as u32);
    if (trap_mask as u32) & bit == 0 {
        return ETRAPDENIED;
    }
    f()
}

/// `SENDREC` is the atomic-send-then-receive primitive used by every
/// `_syscall()` wrapper. SEND half blocks (or delivers); on success we
/// mark `MF_REPLY_PEND` and run the RECEIVE half against the same partner.
fn do_sendrec(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    dst_e: Endpoint,
    user_msg_va: u64,
) -> i32 {
    let send_result = send::mini_send(
        proc_table,
        priv_table,
        caller_nr,
        dst_e,
        user_msg_va,
        SendFlags::Blocking,
    );
    if send_result != OK {
        return send_result;
    }
    let Some(caller_idx) = proc_index(caller_nr) else {
        return EBADCALL;
    };
    proc_table[caller_idx].misc_flags |= MF_REPLY_PEND;
    receive::mini_receive(
        proc_table,
        priv_table,
        caller_nr,
        dst_e,
        user_msg_va,
        RecvFlags::Blocking,
    )
}

/// Copy a pending IPC message out to the user buffer recorded at
/// RECEIVE-time and clear `MF_DELIVERMSG`. Called from
/// `sched::schedule_next` on every EL1 → EL0 transition.
///
/// The copy walks `p`'s page tables rather than the active TTBR0 (slice 5.1),
/// so this works whether or not `p` is the process the kernel is about to
/// resume.
///
/// On failure the message is still consumed — there is no sender left to
/// re-queue it against — and `EFAULT` is patched into the receiver's parked
/// `x0`: the receiver asked for delivery into a buffer it does not have mapped
/// (or cannot write), and that is its error to see. `MF_MSGFAILED` records the
/// outcome for a future signals slice; nothing reads it yet.
pub fn flush_deliver_msg(p: &mut Proc) {
    if p.misc_flags & MF_DELIVERMSG == 0 {
        return;
    }
    let rc = message::copy_msg_to_user(p.ttbr0_pa, p.deliver_msg_vir, &p.deliver_msg);
    p.misc_flags &= !MF_DELIVERMSG;
    match rc {
        Ok(()) => p.misc_flags &= !MF_MSGFAILED,
        Err(e) => {
            // The MINIX `retreg` pattern: write the error into the saved SVC
            // frame the trap restore path hands back on `eret`. Same mechanism
            // `do_exit::unblock_dependents` uses for EDEADSRCDST. When the
            // receiver is the *current* proc (an immediate delivery inside its
            // own RECEIVE), `p.regs` is the very frame `do_ipc` just wrote OK
            // into — this write lands last and wins, which is what we want.
            p.regs.x[0] = e as i64 as u64;
            p.misc_flags |= MF_MSGFAILED;
            trace_deliver_efault(p.name[0], p.nr, p.deliver_msg_vir);
        }
    }
}

/// Emit the `[efault deliver]` trace for a failed deferred delivery.
///
/// Uncounted, unlike the sampled `[ipc {n}]` line: this can only fire on a
/// receiver that named a bad buffer, so every occurrence is worth seeing — and
/// an unsampled marker is stable enough for `tests/qemu-boot.expected`, which
/// the counted forms are not.
fn trace_deliver_efault(name0: u8, nr: ProcNr, va: u64) {
    let id = if name0 != 0 { name0 } else { b'?' };
    let _ = writeln!(
        Uart::new(),
        "[efault deliver] proc={} nr={} va={va:#x}",
        id as char,
        nr.get()
    );
}

/// Kernel-originated page-fault notification: send `VM_PAGEFAULT` to the VM
/// server on behalf of `faulting_nr`. Called from the EL1 page-fault handler
/// (`arch::aarch64::exception::do_page_fault`) after it has recorded the fault
/// state and set `RTS_PAGEFAULT` on the faulter.
///
/// Materializes the proc-table slice here (the page-fault handler otherwise
/// works through `current_proc_mut`), keeping the "only `ipc` materializes the
/// table for the primitives" discipline intact.
pub fn send_pagefault_to_vm(faulting_nr: ProcNr, far: u64, flags: u32) {
    // SAFETY: EL1 page-fault context — single-threaded, DAIF.I masked. The
    // record/`rts_set` borrow in `do_page_fault` has already ended, so no other
    // PROC_TABLE reference is live.
    let proc_table = unsafe { proc_table_mut_slice() };
    send::mini_pf_send(proc_table, faulting_nr, far, flags);
}

/// Kernel-originated `SCHEDULING_NO_QUANTUM` notification: send it to
/// `scheduler_e` on behalf of the preempted proc `preempted_nr` (slice 4.3).
/// Called from `proc::sched::reschedule` after it has dequeued the preempted
/// proc and left `RTS_NO_QUANTUM` set.
///
/// Materializes the proc-table slice here (reschedule otherwise works through
/// `proc_slot_mut`), keeping the "only `ipc` materializes the table for the
/// primitives" discipline intact — exactly like [`send_pagefault_to_vm`].
pub fn send_no_quantum(preempted_nr: ProcNr, scheduler_e: Endpoint) {
    // SAFETY: IRQ/reschedule context — single-threaded, DAIF.I masked. The
    // `cur` borrow in `reschedule` has already ended, so no other PROC_TABLE
    // reference is live.
    let proc_table = unsafe { proc_table_mut_slice() };
    send::mini_sched_no_quantum_send(proc_table, preempted_nr, scheduler_e);
}

/// Cadence of the boot-time alarm-fire trace.
const ALARM_TRACE_EVERY: u64 = 100;
/// Leading alarm fires to trace unconditionally (head carve-out), so RS's first
/// few periodic fires show even though stub C's `SYS_GETINFO` flood dwarfs the
/// modulo sampler (same reasoning as [`TRACE_HEAD`] / `sched::NOQ_TRACE_HEAD`).
const ALARM_TRACE_HEAD: u64 = 6;
/// Count of alarm fires, for the head/modulo trace above.
static ALARM_COUNT: AtomicU64 = AtomicU64::new(0);

/// Fire every per-proc one-shot alarm due at or before `now` (slice 4.4).
///
/// Called from `clock::tick` *only* when the `EARLIEST_ALARM` fast-path gate
/// says an alarm is due, so the O(N) proc-table scan is paid rarely. For each
/// expired [`Proc::alarm_at`] it clears the field, delivers a kernel-originated
/// `NOTIFY` from `CLOCK` to the owner ([`notify::deliver_alarm`]), and traces
/// the fire; it then recomputes the minimum of the still-armed deadlines and
/// writes it back into the clock gate via [`crate::clock::set_earliest_alarm`].
///
/// Materializes the proc/priv slices here — same "only `ipc` materializes the
/// tables for delivery" discipline as [`send_pagefault_to_vm`] /
/// [`send_no_quantum`].
///
/// [`Proc::alarm_at`]: crate::proc::Proc::alarm_at
pub fn fire_expired_alarms(now: u64) {
    // SAFETY: IRQ/clock context — single-threaded, DAIF.I masked. `clock::tick`
    // has taken no PROC_TABLE/PRIV_TABLE borrow before calling us.
    let proc_table = unsafe { proc_table_mut_slice() };
    let priv_table = unsafe { priv_table_mut_slice() };

    let mut next_earliest: u64 = 0;
    for idx in 0..N_PROC_SLOTS {
        let at = proc_table[idx].alarm_at;
        if at == 0 {
            continue; // disarmed slot
        }
        if at > now {
            next_earliest = fold_earliest(next_earliest, at); // armed but not yet due
            continue;
        }
        // Due: disarm, deliver the CLOCK notification, trace the fire.
        proc_table[idx].alarm_at = 0;
        notify::deliver_alarm(proc_table, priv_table, idx);
        trace_alarm_fire(proc_table[idx].name[0], proc_table[idx].nr, now);
    }

    crate::clock::set_earliest_alarm(next_earliest);
}

/// Fold one still-armed deadline `at` into the running next-earliest, treating
/// 0 as "none". Pulled out of [`fire_expired_alarms`] to keep that scan flat.
fn fold_earliest(current: u64, at: u64) -> u64 {
    if current == 0 || at < current {
        at
    } else {
        current
    }
}

/// Emit the head/modulo-sampled `[alarm N]` fire trace. Bumps the fire counter
/// and prints only on the head carve-out or every [`ALARM_TRACE_EVERY`]th fire
/// — kept out of [`fire_expired_alarms`] so its scan stays under the cognitive-
/// complexity bar.
fn trace_alarm_fire(name0: u8, nr: ProcNr, now: u64) {
    let n = ALARM_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n > ALARM_TRACE_HEAD && !n.is_multiple_of(ALARM_TRACE_EVERY) {
        return;
    }
    let id = if name0 != 0 { name0 } else { b'?' };
    let _ = writeln!(
        Uart::new(),
        "[alarm {n}] owner={} nr={} at={now}",
        id as char,
        nr.get()
    );
}

/// SVC-tail shim. `trap.S` calls this between `do_ipc` and
/// `el1_return_to_user`; it picks the next runnable proc (which may be
/// the same caller, may be a higher-priority receiver that just
/// unblocked, or may be someone else entirely if the caller blocked)
/// and flushes any pending message into the new current proc's user
/// buffer.
#[unsafe(no_mangle)]
pub extern "C" fn el1_svc_tail() {
    // SAFETY: SVC dispatch context — single-threaded, DAIF.I masked,
    // no other PROC_TABLE/RUNQ references live at this point.
    unsafe { sched::schedule_next() }
}

// ---------------------------------------------------------------------------
// Re-exported / locally needed `kernel_shared` items.
// ---------------------------------------------------------------------------

use minixrs_kernel_shared::com::NR_SYS_PROCS;

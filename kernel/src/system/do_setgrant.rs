// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_SETGRANT` — register the caller's grant table (slice 5.2, decision D4).
//!
//! A granting process keeps its [`GrantEntry`] array in its *own* address space
//! and tells the kernel where it is. Nothing is copied here: the kernel records
//! `(addr, entries)` in the caller's [`Priv`] and reads entries back out of that
//! address space on demand, in [`do_safecopy`](super::do_safecopy). That is
//! MINIX 3's shape (`kernel/system/do_setgrant.c`) and it is what lets a granter
//! revoke a grant unilaterally, with no kernel round trip and no kernel memory
//! charged per grant.
//!
//! The fields being written (`Priv::grant_table` / `grant_entries`) have existed
//! unused since slice 2.2; this is the call that starts honoring them.
//!
//! ## Why this is routed with the target-taking calls
//!
//! It acts only on the caller, but it needs `&mut Priv` — and
//! [`dispatch_caller_local`](super::dispatch_caller_local) hands out a shared
//! `&Priv`. So, like `do_kill`'s deferred-notify write, it is dispatched from
//! the arm of `kernel_call_dispatch` that still holds the privilege table.
//!
//! ## Shared privilege slots are rejected
//!
//! Ordinary user processes all share one privilege slot (`USER_PRIV_ID`), so a
//! grant table registered there would claim to describe every user process's
//! memory at one address. That is nonsense, and worse, it would let any one of
//! them point the kernel at a table another reads. Unreachable today — the
//! shared slot has an empty `k_call_mask`, so a user process cannot make kernel
//! calls at all — but the check is what keeps a future grant-capable user class
//! from inheriting the hole.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset  | field                    | direction |
//! |---------|--------------------------|-----------|
//! |  4..8   | entry count (i32)        | in        |
//! | 16..24  | table base address (u64) | in        |
//!
//! No reply payload; `m_type` is `OK`, `EINVAL`, or `EPERM`.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::error::{EINVAL, EPERM, OK};
use minixrs_kernel_shared::grant::{GRANT_ENTRY_SIZE, GRANT_MAX_IDX, GrantEntry};
use minixrs_kernel_shared::message::{Message, user_va_ok};

use crate::proc::Priv;
use crate::uart::Uart;

// The kernel indexes the caller's table by `GRANT_ENTRY_SIZE` stride; if that
// ever stopped matching the shared struct, every entry past the first would be
// read from the wrong offset. Pin it here, where the stride is first applied.
const _: () = assert!(core::mem::size_of::<GrantEntry>() == GRANT_ENTRY_SIZE);

/// Registrations traced in full before the trace goes quiet. A server registers
/// once (the pool is lazily registered on its first grant), so a head carve-out
/// catches every real call — the `[ksys N]` every-100th sampler in
/// `kernel_call_sendrec` would miss these entirely.
const SETGRANT_TRACE_HEAD: u64 = 6;
static SETGRANT_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_SETGRANT`. Records (or clears) the caller's grant-table registration.
pub(super) fn do_setgrant(caller_priv: &mut Priv, caller_nr: ProcNr, msg: &Message) -> i32 {
    // A shared privilege slot has no single owning process, so it cannot own a
    // grant table either. `proc_nr` naming *this* caller is what proves the slot
    // is dedicated (`populate_priv` sets it; `populate_user_priv` leaves it
    // `None` precisely because the USER slot is shared).
    if caller_priv.proc_nr != Some(caller_nr) {
        return EPERM;
    }

    let entries = read_i32(msg, 4);
    if entries < 0 {
        return EINVAL;
    }
    // Ids pack the index into `GRANT_SHIFT` bits, so a table larger than the
    // index field could hold entries no id can name.
    if entries as u32 > GRANT_MAX_IDX + 1 {
        return EINVAL;
    }

    // Zero entries clears the registration — the granter is done granting, and
    // every outstanding id now fails the idx range check in `verify_grant`.
    if entries == 0 {
        caller_priv.grant_table = 0;
        caller_priv.grant_entries = 0;
        trace(caller_nr, 0);
        return OK;
    }

    let addr = read_u64(msg, 16);
    // A `GrantEntry` array is 8-aligned, so the message-grade `user_va_ok`
    // (which demands 8-alignment) is exactly the right gate here — unlike the
    // byte buffers `do_safecopy` handles, which use `user_range_ok`.
    let bytes = entries as usize * GRANT_ENTRY_SIZE;
    if !user_va_ok(addr, bytes) {
        return EINVAL;
    }

    // Deliberately *not* probed for mappability: the table may legitimately be
    // in memory that is not faulted in yet, and a granter that registers a bad
    // address only ever hurts itself — `verify_grant` turns a walk miss into
    // `EPERM` for the grantee.
    caller_priv.grant_table = addr;
    caller_priv.grant_entries = entries as u32;
    trace(caller_nr, entries);
    OK
}

fn trace(caller_nr: ProcNr, entries: i32) {
    let n = SETGRANT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= SETGRANT_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_SETGRANT] proc={} entries={entries}",
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
fn read_u64(msg: &Message, off: usize) -> u64 {
    u64::from_ne_bytes(
        msg.payload[off..off + 8]
            .try_into()
            .expect("payload in range"),
    )
}

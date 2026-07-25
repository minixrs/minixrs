// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_COPY` — raw privileged cross-address-space copy (slice 5.2, D4).
//!
//! MINIX 3's `sys_datacopy` (`kernel/system/do_copy.c`): the caller names a
//! source and a destination by endpoint and address, and the kernel moves the
//! bytes. No grant is involved. It rides the same engine as `SYS_SAFECOPY`
//! ([`copy_between_as`]) — the only difference is that the ungoverned form
//! skips the grant resolution entirely.
//!
//! It exists because small control-plane reads do not justify a grant round
//! trip: VFS fetching a path string out of a caller's buffer is the canonical
//! use (D4). Bulk data movement goes through grants.
//!
//! ## Trust model
//!
//! There is **no per-target authorization**, exactly as in
//! [`do_vmctl`](super::do_vmctl): any caller that reaches this handler may name
//! any two processes and copy between them. The single gate is
//! `Priv::k_call_mask` granting `SYS_COPY`, checked in `kernel_call_dispatch`
//! before we run — and the shared USER privilege has an empty mask, so ordinary
//! user processes cannot reach this call at all. This is the deliberate MINIX
//! mechanism/policy split: `SYS_COPY` is privileged, and the servers holding it
//! are trusted not to name processes they have no business touching. If a
//! future slice hands `SYS_COPY` to a less-trusted process, target
//! authorization must be added before that lands.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset  | field                            | direction |
//! |---------|----------------------------------|-----------|
//! |  0..4   | source endpoint (i32, `SELF` ok) | in        |
//! |  4..8   | destination endpoint (i32)       | in        |
//! | 16..24  | source address (u64)             | in        |
//! | 24..32  | destination address (u64)        | in        |
//! | 32..40  | byte count (u64)                 | in        |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_kernel_shared::message::{Message, user_range_ok};

use crate::mm::uaccess::copy_between_as;
use crate::proc::Proc;
use crate::proc::table::N_PROC_SLOTS;
use crate::uart::Uart;

/// Head-carved trace budget, as in `do_safecopy` — a low-rate control-plane
/// call the `[ksys N]` sampler would never catch.
const COPY_TRACE_HEAD: u64 = 6;
static COPY_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_COPY`. Resolves both endpoints and copies `bytes` between them.
pub(super) fn do_copy(proc_table: &[Proc; N_PROC_SLOTS], caller_nr: ProcNr, msg: &Message) -> i32 {
    let src_e: Endpoint = read_i32(msg, 0);
    let dst_e: Endpoint = read_i32(msg, 4);
    let src_addr = read_u64(msg, 16);
    let dst_addr = read_u64(msg, 24);
    let bytes = read_u64(msg, 32);

    // `SELF` resolves to the caller, so a server can copy into its own buffer
    // without knowing its own endpoint. Everything else goes through
    // `okendpt`, so a stale endpoint is `EDEADSRCDST` rather than the slot's
    // new occupant.
    let src_idx = match super::resolve_target(proc_table, caller_nr, src_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };
    let dst_idx = match super::resolve_target(proc_table, caller_nr, dst_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };

    let src_ttbr0 = proc_table[src_idx].ttbr0_pa;
    let dst_ttbr0 = proc_table[dst_idx].ttbr0_pa;
    if src_ttbr0 == 0 || dst_ttbr0 == 0 {
        return EINVAL; // a kernel task / unset slot has no user address space
    }
    if !user_range_ok(src_addr, bytes) || !user_range_ok(dst_addr, bytes) {
        return EINVAL;
    }

    // `bytes` passed `user_range_ok`, so it is below `USER_VA_TOP` and the cast
    // cannot truncate on a 64-bit target.
    let rc = match copy_between_as(src_ttbr0, src_addr, dst_ttbr0, dst_addr, bytes as usize) {
        Ok(()) => OK,
        Err(e) => e,
    };

    let n = COPY_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= COPY_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_COPY] src={src_e} dst={dst_e} bytes={bytes} result={rc}",
        );
    }
    rc
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

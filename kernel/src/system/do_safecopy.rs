// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_SAFECOPY` — grant-mediated cross-address-space copy (slice 5.2, D4).
//!
//! The governed half of the copy engine. A grantee names a *grant id* it was
//! handed out-of-band; the kernel resolves the granter, reads the grant entry
//! back out of the **granter's own** address space, validates it, and only then
//! copies. Nothing about the grant is cached kernel-side, so a granter revokes
//! by writing its own memory — no kernel round trip, and no kernel bookkeeping
//! that could go stale. That is MINIX 3's model
//! (`kernel/system/do_safecopy.c`).
//!
//! One call number covers both directions: the payload's selector picks
//! `SAFECOPY_FROM` (read the granted range) or `SAFECOPY_TO` (write it), and
//! everything but the source/destination assignment is identical.
//!
//! ## Validation order (MINIX's, kept deliberately)
//!
//! 1. the granter endpoint resolves ([`okendpt`] — a stale generation is
//!    `EDEADSRCDST`, the established convention);
//! 2. the granter has a registered table and the id's index is inside it;
//! 3. the entry reads back (a walk miss is reported as `EPERM`, not `EFAULT` —
//!    a granter with a bad table address is a *permission* failure from the
//!    grantee's point of view, and leaking "unmapped" would let a grantee probe
//!    the granter's address space);
//! 4. `CPF_USED | CPF_VALID` are both set;
//! 5. the entry's sequence matches the id's — this is what makes a revoked and
//!    reissued slot unreachable through the old id;
//! 6. the requested access is granted (`CPF_READ` / `CPF_WRITE`);
//! 7. the grantee endpoint is the caller;
//! 8. `offset + bytes` neither overflows nor exceeds the granted length.
//!
//! ## Kinds
//!
//! An entry must claim **exactly one** kind. `CPF_DIRECT` resolves the granted
//! memory to the granter itself. `CPF_MAGIC` resolves it to `who_from` — a
//! third party — and additionally requires the *granter* to hold `SYS_PROC`.
//! MINIX hardcodes a proc-nr list (only VFS and MIB may issue magic grants);
//! minix.rs gates on the existing server-grade privilege marker instead, which
//! is the same trust boundary without a name list to keep in sync.
//! `CPF_INDIRECT` — and any other combination, including none — is `EINVAL`
//! (D4 defers re-granting a received grant; nothing in Phase 5 needs it).
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset  | field                             | direction |
//! |---------|-----------------------------------|-----------|
//! |  0..4   | direction (`SAFECOPY_FROM`/`_TO`) | in        |
//! |  4..8   | granter endpoint (i32)            | in        |
//! |  8..12  | grant id (i32)                    | in        |
//! | 16..24  | offset within the granted range   | in        |
//! | 24..32  | the caller's own buffer (u64)     | in        |
//! | 32..40  | byte count (u64)                  | in        |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::{SAFECOPY_FROM, SAFECOPY_TO};
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, EPERM, OK};
use minixrs_kernel_shared::grant::{
    CPF_DIRECT, CPF_KIND_MASK, CPF_MAGIC, CPF_READ, CPF_USED, CPF_VALID, CPF_WRITE,
    GRANT_ENTRY_SIZE, GrantEntry, grant_idx, grant_seq, grant_valid,
};
use minixrs_kernel_shared::message::{Message, user_range_ok};

use crate::mm::uaccess::{copy_between_as, copy_from_user_as};
use crate::proc::flags::SYS_PROC;
use crate::proc::table::{N_PROC_SLOTS, okendpt, proc_index};
use crate::proc::{Priv, Proc};
use crate::uart::Uart;

/// Head-carved trace budget. Safecopy is a low-rate call in slice 5.2 (one
/// demo exchange), so the `[ksys N]` every-100th sampler would never show it —
/// the same reasoning as `do_vmctl`'s `VMCTL_TRACE_HEAD`.
const SAFECOPY_TRACE_HEAD: u64 = 6;
static SAFECOPY_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_SAFECOPY`. Validates the named grant, then copies in the requested
/// direction between the granted range and the caller's own buffer.
pub(super) fn do_safecopy(
    proc_table: &[Proc; N_PROC_SLOTS],
    priv_table: &[Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    msg: &Message,
) -> i32 {
    let direction = read_i32(msg, 0);
    let granter_e: Endpoint = read_i32(msg, 4);
    let gid = read_i32(msg, 8);
    let offset = read_u64(msg, 16);
    let caller_addr = read_u64(msg, 24);
    let bytes = read_u64(msg, 32);

    // The access the *grant* must carry follows from the direction: reading the
    // granted range needs CPF_READ, writing it needs CPF_WRITE. Selector 0 (a
    // zeroed payload) is invalid rather than defaulting to a direction.
    let access = match direction {
        SAFECOPY_FROM => CPF_READ,
        SAFECOPY_TO => CPF_WRITE,
        _ => return EINVAL,
    };

    let caller_idx = match proc_index(caller_nr) {
        Some(idx) => idx,
        None => return EINVAL,
    };
    let caller_ttbr0 = proc_table[caller_idx].ttbr0_pa;
    if caller_ttbr0 == 0 {
        return EINVAL; // caller never runs at EL0 (kernel task / unset slot)
    }
    if !user_range_ok(caller_addr, bytes) {
        return EINVAL;
    }

    let (granted_ttbr0, granted_addr) = match verify_grant(
        proc_table, priv_table, caller_idx, granter_e, gid, offset, bytes, access,
    ) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // `bytes` survived `user_range_ok`, so it is below `USER_VA_TOP` and the
    // cast cannot truncate on a 64-bit target.
    let len = bytes as usize;
    let result = if direction == SAFECOPY_FROM {
        copy_between_as(granted_ttbr0, granted_addr, caller_ttbr0, caller_addr, len)
    } else {
        copy_between_as(caller_ttbr0, caller_addr, granted_ttbr0, granted_addr, len)
    };

    let rc = match result {
        Ok(()) => OK,
        Err(e) => e,
    };

    let n = SAFECOPY_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= SAFECOPY_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_SAFECOPY] granter={granter_e} gid={gid} bytes={bytes} dir={direction} result={rc}",
        );
    }
    rc
}

/// Resolve and validate the grant `gid` issued by `granter_e` for `access`.
///
/// Returns `(ttbr0_pa, va)` of the granted range's *owning* address space —
/// the granter for a direct grant, `who_from` for a magic one — with `offset`
/// already applied.
///
/// Every rejection is `EPERM` except the two that are genuinely malformed
/// requests (`EINVAL` for an indirect/unknown kind) and the endpoint resolution
/// failures `okendpt` reports. Collapsing the checks onto one code is
/// deliberate: a grantee learns "you may not do this", never *which* of the
/// granter's internal states blocked it.
#[allow(clippy::too_many_arguments)] // one argument per validated field; the
// alternative is a request struct that exists only to be destructured here
// (the `elf.rs` / boot-helper precedent).
fn verify_grant(
    proc_table: &[Proc; N_PROC_SLOTS],
    priv_table: &[Priv; NR_SYS_PROCS],
    caller_idx: usize,
    granter_e: Endpoint,
    gid: i32,
    offset: u64,
    bytes: u64,
    access: u32,
) -> Result<(u64, u64), i32> {
    let granter_idx = okendpt(proc_table, granter_e)?;
    let granter = &proc_table[granter_idx];

    // The granter's registered table lives in the granter's own address space.
    let granter_priv = granter
        .priv_id
        .and_then(|id| priv_table.get(id.as_usize()))
        .ok_or(EPERM)?;
    if granter_priv.grant_table == 0 || granter_priv.grant_entries == 0 {
        return Err(EPERM);
    }

    if !grant_valid(gid) {
        return Err(EPERM);
    }
    let idx = grant_idx(gid);
    if idx >= granter_priv.grant_entries {
        return Err(EPERM);
    }

    // Read the entry out of the granter's address space. A walk miss here is a
    // granter-side problem; the grantee sees EPERM, not EFAULT (see the module
    // note on why the distinction is hidden).
    let entry_va = granter_priv
        .grant_table
        .checked_add(idx as u64 * GRANT_ENTRY_SIZE as u64)
        .ok_or(EPERM)?;
    let mut raw = [0u8; GRANT_ENTRY_SIZE];
    copy_from_user_as(granter.ttbr0_pa, entry_va, &mut raw).map_err(|_| EPERM)?;
    let entry = GrantEntry::from_ne_bytes(&raw);

    if entry.flags & (CPF_USED | CPF_VALID) != (CPF_USED | CPF_VALID) {
        return Err(EPERM);
    }
    // Staleness: a revoked slot that has since been reissued carries a bumped
    // sequence, so the old id names an index that exists but a generation that
    // does not.
    if entry.seq != grant_seq(gid) {
        return Err(EPERM);
    }
    if entry.flags & access != access {
        return Err(EPERM);
    }
    // The grant names exactly one permitted user. Compare against the caller's
    // *stored* endpoint (generation included), not the proc number.
    if entry.who_to != proc_table[caller_idx].endpoint {
        return Err(EPERM);
    }
    let end = offset.checked_add(bytes).ok_or(EPERM)?;
    if end > entry.len {
        return Err(EPERM);
    }

    // Kind. Matching the masked bits (rather than testing them one at a time)
    // is what makes "exactly one kind" the rule: an entry claiming two kinds is
    // malformed, not an invitation to pick the more permissive reading.
    let owner_idx = match entry.flags & CPF_KIND_MASK {
        CPF_DIRECT => granter_idx,
        CPF_MAGIC => {
            // Magic: the granter vouches for a *third party's* memory, so it
            // must be server-grade. MINIX names VFS and MIB explicitly; the
            // privilege flag is the same boundary without a list to maintain.
            if granter_priv.flags & SYS_PROC == 0 {
                return Err(EPERM);
            }
            okendpt(proc_table, entry.who_from)?
        }
        // Indirect is deferred by D4 — nothing in Phase 5 re-grants a grant it
        // received — and lands here with no kind bit, or several, as a
        // malformed request rather than a denial.
        _ => return Err(EINVAL),
    };

    let owner_ttbr0 = proc_table[owner_idx].ttbr0_pa;
    if owner_ttbr0 == 0 {
        return Err(EPERM); // the memory's owner never runs at EL0
    }
    let addr = entry.addr.checked_add(offset).ok_or(EPERM)?;
    if !user_range_ok(addr, bytes) {
        return Err(EPERM);
    }
    Ok((owner_ttbr0, addr))
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

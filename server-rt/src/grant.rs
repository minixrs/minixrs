// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Grant-table client — the granting half of `SYS_SAFECOPY` (slice 5.2, D4).
//!
//! A server that wants to hand a peer access to some memory keeps a
//! [`GrantPool`] and calls [`grant_direct`](GrantPool::grant_direct) (its own
//! memory) or [`grant_magic`](GrantPool::grant_magic) (a third party's, for a
//! server-grade granter). The returned grant id travels to the grantee in an
//! ordinary message payload; the grantee names it in `SYS_SAFECOPY` and the
//! kernel reads the entry back out of *this* address space to validate it.
//!
//! ## The pool is a value, not a static
//!
//! MINIX's `cpf_*` functions work on a process-global table. Here the pool is
//! an ordinary value the server owns — typically a local in `main` that lives
//! as long as the receive loop. That is what keeps `server-rt`
//! `#![forbid(unsafe_code)]`: there is no static mutable state to reach through
//! an `UnsafeCell`, and no `unsafe impl Sync`.
//!
//! It also makes registration **self-healing**. The kernel is told one address;
//! if the pool were ever moved, that address would name freed stack.
//! [`ensure_registered`](GrantPool::ensure_registered) compares the pool's
//! current address against the one last registered and re-issues `SYS_SETGRANT`
//! when they differ — taking a pointer and casting it to an integer are both
//! safe operations, so this costs no `unsafe`.
//!
//! ## Sizing
//!
//! A server's stack is one page, and the pool is `N * 32` bytes plus two words.
//! Keep `N` small — the slice-5.2 demo uses 8 (256 bytes). `N` must also fit
//! the grant id's index field; an over-large pool is rejected by the kernel at
//! registration (`EINVAL`) rather than silently truncated.
//!
//! Only [`ensure_registered`](GrantPool::ensure_registered) traps into the
//! kernel; the allocation, sequence-bump, and revoke logic are pure helpers over
//! a borrowed slice and carry the host unit tests, the
//! `servers/ds/src/registry.rs` shape.

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::SYS_SETGRANT;
use minixrs_kernel_shared::com::{SYSTEM, boot_endpoint};
use minixrs_kernel_shared::endpoint::{Endpoint, NONE};
use minixrs_kernel_shared::error::{EINVAL, ENOMEM, OK};
use minixrs_kernel_shared::grant::{
    CPF_ACCESS_MASK, CPF_DIRECT, CPF_MAGIC, CPF_USED, CPF_VALID, GRANT_MAX_SEQ, GrantEntry,
    grant_id, grant_idx, grant_seq, grant_valid,
};

/// A server's grant table plus the bookkeeping the kernel does not keep.
///
/// `N` is the number of simultaneously outstanding grants the server can hold.
/// Slots are reused as grants are revoked; each reuse bumps the sequence
/// number, so an id for a revoked grant can never reach the slot's new
/// occupant.
pub struct GrantPool<const N: usize> {
    /// The table itself — this is the memory the kernel reads entries from, so
    /// its address is what `SYS_SETGRANT` registers.
    entries: [GrantEntry; N],
    /// Sequence number for the next allocation. Wraps at [`GRANT_MAX_SEQ`].
    next_seq: u32,
    /// Address last registered with the kernel; 0 before the first
    /// registration. Compared against the pool's live address so a moved pool
    /// re-registers rather than leaving the kernel pointed at stale memory.
    registered_at: usize,
}

impl<const N: usize> Default for GrantPool<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> GrantPool<N> {
    /// An empty pool. Nothing is registered with the kernel until the first
    /// grant is issued — a server that never grants pays no kernel call.
    pub const fn new() -> Self {
        Self {
            entries: [GrantEntry::EMPTY; N],
            next_seq: 0,
            registered_at: 0,
        }
    }

    /// Grant `who_to` access to `[addr, addr + len)` **in this process's own
    /// address space**.
    ///
    /// `access` is `CPF_READ`, `CPF_WRITE`, or both. Returns the grant id to
    /// hand the grantee, or `EINVAL` for a malformed request / `ENOMEM` when
    /// every slot is in use. A failed `SYS_SETGRANT` is returned verbatim.
    pub fn grant_direct(
        &mut self,
        who_to: Endpoint,
        addr: u64,
        len: u64,
        access: u32,
    ) -> Result<i32, i32> {
        self.issue(CPF_DIRECT | access, who_to, NONE, addr, len)
    }

    /// Grant `who_to` access to `[addr, addr + len)` in **`who_from`'s** address
    /// space — the third-party (magic) form.
    ///
    /// The kernel additionally requires this process to hold server-grade
    /// privilege, so a magic grant from an ordinary process is rejected at
    /// safecopy time with `EPERM`.
    pub fn grant_magic(
        &mut self,
        who_to: Endpoint,
        who_from: Endpoint,
        addr: u64,
        len: u64,
        access: u32,
    ) -> Result<i32, i32> {
        self.issue(CPF_MAGIC | access, who_to, who_from, addr, len)
    }

    /// Revoke `gid`, freeing its slot for reuse.
    ///
    /// No separate kernel call announces this: the kernel re-reads the entry on
    /// every safecopy, so clearing the slot *is* the revocation. `EINVAL` if
    /// `gid` does not name a live grant of this pool (already revoked, wrong
    /// sequence, out of range).
    ///
    /// That is also exactly why this re-registers first. Clearing the slot only
    /// revokes anything if the kernel is reading *this* copy of the table — a
    /// pool that moved since its last registration would have its entry cleared
    /// here while the kernel kept honouring the stale address. A revocation that
    /// silently fails to take effect is the worst failure mode this type has, so
    /// it pays the address comparison (and, only if it actually moved, one
    /// `SYS_SETGRANT`) before touching the slot.
    pub fn revoke(&mut self, gid: i32) -> Result<(), i32> {
        if self.registered_at == 0 {
            // Nothing was ever granted, so nothing can be revoked — and a pool
            // that never granted still makes no kernel calls, as `new` promises.
            return Err(EINVAL);
        }
        self.ensure_registered()?;
        revoke_in(&mut self.entries, gid)
    }

    /// Number of slots currently holding a live grant. Useful to a server that
    /// wants to fail early rather than discover `ENOMEM` mid-request.
    pub fn used(&self) -> usize {
        used_in(&self.entries)
    }

    /// Register the pool with the kernel if its address has changed (or it was
    /// never registered). The only part of this module that traps.
    fn ensure_registered(&mut self) -> Result<(), i32> {
        let addr = (&raw const self.entries) as usize;
        if addr == self.registered_at {
            return Ok(());
        }
        let rc = sys_setgrant(addr as u64, N as i32);
        if rc != OK {
            return Err(rc);
        }
        self.registered_at = addr;
        Ok(())
    }

    /// Shared body of the two grant constructors: make sure the kernel knows
    /// where the table is, then claim a slot.
    fn issue(
        &mut self,
        flags: u32,
        who_to: Endpoint,
        who_from: Endpoint,
        addr: u64,
        len: u64,
    ) -> Result<i32, i32> {
        self.ensure_registered()?;
        alloc_grant_in(
            &mut self.entries,
            &mut self.next_seq,
            flags,
            who_to,
            who_from,
            addr,
            len,
        )
    }
}

/// Claim the first free slot and write a grant into it. Returns the packed id.
///
/// Rejects a malformed request up front (`EINVAL`): a zero-length or NULL
/// range, a range that overflows, or flags naming no access right — each of
/// which would produce a grant the kernel can only ever refuse, at a moment far
/// from the mistake.
#[allow(clippy::too_many_arguments)] // the fields of one grant entry, minus the
// sequence the helper assigns itself; bundling them into a struct would only be
// destructured here.
fn alloc_grant_in(
    entries: &mut [GrantEntry],
    next_seq: &mut u32,
    flags: u32,
    who_to: Endpoint,
    who_from: Endpoint,
    addr: u64,
    len: u64,
) -> Result<i32, i32> {
    if addr == 0 || len == 0 {
        return Err(EINVAL);
    }
    if addr.checked_add(len).is_none() {
        return Err(EINVAL);
    }
    if flags & CPF_ACCESS_MASK == 0 {
        return Err(EINVAL);
    }
    let Some(idx) = entries.iter().position(|e| e.flags & CPF_USED == 0) else {
        return Err(ENOMEM);
    };

    let seq = *next_seq;
    *next_seq = (*next_seq + 1) & GRANT_MAX_SEQ;
    entries[idx] = GrantEntry {
        flags: flags | CPF_USED | CPF_VALID,
        seq,
        who_to,
        who_from,
        addr,
        len,
    };
    Ok(grant_id(idx as u32, seq))
}

/// Clear the slot `gid` names, after checking that the id actually matches the
/// grant living there. Zeroing the whole entry is what makes the slot free
/// (`CPF_USED` clear) and simultaneously invalidates the id.
fn revoke_in(entries: &mut [GrantEntry], gid: i32) -> Result<(), i32> {
    if !grant_valid(gid) {
        return Err(EINVAL);
    }
    let Some(e) = entries.get_mut(grant_idx(gid) as usize) else {
        return Err(EINVAL);
    };
    if e.flags & CPF_USED == 0 || e.seq != grant_seq(gid) {
        return Err(EINVAL);
    }
    *e = GrantEntry::EMPTY;
    Ok(())
}

/// Count the live grants in a table.
fn used_in(entries: &[GrantEntry]) -> usize {
    entries.iter().filter(|e| e.flags & CPF_USED != 0).count()
}

/// Issue one `SYS_SETGRANT` naming `addr` as this process's grant table.
///
/// Returns the IPC trap's error if the SENDREC itself failed, else the kernel's
/// reply `m_type` — the two-step check every `server-rt` kernel-call helper
/// uses, since a trap-level failure and a call-level failure arrive by
/// different routes.
fn sys_setgrant(addr: u64, entries: i32) -> i32 {
    let mut msg = Message {
        m_source: 0,
        m_type: SYS_SETGRANT,
        payload: [0u8; 96],
    };
    msg.payload[4..8].copy_from_slice(&entries.to_ne_bytes());
    msg.payload[16..24].copy_from_slice(&addr.to_ne_bytes());

    let trap_rc = ipc_sendrec(boot_endpoint(SYSTEM), &mut msg);
    if trap_rc != OK {
        return trap_rc;
    }
    msg.m_type
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE};

    const CAP: usize = 4;

    fn table() -> [GrantEntry; CAP] {
        [GrantEntry::EMPTY; CAP]
    }

    fn direct(
        t: &mut [GrantEntry; CAP],
        seq: &mut u32,
        who_to: Endpoint,
        addr: u64,
        len: u64,
    ) -> Result<i32, i32> {
        alloc_grant_in(t, seq, CPF_DIRECT | CPF_READ, who_to, NONE, addr, len)
    }

    #[test]
    fn a_fresh_table_is_entirely_free() {
        let t = table();
        assert_eq!(used_in(&t), 0);
    }

    #[test]
    fn allocation_fills_the_entry_the_kernel_will_read() {
        let mut t = table();
        let mut seq = 0;
        let gid = direct(&mut t, &mut seq, 7, 0x1000, 64).expect("slot available");
        let e = t[grant_idx(gid) as usize];
        assert_eq!(e.flags & CPF_USED, CPF_USED);
        assert_eq!(e.flags & CPF_VALID, CPF_VALID);
        assert_eq!(e.flags & CPF_DIRECT, CPF_DIRECT);
        assert_eq!(e.flags & CPF_READ, CPF_READ);
        assert_eq!(e.who_to, 7);
        assert_eq!(e.who_from, NONE);
        assert_eq!(e.addr, 0x1000);
        assert_eq!(e.len, 64);
        // The id must name this slot at this generation, or the kernel's
        // sequence check rejects a perfectly good grant.
        assert_eq!(e.seq, grant_seq(gid));
    }

    #[test]
    fn magic_grants_record_the_third_party() {
        let mut t = table();
        let mut seq = 0;
        let gid = alloc_grant_in(&mut t, &mut seq, CPF_MAGIC | CPF_READ, 7, 10, 0x10_0000, 8)
            .expect("slot available");
        let e = t[grant_idx(gid) as usize];
        assert_eq!(e.flags & CPF_MAGIC, CPF_MAGIC);
        assert_eq!(e.flags & CPF_DIRECT, 0);
        assert_eq!(e.who_from, 10);
    }

    #[test]
    fn access_bits_are_recorded_exactly_as_requested() {
        // The kernel tests `flags & access == access`, so a read-only grant must
        // not carry the write bit — and a read/write grant must carry both.
        let mut t = table();
        let mut seq = 0;
        let ro = direct(&mut t, &mut seq, 1, 0x1000, 8).unwrap();
        assert_eq!(t[grant_idx(ro) as usize].flags & CPF_WRITE, 0);

        let rw = alloc_grant_in(
            &mut t,
            &mut seq,
            CPF_DIRECT | CPF_READ | CPF_WRITE,
            1,
            NONE,
            0x2000,
            8,
        )
        .unwrap();
        let flags = t[grant_idx(rw) as usize].flags;
        assert_eq!(flags & CPF_READ, CPF_READ);
        assert_eq!(flags & CPF_WRITE, CPF_WRITE);
    }

    #[test]
    fn successive_grants_take_distinct_slots_and_sequences() {
        let mut t = table();
        let mut seq = 0;
        let a = direct(&mut t, &mut seq, 1, 0x1000, 8).unwrap();
        let b = direct(&mut t, &mut seq, 2, 0x2000, 8).unwrap();
        assert_ne!(grant_idx(a), grant_idx(b));
        assert_ne!(grant_seq(a), grant_seq(b));
        assert_eq!(used_in(&t), 2);
    }

    #[test]
    fn malformed_requests_are_rejected_before_a_slot_is_taken() {
        let mut t = table();
        let mut seq = 0;
        assert_eq!(direct(&mut t, &mut seq, 1, 0, 8), Err(EINVAL)); // NULL
        assert_eq!(direct(&mut t, &mut seq, 1, 0x1000, 0), Err(EINVAL)); // empty
        assert_eq!(direct(&mut t, &mut seq, 1, u64::MAX, 2), Err(EINVAL)); // overflow
        // No access bits: a grant nothing could ever use.
        assert_eq!(
            alloc_grant_in(&mut t, &mut seq, CPF_DIRECT, 1, NONE, 0x1000, 8),
            Err(EINVAL)
        );
        assert_eq!(used_in(&t), 0);
    }

    #[test]
    fn a_full_table_is_enomem() {
        let mut t = table();
        let mut seq = 0;
        for i in 0..CAP {
            assert!(direct(&mut t, &mut seq, 1, 0x1000 + i as u64 * 0x1000, 8).is_ok());
        }
        assert_eq!(direct(&mut t, &mut seq, 1, 0x9000, 8), Err(ENOMEM));
        assert_eq!(used_in(&t), CAP);
    }

    #[test]
    fn revoke_frees_the_slot() {
        let mut t = table();
        let mut seq = 0;
        let gid = direct(&mut t, &mut seq, 1, 0x1000, 8).unwrap();
        assert_eq!(revoke_in(&mut t, gid), Ok(()));
        assert_eq!(used_in(&t), 0);
        assert_eq!(t[grant_idx(gid) as usize].flags & CPF_USED, 0);
    }

    #[test]
    fn revoking_twice_fails_the_second_time() {
        let mut t = table();
        let mut seq = 0;
        let gid = direct(&mut t, &mut seq, 1, 0x1000, 8).unwrap();
        assert_eq!(revoke_in(&mut t, gid), Ok(()));
        assert_eq!(revoke_in(&mut t, gid), Err(EINVAL));
    }

    #[test]
    fn a_revoked_id_does_not_name_the_slots_next_occupant() {
        // The whole point of the sequence field. Revoke, re-grant into the same
        // slot, and the old id must not be a valid name for the new grant.
        let mut t = table();
        let mut seq = 0;
        let old = direct(&mut t, &mut seq, 1, 0x1000, 8).unwrap();
        revoke_in(&mut t, old).unwrap();
        let new = direct(&mut t, &mut seq, 2, 0x2000, 8).unwrap();
        assert_eq!(grant_idx(old), grant_idx(new), "same slot reused");
        assert_ne!(old, new);
        assert_ne!(t[grant_idx(new) as usize].seq, grant_seq(old));
        // And the stale id cannot revoke the new grant either.
        assert_eq!(revoke_in(&mut t, old), Err(EINVAL));
        assert_eq!(used_in(&t), 1);
    }

    #[test]
    fn out_of_range_and_negative_ids_are_rejected() {
        let mut t = table();
        assert_eq!(revoke_in(&mut t, -1), Err(EINVAL));
        assert_eq!(revoke_in(&mut t, grant_id(CAP as u32, 0)), Err(EINVAL));
    }

    #[test]
    fn sequences_wrap_without_escaping_the_field() {
        let mut t = table();
        let mut seq = GRANT_MAX_SEQ;
        let gid = direct(&mut t, &mut seq, 1, 0x1000, 8).unwrap();
        assert_eq!(grant_seq(gid), GRANT_MAX_SEQ);
        assert!(grant_valid(gid));
        assert_eq!(seq, 0, "the sequence must wrap, not overflow the field");
    }

    #[test]
    fn a_new_pool_is_unregistered_and_empty() {
        let pool: GrantPool<CAP> = GrantPool::new();
        assert_eq!(pool.used(), 0);
        assert_eq!(
            pool.registered_at, 0,
            "no kernel call before the first grant"
        );
    }
}

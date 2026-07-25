// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Grant-table ABI — the governed way to move bytes across an address-space
//! boundary (slice 5.2, decision D4).
//!
//! A granting process keeps a [`GrantEntry`] array **in its own address
//! space** and registers `(addr, entries)` with the kernel via `SYS_SETGRANT`.
//! It hands a *grant id* to a peer; the peer names that id in `SYS_SAFECOPY`,
//! and the kernel reads the entry back out of the granter's address space,
//! validates it, and performs the copy. The granter therefore pays no kernel
//! memory for its table and can revoke a grant unilaterally, which is exactly
//! MINIX 3's model (`include/minix/safecopies.h`, `kernel/system/do_safecopy.c`).
//!
//! Two grant kinds are live in Phase 5:
//!
//! - **direct** ([`CPF_DIRECT`]) — the granter grants a range of *its own*
//!   memory to a named grantee (server ↔ server).
//! - **magic** ([`CPF_MAGIC`]) — a server-grade granter grants *a third
//!   process's* memory to a grantee, the single-copy read/write data path
//!   (VFS granting a user's buffer to TTY/MFS). The kernel gates this on the
//!   granter's `SYS_PROC` privilege flag.
//!
//! **Indirect** grants ([`CPF_INDIRECT`], re-granting a received grant) are
//! deferred: nothing in Phase 5 needs them, and the kernel answers `EINVAL`.
//!
//! Layout is pinned by `const _` asserts rather than convention. The kernel
//! decodes an entry from 32 raw bytes read out of user memory, so a field
//! inserted ahead of `addr` would otherwise be a *silent* misread — the byte
//! count would stay right and every decode would still "succeed".

use crate::endpoint::Endpoint;

/// One grant-table entry, as it appears in the granter's address space.
///
/// `#[repr(C)]` and deliberately **flat** — MINIX 3 unions the direct and
/// magic variants, but the two differ by one endpoint field, so a union buys
/// nothing here and costs the kernel a discriminated read.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GrantEntry {
    /// `CPF_*` flags: kind, access rights, and the used/valid markers.
    pub flags: u32,
    /// Sequence number, checked against the grant id's `seq` field so a
    /// revoked-and-reissued slot cannot be reached through a stale id.
    pub seq: u32,
    /// The one endpoint permitted to use this grant.
    pub who_to: Endpoint,
    /// Owner of the granted memory. Meaningful for [`CPF_MAGIC`] only; for a
    /// direct grant the memory belongs to the granter and this is `NONE`.
    pub who_from: Endpoint,
    /// Base address of the granted range, in the *owning* address space.
    pub addr: u64,
    /// Length of the granted range in bytes.
    pub len: u64,
}

/// Wire size of one [`GrantEntry`]. The kernel reads exactly this many bytes
/// per entry out of the granter's address space.
pub const GRANT_ENTRY_SIZE: usize = 32;

// Pin the layout the kernel's byte-level decode assumes. `GRANT_ENTRY_SIZE`
// also multiplies the table's index arithmetic, so a size change that went
// unnoticed would read entries at the wrong stride.
const _: () = assert!(core::mem::size_of::<GrantEntry>() == GRANT_ENTRY_SIZE);
const _: () = assert!(core::mem::align_of::<GrantEntry>() == 8);
const _: () = assert!(core::mem::offset_of!(GrantEntry, flags) == 0);
const _: () = assert!(core::mem::offset_of!(GrantEntry, seq) == 4);
const _: () = assert!(core::mem::offset_of!(GrantEntry, who_to) == 8);
const _: () = assert!(core::mem::offset_of!(GrantEntry, who_from) == 12);
const _: () = assert!(core::mem::offset_of!(GrantEntry, addr) == 16);
const _: () = assert!(core::mem::offset_of!(GrantEntry, len) == 24);

impl GrantEntry {
    /// An unused slot — all-zero, so a freshly zeroed table is entirely free
    /// (`CPF_USED` clear).
    pub const EMPTY: Self = Self {
        flags: 0,
        seq: 0,
        who_to: crate::endpoint::NONE,
        who_from: crate::endpoint::NONE,
        addr: 0,
        len: 0,
    };

    /// Decode an entry from its 32-byte in-memory image.
    ///
    /// The kernel copies raw bytes out of the granter's address space (it never
    /// dereferences a user VA), so the decode is explicit rather than a
    /// transmute — and lives here, beside the layout asserts that justify the
    /// offsets, so it is host-testable.
    pub fn from_ne_bytes(b: &[u8; GRANT_ENTRY_SIZE]) -> Self {
        Self {
            flags: u32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
            seq: u32::from_ne_bytes([b[4], b[5], b[6], b[7]]),
            who_to: i32::from_ne_bytes([b[8], b[9], b[10], b[11]]),
            who_from: i32::from_ne_bytes([b[12], b[13], b[14], b[15]]),
            addr: u64::from_ne_bytes([b[16], b[17], b[18], b[19], b[20], b[21], b[22], b[23]]),
            len: u64::from_ne_bytes([b[24], b[25], b[26], b[27], b[28], b[29], b[30], b[31]]),
        }
    }

    /// Encode an entry to its 32-byte in-memory image. The inverse of
    /// [`from_ne_bytes`](Self::from_ne_bytes); exists so the round trip can be
    /// tested against the real struct layout.
    pub fn to_ne_bytes(&self) -> [u8; GRANT_ENTRY_SIZE] {
        let mut b = [0u8; GRANT_ENTRY_SIZE];
        b[0..4].copy_from_slice(&self.flags.to_ne_bytes());
        b[4..8].copy_from_slice(&self.seq.to_ne_bytes());
        b[8..12].copy_from_slice(&self.who_to.to_ne_bytes());
        b[12..16].copy_from_slice(&self.who_from.to_ne_bytes());
        b[16..24].copy_from_slice(&self.addr.to_ne_bytes());
        b[24..32].copy_from_slice(&self.len.to_ne_bytes());
        b
    }
}

// ---------------------------------------------------------------------------
// CPF flags. Values are MINIX 3's (`include/minix/safecopies.h`) so the wire
// format stays recognizable and the musl-side headers can be generated from
// these constants.
// ---------------------------------------------------------------------------

/// The grantee may read the granted range (`SAFECOPY_FROM`).
pub const CPF_READ: u32 = 0x001;
/// The grantee may write the granted range (`SAFECOPY_TO`).
pub const CPF_WRITE: u32 = 0x002;
/// Reserved (MINIX `CPF_TRY`): soft-fail instead of blocking on a page fault.
/// minix.rs has no soft-fault path — the flag is held so its wire value cannot
/// be reused, and it is ignored by the kernel.
pub const CPF_TRY: u32 = 0x010;
/// Slot is allocated. A zeroed table is therefore entirely free.
pub const CPF_USED: u32 = 0x100;
/// Direct grant: the granted memory belongs to the granter itself.
pub const CPF_DIRECT: u32 = 0x200;
/// Reserved: indirect grant (re-granting a grant received from a third party).
/// Deferred past Phase 5 — the kernel returns `EINVAL`.
pub const CPF_INDIRECT: u32 = 0x400;
/// Magic grant: the granted memory belongs to [`GrantEntry::who_from`], not to
/// the granter. Requires the granter to hold server-grade privilege.
pub const CPF_MAGIC: u32 = 0x800;
/// Slot passed the granter's own consistency check when it was written.
/// Checked alongside [`CPF_USED`]; a partially initialized slot has neither.
pub const CPF_VALID: u32 = 0x1000;

/// The access-right bits, as a mask.
pub const CPF_ACCESS_MASK: u32 = CPF_READ | CPF_WRITE;
/// The grant-kind bits, as a mask.
pub const CPF_KIND_MASK: u32 = CPF_DIRECT | CPF_INDIRECT | CPF_MAGIC;

// ---------------------------------------------------------------------------
// Grant-id packing. MINIX 3 packs `(seq, idx)` into one `cp_grant_id_t` with
// `GRANT_SHIFT = 20`; minix.rs keeps the same split so a grant id is a plain
// non-negative `i32` on the wire.
// ---------------------------------------------------------------------------

/// Bit position of the sequence field within a grant id.
pub const GRANT_SHIFT: u32 = 20;
/// Largest representable table index (the low `GRANT_SHIFT` bits).
pub const GRANT_MAX_IDX: u32 = (1 << GRANT_SHIFT) - 1;
/// Largest representable sequence number — the bits between [`GRANT_SHIFT`]
/// and the `i32` sign bit, so a packed id is never negative.
pub const GRANT_MAX_SEQ: u32 = (1 << (31 - GRANT_SHIFT)) - 1;
/// Sentinel for "no grant" (MINIX `GRANT_INVALID`). Every real id is >= 0.
pub const GRANT_INVALID: i32 = -1;

// A maximal id must still be non-negative, or `grant_valid` would reject it.
const _: () = assert!(grant_id(GRANT_MAX_IDX, GRANT_MAX_SEQ) == i32::MAX);

/// Pack `(idx, seq)` into a grant id. Both are masked to their field widths,
/// so an over-large input wraps rather than corrupting the neighbouring field.
#[inline]
pub const fn grant_id(idx: u32, seq: u32) -> i32 {
    (((seq & GRANT_MAX_SEQ) << GRANT_SHIFT) | (idx & GRANT_MAX_IDX)) as i32
}

/// Extract the table index from a grant id.
#[inline]
pub const fn grant_idx(g: i32) -> u32 {
    (g as u32) & GRANT_MAX_IDX
}

/// Extract the sequence number from a grant id.
#[inline]
pub const fn grant_seq(g: i32) -> u32 {
    ((g as u32) >> GRANT_SHIFT) & GRANT_MAX_SEQ
}

/// True if `g` could be a packed grant id at all. Rejects [`GRANT_INVALID`]
/// and every other negative value; range-checking `idx` against the granter's
/// registered table size is the kernel's job.
#[inline]
pub const fn grant_valid(g: i32) -> bool {
    g >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_32_bytes_and_8_aligned() {
        assert_eq!(core::mem::size_of::<GrantEntry>(), GRANT_ENTRY_SIZE);
        assert_eq!(core::mem::align_of::<GrantEntry>(), 8);
    }

    #[test]
    fn entry_field_offsets_match_the_kernel_decode() {
        // The kernel reads these byte ranges directly out of user memory.
        assert_eq!(core::mem::offset_of!(GrantEntry, flags), 0);
        assert_eq!(core::mem::offset_of!(GrantEntry, seq), 4);
        assert_eq!(core::mem::offset_of!(GrantEntry, who_to), 8);
        assert_eq!(core::mem::offset_of!(GrantEntry, who_from), 12);
        assert_eq!(core::mem::offset_of!(GrantEntry, addr), 16);
        assert_eq!(core::mem::offset_of!(GrantEntry, len), 24);
    }

    #[test]
    fn byte_round_trip_preserves_every_field() {
        let e = GrantEntry {
            flags: CPF_USED | CPF_VALID | CPF_DIRECT | CPF_READ,
            seq: 0x1234,
            who_to: 7,
            who_from: -2,
            addr: 0x0000_1234_5678_9ab0,
            len: 0x0000_0000_0001_0000,
        };
        assert_eq!(GrantEntry::from_ne_bytes(&e.to_ne_bytes()), e);
    }

    #[test]
    fn the_codec_writes_each_field_at_its_repr_c_offset() {
        // The bytes the kernel decodes *are* the granter's struct image, so the
        // hand-written codec has to agree with `#[repr(C)]` itself. Building the
        // reference image from `offset_of!` ties the two together without
        // reading the struct's memory (this crate carries no `unsafe`).
        let e = GrantEntry {
            flags: 0xDEAD_BEEF,
            seq: 0x0BAD_F00D,
            who_to: i32::MIN,
            who_from: i32::MAX,
            addr: u64::MAX,
            len: 1,
        };
        let mut want = [0u8; GRANT_ENTRY_SIZE];
        let put = |w: &mut [u8; GRANT_ENTRY_SIZE], off: usize, bytes: &[u8]| {
            w[off..off + bytes.len()].copy_from_slice(bytes);
        };
        put(
            &mut want,
            core::mem::offset_of!(GrantEntry, flags),
            &e.flags.to_ne_bytes(),
        );
        put(
            &mut want,
            core::mem::offset_of!(GrantEntry, seq),
            &e.seq.to_ne_bytes(),
        );
        put(
            &mut want,
            core::mem::offset_of!(GrantEntry, who_to),
            &e.who_to.to_ne_bytes(),
        );
        put(
            &mut want,
            core::mem::offset_of!(GrantEntry, who_from),
            &e.who_from.to_ne_bytes(),
        );
        put(
            &mut want,
            core::mem::offset_of!(GrantEntry, addr),
            &e.addr.to_ne_bytes(),
        );
        put(
            &mut want,
            core::mem::offset_of!(GrantEntry, len),
            &e.len.to_ne_bytes(),
        );
        assert_eq!(e.to_ne_bytes(), want);
        assert_eq!(GrantEntry::from_ne_bytes(&want), e);
    }

    #[test]
    fn empty_entry_is_all_zero_but_for_the_none_endpoints() {
        // A zeroed table must read back as entirely free — `CPF_USED` clear.
        assert_eq!(GrantEntry::EMPTY.flags & CPF_USED, 0);
        assert_eq!(GrantEntry::EMPTY.len, 0);
    }

    #[test]
    fn cpf_flags_match_minix3_values() {
        // Pinned by MINIX 3 include/minix/safecopies.h; the generated C header
        // and any future musl wrapper depend on these wire values.
        assert_eq!(CPF_READ, 0x001);
        assert_eq!(CPF_WRITE, 0x002);
        assert_eq!(CPF_TRY, 0x010);
        assert_eq!(CPF_USED, 0x100);
        assert_eq!(CPF_DIRECT, 0x200);
        assert_eq!(CPF_INDIRECT, 0x400);
        assert_eq!(CPF_MAGIC, 0x800);
        assert_eq!(CPF_VALID, 0x1000);
    }

    #[test]
    fn kind_flags_are_mutually_distinct_bits() {
        for (a, b) in [
            (CPF_DIRECT, CPF_INDIRECT),
            (CPF_DIRECT, CPF_MAGIC),
            (CPF_INDIRECT, CPF_MAGIC),
        ] {
            assert_eq!(a & b, 0);
        }
        assert_eq!(CPF_KIND_MASK, CPF_DIRECT | CPF_INDIRECT | CPF_MAGIC);
        assert_eq!(CPF_ACCESS_MASK, CPF_READ | CPF_WRITE);
        // Access bits must not overlap the kind bits, or a `flags & access`
        // test would accidentally admit a kind bit.
        assert_eq!(CPF_ACCESS_MASK & CPF_KIND_MASK, 0);
    }

    #[test]
    fn grant_shift_matches_minix3() {
        assert_eq!(GRANT_SHIFT, 20);
        assert_eq!(GRANT_MAX_IDX, 0xF_FFFF);
        assert_eq!(GRANT_MAX_SEQ, 0x7FF);
    }

    #[test]
    fn id_round_trips_through_idx_and_seq() {
        for (idx, seq) in [
            (0u32, 0u32),
            (1, 1),
            (GRANT_MAX_IDX, 0),
            (0, GRANT_MAX_SEQ),
            (GRANT_MAX_IDX, GRANT_MAX_SEQ),
            (12345, 678),
        ] {
            let g = grant_id(idx, seq);
            assert!(grant_valid(g), "packed id {g} must be non-negative");
            assert_eq!(grant_idx(g), idx);
            assert_eq!(grant_seq(g), seq);
        }
    }

    #[test]
    fn the_fields_do_not_bleed_into_each_other() {
        // A maximal index must not perturb the sequence field, and vice versa.
        assert_eq!(grant_seq(grant_id(GRANT_MAX_IDX, 0)), 0);
        assert_eq!(grant_idx(grant_id(0, GRANT_MAX_SEQ)), 0);
        // Over-large inputs wrap into their own field rather than the other's.
        assert_eq!(grant_idx(grant_id(GRANT_MAX_IDX + 1, 3)), 0);
        assert_eq!(grant_seq(grant_id(GRANT_MAX_IDX + 1, 3)), 3);
        assert_eq!(grant_seq(grant_id(5, GRANT_MAX_SEQ + 1)), 0);
        assert_eq!(grant_idx(grant_id(5, GRANT_MAX_SEQ + 1)), 5);
    }

    #[test]
    fn invalid_and_negative_ids_are_rejected() {
        assert!(!grant_valid(GRANT_INVALID));
        assert!(!grant_valid(-2));
        assert!(!grant_valid(i32::MIN));
        assert!(grant_valid(0));
        assert!(grant_valid(i32::MAX));
    }

    #[test]
    fn distinct_slots_and_generations_give_distinct_ids() {
        // Staleness detection depends on this: the same slot reissued with a
        // bumped sequence must not produce the id the revoked grant had.
        assert_ne!(grant_id(3, 1), grant_id(3, 2));
        assert_ne!(grant_id(3, 1), grant_id(4, 1));
    }
}

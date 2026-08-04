// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Shared minixrs ELF brand scan. Spec: tooling/docs/abi-note.md.
//!
//! Lives here under the crate's pure-predicate carve-out (see
//! [`crate::message::user_va_ok`]): one scan over ABI-defined bytes, callable
//! from the kernel at load time, from `kernel/build.rs` host-side at pack
//! time, and later from mkfs — and host-testable, which the kernel crate
//! is not.

pub const NT_MINIXRS_IDENT: u32 = 1;
pub const BRAND_OWNER: &[u8] = b"minixrs\0";
pub const BRAND_ABI_VERSION: u32 = 1;

const PT_NOTE: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrandInfo {
    pub abi_version: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandError {
    /// No PT_NOTE segment carries an owner="minixrs\0", type=1 note.
    MissingBrand,
    /// Brand present but built for an ABI this kernel does not speak.
    UnsupportedAbi(u32),
    /// Not a little-endian ELF64, or truncated/overflowing offsets.
    Malformed,
}

fn u16le(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

pub fn scan_brand(elf: &[u8]) -> Result<BrandInfo, BrandError> {
    use BrandError::Malformed;
    // `e_ident` gate: magic, then EI_CLASS == ELFCLASS64 (2) and
    // EI_DATA == ELFDATA2LSB (1).
    if elf.get(..4) != Some(b"\x7fELF".as_slice())
        || elf.get(4) != Some(&2)
        || elf.get(5) != Some(&1)
    {
        return Err(Malformed);
    }
    let phoff = u64le(elf, 0x20).ok_or(Malformed)? as usize;
    let phentsize = u16le(elf, 0x36).ok_or(Malformed)? as usize;
    let phnum = u16le(elf, 0x38).ok_or(Malformed)? as usize;

    for i in 0..phnum {
        let ph = i
            .checked_mul(phentsize)
            .and_then(|o| o.checked_add(phoff))
            .ok_or(Malformed)?;
        if u32le(elf, ph).ok_or(Malformed)? != PT_NOTE {
            continue;
        }
        let off = u64le(elf, ph + 8).ok_or(Malformed)? as usize;
        let filesz = u64le(elf, ph + 32).ok_or(Malformed)? as usize;
        let end = off.checked_add(filesz).ok_or(Malformed)?;
        let seg = elf.get(off..end).ok_or(Malformed)?;

        match scan_note_segment(seg) {
            // Not in *this* segment; another `PT_NOTE` may still carry it.
            Err(BrandError::MissingBrand) => continue,
            other => return other,
        }
    }
    Err(BrandError::MissingBrand)
}

/// Walk one `PT_NOTE` segment's bytes for the minixrs identity note.
///
/// Split out of [`scan_brand`] in slice 5.9 so the kernel can brand-check an
/// image it is reading **chunked**, out of another address space through a grant,
/// with no contiguous slice of the whole ELF to hand: the loader stages each note
/// segment into a small stack buffer and calls this. `scan_brand` — which does
/// have the whole file — keeps working by delegating, so the two can never
/// disagree about what a brand is.
///
/// `Err(MissingBrand)` means "not in these bytes", which is why the caller loops
/// rather than stopping; `Err(UnsupportedAbi)` means the note was found and
/// rejected, and stops the search — a second, older note must not be able to
/// launder a binary past an ABI the kernel does not speak.
///
/// A truncated final note ends the walk without an error: the segment may simply
/// have been cut short (the loader caps how much it stages), and any earlier note
/// in it was still read in full.
pub fn scan_note_segment(seg: &[u8]) -> Result<BrandInfo, BrandError> {
    use BrandError::Malformed;

    let mut pos = 0usize;
    while pos + 12 <= seg.len() {
        let namesz = u32le(seg, pos).ok_or(Malformed)? as usize;
        let descsz = u32le(seg, pos + 4).ok_or(Malformed)? as usize;
        let ntype = u32le(seg, pos + 8).ok_or(Malformed)?;
        let name_off = pos + 12;
        let desc_off = name_off
            .checked_add(namesz.next_multiple_of(4))
            .ok_or(Malformed)?;
        let next = desc_off
            .checked_add(descsz.next_multiple_of(4))
            .ok_or(Malformed)?;
        if next > seg.len() {
            break; // truncated tail; other PT_NOTE segments may still match
        }
        if &seg[name_off..name_off + namesz] == BRAND_OWNER
            && ntype == NT_MINIXRS_IDENT
            && descsz >= 8
        {
            let abi = u32le(seg, desc_off).ok_or(Malformed)?;
            let flags = u32le(seg, desc_off + 4).ok_or(Malformed)?;
            if abi != BRAND_ABI_VERSION {
                return Err(BrandError::UnsupportedAbi(abi));
            }
            return Ok(BrandInfo {
                abi_version: abi,
                flags,
            });
        }
        pos = next;
    }
    Err(BrandError::MissingBrand)
}

#[cfg(test)]
mod tests {
    // The crate is unconditionally `no_std`; the test config links std via
    // libtest anyway, so borrow `Vec` from it for the synthetic-ELF builder.
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// 28 canonical bytes from tooling/docs/abi-note.md "Exact byte layout".
    const CANON: [u8; 28] = [
        8, 0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, b'm', b'i', b'n', b'i', b'x', b'r', b's', 0, 1, 0, 0,
        0, 0, 0, 0, 0,
    ];

    /// Minimal synthetic ELF64: 64-byte ehdr, one phdr at 64, payload at 120.
    fn synth_elf(p_type: u32, payload: &[u8]) -> Vec<u8> {
        let mut e = vec![0u8; 120];
        e[0..4].copy_from_slice(b"\x7fELF");
        e[4] = 2; // ELFCLASS64
        e[5] = 1; // ELFDATA2LSB
        e[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        e[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        e[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        e[64..68].copy_from_slice(&p_type.to_le_bytes());
        e[72..80].copy_from_slice(&120u64.to_le_bytes()); // p_offset
        e[96..104].copy_from_slice(&(payload.len() as u64).to_le_bytes()); // p_filesz
        e.extend_from_slice(payload);
        e
    }

    #[test]
    fn canonical_note_is_branded() {
        let elf = synth_elf(4, &CANON);
        assert_eq!(
            scan_brand(&elf),
            Ok(BrandInfo {
                abi_version: 1,
                flags: 0
            })
        );
    }

    #[test]
    fn foreign_owner_is_missing() {
        let mut note = CANON;
        note[12..20].copy_from_slice(b"foreign\0");
        assert_eq!(
            scan_brand(&synth_elf(4, &note)),
            Err(BrandError::MissingBrand)
        );
    }

    #[test]
    fn longer_descriptor_still_passes() {
        // descsz = 12: one appended reserved word — growth rule, must pass.
        let mut note = CANON.to_vec();
        note[4..8].copy_from_slice(&12u32.to_le_bytes());
        note.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            scan_brand(&synth_elf(4, &note)),
            Ok(BrandInfo {
                abi_version: 1,
                flags: 0
            })
        );
    }

    #[test]
    fn unknown_abi_is_rejected() {
        let mut note = CANON;
        note[20..24].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            scan_brand(&synth_elf(4, &note)),
            Err(BrandError::UnsupportedAbi(2))
        );
    }

    #[test]
    fn no_pt_note_is_missing() {
        // Same bytes but the phdr is PT_LOAD (1): consumers walk PT_NOTE only.
        assert_eq!(
            scan_brand(&synth_elf(1, &CANON)),
            Err(BrandError::MissingBrand)
        );
    }

    #[test]
    fn foreign_then_canonical_in_one_segment() {
        let mut seg = CANON.to_vec();
        seg[12..20].copy_from_slice(b"foreign\0");
        seg.extend_from_slice(&CANON);
        assert_eq!(
            scan_brand(&synth_elf(4, &seg)),
            Ok(BrandInfo {
                abi_version: 1,
                flags: 0
            })
        );
    }

    #[test]
    fn truncated_segment_is_malformed() {
        let mut elf = synth_elf(4, &CANON);
        // p_filesz says 28 but the file ends early → seg slice fails.
        elf.truncate(elf.len() - 4);
        assert_eq!(scan_brand(&elf), Err(BrandError::Malformed));
    }

    #[test]
    fn not_an_elf_is_malformed() {
        assert_eq!(scan_brand(b"MZ\x90\x00"), Err(BrandError::Malformed));
    }

    #[test]
    fn the_segment_walk_is_what_scan_brand_delegates_to() {
        // Slice 5.9's split: the kernel calls `scan_note_segment` directly, with
        // a segment it staged out of another address space, so the two must agree
        // on every answer. Whole-file and segment-only forms, side by side.
        assert_eq!(scan_note_segment(&CANON), scan_brand(&synth_elf(4, &CANON)),);

        let mut foreign = CANON;
        foreign[12..20].copy_from_slice(b"foreign\0");
        assert_eq!(
            scan_note_segment(&foreign),
            Err(BrandError::MissingBrand),
            "not in these bytes -- the caller keeps looking"
        );

        let mut bad_abi = CANON;
        bad_abi[20..24].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            scan_note_segment(&bad_abi),
            Err(BrandError::UnsupportedAbi(2)),
            "found and rejected -- the caller stops"
        );
    }

    #[test]
    fn a_segment_cut_short_of_the_brand_is_missing_not_malformed() {
        // The loader stages only the first `MAX_NOTE_BYTES` of a note segment, so
        // a brand past that point simply is not found. That must read as
        // "missing" — a rejection with a name — rather than as a parse error.
        let mut seg = [0u8; 12];
        seg[0..4].copy_from_slice(&8u32.to_le_bytes()); // namesz
        seg[4..8].copy_from_slice(&8u32.to_le_bytes()); // descsz
        seg[8..12].copy_from_slice(&NT_MINIXRS_IDENT.to_le_bytes());
        // The name and descriptor are past the end of what was staged.
        assert_eq!(scan_note_segment(&seg), Err(BrandError::MissingBrand));
        assert_eq!(scan_note_segment(&[]), Err(BrandError::MissingBrand));
    }
}

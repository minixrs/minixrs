// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Minimal static aarch64 ELF64 loader.
//!
//! Just enough to bring up the VM server (slice 3.4): parse the ELF header,
//! walk the program headers, and map each `PT_LOAD` segment into a freshly
//! built [`AddrSpace`]. No dynamic relocations, no interpreter, no symbol
//! resolution — the user binaries are statically linked `ET_EXEC` images
//! produced by `servers/*/user.ld`, which keeps every segment page-aligned
//! (vaddr *and* file offset) so this loader never has to split a page across
//! two segments.
//!
//! All field reads are explicit little-endian (`from_le_bytes`) because the
//! ELF bytes are embedded via `include_bytes!` and carry no alignment
//! guarantee. Frames come from the slice-3.1a allocator (zeroed on hand-out,
//! so a segment's BSS tail is satisfied for free) and are copied into via
//! HHDM, mirroring `userland::build_stub`'s stub-blob copy.

use crate::arch::aarch64::addrspace::{AddrSpace, MapError, Prot};
use crate::arch::aarch64::mmu::{PAGE_SIZE, flush_icache_range};
use crate::mm::{alloc_frame, phys_to_hhdm};

/// Errors the loader can surface. All are fatal at boot (the embedded VM ELF
/// is produced by our own build, so any of these means a build/loader bug).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Too few bytes for the ELF header or a program header table entry.
    Truncated,
    /// `e_ident` magic is not `\x7fELF`.
    BadMagic,
    /// Not `ELFCLASS64`.
    BadClass,
    /// Not little-endian (`ELFDATA2LSB`).
    BadData,
    /// `e_machine` is not `EM_AARCH64`.
    BadMachine,
    /// `e_type` is not `ET_EXEC`.
    BadType,
    /// `e_phentsize` is not the expected 56 bytes.
    BadPhentsize,
    /// A `PT_LOAD` segment is not page-aligned (vaddr or file offset), or its
    /// file span exceeds its memory span.
    Misaligned,
    /// A segment requested both write and execute permission (W^X violation).
    WriteExec,
    /// The address-space mapping failed (out of memory, already mapped, …).
    Map(MapError),
}

impl From<MapError> for ElfError {
    fn from(e: MapError) -> Self {
        ElfError::Map(e)
    }
}

// ELF identification / header field offsets and constants (Elf64).
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_AARCH64: u16 = 0xB7;
const ET_EXEC: u16 = 2;
const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

fn rd_u16(b: &[u8], off: usize) -> Result<u16, ElfError> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or(ElfError::Truncated)
}

fn rd_u32(b: &[u8], off: usize) -> Result<u32, ElfError> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(ElfError::Truncated)
}

fn rd_u64(b: &[u8], off: usize) -> Result<u64, ElfError> {
    b.get(off..off + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or(ElfError::Truncated)
}

/// Load every `PT_LOAD` segment of `elf` into `aspace` and return the entry
/// point virtual address (`e_entry`).
pub fn load_into(elf: &[u8], aspace: &mut AddrSpace) -> Result<u64, ElfError> {
    if elf.len() < EHDR_SIZE {
        return Err(ElfError::Truncated);
    }
    if elf[0..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(ElfError::BadMagic);
    }
    if elf[EI_CLASS] != ELFCLASS64 {
        return Err(ElfError::BadClass);
    }
    if elf[EI_DATA] != ELFDATA2LSB {
        return Err(ElfError::BadData);
    }
    if rd_u16(elf, 16)? != ET_EXEC {
        return Err(ElfError::BadType);
    }
    if rd_u16(elf, 18)? != EM_AARCH64 {
        return Err(ElfError::BadMachine);
    }

    let e_entry = rd_u64(elf, 24)?;
    let e_phoff = rd_u64(elf, 32)? as usize;
    let e_phentsize = rd_u16(elf, 54)? as usize;
    let e_phnum = rd_u16(elf, 56)? as usize;
    if e_phentsize != PHDR_SIZE {
        return Err(ElfError::BadPhentsize);
    }

    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if rd_u32(elf, ph)? != PT_LOAD {
            continue;
        }
        let p_flags = rd_u32(elf, ph + 4)?;
        let p_offset = rd_u64(elf, ph + 8)? as usize;
        let p_vaddr = rd_u64(elf, ph + 16)?;
        let p_filesz = rd_u64(elf, ph + 32)? as usize;
        let p_memsz = rd_u64(elf, ph + 40)? as usize;

        load_segment(elf, aspace, p_flags, p_offset, p_vaddr, p_filesz, p_memsz)?;
    }

    Ok(e_entry)
}

#[allow(clippy::too_many_arguments)]
fn load_segment(
    elf: &[u8],
    aspace: &mut AddrSpace,
    p_flags: u32,
    p_offset: usize,
    p_vaddr: u64,
    p_filesz: usize,
    p_memsz: usize,
) -> Result<(), ElfError> {
    // Page-aligned segments keep the per-page copy below trivial. Bit-and
    // alignment check mirrors `addrspace::check_va`'s idiom.
    let page_mask = PAGE_SIZE as u64 - 1;
    if p_vaddr & page_mask != 0 || (p_offset as u64) & page_mask != 0 || p_filesz > p_memsz {
        return Err(ElfError::Misaligned);
    }
    // The file region must actually be present in the embedded image.
    if p_offset
        .checked_add(p_filesz)
        .map(|e| e > elf.len())
        .unwrap_or(true)
    {
        return Err(ElfError::Truncated);
    }

    let writable = p_flags & PF_W != 0;
    let executable = p_flags & PF_X != 0;
    if writable && executable {
        return Err(ElfError::WriteExec);
    }
    let prot = match (writable, executable) {
        (false, true) => Prot::RO_CODE,
        (true, false) => Prot::RW_DATA,
        (false, false) => Prot::RO_DATA,
        (true, true) => unreachable!("W^X checked above"),
    };

    let n_pages = p_memsz.div_ceil(PAGE_SIZE);
    for page_idx in 0..n_pages {
        let va = p_vaddr + (page_idx * PAGE_SIZE) as u64;
        let frame = alloc_frame().ok_or(ElfError::Map(MapError::OutOfMemory))?;

        // Copy this page's slice of file data; the rest of the frame stays
        // zero (BSS tail). `file_page_start` is the byte offset within the
        // segment, not the file.
        let file_page_start = page_idx * PAGE_SIZE;
        let copy_len = p_filesz.saturating_sub(file_page_start).min(PAGE_SIZE);
        if copy_len > 0 {
            let src = &elf[p_offset + file_page_start..p_offset + file_page_start + copy_len];
            // SAFETY: `frame` was just allocated (exclusively ours, zeroed, and
            // HHDM-mapped); `src` is a bounds-checked subslice of the embedded
            // ELF. Disjoint regions, `copy_len <= PAGE_SIZE`.
            unsafe {
                let dst = phys_to_hhdm(frame.addr());
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst, copy_len);
                if executable {
                    flush_icache_range(dst as u64, copy_len);
                }
            }
        }

        aspace.map_page(va, frame.addr(), prot)?;
    }

    Ok(())
}

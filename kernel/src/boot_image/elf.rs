// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Minimal static aarch64 ELF64 loader.
//!
//! Just enough to bring up the servers (slice 3.4) and to `exec` (4.7): parse the
//! ELF header, walk the program headers, and map each `PT_LOAD` segment into a
//! freshly built [`AddrSpace`]. No dynamic relocations, no interpreter, no symbol
//! resolution — the user binaries are statically linked `ET_EXEC` images produced
//! by `servers/*/user.ld` (and by clang for `userland/hello`), which keeps every
//! segment page-aligned (vaddr *and* file offset) so this loader never has to
//! split a page across two segments.
//!
//! ## The image is a *source*, not a slice (slice 5.9, decision D6)
//!
//! Until 5.9 the only image the loader ever saw was an `include_bytes!` slice of
//! the MXBI archive. exec-from-FS adds a second: the bytes of a file VFS staged
//! and granted to PM, which live in **another address space** and have no
//! contiguous kernel mapping at all. So the loader reads through [`ElfSource`],
//! whose two variants are the boot-image slice and a granted user buffer, and
//! every field read goes through a small stack buffer.
//!
//! It is an `enum` rather than a `dyn` trait deliberately: the loader runs on the
//! kernel stack during a kernel call, and a vtable dispatch per header field buys
//! nothing here. The granted arm delegates to `mm::uaccess::copy_from_user_as` —
//! the "read N bytes at a VA in another address space" primitive slice 5.1
//! already built, address-space-independent by design — so there is **no new copy
//! machinery** in this file.
//!
//! ## Why the header numbers are now checked
//!
//! `kernel/build.rs` brand-checks every module it packs, but that assertion
//! cannot reach a file read out of a filesystem, so the runtime scan in
//! [`load_into`] is the only gate — and the same is true of `e_phnum` and
//! `p_memsz`, which decide how much this function allocates. The caps live in
//! `kernel_shared::execimage`, where they are host-testable; this file applies
//! them.
//!
//! All field reads are explicit little-endian (`from_le_bytes`) because the ELF
//! bytes carry no alignment guarantee. Frames come from the slice-3.1a allocator
//! (zeroed on hand-out, so a segment's BSS tail is satisfied for free) and are
//! copied into via HHDM.

use crate::arch::aarch64::addrspace::{AddrSpace, MapError, Prot};
use crate::arch::aarch64::mmu::{PAGE_SIZE, flush_icache_range};
use crate::mm::uaccess::copy_from_user_as;
use crate::mm::{alloc_frame, free_frame, phys_to_hhdm};
use minixrs_kernel_shared::brand;
use minixrs_kernel_shared::execimage::{ImageError, PageBudget, phnum_ok, segment_end};

/// Errors the loader can surface.
///
/// Fatal at boot (a boot server's ELF is produced by our own build, so any of
/// these means a build/loader bug); at `exec` they are mapped to an errno by
/// `userland::load_exec_image` — `Map(OutOfMemory)` to `ENOMEM`, [`Source`] to
/// `EFAULT`, and everything else to `ENOEXEC`.
///
/// [`Source`]: ElfError::Source
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
    /// No PT_NOTE note with owner "minixrs\0", type NT_MINIXRS_IDENT (M1).
    MissingBrand,
    /// Brand present but built for an ABI this kernel does not speak.
    UnsupportedAbi(u32),
    /// The image's bytes could not be read — a walk miss or a permission
    /// failure in the granted address space (slice 5.9). Distinct from
    /// [`Truncated`], which is the image claiming something outside *itself*:
    /// this one is the memory the image lives in going wrong.
    ///
    /// [`Truncated`]: ElfError::Truncated
    Source,
    /// The image's own header asks for more than the kernel will map — too many
    /// program headers, too many pages in total, or a segment whose span
    /// overflows (slice 5.9; see `kernel_shared::execimage`).
    TooLarge,
}

impl From<MapError> for ElfError {
    fn from(e: MapError) -> Self {
        ElfError::Map(e)
    }
}

impl From<ImageError> for ElfError {
    fn from(_: ImageError) -> Self {
        ElfError::TooLarge
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
const PT_NOTE: u32 = 4;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Bytes of one `PT_NOTE` segment the brand scan stages onto the kernel stack.
///
/// The consequence is worth stating rather than discovering: **a brand note past
/// the first `MAX_NOTE_BYTES` of a note segment is not found**, and the image is
/// refused as unbranded. That cannot happen to anything this repo's `user.ld`
/// rule produces — the note is a dedicated `PT_NOTE` phdr at the very start of
/// the RO `PT_LOAD`, 28 bytes long — and a bound is required, because the segment
/// header is one of the numbers a hostile image controls.
const MAX_NOTE_BYTES: usize = 256;

// `PageBudget::charge` counts a segment's pages with `kernel_shared`'s
// `USER_PAGE_SIZE`, and `load_segment` then strides the *returned* count by this
// crate's `mmu::PAGE_SIZE`. Before slice 5.9 the count was computed locally from
// `PAGE_SIZE`, so the two could not disagree; now they can, and a divergence is
// silent — a 16 KiB-granule port would charge `ceil(memsz / 4096)` pages and then
// map that many 16 KiB pages, running four times past the segment's end into the
// next one (or into `SERVER_STACK_VA`). Pin them together here, where both names
// are in scope.
const _: () = assert!(PAGE_SIZE as u64 == minixrs_kernel_shared::message::USER_PAGE_SIZE);

/// Where an image's bytes are, for a loader that must read them a field at a
/// time.
///
/// Both arms answer the same two questions — how long is it, and give me bytes
/// `off..off+n` — which is all [`load_into`] ever asks. See the module note for
/// why this is an enum and not a trait object.
pub enum ElfSource<'a> {
    /// A slice of the MXBI boot archive: the kernel already has the bytes.
    Bytes(&'a [u8]),
    /// A buffer in another address space, reached through the page tables at
    /// `ttbr0_pa`. Produced by `do_exec` after `verify_grant` has approved the
    /// grant PM named, so `va`/`len` describe memory the granter really offered.
    UserGrant { ttbr0_pa: u64, va: u64, len: usize },
}

impl ElfSource<'_> {
    /// The image's length in bytes.
    pub fn len(&self) -> usize {
        match self {
            ElfSource::Bytes(b) => b.len(),
            ElfSource::UserGrant { len, .. } => *len,
        }
    }

    /// Fill `dst` from `off`, or say why not.
    ///
    /// Bounds are checked against [`len`](Self::len) *before* either arm runs, so
    /// the two forms refuse an out-of-range read identically ([`ElfError::Truncated`]
    /// — the image claimed something past its own end). Only a genuine failure to
    /// reach mapped memory is [`ElfError::Source`].
    pub fn read(&self, off: usize, dst: &mut [u8]) -> Result<(), ElfError> {
        let end = off
            .checked_add(dst.len())
            .ok_or(ElfError::Truncated)
            .and_then(|e| {
                if e <= self.len() {
                    Ok(e)
                } else {
                    Err(ElfError::Truncated)
                }
            })?;

        match self {
            ElfSource::Bytes(b) => {
                dst.copy_from_slice(&b[off..end]);
                Ok(())
            }
            ElfSource::UserGrant { ttbr0_pa, va, .. } => {
                // `off` is below `len`, which `do_exec` has already bounded by
                // `MAX_IMAGE_BYTES`, so this cannot wrap in practice — checked
                // anyway, because it is arithmetic on a header field.
                let src = va.checked_add(off as u64).ok_or(ElfError::Source)?;
                copy_from_user_as(*ttbr0_pa, src, dst).map_err(|_| ElfError::Source)
            }
        }
    }
}

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

/// What [`load_into`] learned about the image it just mapped.
///
/// `entry` is all the boot path needs; the program-header fields exist for the
/// `exec` initial stack (slice 5.5), which reports them to the new program as
/// `AT_PHDR` / `AT_PHNUM` / `AT_PHENT` so musl's `__init_tls` can walk the
/// headers without re-opening the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LoadedElf {
    /// Program entry point VA (`e_entry`).
    pub entry: u64,
    /// VA the program header table is readable at, when some `PT_LOAD` maps it.
    ///
    /// `None` is the ordinary case for an image whose first `PT_LOAD` starts
    /// past the header — nothing is broken, there is simply no `AT_PHDR` to
    /// report. `userland/worker/user.ld` uses the `FILEHDR PHDRS` idiom
    /// specifically so this is `Some`.
    pub phdr_va: Option<u64>,
    /// `e_phnum` — number of program header entries.
    pub phnum: u16,
    /// `e_phentsize` — bytes per entry (always 56 here; the header is rejected
    /// otherwise).
    pub phentsize: u16,
}

/// The parts of the ELF header the two phdr walks below both need.
struct Ehdr {
    entry: u64,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

/// Load every `PT_LOAD` segment of `source` into `aspace` and report the entry
/// point plus where the program headers landed (see [`LoadedElf`]).
///
/// Two passes over the program headers, and the order is the point: the brand
/// scan runs first, so an unbranded or foreign-ABI image is refused **before a
/// single frame is allocated**. The alternative — scanning inside the load loop —
/// would leave a half-mapped address space for the caller to unwind on the one
/// path where the image was never going to run.
pub fn load_into(source: &ElfSource, aspace: &mut AddrSpace) -> Result<LoadedElf, ElfError> {
    let eh = read_ehdr(source)?;

    // M1: refuse an image without the minixrs identity note
    // (tooling/docs/abi-note.md). This is the single choke point under
    // `load_exec_image`, covering boot, exec, and — since slice 5.9 —
    // exec-from-FS, where `kernel/build.rs`'s pack-time assertion cannot reach.
    scan_brand_chunked(source, &eh)?;

    let mut budget = PageBudget::new();
    let mut phdr_va = None;
    for i in 0..eh.phnum {
        let ph = read_phdr(source, &eh, i)?;
        if rd_u32(&ph, 0)? != PT_LOAD {
            continue;
        }
        let p_flags = rd_u32(&ph, 4)?;
        let p_offset = rd_u64(&ph, 8)? as usize;
        let p_vaddr = rd_u64(&ph, 16)?;
        let p_filesz = rd_u64(&ph, 32)? as usize;
        let p_memsz = rd_u64(&ph, 40)? as usize;

        // Does this segment's *file* range cover the program header table? If
        // so the headers are mapped, and the VA they are readable at follows
        // from the segment's offset→vaddr relation. First match wins; `None`
        // when no segment covers them is ordinary (see `LoadedElf::phdr_va`).
        if phdr_va.is_none()
            && segment_covers_phdrs(p_offset, p_filesz, eh.phoff, eh.phnum, eh.phentsize)
        {
            // `e_phoff >= p_offset` is part of what `segment_covers_phdrs` just
            // proved, so this subtraction cannot wrap.
            phdr_va = p_vaddr.checked_add((eh.phoff - p_offset) as u64);
        }

        load_segment(
            source,
            aspace,
            &mut budget,
            p_flags,
            p_offset,
            p_vaddr,
            p_filesz,
            p_memsz,
        )?;
    }

    Ok(LoadedElf {
        entry: eh.entry,
        phdr_va,
        phnum: eh.phnum as u16,
        phentsize: eh.phentsize as u16,
    })
}

/// Read and validate the ELF header, including the caps on what the phdr walk
/// will do.
fn read_ehdr(source: &ElfSource) -> Result<Ehdr, ElfError> {
    let mut b = [0u8; EHDR_SIZE];
    source.read(0, &mut b)?;

    if b[0..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(ElfError::BadMagic);
    }
    if b[EI_CLASS] != ELFCLASS64 {
        return Err(ElfError::BadClass);
    }
    if b[EI_DATA] != ELFDATA2LSB {
        return Err(ElfError::BadData);
    }
    if rd_u16(&b, 16)? != ET_EXEC {
        return Err(ElfError::BadType);
    }
    if rd_u16(&b, 18)? != EM_AARCH64 {
        return Err(ElfError::BadMachine);
    }

    let phentsize = rd_u16(&b, 54)? as usize;
    if phentsize != PHDR_SIZE {
        return Err(ElfError::BadPhentsize);
    }
    let phnum = rd_u16(&b, 56)? as usize;
    if !phnum_ok(phnum) {
        return Err(ElfError::TooLarge);
    }

    Ok(Ehdr {
        entry: rd_u64(&b, 24)?,
        phoff: rd_u64(&b, 32)? as usize,
        phentsize,
        phnum,
    })
}

/// Stage program header `i` onto the stack.
fn read_phdr(source: &ElfSource, eh: &Ehdr, i: usize) -> Result<[u8; PHDR_SIZE], ElfError> {
    let off = i
        .checked_mul(eh.phentsize)
        .and_then(|o| o.checked_add(eh.phoff))
        .ok_or(ElfError::Truncated)?;
    let mut ph = [0u8; PHDR_SIZE];
    source.read(off, &mut ph)?;
    Ok(ph)
}

/// Walk the `PT_NOTE` segments looking for the minixrs identity note, staging
/// each one through a bounded stack buffer.
///
/// `MissingBrand` from one segment means "not in these bytes" and the walk
/// continues; `UnsupportedAbi` means the note was found and refused, and stops
/// it — a second, older note must not be able to launder an image past an ABI
/// this kernel does not speak. That is `kernel_shared::brand::scan_brand`'s own
/// contract, which this shares by calling the same `scan_note_segment`.
fn scan_brand_chunked(source: &ElfSource, eh: &Ehdr) -> Result<brand::BrandInfo, ElfError> {
    for i in 0..eh.phnum {
        let ph = read_phdr(source, eh, i)?;
        if rd_u32(&ph, 0)? != PT_NOTE {
            continue;
        }
        let p_offset = rd_u64(&ph, 8)? as usize;
        let p_filesz = rd_u64(&ph, 32)? as usize;
        let n = p_filesz.min(MAX_NOTE_BYTES);
        if n == 0 {
            continue;
        }
        let mut seg = [0u8; MAX_NOTE_BYTES];
        source.read(p_offset, &mut seg[..n])?;

        match brand::scan_note_segment(&seg[..n]) {
            Ok(info) => return Ok(info),
            Err(brand::BrandError::MissingBrand) => continue,
            Err(brand::BrandError::UnsupportedAbi(v)) => return Err(ElfError::UnsupportedAbi(v)),
            Err(brand::BrandError::Malformed) => return Err(ElfError::Truncated),
        }
    }
    Err(ElfError::MissingBrand)
}

/// Is the whole program header table inside this segment's file range?
///
/// All-checked arithmetic: `e_phoff`/`e_phnum`/`e_phentsize` come straight out
/// of the image's header, so an overflow here must read as "not covered" rather
/// than wrap into a bogus containment.
fn segment_covers_phdrs(
    p_offset: usize,
    p_filesz: usize,
    e_phoff: usize,
    e_phnum: usize,
    e_phentsize: usize,
) -> bool {
    let Some(ph_bytes) = e_phnum.checked_mul(e_phentsize) else {
        return false;
    };
    let (Some(ph_end), Some(seg_end)) = (
        e_phoff.checked_add(ph_bytes),
        p_offset.checked_add(p_filesz),
    ) else {
        return false;
    };
    e_phoff >= p_offset && ph_end <= seg_end
}

#[allow(clippy::too_many_arguments)]
fn load_segment(
    source: &ElfSource,
    aspace: &mut AddrSpace,
    budget: &mut PageBudget,
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
    // The file region must actually be present in the image.
    if p_offset
        .checked_add(p_filesz)
        .map(|e| e > source.len())
        .unwrap_or(true)
    {
        return Err(ElfError::Truncated);
    }
    // The memory span must not wrap or leave the user address range, and the
    // image as a whole must stay inside the page budget (slice 5.9 / D6: these
    // are header fields, and since exec-from-FS they are input rather than
    // build output).
    segment_end(p_vaddr, p_memsz)?;
    let n_pages = budget.charge(p_memsz)?;

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

    for page_idx in 0..n_pages {
        // Cannot wrap: `segment_end` proved `p_vaddr + p_memsz` is in range and
        // `page_idx * PAGE_SIZE < p_memsz.next_multiple_of(PAGE_SIZE)`.
        let va = p_vaddr + (page_idx * PAGE_SIZE) as u64;
        let frame = alloc_frame().ok_or(ElfError::Map(MapError::OutOfMemory))?;
        let pa = frame.addr();

        // Copy this page's slice of file data; the rest of the frame stays
        // zero (BSS tail). `file_page_start` is the byte offset within the
        // segment, not the file.
        let file_page_start = page_idx * PAGE_SIZE;
        let copy_len = p_filesz.saturating_sub(file_page_start).min(PAGE_SIZE);
        if copy_len > 0 {
            // SAFETY: `frame` was just allocated, so it is exclusively ours,
            // zeroed, and HHDM-mapped; `copy_len <= PAGE_SIZE`, so the slice
            // stays inside that one frame. Nothing else holds a reference to
            // it — it is not yet linked into any page table.
            let dst = unsafe { core::slice::from_raw_parts_mut(phys_to_hhdm(pa), copy_len) };
            // One copy in both source forms: `Bytes` memcpys out of the archive,
            // `UserGrant` walks the granter's page tables and memcpys through the
            // resolved frame's HHDM alias. Either way the bytes land here once.
            if let Err(e) = source.read(p_offset + file_page_start, dst) {
                // The frame is not mapped yet, so the caller's leaf sweep would
                // never see it — free it here or it leaks.
                free_frame(frame);
                return Err(e);
            }
            if executable {
                // SAFETY: `pa` is a frame we just wrote through its HHDM alias;
                // `copy_len` bytes of it are the range that needs the icache
                // made coherent with the data we placed there.
                unsafe { flush_icache_range(phys_to_hhdm(pa) as u64, copy_len) };
            }
        }

        if let Err(e) = aspace.map_page(va, pa, prot) {
            // Same reasoning: `map_page` failed before linking the leaf.
            free_frame(frame);
            return Err(e.into());
        }
    }

    Ok(())
}

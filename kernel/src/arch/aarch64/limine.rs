// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// `kernel_physical_base` / `kernel_virtual_base` are convenience accessors on
// the kernel-address response; slice 2.3 only consumes `kernel_va_to_pa`,
// but the bases are part of the API surface Phase 3's VM server will need.
#![allow(dead_code)]

//! Rust-side Limine boot protocol requests for aarch64.
//!
//! Magic IDs are copied from `external/limine/dist/limine.h`. Each request
//! lands in the `.limine_requests` ELF section (bracketed by start/end
//! markers in their own sections); the linker script `linker.ld` keeps
//! all three sections in a contiguous PT_LOAD segment so Limine can scan
//! the kernel binary and fill in response pointers before jumping to
//! `_start`.
//!
//! Phase 1 only reads HHDM (as a sanity check that Limine actually ran).
//! The memmap / paging-mode / stack-size requests are present so the
//! responses are available when later phases need them, without requiring
//! us to revisit the linker script.

use core::sync::atomic::{AtomicU64, Ordering};

// --- Markers -------------------------------------------------------------

#[used]
#[unsafe(link_section = ".limine_requests_start")]
static REQUESTS_START_MARKER: [u64; 4] = [
    0xf6b8f4b39de7d1ae,
    0xfab91a6940fcb9cf,
    0x785c6ed015d3e316,
    0x181e920a7852b9d9,
];

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END_MARKER: [u64; 2] = [0xadc0e0531bb10d03, 0x9572709f31764c62];

// --- Base revision -------------------------------------------------------

// Format: [magic_a, magic_b, requested_revision]. The bootloader sets index
// [2] to 0 if it supports the requested revision, otherwise leaves it
// non-zero. Index [1] may be replaced by the loaded revision.
//
// We request revision 2 (not 3) deliberately: under revision 3, Limine
// drops the 0..4 GiB blanket HHDM map and only maps explicit memmap
// regions. QEMU virt's PL011 at phys 0x0900_0000 is *not* in the memmap
// as a usable type, so revision 3 leaves it unmapped and any early UART
// write data-aborts before we have an MMU helper to fix it. Revision 2
// keeps the [0, 4 GiB) → HHDM mapping, which covers PL011 directly.
// Phase 2 (proper MMU + device-memory mapping) will move this back to 3.
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: [AtomicU64; 3] = [
    AtomicU64::new(0xf9562b2d5c95a6c8),
    AtomicU64::new(0x6a7b384944536bdc),
    AtomicU64::new(2),
];

pub fn base_revision_supported() -> bool {
    BASE_REVISION[2].load(Ordering::Relaxed) == 0
}

// --- HHDM ---------------------------------------------------------------

#[repr(C)]
struct HhdmResponse {
    revision: u64,
    offset: u64,
}

#[repr(C)]
struct HhdmRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicU64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest {
    id: [
        0xc7b1dd30df4c8b88,
        0x0a82e883a194f07b,
        0x48dcf1cb8ad2b852,
        0x63984e959a98244b,
    ],
    revision: 0,
    response: AtomicU64::new(0),
};

pub fn hhdm_offset() -> Option<u64> {
    let p = HHDM_REQUEST.response.load(Ordering::Relaxed) as *const HhdmResponse;
    if p.is_null() {
        return None;
    }
    // SAFETY: Limine filled `response` with a pointer to a valid HhdmResponse
    // in our address space before jumping to _start; it does not mutate the
    // response afterward. Reading volatile guards against unintended caching
    // even though there are no concurrent writers.
    Some(unsafe { core::ptr::read_volatile(&(*p).offset) })
}

// --- Memmap --------------------------------------------------------------

#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub kind: u64,
}

/// Memory map entry type: free for use by the kernel.
pub const MEMMAP_USABLE: u64 = 0;

#[repr(C)]
struct MemmapResponseRaw {
    revision: u64,
    entry_count: u64,
    /// Pointer to an array of `entry_count` pointers, each to a [`MemmapEntry`].
    entries: *const *const MemmapEntry,
}

#[repr(C)]
struct MemmapRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicU64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest {
    id: [
        0xc7b1dd30df4c8b88,
        0x0a82e883a194f07b,
        0x67cf3d9d378a806f,
        0xe304acdfc50c3c62,
    ],
    revision: 0,
    response: AtomicU64::new(0),
};

/// Iterate Limine's memory map. Yields one [`MemmapEntry`] per region in the
/// order Limine reported them. Returns `None` if Limine didn't populate the
/// response (e.g. unsupported revision).
pub fn memmap_entries() -> Option<MemmapIter> {
    let p = MEMMAP_REQUEST.response.load(Ordering::Relaxed) as *const MemmapResponseRaw;
    if p.is_null() {
        return None;
    }
    // SAFETY: Limine filled `response` with a pointer to a valid
    // MemmapResponseRaw in our address space and does not mutate it
    // afterward; the `entries` indirection is itself a stable Limine-owned
    // array of pointers to stable entries.
    let (entries, count) = unsafe { ((*p).entries, (*p).entry_count) };
    if entries.is_null() {
        return None;
    }
    Some(MemmapIter {
        entries,
        idx: 0,
        count,
    })
}

pub struct MemmapIter {
    entries: *const *const MemmapEntry,
    idx: u64,
    count: u64,
}

impl Iterator for MemmapIter {
    type Item = &'static MemmapEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.count {
            return None;
        }
        // SAFETY: `idx < count`; Limine's `entries[idx]` points at a valid
        // entry. Entries live for the kernel's lifetime — bootloader memory
        // for them is in BOOTLOADER_RECLAIMABLE, but we never reclaim it.
        let entry: &'static MemmapEntry =
            unsafe { &*(*self.entries.add(self.idx as usize)) };
        self.idx += 1;
        Some(entry)
    }
}

// --- Paging mode (request only) -----------------------------------------

#[repr(C)]
struct PagingModeRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicU64,
    mode: u64,
    max_mode: u64,
    min_mode: u64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static PAGING_MODE_REQUEST: PagingModeRequest = PagingModeRequest {
    id: [
        0xc7b1dd30df4c8b88,
        0x0a82e883a194f07b,
        0x95c1a0edab0944cb,
        0xa4e5cb3842f7488a,
    ],
    revision: 0,
    response: AtomicU64::new(0),
    mode: 0,     // LIMINE_PAGING_MODE_AARCH64_4LVL
    max_mode: 0, // 4-level only
    min_mode: 0,
};

// --- Stack size (request only) ------------------------------------------

#[repr(C)]
struct StackSizeRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicU64,
    stack_size: u64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest {
    id: [
        0xc7b1dd30df4c8b88,
        0x0a82e883a194f07b,
        0x224ef0460a8e8926,
        0xe1cb0fc25f46ea3d,
    ],
    revision: 0,
    response: AtomicU64::new(0),
    stack_size: 64 * 1024,
};

// --- Kernel address ----------------------------------------------------
//
// Limine returns the (physical_base, virtual_base) pair where it loaded the
// kernel image. Slice 2.3 needs this to derive PAs for kernel-image
// addresses (e.g. the static page-table arena that lives in `.bss` and the
// `_user_stub_start` symbol in `.rodata`) — we can't go through HHDM for
// those, because the kernel image is mapped via TTBR1, not via HHDM.

#[repr(C)]
struct KernelAddressResponse {
    revision: u64,
    physical_base: u64,
    virtual_base: u64,
}

#[repr(C)]
struct KernelAddressRequest {
    id: [u64; 4],
    revision: u64,
    response: AtomicU64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static KERNEL_ADDRESS_REQUEST: KernelAddressRequest = KernelAddressRequest {
    id: [
        0xc7b1dd30df4c8b88,
        0x0a82e883a194f07b,
        0x71ba76863cc55f63,
        0xb2644a48c516a487,
    ],
    revision: 0,
    response: AtomicU64::new(0),
};

fn kernel_address_response() -> Option<&'static KernelAddressResponse> {
    let p = KERNEL_ADDRESS_REQUEST.response.load(Ordering::Relaxed)
        as *const KernelAddressResponse;
    if p.is_null() {
        return None;
    }
    // SAFETY: Limine filled `response` with a pointer to a valid
    // KernelAddressResponse in our address space and does not mutate it
    // afterward; the struct is `'static`.
    Some(unsafe { &*p })
}

/// Physical base of the kernel image, as reported by Limine.
pub fn kernel_physical_base() -> Option<u64> {
    kernel_address_response().map(|r| r.physical_base)
}

/// Virtual base of the kernel image, as reported by Limine.
pub fn kernel_virtual_base() -> Option<u64> {
    kernel_address_response().map(|r| r.virtual_base)
}

/// Translate a kernel-image virtual address to its physical address.
///
/// Only valid for VAs that lie inside the kernel ELF image (`.text`,
/// `.rodata`, `.data`, `.bss`). Other VAs (HHDM-mapped device memory,
/// future user-space mappings) require their own translation.
pub fn kernel_va_to_pa(va: u64) -> Option<u64> {
    let r = kernel_address_response()?;
    Some(va.wrapping_sub(r.virtual_base).wrapping_add(r.physical_base))
}

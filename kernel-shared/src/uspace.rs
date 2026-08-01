// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The user-space virtual-address map the kernel and user space agree on.
//!
//! Everything else in this crate describes a *message* ABI — bytes on the wire
//! between two processes. This module describes an *address* ABI: the kernel
//! installs a mapping at one of these VAs and a user-space process dereferences
//! it, with no message in between. Slice 5.3's TTY driver is the first consumer:
//! the kernel pre-maps the PL011 MMIO page at [`TTY_UART_VA`] with Device
//! attributes while building TTY's address space, and TTY's `pl011.rs` does
//! volatile loads and stores there.
//!
//! That is why this is not in [`crate::message`]: nothing here is a payload
//! offset or a pointer predicate, and getting a value wrong is a data abort in
//! user space, not a rejected message.
//!
//! ## The device window
//!
//! [`USER_DEVICE_WINDOW_BASE`] carves one whole L1 slot (1 GiB of VA space,
//! costing exactly one L1 + L2 + L3 frame for the pages actually mapped) out of
//! the user VA range, well clear of every VA any process occupies today:
//!
//! | VA | What |
//! |---|---|
//! | `0x0010_0000` | server / init / worker ELF images (`user.ld` base) |
//! | `0x0020_0000` | server stack page (`SERVER_STACK_VA`) |
//! | `0x0040_0000` / `0x0080_0000` | demo stub code / stack pages |
//! | `0x0100_0000` | VM's heap origin (`region::HEAP_BASE`) |
//! | `0x0200_0000` | VM's anonymous-mmap arena (`region::MMAP_BASE`) |
//! | `0x4000_0000` | **the device window** |
//! | `0x8000_0000` | [the ramdisk window](RAMDISK_WINDOW_BASE) (slice 5.7) |
//!
//! Two of those entries *grow* on request — VM's heap (`brk` raises its end) and
//! VM's mmap arena (a bump cursor that never reuses addresses) — so
//! `servers/vm/src/region.rs` bounds **both** at [`USER_DEVICE_WINDOW_BASE`] with a
//! runtime `REGION_LIMIT` check rather than relying on the 992 MiB of slack. A
//! compile-time assert on their *bases* is not enough: it proves only where each
//! region starts.
//!
//! Phase 6's virtio-mmio drivers get further pages of the same window; the
//! 16 MiB size is sized for that rather than for TTY's single page.
//!
//! ## The ramdisk window
//!
//! [`RAMDISK_WINDOW_BASE`] is the second kernel-owned window, and it is placed
//! *above* the device window on purpose: `REGION_LIMIT` in
//! `servers/vm/src/region.rs` is the base of the **lowest** kernel-owned window,
//! so every window added above that low-water mark needs no VM edit at all. The
//! `USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= RAMDISK_WINDOW_BASE`
//! assert below is what keeps that ordering true, and it is what makes the
//! ramdisk's separation from every *process* VA transitive through the device
//! window's.
//!
//! Unlike the device window this one maps **ordinary RAM**, not MMIO: the kernel
//! copies the boot archive's `rootfs` blob into freshly allocated frames and maps
//! them `Prot::RW_DATA`. So none of the `prot.device` machinery is involved —
//! these frames take the normal `free_frame` path in every leaf sweep. RW rather
//! than RO because slice 5.10's write path is then a body change in the driver
//! rather than a re-mapping in the kernel.
//!
//! These constants are deliberately **not** emitted into the generated C headers:
//! no Phase 5 C touches them (musl's `write()` goes to VFS, not to a driver's
//! MMIO window or ramdisk), and a header constant is an ABI promise that would be
//! awkward to retract once Phase 6 reshapes the windows.

use crate::message::{USER_PAGE_SIZE, USER_VA_TOP};

/// Base of the user-space device (MMIO) window. One whole L1 slot at 1 GiB.
pub const USER_DEVICE_WINDOW_BASE: u64 = 0x4000_0000;

/// Size of the user-space device window: 16 MiB, i.e. 4096 pages. TTY uses one;
/// the rest is headroom for Phase 6's virtio-mmio register pages.
pub const USER_DEVICE_WINDOW_SIZE: u64 = 0x0100_0000;

/// VA at which the kernel pre-maps the PL011 UART's MMIO page into the TTY
/// driver's address space (slice 5.3, decision D1). Page 0 of the window.
///
/// The mapping is installed once, at boot, by
/// `arch::aarch64::userland::load_boot_server` — TTY is a boot server with no
/// way to ask for it (there is no `VMCTL_MAP_PHYS`; D1 deferred VM-mediated
/// device mapping to Phase 6), so this is a bring-up step exactly like the
/// server stack page.
pub const TTY_UART_VA: u64 = USER_DEVICE_WINDOW_BASE;

/// Base of the user-space ramdisk window. One whole L1 slot at 2 GiB (slice 5.7).
///
/// Kernel-owned like the device window, and deliberately *above* it so
/// `region::REGION_LIMIT` — the base of the lowest kernel-owned window — does not
/// move when a window is added.
pub const RAMDISK_WINDOW_BASE: u64 = 0x8000_0000;

/// Size of the ramdisk window: 4 MiB, i.e. 1024 pages.
///
/// Sized from the format rather than from today's image: seven direct zones plus
/// one single-indirect block address `7 + 1024` zones at a 4 KiB block size,
/// which is 4.03 MiB — so a window this size can hold any image MinixFS can
/// address without a double-indirect reader (see `fs/mfs`'s `read` module).
pub const RAMDISK_WINDOW_SIZE: u64 = 0x0040_0000;

/// VA at which the kernel maps the boot ramdisk into the `memory` driver's
/// address space (slice 5.7, decision D3). Page 0 of the window.
///
/// Installed once, at boot, by `arch::aarch64::userland::load_boot_server` — the
/// `memory` driver is a boot server with no way to ask for it — and reported to
/// that driver alone via `SYS_GETINFO(GET_RAMDISK)`.
pub const RAMDISK_VA: u64 = RAMDISK_WINDOW_BASE;

/// Bytes of stack the kernel gives a boot server: exactly one page.
///
/// The VA is the kernel's own business (`arch::aarch64::userland`'s
/// `SERVER_STACK_VA`, which nothing in user space may name — each server has its
/// own TTBR0 and reaches its stack through `SP_EL0`, never through a constant).
/// The *size* is shared because a server has to know how much frame it can spend:
/// it is what decides whether a buffer may be a local at all.
///
/// Slice 5.8's MFS is the first server for which that question has a "no". Its
/// block buffer is [`crate::callnr::BDEV_BLOCK_SIZE`] — the whole stack — so it
/// lives in `.bss` behind a capability token instead, and `fs/mfs` carries the
/// `const _` tripwire that fires if this ever grows enough to make a local
/// plausible again. That tripwire is the only reason this constant exists; it is
/// severable if it ever costs more than it earns.
pub const SERVER_STACK_BYTES: u64 = USER_PAGE_SIZE;

/// Size of one L1 slot in the 4 KiB-granule, 4-level, 48-bit-VA walk this
/// kernel uses: 512 L2 entries × 512 L3 entries × 4 KiB = 1 GiB.
const L1_SLOT_SIZE: u64 = 1 << 30;

// The window must start on an L1-slot boundary and fit inside one slot, so the
// whole device window costs a single L1 entry and can never share an L2 subtree
// with an ordinary mapping.
const _: () = assert!(USER_DEVICE_WINDOW_BASE.is_multiple_of(L1_SLOT_SIZE));
const _: () = assert!(USER_DEVICE_WINDOW_SIZE <= L1_SLOT_SIZE);
const _: () = assert!(USER_DEVICE_WINDOW_SIZE > 0);

// The window must lie inside the user VA range, or `map_page_in`'s `check_va`
// would reject every page of it with `MapError::OutOfRange`.
const _: () = assert!(USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= USER_VA_TOP);

// Every device VA must be page-aligned (a mapping granule) and lie wholly
// inside the window.
const _: () = assert!(TTY_UART_VA.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(TTY_UART_VA >= USER_DEVICE_WINDOW_BASE);
const _: () =
    assert!(TTY_UART_VA + USER_PAGE_SIZE <= USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE);

// The ramdisk window's geometry, mirroring the device window's: one L1-slot
// boundary, no larger than one slot, a whole number of pages, inside the user VA
// range.
const _: () = assert!(RAMDISK_WINDOW_BASE.is_multiple_of(L1_SLOT_SIZE));
const _: () = assert!(RAMDISK_WINDOW_SIZE <= L1_SLOT_SIZE);
const _: () = assert!(RAMDISK_WINDOW_SIZE > 0);
const _: () = assert!(RAMDISK_WINDOW_SIZE.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(RAMDISK_WINDOW_BASE + RAMDISK_WINDOW_SIZE <= USER_VA_TOP);

const _: () = assert!(RAMDISK_VA.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(RAMDISK_VA >= RAMDISK_WINDOW_BASE);
const _: () = assert!(RAMDISK_VA < RAMDISK_WINDOW_BASE + RAMDISK_WINDOW_SIZE);

// The load-bearing one. Every kernel-owned window must sit at or above the
// device window's end, because `servers/vm/src/region.rs` caps every *process*
// region at `USER_DEVICE_WINDOW_BASE` and nothing else. Violate this and VM would
// happily hand a client's heap or mmap an address the kernel has already
// promised to a driver — with no compile error anywhere, because VM's cap would
// still be internally consistent.
const _: () = assert!(USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= RAMDISK_WINDOW_BASE);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_one_l1_slot_at_one_gib() {
        assert_eq!(USER_DEVICE_WINDOW_BASE, 0x4000_0000);
        assert_eq!(USER_DEVICE_WINDOW_BASE, L1_SLOT_SIZE);
        assert_eq!(USER_DEVICE_WINDOW_SIZE, 16 * 1024 * 1024);
        // 4096 pages of headroom for Phase 6's virtio-mmio register pages.
        assert_eq!(USER_DEVICE_WINDOW_SIZE / USER_PAGE_SIZE, 4096);
    }

    #[test]
    fn the_tty_uart_page_is_page_zero_of_the_window() {
        assert_eq!(TTY_UART_VA, USER_DEVICE_WINDOW_BASE);
        assert_eq!(TTY_UART_VA % USER_PAGE_SIZE, 0);
        let window_end = USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE;
        assert!(TTY_UART_VA + USER_PAGE_SIZE <= window_end);
    }

    #[test]
    fn the_ramdisk_window_is_one_l1_slot_at_two_gib() {
        assert_eq!(RAMDISK_WINDOW_BASE, 0x8000_0000);
        assert_eq!(RAMDISK_WINDOW_BASE, 2 * L1_SLOT_SIZE);
        assert_eq!(RAMDISK_WINDOW_SIZE, 4 * 1024 * 1024);
        assert_eq!(RAMDISK_WINDOW_SIZE / USER_PAGE_SIZE, 1024);
        assert_eq!(RAMDISK_VA, RAMDISK_WINDOW_BASE);
        assert_eq!(RAMDISK_VA % USER_PAGE_SIZE, 0);

        let end = RAMDISK_WINDOW_BASE + RAMDISK_WINDOW_SIZE;
        assert_eq!(end, 0x8040_0000);
        assert_eq!(
            end.min(USER_VA_TOP),
            end,
            "the window runs past USER_VA_TOP"
        );
    }

    #[test]
    fn the_kernel_windows_are_disjoint_and_ascending() {
        // `servers/vm/src/region.rs` caps every process region at
        // `USER_DEVICE_WINDOW_BASE` — the *lowest* kernel-owned window — and
        // nothing else. So each new window must be placed above the previous
        // one's end, or VM's cap would stop covering it. (`min` rather than `<`:
        // an all-constant `assert!` trips clippy's `assertions_on_constants`.)
        let device_end = USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE;
        assert_eq!(
            device_end.min(RAMDISK_WINDOW_BASE),
            device_end,
            "the ramdisk window overlaps the device window"
        );
        assert_eq!(
            USER_DEVICE_WINDOW_BASE.min(RAMDISK_WINDOW_BASE),
            USER_DEVICE_WINDOW_BASE,
            "the device window must stay the lowest kernel-owned window"
        );
    }

    #[test]
    fn the_window_clears_every_occupied_user_va() {
        // Mirrors the table in the module docs. These are the highest VAs any
        // process occupies today (VM's mmap arena base is the top one); the
        // window must sit above all of them. The arena is bump-only and grows,
        // which is why `servers/vm/src/region.rs` bounds it — and the heap, which
        // `brk` also grows — at the window base rather than trusting this gap alone.
        //
        // The ramdisk window gets no entry here, and must not: this list
        // enumerates *process* VAs, and the ramdisk is kernel-owned, so it is not
        // one. Its separation from every address below is transitive — it sits
        // above the device window (`the_kernel_windows_are_disjoint_and_ascending`),
        // which sits above every VA listed here.
        for occupied in [
            0x0010_0000_u64, // server / init / worker ELF base
            0x0020_0000,     // SERVER_STACK_VA
            0x0043_0000,     // stub D code
            0x0083_0000,     // stub D stack
            0x0100_0000,     // region::HEAP_BASE
            0x0200_0000,     // region::MMAP_BASE
        ] {
            assert!(
                occupied < USER_DEVICE_WINDOW_BASE,
                "{occupied:#x} collides with the device window"
            );
        }
    }

    #[test]
    fn a_server_stack_is_exactly_one_page() {
        // What `fs/mfs`'s tripwire keys on. The kernel maps one page and points
        // `SP_EL0` at its top (`load_boot_server`), and a `const _` there asserts
        // that against this value — so the two cannot drift.
        assert_eq!(SERVER_STACK_BYTES, USER_PAGE_SIZE);
        assert_eq!(SERVER_STACK_BYTES, 4096);
    }

    #[test]
    fn the_window_fits_the_user_va_range() {
        // `map_page_in`'s `check_va` rejects any VA at or above `USER_VA_TOP`, so
        // a window that ran past it would fail every mapping with
        // `MapError::OutOfRange`. (`min` rather than `<`: an all-constant `assert!`
        // trips clippy's `assertions_on_constants`.)
        let end = USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE;
        assert_eq!(end, 0x4100_0000);
        assert_eq!(
            end.min(USER_VA_TOP),
            end,
            "the window runs past USER_VA_TOP"
        );
    }
}

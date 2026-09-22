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
//! | `0x0020_0000` | SDK-built image base (lld's aarch64 default, once the clang pin is dropped) |
//! | `0x0040_0000` / `0x0080_0000` | demo stub code / stack pages |
//! | *image end* | VM's per-process heap origin (`VM_EXEC` records it) |
//! | *+16 MiB* | VM's per-process anonymous-mmap arena |
//! | `0x3FFE_F000` | [the region ceiling](USER_REGION_LIMIT) — heap and mmap stop here |
//! | `0x3FFF_0000` | [the initial stack](USER_STACK_BASE), 16 pages, growing down |
//! | `0x4000_0000` | **the device window**, and [the stack's top](USER_STACK_TOP) |
//! | `0x8000_0000` | [the ramdisk window](RAMDISK_WINDOW_BASE) (slice 5.7) |
//!
//! Two of those entries *grow* on request — VM's heap (`brk` raises its end) and
//! VM's mmap arena (a bump cursor that never reuses addresses) — so
//! `servers/vm/src/region.rs` bounds **both** at [`USER_REGION_LIMIT`] with a
//! runtime `REGION_LIMIT` check rather than relying on the slack beneath it. A
//! compile-time assert on their *bases* is not enough: it proves only where each
//! region starts.
//!
//! Phase 6's virtio-mmio drivers get further pages of the same window; the
//! 16 MiB size is sized for that rather than for TTY's single page.
//!
//! ## The ramdisk window
//!
//! [`RAMDISK_WINDOW_BASE`] is the second kernel-owned window, and it is placed
//! *above* the device window on purpose: [`USER_REGION_LIMIT`] is derived from
//! the **lowest** kernel-owned window, so every window added above that
//! low-water mark needs no VM edit at all. The
//! `USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= RAMDISK_WINDOW_BASE`
//! assert below is what keeps that ordering true, and it is what makes the
//! ramdisk's separation from every *process* VA transitive through the device
//! window's.
//!
//! Unlike the device window this one maps **ordinary RAM**, not MMIO: the kernel
//! copies the boot archive's `rootfs` blob into freshly allocated frames and maps
//! them `Prot::RW_DATA`. So none of the `prot.device` machinery is involved —
//! these frames take the normal `free_frame` path in every leaf sweep. RW rather
//! than RO so that slice 5.10a's write path was a body change in the driver
//! rather than a re-mapping in the kernel — which is exactly how it landed.
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
/// initial user stack.
pub const TTY_UART_VA: u64 = USER_DEVICE_WINDOW_BASE;

/// Base of the user-space ramdisk window. One whole L1 slot at 2 GiB (slice 5.7).
///
/// Kernel-owned like the device window, and deliberately *above* it so
/// [`USER_REGION_LIMIT`] — which is derived from the lowest kernel-owned window —
/// does not move when a window is added.
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

/// Top of the initial user stack: the highest VA a process may name, and the
/// value the kernel points `SP_EL0` at before `eret`.
///
/// Defined *as* [`USER_DEVICE_WINDOW_BASE`] rather than as an independent
/// number, and that is the whole argument for the placement: the lowest
/// kernel-owned window already is the ceiling on everything a process may
/// touch, so putting the stack immediately beneath it makes "top of the stack"
/// and "top of the process" the same address by construction. No new ordering
/// invariant is introduced, and none can drift.
///
/// It is also the cheap placement. The stack shares L1 slot 0 with the image,
/// so an address space costs exactly one extra L3 frame — not the L1+L2+L3
/// chain a stack near [`USER_VA_TOP`] would need in every address space.
pub const USER_STACK_TOP: u64 = USER_DEVICE_WINDOW_BASE;

/// Bytes of stack the kernel gives a process: 16 pages.
///
/// Every page is **eagerly mapped** at image load — there is no lazy stack
/// fault path, and guard-page *growth* is deliberately out of scope — so this
/// is real RAM per process, at most 2 MiB across the `NR_SERVED_PROCS` ceiling.
///
/// Why 16 and not 1: musl's `%Lf` VLAs are ~7.4 KiB, which was a landmine under
/// the old single page, and `fs/mfs` carried two 4 KiB `.bss` buffers purely
/// because a one-page stack could not hold them. Why not 64: at 256 KiB a
/// server author stops thinking about the stack budget at all, which is the
/// discipline this constant exists to enforce.
///
/// Published — rather than kept private like the VA used to be — because a
/// server has to know how much frame it can spend: it is what decides whether a
/// buffer may be a local at all. `fs/mfs` carries the `const _` tripwire that
/// fires when this grows enough to make a local plausible again.
pub const USER_STACK_BYTES: u64 = 0x1_0000;

/// Lowest mapped stack VA. The kernel maps
/// `[USER_STACK_BASE, USER_STACK_TOP)` and points `SP_EL0` at the top.
///
/// Published so the tooling repo's `verify/check-image.sh` can assert that no
/// `PT_LOAD` reaches it, instead of re-deriving the arithmetic from a comment.
pub const USER_STACK_BASE: u64 = USER_STACK_TOP - USER_STACK_BYTES;

/// One page below the stack that is **never mapped**.
///
/// Free — a page of VA in a 1 GiB span, no frame, no code beyond this constant
/// and [`USER_REGION_LIMIT`] — and it converts stack overflow from "silently
/// walk into the mmap arena" into a fault VM reports as out-of-region. Under
/// the old 4 KiB stack that silent walk was real: `fs/mfs`'s tripwire comment
/// names it, and notes that it "prints nothing `tests/qemu-boot.forbidden`
/// catches".
///
/// Reserved, not grown into: a lazily-growing stack needs a fault path this
/// slice does not write.
pub const USER_STACK_GUARD_BYTES: u64 = USER_PAGE_SIZE;

/// Exclusive upper bound of every VM-tracked region — the heap and the mmap
/// arena — placed below the guard page.
///
/// This lives here, rather than in `servers/vm/src/region.rs` where it used to,
/// for two reasons. `kernel-shared` cannot reference a server crate, so the
/// bound the map is really about was unavailable to the very module that
/// documents the map. And the tooling repo needs one named constant to mirror
/// rather than the arithmetic that produces it.
///
/// `region::REGION_LIMIT` is a re-export of this, pinned by a `const _` there.
pub const USER_REGION_LIMIT: u64 = USER_STACK_BASE - USER_STACK_GUARD_BYTES;

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
// device window's end, because `USER_REGION_LIMIT` — the cap
// `servers/vm/src/region.rs` enforces on every *process* region — is derived
// from `USER_DEVICE_WINDOW_BASE` and nothing else. Violate this and VM would
// happily hand a client's heap or mmap an address the kernel has already
// promised to a driver — with no compile error anywhere, because VM's cap would
// still be internally consistent.
const _: () = assert!(USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= RAMDISK_WINDOW_BASE);

// The stack's geometry. A whole number of pages, page-aligned at both ends, and
// 16-byte-aligned at the top because that is where `SP_EL0` starts and AArch64
// faults on a misaligned SP.
const _: () = assert!(USER_STACK_BYTES > 0);
const _: () = assert!(USER_STACK_BYTES.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(USER_STACK_BASE.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(USER_STACK_TOP.is_multiple_of(16));
const _: () = assert!(USER_STACK_BASE == USER_STACK_TOP - USER_STACK_BYTES);

// The guard page and the region ceiling. `USER_REGION_LIMIT` must sit strictly
// below the stack with the guard page between, or a heap grown to its cap would
// be adjacent to the stack and an overflow would land in it silently.
const _: () = assert!(USER_STACK_GUARD_BYTES.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(USER_REGION_LIMIT == USER_STACK_BASE - USER_STACK_GUARD_BYTES);
const _: () = assert!(USER_REGION_LIMIT < USER_STACK_BASE);

// The stack is a *process* VA: it must stay wholly below every kernel-owned
// window. Flush against the device window is allowed (the ranges are half-open);
// overlapping it is not.
const _: () = assert!(USER_STACK_TOP <= USER_DEVICE_WINDOW_BASE);
const _: () = assert!(USER_STACK_TOP <= RAMDISK_WINDOW_BASE);

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
        // `USER_REGION_LIMIT` — the cap `servers/vm/src/region.rs` enforces on
        // every process region — is derived from `USER_DEVICE_WINDOW_BASE`, the
        // *lowest* kernel-owned window, and nothing else. So each new window must
        // be placed above the previous one's end, or VM's cap would stop covering
        // it. (`min` rather than `<`: an all-constant `assert!` trips clippy's
        // `assertions_on_constants`.)
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
    fn the_region_limit_clears_every_occupied_user_va() {
        // Mirrors the table in the module docs. These are the VAs a *process*
        // occupies below its stack; all must sit under `USER_REGION_LIMIT`, the
        // ceiling `servers/vm/src/region.rs` enforces on every growing region.
        //
        // The stack itself gets no entry: it is not below the limit, it is what
        // the limit is derived *from*. The kernel windows get none either —
        // they are kernel-owned, and their separation is transitive through
        // `the_kernel_windows_are_disjoint_and_ascending`.
        for occupied in [
            0x0010_0000_u64, // server / init / worker ELF base
            0x0020_0000,     // SDK image base once the clang pin is dropped (V11)
            0x0043_0000,     // stub D code
            0x0083_0000,     // stub D stack
            0x0100_0000,     // region::HEAP_BASE (legacy origin, stub D)
            0x0200_0000,     // region::MMAP_BASE (legacy origin, stub D)
        ] {
            assert!(
                occupied < USER_REGION_LIMIT,
                "{occupied:#x} collides with the region limit"
            );
        }
    }

    #[test]
    fn the_stack_is_sixteen_pages_flush_beneath_the_device_window() {
        // The stack top *is* the lowest kernel-owned window, so "top of stack"
        // and "top of process" are the same address by construction (V1).
        assert_eq!(USER_STACK_TOP, USER_DEVICE_WINDOW_BASE);
        assert_eq!(USER_STACK_TOP, 0x4000_0000);
        assert_eq!(USER_STACK_BYTES, 64 * 1024);
        assert_eq!(USER_STACK_BYTES / USER_PAGE_SIZE, 16);
        assert_eq!(USER_STACK_BASE, 0x3FFF_0000);
        assert_eq!(USER_STACK_BASE, USER_STACK_TOP - USER_STACK_BYTES);
        // `sp` starts at the top, and AArch64 requires a 16-byte-aligned SP.
        assert_eq!(USER_STACK_TOP % 16, 0);
        assert_eq!(USER_STACK_BASE % USER_PAGE_SIZE, 0);
    }

    #[test]
    fn the_guard_page_sits_below_the_stack_and_bounds_the_regions() {
        // The guard page is never mapped; `USER_REGION_LIMIT` is below it, so a
        // heap or mmap that reached its cap still cannot touch the stack (V3).
        assert_eq!(USER_STACK_GUARD_BYTES, USER_PAGE_SIZE);
        assert_eq!(USER_REGION_LIMIT, 0x3FFE_F000);
        assert_eq!(USER_REGION_LIMIT, USER_STACK_BASE - USER_STACK_GUARD_BYTES);
        assert_eq!(USER_REGION_LIMIT % USER_PAGE_SIZE, 0);
    }

    #[test]
    fn the_stack_clears_both_kernel_windows() {
        // The stack is a *process* VA, so it must stay wholly below every
        // kernel-owned window. (`min` rather than `<`: an all-constant
        // `assert!` trips clippy's `assertions_on_constants`.)
        assert_eq!(
            USER_STACK_TOP.min(USER_DEVICE_WINDOW_BASE),
            USER_STACK_TOP,
            "the stack runs into the device window"
        );
        assert_eq!(
            USER_STACK_TOP.min(RAMDISK_WINDOW_BASE),
            USER_STACK_TOP,
            "the stack runs into the ramdisk window"
        );
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

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The boot framebuffer the kernel accepted, if any (slice 6.7).
//!
//! The kernel never draws: it decides once, at boot, whether Limine's first
//! framebuffer is one TTY may have (spec F2), and remembers the answer for
//! three consumers that all run later —
//!
//! 1. `mm::init_from_limine_memmap`, which keeps the framebuffer's pages out of
//!    the frame allocator (spec F5) — the reason [`init`] must run **before**
//!    the allocator is seeded;
//! 2. `userland::load_boot_server`'s TTY arm, which maps those pages at
//!    `uspace::FB_VA` as device memory (spec F3);
//! 3. `system::do_getinfo`'s `GET_FRAMEBUFFER`, which reports the geometry to
//!    TTY (spec F4).
//!
//! All three read the one copy held here, so the range carved, the range mapped
//! and the geometry reported cannot disagree. The decision itself is
//! `kernel-shared::framebuffer::validate`, which is host-tested; this module is
//! the static and the boot line.

use core::cell::UnsafeCell;
use core::fmt::Write;

use minixrs_kernel_shared::framebuffer::Framebuffer;

use super::limine;

/// The accepted framebuffer. `None` until [`init`] runs, and afterwards too
/// when there is no display or it was refused.
#[repr(transparent)]
struct AcceptedCell(UnsafeCell<Option<Framebuffer>>);
// SAFETY: written exactly once, by `init`, during single-threaded boot and
// before any reader runs (the allocator seed, the boot-server loader and every
// kernel call all come later); read-only from then on. The kernel is
// single-core and never touches this from interrupt context.
unsafe impl Sync for AcceptedCell {}

static ACCEPTED: AcceptedCell = AcceptedCell(UnsafeCell::new(None));

/// What [`init`] found, kept by `kmain` until the allocator has been seeded and
/// the `[fb]` line can state how many pages were carved.
pub type Verdict = Result<Option<Framebuffer>, &'static str>;

/// Read and validate Limine's framebuffer and record it when accepted.
///
/// Allocates nothing. Call it before `mm::init_from_limine_memmap`, and pass
/// that function [`accepted`]'s range (spec F5: no frame may be handed out
/// before the framebuffer has been removed from the allocator).
///
/// SAFETY: must be called exactly once, single-threaded, before anything reads
/// [`accepted`].
pub unsafe fn init() -> Verdict {
    let verdict = limine::framebuffer();
    if let Ok(Some(fb)) = verdict {
        // SAFETY: caller's contract — single-threaded boot, single writer, no
        // reader yet.
        unsafe { *ACCEPTED.0.get() = Some(fb) };
    }
    verdict
}

/// The framebuffer [`init`] accepted, or `None`: no display, a refused one, or
/// `init` has not run.
pub fn accepted() -> Option<Framebuffer> {
    // SAFETY: written once at boot before any reader (see `AcceptedCell`); the
    // value is `Copy`, so no reference outlives this statement.
    unsafe { *ACCEPTED.0.get() }
}

/// Print the one `[fb]` boot line (spec F2): `[fb] ok …`, `[fb] none` or `[fb]
/// FAIL <condition>`. `carved` is the number of pages the allocator seed removed
/// from `MEMMAP_USABLE` entries for this framebuffer — 0 where the memory map
/// already reports the range as a framebuffer, as QEMU's does.
pub fn report(out: &mut impl Write, verdict: &Verdict, carved: u64) {
    let _ = match verdict {
        Ok(Some(fb)) => writeln!(
            out,
            "[fb] ok w={} h={} pitch={} r={} g={} b={} usable={} pa={:#x}",
            fb.width,
            fb.height,
            fb.pitch,
            fb.red_shift,
            fb.green_shift,
            fb.blue_shift,
            carved,
            fb.pa,
        ),
        Ok(None) => writeln!(out, "[fb] none"),
        Err(condition) => writeln!(out, "[fb] FAIL {condition}"),
    };
}

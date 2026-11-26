// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The TTY driver's renderer — the pure half of `drivers/tty` (slice 6.7,
//! decision F6).
//!
//! Four modules, none of which touches a framebuffer:
//!
//!   * [`font`] — glyph lookup over the vendored Spleen 8×16 font (F7).
//!   * [`grid`] — the text model: a fixed array of bytes, a cursor, and
//!     [`grid::Grid::put`], which says what one byte does (F8).
//!   * [`paint`] — turns grid cells into pixel stores through the
//!     [`paint::Surface`] trait, and back again for the host-side screen check
//!     (F10).
//!   * [`reply`] — the kernel's `GET_FRAMEBUFFER` answer, decoded and then
//!     accepted or refused (F9): TTY re-validates it rather than trusting it.
//!
//! The binary (`src/main.rs`) is the only file that touches a device. It asks
//! the kernel where the framebuffer is, hands the answer to [`reply::accept`],
//! and implements [`paint::Surface`] with one aligned volatile `u32` store into
//! the mapping the kernel gave it; the host tests implement it over a `Vec<u32>`.
//! That is the whole seam, and it is why this half has **no `unsafe`** — asserted
//! below, which the bin cannot do for itself (its `_start` needs
//! `unsafe(no_mangle)`).
//!
//! **Device-memory discipline (H9).** Nothing here reads a pixel back. A scroll
//! shifts the grid and the caller repaints from it, so the renderer needs no
//! load, no `copy_nonoverlapping` and no `memset` — nothing the compiler could
//! merge, widen or misalign on a device mapping. What keeps that affordable is
//! [`paint::Shown`], an ordinary array recording the byte painted in each cell:
//! a scroll stores only the cells where the grid now disagrees with it.
//!
//! Unlike `fs/mfs` there is no feature gate between the two halves: nothing
//! reaches this library from a build script, and the crate's dependencies
//! already build on the host for `cargo test`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod font;
pub mod grid;
pub mod paint;
pub mod reply;

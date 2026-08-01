// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! MinixFS v3 on-disk format — decoders, encoders, and the geometry both sides
//! of the format must agree on (slice 5.7).
//!
//! This crate is **I/O-free by construction**. Every reader takes bytes the
//! caller already fetched; nothing here opens a file, issues a `BDEV_READ`, or
//! knows what a block device is. Two things follow, and both are the point:
//!
//! * It is host-testable with no fake device — `tools/mkfs-mfs` builds a real
//!   image in a `Vec<u8>` and reads it back through these same functions, so the
//!   writer and the reader are checked against each other rather than against a
//!   transcription of the format.
//! * It is the shape slice 5.8's MFS *server* needs, which fetches its blocks
//!   over BDEV and then asks this crate what they mean.
//!
//! Everything decodes **field by field** from `&[u8]` via `from_le_bytes` — no
//! `repr(C)` structs and no transmutes, the discipline `kernel-shared::grant`
//! already follows for the grant table. The layouts are MINIX 3's
//! (`servers/mfs/super.h`, `inode.h`, `include/minix/dir.h`), so an image built
//! here is a real MinixFS v3 image; where minix.rs makes its own choice — the
//! fixed 4096-byte block size, [`layout::START_BLOCK`] — it is documented at the
//! constant.
//!
//! ## The server, and why so little of it is in the binary
//!
//! Slice 5.8 added the MFS *server* as this crate's `[[bin]]` target
//! (`src/main.rs`), behind `required-features = ["server"]` so the format library
//! keeps its single dependency and stays free for `kernel/build.rs` to pull into
//! its build-script graph.
//!
//! That gate has a cost: **the binary is invisible to every CI job** (none passes
//! `--features server`, and a `required-features` target is silently skipped). So
//! the split is drawn much harder than usual — every line with a decision in it
//! is in this library, and `main.rs` is SEF/IPC/grant/diag glue with no policy:
//!
//!   * [`proto`] — the FS-band wire codec: what a request means, and what a reply
//!     says.
//!   * [`walk`] — path traversal and read policy: which block to fetch next, how
//!     much of it to move, and every bound that stops a corrupt image spinning
//!     the server forever.
//!
//! The modules below it ([`superblock`], [`inode`], [`layout`], [`dirent`],
//! [`read`]) are slice 5.7's and are untouched — the server drives exactly the
//! decoders `tools/mkfs-mfs` writes with, which is what the `fs.selfcheck` boot
//! marker proves over a real image.
//!
//! The crate is `#![forbid(unsafe_code)]` **unconditionally**, feature or not, so
//! `geiger` and `miri` see the same crate CI lints. The server's one unavoidable
//! piece of `unsafe` — the `.bss` block buffer, which cannot be a stack local (see
//! [`MFS_BLOCK_SIZE`]) — therefore lives in `main.rs`, alongside the ELF-only
//! attributes every freestanding binary here already carries.

#![no_std]
#![forbid(unsafe_code)]

pub mod dirent;
pub mod inode;
pub mod layout;
pub mod proto;
pub mod read;
pub mod superblock;
pub mod walk;

use minixrs_kernel_shared::callnr::BDEV_BLOCK_SIZE;
use minixrs_kernel_shared::uspace::SERVER_STACK_BYTES;

/// The one block size minix.rs's MinixFS uses.
///
/// MinixFS v3 permits 1 KiB, 2 KiB, and 4 KiB; minix.rs fixes it at 4 KiB so a
/// block is exactly one page and exactly one `BDEV_READ`. [`superblock::Superblock::validate`]
/// rejects anything else rather than growing a general-purpose reader for a
/// configuration nothing produces.
///
/// It is also **the whole of a server's stack**, which is why the server's block
/// buffer is a `.bss` static rather than a local in `main`'s frame — see the
/// tripwire below.
pub const MFS_BLOCK_SIZE: usize = 4096;

// A block is a BDEV transfer unit is a page. If these ever disagreed, a `BDEV_READ`
// would return a fraction of a block and every zone lookup would be off.
const _: () = assert!(MFS_BLOCK_SIZE == BDEV_BLOCK_SIZE);

// The tripwire that `uspace::SERVER_STACK_BYTES` exists for.
//
// A boot server gets one page of stack, which is exactly one block — so the
// server's block buffer cannot be a local in `main`'s frame, and it is a `.bss`
// static reached through a capability token instead (`main.rs`'s `Blocks`). The
// failure mode this guards against is *silent*: a 4 KiB local would put the frame
// base below the mapping, and the resulting fault is turned into a SIGSEGV by VM's
// out-of-region arm, which prints nothing `tests/qemu-boot.forbidden` catches.
//
// The assertion is deliberately the way round that fires when the stack **grows**:
// at that point a local becomes plausible again, and the reasoning above deserves
// to be re-read rather than silently outlived.
const _: () = assert!(
    MFS_BLOCK_SIZE >= SERVER_STACK_BYTES as usize,
    "the server stack grew past one block: re-read why the MFS block buffer is a static"
);

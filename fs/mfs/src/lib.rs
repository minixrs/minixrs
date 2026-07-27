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
//! The `[[bin]]` target (`src/main.rs`) is a slice-5.8 placeholder: the MFS
//! *server* lands there, at which point this crate re-gains `minixrs-ipc` and
//! `server-rt` behind a `server` feature. Until then the crate has exactly one
//! dependency, so pulling it into `kernel/build.rs`'s graph costs nothing.

#![no_std]
#![forbid(unsafe_code)]

pub mod dirent;
pub mod inode;
pub mod layout;
pub mod read;
pub mod superblock;

use minixrs_kernel_shared::callnr::BDEV_BLOCK_SIZE;

/// The one block size minix.rs's MinixFS uses.
///
/// MinixFS v3 permits 1 KiB, 2 KiB, and 4 KiB; minix.rs fixes it at 4 KiB so a
/// block is exactly one page and exactly one `BDEV_READ`. [`superblock::Superblock::validate`]
/// rejects anything else rather than growing a general-purpose reader for a
/// configuration nothing produces.
pub const MFS_BLOCK_SIZE: usize = 4096;

// A block is a BDEV transfer unit is a page. If these ever disagreed, a `BDEV_READ`
// would return a fraction of a block and every zone lookup would be off.
const _: () = assert!(MFS_BLOCK_SIZE == BDEV_BLOCK_SIZE);

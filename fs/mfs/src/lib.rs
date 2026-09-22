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
pub mod write;

use minixrs_kernel_shared::callnr::{BDEV_BLOCK_SIZE, FS_MAX_IO};
use minixrs_kernel_shared::uspace::USER_STACK_BYTES;

/// The one block size minix.rs's MinixFS uses.
///
/// MinixFS v3 permits 1 KiB, 2 KiB, and 4 KiB; minix.rs fixes it at 4 KiB so a
/// block is exactly one page and exactly one `BDEV_READ`. [`superblock::Superblock::validate`]
/// rejects anything else rather than growing a general-purpose reader for a
/// configuration nothing produces.
///
/// It used to be **the whole of a server's stack**, which is why the server's
/// block buffer was a `.bss` static rather than a local in `main`'s frame. A
/// stack is 64 KiB now (`uspace::USER_STACK_BYTES`), so a block is a frame-sized
/// thing again — see the tripwire below, which is re-aimed at the margin that
/// keeps it one.
pub const MFS_BLOCK_SIZE: usize = 4096;

// A block is a BDEV transfer unit is a page. If these ever disagreed, a `BDEV_READ`
// would return a fraction of a block and every zone lookup would be off.
const _: () = assert!(MFS_BLOCK_SIZE == BDEV_BLOCK_SIZE);

// One FS transfer must be able to cover a whole block, or a full-block write is
// unreachable.
//
// `do_write` skips the read-before-splice exactly when its clamped chunk is a
// whole block, and `Blocks::write` then flushes `MFS_BLOCK_SIZE` bytes. Were
// `FS_MAX_IO` ever the smaller of the two, `write::clamp_write` could not produce
// such a chunk, every write would become a read-modify-write, and — worse — the
// skip branch would be dead code that nothing would notice had stopped being
// exercised. This pins the chain the skip depends on rather than leaving it to the
// two constants happening to be equal today.
const _: () = assert!(FS_MAX_IO >= MFS_BLOCK_SIZE);

// The tripwire that `uspace::USER_STACK_BYTES` exists for.
//
// It fired. Under the old one-page stack a block buffer *was* the whole stack,
// so MFS's block and staging buffers could not be locals and lived in `.bss`
// behind a capability token. The stack is 16 pages now, so they *can* be
// locals — which is what the assertion was written to prompt: "at that point a
// local becomes plausible again, and the reasoning above deserves to be re-read
// rather than silently outlived." Whether they actually are is `main.rs`'s
// business, and this comment stays true either way.
//
// Re-aimed at the condition that would make locals wrong again. Two block-sized
// buffers plus ordinary frame overhead should not approach the stack, and the
// margin is deliberately generous: a frame is not just its named locals, and the
// failure mode is a silent fault turned into a SIGSEGV that
// `tests/qemu-boot.forbidden` cannot catch. The guard page (`uspace`'s
// `USER_STACK_GUARD_BYTES`) now turns that overflow into a fault rather than a
// silent walk into the mmap arena, but a fault is still a crash — the margin is
// what keeps it from happening.
const _: () = assert!(
    (MFS_BLOCK_SIZE as u64) * 4 <= USER_STACK_BYTES,
    "two block buffers no longer fit comfortably in a frame: re-read where they live"
);

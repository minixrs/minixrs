// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `mkfs-mfs` — build a MinixFS v3 root filesystem image at compile time
//! (slice 5.7).
//!
//! A host crate in the `tools/gen-c-headers` shape: the library holds everything
//! testable and `src/main.rs` is argv plus file I/O. Unlike `gen-c-headers`,
//! though, this library is also called **directly** from `kernel/build.rs` as a
//! `[build-dependencies]` path dep — the `minixrs_kernel_shared::brand::scan_brand`
//! precedent — so packing the root image into the MXBI archive costs no nested
//! host cargo invocation.
//!
//! ## What makes the tests worth anything
//!
//! The writer here and the reader in `minixrs-mfs` share one set of layout
//! constants and one codec per structure. So [`verify`]'s round trip — build an
//! image in a `Vec<u8>`, then read files and directories back out of it through
//! the *same* decoders slice 5.8's server will use — checks the two halves against
//! each other rather than against a transcription. A hand-written encoder living
//! here would only have proven this tool agrees with itself.

pub mod image;
pub mod manifest;
pub mod verify;

pub use image::{MkfsError, build_image};
pub use manifest::{Entry, Manifest};

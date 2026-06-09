// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Embedded boot-image modules and their loader.
//!
//! Slice 3.4 embeds a single user-space server — the VM server — directly in
//! the kernel image as a rodata `&[u8]` (`kernel/build.rs` builds the VM crate
//! for the EL0 user target and emits `VM_ELF_PATH`). The bytes land in the
//! kernel's `.rodata`, which Limine reports under `EXECUTABLE_AND_MODULES`, so
//! they are never visible to the frame allocator — same model as the hand-coded
//! EL0 stub blobs in `arch/aarch64/user_stub.S`.
//!
//! The multi-module `.boot_image`/MXBI archive sketched in `docs/boot.md` is
//! deferred until Phase 4 loads PM/VFS/RS/etc.; with one server a plain
//! `include_bytes!` needs no packer tool or linker section.

pub mod elf;

/// The VM server ELF, built and embedded by `build.rs`.
pub static VM_ELF: &[u8] = include_bytes!(env!("VM_ELF_PATH"));

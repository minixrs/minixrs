// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Embedded boot-image archive and its loader.
//!
//! `kernel/build.rs` builds every boot server for the EL0 user target, packs the
//! resulting ELFs into a single MXBI archive, and emits `BOOT_IMAGE_PATH`. The
//! archive is embedded here as a rodata `&[u8]` via `include_bytes!`. The bytes
//! land in the kernel's `.rodata`, which Limine reports under
//! `EXECUTABLE_AND_MODULES`, so they are never visible to the frame allocator —
//! same model as the hand-coded EL0 stub blobs in `arch/aarch64/user_stub.S`.
//!
//! Archive layout (slice 4.2; all multi-byte fields little-endian, matching
//! `build.rs::pack_mxbi`):
//!
//! ```text
//!   16-byte header: magic "MXBI" (u32), version (u32), entry_count (u32), total_size (u32)
//!   entry_count × 32-byte records: { proc_nr:i32, offset:u32, len:u32, name:[u8;20] }
//!   then the ELF payloads back-to-back, each at its recorded offset
//! ```
//!
//! [`BootImage`] is a zero-copy view: [`BootImage::iter`] drives the boot loader
//! (one [`load_boot_server`](crate::arch::aarch64::userland) call per module),
//! and [`BootImage::module_by_name`] is reused by exec in slice 4.7.
//!
//! NOTE: this whole module is gated on `target_os = "none"` in `main.rs`, so
//! `env!("BOOT_IMAGE_PATH")` is only ever evaluated for the bare-metal build —
//! host `cargo check`/`cargo test` (where the env var is unset) never compile it.

pub mod elf;

use minixrs_kernel_shared::ProcNr;

/// The packed MXBI boot-image archive, built and embedded by `build.rs`.
static BOOT_IMAGE: &[u8] = include_bytes!(env!("BOOT_IMAGE_PATH"));

/// "MXBI" as a little-endian `u32` (bytes M, X, B, I).
const MXBI_MAGIC: u32 = 0x4942_584D;
const MXBI_VERSION: u32 = 1;
const HDR_LEN: usize = 16;
const REC_LEN: usize = 32;
const NAME_LEN: usize = 20;

/// Zero-copy view over the embedded MXBI archive.
pub struct BootImage {
    bytes: &'static [u8],
    count: usize,
}

impl BootImage {
    /// Borrow the embedded archive, validating its header. Panics at boot on a
    /// malformed image — the archive is produced by our own build, so any
    /// failure is a build bug (same fatal-at-boot policy as [`elf`]).
    pub fn get() -> Self {
        let bytes = BOOT_IMAGE;
        assert!(bytes.len() >= HDR_LEN, "boot image truncated");
        assert!(rd_u32(bytes, 0) == MXBI_MAGIC, "boot image bad magic");
        assert!(rd_u32(bytes, 4) == MXBI_VERSION, "boot image bad version");
        let count = rd_u32(bytes, 8) as usize;
        let total = rd_u32(bytes, 12) as usize;
        assert!(
            HDR_LEN + count * REC_LEN <= bytes.len() && total <= bytes.len(),
            "boot image table out of range"
        );
        Self { bytes, count }
    }

    /// Decode record `i` into `(proc_nr, name, elf)`. The `name` is the
    /// NUL-trimmed record name; `elf` is the payload subslice.
    fn record(&self, i: usize) -> (ProcNr, &'static str, &'static [u8]) {
        let rec = HDR_LEN + i * REC_LEN;
        let proc_nr = rd_i32(self.bytes, rec);
        let offset = rd_u32(self.bytes, rec + 4) as usize;
        let len = rd_u32(self.bytes, rec + 8) as usize;

        let name_field = &self.bytes[rec + 12..rec + 12 + NAME_LEN];
        let nul = name_field.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        let name = core::str::from_utf8(&name_field[..nul]).unwrap_or("");

        let elf = &self.bytes[offset..offset + len];
        (ProcNr::new(proc_nr), name, elf)
    }

    /// Iterate `(proc_nr, elf)` over every module, in archive (load) order.
    pub fn iter(&self) -> BootImageIter {
        BootImageIter {
            bytes: self.bytes,
            count: self.count,
            idx: 0,
        }
    }

    /// The ELF payload for a boot proc number, or `None`. Symmetric with
    /// [`module_by_name`](Self::module_by_name).
    #[allow(dead_code)] // used by later Phase-4 slices (RS restart, exec)
    pub fn module_by_proc_nr(&self, nr: ProcNr) -> Option<&'static [u8]> {
        (0..self.count).map(|i| self.record(i)).find_map(
            |(p, _, elf)| {
                if p == nr { Some(elf) } else { None }
            },
        )
    }

    /// The ELF payload for a module name (e.g. `"vfs"`), or `None`. Reused by
    /// exec in slice 4.7 to resolve a boot-embedded binary by name.
    #[allow(dead_code)] // consumed by slice 4.7 (SYS_EXEC of a boot-embedded binary)
    pub fn module_by_name(&self, name: &str) -> Option<&'static [u8]> {
        (0..self.count).map(|i| self.record(i)).find_map(
            |(_, n, elf)| {
                if n == name { Some(elf) } else { None }
            },
        )
    }
}

/// Iterator over `(proc_nr, elf)` for each module. Holds only `'static` copies,
/// so it does not borrow the [`BootImage`].
pub struct BootImageIter {
    bytes: &'static [u8],
    count: usize,
    idx: usize,
}

impl Iterator for BootImageIter {
    type Item = (ProcNr, &'static [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.count {
            return None;
        }
        // Decode in place rather than building a transient BootImage.
        let rec = HDR_LEN + self.idx * REC_LEN;
        let proc_nr = rd_i32(self.bytes, rec);
        let offset = rd_u32(self.bytes, rec + 4) as usize;
        let len = rd_u32(self.bytes, rec + 8) as usize;
        self.idx += 1;
        Some((ProcNr::new(proc_nr), &self.bytes[offset..offset + len]))
    }
}

/// Read a little-endian `u32` at `off`. Panics (via slicing) on a malformed
/// archive — fatal at boot, like [`elf`]'s reads.
fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("u32 in range"))
}

/// Read a little-endian `i32` at `off`.
fn rd_i32(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(b[off..off + 4].try_into().expect("i32 in range"))
}

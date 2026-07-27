// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The boot ramdisk image's own header — a *build-time to run-time* ABI
//! (slice 5.7).
//!
//! `tools/mkfs-mfs` writes these bytes at build time; the `memory` driver and
//! VFS's demo client read them at run time. Three different crates, none of which
//! may depend on the others, so the bytes live here for the same reason
//! [`crate::com::ROOTFS_MODULE_NAME`] does: one literal, shared, cannot drift.
//!
//! ## Why a header at all, when there is already a superblock
//!
//! Because a *block driver* must not depend on the *filesystem format*. The
//! `memory` driver checks that the bytes the kernel copied into its address space
//! are the image it expects; that is a device-level question, and answering it
//! with `minixrs-mfs`'s superblock decoder would give Phase 6 a dependency to
//! unwind when virtio-blk replaces the ramdisk under an unchanged MFS.
//!
//! So the header sits in the **boot block** — bytes `0..1024` of block 0, which
//! MinixFS never reads (its superblock starts at byte 1024) — and carries the
//! three geometry facts a driver can check without knowing what a superblock is.
//! `tools/mkfs-mfs`'s round-trip tests assert those three equal the real
//! superblock's, which is what licenses a boot marker to check the header instead.
//!
//! ## The tail label
//!
//! [`IMAGE_TAIL_LABEL`] is written into the image's **last** zone, which mkfs
//! reserves (its zone-bitmap bit is set, so no file can be allocated there). It
//! exists to make one specific bug visible: a kernel copy loop that failed to
//! advance would map 256 pages of *block 0*, and every header check would still
//! pass. Reading a different label from the far end is what distinguishes "the
//! blob was copied" from "the blob's first page was copied 256 times".
//!
//! Both labels are 16 bytes and differ from each other, so a 32-byte read of
//! either end is self-identifying.

use crate::callnr::BDEV_BLOCK_SIZE;
use crate::uspace::RAMDISK_WINDOW_SIZE;

/// Blocks in the root filesystem image: 256 × 4 KiB = 1 MiB.
///
/// **Fixed, not derived from the content.** When the musl sysroot is absent
/// `kernel/build.rs` packs the 15 KB `worker` ELF under the name `hello`, so a
/// content-sized image would make every size-derived boot marker
/// config-dependent — the slice-5.5/5.6 "right in one config, vacuous in the
/// other" trap. An image that outgrows this is a *build* failure naming this
/// constant (`MkfsError::TooBig`), with a one-constant fix.
pub const ROOTFS_IMAGE_BLOCKS: u32 = 256;

/// Inodes in the root filesystem image. 64 is exactly one inode-table block, and
/// far more than the handful of files slices 5.7–5.9 put in the image.
pub const ROOTFS_NINODES: u32 = 64;

/// Size of the root filesystem image in bytes.
pub const ROOTFS_IMAGE_BYTES: usize = ROOTFS_IMAGE_BLOCKS as usize * BDEV_BLOCK_SIZE;

/// The reserved last block, which holds [`IMAGE_TAIL_LABEL`].
pub const ROOTFS_TAIL_BLOCK: u32 = ROOTFS_IMAGE_BLOCKS - 1;

/// Bytes of image header at the start of block 0.
pub const IMAGE_HDR_LEN: usize = 32;

/// Label at the start of the image. NUL-padded to [`IMAGE_LABEL_LEN`].
pub const IMAGE_LABEL: [u8; IMAGE_LABEL_LEN] = *b"minix.rs rootfs\0";

/// Label at the start of the reserved last block. Deliberately different from
/// [`IMAGE_LABEL`] — that difference is the whole proof (see the module docs).
pub const IMAGE_TAIL_LABEL: [u8; IMAGE_LABEL_LEN] = *b"minix.rs tailv1\0";

/// Length of each label.
pub const IMAGE_LABEL_LEN: usize = 16;

/// Offset of the label within the header (and within the tail block).
pub const HDR_LABEL_OFF: usize = 0;
/// Offset of the image's block count within the header (u32).
pub const HDR_BLOCKS_OFF: usize = 16;
/// Offset of the image's block size within the header (u32).
pub const HDR_BLOCK_SIZE_OFF: usize = 20;
/// Offset of the MinixFS magic within the header (i32, sign-extended from the
/// superblock's `s_magic`). The one format fact a device-level check may look at:
/// it says "a filesystem was written here", not what its geometry is.
pub const HDR_MFS_MAGIC_OFF: usize = 24;

// The header tiles exactly, with the trailing bytes reserved (written as zero).
const _: () = assert!(HDR_LABEL_OFF + IMAGE_LABEL_LEN == HDR_BLOCKS_OFF);
const _: () = assert!(HDR_BLOCKS_OFF + 4 == HDR_BLOCK_SIZE_OFF);
const _: () = assert!(HDR_BLOCK_SIZE_OFF + 4 == HDR_MFS_MAGIC_OFF);
const _: () = assert!(HDR_MFS_MAGIC_OFF + 4 <= IMAGE_HDR_LEN);

// The header must fit inside MinixFS's boot block (bytes 0..1024 of block 0),
// which the filesystem never reads. Overrunning it would overwrite the superblock.
const _: () = assert!(IMAGE_HDR_LEN <= 1024);

// A whole image must fit the VA window the kernel reserves for it, or the kernel's
// pre-map loop would run off the end of the window.
const _: () = assert!(ROOTFS_IMAGE_BYTES as u64 <= RAMDISK_WINDOW_SIZE);
// ...and it must be a whole number of blocks, which it is by construction — but
// the driver's geometry check leans on it, so state it.
const _: () = assert!(ROOTFS_IMAGE_BYTES.is_multiple_of(BDEV_BLOCK_SIZE));
const _: () = assert!(ROOTFS_IMAGE_BLOCKS > 0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_is_one_mebibyte_of_four_kib_blocks() {
        assert_eq!(ROOTFS_IMAGE_BLOCKS, 256);
        assert_eq!(ROOTFS_IMAGE_BYTES, 1024 * 1024);
        assert_eq!(ROOTFS_IMAGE_BYTES / BDEV_BLOCK_SIZE, 256);
        assert_eq!(ROOTFS_TAIL_BLOCK, 255);
        // The `[ramdisk] … len=1048576 pages=256` boot marker is these numbers.
        assert_eq!(ROOTFS_IMAGE_BYTES / 4096, 256);
    }

    #[test]
    fn the_two_labels_differ() {
        // Head and tail must be distinguishable, or a copy loop that never
        // advanced would pass both probes.
        assert_ne!(IMAGE_LABEL, IMAGE_TAIL_LABEL);
        assert_eq!(IMAGE_LABEL.len(), IMAGE_LABEL_LEN);
        assert_eq!(IMAGE_TAIL_LABEL.len(), IMAGE_LABEL_LEN);
    }

    #[test]
    fn each_label_is_nul_padded_and_not_full() {
        // A trailing NUL means the label reads as a C string too, which is what
        // makes it greppable in a hexdump without a length.
        for label in [IMAGE_LABEL, IMAGE_TAIL_LABEL] {
            assert_eq!(*label.last().unwrap(), 0);
            assert!(label.iter().any(|&b| b != 0));
        }
    }

    #[test]
    fn the_header_fields_are_ordered_and_fit_the_boot_block() {
        let fields = [
            ("label", HDR_LABEL_OFF, IMAGE_LABEL_LEN),
            ("blocks", HDR_BLOCKS_OFF, 4),
            ("block_size", HDR_BLOCK_SIZE_OFF, 4),
            ("mfs_magic", HDR_MFS_MAGIC_OFF, 4),
        ];
        for pair in fields.windows(2) {
            let (name, off, width) = pair[0];
            let (next_name, next_off, _) = pair[1];
            assert!(
                off + width <= next_off,
                "{name} ({off}..{}) overlaps {next_name} at {next_off}",
                off + width
            );
        }
        let (_, last_off, last_width) = fields[fields.len() - 1];
        assert!(last_off + last_width <= IMAGE_HDR_LEN);
        // MinixFS's superblock starts at byte 1024; the header must not reach it.
        assert_eq!(IMAGE_HDR_LEN.min(1024), IMAGE_HDR_LEN);
    }

    #[test]
    fn the_image_fits_the_ramdisk_window() {
        assert_eq!(
            (ROOTFS_IMAGE_BYTES as u64).min(RAMDISK_WINDOW_SIZE),
            ROOTFS_IMAGE_BYTES as u64,
            "the image does not fit the VA window reserved for it"
        );
    }
}

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

// ---------------------------------------------------------------------------
// The image's contents — a second build-time to run-time ABI (slice 5.8).
//
// `kernel/build.rs` builds the image from these; the MFS server reads them back
// over BDEV and compares. Exactly the reasoning [`IMAGE_LABEL`] above already
// carries, one level up: three crates that may not depend on each other
// (`tools/mkfs-mfs` writes, `fs/mfs` reads, `kernel/build.rs` orchestrates) have
// to agree on bytes, so the bytes live here.
//
// Until this slice these literals sat inline in `build_rootfs`, which made the
// server's read proof a *transcription* of them rather than a check against
// them — the failure mode where both sides are edited together and the test
// keeps passing while the content silently changed.
// ---------------------------------------------------------------------------

/// Path of the C milestone program inside the root image. Slice 5.9 execs it.
pub const ROOTFS_HELLO_PATH: &str = "/bin/hello";

/// Path of the greppable message-of-the-day file.
pub const ROOTFS_MOTD_PATH: &str = "/etc/motd";

/// Contents of [`ROOTFS_MOTD_PATH`], byte for byte.
///
/// Short enough to cross the whole stack in one `FS_READ`, and *greppable*: it
/// reaches the console verbatim when init reads the file and writes it to fd 1,
/// which is the slice-5.8 milestone marker. Content rather than length is what
/// the proof asserts — a path that moved the right number of wrong bytes is the
/// bug a length check cannot see.
pub const ROOTFS_MOTD: &[u8] = b"minix.rs rootfs: motd from MFS\n";

/// Path of the file that forces MinixFS's single-indirect zone arm.
pub const ROOTFS_PATTERN_PATH: &str = "/etc/pattern";

/// Length of [`ROOTFS_PATTERN_PATH`]: 40 KiB, i.e. 10 blocks.
///
/// **Mandatory rather than filler.** Seven direct zones cover 28 KiB, so a file
/// past that boundary is what keeps the single-indirect arm (and mkfs's indirect
/// writer) live. `/bin/hello` cannot serve: it is ~200 KB with a real C
/// toolchain but the 15 KB `worker` ELF in the musl-sysroot-absent fallback,
/// which fits inside the direct zones — so an indirect proof keyed on it would be
/// dead in exactly the configuration CI's non-QEMU jobs build. This length is
/// constant in every configuration.
pub const ROOTFS_PATTERN_LEN: usize = 40 * 1024;

/// Byte `i` of [`ROOTFS_PATTERN_PATH`]'s contents.
///
/// Position-dependent and non-repeating over a block (251 is prime and coprime
/// with the 4096-byte block size), so a read that lost, duplicated, or reordered
/// a block changes the bytes rather than landing on the same value again.
pub const fn rootfs_pattern_byte(i: usize) -> u8 {
    (i % 251) as u8
}

// The pattern really does run past the direct zones, which is the whole reason
// it exists. Seven zones at 4 KiB is 28 KiB; anything shorter would make the
// single-indirect arm unreachable without saying so.
const _: () = assert!(ROOTFS_PATTERN_LEN > 7 * BDEV_BLOCK_SIZE);
// ...and the motd must fit one FS transfer, so the read proof is one round trip.
const _: () = assert!(!ROOTFS_MOTD.is_empty());
const _: () = assert!(ROOTFS_MOTD.len() <= BDEV_BLOCK_SIZE);

/// A file the root image ships **empty**, for slice 5.10a's write proof to fill.
///
/// Create does not exist until 5.10b, so the write path needs a target that is
/// already in the image. Zero-length rather than pre-sized: that makes
/// growth-from-nothing the ordinary path rather than a special case, and it keeps
/// `/etc/motd` and `/etc/pattern` — which are *read* proofs — untouched by a
/// probe that writes.
pub const ROOTFS_SCRATCH_PATH: &str = "/etc/scratch";

/// Bytes init writes to [`ROOTFS_SCRATCH_PATH`]: 32 KiB, i.e. 8 blocks.
///
/// **Mandatory rather than round.** Seven direct zones cover 28 KiB, so this
/// length is what puts the single-indirect *allocation* arm — and the allocation
/// of the indirect block itself — on a boot marker. The last zone is indirect
/// slot 0. Unlike [`ROOTFS_PATTERN_LEN`] this content is written at runtime, so
/// the length is a claim init proves rather than something the image asserts.
pub const ROOTFS_SCRATCH_LEN: usize = 32 * 1024;

/// Period of [`rootfs_scratch_byte`]. Prime, and coprime with the 4096-byte
/// block, so a lost, duplicated, or reordered block changes the bytes rather
/// than landing on the same value again — [`rootfs_pattern_byte`]'s reasoning.
///
/// It is *also* what lets init hold a single source buffer: init's write chunk is
/// a whole multiple of this, so every chunk's contents are identical and one
/// `const`-generated static is correct for all of them.
pub const ROOTFS_SCRATCH_PERIOD: usize = 251;

/// Byte `i` of what init writes to [`ROOTFS_SCRATCH_PATH`].
///
/// Skewed by 7 off [`rootfs_pattern_byte`] so that reading the wrong file is a
/// mismatch rather than a coincidence.
pub const fn rootfs_scratch_byte(i: usize) -> u8 {
    ((i + 7) % ROOTFS_SCRATCH_PERIOD) as u8
}

// The scratch file really does run past the direct zones, which is the whole
// reason its length is what it is. Anything shorter would make the
// single-indirect *allocation* arm unreachable without saying so.
const _: () = assert!(ROOTFS_SCRATCH_LEN > 7 * BDEV_BLOCK_SIZE);
// ...and it must stay inside the single-indirect span, which is what MFS's
// writer covers: 7 direct zones plus one block of 4-byte pointers.
const _: () = assert!(ROOTFS_SCRATCH_LEN <= (7 + BDEV_BLOCK_SIZE / 4) * BDEV_BLOCK_SIZE);
// The image has room for it: 8 data zones plus 1 indirect block, against an
// image whose other contents leave well over that free. A future image shrink
// fails here rather than at boot with ENOSPC.
const _: () = assert!(ROOTFS_SCRATCH_LEN / BDEV_BLOCK_SIZE + 1 < ROOTFS_IMAGE_BLOCKS as usize);

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
    fn the_motd_is_one_greppable_line() {
        // It reaches the console verbatim as the slice-5.8 milestone marker, so
        // it must be exactly one line: no interior newline (which would split the
        // marker across two console writes and two log lines) and a trailing one.
        assert_eq!(*ROOTFS_MOTD.last().unwrap(), b'\n');
        assert_eq!(
            ROOTFS_MOTD.iter().filter(|&&b| b == b'\n').count(),
            1,
            "an interior newline would split the boot marker"
        );
        assert!(
            ROOTFS_MOTD[..ROOTFS_MOTD.len() - 1]
                .iter()
                .all(|&b| (0x20..0x7f).contains(&b)),
            "the marker must be printable ASCII to survive `grep -aF`"
        );
    }

    #[test]
    fn the_pattern_reaches_past_the_direct_zones() {
        // 40 KiB is ten 4 KiB blocks; the seven direct zones cover 28 KiB. So the
        // file's last three blocks are addressed through the single-indirect
        // block, which is the arm the `fs.indirect` boot marker exists to reach.
        assert_eq!(ROOTFS_PATTERN_LEN, 40 * 1024);
        assert_eq!(ROOTFS_PATTERN_LEN / BDEV_BLOCK_SIZE, 10);
        let direct = 7 * BDEV_BLOCK_SIZE;
        assert_eq!(direct, 28 * 1024);
        assert_eq!(
            ROOTFS_PATTERN_LEN.min(direct),
            direct,
            "the pattern no longer needs the indirect block"
        );
    }

    #[test]
    fn the_pattern_generator_does_not_repeat_within_a_block() {
        // Non-repeating over a block is what makes a lost or duplicated block
        // visible: two different offsets 4096 apart must not produce the same
        // byte, or a copy that returned the wrong block could still compare equal.
        for i in 0..BDEV_BLOCK_SIZE {
            assert_ne!(
                rootfs_pattern_byte(i),
                rootfs_pattern_byte(i + BDEV_BLOCK_SIZE),
                "offset {i} repeats one block later"
            );
        }
        // And it really is position-dependent at the offset the indirect proof
        // reads from (block 7, the first indirect one).
        let indirect_off = 7 * BDEV_BLOCK_SIZE;
        assert_ne!(
            rootfs_pattern_byte(indirect_off),
            rootfs_pattern_byte(indirect_off + 1)
        );
    }

    #[test]
    fn the_scratch_file_spans_the_direct_indirect_seam() {
        // W9/W3: the write proof must cross 7 direct zones, or the single-indirect
        // allocation arm has no boot marker. Same reasoning ROOTFS_PATTERN_LEN
        // records for the read side.
        assert_eq!(ROOTFS_SCRATCH_LEN, 32 * 1024);
        assert_eq!(
            ROOTFS_SCRATCH_LEN.min(7 * BDEV_BLOCK_SIZE),
            7 * BDEV_BLOCK_SIZE
        );
    }

    #[test]
    fn the_scratch_generator_is_position_dependent_and_skewed_off_the_pattern() {
        // Skewed by 7 so a cross-file mix-up is a mismatch, not a coincidence.
        assert_ne!(rootfs_scratch_byte(0), rootfs_pattern_byte(0));
        // Non-repeating across a block: 251 is prime and coprime with 4096.
        assert_ne!(rootfs_scratch_byte(0), rootfs_scratch_byte(BDEV_BLOCK_SIZE));
        // Periodic with period 251, which is what lets init hold one source buffer.
        assert_eq!(
            rootfs_scratch_byte(0),
            rootfs_scratch_byte(ROOTFS_SCRATCH_PERIOD)
        );
    }

    #[test]
    fn every_image_path_is_absolute_and_distinct() {
        let paths = [ROOTFS_HELLO_PATH, ROOTFS_MOTD_PATH, ROOTFS_PATTERN_PATH];
        for p in paths {
            assert!(p.starts_with('/'), "{p} is not absolute");
            assert!(!p.ends_with('/'), "{p} names a directory");
        }
        for (i, a) in paths.iter().enumerate() {
            for b in &paths[i + 1..] {
                assert_ne!(a, b);
            }
        }
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

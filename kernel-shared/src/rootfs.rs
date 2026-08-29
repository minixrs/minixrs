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

/// Inodes in the root filesystem image.
///
/// **128, i.e. two inode-table blocks.** 64 was one block and ample through
/// slice 5.9; slice 5.10b's `/full` directory (see [`ROOTFS_FULL_ENTRIES`]) costs
/// 62 inodes on its own. Raising it shifts `first_data_zone` by one block, which
/// moves every zone number in the image — the layout unit tests and mkfs's
/// fixtures move with it.
pub const ROOTFS_NINODES: u32 = 128;

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
/// Create did not exist when this file was chosen in 5.10a, so the write path
/// needed a target that was already in the image. Zero-length rather than
/// pre-sized: that makes growth-from-nothing the ordinary path rather than a
/// special case, and it keeps
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
/// Zones the write proof allocates at **runtime**: one per block of
/// [`ROOTFS_SCRATCH_LEN`], plus the single indirect block holding the pointer to
/// the eighth.
///
/// Named rather than open-coded because three places need the same number and
/// must not drift — this module's headroom guard below, `kernel/build.rs`'s check
/// that the *built* image really leaves that many zones free, and the test that
/// proves that check is not vacuous. The image ships `/etc/scratch` empty, so
/// every one of these zones is allocated by MFS while the kernel is running;
/// running out is `ENOSPC` at boot, which is why the count is checked against a
/// real image rather than reasoned about.
pub const ROOTFS_SCRATCH_GROWTH_ZONES: usize = ROOTFS_SCRATCH_LEN.div_ceil(BDEV_BLOCK_SIZE) + 1;

// The image is at least large enough in principle. This is a *necessary*
// condition only — it says nothing about what the image's other contents leave
// free, which depends on the `hello` flavour and so cannot be known here. The
// sufficient check is `kernel/build.rs`'s, against the bytes it just built.
const _: () = assert!(ROOTFS_SCRATCH_GROWTH_ZONES < ROOTFS_IMAGE_BLOCKS as usize);

/// Bytes per MinixFS v3 directory entry.
///
/// Duplicated from `minixrs_mfs::dirent::DIRENT_SIZE` — `fs/mfs` depends on this
/// crate, so the dependency cannot run the other way. `tools/mkfs-mfs` depends on
/// both and carries the test that pins them equal.
pub const ROOTFS_DIRENT_SIZE: usize = 64;

/// A **sparse** file the image ships, for the write-back proof.
///
/// Its first block is a hole and its second holds a pattern, so a write at
/// position 0 assigns `zone[0]` while `size` does not move. That is the only way
/// to reach the second half of MFS's write-back condition — "a zone was assigned
/// **or** the size grew" — because with no `lseek` every write runs forward from
/// a descriptor's position and therefore always extends the file. Slice 5.10a
/// left that half unproven and predicted `FS_TRUNC` would reach it; it does not,
/// and this file is the correction.
pub const ROOTFS_HOLEY_PATH: &str = "/etc/holey";

/// Length of [`ROOTFS_HOLEY_PATH`]: two blocks, the first of them a hole.
pub const ROOTFS_HOLEY_LEN: usize = 2 * BDEV_BLOCK_SIZE;

/// Byte `i` of [`ROOTFS_HOLEY_PATH`]'s **shipped** contents.
///
/// Zero throughout the hole — which is what a hole reads as, so the image is
/// self-consistent — and a position-dependent pattern after it. Skewed off
/// [`rootfs_scratch_byte`] and [`rootfs_pattern_byte`] so that reading the wrong
/// file is a mismatch rather than a coincidence.
pub const fn rootfs_holey_byte(i: usize) -> u8 {
    if i < BDEV_BLOCK_SIZE {
        0
    } else {
        ((i + 23) % ROOTFS_SCRATCH_PERIOD) as u8
    }
}

/// What init writes at position 0 of [`ROOTFS_HOLEY_PATH`], filling the hole.
pub const ROOTFS_HOLEY_TEXT: &[u8] = b"minix.rs holey: filled at zero\n";

/// A file the image ships **empty**, as the `EEXIST` probe's target.
///
/// Read by nothing else, so a probe that accidentally *succeeded* in creating a
/// second entry for this name would corrupt no proof but its own. It exists
/// because the probe re-resolves the name afterwards and compares inode numbers:
/// a dropped `EEXIST` would insert a duplicate entry shadowing the first,
/// silently, with every other marker still green.
pub const ROOTFS_DENY_PATH: &str = "/etc/deny";

/// A directory whose single block is **exactly full**, so that the first create
/// in it must allocate a second directory zone.
pub const ROOTFS_FULL_DIR: &str = "/full";

/// Files [`ROOTFS_FULL_DIR`] ships, all zero-length.
///
/// `.` and `..` plus these must be exactly one block of entries — the `const _`
/// below is what enforces it — so directory growth is on a boot marker rather
/// than being an arm no QEMU boot executes. They cost 62 inodes and no zones.
pub const ROOTFS_FULL_ENTRIES: usize = 62;

/// The create that must grow [`ROOTFS_FULL_DIR`].
pub const ROOTFS_FULL_NEW_PATH: &str = "/full/new";

/// What init writes to [`ROOTFS_FULL_NEW_PATH`].
pub const ROOTFS_DIRGROW_TEXT: &[u8] = b"minix.rs dirgrow by init\n";

/// A file that is **not** in the image, which init creates at boot.
pub const ROOTFS_CREATE_PATH: &str = "/etc/new";

/// What init writes to [`ROOTFS_CREATE_PATH`].
pub const ROOTFS_CREATE_TEXT: &[u8] = b"minix.rs created by init\n";

/// A file init creates to prove that a *failing* write allocates nothing.
pub const ROOTFS_LEAK_PATH: &str = "/etc/leak";

/// What init writes to [`ROOTFS_LEAK_PATH`] once the failing writes are done.
pub const ROOTFS_LEAK_TEXT: &[u8] = b"minix.rs leak: nothing lost\n";

/// Failing writes the leak probe issues before its one good write.
///
/// **[`ROOTFS_IMAGE_BLOCKS`], which is greater than any possible free-zone count
/// in the image**, so the probe is config-independent *by construction* rather
/// than by measurement — no number here differs between the musl, SDK and
/// sysroot-absent `hello` flavours, which is the slice-5.5/5.6 trap. Before the
/// staging fix each failure leaked one zone, so this many of them would exhaust
/// the image and the probe's final good write would answer `ENOSPC`.
pub const ROOTFS_LEAK_PROBES: usize = ROOTFS_IMAGE_BLOCKS as usize;

/// Zones the boot-time probes allocate at **runtime**, in total.
///
/// `/etc/scratch`'s eight data blocks and its indirect block
/// ([`ROOTFS_SCRATCH_GROWTH_ZONES`]), plus one each for `/etc/new`,
/// `/full/new`, `/etc/holey`'s filled hole and `/etc/leak`'s one good write, plus
/// one for `/full`'s second directory block. Checked against the *built* image by
/// `kernel/build.rs`, for the reason that check already carries: the image's
/// largest file is `/bin/hello`, whose size is a property of the toolchain
/// flavour, so no unit test over a fixture measures this image.
pub const ROOTFS_RUNTIME_ZONES: usize = ROOTFS_SCRATCH_GROWTH_ZONES + 5;

/// Inodes the boot-time probes allocate at runtime: `/etc/new`, `/full/new` and
/// `/etc/leak`.
pub const ROOTFS_RUNTIME_INODES: usize = 3;

// C10: `.` + `..` + the filler files must be exactly one block of entries. One
// short and the create finds a free slot; one over and mkfs has already grown the
// directory, so the arm this exists for stays unreachable either way.
const _: () = assert!(2 + ROOTFS_FULL_ENTRIES == BDEV_BLOCK_SIZE / ROOTFS_DIRENT_SIZE);

// The sparse file's hole is exactly its first block, and its tail is real.
const _: () = assert!(ROOTFS_HOLEY_LEN == 2 * BDEV_BLOCK_SIZE);
// ...and what init writes into the hole fits inside it, so the write assigns
// `zone[0]` and touches nothing else.
const _: () = assert!(!ROOTFS_HOLEY_TEXT.is_empty());
const _: () = assert!(ROOTFS_HOLEY_TEXT.len() < BDEV_BLOCK_SIZE);

// Each of the three created files fits one FS transfer, so its proof is one
// round trip and its marker's byte count is a literal.
const _: () = assert!(ROOTFS_CREATE_TEXT.len() <= BDEV_BLOCK_SIZE);
const _: () = assert!(ROOTFS_DIRGROW_TEXT.len() <= BDEV_BLOCK_SIZE);
const _: () = assert!(ROOTFS_LEAK_TEXT.len() <= BDEV_BLOCK_SIZE);

// The leak probe must out-number any free-zone count the image can have.
const _: () = assert!(ROOTFS_LEAK_PROBES >= ROOTFS_IMAGE_BLOCKS as usize);

// Necessary conditions only — the sufficient checks are `kernel/build.rs`'s,
// against the bytes it just built.
const _: () = assert!(ROOTFS_RUNTIME_ZONES < ROOTFS_IMAGE_BLOCKS as usize);
const _: () = assert!(ROOTFS_RUNTIME_INODES < ROOTFS_NINODES as usize);

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
        // Strictly past the seam, not merely up to it: at exactly 7 * BDEV_BLOCK_SIZE
        // the file still fits the direct zones and the arm this test exists to keep
        // on a boot marker becomes unreachable. `.min(seam + 1)` is the house idiom
        // for an ordering assertion (there is no `assert_gt!`), and it is the whole
        // point here — a `.min(seam)` form would hold at the failing length.
        assert_eq!(
            ROOTFS_SCRATCH_LEN.min(7 * BDEV_BLOCK_SIZE + 1),
            7 * BDEV_BLOCK_SIZE + 1
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
        let paths = [
            ROOTFS_HELLO_PATH,
            ROOTFS_MOTD_PATH,
            ROOTFS_PATTERN_PATH,
            ROOTFS_SCRATCH_PATH,
        ];
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

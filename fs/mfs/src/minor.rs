// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The verdict of the `bdev.minor` boot probe (slice 6.4, spec P6).
//!
//! Since slice 6.4 `virtio-blk` serves more than the root: minor 1
//! ([`BDEV_MINOR_WHOLE_DISK`]) is the whole disk and minor `1 + n` is GPT
//! partition *n*. Nothing in the system opens any of them — the filesystem
//! mounts minor 0 and that is all — so without a probe the driver's minor table
//! would be exercised by host tests alone. The server's prologue therefore
//! reads **block 0 of minor 1** once, and this module says what the answer
//! means. It is in the library for the reason [`crate::root`] is: no host test,
//! Miri or coverage job sees `main.rs`.
//!
//! Block 0 of the whole disk begins with LBA 0, the **protective MBR**, and
//! three facts about those 512 bytes tell it from block 0 of anything else the
//! driver could have served by mistake:
//!
//! | Served instead            | Offset 510 | Offset 450 | Offset 0        |
//! | ------------------------- | ---------- | ---------- | --------------- |
//! | the whole disk (correct)  | `55 AA`    | `EE`       | zero            |
//! | the root partition        | zero       | zero       | [`IMAGE_LABEL`] |
//! | the ESP (a FAT32 volume)  | `55 AA`    | zero       | a jump opcode   |
//!
//! The root's row is what `tools/mkfs-mfs` writes and tests: block 0 is the
//! 32-byte image header, then zeroes up to the superblock at byte 1024
//! (`verify.rs` asserts `img[IMAGE_HDR_LEN..SUPER_OFFSET]` is all zero), so it
//! has no boot signature — "minor 1 answered from the root extent" (the spec's
//! mutation 11) fails on the signature alone. The label is checked all the same
//! so that the verdict does not silently depend on the root image never growing
//! a boot block. The ESP's row is why the signature is not enough by itself: a
//! FAT boot sector ends in the same two bytes, and what it does not have is a
//! first partition record of type `0xEE`, which `tools/mkimage`'s
//! `protective_mbr` writes at 446 + 4.
//!
//! `memory` has no minor 1 and answers `ENXIO`, as it always has for any minor
//! but 0. That is the healthy answer on the ramdisk boot and has its own
//! verdict; every other reply that is not one whole block is a failure.

use crate::MFS_BLOCK_SIZE;
use minixrs_kernel_shared::callnr::BDEV_MINOR_WHOLE_DISK;
use minixrs_kernel_shared::error::ENXIO;
use minixrs_kernel_shared::rootfs::{HDR_LABEL_OFF, IMAGE_LABEL, IMAGE_LABEL_LEN};

/// The minor the probe reads: the whole disk.
pub const PROBE_MINOR: i32 = BDEV_MINOR_WHOLE_DISK;

/// Bytes of block 0 that are LBA 0. The disk's sectors are 512 bytes whatever
/// the BDEV block size is.
pub const MBR_LEN: usize = 512;
/// Offset of the boot signature in an MBR.
pub const MBR_SIGNATURE_OFF: usize = 510;
/// The boot signature.
pub const MBR_SIGNATURE: [u8; 2] = [0x55, 0xAA];
/// Offset of the first partition record's type byte: the record is at 446 and
/// its `OSType` is its fifth byte.
pub const MBR_PART0_TYPE_OFF: usize = 446 + 4;
/// The type a protective MBR gives its one record (UEFI, "Protective MBR").
pub const MBR_TYPE_GPT_PROTECTIVE: u8 = 0xEE;

// The MBR is the start of block 0, and both fields are inside it.
const _: () = assert!(MBR_LEN <= MFS_BLOCK_SIZE);
const _: () = assert!(MBR_SIGNATURE_OFF + MBR_SIGNATURE.len() == MBR_LEN);
const _: () = assert!(MBR_PART0_TYPE_OFF < MBR_SIGNATURE_OFF);
const _: () = assert!(HDR_LABEL_OFF + IMAGE_LABEL_LEN <= MBR_PART0_TYPE_OFF);

/// What the probe found.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MinorProbe {
    /// One whole block came back and it starts with a protective MBR:
    /// `bdev.minor ok minor=1 mbr=1`.
    Mbr,
    /// The driver has no such minor (`ENXIO`): `bdev.minor absent minor=1
    /// rc=-6`. What `memory` answers.
    Absent,
    /// Any other reply that is not one whole block — an errno, a short count, a
    /// long one: `bdev.minor FAIL read rc=<rc>`.
    FailRead(i32),
    /// One whole block came back and it is not the start of a GPT disk:
    /// `bdev.minor FAIL mbr`.
    FailMbr,
}

/// Classify the reply to the probe's `BDEV_READ`.
///
/// `rc` is the reply `m_type` (or the trap's errno if the SENDREC itself
/// failed) and `block` the block buffer afterwards. `block` is only looked at
/// when `rc` says the driver filled it.
pub fn classify(rc: i32, block: &[u8]) -> MinorProbe {
    if rc == ENXIO {
        return MinorProbe::Absent;
    }
    if i32::try_from(MFS_BLOCK_SIZE) != Ok(rc) {
        return MinorProbe::FailRead(rc);
    }
    let signed = block.get(MBR_SIGNATURE_OFF..MBR_LEN) == Some(&MBR_SIGNATURE[..]);
    let protective = block.get(MBR_PART0_TYPE_OFF) == Some(&MBR_TYPE_GPT_PROTECTIVE);
    let root = block.get(HDR_LABEL_OFF..HDR_LABEL_OFF + IMAGE_LABEL_LEN) == Some(&IMAGE_LABEL[..]);
    if signed && protective && !root {
        MinorProbe::Mbr
    } else {
        MinorProbe::FailMbr
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use minixrs_kernel_shared::callnr::BDEV_MINOR_ROOT;
    use minixrs_kernel_shared::error::{EINVAL, EIO, EPERM};
    use minixrs_kernel_shared::rootfs::IMAGE_HDR_LEN;
    use std::vec;
    use std::vec::Vec;

    const FULL: i32 = MFS_BLOCK_SIZE as i32;

    /// Block 0 of a GPT disk: a protective MBR, then the GPT header's signature
    /// at LBA 1.
    fn disk_block0() -> Vec<u8> {
        let mut b = vec![0u8; MFS_BLOCK_SIZE];
        b[MBR_PART0_TYPE_OFF] = MBR_TYPE_GPT_PROTECTIVE;
        b[MBR_SIGNATURE_OFF..MBR_LEN].copy_from_slice(&MBR_SIGNATURE);
        b[MBR_LEN..MBR_LEN + 8].copy_from_slice(b"EFI PART");
        b
    }

    /// Block 0 of the root image as `mkfs-mfs` lays it out: the labelled header,
    /// zeroes to byte 1024, then a (non-zero) superblock.
    fn root_block0() -> Vec<u8> {
        let mut b = vec![0u8; MFS_BLOCK_SIZE];
        b[..IMAGE_LABEL_LEN].copy_from_slice(&IMAGE_LABEL);
        b[IMAGE_LABEL_LEN..IMAGE_HDR_LEN].fill(0x01);
        b[1024..1056].fill(0x5A);
        b
    }

    /// Block 0 of a FAT32 volume: a jump, an OEM name, the boot signature.
    fn esp_block0() -> Vec<u8> {
        let mut b = vec![0u8; MFS_BLOCK_SIZE];
        b[..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        b[MBR_SIGNATURE_OFF..MBR_LEN].copy_from_slice(&MBR_SIGNATURE);
        b
    }

    #[test]
    fn the_probe_reads_the_whole_disk_minor_and_not_the_root() {
        assert_eq!(PROBE_MINOR, 1);
        assert_ne!(PROBE_MINOR, BDEV_MINOR_ROOT);
    }

    #[test]
    fn a_full_block_with_a_protective_mbr_is_ok() {
        assert_eq!(classify(FULL, &disk_block0()), MinorProbe::Mbr);
    }

    #[test]
    fn enxio_is_absent_whatever_the_buffer_holds() {
        // The buffer is stale on a refusal — here it even holds a perfect MBR.
        assert_eq!(classify(ENXIO, &disk_block0()), MinorProbe::Absent);
        assert_eq!(classify(ENXIO, &[]), MinorProbe::Absent);
        assert_eq!(ENXIO, -6, "the marker files assert rc=-6");
    }

    #[test]
    fn every_other_errno_is_a_failed_read_carrying_it() {
        for rc in [EIO, EINVAL, EPERM, -1, i32::MIN] {
            assert_eq!(
                classify(rc, &disk_block0()),
                MinorProbe::FailRead(rc),
                "rc={rc}"
            );
        }
    }

    #[test]
    fn a_reply_that_is_not_exactly_one_block_is_a_failed_read() {
        // Zero, short by one, long by one, absurd: the buffer is not trusted
        // even though it holds a perfect MBR.
        for rc in [0, 512, FULL - 1, FULL + 1, i32::MAX] {
            assert_eq!(
                classify(rc, &disk_block0()),
                MinorProbe::FailRead(rc),
                "rc={rc}"
            );
        }
    }

    #[test]
    fn the_root_partitions_block_0_is_not_an_mbr() {
        // Mutation 11: minor 1 answered from the root extent.
        assert_eq!(classify(FULL, &root_block0()), MinorProbe::FailMbr);
    }

    #[test]
    fn the_esps_block_0_is_not_a_protective_mbr() {
        // It carries the same boot signature; the 0xEE record is what it lacks.
        assert_eq!(classify(FULL, &esp_block0()), MinorProbe::FailMbr);
    }

    #[test]
    fn each_of_the_three_conditions_is_needed() {
        let mut no_sig = disk_block0();
        no_sig[MBR_SIGNATURE_OFF] = 0;
        assert_eq!(classify(FULL, &no_sig), MinorProbe::FailMbr);

        let mut half_sig = disk_block0();
        half_sig[MBR_SIGNATURE_OFF + 1] = 0x55;
        assert_eq!(classify(FULL, &half_sig), MinorProbe::FailMbr);

        let mut no_type = disk_block0();
        no_type[MBR_PART0_TYPE_OFF] = 0x0C; // FAT32 LBA: a real, non-GPT MBR
        assert_eq!(classify(FULL, &no_type), MinorProbe::FailMbr);

        // A root image that had grown a boot block with both MBR fields set is
        // still the root.
        let mut labelled = disk_block0();
        labelled[..IMAGE_LABEL_LEN].copy_from_slice(&IMAGE_LABEL);
        assert_eq!(classify(FULL, &labelled), MinorProbe::FailMbr);
    }

    #[test]
    fn a_zero_block_is_not_an_mbr() {
        assert_eq!(classify(FULL, &[0u8; MFS_BLOCK_SIZE]), MinorProbe::FailMbr);
    }

    #[test]
    fn a_buffer_shorter_than_a_sector_is_not_an_mbr_and_does_not_panic() {
        for len in [0, 1, MBR_PART0_TYPE_OFF, MBR_SIGNATURE_OFF, MBR_LEN - 1] {
            assert_eq!(
                classify(FULL, &disk_block0()[..len]),
                MinorProbe::FailMbr,
                "len={len}"
            );
        }
        // Exactly one sector is enough: nothing past LBA 0 is consulted.
        assert_eq!(classify(FULL, &disk_block0()[..MBR_LEN]), MinorProbe::Mbr);
    }
}

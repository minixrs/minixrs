// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Reading an image back — the block-fetching glue `minixrs-mfs` deliberately
//! omits, over a `&[u8]` instead of over a block device.
//!
//! Slice 5.8's MFS server writes the BDEV-backed equivalent of exactly these
//! functions: same decoders, same zone resolver, same directory walk, with
//! [`block`] replaced by a `BDEV_READ`. That symmetry is the point — the round
//! trip below exercises the reader the server will use, not a second reader
//! written to agree with the writer.

use minixrs_mfs::MFS_BLOCK_SIZE;
use minixrs_mfs::dirent::iter_block;
use minixrs_mfs::inode::{Inode, ROOT_INODE};
use minixrs_mfs::layout::{Layout, imap_bit, layout, zmap_bit};
use minixrs_mfs::read::{
    ZoneLookup, inode_at, inode_location, zone_for_offset, zone_from_indirect,
};
use minixrs_mfs::superblock::{SUPER_OFFSET, SUPER_ON_DISK_LEN, Superblock};

/// Borrow block `b` of `img`. `None` past the end of the image.
pub fn block(img: &[u8], b: u32) -> Option<&[u8]> {
    let start = (b as usize).checked_mul(MFS_BLOCK_SIZE)?;
    img.get(start..start.checked_add(MFS_BLOCK_SIZE)?)
}

/// Decode the superblock. `None` if the image is too short or the superblock is
/// not a MinixFS v3 this build reads.
pub fn superblock(img: &[u8]) -> Option<Superblock> {
    let sb = Superblock::from_le_bytes(img.get(SUPER_OFFSET..SUPER_OFFSET + SUPER_ON_DISK_LEN)?)?;
    sb.validate().ok()?;
    Some(sb)
}

/// The image's layout, derived from its own superblock.
pub fn image_layout(img: &[u8]) -> Option<Layout> {
    let sb = superblock(img)?;
    Some(layout(sb.ninodes, sb.zones, MFS_BLOCK_SIZE))
}

/// Fetch inode `ino`.
pub fn inode(img: &[u8], ino: u32) -> Option<Inode> {
    let l = image_layout(img)?;
    let (b, slot) = inode_location(ino, &l, MFS_BLOCK_SIZE)?;
    inode_at(block(img, b)?, slot)
}

/// Read a file's (or directory's) contents, honouring holes.
///
/// A hole reads as zeroes — that is what a hole *means* — while an offset past
/// what the format can address is `None`, never silent zeroes. That distinction is
/// the reason `zone_for_offset` has both a `Hole` and an `OutOfRange`.
pub fn read_inode_bytes(img: &[u8], ino: &Inode) -> Option<Vec<u8>> {
    let size = usize::try_from(ino.size).ok()?;
    let mut out = Vec::with_capacity(size);
    let mut off = 0usize;
    while off < size {
        let want = (size - off).min(MFS_BLOCK_SIZE);
        match zone_for_offset(ino, off as u64, MFS_BLOCK_SIZE) {
            ZoneLookup::Direct(z) => out.extend_from_slice(&block(img, z)?[..want]),
            ZoneLookup::Indirect { zone, slot } => {
                let z = zone_from_indirect(block(img, zone)?, slot)?;
                if z == 0 {
                    out.extend(std::iter::repeat_n(0u8, want));
                } else {
                    out.extend_from_slice(&block(img, z)?[..want]);
                }
            }
            ZoneLookup::Hole => out.extend(std::iter::repeat_n(0u8, want)),
            ZoneLookup::OutOfRange => return None,
        }
        off += want;
    }
    Some(out)
}

/// Resolve an absolute path to `(inode number, inode)`.
///
/// `"/"` is the root. Each component is looked up through the directory's own
/// entries, so a path that names a file as an intermediate component fails rather
/// than reading a file's bytes as directory entries.
pub fn lookup(img: &[u8], path: &str) -> Option<(u32, Inode)> {
    let mut ino = ROOT_INODE;
    let mut node = inode(img, ino)?;
    for component in path.split('/').filter(|c| !c.is_empty()) {
        if !node.is_dir() {
            return None;
        }
        let bytes = read_inode_bytes(img, &node)?;
        ino = bytes
            .chunks(MFS_BLOCK_SIZE)
            .flat_map(iter_block)
            .find(|e| e.name_str() == component)
            .map(|e| e.ino)?;
        node = inode(img, ino)?;
    }
    Some((ino, node))
}

/// Read a regular file's contents by path.
pub fn read_file(img: &[u8], path: &str) -> Option<Vec<u8>> {
    let (_, node) = lookup(img, path)?;
    if !node.is_reg() {
        return None;
    }
    read_inode_bytes(img, &node)
}

/// List a directory's entry names by path, in on-disk order (so `.` and `..`
/// come first).
pub fn read_dir(img: &[u8], path: &str) -> Option<Vec<String>> {
    let (_, node) = lookup(img, path)?;
    if !node.is_dir() {
        return None;
    }
    let bytes = read_inode_bytes(img, &node)?;
    Some(
        bytes
            .chunks(MFS_BLOCK_SIZE)
            .flat_map(iter_block)
            .map(|e| e.name_str().to_string())
            .collect(),
    )
}

/// Is inode `ino` marked allocated in the inode bitmap?
pub fn imap_bit_set(img: &[u8], ino: u32) -> Option<bool> {
    let l = image_layout(img)?;
    bit_set(img, l.imap_start, imap_bit(ino))
}

/// Is zone `zone` marked allocated in the zone bitmap? `None` for a zone below
/// the first data zone, which has no bit.
pub fn zmap_bit_set(img: &[u8], zone: u32) -> Option<bool> {
    let l = image_layout(img)?;
    bit_set(img, l.zmap_start, zmap_bit(zone, l.first_data_zone)?)
}

fn bit_set(img: &[u8], start: u32, bit: u32) -> Option<bool> {
    let byte = (start as usize).checked_mul(MFS_BLOCK_SIZE)? + (bit as usize) / 8;
    Some(img.get(byte)? & (1 << (bit % 8)) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{
        HDR_BLOCK_SIZE_OFF, HDR_BLOCKS_OFF, HDR_LABEL_OFF, HDR_MFS_MAGIC_OFF, IMAGE_HDR_LEN,
        IMAGE_LABEL, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL, MkfsError, ROOTFS_IMAGE_BLOCKS,
        ROOTFS_IMAGE_BYTES, ROOTFS_TAIL_BLOCK, build_image,
    };
    use crate::manifest::Manifest;
    use minixrs_kernel_shared::rootfs::ROOTFS_SCRATCH_LEN;
    use minixrs_mfs::inode::{NR_DIRECT_ZONES, NR_TZONES, SINGLE_INDIRECT_SLOT};

    /// A file large enough to need the indirect block: nine blocks, two past the
    /// seven direct zones.
    const BIG_LEN: usize = (NR_DIRECT_ZONES + 2) * MFS_BLOCK_SIZE;

    fn big_bytes() -> Vec<u8> {
        // Position-dependent and non-repeating over a block, so a copy that lost,
        // duplicated, or reordered a block changes the bytes.
        (0..BIG_LEN).map(|i| (i % 251) as u8).collect()
    }

    /// An image shaped like the real root filesystem: a `/bin` holding one large
    /// file, and an `/etc` holding a small one, an empty one, and a one-byte one.
    fn sample() -> Vec<u8> {
        let mut m = Manifest::new();
        m.add("/bin/hello", big_bytes())
            .add("/etc/motd", b"minix.rs rootfs motd\n".to_vec())
            .add("/etc/empty", Vec::new())
            .add("/etc/one", vec![0xA5]);
        build_image(&m).expect("the sample manifest fits the fixed image")
    }

    #[test]
    fn the_image_is_the_fixed_size_and_its_superblock_agrees() {
        let img = sample();
        assert_eq!(img.len(), ROOTFS_IMAGE_BYTES);

        let sb = superblock(&img).expect("the superblock decodes and validates");
        assert_eq!(sb.validate(), Ok(()));
        assert_eq!(sb.zones, ROOTFS_IMAGE_BLOCKS);
        assert_eq!(sb.zones as usize, img.len() / MFS_BLOCK_SIZE);
        assert_eq!(sb.block_size as usize, MFS_BLOCK_SIZE);
        assert_eq!(sb.log_zone_size, 0);
    }

    #[test]
    fn the_root_directory_lists_dot_dotdot_and_every_subdirectory() {
        let img = sample();
        assert_eq!(
            read_dir(&img, "/").unwrap(),
            [".", "..", "bin", "etc"],
            "root's entries, in on-disk order"
        );
        assert_eq!(
            read_dir(&img, "/etc").unwrap(),
            [".", "..", "motd", "empty", "one"],
            "a subdirectory's entries follow manifest order"
        );
        assert_eq!(read_dir(&img, "/bin").unwrap(), [".", "..", "hello"]);
    }

    #[test]
    fn every_directory_is_a_directory_and_every_file_is_a_file() {
        let img = sample();
        for dir in ["/", "/bin", "/etc"] {
            assert!(lookup(&img, dir).unwrap().1.is_dir(), "{dir}");
            assert_eq!(read_file(&img, dir), None, "{dir} is not a regular file");
        }
        for file in ["/bin/hello", "/etc/motd", "/etc/empty", "/etc/one"] {
            assert!(lookup(&img, file).unwrap().1.is_reg(), "{file}");
            assert_eq!(read_dir(&img, file), None, "{file} is not a directory");
        }
    }

    #[test]
    fn a_file_larger_than_seven_zones_uses_the_indirect_block() {
        // The arm that would otherwise be dead in the musl-sysroot-absent build,
        // where `hello` is a 15 KB `worker` ELF that fits the direct zones. This is
        // why `/etc/pattern` exists in the real manifest, and it is what this test
        // stands in for.
        let img = sample();
        let (_, node) = lookup(&img, "/bin/hello").unwrap();
        assert_eq!(node.size as usize, BIG_LEN);
        assert_ne!(
            node.zone[SINGLE_INDIRECT_SLOT], 0,
            "a nine-block file must have an indirect block"
        );
        for slot in 0..NR_DIRECT_ZONES {
            assert_ne!(
                node.zone[slot], 0,
                "direct zone {slot} must be filled first"
            );
        }
        assert_eq!(read_file(&img, "/bin/hello").unwrap(), big_bytes());
    }

    #[test]
    fn a_small_file_round_trips_byte_for_byte() {
        let img = sample();
        assert_eq!(
            read_file(&img, "/etc/motd").unwrap(),
            b"minix.rs rootfs motd\n"
        );
    }

    #[test]
    fn the_div_ceil_boundary_files_round_trip() {
        // A one-byte file occupies exactly one block; a zero-byte file occupies
        // none at all, which is the case `len.div_ceil(BLOCK_SIZE)` gets wrong if
        // it is written as `len / BLOCK_SIZE + 1`.
        let img = sample();
        assert_eq!(read_file(&img, "/etc/one").unwrap(), vec![0xA5]);
        assert_eq!(read_file(&img, "/etc/empty").unwrap(), Vec::<u8>::new());

        let (_, one) = lookup(&img, "/etc/one").unwrap();
        assert_eq!(one.size, 1);
        assert_ne!(one.zone[0], 0, "a one-byte file still needs a zone");

        let (_, empty) = lookup(&img, "/etc/empty").unwrap();
        assert_eq!(empty.size, 0);
        assert_eq!(empty.zone, [0u32; minixrs_mfs::inode::NR_TZONES]);
    }

    #[test]
    fn a_path_that_does_not_exist_resolves_to_nothing() {
        let img = sample();
        for missing in ["/bin/nope", "/nope/hello", "/etc/motd/x"] {
            assert_eq!(lookup(&img, missing), None, "{missing}");
        }
    }

    /// **This is the test that licenses the boot markers to check the image header
    /// instead of the superblock.** The `memory` driver is a *block* driver and must
    /// not depend on the filesystem format, so it verifies the header — which is only
    /// a meaningful check if the header cannot disagree with the real superblock.
    #[test]
    fn the_image_header_agrees_with_the_real_superblock() {
        let img = sample();
        let sb = superblock(&img).unwrap();

        assert_eq!(
            &img[HDR_LABEL_OFF..HDR_LABEL_OFF + IMAGE_LABEL_LEN],
            &IMAGE_LABEL
        );
        assert_eq!(
            u32::from_le_bytes(img[HDR_BLOCKS_OFF..HDR_BLOCKS_OFF + 4].try_into().unwrap()),
            sb.zones
        );
        assert_eq!(
            u32::from_le_bytes(
                img[HDR_BLOCK_SIZE_OFF..HDR_BLOCK_SIZE_OFF + 4]
                    .try_into()
                    .unwrap()
            ) as u16,
            sb.block_size
        );
        assert_eq!(
            i32::from_le_bytes(
                img[HDR_MFS_MAGIC_OFF..HDR_MFS_MAGIC_OFF + 4]
                    .try_into()
                    .unwrap()
            ),
            i32::from(sb.magic)
        );
    }

    #[test]
    fn the_header_sits_in_the_boot_block_and_leaves_the_superblock_alone() {
        // The header lives in bytes 0..32, MinixFS's superblock at 1024. If the
        // header ever grew into it, `superblock()` above would already be failing —
        // but state the gap, because "32 < 1024" is the whole reason this design
        // works.
        assert_eq!(IMAGE_HDR_LEN.min(SUPER_OFFSET), IMAGE_HDR_LEN);
        let img = sample();
        assert!(
            img[IMAGE_HDR_LEN..SUPER_OFFSET].iter().all(|&b| b == 0),
            "the rest of the boot block must stay zero"
        );
    }

    #[test]
    fn the_tail_label_sits_in_the_reserved_last_block() {
        // The tail is what distinguishes "the blob was copied" from "the blob's
        // first page was copied 256 times" — so it must be at the far end, and no
        // file may ever be allocated there.
        let img = sample();
        let off = ROOTFS_TAIL_BLOCK as usize * MFS_BLOCK_SIZE;
        assert_eq!(off, (ROOTFS_IMAGE_BLOCKS as usize - 1) * 4096);
        assert_eq!(&img[off..off + IMAGE_LABEL_LEN], &IMAGE_TAIL_LABEL);
        assert_ne!(&img[off..off + IMAGE_LABEL_LEN], &IMAGE_LABEL);

        assert_eq!(
            zmap_bit_set(&img, ROOTFS_TAIL_BLOCK),
            Some(true),
            "the tail block must be marked allocated so no file can land on it"
        );
    }

    #[test]
    fn every_allocated_inode_and_zone_is_marked_in_the_bitmaps() {
        let img = sample();
        // Root, /bin, /etc, then the four files: inodes 1..=7.
        for ino in 1..=7 {
            assert_eq!(imap_bit_set(&img, ino), Some(true), "inode {ino}");
        }
        assert_eq!(imap_bit_set(&img, 0), Some(true), "inode 0 is reserved");
        assert_eq!(imap_bit_set(&img, 8), Some(false), "inode 8 is free");

        // Every zone a file actually uses.
        let (_, hello) = lookup(&img, "/bin/hello").unwrap();
        for z in hello.zone.iter().copied().filter(|&z| z != 0) {
            assert_eq!(zmap_bit_set(&img, z), Some(true), "zone {z}");
        }
    }

    #[test]
    fn an_over_large_manifest_is_refused_with_exact_numbers() {
        // Two files of ~600 KB each cannot fit a 1 MiB image. The error names the
        // shortfall so the fix is one constant.
        let mut m = Manifest::new();
        m.add("/bin/a", vec![1u8; 600 * 1024])
            .add("/bin/b", vec![2u8; 600 * 1024]);
        match build_image(&m) {
            Err(MkfsError::TooBig { needed, have }) => {
                assert!(needed > have, "needed {needed} have {have}");
                assert!(
                    have < ROOTFS_IMAGE_BLOCKS,
                    "the free zones exclude metadata and the reserved tail"
                );
            }
            other => panic!("expected TooBig, got {other:?}"),
        }
    }

    #[test]
    fn the_build_is_deterministic() {
        // `kernel/build.rs` embeds this image in the kernel ELF, so two builds of
        // the same manifest must produce identical bytes.
        assert_eq!(sample(), sample());
    }

    #[test]
    fn a_zero_length_file_is_regular_empty_and_costs_no_zones() {
        // Slice 5.10a's write target ships empty, so growth-from-nothing is the
        // ordinary path rather than a special case. Nothing in the image had zero
        // length before, so this really is a new case for the writer.
        let mut m = Manifest::new();
        m.add("/etc/motd", b"hi\n".to_vec())
            .add("/etc/scratch", Vec::new());
        let img = build_image(&m).expect("image builds");

        let (_, node) = lookup(&img, "/etc/scratch").expect("scratch resolves");
        assert!(node.is_reg());
        assert_eq!(node.size, 0);
        assert_eq!(node.zone, [0u32; NR_TZONES]);
        assert_eq!(read_file(&img, "/etc/scratch"), Some(Vec::new()));
    }

    #[test]
    fn the_image_leaves_room_for_the_scratch_file_to_grow() {
        // The write allocates 8 data zones plus 1 indirect block at runtime. If a
        // future image shrink made that impossible, init's first write would answer
        // ENOSPC at boot rather than failing here.
        let img = sample();
        let l = image_layout(&img).expect("layout");
        let free = (l.first_data_zone..ROOTFS_TAIL_BLOCK)
            .filter(|z| zmap_bit_set(&img, *z) == Some(false))
            .count();
        assert!(
            free > ROOTFS_SCRATCH_LEN / MFS_BLOCK_SIZE,
            "only {free} free zones"
        );
    }
}

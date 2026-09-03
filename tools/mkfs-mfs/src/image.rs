// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The writer: turn a [`Manifest`] into a MinixFS v3 image.
//!
//! The image is a **fixed** [`ROOTFS_IMAGE_BLOCKS`] blocks regardless of its
//! contents — see that constant for why — so every size-derived boot marker is a
//! literal, in every build configuration. An image that no longer fits is a build
//! failure ([`MkfsError::TooBig`]) naming the constant to raise.
//!
//! ## Layout written
//!
//! ```text
//!   block 0                image header (bytes 0..32) + superblock (bytes 1024..1055)
//!   block 1                reserved
//!   block 2                inode bitmap
//!   block 3                zone bitmap
//!   block 4                inode table
//!   blocks 5..255          data zones (directories then files, in manifest order)
//!   block 255              reserved, holds IMAGE_TAIL_LABEL
//! ```
//!
//! (Block numbers are the slice-5.7 geometry; the code derives them from
//! [`minixrs_mfs::layout::layout`] and never hardcodes them.)
//!
//! ## Determinism
//!
//! No timestamps, no host state, no iteration over a hash map: the same manifest
//! produces the same bytes. `kernel/build.rs` embeds this image in the kernel
//! ELF, so a nondeterministic byte anywhere would make every kernel build differ.

use std::collections::BTreeSet;
use std::fmt;

use minixrs_mfs::MFS_BLOCK_SIZE;
use minixrs_mfs::dirent::{DIRENT_SIZE, DirEntry};
use minixrs_mfs::inode::{
    I_DIRECTORY, I_REGULAR, Inode, NR_DIRECT_ZONES, NR_TZONES, ROOT_INODE, SINGLE_INDIRECT_SLOT,
};
use minixrs_mfs::layout::{Layout, imap_bit, layout, zmap_bit};
use minixrs_mfs::read::{inode_location, ptrs_per_block};
use minixrs_mfs::superblock::{MFSFLAG_CLEAN, SUPER_MAGIC_V3, SUPER_OFFSET, Superblock};

use crate::manifest::{Manifest, split_path};

// The image-header ABI and the fixed geometry live in `kernel-shared` rather than
// here: this tool writes those bytes at build time, and the `memory` driver and
// VFS read them at run time, so all three need one source and none of them may
// depend on the others. Re-exported so the writer's own constants read locally.
pub use minixrs_kernel_shared::rootfs::{
    HDR_BLOCK_SIZE_OFF, HDR_BLOCKS_OFF, HDR_LABEL_OFF, HDR_MFS_MAGIC_OFF, IMAGE_HDR_LEN,
    IMAGE_LABEL, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL, ROOTFS_IMAGE_BLOCKS, ROOTFS_IMAGE_BYTES,
    ROOTFS_NINODES, ROOTFS_TAIL_BLOCK,
};

/// Permissions every regular file gets.
///
/// One value rather than a per-path rule: nothing in minix.rs enforces an
/// execute bit yet (slice 5.9 execs `/bin/hello` without consulting one), and a
/// hidden "files under /bin are 0755" convention would be a rule with no
/// enforcement and no test. 0755 is the one that stays correct when permission
/// checking does arrive.
const FILE_MODE: u16 = I_REGULAR | 0o755;
/// Permissions every directory gets.
const DIR_MODE: u16 = I_DIRECTORY | 0o755;

/// Largest file this format addresses: seven direct zones plus one single-indirect
/// block. `fs/mfs`'s reader stops here too, and for the same reason.
fn max_file_bytes() -> usize {
    (NR_DIRECT_ZONES + ptrs_per_block(MFS_BLOCK_SIZE)) * MFS_BLOCK_SIZE
}

/// Why an image could not be built.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MkfsError {
    /// The manifest needs more data blocks than the fixed image has. `needed` and
    /// `have` are both block counts, so the fix is arithmetic: raise
    /// [`ROOTFS_IMAGE_BLOCKS`] by at least `needed - have`.
    TooBig { needed: u32, have: u32 },
    /// More inodes than the fixed image has. Raise [`ROOTFS_NINODES`].
    TooManyInodes { needed: u32, have: u32 },
    /// A path is not of the form `/<dir>/<name>` — see [`split_path`].
    BadPath(String),
    /// Two manifest entries name the same path.
    Duplicate(String),
    /// An entry's hole is not a whole number of blocks, covers the whole file, or
    /// names a prefix of the contents that is not already zero.
    BadHole(String),
}

impl fmt::Display for MkfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooBig { needed, have } => write!(
                f,
                "root image needs {needed} data blocks but only {have} are free; \
                 raise ROOTFS_IMAGE_BLOCKS (kernel-shared/src/rootfs.rs) by at least {}",
                needed.saturating_sub(*have)
            ),
            Self::TooManyInodes { needed, have } => write!(
                f,
                "root image needs {needed} inodes but only {have} exist; \
                 raise ROOTFS_NINODES (kernel-shared/src/rootfs.rs)"
            ),
            Self::BadPath(p) => write!(f, "{p:?} is not of the form /<dir>/<name>"),
            Self::Duplicate(p) => write!(f, "{p:?} appears twice in the manifest"),
            Self::BadHole(p) => write!(
                f,
                "{p:?} has a hole that is not whole blocks of zeroes inside the file"
            ),
        }
    }
}

impl std::error::Error for MkfsError {}

/// Build the root filesystem image.
pub fn build_image(manifest: &Manifest) -> Result<Vec<u8>, MkfsError> {
    let tree = Tree::plan(manifest)?;
    tree.write()
}

// ----- planning -------------------------------------------------------------

/// A directory and the manifest entries inside it.
struct PlannedDir {
    name: String,
    ino: u32,
    /// Indices into `Manifest::entries`, in manifest order.
    files: Vec<usize>,
}

/// A file, with its assigned inode.
struct PlannedFile {
    name: String,
    ino: u32,
}

/// The whole image, resolved: every inode number assigned and every directory's
/// byte content built, before a single block is allocated. Planning first is what
/// lets [`MkfsError::TooBig`] report exact numbers instead of "ran out somewhere".
struct Tree<'a> {
    manifest: &'a Manifest,
    layout: Layout,
    dirs: Vec<PlannedDir>,
    /// Parallel to `manifest.entries`.
    files: Vec<PlannedFile>,
    /// Directory contents, in the order they are written: root first, then each
    /// directory. Paired with the inode that owns them.
    dir_blocks: Vec<(u32, Vec<u8>)>,
}

impl<'a> Tree<'a> {
    fn plan(manifest: &'a Manifest) -> Result<Self, MkfsError> {
        // 1. Validate every path, and group files by directory in first-seen order.
        let mut seen_paths: BTreeSet<&str> = BTreeSet::new();
        let mut dirs: Vec<PlannedDir> = Vec::new();
        let mut file_names: Vec<String> = Vec::with_capacity(manifest.entries.len());

        for (i, entry) in manifest.entries.iter().enumerate() {
            let (dir, name) = split_path(&entry.path)?;
            if entry.hole != 0 {
                let bad = || MkfsError::BadHole(entry.path.clone());
                if !entry.hole.is_multiple_of(MFS_BLOCK_SIZE) || entry.hole >= entry.bytes.len() {
                    return Err(bad());
                }
                if entry.bytes[..entry.hole].iter().any(|&b| b != 0) {
                    return Err(bad());
                }
            }
            if !seen_paths.insert(entry.path.as_str()) {
                return Err(MkfsError::Duplicate(entry.path.clone()));
            }
            match dirs.iter_mut().find(|d| d.name == dir) {
                Some(d) => d.files.push(i),
                None => dirs.push(PlannedDir {
                    name: dir.to_string(),
                    ino: 0, // assigned below
                    files: vec![i],
                }),
            }
            file_names.push(name.to_string());
        }

        // 2. Assign inodes: root, then directories, then files in manifest order.
        //    Deterministic and independent of any hash iteration.
        let mut next_ino = ROOT_INODE + 1;
        for d in &mut dirs {
            d.ino = next_ino;
            next_ino += 1;
        }
        let files: Vec<PlannedFile> = file_names
            .into_iter()
            .map(|name| {
                let ino = next_ino;
                next_ino += 1;
                PlannedFile { name, ino }
            })
            .collect();

        let needed_inodes = next_ino - 1; // inode numbering starts at 1
        if needed_inodes > ROOTFS_NINODES {
            return Err(MkfsError::TooManyInodes {
                needed: needed_inodes,
                have: ROOTFS_NINODES,
            });
        }

        // 3. Build every directory's bytes. A directory is written through the
        //    same data path as a file, so nothing here caps it at one block.
        let mut dir_blocks = Vec::with_capacity(dirs.len() + 1);
        let mut root = DirBuilder::new(ROOT_INODE, ROOT_INODE);
        for d in &dirs {
            root.push(d.ino, &d.name);
        }
        dir_blocks.push((ROOT_INODE, root.finish()));
        for d in &dirs {
            let mut b = DirBuilder::new(d.ino, ROOT_INODE);
            for &i in &d.files {
                b.push(files[i].ino, &files[i].name);
            }
            dir_blocks.push((d.ino, b.finish()));
        }

        let layout = layout(ROOTFS_NINODES, ROOTFS_IMAGE_BLOCKS, MFS_BLOCK_SIZE);

        let tree = Self {
            manifest,
            layout,
            dirs,
            files,
            dir_blocks,
        };
        tree.check_block_budget()?;
        Ok(tree)
    }

    /// Every zone from `first_data_zone` up to but not including the reserved tail
    /// block is available for data.
    fn available_zones(&self) -> u32 {
        ROOTFS_TAIL_BLOCK.saturating_sub(self.layout.first_data_zone)
    }

    fn check_block_budget(&self) -> Result<(), MkfsError> {
        let have = self.available_zones();
        let mut needed = 0u32;
        let sized = self
            .dir_blocks
            .iter()
            .map(|(_, b)| (b.as_slice(), 0usize))
            .chain(
                self.manifest
                    .entries
                    .iter()
                    .map(|e| (e.bytes.as_slice(), e.hole)),
            );
        for (bytes, hole) in sized {
            if bytes.len() > max_file_bytes() {
                // Unreachable while the image is smaller than the format's reach,
                // but reported in the same terms rather than left to a panic.
                return Err(MkfsError::TooBig {
                    needed: blocks_for(bytes.len(), hole),
                    have,
                });
            }
            needed += blocks_for(bytes.len(), hole);
        }
        if needed > have {
            return Err(MkfsError::TooBig { needed, have });
        }
        Ok(())
    }

    // ----- writing ----------------------------------------------------------

    fn write(&self) -> Result<Vec<u8>, MkfsError> {
        let mut img = Image::new();
        let mut alloc = ZoneAlloc {
            next: self.layout.first_data_zone,
            limit: ROOTFS_TAIL_BLOCK,
            have: self.available_zones(),
        };

        // Directories first (root, then each subdirectory), then files. The order
        // only affects which zone numbers things land on, but fixing it keeps the
        // image byte-for-byte reproducible.
        let dir_count = self.dirs.len() as u16;
        for (i, (ino, bytes)) in self.dir_blocks.iter().enumerate() {
            let zones = write_data(&mut img, &mut alloc, bytes, 0)?;
            let nlinks = if *ino == ROOT_INODE {
                // "." + ".." + one per subdirectory's own "..".
                2 + dir_count
            } else {
                2
            };
            debug_assert!(i == 0 || *ino != ROOT_INODE);
            img.write_inode(
                &self.layout,
                *ino,
                Inode {
                    mode: DIR_MODE,
                    nlinks,
                    size: bytes.len() as i32,
                    zone: zones,
                    ..Inode::EMPTY
                },
            );
        }

        for (entry, planned) in self.manifest.entries.iter().zip(&self.files) {
            let zones = write_data(&mut img, &mut alloc, &entry.bytes, entry.hole)?;
            img.write_inode(
                &self.layout,
                planned.ino,
                Inode {
                    mode: FILE_MODE,
                    nlinks: 1,
                    size: entry.bytes.len() as i32,
                    zone: zones,
                    ..Inode::EMPTY
                },
            );
        }

        // Inode bitmap: bit 0 is reserved (there is no inode 0), then one bit per
        // inode actually in use.
        img.set_bit(self.layout.imap_start, imap_bit(0));
        img.set_bit(self.layout.imap_start, imap_bit(ROOT_INODE));
        for ino in self
            .dirs
            .iter()
            .map(|d| d.ino)
            .chain(self.files.iter().map(|f| f.ino))
        {
            img.set_bit(self.layout.imap_start, imap_bit(ino));
        }

        // Zone bitmap: bit 0 is reserved (it maps to `first_data_zone - 1`, which
        // is metadata), every allocated zone is in use, and so is the reserved
        // tail block — no file may ever be given the block the tail label lives in.
        img.set_bit(self.layout.zmap_start, 0);
        for zone in self.layout.first_data_zone..alloc.next {
            let bit = zmap_bit(zone, self.layout.first_data_zone)
                .expect("an allocated zone is at or above first_data_zone");
            img.set_bit(self.layout.zmap_start, bit);
        }
        let tail_bit = zmap_bit(ROOTFS_TAIL_BLOCK, self.layout.first_data_zone)
            .expect("the tail block is above first_data_zone");
        img.set_bit(self.layout.zmap_start, tail_bit);

        img.write_superblock(&self.layout);
        img.write_header();
        img.write_tail();
        Ok(img.bytes)
    }
}

/// Blocks a file occupies, including its indirect block if it needs one and
/// excluding any leading hole.
///
/// The *indirect* test is on the total block count, not the allocated one: a hole
/// does not renumber the zones after it, so a file whose last block sits past the
/// seventh still needs an indirect block however much of its front is missing.
fn blocks_for(len: usize, hole: usize) -> u32 {
    let total = len.div_ceil(MFS_BLOCK_SIZE);
    let holed = hole / MFS_BLOCK_SIZE;
    let indirect = usize::from(total > NR_DIRECT_ZONES);
    (total.saturating_sub(holed) + indirect) as u32
}

/// Accumulates a directory's entries. Every directory starts with `.` and `..`,
/// which is what makes `nlinks` 2 for a leaf directory.
struct DirBuilder {
    bytes: Vec<u8>,
}

impl DirBuilder {
    fn new(self_ino: u32, parent_ino: u32) -> Self {
        let mut b = Self { bytes: Vec::new() };
        b.push(self_ino, ".");
        b.push(parent_ino, "..");
        b
    }

    fn push(&mut self, ino: u32, name: &str) {
        let e = DirEntry::new(ino, name.as_bytes())
            .expect("manifest paths are validated by split_path before reaching here");
        self.bytes.extend_from_slice(&e.to_le_bytes());
    }

    fn finish(self) -> Vec<u8> {
        debug_assert!(self.bytes.len().is_multiple_of(DIRENT_SIZE));
        self.bytes
    }
}

/// Hands out data zones from `first_data_zone` upward, stopping before the
/// reserved tail block.
struct ZoneAlloc {
    next: u32,
    limit: u32,
    have: u32,
}

impl ZoneAlloc {
    fn alloc(&mut self) -> Result<u32, MkfsError> {
        if self.next >= self.limit {
            // Unreachable after `check_block_budget`, which is what reports exact
            // numbers; this keeps a planning bug from becoming a panic.
            return Err(MkfsError::TooBig {
                needed: self.have + 1,
                have: self.have,
            });
        }
        let z = self.next;
        self.next += 1;
        Ok(z)
    }
}

/// Write `data` into freshly allocated zones and return the inode's zone array.
///
/// The indirect block is allocated lazily, on the first zone past the seventh, so
/// a file inside the direct range costs no extra block. Blocks inside `hole` are
/// skipped entirely: the pointer stays 0, which is exactly what the reader treats
/// as a hole.
fn write_data(
    img: &mut Image,
    alloc: &mut ZoneAlloc,
    data: &[u8],
    hole: usize,
) -> Result<[u32; NR_TZONES], MkfsError> {
    let mut zone = [0u32; NR_TZONES];
    let mut indirect: Option<u32> = None;
    let hole_blocks = hole / MFS_BLOCK_SIZE;

    for (i, chunk) in data.chunks(MFS_BLOCK_SIZE).enumerate() {
        // A hole: no zone at all. The pointer stays 0, which is exactly what the
        // reader treats as a hole, and the prefix is already zero (checked in
        // `plan`), so the file's bytes are unchanged by not storing them.
        if i < hole_blocks {
            continue;
        }
        let z = alloc.alloc()?;
        img.block_mut(z)[..chunk.len()].copy_from_slice(chunk);

        if i < NR_DIRECT_ZONES {
            zone[i] = z;
            continue;
        }
        let ind = match indirect {
            Some(ind) => ind,
            None => {
                let ind = alloc.alloc()?;
                zone[SINGLE_INDIRECT_SLOT] = ind;
                indirect = Some(ind);
                ind
            }
        };
        let slot = i - NR_DIRECT_ZONES;
        img.block_mut(ind)[slot * 4..slot * 4 + 4].copy_from_slice(&z.to_le_bytes());
    }
    Ok(zone)
}

// ----- raw image ------------------------------------------------------------

struct Image {
    bytes: Vec<u8>,
}

impl Image {
    fn new() -> Self {
        Self {
            bytes: vec![0u8; ROOTFS_IMAGE_BYTES],
        }
    }

    fn block_mut(&mut self, b: u32) -> &mut [u8] {
        let start = b as usize * MFS_BLOCK_SIZE;
        &mut self.bytes[start..start + MFS_BLOCK_SIZE]
    }

    /// Set bit `bit` of the bitmap starting at block `start`.
    ///
    /// MINIX stores bitmaps as `bitchunk_t` (u32) arrays, bit `b` in chunk `b/32`
    /// at position `b%32`. On a little-endian device that is byte-for-byte
    /// identical to plain `byte = b/8, mask = 1 << (b%8)` addressing, which is
    /// what this does — and both the build host and aarch64 are little-endian, the
    /// same assumption the MXBI archive already makes.
    fn set_bit(&mut self, start: u32, bit: u32) {
        let byte = start as usize * MFS_BLOCK_SIZE + (bit as usize) / 8;
        self.bytes[byte] |= 1 << (bit % 8);
    }

    fn write_inode(&mut self, layout: &Layout, ino: u32, inode: Inode) {
        let (block, slot) = inode_location(ino, layout, MFS_BLOCK_SIZE)
            .expect("inode counts are checked against ROOTFS_NINODES during planning");
        let off = block as usize * MFS_BLOCK_SIZE + slot * minixrs_mfs::inode::INODE_SIZE;
        self.bytes[off..off + minixrs_mfs::inode::INODE_SIZE].copy_from_slice(&inode.to_le_bytes());
    }

    fn write_superblock(&mut self, layout: &Layout) {
        let sb = Superblock {
            ninodes: ROOTFS_NINODES,
            // Legacy v1 field: MINIX writes 0 when the zone count does not fit.
            nzones_v1: u16::try_from(ROOTFS_IMAGE_BLOCKS).unwrap_or(0),
            imap_blocks: layout.imap_blocks as i16,
            zmap_blocks: layout.zmap_blocks as i16,
            first_data_zone_old: u16::try_from(layout.first_data_zone).unwrap_or(0),
            log_zone_size: 0, // one block per zone
            flags: MFSFLAG_CLEAN,
            max_size: max_file_bytes() as i32,
            zones: ROOTFS_IMAGE_BLOCKS,
            magic: SUPER_MAGIC_V3,
            pad2: 0,
            block_size: MFS_BLOCK_SIZE as u16,
            disk_version: 0,
        };
        let bytes = sb.to_le_bytes();
        self.bytes[SUPER_OFFSET..SUPER_OFFSET + bytes.len()].copy_from_slice(&bytes);
    }

    /// The device-level header, in the boot block MinixFS never reads.
    fn write_header(&mut self) {
        let mut hdr = [0u8; IMAGE_HDR_LEN];
        write_label(&mut hdr, &IMAGE_LABEL);
        self.bytes[..IMAGE_HDR_LEN].copy_from_slice(&hdr);
    }

    /// The tail label, in the reserved last block. Carries the same three
    /// geometry fields as the header so either end is self-describing.
    fn write_tail(&mut self) {
        let mut tail = [0u8; IMAGE_HDR_LEN];
        write_label(&mut tail, &IMAGE_TAIL_LABEL);
        let off = ROOTFS_TAIL_BLOCK as usize * MFS_BLOCK_SIZE;
        self.bytes[off..off + IMAGE_HDR_LEN].copy_from_slice(&tail);
    }
}

/// Fill a 32-byte header (or tail) with `label` plus the three geometry fields.
fn write_label(hdr: &mut [u8; IMAGE_HDR_LEN], label: &[u8; IMAGE_LABEL_LEN]) {
    hdr[HDR_LABEL_OFF..HDR_LABEL_OFF + IMAGE_LABEL_LEN].copy_from_slice(label);
    hdr[HDR_BLOCKS_OFF..HDR_BLOCKS_OFF + 4].copy_from_slice(&ROOTFS_IMAGE_BLOCKS.to_le_bytes());
    hdr[HDR_BLOCK_SIZE_OFF..HDR_BLOCK_SIZE_OFF + 4]
        .copy_from_slice(&(MFS_BLOCK_SIZE as u32).to_le_bytes());
    hdr[HDR_MFS_MAGIC_OFF..HDR_MFS_MAGIC_OFF + 4]
        .copy_from_slice(&i32::from(SUPER_MAGIC_V3).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify;

    #[test]
    fn blocks_for_counts_the_indirect_block_only_when_it_is_needed() {
        assert_eq!(blocks_for(0, 0), 0);
        assert_eq!(blocks_for(1, 0), 1);
        assert_eq!(blocks_for(MFS_BLOCK_SIZE, 0), 1);
        assert_eq!(blocks_for(MFS_BLOCK_SIZE + 1, 0), 2);
        // Exactly seven blocks still fits the direct zones...
        assert_eq!(blocks_for(NR_DIRECT_ZONES * MFS_BLOCK_SIZE, 0), 7);
        // ...and the eighth costs an indirect block as well as itself.
        assert_eq!(blocks_for(NR_DIRECT_ZONES * MFS_BLOCK_SIZE + 1, 0), 9);
    }

    #[test]
    fn blocks_for_excludes_the_hole_but_not_the_indirect_test() {
        // A one-block hole in a two-block file costs one zone, not two.
        assert_eq!(blocks_for(2 * MFS_BLOCK_SIZE, MFS_BLOCK_SIZE), 1);
        // A hole below the seam does not change whether the *total* length needs
        // an indirect block -- the file's last block still sits past the seventh.
        let past_seam = (NR_DIRECT_ZONES + 1) * MFS_BLOCK_SIZE;
        assert_eq!(
            blocks_for(past_seam, MFS_BLOCK_SIZE),
            blocks_for(past_seam, 0) - 1,
            "one fewer data zone, same indirect block"
        );
    }

    #[test]
    fn the_maximum_file_is_the_single_indirect_span() {
        assert_eq!(max_file_bytes(), 4_222_976);
        assert_eq!(
            max_file_bytes(),
            (7 + MFS_BLOCK_SIZE / 4) * MFS_BLOCK_SIZE,
            "seven direct zones plus one indirect block of pointers"
        );
    }

    #[test]
    fn a_duplicate_path_is_refused() {
        let mut m = Manifest::new();
        m.add("/etc/motd", b"a".to_vec())
            .add("/etc/motd", b"b".to_vec());
        assert_eq!(
            build_image(&m),
            Err(MkfsError::Duplicate("/etc/motd".to_string()))
        );
    }

    #[test]
    fn a_bad_path_is_refused() {
        let mut m = Manifest::new();
        m.add("/motd", b"a".to_vec());
        assert_eq!(
            build_image(&m),
            Err(MkfsError::BadPath("/motd".to_string()))
        );
    }

    #[test]
    fn too_many_files_exhausts_the_inode_table() {
        let mut m = Manifest::new();
        // One directory plus N files; root and the directory take two inodes.
        for i in 0..ROOTFS_NINODES {
            m.add(format!("/etc/f{i}"), b"x".to_vec());
        }
        match build_image(&m) {
            Err(MkfsError::TooManyInodes { needed, have }) => {
                assert_eq!(have, ROOTFS_NINODES);
                assert_eq!(needed, ROOTFS_NINODES + 2);
            }
            other => panic!("expected TooManyInodes, got {other:?}"),
        }
    }

    #[test]
    fn a_sparse_file_reads_back_whole_but_allocates_only_its_tail() {
        // The hole occupies no zone, so the image spends one block on a
        // two-block file -- and `read_inode_bytes` still returns all 8192 bytes,
        // because a zero zone pointer *means* zeroes.
        let mut bytes = vec![0u8; 2 * MFS_BLOCK_SIZE];
        for (i, b) in bytes.iter_mut().enumerate().skip(MFS_BLOCK_SIZE) {
            *b = (i % 251) as u8;
        }
        let mut m = Manifest::new();
        m.add_sparse("/etc/holey", bytes.clone(), MFS_BLOCK_SIZE);
        let img = build_image(&m).expect("a one-hole file is buildable");

        assert_eq!(verify::read_file(&img, "/etc/holey"), Some(bytes));
        let (_, node) = verify::lookup(&img, "/etc/holey").expect("it is there");
        assert_eq!(node.zone[0], 0, "the hole must occupy no zone");
        assert_ne!(node.zone[1], 0, "the tail must be allocated");
        assert_eq!(node.size, 2 * MFS_BLOCK_SIZE as i32);
    }

    #[test]
    fn a_hole_that_is_not_whole_blocks_or_not_zero_is_refused() {
        // Both would have the image claim content it does not store.
        for (bytes, hole) in [
            (vec![0u8; 2 * MFS_BLOCK_SIZE], MFS_BLOCK_SIZE + 1), // not block-aligned
            (vec![1u8; 2 * MFS_BLOCK_SIZE], MFS_BLOCK_SIZE),     // prefix not zero
            (vec![0u8; MFS_BLOCK_SIZE], MFS_BLOCK_SIZE),         // wholly hole
        ] {
            let mut m = Manifest::new();
            m.add_sparse("/etc/holey", bytes, hole);
            assert_eq!(
                build_image(&m),
                Err(MkfsError::BadHole("/etc/holey".to_string())),
                "hole {hole}"
            );
        }
    }

    #[test]
    fn the_shared_dirent_size_matches_the_format_crate() {
        // `kernel-shared` cannot depend on `minixrs-mfs` (the dependency runs the
        // other way), so `ROOTFS_DIRENT_SIZE` is a duplicate. This crate depends
        // on both and is therefore the only place that can pin them equal.
        assert_eq!(
            minixrs_kernel_shared::rootfs::ROOTFS_DIRENT_SIZE,
            minixrs_mfs::dirent::DIRENT_SIZE
        );
    }

    #[test]
    fn a_directory_of_exactly_sixty_four_entries_fills_one_block() {
        // C10's arithmetic, checked against a real image rather than reasoned
        // about: `.` + `..` + ROOTFS_FULL_ENTRIES must be exactly one block, so
        // that the first create in it has to grow the directory.
        use minixrs_kernel_shared::rootfs::ROOTFS_FULL_ENTRIES;
        let mut m = Manifest::new();
        for i in 0..ROOTFS_FULL_ENTRIES {
            m.add(format!("/full/f{i:02}"), Vec::new());
        }
        let img = build_image(&m).expect("62 empty files fit");
        let (_, dir) = verify::lookup(&img, "/full").expect("the directory is there");
        assert_eq!(
            dir.size, MFS_BLOCK_SIZE as i32,
            "exactly full, not one short"
        );
    }
}

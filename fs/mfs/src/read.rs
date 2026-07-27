// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Locating an inode, and resolving a file offset to a zone.
//!
//! Every function here takes bytes the caller already fetched and returns *what
//! to fetch next*, never fetching anything itself. That is what lets slice 5.8's
//! MFS server drive the same code over `BDEV_READ` while `tools/mkfs-mfs` drives
//! it over a `Vec<u8>`, with no trait, no I/O abstraction, and no fake device in
//! the tests.
//!
//! ## Why `OutOfRange` is explicit
//!
//! [`zone_for_offset`] distinguishes a **hole** (a zone pointer that is legitimately
//! zero — the file is sparse there, and reading it yields zeroes) from an offset
//! **past the single-indirect span**, which is a caller bug. Collapsing the second
//! into "read zeroes" is how a filesystem silently returns wrong data; the D4
//! documented-`EINVAL` discipline says the caller hears about it.
//!
//! ## Why double-indirect is not implemented
//!
//! Seven direct zones plus one single-indirect block of `block_size / 4` pointers
//! is `7 + 1024` zones at a 4 KiB block size — **4.03 MiB**, which is larger than
//! any image this filesystem holds (the slice-5.7 root image is 1 MiB, and the VA
//! window reserved for it is 4 MiB). A double-indirect reader would therefore be
//! code no test could reach with a real image. [`ZoneLookup::OutOfRange`] is the
//! honest answer until an image needs one.

use crate::inode::{INODE_SIZE, Inode, NR_DIRECT_ZONES, SINGLE_INDIRECT_SLOT};
use crate::layout::Layout;

/// What backs a given file offset.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ZoneLookup {
    /// The zone is named directly by the inode. Always non-zero.
    Direct(u32),
    /// The zone number lives at `slot` of the indirect block at zone `zone`.
    /// Fetch that block and pass it to [`zone_from_indirect`].
    Indirect { zone: u32, slot: usize },
    /// A hole: the relevant pointer is zero, so this range of the file has never
    /// been written and reads as zeroes.
    Hole,
    /// Past what the single-indirect span can address. See the module docs.
    OutOfRange,
}

/// Zone pointers one indirect block can hold.
pub fn ptrs_per_block(block_size: usize) -> usize {
    block_size / 4
}

/// Which block of the inode table holds inode `ino`, and its slot within it.
///
/// `None` for inode 0 (which does not exist) or an inode past the table. The
/// bound comes from `layout.inode_blocks`, so a caller cannot read an inode out of
/// the first data block by asking for one past the end.
pub fn inode_location(ino: u32, layout: &Layout, block_size: usize) -> Option<(u32, usize)> {
    if ino == 0 {
        return None;
    }
    let per_block = (block_size / INODE_SIZE) as u32;
    if per_block == 0 {
        return None;
    }
    let index = ino - 1;
    let block = layout.inode_start.checked_add(index / per_block)?;
    if block >= layout.inode_start.checked_add(layout.inode_blocks)? {
        return None;
    }
    Some((block, (index % per_block) as usize))
}

/// Decode the inode at `slot` of a fetched inode-table block.
///
/// `None` if the slot runs past the block — which is how a caller that mixed up
/// `block_size` finds out, rather than by reading the next inode's bytes.
pub fn inode_at(inode_block: &[u8], slot: usize) -> Option<Inode> {
    let start = slot.checked_mul(INODE_SIZE)?;
    let end = start.checked_add(INODE_SIZE)?;
    Inode::from_le_bytes(inode_block.get(start..end)?)
}

/// Resolve byte offset `off` within `inode` to the zone that backs it.
///
/// `off` is **not** checked against `inode.size`: that is the caller's policy
/// decision (a reader clamps to the size, `fsck` does not), and mixing it in here
/// would make a hole past EOF indistinguishable from a hole inside the file.
pub fn zone_for_offset(inode: &Inode, off: u64, block_size: usize) -> ZoneLookup {
    if block_size == 0 {
        return ZoneLookup::OutOfRange;
    }
    let index = off / block_size as u64;

    if index < NR_DIRECT_ZONES as u64 {
        let zone = inode.zone[index as usize];
        return if zone == 0 {
            ZoneLookup::Hole
        } else {
            ZoneLookup::Direct(zone)
        };
    }

    let slot = index - NR_DIRECT_ZONES as u64;
    if slot >= ptrs_per_block(block_size) as u64 {
        return ZoneLookup::OutOfRange;
    }
    let indirect = inode.zone[SINGLE_INDIRECT_SLOT];
    if indirect == 0 {
        // No indirect block at all: everything it would have covered is a hole.
        return ZoneLookup::Hole;
    }
    ZoneLookup::Indirect {
        zone: indirect,
        slot: slot as usize,
    }
}

/// Read zone pointer `slot` out of a fetched indirect block.
///
/// `None` if the slot runs past the block. `Some(0)` is a **hole**, not an error:
/// an indirect block's unused entries are zero, exactly like an inode's.
pub fn zone_from_indirect(block: &[u8], slot: usize) -> Option<u32> {
    let start = slot.checked_mul(4)?;
    let b = block.get(start..start.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inode::{I_REGULAR, NR_TZONES, ROOT_INODE};
    use crate::layout::layout;

    const BS: usize = crate::MFS_BLOCK_SIZE;

    #[test]
    fn ptrs_per_block_is_a_thousand_and_twenty_four_at_four_kib() {
        assert_eq!(ptrs_per_block(BS), 1024);
        assert_eq!(ptrs_per_block(1024), 256);
    }

    #[test]
    fn the_root_inode_is_the_first_slot_of_the_first_inode_block() {
        let l = layout(64, 256, BS);
        assert_eq!(inode_location(ROOT_INODE, &l, BS), Some((l.inode_start, 0)));
    }

    #[test]
    fn inodes_fill_a_block_then_move_to_the_next() {
        // 64 inodes per 4 KiB block. Inode 64 is the last slot of block 0 of the
        // table; inode 65 would be the first slot of block 1 — but this geometry
        // only has one inode block, so it is out of range instead.
        let l = layout(128, 256, BS); // two inode blocks
        assert_eq!(inode_location(64, &l, BS), Some((l.inode_start, 63)));
        assert_eq!(inode_location(65, &l, BS), Some((l.inode_start + 1, 0)));
        assert_eq!(inode_location(128, &l, BS), Some((l.inode_start + 1, 63)));
    }

    #[test]
    fn inode_zero_and_inodes_past_the_table_have_no_location() {
        let l = layout(64, 256, BS);
        assert_eq!(inode_location(0, &l, BS), None);
        // 64 inodes = exactly one block; 65 runs into the first data block, which
        // is precisely the read this bound exists to prevent.
        assert_eq!(inode_location(65, &l, BS), None);
        assert_eq!(inode_location(u32::MAX, &l, BS), None);
    }

    #[test]
    fn inode_at_decodes_the_named_slot_and_refuses_the_rest() {
        let mut block = [0u8; BS];
        let mut ino = Inode::EMPTY;
        ino.mode = I_REGULAR;
        ino.size = 99;
        ino.zone[0] = 12;
        block[63 * INODE_SIZE..64 * INODE_SIZE].copy_from_slice(&ino.to_le_bytes());

        assert_eq!(inode_at(&block, 63), Some(ino));
        assert_eq!(inode_at(&block, 0), Some(Inode::EMPTY));
        assert_eq!(inode_at(&block, 64), None, "past the end of the block");
        assert_eq!(inode_at(&block, usize::MAX), None, "no overflow panic");
    }

    /// An inode with every direct zone filled and a single-indirect block.
    fn spread() -> Inode {
        let mut i = Inode::EMPTY;
        i.mode = I_REGULAR;
        for slot in 0..NR_TZONES {
            i.zone[slot] = 200 + slot as u32;
        }
        i
    }

    #[test]
    fn the_first_seven_zones_come_straight_from_the_inode() {
        let i = spread();
        for idx in 0..NR_DIRECT_ZONES {
            let off = (idx * BS) as u64;
            assert_eq!(
                zone_for_offset(&i, off, BS),
                ZoneLookup::Direct(i.zone[idx]),
                "index {idx}"
            );
            // Any offset *within* the block resolves to the same zone.
            assert_eq!(
                zone_for_offset(&i, off + BS as u64 - 1, BS),
                ZoneLookup::Direct(i.zone[idx])
            );
        }
    }

    #[test]
    fn the_eighth_zone_onwards_goes_through_the_indirect_block() {
        let i = spread();
        // Index 7 is slot 0 of the indirect block — the boundary the whole
        // indirect arm hangs on.
        assert_eq!(
            zone_for_offset(&i, (NR_DIRECT_ZONES * BS) as u64, BS),
            ZoneLookup::Indirect {
                zone: i.zone[SINGLE_INDIRECT_SLOT],
                slot: 0
            }
        );
        assert_eq!(
            zone_for_offset(&i, ((NR_DIRECT_ZONES + 5) * BS) as u64, BS),
            ZoneLookup::Indirect {
                zone: i.zone[SINGLE_INDIRECT_SLOT],
                slot: 5
            }
        );
        // The last slot the single-indirect block can hold.
        let last = NR_DIRECT_ZONES + ptrs_per_block(BS) - 1;
        assert_eq!(
            zone_for_offset(&i, (last * BS) as u64, BS),
            ZoneLookup::Indirect {
                zone: i.zone[SINGLE_INDIRECT_SLOT],
                slot: ptrs_per_block(BS) - 1
            }
        );
    }

    #[test]
    fn one_zone_past_the_indirect_span_is_out_of_range_not_a_hole() {
        // The documented-EINVAL discipline: a filesystem that answered `Hole` here
        // would hand the caller zeroes for data it simply cannot address.
        let i = spread();
        let past = NR_DIRECT_ZONES + ptrs_per_block(BS);
        assert_eq!(
            zone_for_offset(&i, (past * BS) as u64, BS),
            ZoneLookup::OutOfRange
        );
        assert_eq!(zone_for_offset(&i, u64::MAX, BS), ZoneLookup::OutOfRange);
    }

    #[test]
    fn the_indirect_span_covers_more_than_four_mebibytes() {
        // The justification for not implementing double-indirect, as arithmetic
        // rather than prose: the largest addressable offset exceeds the 4 MiB VA
        // window reserved for the ramdisk, so no image can outgrow it.
        let addressable = (NR_DIRECT_ZONES + ptrs_per_block(BS)) as u64 * BS as u64;
        assert_eq!(addressable, 4_222_976);
        assert_eq!(addressable.max(4 * 1024 * 1024), addressable);
    }

    #[test]
    fn a_zero_pointer_is_a_hole_direct_or_indirect() {
        let mut i = Inode::EMPTY;
        i.mode = I_REGULAR;
        // No zones at all: every offset inside the direct range is a hole...
        assert_eq!(zone_for_offset(&i, 0, BS), ZoneLookup::Hole);
        // ...and so is everything the (absent) indirect block would have covered.
        assert_eq!(
            zone_for_offset(&i, (NR_DIRECT_ZONES * BS) as u64, BS),
            ZoneLookup::Hole
        );
        // A sparse file: zone 0 present, zone 1 missing, zone 2 present.
        i.zone[0] = 9;
        i.zone[2] = 11;
        assert_eq!(zone_for_offset(&i, 0, BS), ZoneLookup::Direct(9));
        assert_eq!(zone_for_offset(&i, BS as u64, BS), ZoneLookup::Hole);
        assert_eq!(
            zone_for_offset(&i, 2 * BS as u64, BS),
            ZoneLookup::Direct(11)
        );
    }

    #[test]
    fn zone_from_indirect_reads_the_named_slot() {
        let mut block = [0u8; BS];
        block[4..8].copy_from_slice(&77u32.to_le_bytes());
        block[(ptrs_per_block(BS) - 1) * 4..].copy_from_slice(&88u32.to_le_bytes());

        assert_eq!(zone_from_indirect(&block, 0), Some(0), "an unused slot");
        assert_eq!(zone_from_indirect(&block, 1), Some(77));
        assert_eq!(zone_from_indirect(&block, ptrs_per_block(BS) - 1), Some(88));
        assert_eq!(zone_from_indirect(&block, ptrs_per_block(BS)), None);
        assert_eq!(zone_from_indirect(&block, usize::MAX), None);
    }
}

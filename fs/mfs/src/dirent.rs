// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! MinixFS v3 directory entries.
//!
//! Layout is MINIX 3's `struct direct` (`include/minix/dir.h`) at V3's sizing: a
//! 4-byte inode number followed by a 60-byte name field, 64 bytes total, so a
//! 4096-byte block holds exactly 64 entries.
//!
//! The name is **NUL-padded, not NUL-terminated**: a name of exactly
//! [`NAME_MAX`] bytes fills the field with no room for a terminator. That is why
//! [`DirEntry::name_str`] trims at the first NUL rather than looking for one.
//!
//! Inode number `0` marks a free slot. Directories are not compacted — a removed
//! entry leaves a zeroed slot behind — so a reader must skip them, which is what
//! [`iter_block`] does.

/// Bytes per on-disk directory entry.
pub const DIRENT_SIZE: usize = 64;

/// Longest file name, in bytes (`MFS_DIRSIZ` for V3).
pub const NAME_MAX: usize = 60;

/// Offset of the inode number within an entry (u32).
pub const DIRENT_INO_OFF: usize = 0;
/// Offset of the name field within an entry ([`NAME_MAX`] bytes, NUL-padded).
pub const DIRENT_NAME_OFF: usize = 4;

const _: () = assert!(DIRENT_NAME_OFF + NAME_MAX == DIRENT_SIZE);
const _: () = assert!(crate::MFS_BLOCK_SIZE.is_multiple_of(DIRENT_SIZE));

/// One directory entry, decoded.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// Inode number. `0` marks a free slot.
    pub ino: u32,
    /// The name field verbatim, NUL-padded.
    pub name: [u8; NAME_MAX],
}

impl DirEntry {
    /// A free slot.
    pub const EMPTY: Self = Self {
        ino: 0,
        name: [0; NAME_MAX],
    };

    /// Build an entry from an inode number and a name. `None` if the name is
    /// empty, longer than [`NAME_MAX`], or contains a NUL or `/`.
    ///
    /// Rejecting the separator here is what keeps a path component from being
    /// able to *contain* a path: `mkfs-mfs` splits on `/` before it gets here, and
    /// a name that smuggled one back in would resolve differently depending on
    /// which side of the split looked at it.
    pub fn new(ino: u32, name: &[u8]) -> Option<Self> {
        if name.is_empty() || name.len() > NAME_MAX {
            return None;
        }
        if name.contains(&0) || name.contains(&b'/') {
            return None;
        }
        let mut e = Self::EMPTY;
        e.ino = ino;
        e.name[..name.len()].copy_from_slice(name);
        Some(e)
    }

    /// Decode an entry from the first [`DIRENT_SIZE`] bytes of `b`. `None` if `b`
    /// is short.
    pub fn from_le_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < DIRENT_SIZE {
            return None;
        }
        let ino = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let mut name = [0u8; NAME_MAX];
        name.copy_from_slice(&b[DIRENT_NAME_OFF..DIRENT_NAME_OFF + NAME_MAX]);
        Some(Self { ino, name })
    }

    /// Encode back to the on-disk form.
    pub fn to_le_bytes(&self) -> [u8; DIRENT_SIZE] {
        let mut b = [0u8; DIRENT_SIZE];
        b[DIRENT_INO_OFF..DIRENT_INO_OFF + 4].copy_from_slice(&self.ino.to_le_bytes());
        b[DIRENT_NAME_OFF..DIRENT_NAME_OFF + NAME_MAX].copy_from_slice(&self.name);
        b
    }

    /// The name as text, trimmed at the first NUL.
    ///
    /// Empty for a name that is not valid UTF-8 — the `BootImage::record`
    /// precedent. A caller comparing against a known name therefore gets a
    /// mismatch rather than a panic, and a free slot (all-zero name) is empty too,
    /// which is consistent: neither names anything.
    pub fn name_str(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }
}

/// Iterate the **live** entries of one directory block, skipping free slots.
///
/// A trailing partial entry (a block whose length is not a multiple of
/// [`DIRENT_SIZE`]) is ignored rather than half-decoded, so a short read cannot
/// synthesize an entry out of whatever followed it.
pub fn iter_block(block: &[u8]) -> impl Iterator<Item = DirEntry> + '_ {
    // `as_chunks` rather than `chunks_exact`: the chunk size is a constant, and
    // it drops the trailing partial entry into `.1`, which we ignore.
    block
        .as_chunks::<DIRENT_SIZE>()
        .0
        .iter()
        .filter_map(|c| DirEntry::from_le_bytes(c))
        .filter(|e| e.ino != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codec_round_trips() {
        let e = DirEntry::new(7, b"hello").unwrap();
        assert_eq!(DirEntry::from_le_bytes(&e.to_le_bytes()), Some(e));
        assert_eq!(e.name_str(), "hello");
        assert_eq!(e.ino, 7);
    }

    #[test]
    fn an_entry_is_sixty_four_bytes_and_a_block_holds_sixty_four() {
        assert_eq!(DIRENT_SIZE, 64);
        assert_eq!(NAME_MAX, 60);
        assert_eq!(crate::MFS_BLOCK_SIZE / DIRENT_SIZE, 64);
    }

    #[test]
    fn a_maximum_length_name_round_trips_without_a_terminator() {
        // The whole point of NUL-*padding*: at exactly NAME_MAX there is no room
        // for a terminator, so a reader that looked for one would truncate.
        let name = [b'x'; NAME_MAX];
        let e = DirEntry::new(3, &name).unwrap();
        let back = DirEntry::from_le_bytes(&e.to_le_bytes()).unwrap();
        assert_eq!(back.name_str().len(), NAME_MAX);
        assert_eq!(back.name, name);
    }

    #[test]
    fn a_name_one_byte_too_long_is_refused() {
        assert_eq!(DirEntry::new(3, &[b'x'; NAME_MAX + 1]), None);
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(DirEntry::new(3, b""), None);
    }

    #[test]
    fn a_name_containing_a_separator_or_nul_is_refused() {
        assert_eq!(DirEntry::new(3, b"bin/hello"), None);
        assert_eq!(DirEntry::new(3, b"he\0llo"), None);
    }

    #[test]
    fn a_short_buffer_decodes_to_none() {
        let e = DirEntry::new(1, b".").unwrap();
        let b = e.to_le_bytes();
        assert_eq!(DirEntry::from_le_bytes(&b[..DIRENT_SIZE - 1]), None);
    }

    #[test]
    fn iter_block_skips_free_slots_and_trailing_partials() {
        let mut block = [0u8; DIRENT_SIZE * 4 + 7];
        for (i, (ino, name)) in [(1u32, &b"."[..]), (0, b"ghost"), (2, b".."), (9, b"bin")]
            .into_iter()
            .enumerate()
        {
            let e = DirEntry::new(ino, name).unwrap();
            block[i * DIRENT_SIZE..(i + 1) * DIRENT_SIZE].copy_from_slice(&e.to_le_bytes());
        }
        // Slot 1 was written with ino 0, so it is free regardless of its name.
        let names: heapless::Names = iter_block(&block).map(|e| e.ino).collect();
        assert_eq!(names.as_slice(), &[1, 2, 9]);
        // ...and the 7 trailing bytes never became a fifth entry.
        assert_eq!(iter_block(&block).count(), 3);
    }

    /// A tiny fixed-capacity collector so the iterator test needs no allocator.
    mod heapless {
        #[derive(Default)]
        pub struct Names {
            buf: [u32; 8],
            len: usize,
        }
        impl Names {
            pub fn as_slice(&self) -> &[u32] {
                &self.buf[..self.len]
            }
        }
        impl FromIterator<u32> for Names {
            fn from_iter<I: IntoIterator<Item = u32>>(it: I) -> Self {
                let mut n = Self::default();
                for v in it {
                    n.buf[n.len] = v;
                    n.len += 1;
                }
                n
            }
        }
    }
}

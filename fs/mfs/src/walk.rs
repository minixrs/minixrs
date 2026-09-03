// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Path traversal and read policy, allocator-free (slice 5.8).
//!
//! This is the `Vec`-free rewrite of `tools/mkfs-mfs`'s `verify.rs`, which is the
//! reference implementation: same decoders, same zone resolver, same directory
//! walk, with `block()` replaced by a `BDEV_READ`. The structure is deliberately
//! mirrored so the two can be diffed, and the `fs.selfcheck` boot marker is the
//! one place the two readers meet over a real image.
//!
//! Two of `verify.rs`'s functions could not survive the move to `no_std`:
//!
//!   * `read_inode_bytes` materializes a whole directory into a `Vec`. The server
//!     has one 4 KiB block buffer and a one-page stack, so it **streams** instead —
//!     one block at a time, asking [`find_in_block`] about each. That is why this
//!     module offers a per-block search rather than a directory reader.
//!   * `read_dir` is absent entirely. Nothing needs it: there is no `GETDENTS`
//!     request, on the `CDEV_READ` precedent that a request without a consumer is
//!     better absent than stubbed.
//!
//! ## Every bound here exists to stop a loop
//!
//! The server's loop bounds all come from the *device*, and a corrupt image is
//! not a hypothetical the way it is for a filesystem with an `fsck`: a directory
//! whose inode claims `size = i32::MAX` would spin MFS forever, and a spinning
//! MFS blocks VFS, which blocks init, which takes the slice-5.4-to-5.6 markers
//! down with it. So a negative size is [`EIO`], an over-large directory is
//! [`EIO`], and every zone is bounds-checked ([`zone_ok`]) *before* it becomes a
//! `BDEV_READ`. None of this is reachable from a VFS probe — VFS cannot forge a
//! corrupt inode — which is exactly why it needs unit tests instead.

use minixrs_kernel_shared::callnr::FS_MAX_IO;
use minixrs_kernel_shared::error::{EINVAL, EIO, EISDIR, ENAMETOOLONG};

use crate::MFS_BLOCK_SIZE;
use crate::dirent::{NAME_MAX, iter_block};

/// Largest directory this server will scan, in bytes: the seven direct zones.
///
/// `tools/mkfs-mfs` never builds a directory past one block (a 4 KiB block holds
/// 64 entries), so this is 7× headroom for the images that exist. A directory
/// larger than this is [`EIO`] rather than a silent truncation — answering
/// `ENOENT` for a name that is really there is the one failure mode a filesystem
/// must not have, and "I cannot read this" is the honest alternative.
pub const MAX_DIR_BYTES: usize = 7 * MFS_BLOCK_SIZE;

/// What one [`FS_READ`](minixrs_kernel_shared::callnr::FS_READ) round moves.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Chunk {
    /// Bytes to transfer. **`0` is end of file**, not an error — and it is the
    /// only way a client learns where a file ends, since no size is cached
    /// anywhere along the path.
    pub len: usize,
    /// Byte offset within the block that backs the transfer. A chunk never
    /// straddles two blocks, which is what lets the server stage through a single
    /// block buffer.
    pub off_in_block: usize,
}

/// Check an absolute path and hand it back.
///
/// The three ways a path can be malformed, in the order a caller should hear
/// about them:
///
/// 1. Empty, or not starting with `/` → `EINVAL`. There is no working directory
///    on minix.rs and no `chdir`, so a relative path is not a thing that could be
///    resolved — it is a malformed request, not a missing file.
/// 2. A component longer than [`NAME_MAX`] → `ENAMETOOLONG`. MinixFS cannot
///    *store* such a name, so no image can contain one and `ENOENT` would be a
///    less informative way of saying the same thing.
/// 3. Otherwise the path itself, unchanged.
///
/// The overall length cap (`FS_PATH_MAX`) is enforced by the wire codec in
/// `proto.rs`, where the field's width is: a path that filled its field with no
/// NUL never becomes a `&str` at all.
pub fn parse_path(path: &str) -> Result<&str, i32> {
    if !path.starts_with('/') {
        return Err(EINVAL);
    }
    for component in components(path) {
        if component.len() > NAME_MAX {
            return Err(ENAMETOOLONG);
        }
    }
    Ok(path)
}

/// The non-empty components of a path, in order.
///
/// Repeated and trailing slashes collapse — `/etc//motd/` walks the same two
/// components as `/etc/motd` — which is POSIX's rule and `verify.rs`'s. `"/"`
/// yields nothing at all, so it resolves to the root inode without a single
/// directory read.
pub fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|c| !c.is_empty())
}

/// Find `name` among the directory entries of one fetched block.
///
/// **Returns the inode number by value**, so the caller's borrow of the block
/// buffer ends at the call. That is not a style preference: the buffer is a
/// single shared staging area, and the next thing the caller does is fetch the
/// *inode's* block into it. Returning a `DirEntry` — or worse a `&str` into the
/// block — would make holding a name across that fetch expressible.
///
/// Free slots (`ino == 0`) are skipped by [`iter_block`], and a name that is not
/// valid UTF-8 decodes to `""`, which cannot equal any component `parse_path`
/// accepted. So neither can be matched by accident.
pub fn find_in_block(block: &[u8], name: &str) -> Option<u32> {
    iter_block(block)
        .find(|e| e.name_str() == name)
        .map(|e| e.ino)
}

/// Validate a directory's size and return it as a scan bound.
///
/// Separate from [`clamp_read`] because the two answer different questions: a
/// *file* read clamps to the size, while a *directory* scan has to reject one it
/// cannot bound. See [`MAX_DIR_BYTES`].
pub fn dir_size(size: i32) -> Result<usize, i32> {
    if size < 0 {
        return Err(EIO);
    }
    let size = size as usize;
    if size > MAX_DIR_BYTES {
        return Err(EIO);
    }
    Ok(size)
}

/// How many bytes to scan in the directory round starting at `off`, or `None`
/// when the directory is out.
///
/// The step function of the loop that drives [`dir_size`] and [`find_in_block`].
/// It lives here rather than in the server's `find_component` for the reason
/// `rw::advance` lives in VFS's library: `main.rs` is behind
/// `required-features = ["server"]`, so clippy, miri and llvm-cov never compile
/// it, and this is the only arithmetic in that loop. It was **unreachable at
/// boot through slice 5.10a** — `tools/mkfs-mfs` never built a directory past
/// one block, so the second iteration had no image to run against and unit
/// tests were the only thing that exercised it. As of the `fs.dirgrow` probe
/// that is no longer true: `/full` grows to a second block at runtime, and the
/// re-open walks both.
///
/// Three answers, in the order they can arise:
///
/// 1. `off >= size` → `None`. The termination condition, and the reason the caller
///    needs no bound of its own: [`dir_size`] has already refused a `size` this
///    could not converge on.
/// 2. `bs == 0` → `None`. Unreachable (the superblock validator rejects it), but a
///    zero-length round would advance `off` by nothing and spin forever, which is
///    precisely the failure mode this module exists to make impossible.
/// 3. Otherwise `min(size - off, bs)` — the rest of the directory, capped at one
///    block, because one block is the whole staging buffer.
///
/// The returned length is always `> 0` and never carries `off` past `size`, so a
/// caller advancing by it terminates in at most `size.div_ceil(bs)` rounds.
pub fn next_dir_chunk(off: usize, size: usize, bs: usize) -> Option<usize> {
    if off >= size || bs == 0 {
        return None;
    }
    Some((size - off).min(bs))
}

/// Decide how much of one read request to serve in this round.
///
/// `size` is the inode's (signed on disk, so a negative one is a corrupt inode →
/// `EIO`), `pos` the file offset, `len` the bytes asked for, `bs` the block size.
///
/// Four clamps, and the order does not matter because they are all minima — but
/// each has its own reason:
///
/// 1. **To the file's size.** Past the end is [`Chunk::len`] `== 0`, i.e. EOF.
/// 2. **To [`FS_MAX_IO`].** One request moves at most one block, because that is
///    the staging buffer a server on a one-page stack can afford. This is a
///    **short read, not `EINVAL`** — the deliberate departure from `BDEV_READ`,
///    whose client is a filesystem that cannot interpret half a block, towards
///    `CDEV_WRITE`, whose client is VFS and whose job is hiding staging from
///    POSIX. A read is short at EOF regardless, so a client that could not cope
///    with a short read could not use this request at all.
/// 3. **To the end of the block containing `pos`.** The invariant that makes one
///    block buffer sufficient: a chunk never straddles two blocks.
/// 4. `len < 0` → `EINVAL`, before any of the above. Left unchecked it would
///    widen into a ~16 EiB `u64` byte count on the safecopy.
///
/// `bs == 0` is `EINVAL` rather than a division by zero — it cannot happen (the
/// superblock validator rejects it) but this function is total.
pub fn clamp_read(size: i32, pos: u64, len: i32, bs: usize) -> Result<Chunk, i32> {
    if size < 0 {
        return Err(EIO);
    }
    if len < 0 || bs == 0 {
        return Err(EINVAL);
    }
    let size = size as u64;
    let off_in_block = (pos % bs as u64) as usize;
    if pos >= size {
        // EOF. `off_in_block` is still reported so the value is total, but a
        // zero-length chunk names no bytes and the caller fetches no block.
        return Ok(Chunk {
            len: 0,
            off_in_block,
        });
    }

    let remaining = size - pos;
    let to_block_end = bs - off_in_block;
    // Each term is already bounded by a `usize` quantity, so the `as usize` on
    // `remaining` cannot truncate on any target this runs on — but take the
    // minimum in `u64` first so it is true by construction rather than by
    // inspection.
    let len = (len as u64)
        .min(remaining)
        .min(FS_MAX_IO as u64)
        .min(to_block_end as u64) as usize;
    Ok(Chunk { len, off_in_block })
}

/// May zone `zone` be read from a device holding `blocks` blocks?
///
/// Zone 0 is never a data zone in MinixFS (it is the boot block), so a zero here
/// means a hole reached this function by mistake rather than being handled as one
/// — reject it. The upper bound is what stops a corrupt inode turning into a
/// `BDEV_READ` past the end of the device, which the driver would refuse anyway;
/// checking here makes the refusal an `EIO` about the *filesystem* rather than an
/// `EINVAL` about the device.
///
/// **The reader is deliberately looser than the writer**, and the asymmetry is a
/// decision rather than an oversight: there is no lower bound here, so a corrupt
/// inode naming a metadata zone reads a metadata block back as file data — wrong
/// bytes, and nothing worse. The same pointer on the write path would store over
/// the bitmaps or the inode table, so [`crate::write::write_zone_ok`] adds the
/// `zone >= first_data_zone` bound that this one does not need.
pub fn zone_ok(zone: u32, blocks: u32) -> bool {
    zone != 0 && zone < blocks
}

/// Split an absolute path into its parent directory and its final component.
///
/// `/etc/new` splits into `("/etc", "new")`, and `/new` into `("/", "new")` — the
/// parent is `"/"` rather than `""`, so it resolves through the ordinary walk.
///
/// The create path is the only caller, and it applies [`parse_path`] first, so
/// the length rules there are already enforced. What is left is what a *create*
/// needs on top of a lookup:
///
///   * `"/"` is `EISDIR`. It names the root directory, and a create whose target
///     is a directory gets the same errno `FS_TRUNC` and `VFS_OPEN` use for one.
///   * An empty final component (a trailing slash) is `EINVAL` — it names
///     nothing. So are `.` and `..`, which every directory already carries: a
///     create there would insert a duplicate of an entry that exists.
///   * A final component past [`crate::dirent::NAME_MAX`] is `ENAMETOOLONG`; it
///     could not be written into a directory entry at all.
pub fn split_basename(path: &str) -> Result<(&str, &str), i32> {
    if path == "/" {
        return Err(EISDIR);
    }
    let cut = path.rfind('/').ok_or(EINVAL)?;
    // `checked_add`, not `+`: this crate ships with `overflow-checks = false`.
    let name = path
        .get(cut.checked_add(1).ok_or(EINVAL)?..)
        .ok_or(EINVAL)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(EINVAL);
    }
    if name.len() > NAME_MAX {
        return Err(ENAMETOOLONG);
    }
    let parent = if cut == 0 {
        "/"
    } else {
        path.get(..cut).ok_or(EINVAL)?
    };
    Ok((parent, name))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::dirent::DirEntry;
    use crate::inode::ROOT_INODE;
    use std::format;

    const BS: usize = MFS_BLOCK_SIZE;

    #[test]
    fn an_absolute_path_passes_through() {
        for p in ["/", "/etc", "/etc/motd", "/bin/hello"] {
            assert_eq!(parse_path(p), Ok(p), "{p}");
        }
    }

    #[test]
    fn a_relative_or_empty_path_is_einval() {
        // There is no working directory on minix.rs, so a relative path is a
        // malformed request rather than a missing file.
        for p in ["", "etc/motd", ".", "..", "motd"] {
            assert_eq!(parse_path(p), Err(EINVAL), "{p}");
        }
    }

    #[test]
    fn a_component_longer_than_the_format_allows_is_enametoolong() {
        // MinixFS cannot store a name this long, so no image can contain one:
        // `/` followed by NAME_MAX + 1 letters.
        let mut buf = [b'a'; NAME_MAX + 2];
        buf[0] = b'/';
        let too_long = core::str::from_utf8(&buf).unwrap();
        assert_eq!(too_long.len(), NAME_MAX + 2);
        assert_eq!(parse_path(too_long), Err(ENAMETOOLONG));

        // Exactly NAME_MAX is fine — the boundary itself, not one inside it.
        let ok = core::str::from_utf8(&buf[..NAME_MAX + 1]).unwrap();
        assert_eq!(parse_path(ok), Ok(ok));
    }

    #[test]
    fn components_collapse_repeated_and_trailing_slashes() {
        let mut seen = [""; 4];
        let mut n = 0;
        for c in components("/etc//motd/") {
            seen[n] = c;
            n += 1;
        }
        assert_eq!(n, 2);
        assert_eq!(seen[0], "etc");
        assert_eq!(seen[1], "motd");
    }

    #[test]
    fn the_root_path_has_no_components() {
        // So `/` resolves to the root inode without a single directory read.
        assert_eq!(components("/").count(), 0);
        assert_eq!(components("///").count(), 0);
    }

    /// One directory block holding `.`, `..`, and two named entries.
    fn dir_block() -> [u8; BS] {
        let mut b = [0u8; BS];
        for (slot, (ino, name)) in [
            (ROOT_INODE, &b"."[..]),
            (ROOT_INODE, &b".."[..]),
            (5u32, &b"motd"[..]),
            (9u32, &b"pattern"[..]),
        ]
        .into_iter()
        .enumerate()
        {
            let e = DirEntry::new(ino, name).unwrap();
            b[slot * 64..(slot + 1) * 64].copy_from_slice(&e.to_le_bytes());
        }
        b
    }

    #[test]
    fn find_in_block_returns_the_named_entry() {
        let b = dir_block();
        assert_eq!(find_in_block(&b, "motd"), Some(5));
        assert_eq!(find_in_block(&b, "pattern"), Some(9));
        assert_eq!(find_in_block(&b, "."), Some(ROOT_INODE));
        assert_eq!(find_in_block(&b, ".."), Some(ROOT_INODE));
    }

    #[test]
    fn find_in_block_does_not_match_a_prefix_or_a_free_slot() {
        // A prefix match would resolve `/etc/mot` to `motd`; a free slot decodes
        // to an empty name, which must not match an empty query either (though
        // `parse_path`'s components are never empty).
        let b = dir_block();
        assert_eq!(find_in_block(&b, "mot"), None);
        assert_eq!(find_in_block(&b, "motdd"), None);
        assert_eq!(find_in_block(&b, ""), None);
        assert_eq!(find_in_block(&[0u8; BS], "motd"), None);
    }

    #[test]
    fn find_in_block_ignores_a_trailing_partial_entry() {
        // A short block must not synthesize an entry out of whatever followed it.
        let b = dir_block();
        assert_eq!(find_in_block(&b[..64 * 3 + 40], "pattern"), None);
        assert_eq!(find_in_block(&b[..64 * 3 + 40], "motd"), Some(5));
        assert_eq!(find_in_block(&[], "motd"), None);
    }

    #[test]
    fn a_directory_size_is_bounded() {
        assert_eq!(dir_size(0), Ok(0));
        assert_eq!(dir_size(BS as i32), Ok(BS));
        assert_eq!(dir_size(MAX_DIR_BYTES as i32), Ok(MAX_DIR_BYTES));
    }

    #[test]
    fn a_corrupt_directory_size_is_eio_rather_than_an_unbounded_scan() {
        // The bound that stops a corrupt inode spinning the server forever. Both
        // ends: a negative size (which would widen into ~2 GiB as a `usize`) and
        // one past what this server will scan.
        assert_eq!(dir_size(-1), Err(EIO));
        assert_eq!(dir_size(i32::MIN), Err(EIO));
        assert_eq!(dir_size(MAX_DIR_BYTES as i32 + 1), Err(EIO));
        assert_eq!(dir_size(i32::MAX), Err(EIO));
    }

    #[test]
    fn a_one_block_directory_is_one_round() {
        // The shape every directory `mkfs-mfs` builds starts in: one block, and
        // then the scan is over. `/full` leaves it at runtime; see the multi-block
        // test below.
        assert_eq!(next_dir_chunk(0, 128, BS), Some(128));
        assert_eq!(next_dir_chunk(128, 128, BS), None);
        // An empty directory is no rounds at all.
        assert_eq!(next_dir_chunk(0, 0, BS), None);
    }

    #[test]
    fn driving_next_dir_chunk_over_a_three_block_directory_converges_on_the_size() {
        // A boot reaches the second iteration as of slice 5.10b — `/full` grows to
        // a second block at runtime and the `fs.dirgrow` probe walks it — but not
        // the third, and not this exact-landing case. Each round is a whole block
        // and the third lands exactly on `size`, which is what a `<=`/`<` slip in
        // the termination test would turn into a fourth round scanning bytes that
        // are not there.
        let size = BS * 3;
        let mut off = 0usize;
        let mut rounds = 0;
        while let Some(want) = next_dir_chunk(off, size, BS) {
            rounds += 1;
            assert!(rounds <= 8, "the directory scan failed to converge");
            assert_eq!(want, BS);
            off += want;
        }
        assert_eq!(rounds, 3);
        assert_eq!(off, size, "the scan must stop exactly on the size");
    }

    #[test]
    fn a_directory_whose_size_is_not_a_whole_number_of_blocks_ends_on_a_short_round() {
        // The final round is the remainder, never a whole block: reading a whole
        // one would hand `find_in_block` bytes past the directory's end, which is
        // where a stale entry from a shrunken directory would live.
        let size = BS * 2 + 37;
        let mut off = 0usize;
        let mut seen = [0usize; 4];
        let mut rounds = 0;
        while let Some(want) = next_dir_chunk(off, size, BS) {
            assert!(rounds < seen.len(), "the directory scan failed to converge");
            seen[rounds] = want;
            rounds += 1;
            off += want;
        }
        assert_eq!(rounds, 3);
        assert_eq!(seen[..3], [BS, BS, 37]);
        assert_eq!(off, size);
    }

    #[test]
    fn a_zero_block_size_ends_the_scan_rather_than_spinning() {
        // Unreachable — the superblock validator rejects it — but a zero-length
        // round would advance `off` by nothing forever, which is the one failure
        // mode this module exists to prevent.
        assert_eq!(next_dir_chunk(0, MAX_DIR_BYTES, 0), None);
        // ...as does an offset already past the end, however far past.
        assert_eq!(next_dir_chunk(usize::MAX, MAX_DIR_BYTES, BS), None);
    }

    #[test]
    fn a_whole_small_file_reads_in_one_chunk() {
        assert_eq!(
            clamp_read(31, 0, 4096, BS),
            Ok(Chunk {
                len: 31,
                off_in_block: 0
            })
        );
    }

    #[test]
    fn a_read_never_straddles_a_block_boundary() {
        // The invariant that makes one block buffer sufficient. Asking for 4096
        // bytes from 100 bytes into a block yields the rest of that block only.
        let c = clamp_read(BS as i32 * 4, 100, 4096, BS).unwrap();
        assert_eq!(c.off_in_block, 100);
        assert_eq!(c.len, BS - 100);
        assert_eq!(c.off_in_block + c.len, BS);

        // ...and driving it round the loop lands exactly on the boundary.
        let c = clamp_read(BS as i32 * 4, BS as u64 - 1, 4096, BS).unwrap();
        assert_eq!(c.len, 1);
        assert_eq!(c.off_in_block, BS - 1);
    }

    #[test]
    fn a_read_past_the_end_of_the_file_is_zero_not_an_error() {
        // EOF is `len == 0`, the only way a client learns where a file ends.
        for pos in [31u64, 32, 4096, u64::MAX] {
            let c = clamp_read(31, pos, 4096, BS).unwrap();
            assert_eq!(c.len, 0, "pos {pos}");
        }
        // An empty file is EOF at offset 0.
        assert_eq!(clamp_read(0, 0, 4096, BS).unwrap().len, 0);
    }

    #[test]
    fn an_over_long_request_is_clamped_rather_than_refused() {
        // The deliberate departure from BDEV_READ, whose answer here is EINVAL.
        let big = BS as i32 * 4;
        assert_eq!(clamp_read(big, 0, i32::MAX, BS).unwrap().len, FS_MAX_IO);
        assert_eq!(
            clamp_read(big, 0, FS_MAX_IO as i32 + 1, BS).unwrap().len,
            FS_MAX_IO
        );
    }

    #[test]
    fn a_negative_length_or_size_is_rejected() {
        // A negative length would widen into a ~16 EiB `u64` on the safecopy; a
        // negative size is a corrupt inode, which is a different failure.
        assert_eq!(clamp_read(100, 0, -1, BS), Err(EINVAL));
        assert_eq!(clamp_read(100, 0, i32::MIN, BS), Err(EINVAL));
        assert_eq!(clamp_read(-1, 0, 8, BS), Err(EIO));
        // The size check comes first: a request wrong in both ways reports the
        // one that is about the filesystem rather than the one about the caller.
        assert_eq!(clamp_read(-1, 0, -1, BS), Err(EIO));
    }

    #[test]
    fn a_zero_block_size_is_einval_rather_than_a_division_by_zero() {
        // Unreachable — the superblock validator rejects it — but this function
        // is total, so it has to say something.
        assert_eq!(clamp_read(100, 0, 8, 0), Err(EINVAL));
    }

    #[test]
    fn driving_clamp_read_round_a_whole_file_terminates_at_the_size() {
        // The property, over a file that spans several blocks and ends mid-block.
        let size = BS * 2 + 37;
        let mut pos = 0u64;
        let mut total = 0usize;
        let mut rounds = 0;
        loop {
            rounds += 1;
            assert!(rounds <= 8, "the read loop failed to converge");
            let c = clamp_read(size as i32, pos, i32::MAX, BS).unwrap();
            if c.len == 0 {
                break;
            }
            assert!(c.off_in_block + c.len <= BS, "chunk straddles a block");
            pos += c.len as u64;
            total += c.len;
        }
        assert_eq!(total, size);
        assert_eq!(rounds, 4);
    }

    #[test]
    fn a_zone_must_be_non_zero_and_inside_the_device() {
        assert!(zone_ok(1, 256));
        assert!(zone_ok(255, 256));
        // Zone 0 is the boot block: a hole that reached this call by mistake.
        assert!(!zone_ok(0, 256));
        // Past the end — what stops a corrupt inode becoming a BDEV_READ off the
        // end of the device.
        assert!(!zone_ok(256, 256));
        assert!(!zone_ok(u32::MAX, 256));
        // A device with no blocks admits nothing.
        assert!(!zone_ok(1, 0));
    }

    // ----- split_basename ----------------------------------------------------

    #[test]
    fn a_path_splits_into_its_parent_and_its_final_component() {
        assert_eq!(split_basename("/etc/new"), Ok(("/etc", "new")));
        assert_eq!(split_basename("/full/new"), Ok(("/full", "new")));
    }

    #[test]
    fn a_top_level_name_has_the_root_as_its_parent() {
        // The `cut == 0` case: the parent is "/", not "".
        assert_eq!(split_basename("/new"), Ok(("/", "new")));
    }

    #[test]
    fn the_root_itself_is_eisdir() {
        // It names a directory, and a create whose target is a directory gets the
        // same errno `FS_TRUNC` and `VFS_OPEN` use for one.
        assert_eq!(split_basename("/"), Err(EISDIR));
    }

    #[test]
    fn a_trailing_slash_or_a_dot_component_is_einval() {
        // An empty final component names nothing; `.` and `..` are entries every
        // directory already carries, so creating them would insert a duplicate.
        for p in ["/etc/", "/etc/.", "/etc/..", "/."] {
            assert_eq!(split_basename(p), Err(EINVAL), "{p:?}");
        }
    }

    #[test]
    fn a_relative_path_has_no_separator_and_is_einval() {
        // Unreachable through `parse_path`, which refuses it first -- but a
        // second caller must not be able to reach a `rfind` that returns `None`.
        assert_eq!(split_basename("etc"), Err(EINVAL));
        assert_eq!(split_basename(""), Err(EINVAL));
    }

    #[test]
    fn a_final_component_past_the_name_field_is_enametoolong() {
        // It could not be written into a directory entry. Exactly NAME_MAX is
        // fine, because the name field is NUL-padded rather than terminated.
        let long = "x".repeat(NAME_MAX + 1);
        assert_eq!(split_basename(&format!("/etc/{long}")), Err(ENAMETOOLONG));
        let ok = "x".repeat(NAME_MAX);
        let path = format!("/etc/{ok}");
        assert_eq!(split_basename(&path), Ok(("/etc", ok.as_str())));
    }
}

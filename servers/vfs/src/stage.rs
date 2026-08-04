// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `VFS_EXEC_STAGE` request parsing, validation, and the size rules that decide
//! whether a file can be staged at all (slice 5.9).
//!
//! A sibling of `rw.rs` and `open.rs` for the reason those two are siblings of
//! each other: almost nothing is shared. `open` takes a path *address* and a
//! length, this takes a path **inline**; `open` hands back a descriptor, this
//! hands back a whole file. What the three do share is the split — every total
//! function over plain values lives in a module like this one, where it carries
//! the crate's unit tests, while the IPC round trip stays in `main.rs`, which is
//! Sonar-coverage-excluded.
//!
//! ## Why the path is inline
//!
//! `VFS_EXEC_STAGE`'s client is PM, serving a `PM_EXEC` whose path it already
//! holds inline. Passing it by value costs no `SYS_COPY` — and it removes the
//! confused-deputy question entirely, because there is no source process for a
//! caller to misname. `VFS_OPEN` cannot do that: its client is an ordinary user
//! process whose path lives in its own address space, so VFS has to go and fetch
//! it. Two clients, two shapes.
//!
//! ## What is *not* here
//!
//! The caller check. `main.rs` refuses any `m_source` but PM's, and that has to
//! live where the kernel-stamped source is — a function taking an endpoint as an
//! argument would be a function whose test proves the comparison and not the
//! provenance.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{FS_PATH_MAX, VFS_EXEC_MAX, VFS_EXEC_PATH_OFF};
use minixrs_kernel_shared::error::{EINVAL, ENAMETOOLONG, ENOEXEC, ENOMEM};

/// Read the inline path field out of a `VFS_EXEC_STAGE` payload, or say why it is
/// not a path.
///
/// The field is NUL-**padded**, so the rules are the FS band's, verbatim:
///
/// 1. **No NUL anywhere** → `ENAMETOOLONG`. Never a silent truncation, which
///    could resolve to a different file — and for an *executable* that is the
///    difference between running one program and another.
/// 2. **Empty** → `EINVAL`. There is no file it could name.
/// 3. **Not absolute** → `EINVAL`. minix.rs has no working directory, so a
///    relative path is malformed rather than missing. This is also the check that
///    makes `PM_EXEC`'s leading-`/` discriminator honest: everything reaching
///    here has already been routed *because* it starts with `/`, and a second
///    opinion at the far end costs one comparison.
/// 4. Non-UTF-8 → `EINVAL`, since the path travels on to `FS_LOOKUP` as text.
pub fn parse_path(msg: &Message) -> Result<&str, i32> {
    let field = msg
        .payload
        .get(VFS_EXEC_PATH_OFF..VFS_EXEC_PATH_OFF + FS_PATH_MAX)
        .ok_or(EINVAL)?;
    let Some(end) = field.iter().position(|&b| b == 0) else {
        return Err(ENAMETOOLONG);
    };
    let path = core::str::from_utf8(&field[..end]).map_err(|_| EINVAL)?;
    if path.is_empty() || !path.starts_with('/') {
        return Err(EINVAL);
    }
    Ok(path)
}

/// Turn an `FS_LOOKUP`'s reported size into the number of bytes to stage.
///
/// Three rules:
///
/// 1. **Negative** → `EINVAL`. The size arrives as an i32 widened from the
///    inode's own field, so a corrupt inode is a value this must refuse rather
///    than a `usize` cast that wraps into something enormous.
/// 2. **Zero** → `ENOEXEC`, not `EINVAL`. The request was well-formed and the
///    *file* is the problem, which is the distinction the loader draws one hop
///    later for a file that is not an ELF — and answering here means it is never
///    handed a zero-length grant to reject with something less useful.
/// 3. **Past [`VFS_EXEC_MAX`]** → `ENOMEM`. The staging buffer is a fixed `.bss`
///    array, so this is a real resource limit rather than a malformed request —
///    and `ENOMEM` is what POSIX `execve` reports when the image will not fit.
pub fn stage_len(size: i32) -> Result<usize, i32> {
    if size < 0 {
        return Err(EINVAL);
    }
    let size = size as usize;
    if size == 0 {
        return Err(ENOEXEC);
    }
    if size > VFS_EXEC_MAX {
        return Err(ENOMEM);
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &[u8]) -> Message {
        let mut m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        // The payload starts zeroed, so writing the bytes *is* NUL-padding it —
        // exactly what `main.rs`'s marshaller does.
        m.payload[VFS_EXEC_PATH_OFF..VFS_EXEC_PATH_OFF + path.len()].copy_from_slice(path);
        m
    }

    #[test]
    fn an_absolute_path_reads_back_verbatim() {
        assert_eq!(parse_path(&request(b"/bin/hello")), Ok("/bin/hello"));
        assert_eq!(parse_path(&request(b"/x")), Ok("/x"));
    }

    #[test]
    fn the_longest_path_that_fits_is_accepted() {
        // The field is NUL-padded, so the longest path leaves room for the
        // terminator — `FS_PATH_MAX - 1` bytes, the same boundary `open` applies
        // one layer up.
        let mut p = [b'a'; FS_PATH_MAX - 1];
        p[0] = b'/';
        let m = request(&p);
        assert_eq!(parse_path(&m).map(str::len), Ok(FS_PATH_MAX - 1));
    }

    #[test]
    fn a_field_with_no_nul_is_enametoolong() {
        // Never a truncation: for an executable, a silently shortened path is
        // the difference between running one program and another.
        assert_eq!(
            parse_path(&request(&[b'/'; FS_PATH_MAX])),
            Err(ENAMETOOLONG)
        );
    }

    #[test]
    fn an_empty_field_is_einval() {
        // A zeroed payload — a client that sent nothing.
        assert_eq!(parse_path(&request(b"")), Err(EINVAL));
    }

    #[test]
    fn a_relative_path_is_einval() {
        // There is no working directory on minix.rs, so this is malformed rather
        // than missing. It is also the second opinion on `PM_EXEC`'s leading-`/`
        // discriminator: nothing relative should have been routed here at all.
        for p in [&b"bin/hello"[..], b"hello", b".", b"./hello"] {
            assert_eq!(parse_path(&request(p)), Err(EINVAL), "path {p:?}");
        }
    }

    #[test]
    fn a_non_utf8_path_is_einval() {
        assert_eq!(parse_path(&request(b"/\xff\xfe")), Err(EINVAL));
    }

    #[test]
    fn an_ordinary_size_stages_whole() {
        for n in [1i32, 31, 4096, 200_152, VFS_EXEC_MAX as i32] {
            assert_eq!(stage_len(n), Ok(n as usize), "size {n}");
        }
    }

    #[test]
    fn an_empty_file_is_not_an_executable() {
        // ENOEXEC rather than EINVAL: the request was well-formed and the *file*
        // is the problem, which is the same distinction the loader draws one hop
        // later for a file that is not an ELF.
        assert_eq!(stage_len(0), Err(ENOEXEC));
    }

    #[test]
    fn a_negative_size_is_einval() {
        // The size is an i32 widened from the inode's own field, so a corrupt
        // inode must be refused here rather than cast into an enormous `usize`.
        for n in [-1i32, -4096, i32::MIN] {
            assert_eq!(stage_len(n), Err(EINVAL), "size {n}");
        }
    }

    #[test]
    fn a_file_past_the_cap_is_enomem() {
        // A real resource limit — the staging buffer is a fixed array — not a
        // malformed request, and `ENOMEM` is what POSIX `execve` reports.
        assert_eq!(stage_len(VFS_EXEC_MAX as i32 + 1), Err(ENOMEM));
        assert_eq!(stage_len(i32::MAX), Err(ENOMEM));
    }
}

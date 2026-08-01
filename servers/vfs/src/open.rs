// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `VFS_OPEN` request parsing, validation, and the classification of what
//! `FS_LOOKUP` came back with (slice 5.8).
//!
//! A sibling of `rw.rs` rather than a part of it, because almost nothing is
//! shared: `open` takes a *path* where read and write take a descriptor, and its
//! whole job is turning what the filesystem said about an inode into either a
//! descriptor or an errno. Everything here is a total function over plain values,
//! so it carries the crate's unit tests while the IPC round trip lives in
//! `main.rs` — the `drivers/tty` `cdev.rs` / `main.rs` split.
//!
//! ## The path arrives as an address, and VFS reads it itself
//!
//! `VFS_OPEN`'s client is an ordinary user process with no grant table, so the
//! payload carries a raw buffer address. VFS copies the bytes out with `SYS_COPY`
//! — **the first live consumer of decision D4's "`SYS_COPY` for control plane"
//! sentence**, and the confused-deputy rule in its sharpest form: `SYS_COPY` has
//! *no per-target authorization at all*, so a payload-supplied source process
//! would let any client read any process's memory through VFS. The source is the
//! kernel-stamped `m_source`, and there is no field for it.
//!
//! That copy is `main.rs`'s business. What lives here is deciding whether the
//! request is worth making it for ([`validate`]) and what to do with the answer
//! ([`classify`]).

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{FS_PATH_MAX, VFS_PATH_LEN_OFF, VFS_PATH_OFF};
use minixrs_kernel_shared::error::{EFAULT, EINVAL, EISDIR, ENAMETOOLONG};
use minixrs_kernel_shared::message::user_range_ok;
use minixrs_server_rt::{rd_i32, rd_u64};

use crate::fd::Fd;

/// A parsed `VFS_OPEN` request. Field-for-field the payload, with no
/// interpretation applied.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct OpenRequest {
    /// The path buffer's address **in the caller's own address space**.
    pub path: u64,
    /// Its length in bytes, not counting any terminator the caller may have.
    pub len: i32,
}

/// Read a `VFS_OPEN` request out of a message payload.
///
/// Total: both fields are fixed-offset scalar reads that cannot fail, so a
/// malformed request becomes an invalid *value* the checks reject, never a panic.
pub fn parse(msg: &Message) -> OpenRequest {
    OpenRequest {
        path: rd_u64(msg, VFS_PATH_OFF),
        len: rd_i32(msg, VFS_PATH_LEN_OFF),
    }
}

/// Decide how many path bytes to copy in, or which errno to reply.
///
/// The checks, in order — each is the first thing that can be wrong given the
/// ones before it:
///
/// 1. `len <= 0` → `EINVAL`. Unlike a read or a write, an **empty path is not a
///    legal no-op**: there is no file it could name. Negative is the same class of
///    bug it is everywhere else — left unchecked it would widen into a ~16 EiB
///    `u64` on the copy.
/// 2. `len >= FS_PATH_MAX` → `ENAMETOOLONG`, **refused before the wire**. The FS
///    band carries the path inline in a fixed, NUL-padded field, so the longest
///    path that can travel is one byte shorter than the field — and catching it
///    here means the client hears a specific errno without a round trip, rather
///    than a truncated path resolving to some other file.
/// 3. The buffer lies in the user address range → else `EFAULT`. `user_range_ok`,
///    the no-alignment variant, since a path is a byte buffer. Defence in depth
///    rather than the load-bearing gate: the kernel's copy engine walks the
///    caller's page tables and answers `EFAULT` regardless (D5).
pub fn validate(len: i32, path: u64) -> Result<usize, i32> {
    if len <= 0 {
        return Err(EINVAL);
    }
    let len = len as usize;
    if len >= FS_PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    if !user_range_ok(path, len as u64) {
        return Err(EFAULT);
    }
    Ok(len)
}

/// Turn an `FS_LOOKUP` reply's mode into the descriptor entry to install, or the
/// errno to reply.
///
/// Two answers, and the second is the one worth having:
///
/// * A **regular file** becomes an [`Fd::File`] at position 0.
/// * A **directory** is `EISDIR`. Not `EINVAL`, and not silently opened: there is
///   no `O_DIRECTORY` and nothing that could read one (no `GETDENTS`), so a
///   descriptor onto a directory would be a descriptor every subsequent `read`
///   had to refuse. Saying so at `open` is both the POSIX-shaped answer and the
///   one that keeps `read`'s own `EISDIR` arm from being the only line of defence.
/// * Anything else — a device node, a symlink, a mode this build does not
///   recognize — is `EINVAL`. `mkfs-mfs` writes none of them, so this arm exists
///   to make an unrecognized mode a refusal rather than a file.
///
/// The mode arrives as an i32 widened from MinixFS's `u16`, so it is masked back
/// down before the type bits are read.
pub fn classify(mode: i32) -> Result<Fd, i32> {
    const I_TYPE: u16 = 0o170000;
    const I_REGULAR: u16 = 0o100000;
    const I_DIRECTORY: u16 = 0o040000;

    match (mode as u16) & I_TYPE {
        I_REGULAR => Ok(Fd::File { ino: 0, pos: 0 }),
        I_DIRECTORY => Err(EISDIR),
        _ => Err(EINVAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::message::USER_VA_TOP;
    use minixrs_server_rt::{wr_i32, wr_u64};

    /// A plausible user buffer: non-zero and well inside the user range.
    const BUF: u64 = 0x0010_0000;

    fn request(path: u64, len: i32) -> Message {
        let mut m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        wr_u64(&mut m, VFS_PATH_OFF, path);
        wr_i32(&mut m, VFS_PATH_LEN_OFF, len);
        m
    }

    #[test]
    fn parse_reads_both_fields_from_their_own_offsets() {
        // Two distinct values, so a swapped pair of offsets would fail.
        assert_eq!(parse(&request(BUF, 9)), OpenRequest { path: BUF, len: 9 });
    }

    #[test]
    fn parse_of_a_zeroed_payload_yields_a_rejectable_request() {
        // A client that sent nothing: length 0 is refused without the (null)
        // buffer ever being looked at.
        let req = parse(&Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        });
        assert_eq!(req.len, 0);
        assert_eq!(validate(req.len, req.path), Err(EINVAL));
    }

    #[test]
    fn a_normal_path_length_passes_through() {
        for len in [1i32, 9, 12, FS_PATH_MAX as i32 - 1] {
            assert_eq!(validate(len, BUF), Ok(len as usize), "len {len}");
        }
    }

    #[test]
    fn an_empty_path_is_einval_not_a_legal_no_op() {
        // The one place `open` differs from `read`/`write` on length: zero bytes
        // is a legal empty transfer there, and names no file here.
        assert_eq!(validate(0, BUF), Err(EINVAL));
        assert_eq!(validate(0, 0), Err(EINVAL));
    }

    #[test]
    fn a_negative_path_length_is_einval() {
        // Left unchecked it would widen into a ~16 EiB `u64` on the copy.
        for len in [-1i32, -64, i32::MIN] {
            assert_eq!(validate(len, BUF), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn a_path_past_the_wire_field_is_enametoolong_before_the_wire() {
        // The FS band carries the path inline in a fixed, NUL-padded field; this
        // is the cap that makes that viable, and refusing here is what stops a
        // truncated path resolving to a different file.
        //
        // The boundary is `FS_PATH_MAX - 1`, not `FS_PATH_MAX`: a path that
        // filled the field would leave no room for the terminator, and MFS would
        // answer `ENAMETOOLONG` a round trip later. Refusing both here means the
        // two layers cannot disagree about where the limit is.
        assert_eq!(validate(FS_PATH_MAX as i32, BUF), Err(ENAMETOOLONG));
        assert_eq!(validate(FS_PATH_MAX as i32 + 1, BUF), Err(ENAMETOOLONG));
        assert_eq!(validate(i32::MAX, BUF), Err(ENAMETOOLONG));
        assert_eq!(
            validate(FS_PATH_MAX as i32 - 1, BUF),
            Ok(FS_PATH_MAX - 1),
            "the longest path that leaves room for the NUL"
        );
    }

    #[test]
    fn a_path_buffer_outside_the_user_range_is_efault() {
        // NULL, a range that overflows, and one that runs past the top of the
        // user address space.
        assert_eq!(validate(9, 0), Err(EFAULT));
        assert_eq!(validate(9, u64::MAX), Err(EFAULT));
        assert_eq!(validate(9, USER_VA_TOP), Err(EFAULT));
    }

    #[test]
    fn the_length_checks_precede_the_buffer_check() {
        // A request wrong in two ways reports the *first* problem, so a client
        // fixing one learns about the next.
        assert_eq!(validate(-1, 0), Err(EINVAL));
        assert_eq!(validate(i32::MAX, 0), Err(ENAMETOOLONG));
    }

    #[test]
    fn a_regular_file_becomes_a_descriptor_at_position_zero() {
        // The inode is filled in by the caller, which is the one that knows it;
        // what this decides is the *kind* of descriptor and where it starts.
        assert_eq!(classify(0o100644), Ok(Fd::File { ino: 0, pos: 0 }));
        assert_eq!(classify(0o100755), Ok(Fd::File { ino: 0, pos: 0 }));
    }

    #[test]
    fn a_directory_is_eisdir() {
        // Not EINVAL, and not silently opened: there is nothing that could read a
        // directory, so a descriptor onto one would be refused by every
        // subsequent `read`. Delete this arm and `open("/etc")` succeeds.
        assert_eq!(classify(0o040755), Err(EISDIR));
        assert_eq!(classify(0o040000), Err(EISDIR));
    }

    #[test]
    fn an_unrecognized_mode_is_einval_rather_than_a_file() {
        // mkfs-mfs writes none of these, so the arm exists to make an
        // unrecognized mode a refusal rather than something that reads as data.
        for mode in [
            0o020000, // character device
            0o060000, // block device
            0o010000, // FIFO
            0o120000, // symlink
            0o140000, // socket
            0,        // a zeroed inode
        ] {
            assert_eq!(classify(mode), Err(EINVAL), "mode {mode:o}");
        }
    }

    #[test]
    fn only_the_type_bits_decide() {
        // The permission bits must not change the answer, and the widening from
        // MinixFS's u16 must not let a high bit leak in.
        for perm in [0, 0o444, 0o777] {
            assert_eq!(
                classify(0o100000 | perm),
                Ok(Fd::File { ino: 0, pos: 0 }),
                "perm {perm:o}"
            );
            assert_eq!(classify(0o040000 | perm), Err(EISDIR), "perm {perm:o}");
        }
        // A value with rubbish above bit 16 still classifies on the low bits,
        // because the reply field is a widened u16 and nothing above it is ours.
        assert_eq!(
            classify(0x7fff_0000u32 as i32 | 0o100644),
            Ok(Fd::File { ino: 0, pos: 0 })
        );
    }
}

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The FS band on the wire: parsing requests and building replies (slice 5.8).
//!
//! Everything here is a total function over a [`Message`], so it carries unit
//! tests while the IPC round trip lives in `main.rs` — the `servers/ds`
//! `registry.rs` / `main.rs` split, and the same reason: `main.rs` is behind
//! `required-features = ["server"]` and therefore invisible to every CI job, so
//! anything with a decision in it belongs here.
//!
//! ## Why this crate has its own payload accessors
//!
//! `server-rt`'s `rd_i32` / `rd_u64` are the same six lines, but `server-rt` is an
//! **optional** dependency of this crate (it comes in with the `server` feature),
//! and this module is not. Re-deriving them costs a dozen lines and buys the test
//! that catches a swapped payload offset — the trade `userland/init` already
//! makes for exactly the same reason.
//!
//! ## The path is inline, and that is the interesting part
//!
//! [`parse_lookup`] reads the path out of a **fixed, NUL-padded** field rather
//! than through a grant. It is control plane rather than the data path grants
//! were provisioned for, it costs this server no staging buffer (its scarcest
//! resource — one 4 KiB block buffer is already the whole stack), and it deletes
//! the confused-deputy question outright, because a request with no granter
//! cannot aim one anywhere. The cost is the cap, and it is paid here: a path that
//! fills its field with no NUL is `ENAMETOOLONG`, never a truncated path that
//! resolves to some other file.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    FS_GRANT_OFF, FS_INO_OFF, FS_LEN_OFF, FS_MODE_OFF, FS_PATH_MAX, FS_PATH_OFF, FS_POS_OFF,
    FS_SIZE_OFF, FS_SUPER_BLOCK_SIZE_OFF, FS_SUPER_BLOCKS_OFF, FS_SUPER_MINOR_OFF,
    FS_SUPER_ROOT_OFF,
};
use minixrs_kernel_shared::error::{EINVAL, ENAMETOOLONG};

/// A parsed [`FS_READ`](minixrs_kernel_shared::callnr::FS_READ) **or**
/// [`FS_WRITE`](minixrs_kernel_shared::callnr::FS_WRITE) request.
///
/// One struct for both, because the two payloads are the same fields in the same
/// places — the same question asked in the other direction (slice 5.10a, W1).
/// Nothing here says which direction it is: the request number does, and the
/// server's dispatch is the only place that needs to know.
///
/// Field-for-field the payload, with no interpretation applied — the checks live
/// in [`crate::walk::clamp_read`] / [`crate::write::clamp_write`] and in the
/// server's inode lookup. There is deliberately **no granter field**: the server
/// takes the granter from the kernel-stamped `m_source`, so this struct has no
/// way to express one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ReadRequest {
    /// The inode to transfer, as handed out by a previous `FS_LOOKUP`.
    pub ino: i32,
    /// Grant id over the client's buffer, naming this server as grantee. The
    /// access bit it must carry is the one the *direction* needs — `CPF_WRITE`
    /// for an `FS_READ`, whose copy lands in the client's buffer, `CPF_READ` for
    /// an `FS_WRITE`, whose copy is taken out of it. The kernel checks grantee
    /// and access; no server re-implements either.
    pub gid: i32,
    /// Bytes asked for. Clamped, not refused, in both directions — see
    /// [`crate::walk::clamp_read`] and [`crate::write::clamp_write`].
    pub len: i32,
    /// Byte offset within the file.
    pub pos: u64,
}

/// Read the block-device minor out of an `FS_READSUPER` request.
pub fn parse_readsuper(msg: &Message) -> i32 {
    rd_i32(msg, FS_SUPER_MINOR_OFF)
}

/// Read the path out of an `FS_LOOKUP` request.
///
/// The field is [`FS_PATH_MAX`] bytes, NUL-**padded**: the server reads the whole
/// fixed width and stops at the first NUL, so bytes past the terminator cannot
/// leak between requests.
///
/// Three ways it can fail, and each is a different thing to tell a client:
///
///   * no NUL anywhere in the field → `ENAMETOOLONG`. Never a silently truncated
///     path, which could resolve to a different file than the one asked for.
///   * not valid UTF-8 → `EINVAL`. minix.rs paths are UTF-8 by construction (they
///     come from a Rust `&str` at the other end), so this is a malformed request.
///   * an empty path → `EINVAL`, via [`crate::walk::parse_path`], which the caller
///     applies next. Left here it would be indistinguishable from "the client sent
///     a zeroed payload", which it is.
pub fn parse_lookup(msg: &Message) -> Result<&str, i32> {
    let field = &msg.payload[FS_PATH_OFF..FS_PATH_OFF + FS_PATH_MAX];
    let end = field.iter().position(|&b| b == 0).ok_or(ENAMETOOLONG)?;
    core::str::from_utf8(&field[..end]).map_err(|_| EINVAL)
}

/// Read an `FS_READ` — or `FS_WRITE`, which is the same payload — request out of
/// a message payload.
///
/// Total: every field is a fixed-offset scalar read that cannot fail (an
/// out-of-range offset reads as `0`), so a malformed request becomes an invalid
/// *value* the checks reject, never a panic.
pub fn parse_read(msg: &Message) -> ReadRequest {
    ReadRequest {
        ino: rd_i32(msg, FS_INO_OFF),
        gid: rd_i32(msg, FS_GRANT_OFF),
        len: rd_i32(msg, FS_LEN_OFF),
        pos: rd_u64(msg, FS_POS_OFF),
    }
}

/// Build the payload of a successful `FS_READSUPER` reply.
///
/// The caller stamps `m_type` (`OK`). These three are what make the reply worth
/// more than its `m_type`: a boot marker can cross-check them against the
/// *device*'s independently derived geometry.
pub fn reply_readsuper(msg: &mut Message, root: u32, block_size: usize, blocks: u32) {
    msg.payload = [0u8; 96];
    wr_i32(msg, FS_SUPER_ROOT_OFF, root as i32);
    wr_i32(msg, FS_SUPER_BLOCK_SIZE_OFF, block_size as i32);
    wr_i32(msg, FS_SUPER_BLOCKS_OFF, blocks as i32);
}

/// Build the payload of a successful `FS_LOOKUP` reply.
///
/// `mode` is widened from MinixFS's `u16` so the wire field is an ordinary i32
/// like every other; the type bits a client cares about (`S_IFDIR`, `S_IFREG`)
/// are well inside 16 bits, so nothing is lost. `size` is already the inode's own
/// signed size.
pub fn reply_lookup(msg: &mut Message, ino: u32, mode: u16, size: i32) {
    msg.payload = [0u8; 96];
    wr_i32(msg, FS_INO_OFF, ino as i32);
    wr_i32(msg, FS_MODE_OFF, i32::from(mode));
    wr_i32(msg, FS_SIZE_OFF, size);
}

/// Read a native-endian `i32` from a payload offset. `0` past the end.
///
/// `checked_add`, not `+`: `[profile.release]` sets `overflow-checks = false`, so
/// `off + 4` on a huge offset *wraps* in the shipped binary — where it would be
/// safe only by accident, via a `None` from `get` — while panicking under
/// `cargo test`. Being explicit makes both profiles agree, which is the
/// workspace-wide rule for offset arithmetic in a server.
fn rd_i32(m: &Message, off: usize) -> i32 {
    match off.checked_add(4).and_then(|end| m.payload.get(off..end)) {
        Some(b) => i32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

/// Read a native-endian `u64` from a payload offset. `0` past the end.
fn rd_u64(m: &Message, off: usize) -> u64 {
    match off.checked_add(8).and_then(|end| m.payload.get(off..end)) {
        Some(b) => u64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        None => 0,
    }
}

/// Write a native-endian `i32` to a payload offset. A no-op past the end.
fn wr_i32(m: &mut Message, off: usize, v: i32) {
    if let Some(b) = off
        .checked_add(4)
        .and_then(|end| m.payload.get_mut(off..end))
    {
        b.copy_from_slice(&v.to_ne_bytes());
    }
}

/// Read the inode number out of an `FS_TRUNC` request.
///
/// One field. It shares [`FS_INO_OFF`] with `FS_LOOKUP`'s reply and `FS_READ`'s
/// request for the reason that constant's docs give: it is one field, and the
/// number `FS_LOOKUP` hands out is the number every later request takes back.
///
/// Its own function rather than `parse_read(msg).ino`, so that a request whose
/// other three fields are meaningless cannot be read as though they were not.
pub fn parse_trunc(msg: &Message) -> i32 {
    rd_i32(msg, FS_INO_OFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inode::{I_DIRECTORY, I_REGULAR};
    use minixrs_kernel_shared::callnr::FS_MAX_IO;

    fn empty() -> Message {
        Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        }
    }

    fn with_path(path: &[u8]) -> Message {
        let mut m = empty();
        m.payload[FS_PATH_OFF..FS_PATH_OFF + path.len()].copy_from_slice(path);
        m
    }

    #[test]
    fn parse_read_reads_every_field_from_its_own_offset() {
        // Four distinct values, so a swapped pair of offsets fails — the whole
        // reason this codec is tested rather than trusted.
        let mut m = empty();
        wr_i32(&mut m, FS_INO_OFF, 7);
        wr_i32(&mut m, FS_GRANT_OFF, 0x1234);
        wr_i32(&mut m, FS_LEN_OFF, 512);
        m.payload[FS_POS_OFF..FS_POS_OFF + 8].copy_from_slice(&0x9abc_u64.to_ne_bytes());

        assert_eq!(
            parse_read(&m),
            ReadRequest {
                ino: 7,
                gid: 0x1234,
                len: 512,
                pos: 0x9abc,
            }
        );
    }

    #[test]
    fn parse_read_of_a_zeroed_payload_yields_a_rejectable_request() {
        // A client that sent nothing: inode 0 does not exist, so the lookup fails
        // rather than anything panicking or copying.
        let req = parse_read(&empty());
        assert_eq!(req.ino, 0);
        assert_eq!(req.len, 0);
        assert_eq!(req.pos, 0);
    }

    #[test]
    fn parse_readsuper_reads_the_minor() {
        let mut m = empty();
        wr_i32(&mut m, FS_SUPER_MINOR_OFF, 3);
        assert_eq!(parse_readsuper(&m), 3);
        assert_eq!(parse_readsuper(&empty()), 0);
    }

    #[test]
    fn a_nul_terminated_path_decodes_to_its_bytes() {
        assert_eq!(parse_lookup(&with_path(b"/etc/motd\0")), Ok("/etc/motd"));
        // A zeroed payload is the empty path — malformed, but `walk::parse_path`
        // is what says so; this layer only decodes.
        assert_eq!(parse_lookup(&empty()), Ok(""));
    }

    #[test]
    fn bytes_past_the_terminator_are_not_part_of_the_path() {
        // NUL-*padded*, not merely NUL-terminated: a stale tail from an earlier
        // request must not extend this one's path.
        let mut m = with_path(b"/etc/motd\0");
        m.payload[FS_PATH_OFF + 20..FS_PATH_OFF + 28].copy_from_slice(b"/pattern");
        assert_eq!(parse_lookup(&m), Ok("/etc/motd"));
    }

    #[test]
    fn a_path_that_fills_the_field_with_no_nul_is_enametoolong() {
        // Never a silent truncation: a truncated path could resolve to a
        // different file than the one the client asked for.
        let mut m = empty();
        for b in &mut m.payload[FS_PATH_OFF..FS_PATH_OFF + FS_PATH_MAX] {
            *b = b'a';
        }
        assert_eq!(parse_lookup(&m), Err(ENAMETOOLONG));

        // One byte shorter, with the NUL in the last slot, is fine — the
        // boundary itself.
        m.payload[FS_PATH_OFF + FS_PATH_MAX - 1] = 0;
        assert_eq!(parse_lookup(&m).map(str::len), Ok(FS_PATH_MAX - 1));
    }

    #[test]
    fn a_path_field_that_is_not_utf8_is_einval() {
        // Distinguishable from ENAMETOOLONG on purpose: one is a client that
        // asked for too much, the other a client that sent nonsense.
        assert_eq!(
            parse_lookup(&with_path(&[b'/', 0xff, 0xfe, 0])),
            Err(EINVAL)
        );
    }

    #[test]
    fn the_path_field_cannot_reach_past_the_payload() {
        // The field's width is what makes the inline path viable at all; if it
        // ever grew past the payload this slice would panic rather than truncate.
        assert_eq!(
            (FS_PATH_OFF + FS_PATH_MAX).min(96),
            FS_PATH_OFF + FS_PATH_MAX
        );
    }

    #[test]
    fn a_readsuper_reply_round_trips_through_its_own_offsets() {
        let mut m = empty();
        reply_readsuper(&mut m, 1, 4096, 256);
        assert_eq!(rd_i32(&m, FS_SUPER_ROOT_OFF), 1);
        assert_eq!(rd_i32(&m, FS_SUPER_BLOCK_SIZE_OFF), 4096);
        assert_eq!(rd_i32(&m, FS_SUPER_BLOCKS_OFF), 256);
    }

    #[test]
    fn a_lookup_reply_round_trips_and_keeps_the_type_bits() {
        // Widening the u16 mode to i32 must not lose the bits a client branches
        // on — which is the whole content of the reply beyond the inode number.
        let mut m = empty();
        reply_lookup(&mut m, 5, I_REGULAR | 0o644, 31);
        assert_eq!(rd_i32(&m, FS_INO_OFF), 5);
        assert_eq!(rd_i32(&m, FS_MODE_OFF), i32::from(I_REGULAR | 0o644));
        assert_eq!(rd_i32(&m, FS_SIZE_OFF), 31);

        reply_lookup(&mut m, 1, I_DIRECTORY | 0o755, 4096);
        assert_eq!(rd_i32(&m, FS_MODE_OFF) as u16 & 0o170000, I_DIRECTORY);
    }

    #[test]
    fn a_reply_clears_the_request_it_is_built_over() {
        // Replies are built in the request's own message, so a field the reply
        // does not set must not still hold the request's value — a client reading
        // `size` out of a reply that never set one would otherwise get the
        // request's `len`.
        let mut m = empty();
        wr_i32(&mut m, FS_LEN_OFF, FS_MAX_IO as i32);
        reply_lookup(&mut m, 5, I_REGULAR, 0);
        assert_eq!(rd_i32(&m, FS_SIZE_OFF), 0);
        assert!(m.payload[12..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_trunc_reads_the_inode_and_ignores_everything_else() {
        let mut m = empty();
        wr_i32(&mut m, FS_INO_OFF, 7);
        wr_i32(&mut m, FS_GRANT_OFF, 0x1234);
        wr_i32(&mut m, FS_LEN_OFF, 512);
        assert_eq!(parse_trunc(&m), 7);
        // A zeroed payload reads as inode 0, which the server refuses as EINVAL.
        assert_eq!(parse_trunc(&empty()), 0);
    }

    #[test]
    fn scalar_accessors_are_total_past_the_end_of_the_payload() {
        // A malformed request must become an invalid value, never a panic.
        let mut m = empty();
        assert_eq!(rd_i32(&m, 96), 0);
        assert_eq!(rd_i32(&m, usize::MAX), 0);
        assert_eq!(rd_u64(&m, 90), 0);
        wr_i32(&mut m, 96, 7);
        assert!(m.payload.iter().all(|&b| b == 0));
    }
}

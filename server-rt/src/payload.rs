// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Native-endian scalar accessors for a [`Message`] payload.
//!
//! Every server and driver marshals its requests by hand — there is no typed
//! message-union layer in minix.rs (MINIX 3's `m1`/`m2`/… structs), because a
//! payload offset is documented per request in `kernel-shared::callnr` and
//! reading it by offset keeps the ABI in one place. What that leaves is these
//! four one-line accessors, which had been copied into PM, VFS, VM, DS, and
//! SCHED. Slice 5.3 lifted them here rather than copy them a sixth time into
//! `drivers/tty`.
//!
//! Native-endian on purpose: both ends of every message are the same machine, and
//! the generated C headers describe the payload as raw bytes at an offset. There
//! is no wire format to agree on.
//!
//! Out-of-range offsets and short slices are handled by returning `0` / writing
//! nothing rather than panicking. A server has no business dying on a malformed
//! request from a client it does not trust, and `0` is a value every request's
//! own validation already has to reject (endpoint `NONE` is `0`, grant id `0` has
//! sequence 0, length `0` is a no-op).

use minixrs_kernel_shared::Message;

/// `off..off + width` as a range, or `None` if the addition would overflow.
///
/// `checked_add` rather than `off + width`: servers ship `--release`, where
/// `overflow-checks = false` makes `usize::MAX + 4` *wrap* to 3 — which happens to
/// produce a `None` from `get(MAX..3)` and so happens to be safe, but only by
/// accident, and it panics under `cargo test`. Being explicit makes both profiles
/// behave the same.
fn span(off: usize, width: usize) -> Option<core::ops::Range<usize>> {
    off.checked_add(width).map(|end| off..end)
}

/// Read a native-endian `i32` from payload `off..off+4`. Returns `0` if the range
/// does not fit the payload.
pub fn rd_i32(m: &Message, off: usize) -> i32 {
    match span(off, 4).and_then(|r| m.payload.get(r)) {
        Some(b) => i32::from_ne_bytes(b.try_into().unwrap_or([0; 4])),
        None => 0,
    }
}

/// Write a native-endian `i32` into payload `off..off+4`. No-op if the range does
/// not fit the payload.
pub fn wr_i32(m: &mut Message, off: usize, v: i32) {
    if let Some(b) = span(off, 4).and_then(|r| m.payload.get_mut(r)) {
        b.copy_from_slice(&v.to_ne_bytes());
    }
}

/// Read a native-endian `u64` from payload `off..off+8`. Returns `0` if the range
/// does not fit the payload.
pub fn rd_u64(m: &Message, off: usize) -> u64 {
    match span(off, 8).and_then(|r| m.payload.get(r)) {
        Some(b) => u64::from_ne_bytes(b.try_into().unwrap_or([0; 8])),
        None => 0,
    }
}

/// Write a native-endian `u64` into payload `off..off+8`. No-op if the range does
/// not fit the payload.
pub fn wr_u64(m: &mut Message, off: usize, v: u64) {
    if let Some(b) = span(off, 8).and_then(|r| m.payload.get_mut(r)) {
        b.copy_from_slice(&v.to_ne_bytes());
    }
}

/// Read a NUL-padded, fixed-width name from payload `off..off+width`.
///
/// Returns `None` when the field does not fit the payload, is empty before its
/// first NUL, or is not valid UTF-8. Those are the three cases every caller has
/// to reject anyway, so collapsing them into one `None` keeps the call site to a
/// single errno arm — and it matches the kernel's own `SYS_EXEC` name reader
/// (`kernel/src/system/do_exec.rs`), which answers `EINVAL` to exactly this set.
///
/// A name that fills all `width` bytes with no NUL at all is well-formed; the
/// field is padded, not terminated. That is what lets `EXEC_NAME_LEN` carry a
/// name of exactly `EXEC_NAME_LEN` characters.
pub fn rd_name(m: &Message, off: usize, width: usize) -> Option<&str> {
    let field = span(off, width).and_then(|r| m.payload.get(r))?;
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let name = &field[..end];
    if name.is_empty() {
        return None;
    }
    core::str::from_utf8(name).ok()
}

/// The address of a local staging buffer, in the form the kernel's copy calls
/// want it (`SYS_COPY` / `SYS_SAFECOPY` take a plain `u64` VA).
///
/// `&mut` rather than `&`: the kernel writes through *its own* mapping of this
/// frame (the HHDM alias), not through this pointer, so the borrow checker cannot
/// see the write. Requiring an exclusive borrow is what stops a caller handing
/// over a buffer it is simultaneously reading. The SENDREC's inline asm is a full
/// memory clobber, so the compiler cannot cache the buffer's contents across the
/// call either.
pub fn buf_addr(b: &mut [u8]) -> u64 {
    b.as_mut_ptr() as usize as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> Message {
        Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        }
    }

    #[test]
    fn i32_round_trips_at_every_documented_offset() {
        // The offsets real requests use: 0/4/8/12 (i32 words) and 16 (where the
        // u64 fields start).
        for off in [0usize, 4, 8, 12, 16] {
            let mut m = msg();
            wr_i32(&mut m, off, -12345);
            assert_eq!(rd_i32(&m, off), -12345, "offset {off}");
        }
    }

    #[test]
    fn u64_round_trips() {
        for off in [0usize, 8, 16, 24, 32, 88] {
            let mut m = msg();
            wr_u64(&mut m, off, 0xDEAD_BEEF_CAFE_F00D);
            assert_eq!(rd_u64(&m, off), 0xDEAD_BEEF_CAFE_F00D, "offset {off}");
        }
    }

    #[test]
    fn writes_do_not_disturb_neighbouring_fields() {
        let mut m = msg();
        wr_i32(&mut m, 0, 1);
        wr_i32(&mut m, 4, 2);
        wr_i32(&mut m, 8, 3);
        wr_u64(&mut m, 16, 4);
        assert_eq!((rd_i32(&m, 0), rd_i32(&m, 4), rd_i32(&m, 8)), (1, 2, 3));
        assert_eq!(rd_u64(&m, 16), 4);
        // Nothing leaked into the gap at 12..16 or past the u64.
        assert_eq!(rd_i32(&m, 12), 0);
        assert_eq!(rd_u64(&m, 24), 0);
    }

    /// A malformed request must not be able to kill a server. The last legal
    /// offsets are 92 for an i32 and 88 for a u64; one past each must be inert.
    #[test]
    fn out_of_range_offsets_are_inert_not_panics() {
        let mut m = msg();
        wr_i32(&mut m, 92, 7);
        assert_eq!(rd_i32(&m, 92), 7, "92..96 is the last legal i32 slot");
        wr_u64(&mut m, 88, 9);
        assert_eq!(rd_u64(&m, 88), 9, "88..96 is the last legal u64 slot");

        let before = m.payload;
        wr_i32(&mut m, 93, 1);
        wr_u64(&mut m, 89, 1);
        wr_i32(&mut m, usize::MAX, 1);
        wr_u64(&mut m, usize::MAX, 1);
        assert_eq!(
            m.payload, before,
            "an out-of-range write must change nothing"
        );

        assert_eq!(rd_i32(&m, 93), 0);
        assert_eq!(rd_u64(&m, 89), 0);
        assert_eq!(rd_i32(&m, usize::MAX), 0);
        assert_eq!(rd_u64(&m, usize::MAX), 0);
    }

    /// Writes a name into `off..off+width` the way a client marshals one: the
    /// bytes, then NUL padding out to the field width.
    fn put_name(m: &mut Message, off: usize, width: usize, name: &str) {
        m.payload[off..off + width].fill(0);
        m.payload[off..off + name.len()].copy_from_slice(name.as_bytes());
    }

    #[test]
    fn name_round_trips_nul_padded() {
        let mut m = msg();
        put_name(&mut m, 0, 16, "worker");
        assert_eq!(rd_name(&m, 0, 16), Some("worker"));

        put_name(&mut m, 0, 16, "hello");
        assert_eq!(rd_name(&m, 0, 16), Some("hello"));
    }

    /// The field is padded, not terminated: a name filling all `width` bytes is
    /// well-formed. This is the case a `position(NUL).unwrap()` would panic on.
    #[test]
    fn a_name_filling_the_whole_field_is_well_formed() {
        let mut m = msg();
        let full = "0123456789abcdef";
        assert_eq!(full.len(), 16);
        put_name(&mut m, 0, 16, full);
        assert_eq!(rd_name(&m, 0, 16), Some(full));
    }

    /// The three rejected shapes, which the caller collapses into one `EINVAL`.
    #[test]
    fn empty_out_of_range_and_non_utf8_names_are_rejected() {
        let m = msg();
        assert_eq!(rd_name(&m, 0, 16), None, "an all-NUL field is empty");

        let mut m = msg();
        put_name(&mut m, 0, 16, "worker");
        assert_eq!(rd_name(&m, 88, 16), None, "88..104 overruns the payload");
        assert_eq!(rd_name(&m, usize::MAX, 16), None, "offset overflow");

        let mut m = msg();
        m.payload[0] = 0xFF;
        assert_eq!(rd_name(&m, 0, 16), None, "0xFF is not valid UTF-8");
    }

    /// Reading stops at the first NUL, so a stale longer name left in the field
    /// by a previous message cannot leak into a shorter one.
    #[test]
    fn reading_stops_at_the_first_nul() {
        let mut m = msg();
        put_name(&mut m, 0, 16, "worker");
        m.payload[0..5].copy_from_slice(b"init\0");
        assert_eq!(rd_name(&m, 0, 16), Some("init"));
    }

    #[test]
    fn buf_addr_reports_the_buffers_own_address() {
        let mut buf = [0u8; 16];
        let want = buf.as_mut_ptr() as usize as u64;
        assert_eq!(buf_addr(&mut buf), want);
        assert_ne!(buf_addr(&mut buf), 0);
    }
}

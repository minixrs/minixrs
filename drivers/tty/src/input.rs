// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The console's input ring (slice 6.5, spec R3).
//!
//! Bytes the PL011 receives wait here until a `CDEV_READ` collects them. The
//! ring is plain data: no `unsafe`, no IPC, no register access, so every rule
//! below is pinned by a host test rather than by a boot.
//!
//! ## Why `peek` and `consume` are two calls
//!
//! A read is served in three steps: copy the oldest bytes out of the ring into
//! a staging buffer, hand them to the client with `SYS_SAFECOPY`, and only then
//! remove them. The copy can fail, with `EPERM` for a grant that does not
//! authorize it or `EFAULT` for a buffer that is not mapped, and a failed read
//! must **consume nothing** (spec R1): the bytes are still owed to whoever asks
//! next. A single `pop`-into-slice call could not express that, because by the
//! time the copy's result is known the bytes would already be gone. So
//! [`Ring::peek`] takes `&self` and cannot remove anything, and
//! [`Ring::consume`] is called with the count that was actually delivered.
//!
//! ## Why a full ring drops the newest byte
//!
//! When [`TTY_INPUT_MAX`] bytes are already waiting, [`Ring::push`] refuses the
//! new one, counts it in [`Ring::dropped`], and returns `false` (spec R3).
//! Overwriting the oldest instead would tear a hole in the front of what the
//! reader has not yet seen; dropping the newest keeps every byte already
//! buffered, in the order it arrived, and loses only the tail of a burst
//! nobody was reading fast enough to keep.

// Until Task 3 wires RX in: nothing in `main.rs` constructs a `Ring` yet, so
// the bare-metal build would read this whole module as dead code.
#![allow(dead_code)]

/// Capacity of the input ring in bytes.
///
/// Equal to `CDEV_MAX_IO` today, so one read can empty a full ring, but the two
/// are separate facts: this one bounds what may wait between reads, that one
/// bounds what a single request moves.
pub const TTY_INPUT_MAX: usize = 256;

/// A fixed-capacity byte FIFO.
///
/// `head` is the index of the oldest byte and `len` the number buffered; the
/// newest byte is at `head + len - 1`, wrapped. Both invariants,
/// `head < TTY_INPUT_MAX` and `len <= TTY_INPUT_MAX`, hold after every call.
pub struct Ring {
    buf: [u8; TTY_INPUT_MAX],
    head: usize,
    len: usize,
    dropped: u32,
}

/// `idx + by`, wrapped into `0..TTY_INPUT_MAX`.
///
/// Total over all of `usize`: both operands are reduced first, and the sum is
/// then formed by subtraction from the capacity, so there is no addition that
/// could wrap in a release build (`overflow-checks = false`) and panic under
/// `cargo test`.
const fn advance(idx: usize, by: usize) -> usize {
    let idx = idx % TTY_INPUT_MAX;
    let by = by % TTY_INPUT_MAX;
    // `idx < TTY_INPUT_MAX`, so `room` is in `1..=TTY_INPUT_MAX`.
    let room = TTY_INPUT_MAX - idx;
    if by < room {
        // No wrap: `idx + by`, written as a subtraction that cannot underflow
        // (`room - by` is in `1..=room`).
        TTY_INPUT_MAX - (room - by)
    } else {
        // Wrapped past the end: what is left of `by` after reaching it.
        by - room
    }
}

impl Ring {
    /// An empty ring. `const`, so it can initialize a `static` or sit in
    /// `main`'s frame without a runtime constructor.
    pub const fn new() -> Self {
        Self {
            buf: [0u8; TTY_INPUT_MAX],
            head: 0,
            len: 0,
            dropped: 0,
        }
    }

    /// Append `b` as the newest byte. Returns whether it was stored.
    ///
    /// A full ring keeps what it has: the new byte is dropped, counted in
    /// [`Ring::dropped`] (saturating), and `false` comes back.
    pub fn push(&mut self, b: u8) -> bool {
        if self.len >= TTY_INPUT_MAX {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        // The slot one past the newest byte; `advance` is always in range.
        self.buf[advance(self.head, self.len)] = b;
        // `len < TTY_INPUT_MAX` was just checked, so this cannot saturate.
        self.len = self.len.saturating_add(1);
        true
    }

    /// Copy up to `out.len()` of the oldest bytes into `out`, oldest first,
    /// **without removing them**. Returns how many were copied.
    pub fn peek(&self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        for (i, dst) in out.iter_mut().take(n).enumerate() {
            // `i < n <= TTY_INPUT_MAX`, and `advance` reduces regardless.
            *dst = self.buf[advance(self.head, i)];
        }
        n
    }

    /// Remove the `min(n, len)` oldest bytes.
    pub fn consume(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = advance(self.head, n);
        self.len -= n;
    }

    /// Bytes buffered. Never more than [`TTY_INPUT_MAX`].
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes refused because the ring was full, saturating at `u32::MAX`.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring whose `head` sits at `at`, reached through the public API only:
    /// push `at` bytes and consume them.
    fn ring_with_head_at(at: usize) -> Ring {
        let mut r = Ring::new();
        for _ in 0..at {
            assert!(r.push(0xEE));
        }
        r.consume(at);
        assert!(r.is_empty());
        assert_eq!(r.head, at % TTY_INPUT_MAX);
        r
    }

    /// Everything buffered, oldest first, without disturbing the ring.
    fn contents(r: &Ring) -> Vec<u8> {
        let mut out = [0u8; TTY_INPUT_MAX];
        let n = r.peek(&mut out);
        out[..n].to_vec()
    }

    #[test]
    fn a_new_ring_is_empty() {
        let r = Ring::new();
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
        assert_eq!(r.dropped(), 0);
        let mut out = [0xAAu8; 8];
        assert_eq!(r.peek(&mut out), 0);
        // Nothing was copied, so nothing was written.
        assert_eq!(out, [0xAA; 8]);
    }

    #[test]
    fn push_then_peek_returns_bytes_oldest_first() {
        let mut r = Ring::new();
        for &b in b"abc" {
            assert!(r.push(b));
        }
        assert_eq!(r.len(), 3);
        assert!(!r.is_empty());
        let mut out = [0u8; 8];
        assert_eq!(r.peek(&mut out), 3);
        assert_eq!(&out[..3], b"abc");
    }

    #[test]
    fn peek_copies_no_more_than_the_slice_holds() {
        let mut r = Ring::new();
        for &b in b"abcdef" {
            r.push(b);
        }
        let mut out = [0u8; 4];
        assert_eq!(r.peek(&mut out), 4);
        assert_eq!(&out, b"abcd");
        // An empty slice is legal and copies nothing.
        assert_eq!(r.peek(&mut []), 0);
    }

    /// Review Focus 1, and the host half of mutation 7: a `SYS_SAFECOPY` that
    /// fails after the peek must leave every byte where it was.
    #[test]
    fn peek_does_not_consume() {
        let mut r = Ring::new();
        for &b in b"hello" {
            r.push(b);
        }
        let mut first = [0u8; 8];
        let mut second = [0u8; 8];
        assert_eq!(r.peek(&mut first), 5);
        assert_eq!(r.len(), 5);
        assert_eq!(r.peek(&mut second), 5);
        assert_eq!(first, second);
        assert_eq!(&first[..5], b"hello");
    }

    #[test]
    fn consume_removes_the_oldest_and_keeps_the_rest_in_order() {
        let mut r = Ring::new();
        for &b in b"abcdef" {
            r.push(b);
        }
        r.consume(2);
        assert_eq!(r.len(), 4);
        assert_eq!(contents(&r), b"cdef");
        r.consume(0);
        assert_eq!(contents(&r), b"cdef");
        r.consume(4);
        assert!(r.is_empty());
    }

    /// The served-read sequence: peek a short count, deliver it, consume
    /// exactly that count, and the remainder is what the next read sees.
    #[test]
    fn peek_then_consume_of_a_short_count_leaves_the_remainder() {
        let mut r = Ring::new();
        for &b in b"minix.rs" {
            r.push(b);
        }
        let mut out = [0u8; 5];
        let n = r.peek(&mut out);
        assert_eq!(&out[..n], b"minix");
        r.consume(n);
        assert_eq!(contents(&r), b".rs");
    }

    #[test]
    fn consume_of_more_than_is_buffered_empties_the_ring_and_no_more() {
        for n in [4usize, TTY_INPUT_MAX, TTY_INPUT_MAX + 1, usize::MAX] {
            let mut r = Ring::new();
            for &b in b"abc" {
                r.push(b);
            }
            r.consume(n);
            assert_eq!(r.len(), 0, "consume({n})");
            assert!(r.is_empty());
            // Still a working ring afterwards, with its invariants intact.
            assert!(r.head < TTY_INPUT_MAX, "consume({n})");
            assert!(r.push(b'z'));
            assert_eq!(contents(&r), b"z");
        }
    }

    #[test]
    fn consume_on_an_empty_ring_is_a_no_op() {
        let mut r = Ring::new();
        r.consume(1);
        r.consume(usize::MAX);
        assert_eq!(r.len(), 0);
        assert_eq!(r.head, 0);
    }

    /// Review Focus 2: with `head` on the last index, the second byte pushed
    /// lands at index 0 and a peek has to read across the end of the array.
    #[test]
    fn push_and_peek_wrap_across_the_end_of_the_array() {
        let mut r = ring_with_head_at(TTY_INPUT_MAX - 1);
        for &b in b"wxyz" {
            assert!(r.push(b));
        }
        assert_eq!(r.len(), 4);
        // Where the bytes physically are: one at the last index, three wrapped.
        assert_eq!(r.buf[TTY_INPUT_MAX - 1], b'w');
        assert_eq!(&r.buf[..3], b"xyz");
        assert_eq!(contents(&r), b"wxyz");

        // Consuming across the wrap leaves `head` in range and the order intact.
        r.consume(2);
        assert_eq!(r.head, 1);
        assert_eq!(contents(&r), b"yz");
    }

    #[test]
    fn a_ring_filled_from_the_last_index_holds_a_full_capacity_in_order() {
        let mut r = ring_with_head_at(TTY_INPUT_MAX - 1);
        for i in 0..TTY_INPUT_MAX {
            assert!(r.push(i as u8), "push {i}");
        }
        assert_eq!(r.len(), TTY_INPUT_MAX);
        let want: Vec<u8> = (0..TTY_INPUT_MAX).map(|i| i as u8).collect();
        assert_eq!(contents(&r), want);
        // Full, and wrapped: the next push is refused.
        assert!(!r.push(0xFF));
        assert_eq!(contents(&r), want);
    }

    /// Review Focus 2 and spec R3: the reader's unread bytes survive a burst.
    #[test]
    fn a_full_ring_drops_the_newest_and_keeps_what_it_had_in_order() {
        let mut r = Ring::new();
        for i in 0..TTY_INPUT_MAX {
            assert!(r.push(i as u8), "push {i}");
        }
        let before = contents(&r);
        assert_eq!(before.len(), TTY_INPUT_MAX);

        assert!(!r.push(0xAB));
        assert!(!r.push(0xCD));
        assert_eq!(r.dropped(), 2);
        assert_eq!(r.len(), TTY_INPUT_MAX);
        assert_eq!(contents(&r), before);

        // Room made by a read is usable again, and the dropped bytes are gone
        // for good rather than reappearing.
        r.consume(1);
        assert!(r.push(0xEF));
        assert_eq!(r.dropped(), 2);
        let after = contents(&r);
        assert_eq!(after[..TTY_INPUT_MAX - 1], before[1..]);
        assert_eq!(after[TTY_INPUT_MAX - 1], 0xEF);
    }

    #[test]
    fn a_stored_byte_is_not_counted_as_dropped() {
        let mut r = Ring::new();
        for i in 0..TTY_INPUT_MAX {
            r.push(i as u8);
        }
        assert_eq!(r.dropped(), 0);
    }

    /// Review Focus 2: over several laps of pushes and uneven reads, with
    /// overflows mixed in, `len` and `head` never leave their ranges and the
    /// bytes come out in the order they went in.
    #[test]
    fn len_never_exceeds_the_capacity() {
        let mut r = Ring::new();
        let mut model: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
        let mut next = 0u8;
        for round in 0..(TTY_INPUT_MAX * 5) {
            // Push more than is read in most rounds, so the ring fills and
            // overflows repeatedly.
            for _ in 0..(round % 7) {
                let stored = r.push(next);
                assert_eq!(stored, model.len() < TTY_INPUT_MAX, "round {round}");
                if stored {
                    model.push_back(next);
                }
                next = next.wrapping_add(1);
                assert_eq!(r.len().min(TTY_INPUT_MAX), r.len(), "round {round}");
            }
            let take = round % 5;
            r.consume(take);
            for _ in 0..take.min(model.len()) {
                model.pop_front();
            }
            assert_eq!(r.len(), model.len(), "round {round}");
            assert_eq!(r.len().min(TTY_INPUT_MAX), r.len(), "round {round}");
            assert_eq!(r.head.min(TTY_INPUT_MAX - 1), r.head, "round {round}");
            assert_eq!(contents(&r), Vec::from(model.clone()), "round {round}");
        }
        // The loop is only a proof if it actually overflowed.
        assert_ne!(r.dropped(), 0);
    }

    /// Four thousand million pushes is not a unit test, so start the counter
    /// one short of its ceiling.
    #[test]
    fn the_dropped_counter_saturates() {
        let mut r = Ring::new();
        for i in 0..TTY_INPUT_MAX {
            r.push(i as u8);
        }
        r.dropped = u32::MAX - 1;
        assert!(!r.push(1));
        assert_eq!(r.dropped(), u32::MAX);
        assert!(!r.push(2));
        assert_eq!(r.dropped(), u32::MAX);
        assert_eq!(r.len(), TTY_INPUT_MAX);
    }

    /// `rust-style.md`: release builds do not check overflow, so the index
    /// helper gets the extreme operands explicitly.
    #[test]
    fn advance_wraps_and_is_total_over_usize() {
        assert_eq!(advance(0, 0), 0);
        assert_eq!(advance(0, 1), 1);
        assert_eq!(advance(10, 20), 30);
        assert_eq!(advance(TTY_INPUT_MAX - 1, 0), TTY_INPUT_MAX - 1);
        assert_eq!(advance(TTY_INPUT_MAX - 1, 1), 0);
        assert_eq!(advance(TTY_INPUT_MAX - 1, 2), 1);
        assert_eq!(advance(TTY_INPUT_MAX - 2, 1), TTY_INPUT_MAX - 1);
        assert_eq!(advance(5, TTY_INPUT_MAX), 5);
        assert_eq!(
            advance(TTY_INPUT_MAX - 1, TTY_INPUT_MAX - 1),
            TTY_INPUT_MAX - 2
        );
        for (idx, by) in [
            (usize::MAX, usize::MAX),
            (usize::MAX, 0),
            (0, usize::MAX),
            (TTY_INPUT_MAX - 1, usize::MAX),
        ] {
            let got = advance(idx, by);
            assert!(got < TTY_INPUT_MAX, "advance({idx}, {by}) = {got}");
            assert_eq!(
                got as u128,
                (idx as u128 + by as u128) % TTY_INPUT_MAX as u128,
                "advance({idx}, {by})"
            );
        }
    }
}

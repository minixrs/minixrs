// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The port's state, with no device and no IPC in it (slice 6.6, spec C5–C8):
//! which side holds the one receive buffer, and whether a transmit chain is in
//! flight. `main.rs` does the copies, the queue calls and the replies, and asks
//! this module what each one is allowed to be. Host-tested.
//!
//! ## Receive
//!
//! The one receive buffer is [`Rx::Posted`] (chained into `receiveq`; the device
//! may fill it) or [`Rx::Held`] (the device returned it with `len` bytes, `off`
//! of which a reader has taken). The order a read follows is peek, copy,
//! consume:
//!
//! 1. [`Rx::peek`] says which bytes to copy and changes nothing.
//! 2. `main.rs` copies them to the caller's grant.
//! 3. Only if that copy succeeded, [`Rx::advance`] moves `off`. A refused copy
//!    calls nothing, so the bytes stay owed to the next reader.
//!
//! `advance` answering `Ok(true)` means the buffer is drained and the state is
//! `Posted` again: the caller must chain the buffer into `receiveq` and notify
//! the queue before it replies. Nothing else re-posts, except a zero-length
//! completion ([`RxUsed::Repost`]), which held nothing.
//!
//! ## Transmit
//!
//! [`Tx`] remembers the head of the one chain in flight. A used element in
//! `transmitq` is in turn only if it carries that head.
//!
//! ## Out of turn
//!
//! Every refusal is a [`Fault`] and leaves the state exactly as it was. The
//! caller ends the device on one (spec C8/C9) and prints it in the
//! `virtio FAIL step=io …` line.
//!
//! ## Two decisions `main.rs` only carries out
//!
//! [`absent_reply`] is the gate in front of every CDEV request: with no working
//! device the answer is `ENXIO`, whatever the request holds (spec C4).
//! [`wake_due`] says when the recorded waiter is owed its one `NOTIFY`: input
//! arrived, or the device has just stopped for good (spec C6, C9). Both are
//! here, and not in the loop that acts on them, so that a host test holds them.

// Only `main.rs`'s `not(test)`-gated paths reach this, so under `cargo test`
// (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use minixrs_kernel_shared::callnr::CDEV_MAX_IO;
use minixrs_kernel_shared::error::ENXIO;

/// Bytes in the receive buffer, and so the most one completion can deliver. The
/// same number as `bringup::VCON_BUF_LEN`; a test holds the two together.
pub const RX_BUF_LEN: usize = CDEV_MAX_IO;

/// A step taken out of turn. The first three are the device's doing (a used
/// element nobody was waiting for); the last two can only be this driver's own
/// bug, and are refused the same way rather than trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A used element in `receiveq` while the buffer was already held: the
    /// device returned a buffer it did not have.
    RxWhileHeld,
    /// A used element in `transmitq` with no chain in flight.
    TxIdle {
        /// The head the device reported.
        got: u16,
    },
    /// A used element in `transmitq` for a head other than the one in flight.
    TxWrongHead {
        /// The head that is in flight.
        in_flight: u16,
        /// The head the device reported.
        got: u16,
    },
    /// [`Tx::start`] while a chain was already in flight.
    TxBusy {
        /// The head that is in flight.
        in_flight: u16,
    },
    /// [`Rx::advance`] by more than [`Rx::available`].
    RxOverAdvance,
}

/// What a used element in `receiveq` means for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxUsed {
    /// The device returned the buffer empty. Nothing is held: chain it into
    /// `receiveq` again at once, and wake nobody.
    Repost,
    /// Input arrived and is now held: wake the recorded waiter (after the line
    /// is re-armed).
    Input,
}

/// Who has the one receive buffer.
///
/// [`Rx::on_used`] and [`Rx::advance`] only ever produce a `Held` with
/// `0 <= off < len <= RX_BUF_LEN`. The methods do not rely on that: they are
/// total for any field values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rx {
    /// Chained into `receiveq`: the device may fill it. The state at bring-up,
    /// once the buffer has been posted.
    Posted,
    /// The device returned it.
    Held {
        /// Bytes the device wrote, clamped to [`RX_BUF_LEN`].
        len: usize,
        /// Bytes of those already copied out to readers.
        off: usize,
    },
}

/// The one transmit chain in flight, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tx {
    in_flight: Option<u16>,
}

/// What one interrupt did, as `main.rs`'s handler reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Irq {
    /// A used element in `receiveq` left bytes held ([`RxUsed::Input`]).
    pub input: bool,
    /// The transmit chain in flight came back ([`Tx::complete`] said `Ok`).
    pub tx_done: bool,
}

/// The reply to a `CDEV_READ` or `CDEV_WRITE` that finds no working device, or
/// `None` when the device came `up` and the request is to be served (spec C4).
///
/// `up` is whether bring-up succeeded: `false` covers a machine with no console
/// attached and a bring-up step that failed. The answer is then `ENXIO` — the
/// device does not exist — and it is decided before the request is parsed, so a
/// malformed request and a zero-length one hear it too. A device that came up
/// and broke later is not this case: it exists, and says `EIO`.
pub const fn absent_reply(up: bool) -> Option<i32> {
    if up { None } else { Some(ENXIO) }
}

/// Whether the recorded waiter is owed its `NOTIFY` now (spec C6, C9): the
/// decision both character drivers share, with `broken` as this driver's
/// "stopped".
pub use minixrs_driver_rt::cdev::wake_due;

/// Bytes one `CDEV_WRITE` moves: `want`, capped at the transmit buffer. A longer
/// write is a short write, and the caller's loop re-sends the rest.
pub const fn tx_len(want: usize) -> usize {
    if want < CDEV_MAX_IO {
        want
    } else {
        CDEV_MAX_IO
    }
}

impl Rx {
    /// The state once the buffer has first been chained into `receiveq`.
    pub const fn new() -> Self {
        Rx::Posted
    }

    /// A used element came back in `receiveq` with the device-supplied length
    /// `written`.
    ///
    /// The length is the device's word, so it is clamped to [`RX_BUF_LEN`]: the
    /// device cannot have written more than the descriptor let it, and a read
    /// must never copy past the buffer. A clamped length of 0 holds nothing
    /// ([`RxUsed::Repost`], still `Posted`); anything else becomes
    /// `Held { len, off: 0 }` ([`RxUsed::Input`]).
    ///
    /// [`Fault::RxWhileHeld`], with the held bytes untouched, if the buffer was
    /// not posted.
    pub fn on_used(&mut self, written: u32) -> Result<RxUsed, Fault> {
        if matches!(self, Rx::Held { .. }) {
            return Err(Fault::RxWhileHeld);
        }
        // A `u32` that does not fit `usize` is certainly above the buffer.
        let len = usize::try_from(written).map_or(RX_BUF_LEN, |w| w.min(RX_BUF_LEN));
        if len == 0 {
            return Ok(RxUsed::Repost);
        }
        *self = Rx::Held { len, off: 0 };
        Ok(RxUsed::Input)
    }

    /// Bytes a read could take now: `len - off` when held, 0 when posted.
    pub const fn available(&self) -> usize {
        match *self {
            Rx::Posted => 0,
            Rx::Held { len, off } => len.saturating_sub(off),
        }
    }

    /// What a read wanting `want` bytes copies: `(offset into the buffer, n)`
    /// with `n = min(want, available)`, never 0. `None` when there is nothing to
    /// copy — the buffer is posted, or `want` is 0. Changes nothing; the caller
    /// follows a successful copy with [`Self::advance`]`(n)`.
    pub const fn peek(&self, want: usize) -> Option<(usize, usize)> {
        let avail = self.available();
        let n = if want < avail { want } else { avail };
        match *self {
            Rx::Held { off, .. } if n > 0 => Some((off, n)),
            _ => None,
        }
    }

    /// `n` bytes were copied out. Call only after the copy succeeded.
    ///
    /// `Ok(true)`: that was the last of them; the state is `Posted` and the
    /// caller must re-post the buffer and notify `receiveq` before replying.
    /// `Ok(false)`: bytes remain held (or `n` was 0 and nothing changed).
    ///
    /// [`Fault::RxOverAdvance`], with nothing changed, if `n` exceeds
    /// [`Self::available`] — which includes any `n > 0` while posted. No caller
    /// that passes [`Self::peek`]'s `n` can get it.
    pub fn advance(&mut self, n: usize) -> Result<bool, Fault> {
        if n > self.available() {
            return Err(Fault::RxOverAdvance);
        }
        let Rx::Held { len, off } = *self else {
            // Posted, so `available` is 0 and `n` is 0: nothing to do.
            return Ok(false);
        };
        // `n <= len - off`, so this cannot overflow; stay checked regardless.
        let off = off.checked_add(n).ok_or(Fault::RxOverAdvance)?;
        if off >= len {
            *self = Rx::Posted;
            return Ok(true);
        }
        *self = Rx::Held { len, off };
        Ok(false)
    }
}

impl Tx {
    /// Nothing in flight.
    pub const fn new() -> Self {
        Self { in_flight: None }
    }

    /// The chain headed by `head` was published to `transmitq`.
    /// [`Fault::TxBusy`], with the chain in flight unchanged, if there is one.
    pub fn start(&mut self, head: u16) -> Result<(), Fault> {
        if let Some(in_flight) = self.in_flight {
            return Err(Fault::TxBusy { in_flight });
        }
        self.in_flight = Some(head);
        Ok(())
    }

    /// A used element came back in `transmitq` for `head`. `Ok` only if that is
    /// the chain in flight, which is then finished. [`Fault::TxIdle`] with
    /// nothing in flight, [`Fault::TxWrongHead`] for any other head; neither
    /// changes anything.
    pub fn complete(&mut self, head: u16) -> Result<(), Fault> {
        match self.in_flight {
            None => Err(Fault::TxIdle { got: head }),
            Some(in_flight) if in_flight != head => Err(Fault::TxWrongHead {
                in_flight,
                got: head,
            }),
            Some(_) => {
                self.in_flight = None;
                Ok(())
            }
        }
    }

    /// The head of the chain in flight, if any. The tests' view of the state:
    /// the driver needs only what [`Self::complete`] answers.
    #[cfg(test)]
    pub const fn in_flight(&self) -> Option<u16> {
        self.in_flight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port holding `len` bytes, none read.
    fn held(len: u32) -> Rx {
        let mut rx = Rx::new();
        assert_eq!(rx.on_used(len), Ok(RxUsed::Input));
        rx
    }

    #[test]
    fn the_buffer_is_256_bytes_and_the_same_one_bringup_lays_out() {
        assert_eq!(RX_BUF_LEN, 256);
        assert_eq!(RX_BUF_LEN, crate::bringup::VCON_BUF_LEN);
    }

    #[test]
    fn a_new_port_is_posted_with_nothing_to_read() {
        let rx = Rx::new();
        assert_eq!(rx, Rx::Posted);
        assert_eq!(rx.available(), 0);
        assert_eq!(rx.peek(1), None);
        assert_eq!(rx.peek(usize::MAX), None);
    }

    #[test]
    fn a_completion_moves_posted_to_held_at_offset_zero() {
        let mut rx = Rx::new();
        assert_eq!(rx.on_used(16), Ok(RxUsed::Input));
        assert_eq!(rx, Rx::Held { len: 16, off: 0 });
        assert_eq!(rx.available(), 16);
        // One byte, and a whole buffer.
        assert_eq!(held(1), Rx::Held { len: 1, off: 0 });
        assert_eq!(held(256), Rx::Held { len: 256, off: 0 });
    }

    #[test]
    fn a_length_above_the_buffer_is_clamped_to_it() {
        for written in [257, 4096, 0x1_0000, u32::MAX - 1, u32::MAX] {
            let mut rx = Rx::new();
            assert_eq!(rx.on_used(written), Ok(RxUsed::Input), "{written}");
            assert_eq!(rx, Rx::Held { len: 256, off: 0 }, "{written}");
            assert_eq!(rx.available(), 256);
            // A read never reaches past the buffer, whatever it asks for.
            assert_eq!(rx.peek(usize::MAX), Some((0, 256)));
        }
    }

    #[test]
    fn a_zero_length_completion_reposts_and_holds_nothing() {
        let mut rx = Rx::new();
        assert_eq!(rx.on_used(0), Ok(RxUsed::Repost));
        assert_eq!(rx, Rx::Posted);
        assert_eq!(rx.available(), 0);
        assert_eq!(rx.peek(8), None);
        // Still posted, so the next completion is in turn.
        assert_eq!(rx.on_used(0), Ok(RxUsed::Repost));
        assert_eq!(rx.on_used(3), Ok(RxUsed::Input));
        assert_eq!(rx, Rx::Held { len: 3, off: 0 });
    }

    #[test]
    fn a_partial_read_leaves_the_rest_held_at_the_right_offset() {
        let mut rx = held(10);
        assert_eq!(rx.peek(4), Some((0, 4)));
        assert_eq!(rx.advance(4), Ok(false));
        assert_eq!(rx, Rx::Held { len: 10, off: 4 });
        assert_eq!(rx.available(), 6);
        // The next read starts where the last one stopped.
        assert_eq!(rx.peek(4), Some((4, 4)));
        assert_eq!(rx.advance(4), Ok(false));
        assert_eq!(rx, Rx::Held { len: 10, off: 8 });
        assert_eq!(rx.available(), 2);
        assert_eq!(rx.peek(4), Some((8, 2)));
    }

    #[test]
    fn the_offset_does_not_move_unless_advance_is_called() {
        // A refused copy calls nothing: the same bytes are offered again.
        let mut rx = held(10);
        assert_eq!(rx.advance(3), Ok(false));
        let before = rx;
        for _ in 0..4 {
            assert_eq!(rx.peek(5), Some((3, 5)));
            assert_eq!(rx.available(), 7);
        }
        assert_eq!(rx, before);
        assert_eq!(rx, Rx::Held { len: 10, off: 3 });
    }

    #[test]
    fn draining_the_buffer_posts_it_again_and_says_so() {
        // In one read.
        let mut rx = held(10);
        assert_eq!(rx.peek(10), Some((0, 10)));
        assert_eq!(rx.advance(10), Ok(true));
        assert_eq!(rx, Rx::Posted);
        assert_eq!(rx.available(), 0);
        // In several: only the last one says "re-post".
        let mut rx = held(5);
        assert_eq!(rx.advance(2), Ok(false));
        assert_eq!(rx.advance(2), Ok(false));
        assert_eq!(rx, Rx::Held { len: 5, off: 4 });
        assert_eq!(rx.advance(1), Ok(true));
        assert_eq!(rx, Rx::Posted);
        // Posted again, the device's next completion is in turn.
        assert_eq!(rx.on_used(7), Ok(RxUsed::Input));
        assert_eq!(rx, Rx::Held { len: 7, off: 0 });
    }

    #[test]
    fn a_whole_clamped_buffer_drains_at_exactly_256() {
        let mut rx = held(u32::MAX);
        assert_eq!(rx.advance(255), Ok(false));
        assert_eq!(rx.peek(100), Some((255, 1)));
        assert_eq!(rx.advance(1), Ok(true));
        assert_eq!(rx, Rx::Posted);
    }

    #[test]
    fn a_second_completion_while_held_is_a_fault_and_changes_nothing() {
        let mut rx = held(10);
        assert_eq!(rx.advance(4), Ok(false));
        for written in [0, 1, 10, 256, u32::MAX] {
            assert_eq!(rx.on_used(written), Err(Fault::RxWhileHeld), "{written}");
            assert_eq!(rx, Rx::Held { len: 10, off: 4 }, "{written}");
        }
        assert_eq!(rx.peek(100), Some((4, 6)));
    }

    #[test]
    fn a_read_never_gets_more_than_is_available() {
        let mut rx = held(10);
        assert_eq!(rx.advance(7), Ok(false));
        for want in [3, 4, 256, 257, usize::MAX - 1, usize::MAX] {
            assert_eq!(rx.peek(want), Some((7, 3)), "{want}");
        }
        assert_eq!(rx.peek(1), Some((7, 1)));
        assert_eq!(rx.peek(2), Some((7, 2)));
        // Held and asked for at least one byte: never an empty copy.
        for want in 1..=300usize {
            let (off, n) = rx.peek(want).expect("held");
            assert!(n >= 1 && n <= want && n <= rx.available());
            assert!(off + n <= RX_BUF_LEN);
        }
        // A zero-length read copies nothing (and is answered before this).
        assert_eq!(rx.peek(0), None);
    }

    #[test]
    fn every_offered_range_lies_inside_the_buffer() {
        // Every length the device can claim, read back in 7-byte pieces.
        for written in [1u32, 6, 7, 8, 255, 256, 257, u32::MAX] {
            let mut rx = held(written);
            let mut total = 0usize;
            loop {
                let (off, n) = rx.peek(7).expect("held until drained");
                assert_eq!(off, total);
                assert!(off + n <= RX_BUF_LEN);
                total += n;
                if rx.advance(n).expect("peek's n is in turn") {
                    break;
                }
            }
            assert_eq!(total, (written as usize).min(256));
            assert_eq!(rx, Rx::Posted);
        }
    }

    #[test]
    fn advancing_past_what_is_available_is_refused_and_changes_nothing() {
        let mut rx = held(10);
        assert_eq!(rx.advance(4), Ok(false));
        for n in [7, 10, 256, usize::MAX - 4, usize::MAX - 3, usize::MAX] {
            assert_eq!(rx.advance(n), Err(Fault::RxOverAdvance), "{n}");
            assert_eq!(rx, Rx::Held { len: 10, off: 4 }, "{n}");
        }
        // Exactly what is available is in turn.
        assert_eq!(rx.advance(6), Ok(true));
        // Posted: nothing is available, so any advance but 0 is refused.
        assert_eq!(rx.advance(1), Err(Fault::RxOverAdvance));
        assert_eq!(rx.advance(usize::MAX), Err(Fault::RxOverAdvance));
        assert_eq!(rx, Rx::Posted);
    }

    #[test]
    fn advancing_by_zero_changes_nothing_and_never_reposts() {
        let mut rx = Rx::new();
        assert_eq!(rx.advance(0), Ok(false));
        assert_eq!(rx, Rx::Posted);
        let mut rx = held(10);
        assert_eq!(rx.advance(0), Ok(false));
        assert_eq!(rx, Rx::Held { len: 10, off: 0 });
    }

    #[test]
    fn a_held_state_no_method_builds_is_still_handled_without_a_panic() {
        // `off` past `len`, and both at the top of `usize`: the fields are
        // public, so the methods may not assume their own invariant.
        for rx0 in [
            Rx::Held { len: 4, off: 9 },
            Rx::Held { len: 0, off: 0 },
            Rx::Held {
                len: usize::MAX,
                off: usize::MAX,
            },
            Rx::Held {
                len: 0,
                off: usize::MAX,
            },
        ] {
            let mut rx = rx0;
            assert_eq!(rx.available(), 0, "{rx0:?}");
            assert_eq!(rx.peek(usize::MAX), None, "{rx0:?}");
            assert_eq!(rx.advance(usize::MAX), Err(Fault::RxOverAdvance));
            assert_eq!(rx.on_used(u32::MAX), Err(Fault::RxWhileHeld));
            assert_eq!(rx, rx0);
        }
        let mut rx = Rx::Held {
            len: usize::MAX,
            off: 0,
        };
        assert_eq!(rx.available(), usize::MAX);
        assert_eq!(rx.peek(usize::MAX), Some((0, usize::MAX)));
        assert_eq!(rx.advance(usize::MAX), Ok(true));
        assert_eq!(rx, Rx::Posted);
    }

    #[test]
    fn a_transmit_starts_and_completes() {
        let mut tx = Tx::new();
        assert_eq!(tx.in_flight(), None);
        assert_eq!(tx.start(3), Ok(()));
        assert_eq!(tx.in_flight(), Some(3));
        assert_eq!(tx.complete(3), Ok(()));
        assert_eq!(tx.in_flight(), None);
        // And again, with the heads at the ends of their range.
        for head in [0, 7, u16::MAX] {
            assert_eq!(tx.start(head), Ok(()));
            assert_eq!(tx.in_flight(), Some(head));
            assert_eq!(tx.complete(head), Ok(()));
            assert_eq!(tx.in_flight(), None);
        }
    }

    #[test]
    fn a_completion_with_nothing_in_flight_is_a_fault() {
        let mut tx = Tx::new();
        assert_eq!(tx.complete(0), Err(Fault::TxIdle { got: 0 }));
        assert_eq!(tx.complete(5), Err(Fault::TxIdle { got: 5 }));
        assert_eq!(tx.in_flight(), None);
        // A chain that already completed cannot complete twice.
        assert_eq!(tx.start(5), Ok(()));
        assert_eq!(tx.complete(5), Ok(()));
        assert_eq!(tx.complete(5), Err(Fault::TxIdle { got: 5 }));
        assert_eq!(tx.in_flight(), None);
    }

    #[test]
    fn a_completion_for_another_head_is_a_fault_and_the_chain_stays_in_flight() {
        let mut tx = Tx::new();
        assert_eq!(tx.start(2), Ok(()));
        for got in [0, 1, 3, u16::MAX] {
            assert_eq!(
                tx.complete(got),
                Err(Fault::TxWrongHead { in_flight: 2, got })
            );
            assert_eq!(tx.in_flight(), Some(2));
        }
        assert_eq!(tx.complete(2), Ok(()));
    }

    #[test]
    fn a_second_start_is_refused_and_the_first_chain_stays_in_flight() {
        let mut tx = Tx::new();
        assert_eq!(tx.start(1), Ok(()));
        assert_eq!(tx.start(4), Err(Fault::TxBusy { in_flight: 1 }));
        // The same head again is refused too.
        assert_eq!(tx.start(1), Err(Fault::TxBusy { in_flight: 1 }));
        assert_eq!(tx.in_flight(), Some(1));
        // The refused head never became the one in flight.
        assert_eq!(
            tx.complete(4),
            Err(Fault::TxWrongHead {
                in_flight: 1,
                got: 4
            })
        );
        assert_eq!(tx.complete(1), Ok(()));
    }

    #[test]
    fn tx_len_passes_small_values_through_and_clamps_at_the_buffer() {
        assert_eq!(tx_len(0), 0);
        assert_eq!(tx_len(1), 1);
        assert_eq!(tx_len(17), 17);
        assert_eq!(tx_len(255), 255);
        assert_eq!(tx_len(256), 256);
        assert_eq!(tx_len(257), 256);
        assert_eq!(tx_len(4096), 256);
        assert_eq!(tx_len(usize::MAX - 1), 256);
        assert_eq!(tx_len(usize::MAX), 256);
        assert_eq!(tx_len(CDEV_MAX_IO), CDEV_MAX_IO);
    }

    #[test]
    fn with_no_working_device_every_request_is_enxio() {
        // Spec C4: no device, or a failed bring-up. The gate takes nothing of
        // the request, so there is no request it could let through.
        assert_eq!(absent_reply(false), Some(ENXIO));
        assert_eq!(ENXIO, -6);
        // A device that came up is served; what it answers is not decided here.
        assert_eq!(absent_reply(true), None);
    }

    #[test]
    fn input_wakes_the_waiter() {
        assert!(wake_due(true, false, false));
    }

    #[test]
    fn a_device_that_just_broke_wakes_the_waiter_with_or_without_input() {
        // Spec C9: the parked reader must hear `EIO`, not wait for good.
        assert!(wake_due(false, false, true));
        assert!(wake_due(true, false, true));
    }

    #[test]
    fn nothing_arriving_on_a_healthy_or_long_broken_device_wakes_nobody() {
        // A configuration change, or a transmit completion alone.
        assert!(!wake_due(false, false, false));
        // Already broken before this event: the one wake-up went out then.
        assert!(!wake_due(false, true, true));
    }

    #[test]
    fn an_interrupt_reports_nothing_until_something_happened() {
        assert_eq!(
            Irq::default(),
            Irq {
                input: false,
                tx_done: false
            }
        );
    }

    #[test]
    fn each_fault_names_itself_for_the_fail_line() {
        assert_eq!(format!("{:?}", Fault::RxWhileHeld), "RxWhileHeld");
        assert_eq!(format!("{:?}", Fault::RxOverAdvance), "RxOverAdvance");
        assert_eq!(
            format!("{:?}", Fault::TxIdle { got: 3 }),
            "TxIdle { got: 3 }"
        );
        assert_eq!(
            format!("{:?}", Fault::TxBusy { in_flight: 3 }),
            "TxBusy { in_flight: 3 }"
        );
        assert_eq!(
            format!(
                "{:?}",
                Fault::TxWrongHead {
                    in_flight: 1,
                    got: 2
                }
            ),
            "TxWrongHead { in_flight: 1, got: 2 }"
        );
    }
}

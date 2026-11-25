// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! TTY's interrupt line: the `SYS_IRQCTL` claim, its denial battery, the
//! two-round delivery proof (slice 6.1, spec I3–I7, I10, I13), and the receive
//! service the proof hands the line to (slice 6.5, spec R4, R5).
//!
//! TTY claims the PL011's line, INTID 33 — the one entry the kernel puts on its
//! allow-list at boot — under [`TTY_IRQ_NOTIFY_ID`], then drives the call's
//! refusals from EL0: each row of [`CASES`] is valid in every respect but one,
//! and must come back with exactly the result code the spec's table names. It
//! ends with the one positive case, the I6 replacement: the same claim again
//! answers `OK` with the same hook id, which TTY adopts.
//!
//! ## The proof: two TX interrupts
//!
//! [`IrqService`] first makes the line fire for real (I13). TTY sets the
//! PL011's `UARTIMSC.TXIM`, the device raises its level-triggered TX interrupt,
//! and the kernel masks the line, records bit [`TTY_IRQ_NOTIFY_ID`] in TTY's
//! pending word and notifies TTY from `HARDWARE` with that word in the payload.
//! TTY acknowledges at the device by clearing `TXIM` and re-arms the line with
//! `IRQ_ENABLE` — twice, so `IRQ_ENABLE` is seen to work rather than merely
//! return — and then leaves `TXIM` clear for good: the kernel's own polled
//! console writes the same UART, and every byte it sends raises the TX
//! interrupt again, so a set `TXIM` would keep raising a line TTY has no reason
//! to service. (That is also why the acknowledgement is clearing `TXIM` and not
//! writing `UARTICR.TXIC`: the next transmitted byte undoes the latter.)
//! Transmission itself stays polled.
//!
//! ## Then the steady state: receive
//!
//! When the last round is in, the same object sets `UARTIMSC.RXIM | RTIM`,
//! prints `[diag tty] rx on`, and from then on every notification carrying the
//! line's bit is serviced in this order (spec R4):
//!
//! ```text
//! write UARTICR = RXIC | RTIC          acknowledge both sources
//! while FR.RXFE == 0: push(DR & 0xFF)  drain until the FIFO reports empty
//! IRQ_ENABLE                           re-arm the line at the GIC
//! (main) NOTIFY the waiter, if any     only if a byte was pushed
//! ```
//!
//! **The acknowledgement precedes the drain.** A byte landing between a
//! drain-to-empty and a later `UARTICR` write would have its interrupt
//! acknowledged away with the byte still in the FIFO, and the re-arm would find
//! the line low: one keystroke stuck until the next. Acknowledged first, a byte
//! that lands after the final `RXFE` test raises a fresh, unacknowledged
//! interrupt, which the re-arm takes. The cost is at worst one notification
//! that finds the FIFO empty, which the loop handles by re-arming and doing
//! nothing else.
//!
//! **The re-arm precedes the waiter's `NOTIFY`**, which is why
//! [`IrqService::on_notify`] only *reports* that bytes arrived and `main` sends
//! the notification afterwards: the line is live again before VFS can come
//! back with a read.
//!
//! One empty notification is legal; a line that keeps firing with nothing to
//! drain is a device that is not dropping its line. [`EmptyRun`] counts
//! consecutive empty notifications, and at [`RX_EMPTY_STORM`] of them TTY
//! prints `[diag tty] rx FAIL storm` and leaves the line masked (spec R5). A
//! line that stops — that way, by a refused re-arm, or by a proof round going
//! wrong — can deliver nothing more: [`IrqService::can_receive`] turns `false`
//! for good, `main` answers every later read `EIO`, and it wakes the reader
//! parked on the line once so that its retry hears so (slice 6.6, spec C9).
//!
//! The storm guard replaces slice 6.1's post-proof guard, which reported every
//! notification after the proof as a failure and so could not survive a line
//! that is meant to keep firing.

// Only `main.rs`'s `not(test)`-gated paths reach this, so under `cargo test`
// (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use minixrs_driver_rt::IrqLine;
use minixrs_kernel_shared::callnr::{IRQ_ENABLE, IRQ_REENABLE, IRQ_SETPOLICY};
use minixrs_kernel_shared::error::{EBUSY, EINVAL, EPERM, OK};
use minixrs_kernel_shared::irq::NR_IRQ_HOOKS;
use minixrs_kernel_shared::virtio::NR_VIRTIO_DRIVERS;
use minixrs_server_rt::{diag_fmt, sys_irqctl};

use crate::input::Ring;
use crate::pl011::{self, PL011_INTID};

/// The bit TTY's interrupt notifications carry in their pending bitmap.
pub const TTY_IRQ_NOTIFY_ID: u8 = 0;

/// How many TX interrupts [`IrqService`]'s proof takes before it reports: the
/// second one can only arrive if the first round's `IRQ_ENABLE` really
/// unmasked the line.
const IRQ_PROOF_ROUNDS: u32 = 2;

/// How many **consecutive** notifications may drain nothing before the line is
/// declared stuck (spec R5). One empty notification is the legal cost of
/// acknowledging before draining; this many in a row is a device that is not
/// dropping its line. The threshold is a guess — see the spec's rulings table.
pub const RX_EMPTY_STORM: u32 = 64;

/// The run of consecutive notifications that drained no byte (spec R5).
///
/// Plain data, so the rule is pinned by host tests rather than by a boot that
/// would need a misbehaving UART to reach it.
pub struct EmptyRun(u32);

impl EmptyRun {
    /// No empty notification seen yet.
    pub const fn new() -> Self {
        Self(0)
    }

    /// Account for one serviced notification that drained `drained` bytes.
    /// Returns whether the run has reached [`RX_EMPTY_STORM`].
    ///
    /// Any notification that drained at least one byte ends the run; only
    /// *consecutive* empty ones count. The count saturates, so the answer
    /// stays `true` however long a caller keeps asking.
    pub fn note(&mut self, drained: usize) -> bool {
        if drained > 0 {
            self.0 = 0;
            return false;
        }
        self.0 = self.0.saturating_add(1);
        self.0 >= RX_EMPTY_STORM
    }
}

/// The battery's last argument word: a literal, or "the slot after TTY's own
/// hook", which only exists once the claim has answered.
#[derive(Clone, Copy)]
enum Id {
    Lit(i32),
    /// `NR_IRQ_HOOKS` — the highest valid hook id (ids are 1-based), so a slot
    /// in range but unclaimed for as long as the boot has fewer than
    /// `NR_IRQ_HOOKS` claimers. (`claimed + 1` would name another driver's hook
    /// — `EPERM`, not `EINVAL` — once virtio-blk claims a line.)
    FreeSlot,
}

/// One refusal: `(request, vector, policy, id, expected result)`.
type Case = (i32, i32, i32, Id, i32);

const INTID: i32 = PL011_INTID as i32;

/// TTY plus every virtio driver claim a line; the free-slot row needs one hook
/// left over.
const _: () = assert!(NR_IRQ_HOOKS > 1 + NR_VIRTIO_DRIVERS);

/// [`TTY_IRQ_NOTIFY_ID`] as the battery's signed argument word.
const NOTIFY: i32 = TTY_IRQ_NOTIFY_ID as i32;

/// The denial battery, in order. Each row breaks exactly one rule.
const CASES: [Case; 11] = [
    // I4: a real SPI, but not on TTY's allow-list.
    (IRQ_SETPOLICY, INTID + 1, 0, Id::Lit(NOTIFY), EPERM),
    // I3: PPI 27 is the kernel's timer; SGIs and PPIs are never claimable.
    (IRQ_SETPOLICY, 27, 0, Id::Lit(NOTIFY), EINVAL),
    // I3: INTID 1020 is past every distributor's implemented range.
    (IRQ_SETPOLICY, 1020, 0, Id::Lit(NOTIFY), EINVAL),
    // I10: the policy word must be 0; IRQ_REENABLE is defined and refused.
    (IRQ_SETPOLICY, INTID, IRQ_REENABLE, Id::Lit(NOTIFY), EINVAL),
    // I6: TTY's own claimed line under a second notify_id.
    (IRQ_SETPOLICY, INTID, 0, Id::Lit(NOTIFY + 1), EBUSY),
    // I2: notify_id is a bit index into a u32, range-checked as signed.
    (IRQ_SETPOLICY, INTID, 0, Id::Lit(32), EINVAL),
    (IRQ_SETPOLICY, INTID, 0, Id::Lit(-1), EINVAL),
    // I2: hook ids are 1-based, so 0 never names one...
    (IRQ_ENABLE, 0, 0, Id::Lit(0), EINVAL),
    // ...nor does anything past the table...
    (IRQ_ENABLE, 0, 0, Id::Lit(NR_IRQ_HOOKS as i32 + 1), EINVAL),
    // ...nor a slot nobody holds.
    (IRQ_ENABLE, 0, 0, Id::FreeSlot, EINVAL),
    // I1: an unknown subcode.
    (99, 0, 0, Id::Lit(0), EINVAL),
];

/// Claim the PL011 line, run [`CASES`], then the I6 replacement. Returns the
/// line TTY holds, or `None` if the claim itself was refused. Every outcome
/// other than all-pass prints one `[diag tty] irq…FAIL` line.
pub fn claim_and_probe() -> Option<IrqLine> {
    let line = match IrqLine::new(PL011_INTID, TTY_IRQ_NOTIFY_ID) {
        Ok(line) => line,
        Err(rc) => {
            diag_fmt(format_args!("irq FAIL claim rc={rc}"));
            return None;
        }
    };

    for (i, &(request, vector, policy, id, expected)) in CASES.iter().enumerate() {
        let id = match id {
            Id::Lit(v) => v,
            Id::FreeSlot => NR_IRQ_HOOKS as i32,
        };
        let (rc, _) = sys_irqctl(request, vector, policy, id);
        if rc != expected {
            diag_fmt(format_args!("irq.deny FAIL case={i} rc={rc}"));
            return Some(line);
        }
    }

    // I6: a repeat claim by the same owner under the same notify_id replaces
    // its hook in place — same id, no second slot.
    match IrqLine::new(PL011_INTID, TTY_IRQ_NOTIFY_ID) {
        Ok(again) if again.hook_id() == line.hook_id() => {
            diag_fmt(format_args!("irq.deny ok n={}", CASES.len() + 1));
            Some(again)
        }
        Ok(again) => {
            diag_fmt(format_args!(
                "irq.deny FAIL case={} rc={OK} hook={}",
                CASES.len(),
                again.hook_id()
            ));
            Some(line)
        }
        Err(rc) => {
            diag_fmt(format_args!(
                "irq.deny FAIL case={} rc={rc} hook=0",
                CASES.len()
            ));
            Some(line)
        }
    }
}

/// What [`IrqService`] is doing with its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The I13 delivery proof, with this many TX rounds serviced so far.
    Proof(u32),
    /// The proof passed and the RX masks are set: every notification is
    /// receive service.
    Rx,
    /// A proof round went wrong, a re-arm failed or the line stormed. The
    /// failure has been printed and the line is masked at the GIC, so nothing
    /// further is serviced and no byte can arrive: a read is answered `EIO`
    /// (slice 6.6, spec C9).
    Stopped,
}

impl Phase {
    /// Whether a byte can still arrive: everything but [`Phase::Stopped`].
    ///
    /// During the proof receive is not on *yet*, but it is coming, so the
    /// answer is `true`: a read then is answered `EAGAIN` and its waiter is
    /// owed a `NOTIFY` that will be sent — for the first byte, or for the
    /// proof failing.
    const fn can_receive(self) -> bool {
        !matches!(self, Phase::Stopped)
    }

    /// The phase after one serviced notification, given what its re-arm
    /// answered: `Some(rc)` is `IRQ_ENABLE`'s result, `None` a line that was
    /// not re-armed at all (a proof notification without the line's bit, a
    /// storm).
    ///
    /// The line lives only on `Some(OK)`; anything else leaves it masked at
    /// the GIC with no interrupt to come, which is [`Phase::Stopped`], and
    /// nothing leaves that. Plain data, so each way of stopping is pinned by a
    /// host test rather than by a boot that would need a broken device.
    fn after(self, rearm: Option<i32>) -> Self {
        match (self, rearm) {
            (Phase::Stopped, _) => Phase::Stopped,
            (Phase::Proof(rounds), Some(OK)) => {
                let rounds = rounds.saturating_add(1);
                if rounds < IRQ_PROOF_ROUNDS {
                    Phase::Proof(rounds)
                } else {
                    Phase::Rx
                }
            }
            (Phase::Rx, Some(OK)) => Phase::Rx,
            (Phase::Proof(_) | Phase::Rx, _) => Phase::Stopped,
        }
    }
}

/// The owner of TTY's interrupt line: first the I13 delivery proof —
/// [`IRQ_PROOF_ROUNDS`] real PL011 TX interrupts, each acknowledged at the
/// device and re-armed with `IRQ_ENABLE` — and then, for the rest of the boot,
/// the receive service (spec R4, R5).
pub struct IrqService {
    /// The line `claim_and_probe` adopted.
    line: IrqLine,
    phase: Phase,
    /// Consecutive RX notifications that drained nothing.
    empty: EmptyRun,
}

impl IrqService {
    /// Start the proof on `line` (the result of [`claim_and_probe`]) by
    /// setting `TXIM`, which makes the PL011 raise its line. A `None` line is a
    /// failed claim, already reported: `TXIM` stays clear, there is no proof,
    /// and RX is never enabled — `main` answers `CDEV_READ` with `EIO`, as it
    /// does once a service that did start reports it cannot receive
    /// ([`IrqService::can_receive`]).
    pub fn start(line: Option<IrqLine>) -> Option<Self> {
        let line = line?;
        pl011::set_txim(true);
        Some(Self {
            line,
            phase: Phase::Proof(0),
            empty: EmptyRun::new(),
        })
    }

    /// Whether a byte can still arrive on this line (slice 6.6, spec C9).
    ///
    /// `true` through the proof — receive is not on yet, but it will be — and
    /// while receiving; `false` once the line has stopped, for any of the four
    /// reasons it can: `irq FAIL bits`, `irq FAIL enable`, `rx FAIL storm`,
    /// `rx FAIL enable`. It never turns `true` again. `main` answers a read
    /// `EIO` when this is `false`, and wakes its waiter once when a
    /// notification's handling makes it so.
    pub const fn can_receive(&self) -> bool {
        self.phase.can_receive()
    }

    /// Service one `HARDWARE` notification, whose payload carried `bits`.
    /// Received bytes go into `ring`.
    ///
    /// Returns whether at least one byte was pushed into `ring`, which is the
    /// caller's cue to `NOTIFY` its waiter. By the time this returns the line
    /// has been re-armed, so the notification goes out *after* `IRQ_ENABLE`
    /// (spec R4).
    pub fn on_notify(&mut self, bits: u32, ring: &mut Ring) -> bool {
        match self.phase {
            Phase::Proof(_) => {
                self.proof_round(bits);
                false
            }
            Phase::Rx => self.service_rx(bits, ring),
            Phase::Stopped => false,
        }
    }

    /// One round of the TX proof.
    ///
    /// The order is I12's: quiesce the device (clear `TXIM`, which drops the
    /// line), *then* re-arm the line at the GIC. Re-arming first would take the
    /// still-asserted line again at once. Prints `[diag tty] irq ok` when the
    /// last round is in — and hands the line to receive — or one
    /// `[diag tty] irq FAIL …` line, leaving the line masked, RX off and the
    /// service stopped, if a round goes wrong.
    fn proof_round(&mut self, bits: u32) {
        if !self.line.fired(bits) {
            self.phase = self.phase.after(None);
            diag_fmt(format_args!("irq FAIL bits={bits:#x}"));
            return;
        }
        pl011::set_txim(false);
        let rc = self.line.rearm();
        self.phase = self.phase.after(Some(rc));
        match self.phase {
            Phase::Stopped => diag_fmt(format_args!("irq FAIL enable rc={rc}")),
            // Raise the line again; the next notification is the next round.
            Phase::Proof(_) => pl011::set_txim(true),
            Phase::Rx => {
                // TXIM stays clear (I13): see the module docs.
                diag_fmt(format_args!("irq ok rounds={IRQ_PROOF_ROUNDS}"));
                // The hand-over (spec R5). The line was re-armed just above,
                // so a byte already sitting in the FIFO raises it the moment
                // the masks are set and is serviced like any other.
                pl011::set_rx_masks(true);
                diag_fmt(format_args!("rx on"));
            }
        }
    }

    /// Receive service for one notification (spec R4): acknowledge, drain to
    /// empty, re-arm. Returns whether a byte was pushed into `ring`.
    fn service_rx(&mut self, bits: u32, ring: &mut Ring) -> bool {
        if !self.line.fired(bits) {
            // Some other line's bit. TTY holds one line, so there is nothing
            // to service and nothing was masked on its account.
            return false;
        }

        // Acknowledge *before* draining; the module docs say why.
        pl011::ack_rx();

        let mut drained = 0usize;
        let mut pushed = false;
        while let Some(b) = pl011::getc() {
            drained = drained.saturating_add(1);
            if ring.push(b) {
                pushed = true;
            } else if ring.dropped() == 1 {
                // The first byte this boot to find the ring full (spec R3).
                // The count saturates and never returns to 1, so this prints
                // once however long nobody reads.
                diag_fmt(format_args!("rx overflow"));
            }
        }

        if self.empty.note(drained) {
            // Not re-armed: the line stays masked at the GIC, so this is the
            // last notification and the line prints once.
            self.phase = self.phase.after(None);
            diag_fmt(format_args!("rx FAIL storm"));
            return pushed;
        }

        let rc = self.line.rearm();
        self.phase = self.phase.after(Some(rc));
        if rc != OK {
            // The next byte's interrupt can never arrive. Whatever was just
            // drained stays in the ring, but nobody reads it: from here a
            // read is answered `EIO` (`read_plan` puts a stopped device
            // before the buffer; slice 6.6, spec C9).
            diag_fmt(format_args!("rx FAIL enable rc={rc}"));
        }
        pushed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the phases, and the ways the line stops (slice 6.6, spec C9) ----

    /// A refusal `IRQ_ENABLE` can answer; which one does not matter.
    const REFUSED: i32 = EINVAL;

    /// Receive is not on during the proof, but it is coming: a read then is
    /// answered `EAGAIN`, not `EIO`.
    #[test]
    fn a_line_still_in_its_proof_can_receive() {
        for rounds in 0..IRQ_PROOF_ROUNDS {
            assert!(Phase::Proof(rounds).can_receive(), "rounds {rounds}");
        }
        assert!(Phase::Rx.can_receive());
    }

    #[test]
    fn a_stopped_line_cannot_receive() {
        assert!(!Phase::Stopped.can_receive());
    }

    /// The healthy path: each re-armed proof round is one step, the last one
    /// hands the line to receive, and receive stays receive.
    #[test]
    fn re_armed_rounds_walk_the_proof_into_receive_and_stay_there() {
        let mut phase = Phase::Proof(0);
        for round in 1..IRQ_PROOF_ROUNDS {
            phase = phase.after(Some(OK));
            assert_eq!(phase, Phase::Proof(round));
        }
        phase = phase.after(Some(OK));
        assert_eq!(phase, Phase::Rx);
        assert_eq!(phase.after(Some(OK)), Phase::Rx);
    }

    /// `irq FAIL bits=…`: a proof notification without the line's bit is not
    /// re-armed, and receive is over before it began.
    #[test]
    fn a_proof_round_with_the_wrong_bits_ends_receive() {
        for rounds in 0..IRQ_PROOF_ROUNDS {
            let phase = Phase::Proof(rounds).after(None);
            assert_eq!(phase, Phase::Stopped, "rounds {rounds}");
            assert!(!phase.can_receive(), "rounds {rounds}");
        }
    }

    /// `irq FAIL enable rc=…`: the proof's re-arm was refused.
    #[test]
    fn a_refused_re_arm_in_the_proof_ends_receive() {
        for rounds in 0..IRQ_PROOF_ROUNDS {
            let phase = Phase::Proof(rounds).after(Some(REFUSED));
            assert_eq!(phase, Phase::Stopped, "rounds {rounds}");
            assert!(!phase.can_receive(), "rounds {rounds}");
        }
    }

    /// `rx FAIL storm`: the line is deliberately left without a re-arm.
    #[test]
    fn a_storm_ends_receive() {
        let phase = Phase::Rx.after(None);
        assert_eq!(phase, Phase::Stopped);
        assert!(!phase.can_receive());
    }

    /// `rx FAIL enable rc=…`: the re-arm after a drain was refused.
    #[test]
    fn a_refused_re_arm_in_receive_ends_receive() {
        let phase = Phase::Rx.after(Some(REFUSED));
        assert_eq!(phase, Phase::Stopped);
        assert!(!phase.can_receive());
    }

    /// Nothing restarts a stopped line: `main` wakes the waiter on the one
    /// transition into `Stopped`, and there must be only one.
    #[test]
    fn a_stopped_line_stays_stopped() {
        for rearm in [Some(OK), Some(REFUSED), None] {
            assert_eq!(Phase::Stopped.after(rearm), Phase::Stopped, "{rearm:?}");
        }
    }

    #[test]
    fn a_fresh_run_is_not_a_storm() {
        let mut run = EmptyRun::new();
        assert!(!run.note(1));
    }

    /// One empty notification is legal (spec R4); so are sixty-three.
    #[test]
    fn the_storm_trips_on_exactly_the_threshold_th_empty_notification() {
        let mut run = EmptyRun::new();
        for i in 1..RX_EMPTY_STORM {
            assert!(!run.note(0), "empty notification {i}");
        }
        assert!(run.note(0), "empty notification {RX_EMPTY_STORM}");
    }

    /// Only *consecutive* empty notifications count: one that drains a byte
    /// starts the run over.
    #[test]
    fn a_notification_that_drains_a_byte_resets_the_run() {
        let mut run = EmptyRun::new();
        for _ in 1..RX_EMPTY_STORM {
            assert!(!run.note(0));
        }
        assert!(!run.note(1));
        // A full threshold's worth is needed again, not one more.
        for i in 1..RX_EMPTY_STORM {
            assert!(!run.note(0), "empty notification {i} after the reset");
        }
        assert!(run.note(0));
    }

    #[test]
    fn any_non_zero_drain_resets_not_just_one_byte() {
        for drained in [1usize, 16, 36, usize::MAX] {
            let mut run = EmptyRun::new();
            for _ in 1..RX_EMPTY_STORM {
                assert!(!run.note(0));
            }
            assert!(!run.note(drained), "drained {drained}");
            assert!(!run.note(0), "drained {drained}");
        }
    }

    /// Empty notifications interleaved with input never accumulate.
    #[test]
    fn alternating_empty_and_full_never_storms() {
        let mut run = EmptyRun::new();
        for _ in 0..(RX_EMPTY_STORM as usize * 4) {
            assert!(!run.note(0));
            assert!(!run.note(3));
        }
    }

    /// The count saturates: asking past the threshold neither wraps back to
    /// "healthy" nor panics under `cargo test`'s overflow checks.
    #[test]
    fn a_tripped_run_stays_tripped() {
        let mut run = EmptyRun(u32::MAX - 1);
        assert!(run.note(0));
        assert!(run.note(0));
        assert!(run.note(0));
        // ...until a byte arrives.
        assert!(!run.note(1));
    }
}

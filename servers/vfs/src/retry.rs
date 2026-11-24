// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The retry walk's decisions: which driver a notification speaks for, and what
//! to do with one parked read on its behalf (slice 6.6, decision C10).
//!
//! `park.rs` is the record and the order; `main.rs` holds the grant, the
//! `CDEV_READ` and the late reply. This module is what sits between them and
//! needs neither: three small pure things, host-tested because two of them are
//! unreachable on a boot unless two drivers have readers parked at once — the
//! "lift a retry loop's body into a step function" rule.
//!
//! ## The walk is per driver
//!
//! A character driver that answers `CDEV_READ` with `EAGAIN` owes its caller one
//! `NOTIFY`, and that notification speaks for **that driver's** input only. So
//! on driver *D*'s notification the walk takes the parked reads oldest first
//! and, for each, [`step`] says one of three things:
//!
//! - [`Step::Skip`] — the descriptor resolves to another driver. The entry
//!   stays parked, untouched, and the walk moves past it. *D* has nothing to
//!   say about it, and asking its own driver would collect an `EAGAIN` that
//!   says nothing about *D*.
//! - [`Step::Ask`] — it is *D*'s. A count or an error is that reader's reply and
//!   the entry is freed; **the first `EAGAIN` from *D* ends the walk**, because
//!   *D* is empty again and that refusal re-armed its waiter.
//! - [`Step::Reply`] — the descriptor is no longer a character device at all.
//!   The reader is answered `EBADF` and freed, on any driver's walk.
//!
//! With one notifier (slice 6.5) the walk asked the oldest entry's driver,
//! whoever it was, and stopped at the first `EAGAIN`. With two that hangs: a
//! reader parked on TTY ahead of one parked on `vcon` turns every `vcon`
//! notification into a question to TTY, an `EAGAIN`, and a return — and the
//! `vcon` reader is never asked for.
//!
//! "Never spins on `EAGAIN`" and "a spurious notification costs one round" both
//! survive, per driver: a walk sends at most one `CDEV_READ` that is answered
//! `EAGAIN`, and it goes to the driver that notified.
//!
//! ## Which driver an entry belongs to is not stored
//!
//! [`step`] is handed what the parked descriptor resolves to *now*. The park
//! table keeps the raw request and nothing derived from it, so there is no
//! second copy of "which driver" to disagree with the descriptor table.

use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::EBADF;

use crate::fd::{CharDriver, Fd};

/// The character drivers' endpoints, resolved once in VFS's prologue and
/// carried by value from there.
///
/// One `Copy` struct rather than a positional `Endpoint` per driver, so a
/// fourth driver is a field and two `match` arms here instead of a parameter
/// threaded through every handler — and so two endpoints of the same type
/// cannot be passed in each other's place. MFS is not in it: it is not a
/// character driver, and nothing here routes to it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CharPeers {
    /// TTY, the serial console.
    pub tty: Endpoint,
    /// The memory driver: `/dev/null` and `/dev/zero`.
    pub mem: Endpoint,
    /// The virtio console driver: `/dev/hvc0`.
    pub vcon: Endpoint,
}

impl CharPeers {
    /// The one place a [`CharDriver`] becomes an address.
    pub fn endpoint(&self, dev: CharDriver) -> Endpoint {
        match dev {
            CharDriver::Tty => self.tty,
            CharDriver::Memory => self.mem,
            CharDriver::Vcon => self.vcon,
        }
    }

    /// The driver a `NOTIFY_MESSAGE` from `source` speaks for, or `None` if
    /// `source` is not a driver whose notification means "ask again".
    ///
    /// `source` must be the **kernel-stamped `m_source`**, never a payload
    /// field. The whole endpoint is compared, generation included, against the
    /// endpoints VFS resolved — the ones its `CDEV_READ`s go to, and so the only
    /// ones that can owe it a notification.
    pub fn notifier(&self, source: Endpoint) -> Option<CharDriver> {
        CharDriver::ALL
            .into_iter()
            .find(|&dev| can_say_eagain(dev) && self.endpoint(dev) == source)
    }
}

/// Whether `dev` ever answers `CDEV_READ` with `EAGAIN` — and therefore ever
/// owes VFS a notification, and ever has a reader parked on it.
///
/// TTY and the virtio console wait for input. The memory driver never does:
/// `/dev/null` is always at EOF and `/dev/zero` is always full, so a
/// notification from it is not an "ask again" and is dropped like a stranger's.
/// No wildcard arm: a new driver must be placed on one side or the other.
const fn can_say_eagain(dev: CharDriver) -> bool {
    match dev {
        CharDriver::Tty | CharDriver::Vcon => true,
        CharDriver::Memory => false,
    }
}

/// What the retry walk does with one parked read. See the module note.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Step {
    /// Another driver's reader: leave the entry parked and move past it.
    Skip,
    /// The notifier's own reader: send `CDEV_READ` for `minor` to `dev`. Any
    /// answer but `EAGAIN` is the reader's reply; `EAGAIN` ends the walk.
    Ask {
        /// The driver to ask — always the notifier.
        dev: CharDriver,
        /// The minor to ask it for, as the descriptor holds it. Whether it
        /// exists is the driver's answer, not the walk's.
        minor: i32,
    },
    /// Answer the reader with this errno and free its entry, asking nobody.
    Reply(i32),
}

/// Decide what the walk run by `notifier`'s notification does with one parked
/// read, given what that read's descriptor resolves to now (`fd::resolve`'s
/// answer, unmodified).
///
/// Anything that is not a character device — a file, a closed descriptor, or a
/// refusal from `resolve`, whatever its errno — is `EBADF`, on every driver's
/// walk: the read cannot be completed by anyone, so there is no driver whose
/// notification it should wait for. (A descriptor cannot be closed while its
/// owner is blocked, so this is unreachable today; the check is free.)
pub fn step(notifier: CharDriver, resolved: Result<Fd, i32>) -> Step {
    match resolved {
        Ok(Fd::CharDev { dev, minor }) if dev == notifier => Step::Ask { dev, minor },
        Ok(Fd::CharDev { .. }) => Step::Skip,
        Ok(Fd::File { .. }) | Ok(Fd::Unused) | Err(_) => Step::Reply(EBADF),
    }
}

/// What one walk has done so far, for the `cons.other ok` marker: whether it
/// has passed over a read parked on another driver.
///
/// A value made fresh for each notification, so the claim is about **one**
/// walk: an entry skipped by an earlier walk does not make a later walk's
/// answer "behind" it.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Walk {
    passed_over: bool,
}

impl Walk {
    /// A walk that has looked at nothing yet.
    pub const fn new() -> Self {
        Self { passed_over: false }
    }

    /// Record a [`Step::Skip`]: an entry stayed parked and the walk went on.
    pub fn pass_over(&mut self) {
        self.passed_over = true;
    }

    /// Asked at the moment the notifier answers one of its own readers with
    /// anything but `EAGAIN`: whether that reader sat **behind** another
    /// driver's — the thing a walk that stopped at the oldest entry could never
    /// do.
    pub fn answered_behind_another(&self) -> bool {
        self.passed_over
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::{
        CDEV_MINOR_CONSOLE, CDEV_MINOR_HVC0, CDEV_MINOR_NULL, CDEV_MINOR_ZERO,
    };
    use minixrs_kernel_shared::com::{
        MEM_PROC_NR, MFS_PROC_NR, PM_PROC_NR, TTY_PROC_NR, VCON_PROC_NR, boot_endpoint,
    };
    use minixrs_kernel_shared::endpoint::bump_generation;
    use minixrs_kernel_shared::error::{EINVAL, EMFILE};

    /// The drivers that can answer `CDEV_READ` with `EAGAIN`, and so notify.
    const NOTIFIERS: [CharDriver; 2] = [CharDriver::Tty, CharDriver::Vcon];

    fn peers() -> CharPeers {
        CharPeers {
            tty: boot_endpoint(TTY_PROC_NR),
            mem: boot_endpoint(MEM_PROC_NR),
            vcon: boot_endpoint(VCON_PROC_NR),
        }
    }

    fn chardev(dev: CharDriver, minor: i32) -> Result<Fd, i32> {
        Ok(Fd::CharDev { dev, minor })
    }

    // ----- the step function ------------------------------------------------

    #[test]
    fn the_notifiers_own_reader_is_asked_with_its_driver_and_minor() {
        assert_eq!(
            step(
                CharDriver::Tty,
                chardev(CharDriver::Tty, CDEV_MINOR_CONSOLE)
            ),
            Step::Ask {
                dev: CharDriver::Tty,
                minor: CDEV_MINOR_CONSOLE,
            }
        );
        assert_eq!(
            step(CharDriver::Vcon, chardev(CharDriver::Vcon, CDEV_MINOR_HVC0)),
            Step::Ask {
                dev: CharDriver::Vcon,
                minor: CDEV_MINOR_HVC0,
            }
        );
    }

    #[test]
    fn the_minor_is_passed_through_not_judged() {
        // Whether a minor exists is the driver's answer (`ENXIO`), never the
        // walk's: a nonsense minor on the notifier's driver is still asked.
        for minor in [-1, 7, i32::MAX, i32::MIN] {
            assert_eq!(
                step(CharDriver::Vcon, chardev(CharDriver::Vcon, minor)),
                Step::Ask {
                    dev: CharDriver::Vcon,
                    minor,
                }
            );
        }
    }

    #[test]
    fn a_reader_on_another_driver_is_skipped_whatever_its_minor() {
        // The hang C10 exists to prevent: `vcon` notifies, the oldest read is
        // TTY's. Asking TTY would hear `EAGAIN` and end the walk.
        assert_eq!(
            step(
                CharDriver::Vcon,
                chardev(CharDriver::Tty, CDEV_MINOR_CONSOLE)
            ),
            Step::Skip
        );
        // And the other way round — the two minors are both 0, so a step that
        // compared minors instead of drivers fails here.
        assert_eq!(CDEV_MINOR_CONSOLE, CDEV_MINOR_HVC0);
        assert_eq!(
            step(CharDriver::Tty, chardev(CharDriver::Vcon, CDEV_MINOR_HVC0)),
            Step::Skip
        );
        for minor in [CDEV_MINOR_NULL, CDEV_MINOR_ZERO] {
            assert_eq!(
                step(CharDriver::Tty, chardev(CharDriver::Memory, minor)),
                Step::Skip
            );
            assert_eq!(
                step(CharDriver::Vcon, chardev(CharDriver::Memory, minor)),
                Step::Skip
            );
        }
    }

    #[test]
    fn every_pair_of_drivers_asks_on_a_match_and_skips_otherwise() {
        // Exhaustive over `CharDriver::ALL`, so a fourth driver is covered the
        // moment it exists.
        for notifier in CharDriver::ALL {
            for dev in CharDriver::ALL {
                let want = if dev == notifier {
                    Step::Ask { dev, minor: 3 }
                } else {
                    Step::Skip
                };
                assert_eq!(
                    step(notifier, chardev(dev, 3)),
                    want,
                    "{notifier:?} / {dev:?}"
                );
            }
        }
    }

    #[test]
    fn a_descriptor_that_is_no_longer_a_device_is_ebadf_on_any_walk() {
        for notifier in CharDriver::ALL {
            // A file where the device was, a closed descriptor, and every way
            // `fd::resolve` can refuse — whatever errno it refused with.
            for resolved in [
                Ok(Fd::File { ino: 5, pos: 0 }),
                Ok(Fd::File {
                    ino: 0,
                    pos: u64::MAX,
                }),
                Ok(Fd::Unused),
                Err(EBADF),
                Err(EINVAL),
                Err(EMFILE),
                Err(0),
            ] {
                assert_eq!(
                    step(notifier, resolved),
                    Step::Reply(EBADF),
                    "{notifier:?} / {resolved:?}"
                );
            }
        }
    }

    // ----- the one-walk record behind `cons.other ok` ------------------------

    #[test]
    fn a_walk_that_passed_over_nothing_proves_nothing() {
        let w = Walk::new();
        assert!(!w.answered_behind_another());
        assert_eq!(w, Walk::default());
    }

    #[test]
    fn an_answer_after_a_pass_over_is_the_proof() {
        let mut w = Walk::new();
        w.pass_over();
        assert!(w.answered_behind_another());
        // Still true for every later answer of the same walk, and passing over
        // a second entry does not unsay it.
        w.pass_over();
        assert!(w.answered_behind_another());
    }

    #[test]
    fn the_record_is_per_walk() {
        let mut first = Walk::new();
        first.pass_over();
        // The next notification's walk starts clean: an entry passed over by
        // one walk does not make an answer in another walk "behind" it.
        let second = Walk::new();
        assert!(!second.answered_behind_another());
        assert!(first.answered_behind_another());
    }

    // ----- the peers ---------------------------------------------------------

    #[test]
    fn each_driver_resolves_to_its_own_endpoint() {
        let p = peers();
        assert_eq!(p.endpoint(CharDriver::Tty), p.tty);
        assert_eq!(p.endpoint(CharDriver::Memory), p.mem);
        assert_eq!(p.endpoint(CharDriver::Vcon), p.vcon);
        // Distinct in the fixture, or the three lines above prove nothing.
        assert_ne!(p.tty, p.mem);
        assert_ne!(p.tty, p.vcon);
        assert_ne!(p.mem, p.vcon);
    }

    #[test]
    fn tty_and_vcon_are_the_notifiers() {
        let p = peers();
        assert_eq!(p.notifier(p.tty), Some(CharDriver::Tty));
        assert_eq!(p.notifier(p.vcon), Some(CharDriver::Vcon));
        for d in NOTIFIERS {
            assert_eq!(p.notifier(p.endpoint(d)), Some(d));
        }
    }

    #[test]
    fn nobody_else_is_a_notifier() {
        let p = peers();
        // The memory driver is a character driver, but it never says `EAGAIN`,
        // so it never owes a notification and one from it is dropped.
        assert_eq!(p.notifier(p.mem), None);
        // Servers that are not character drivers at all.
        assert_eq!(p.notifier(boot_endpoint(PM_PROC_NR)), None);
        assert_eq!(p.notifier(boot_endpoint(MFS_PROC_NR)), None);
        // The whole endpoint is compared, generation included: a restarted
        // occupant of a driver's slot is not the driver VFS resolved.
        assert_eq!(p.notifier(bump_generation(p.tty)), None);
        assert_eq!(p.notifier(bump_generation(p.vcon)), None);
    }

    #[test]
    fn a_notifier_is_recognised_wherever_ds_put_it() {
        // The endpoints are whatever the prologue resolved, not the boot ones.
        let p = CharPeers {
            tty: bump_generation(boot_endpoint(TTY_PROC_NR)),
            mem: boot_endpoint(MEM_PROC_NR),
            vcon: bump_generation(boot_endpoint(VCON_PROC_NR)),
        };
        assert_eq!(p.notifier(p.tty), Some(CharDriver::Tty));
        assert_eq!(p.notifier(p.vcon), Some(CharDriver::Vcon));
        assert_eq!(p.notifier(boot_endpoint(TTY_PROC_NR)), None);
        assert_eq!(p.notifier(boot_endpoint(VCON_PROC_NR)), None);
    }
}

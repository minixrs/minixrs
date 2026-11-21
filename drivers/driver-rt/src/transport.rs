// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The transport seam and device bring-up (slice 6.2, spec T10).
//!
//! [`Transport`] is what a device driver and its virtqueue need and nothing
//! register-shaped. [`negotiate`] writes the virtio 1.x initialisation sequence
//! once for every device and every future transport; the decision inside
//! ([`accept_features`]) is pure and host-tested against a mock transport.
//! Nothing here reads a register in a loop waiting for a value: the only loop
//! is [`config_read`]'s bounded generation check.

/// Status bit: the guest has noticed the device.
pub const STATUS_ACKNOWLEDGE: u32 = 1;
/// Status bit: the guest knows how to drive the device.
pub const STATUS_DRIVER: u32 = 2;
/// Status bit: the driver is set up and the device may go live.
pub const STATUS_DRIVER_OK: u32 = 4;
/// Status bit: the driver has accepted the feature set.
pub const STATUS_FEATURES_OK: u32 = 8;
/// Status bit: the device hit an unrecoverable error and needs a reset.
pub const STATUS_NEEDS_RESET: u32 = 64;
/// Status bit: the driver gave up on the device.
pub const STATUS_FAILED: u32 = 128;

/// Feature bit 32: the device follows the virtio 1.x (non-legacy) layout.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Most attempts [`config_read`] makes to see a stable `ConfigGeneration`.
pub const CONFIG_READ_RETRIES: usize = 8;

/// Guest-physical addresses of one split virtqueue's three areas.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QueueAddrs {
    /// Descriptor table.
    pub desc_pa: u64,
    /// Available (driver) ring.
    pub avail_pa: u64,
    /// Used (device) ring.
    pub used_pa: u64,
}

/// What a virtio device driver needs from the bus it sits on.
pub trait Transport {
    /// The virtio DeviceID (2 = block, ...).
    fn device_id(&self) -> u32;
    /// The device status byte.
    fn status(&self) -> u32;
    /// Write the status byte; 0 resets the device.
    fn set_status(&mut self, status: u32);
    /// The device's 64 offered feature bits.
    fn device_features(&mut self) -> u64;
    /// Write the 64 feature bits the driver accepts.
    fn set_driver_features(&mut self, features: u64);
    /// Largest size the device supports for `queue`; 0 means the queue is unavailable.
    fn queue_max(&mut self, queue: u16) -> u16;
    /// Program and enable `queue` with `size` entries at `addrs`.
    fn queue_enable(&mut self, queue: u16, size: u16, addrs: &QueueAddrs);
    /// Tell the device `queue` has new available buffers.
    fn notify(&mut self, queue: u16);
    /// The pending interrupt reasons.
    fn interrupt_status(&self) -> u32;
    /// Acknowledge the interrupt reasons in `bits`.
    fn interrupt_ack(&mut self, bits: u32);
    /// The configuration-space generation counter.
    fn config_generation(&self) -> u32;
    /// One raw (not generation-checked) read of device config at `offset`.
    ///
    /// Restriction: the MMIO implementation performs aligned 32-bit accesses, which
    /// virtio permits only for 32- and 64-bit config fields; 8- and 16-bit fields
    /// need width-typed reads that do not exist yet (deferred to the first driver
    /// that needs one).
    fn config_read_raw(&self, offset: usize, buf: &mut [u8]);
}

/// `wanted ∩ offered` plus `VERSION_1`; `None` when the device does not offer `VERSION_1`.
pub const fn accept_features(offered: u64, wanted: u64) -> Option<u64> {
    if offered & VIRTIO_F_VERSION_1 == 0 {
        return None;
    }
    Some((wanted & offered) | VIRTIO_F_VERSION_1)
}

/// Why [`negotiate`] failed. After `NoVersion1` or `FeaturesRejected` the device
/// has been left with `FAILED` set; after `ResetIncomplete` nothing was written
/// past the reset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NegotiateError {
    /// The device does not offer `VIRTIO_F_VERSION_1`.
    NoVersion1 {
        /// The feature word the device offered.
        offered: u64,
    },
    /// The device cleared `FEATURES_OK` on read-back.
    FeaturesRejected {
        /// The status read back.
        status: u32,
    },
    /// Status did not read 0 right after the reset write.
    ResetIncomplete {
        /// The status read back.
        status: u32,
    },
}

/// Reset, `ACKNOWLEDGE`, `DRIVER`, feature exchange, `FEATURES_OK`, read-back.
/// Returns the accepted feature word. A feature failure sets `FAILED` and returns
/// the reason; a device whose status does not read 0 after the reset write is
/// left as it is (`ResetIncomplete`, nothing written after the reset).
/// `DRIVER_OK` is separate ([`driver_ok`]), after the queues exist.
pub fn negotiate<T: Transport>(t: &mut T, wanted: u64) -> Result<u64, NegotiateError> {
    t.set_status(0);
    // No wait-for-zero loop: the spec's wait is needed for the PCI transport, not
    // for MMIO, where reset completes inside the Status write; and this module may
    // not poll. One non-looping read-back catches a device that did not reset.
    let after_reset = t.status();
    if after_reset != 0 {
        return Err(NegotiateError::ResetIncomplete {
            status: after_reset,
        });
    }
    t.set_status(STATUS_ACKNOWLEDGE);
    t.set_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    let offered = t.device_features();
    let Some(accepted) = accept_features(offered, wanted) else {
        t.set_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FAILED);
        return Err(NegotiateError::NoVersion1 { offered });
    };
    t.set_driver_features(accepted);
    let with_ok = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
    t.set_status(with_ok);
    let status = t.status();
    if status & STATUS_FEATURES_OK == 0 {
        // FAILED goes without FEATURES_OK: a device re-validates features on any write
        // that sets it and drops the whole write when validation fails.
        t.set_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FAILED);
        return Err(NegotiateError::FeaturesRejected { status });
    }
    Ok(accepted)
}

/// Set `DRIVER_OK`; `Err(status)` without writing if the device already shows
/// `FAILED` or `NEEDS_RESET`, and if the read-back shows `FAILED` or
/// `NEEDS_RESET`, or lacks `DRIVER_OK`.
pub fn driver_ok<T: Transport>(t: &mut T) -> Result<(), u32> {
    let before = t.status();
    if before & (STATUS_FAILED | STATUS_NEEDS_RESET) != 0 {
        // Never echo a device-owned bit back in a write.
        return Err(before);
    }
    t.set_status(before | STATUS_DRIVER_OK);
    let status = t.status();
    if status & (STATUS_FAILED | STATUS_NEEDS_RESET) != 0 || status & STATUS_DRIVER_OK == 0 {
        return Err(status);
    }
    Ok(())
}

/// Give up on a device after a successful [`negotiate`]: write `FAILED` on top of
/// the status read back, without echoing the device-owned `NEEDS_RESET` and
/// without resetting. Re-writing bits the device still shows re-asserts nothing
/// it cleared, and `FEATURES_OK` already holds, so the write triggers no
/// re-validation.
pub fn fail<T: Transport>(t: &mut T) {
    let status = t.status();
    t.set_status((status & !STATUS_NEEDS_RESET) | STATUS_FAILED);
}

/// Generation-stable read of device config: re-read while `ConfigGeneration`
/// changed across the read, at most [`CONFIG_READ_RETRIES`] attempts. `true`
/// when a generation-stable read was obtained; `false` when it gave up, with the
/// last read left in `buf` (a config space that never settles must not hang).
pub fn config_read<T: Transport>(t: &T, offset: usize, buf: &mut [u8]) -> bool {
    for _ in 0..CONFIG_READ_RETRIES {
        let before = t.config_generation();
        t.config_read_raw(offset, buf);
        if t.config_generation() == before {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct MockTransport {
        offered: u64,
        reject_features: bool,
        rejected: bool,
        reset_sticks: bool,
        drop_driver_ok: bool,
        status: u32,
        status_writes: Vec<u32>,
        feature_writes: Vec<u64>,
        generations: RefCell<Vec<u32>>,
        last_gen: Cell<u32>,
        raw_reads: Cell<usize>,
    }

    impl MockTransport {
        fn new(offered: u64) -> Self {
            Self {
                offered,
                reject_features: false,
                rejected: false,
                reset_sticks: false,
                drop_driver_ok: false,
                status: 0,
                status_writes: Vec::new(),
                feature_writes: Vec::new(),
                generations: RefCell::new(Vec::new()),
                last_gen: Cell::new(0),
                raw_reads: Cell::new(0),
            }
        }
    }

    impl Transport for MockTransport {
        fn device_id(&self) -> u32 {
            2
        }
        fn status(&self) -> u32 {
            self.status
        }
        fn set_status(&mut self, status: u32) {
            self.status_writes.push(status);
            if status == 0 && self.reset_sticks {
                return;
            }
            if self.reject_features && status & STATUS_FEATURES_OK != 0 {
                // Like QEMU: the first FEATURES_OK write fails validation and the bit
                // reads back clear; any later write carrying FEATURES_OK is dropped whole.
                if !self.rejected {
                    self.rejected = true;
                    self.status = status & !STATUS_FEATURES_OK;
                }
                return;
            }
            if self.drop_driver_ok && status & STATUS_DRIVER_OK != 0 {
                return;
            }
            self.status = status;
        }
        fn device_features(&mut self) -> u64 {
            self.offered
        }
        fn set_driver_features(&mut self, features: u64) {
            self.feature_writes.push(features);
        }
        fn queue_max(&mut self, _queue: u16) -> u16 {
            8
        }
        fn queue_enable(&mut self, _queue: u16, _size: u16, _addrs: &QueueAddrs) {}
        fn notify(&mut self, _queue: u16) {}
        fn interrupt_status(&self) -> u32 {
            0
        }
        fn interrupt_ack(&mut self, _bits: u32) {}
        fn config_generation(&self) -> u32 {
            let mut g = self.generations.borrow_mut();
            if g.is_empty() {
                // Script exhausted: keep answering the last value served.
                return self.last_gen.get();
            }
            let v = g.remove(0);
            self.last_gen.set(v);
            v
        }
        fn config_read_raw(&self, _offset: usize, buf: &mut [u8]) {
            self.raw_reads.set(self.raw_reads.get() + 1);
            buf.fill(0xAB);
        }
    }

    #[test]
    fn accept_features_requires_version_1() {
        assert_eq!(accept_features(0xFF, 0), None);
    }

    #[test]
    fn accept_features_is_the_intersection_plus_version_1() {
        assert_eq!(
            accept_features(VIRTIO_F_VERSION_1 | 0b1010, 0b0110),
            Some(VIRTIO_F_VERSION_1 | 0b0010)
        );
    }

    #[test]
    fn negotiate_writes_the_spec_sequence() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1 | 0b1010);
        let got = negotiate(&mut t, 0b0110);
        assert_eq!(got, Ok(VIRTIO_F_VERSION_1 | 0b0010));
        assert_eq!(t.status_writes, [0, 1, 3, 11]);
        assert_eq!(t.feature_writes, [VIRTIO_F_VERSION_1 | 0b0010]);
    }

    #[test]
    fn negotiate_without_version_1_sets_failed() {
        let mut t = MockTransport::new(0xFF);
        assert_eq!(
            negotiate(&mut t, 0),
            Err(NegotiateError::NoVersion1 { offered: 0xFF })
        );
        assert_ne!(t.status_writes.last().unwrap() & STATUS_FAILED, 0);
        assert!(t.feature_writes.is_empty());
    }

    #[test]
    fn negotiate_fails_when_the_device_clears_features_ok() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        t.reject_features = true;
        let got = negotiate(&mut t, 0);
        assert!(matches!(got, Err(NegotiateError::FeaturesRejected { .. })));
        // The device state, not just the write log: FAILED must have landed.
        assert_ne!(t.status & STATUS_FAILED, 0);
        assert_eq!(t.status & STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn driver_ok_sets_the_bit_and_succeeds() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        negotiate(&mut t, 0).unwrap();
        assert_eq!(driver_ok(&mut t), Ok(()));
        assert_eq!(t.status & STATUS_DRIVER_OK, STATUS_DRIVER_OK);
    }

    #[test]
    fn driver_ok_reports_needs_reset() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        negotiate(&mut t, 0).unwrap();
        t.status |= STATUS_NEEDS_RESET;
        let got = driver_ok(&mut t);
        assert!(matches!(got, Err(s) if s & STATUS_NEEDS_RESET != 0));
    }

    #[test]
    fn negotiate_fails_cleanly_when_reset_does_not_clear_status() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        t.status = STATUS_ACKNOWLEDGE | STATUS_DRIVER;
        t.reset_sticks = true;
        let got = negotiate(&mut t, 0);
        assert_eq!(got, Err(NegotiateError::ResetIncomplete { status: 3 }));
        assert_eq!(t.status_writes, [0]);
    }

    #[test]
    fn driver_ok_refuses_a_failed_device_without_writing() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        t.status = STATUS_ACKNOWLEDGE | STATUS_FAILED;
        assert_eq!(driver_ok(&mut t), Err(STATUS_ACKNOWLEDGE | STATUS_FAILED));
        assert!(t.status_writes.is_empty());
    }

    #[test]
    fn driver_ok_refuses_needs_reset_without_echoing_it() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        t.status = STATUS_NEEDS_RESET;
        assert_eq!(driver_ok(&mut t), Err(STATUS_NEEDS_RESET));
        assert!(t.status_writes.is_empty());
    }

    #[test]
    fn driver_ok_fails_when_the_bit_is_missing_on_read_back() {
        // A device that ignores the DRIVER_OK write.
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        t.status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        t.drop_driver_ok = true;
        let got = driver_ok(&mut t);
        assert_eq!(
            got,
            Err(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK)
        );
    }

    #[test]
    fn fail_sets_failed_on_top_of_the_status_read_back() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        negotiate(&mut t, 0).unwrap();
        let before = t.status;
        fail(&mut t);
        assert_eq!(t.status_writes.last(), Some(&(before | STATUS_FAILED)));
        // The device state: FAILED landed and FEATURES_OK was not dropped.
        assert_ne!(t.status & STATUS_FAILED, 0);
        assert_ne!(t.status & STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn fail_never_echoes_needs_reset() {
        let mut t = MockTransport::new(VIRTIO_F_VERSION_1);
        negotiate(&mut t, 0).unwrap();
        let before = t.status;
        t.status |= STATUS_NEEDS_RESET;
        fail(&mut t);
        assert_eq!(t.status_writes.last(), Some(&(before | STATUS_FAILED)));
    }

    #[test]
    fn config_read_retries_until_the_generation_is_stable() {
        let t = MockTransport::new(0);
        *t.generations.borrow_mut() = vec![1, 2, 2, 2];
        let mut buf = [0u8; 4];
        assert!(config_read(&t, 0, &mut buf));
        assert_eq!(t.raw_reads.get(), 2);
        assert_eq!(buf, [0xAB; 4]);
    }

    #[test]
    fn config_read_gives_up_after_the_retry_bound() {
        let t = MockTransport::new(0);
        *t.generations.borrow_mut() = (1..=100).collect();
        let mut buf = [0u8; 4];
        assert!(!config_read(&t, 0, &mut buf));
        assert_eq!(t.raw_reads.get(), CONFIG_READ_RETRIES);
        assert_eq!(buf, [0xAB; 4]);
    }
}

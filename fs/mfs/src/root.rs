// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Which block driver the root filesystem is mounted from (slice 6.3, B2).
//!
//! Two drivers can serve the root: `vblk` (virtio-blk, a disk) and `memory`
//! (the ramdisk the kernel copies out of the boot archive). The server prefers
//! the disk and falls back to the ramdisk, and this module is the whole of that
//! preference — in the library because no host test, Miri or coverage job sees
//! `main.rs` (CI only lints it, with the `server` feature, and boots it).
//!
//! **A DS entry for `vblk` is not a root.** The driver publishes as soon as the
//! kernel assigns it a device, before it has read a single sector, because
//! anything later would lose the race with this server's lookup. What says
//! whether the device carries a minix.rs root is the driver's *answer*: it
//! replies `ENXIO` to every BDEV request when it has none. So the server asks
//! once — a zero-length `BDEV_READ` of the root minor, which the shared BDEV
//! validation answers `0` on a live minor with no grant use and no device I/O —
//! and that SENDREC cannot be answered early, because the driver does not reach
//! its receive loop until its own root check has a verdict.
//!
//! **Only `0` selects the disk.** `ENXIO` is the designed refusal, but every
//! other reply falls back too, positive ones included: a driver that answers a
//! zero-length read with a byte count, or with an errno this server did not
//! expect, is not one to mount a filesystem through. The reply is carried into
//! the fallback line verbatim, so the log says which of those it was.

use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::OK;

/// DS name `virtio-blk` publishes under — its proc name.
pub const VBLK_NAME: &[u8] = b"vblk";

/// DS name the ramdisk driver publishes under.
pub const MEMORY_NAME: &[u8] = b"memory";

/// What the server learned about `vblk` before choosing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum VblkProbe {
    /// DS has no `vblk` entry: no disk is attached. The healthy ramdisk boot.
    Absent,
    /// DS resolved `vblk` to `ep`, and the driver answered the zero-length
    /// root read with `rc` (the reply `m_type`, or the trap's own errno if the
    /// SENDREC itself failed).
    Answered { ep: Endpoint, rc: i32 },
}

/// The device the root is mounted from.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RootDevice {
    /// `vblk`, at this endpoint.
    Vblk(Endpoint),
    /// The `memory` driver, which the caller still has to resolve.
    ///
    /// `refused` is `Some(rc)` when a `vblk` was found and declined — the one
    /// case that owes the log a `bdev.root vblk rc=<rc> fallback=memory` line —
    /// and `None` when there was no `vblk` at all, which owes it nothing.
    Memory { refused: Option<i32> },
}

/// Choose the root device from what the `vblk` probe found.
pub fn select(probe: VblkProbe) -> RootDevice {
    match probe {
        VblkProbe::Absent => RootDevice::Memory { refused: None },
        VblkProbe::Answered { ep, rc: OK } => RootDevice::Vblk(ep),
        VblkProbe::Answered { rc, .. } => RootDevice::Memory { refused: Some(rc) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::error::{EINVAL, EIO, ENXIO, EPERM};

    const EP: Endpoint = 0x1_0007;

    #[test]
    fn a_ds_miss_is_the_ramdisk_with_nothing_to_report() {
        assert_eq!(
            select(VblkProbe::Absent),
            RootDevice::Memory { refused: None }
        );
    }

    #[test]
    fn a_zero_reply_selects_the_disk_at_the_resolved_endpoint() {
        assert_eq!(
            select(VblkProbe::Answered { ep: EP, rc: 0 }),
            RootDevice::Vblk(EP)
        );
    }

    #[test]
    fn enxio_falls_back_and_carries_the_errno() {
        assert_eq!(
            select(VblkProbe::Answered { ep: EP, rc: ENXIO }),
            RootDevice::Memory {
                refused: Some(ENXIO)
            }
        );
    }

    #[test]
    fn every_other_negative_reply_falls_back_with_that_reply() {
        for rc in [EIO, EINVAL, EPERM, -1, i32::MIN] {
            assert_eq!(
                select(VblkProbe::Answered { ep: EP, rc }),
                RootDevice::Memory { refused: Some(rc) },
                "rc={rc}"
            );
        }
    }

    #[test]
    fn a_positive_reply_to_a_zero_length_read_falls_back_too() {
        // A byte count for a request that asked for no bytes is not success.
        for rc in [1, 4096, i32::MAX] {
            assert_eq!(
                select(VblkProbe::Answered { ep: EP, rc }),
                RootDevice::Memory { refused: Some(rc) },
                "rc={rc}"
            );
        }
    }

    #[test]
    fn the_names_fit_a_ds_key_with_its_nul() {
        // A DS key is 16 bytes, NUL-padded; a name that filled it would have no
        // terminator.
        assert_eq!(VBLK_NAME, b"vblk");
        assert_eq!(MEMORY_NAME, b"memory");
        for name in [VBLK_NAME, MEMORY_NAME] {
            assert_eq!(name.len().min(15), name.len());
        }
    }
}

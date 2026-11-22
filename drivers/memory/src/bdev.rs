// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The ramdisk's geometry check — the one piece of BDEV logic that is this
//! driver's alone.
//!
//! The request parse and validation lived here until slice 6.3; they are now
//! `minixrs_server_rt::bdev`, shared with `virtio-blk`, because both block
//! drivers must refuse the same requests with the same errno in the same order.
//! [`validate_geometry`] stays: it is about the VA window the kernel maps the
//! ramdisk image into, which no other driver has. The block count it returns is
//! what `main.rs` hands the shared `validate_read` as the device size.
//!
//! A total function over a scalar, so it carries the crate's unit tests. The IPC
//! round-trip and the kernel calls live in `main.rs` and are verified through the
//! boot log. Same split as `drivers/tty`'s `cdev.rs` / `main.rs`.

use minixrs_kernel_shared::callnr::BDEV_BLOCK_SIZE;
use minixrs_kernel_shared::error::EINVAL;
use minixrs_kernel_shared::uspace::RAMDISK_WINDOW_SIZE;

/// Check the length the kernel reported for the ramdisk, and convert it to a block
/// count.
///
/// Cheap, host-testable, and it bounds the damage from a bogus reply: every later
/// range check is against the block count this returns, so a length the driver
/// accepted uncritically would become the ceiling on what it hands to
/// `SYS_SAFECOPY`.
///
/// `EINVAL` for zero, for a length that is not a whole number of blocks (the
/// driver serves whole blocks and nothing else), or for one larger than the VA
/// window the kernel reserves — which no honest reply can exceed, because the
/// kernel maps the image into that window.
pub fn validate_geometry(len: u64) -> Result<u64, i32> {
    if len == 0 {
        return Err(EINVAL);
    }
    if !len.is_multiple_of(BDEV_BLOCK_SIZE as u64) {
        return Err(EINVAL);
    }
    if len > RAMDISK_WINDOW_SIZE {
        return Err(EINVAL);
    }
    Ok(len / BDEV_BLOCK_SIZE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plausible_geometry_converts_to_a_block_count() {
        assert_eq!(validate_geometry(1024 * 1024), Ok(256));
        assert_eq!(validate_geometry(BDEV_BLOCK_SIZE as u64), Ok(1));
        assert_eq!(
            validate_geometry(RAMDISK_WINDOW_SIZE),
            Ok(RAMDISK_WINDOW_SIZE / BDEV_BLOCK_SIZE as u64)
        );
    }

    #[test]
    fn an_implausible_geometry_is_refused() {
        assert_eq!(validate_geometry(0), Err(EINVAL), "an empty device");
        assert_eq!(validate_geometry(1), Err(EINVAL), "a partial block");
        assert_eq!(
            validate_geometry(BDEV_BLOCK_SIZE as u64 + 1),
            Err(EINVAL),
            "a trailing partial block"
        );
        assert_eq!(
            validate_geometry(RAMDISK_WINDOW_SIZE + BDEV_BLOCK_SIZE as u64),
            Err(EINVAL),
            "larger than the window the kernel maps it into"
        );
        assert_eq!(validate_geometry(u64::MAX), Err(EINVAL));
    }
}

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `open(2)` flags (slice 5.10b).
//!
//! **The Linux/musl values**, for decision D7's reason applied to a second ABI:
//! musl's `open()` passes its own `O_CREAT` straight to the syscall, so matching
//! the numbers means the `__minixrs_syscall` shim will need no translation table
//! when it grows `openat`. Today's only client is `userland/init`, which is Rust
//! — the choice is forward-looking, and that is the honest framing, not a claim
//! that C uses it now.
//!
//! **Not emitted by `tools/gen-c-headers`**, exactly like the `AT_*` auxv values
//! slice 5.5 added: musl's own `fcntl.h` defines these, and a second definition
//! in `minixrs/*.h` would be a redefinition in any translation unit that included
//! both. The `const _`s below are what pin the values instead.

/// Mask selecting the access mode.
pub const O_ACCMODE: i32 = 0o3;
/// Access mode: read only.
pub const O_RDONLY: i32 = 0;
/// Access mode: write only.
pub const O_WRONLY: i32 = 1;
/// Access mode: read and write.
pub const O_RDWR: i32 = 2;

/// Create the file if it does not exist.
pub const O_CREAT: i32 = 0o100;
/// Discard the contents of a file that does exist.
pub const O_TRUNC: i32 = 0o1000;

/// Every bit this build reads. Any other bit in an `open` request is `EINVAL`.
///
/// The access mode is in here because it is **accepted and ignored**: there is no
/// uid, no gid and no permission check anywhere in the tree, so honouring it
/// would be a check with nothing behind it. `O_CREAT` and `O_TRUNC` are in here
/// because they are acted on.
pub const O_KNOWN: i32 = O_ACCMODE | O_CREAT | O_TRUNC;

/// A flag bit this build does **not** honour — what a denial probe aims at.
///
/// Derived from [`O_KNOWN`] rather than written as a literal, so that a flag
/// becoming real makes the probe using it fail loudly instead of passing
/// vacuously. That is slice 5.8's `VFS_WRITE + 1` lesson and slice 5.10a's
/// `write-file` lesson, applied before the fact rather than after.
///
/// `O_KNOWN + 1` sets at least one bit outside `O_KNOWN` (the carry stops at the
/// first clear bit), and the mask keeps only those — so the result is non-zero
/// and disjoint from `O_KNOWN` for any non-negative `O_KNOWN`.
pub const O_UNKNOWN_BIT: i32 = (O_KNOWN + 1) & !O_KNOWN;

// The values are Linux's, and that identity is the whole point — a literal test
// rather than a restatement of the definitions above.
const _: () = assert!(O_ACCMODE == 3);
const _: () = assert!(O_RDONLY == 0);
const _: () = assert!(O_WRONLY == 1);
const _: () = assert!(O_RDWR == 2);
const _: () = assert!(O_CREAT == 64);
const _: () = assert!(O_TRUNC == 512);

// The access mode and the behaviour bits must not overlap, or masking one would
// silently read the other.
const _: () = assert!(O_ACCMODE & (O_CREAT | O_TRUNC) == 0);
const _: () = assert!(O_CREAT & O_TRUNC == 0);

// The denial probe's bit really is outside what this build honours.
const _: () = assert!(O_UNKNOWN_BIT != 0);
const _: () = assert!(O_UNKNOWN_BIT & O_KNOWN == 0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_mode_masks_out_of_a_combined_flag_word() {
        // The one piece of arithmetic a caller performs on these.
        assert_eq!((O_RDWR | O_CREAT | O_TRUNC) & O_ACCMODE, O_RDWR);
        assert_eq!((O_WRONLY | O_TRUNC) & O_ACCMODE, O_WRONLY);
        assert_eq!(O_CREAT & O_ACCMODE, O_RDONLY);
    }

    #[test]
    fn every_honoured_bit_is_in_the_known_mask() {
        for flag in [O_ACCMODE, O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_TRUNC] {
            assert_eq!(flag & !O_KNOWN, 0, "flag {flag:o} is outside O_KNOWN");
        }
    }

    #[test]
    fn the_unknown_probe_bit_is_rejected_by_the_known_mask() {
        // What `open::validate_flags` will test it with.
        assert_ne!(O_UNKNOWN_BIT & !O_KNOWN, 0);
    }
}

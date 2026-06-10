// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! MINIX-style negative errno codes.
//!
//! Subset needed by Phase 2; more added as servers come online. Values match
//! MINIX 3 `include/minix/errno.h`. Returning a negative `m_type` is how an
//! IPC reply signals failure.

pub const OK: i32 = 0;
pub const EGENERIC: i32 = -1;
pub const EPERM: i32 = -2;
pub const ENOENT: i32 = -3;
pub const ESRCH: i32 = -4;
pub const EINTR: i32 = -5;
pub const EIO: i32 = -6;
pub const ENXIO: i32 = -7;
pub const E2BIG: i32 = -8;
pub const ENOEXEC: i32 = -9;
pub const EBADF: i32 = -10;
pub const ECHILD: i32 = -11;
pub const EAGAIN: i32 = -12;
pub const ENOMEM: i32 = -13;
pub const EACCES: i32 = -14;
pub const EFAULT: i32 = -15;
pub const EINVAL: i32 = -21;
pub const EDEADLK: i32 = -29;
pub const ENOSYS: i32 = -44;

// ---------------------------------------------------------------------------
// IPC-specific errors — raised by the kernel's IPC subsystem.
// ---------------------------------------------------------------------------

pub const ELOCKED: i32 = -101;
pub const EBADCALL: i32 = -102;
pub const EBADSRCDST: i32 = -103;
pub const ECALLDENIED: i32 = -104;
pub const EDEADSRCDST: i32 = -105;
pub const ENOTREADY: i32 = -106;
pub const EBADREQUEST: i32 = -107;
pub const ETRAPDENIED: i32 = -108;
pub const EDONTREPLY: i32 = -109;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_is_zero() {
        assert_eq!(OK, 0);
    }

    #[test]
    fn all_errors_are_negative() {
        for &e in &[
            EGENERIC,
            EPERM,
            ENOENT,
            ESRCH,
            EINTR,
            EIO,
            ENXIO,
            E2BIG,
            ENOEXEC,
            EBADF,
            ECHILD,
            EAGAIN,
            ENOMEM,
            EACCES,
            EFAULT,
            EINVAL,
            EDEADLK,
            ENOSYS,
            ELOCKED,
            EBADCALL,
            EBADSRCDST,
            ECALLDENIED,
            EDEADSRCDST,
            ENOTREADY,
            EBADREQUEST,
            ETRAPDENIED,
            EDONTREPLY,
        ] {
            assert!(e < 0, "errno {e} should be negative");
        }
    }

    #[test]
    fn ipc_errors_distinct_from_posix_errors() {
        // The IPC range starts at -101 and must not collide with POSIX errnos.
        for &posix in &[EGENERIC, EFAULT, EINVAL, ENOSYS] {
            for &ipc in &[ELOCKED, EBADSRCDST, EDEADSRCDST, EDONTREPLY] {
                assert_ne!(posix, ipc);
            }
        }
    }
}

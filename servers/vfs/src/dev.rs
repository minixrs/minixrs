// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The device-node table: three paths VFS answers itself, ahead of the mount
//! (slice 5.11, decision Z6).
//!
//! There is no `/dev` on the image and no device inode — the deliberate
//! simplification decision D11 names — so `open` matches these paths
//! **byte-for-byte** before it consults the filesystem, and a hit becomes a
//! [`Fd::CharDev`] naming the driver and minor. Everything else, `/dev/other`
//! included, falls through to MFS unchanged and answers whatever the FS path
//! answers (`ENOENT`, from the walk of a `/dev` that does not exist).
//!
//! Exact match is the whole contract. `/dev//null`, `/dev/./null`, a trailing
//! slash, a trailing NUL, a case variant — all misses, all MFS's problem. A real
//! `/dev` with inodes replaces this table; the `CharDriver` resolution stays.
//!
//! Pure and host-tested; the copy-in and the descriptor allocation are `main.rs`'s.

use minixrs_kernel_shared::callnr::{
    CDEV_MINOR_CONSOLE, CDEV_MINOR_NULL, CDEV_MINOR_ZERO, DEV_CONSOLE_PATH, DEV_NULL_PATH,
    DEV_ZERO_PATH,
};

use crate::fd::{CharDriver, Fd};

/// Rows in the table.
pub const NR_DEV_NODES: usize = 3;

/// The table: path, driver, minor. Paths come from `kernel-shared` so init's
/// probes cannot drift from what VFS matches.
static DEV_NODES: [(&str, CharDriver, i32); NR_DEV_NODES] = [
    (DEV_CONSOLE_PATH, CharDriver::Tty, CDEV_MINOR_CONSOLE),
    (DEV_NULL_PATH, CharDriver::Memory, CDEV_MINOR_NULL),
    (DEV_ZERO_PATH, CharDriver::Memory, CDEV_MINOR_ZERO),
];

/// Resolve `path` — the exact bytes VFS copied in, no terminator — against the
/// table.
pub fn lookup(path: &[u8]) -> Option<Fd> {
    DEV_NODES
        .iter()
        .find(|(p, _, _)| p.as_bytes() == path)
        .map(|&(_, dev, minor)| Fd::CharDev { dev, minor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_resolves_to_its_driver_and_minor() {
        assert_eq!(
            lookup(b"/dev/console"),
            Some(Fd::CharDev {
                dev: CharDriver::Tty,
                minor: CDEV_MINOR_CONSOLE,
            })
        );
        assert_eq!(
            lookup(b"/dev/null"),
            Some(Fd::CharDev {
                dev: CharDriver::Memory,
                minor: CDEV_MINOR_NULL,
            })
        );
        assert_eq!(
            lookup(b"/dev/zero"),
            Some(Fd::CharDev {
                dev: CharDriver::Memory,
                minor: CDEV_MINOR_ZERO,
            })
        );
    }

    #[test]
    fn anything_but_an_exact_match_misses() {
        // Each of these must reach MFS, not the table: a prefix, a suffix, a
        // doubled slash, a dot component, a case variant, a stray terminator, the
        // empty path, and the `/dev` directory itself.
        for path in [
            &b"/dev/nul"[..],
            b"/dev/null/",
            b"/dev/nullx",
            b"/dev//null",
            b"/dev/./null",
            b"/dev/NULL",
            b"/dev/null\0",
            b"dev/null",
            b"",
            b"/dev",
            b"/dev/",
            b"/dev/nope",
        ] {
            assert_eq!(lookup(path), None, "{:?}", core::str::from_utf8(path));
        }
    }

    #[test]
    fn the_table_has_no_duplicate_paths_and_uses_both_drivers() {
        for (i, (a, _, _)) in DEV_NODES.iter().enumerate() {
            for (b, _, _) in &DEV_NODES[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(DEV_NODES.iter().any(|(_, d, _)| *d == CharDriver::Tty));
        assert!(DEV_NODES.iter().any(|(_, d, _)| *d == CharDriver::Memory));
        assert_eq!(DEV_NODES.len(), NR_DEV_NODES);
    }
}

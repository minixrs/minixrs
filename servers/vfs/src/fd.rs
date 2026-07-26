// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The per-process file-descriptor table (slice 5.4).
//!
//! VFS's whole job is turning a small integer into a thing that can be written
//! to. This module is that mapping and nothing else: pure, total, and host-tested,
//! with the IPC glue in `main.rs` — the `servers/ds` `registry.rs` / `main.rs`
//! split.
//!
//! ## Every process starts with the same three descriptors
//!
//! There is no `open` yet (slice 5.8), so the table is *entirely* determined at
//! compile time: fds 0, 1, and 2 name the console character device in every row,
//! and nothing else is open. That is the POSIX convention — a process inherits
//! stdin/stdout/stderr rather than opening them — and it is what lets init write
//! a banner without a filesystem existing.
//!
//! Because nothing mutates, the storage is an ordinary **immutable `static`** and
//! this crate carries no `unsafe` at all. Slice 5.8's `open`/`close` flips it to
//! the `UnsafeCell<[FdRow; N]>` + `unsafe impl Sync` newtype that
//! `servers/vm/src/region.rs` and `servers/ds/src/registry.rs` already use, under
//! the same single-mutator invariant (VFS is one EL0 thread running a
//! straight-line receive loop, with no interrupt handlers of its own). Nothing
//! else about the module changes: [`resolve_in`] already takes the rows as a
//! borrowed slice precisely so it stays pure across that switch.

use minixrs_kernel_shared::callnr::CDEV_MINOR_CONSOLE;
use minixrs_kernel_shared::com::NR_SERVED_PROCS;
use minixrs_kernel_shared::error::EBADF;

/// Descriptors per process. Three are pre-opened (stdin/stdout/stderr) and the
/// fourth exists so "past the end of the row" is reachable in a test without
/// being the same case as "not open". VFS-local, deliberately **not** part of the
/// ABI: a client learns a descriptor is bad from `EBADF`, never from arithmetic
/// on a published constant. Slice 5.8 raises it.
pub const NR_FDS: usize = 4;

/// What a descriptor refers to.
///
/// One variant today. Slice 5.8's `open` adds the regular-file variant (an inode
/// on a mounted MFS plus a seek offset), at which point `do_write`'s `match` on
/// this enum is what routes a write to a driver or to the filesystem.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Fd {
    /// Not open. Every operation on it is `EBADF`.
    Unused,
    /// A character device — today only the console, via `CDEV_WRITE` to TTY.
    CharDev {
        /// Device minor, e.g. [`CDEV_MINOR_CONSOLE`]. Passed through to the
        /// driver, which is the one that decides whether it exists (`ENXIO`).
        minor: i32,
    },
}

/// One process's descriptors, indexed by descriptor number.
pub type FdRow = [Fd; NR_FDS];

/// The descriptors every process is born with: 0, 1, and 2 on the console.
const DEFAULT_ROW: FdRow = [
    Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    },
    Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    },
    Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    },
    Fd::Unused,
];

/// Rows in the table — one per process, indexed by **kernel proc number**.
///
/// Sized from [`NR_SERVED_PROCS`], the single shared ceiling PM's `mproc`, VM's
/// `ClientRegions`, and SCHED's policy table all derive from, so a process that
/// exists to one server is addressable in all of them. Never reintroduce an
/// independent capacity literal here: the guard below is what makes an
/// under-sized edit a compile error rather than a runtime `EBADF` that only the
/// highest-numbered processes ever see. Same shape as VM's `MAX_CLIENTS`.
const NR_FD_ROWS: usize = NR_SERVED_PROCS;

const _: () = assert!(NR_FD_ROWS >= NR_SERVED_PROCS);

/// The table itself. Immutable until slice 5.8's `open` — see the module note.
static ROWS: [FdRow; NR_FD_ROWS] = [DEFAULT_ROW; NR_FD_ROWS];

/// Resolve `(proc_nr, fd)` against a borrowed table.
///
/// The pure half — `rows` is passed in so this is testable without touching the
/// static, and so it survives slice 5.8's switch to interior-mutable storage
/// unchanged.
///
/// `EBADF` for every way of naming nothing: a negative descriptor, one past the
/// end of the row, a descriptor that is not open, and a proc number outside the
/// table. The last of those is the interesting one — it is how a message from a
/// process VFS has no row for (a proc number beyond `NR_SERVED_PROCS`, or the
/// negative number of a kernel task) fails closed rather than indexing wild.
pub fn resolve_in(rows: &[FdRow], proc_nr: i32, fd: i32) -> Result<Fd, i32> {
    let Ok(slot) = usize::try_from(proc_nr) else {
        return Err(EBADF);
    };
    let Ok(idx) = usize::try_from(fd) else {
        return Err(EBADF);
    };
    match rows.get(slot).and_then(|row| row.get(idx)) {
        Some(Fd::Unused) | None => Err(EBADF),
        Some(open) => Ok(*open),
    }
}

/// Resolve `(proc_nr, fd)` against the live table. The thin wrapper the receive
/// loop calls; all the logic is in [`resolve_in`].
pub fn resolve(proc_nr: i32, fd: i32) -> Result<Fd, i32> {
    resolve_in(&ROWS, proc_nr, fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONSOLE: Fd = Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    };

    #[test]
    fn stdin_stdout_and_stderr_are_the_console_for_every_process() {
        // The pre-open contract, checked across the whole proc-number range
        // rather than for one process: init, a boot server, and a forked child
        // must all find fd 1 without anything having opened it.
        for proc_nr in 0..NR_SERVED_PROCS as i32 {
            for fd in 0..3 {
                assert_eq!(resolve(proc_nr, fd), Ok(CONSOLE), "proc {proc_nr} fd {fd}");
            }
        }
    }

    #[test]
    fn every_row_is_identical_today() {
        // Slice 5.8's `open` is what makes rows diverge. Stating the invariant
        // now means that change shows up here as a deliberate edit rather than
        // silently invalidating the loop above.
        for row in ROWS.iter() {
            assert_eq!(*row, DEFAULT_ROW);
        }
    }

    #[test]
    fn an_unopened_descriptor_is_ebadf() {
        // fd 3 exists in the row but holds `Unused` — the case that proves
        // "in range" and "open" are separate questions.
        assert_eq!(ROWS[0][3], Fd::Unused);
        assert_eq!(resolve(0, 3), Err(EBADF));
    }

    #[test]
    fn a_descriptor_past_the_end_of_the_row_is_ebadf() {
        for fd in [NR_FDS as i32, NR_FDS as i32 + 1, 64, i32::MAX] {
            assert_eq!(resolve(0, fd), Err(EBADF), "fd {fd}");
        }
    }

    #[test]
    fn a_negative_descriptor_is_ebadf() {
        // A client that passed through an errno by mistake, or a wrapped count.
        for fd in [-1i32, -3, i32::MIN] {
            assert_eq!(resolve(0, fd), Err(EBADF), "fd {fd}");
        }
    }

    #[test]
    fn a_proc_number_outside_the_table_is_ebadf() {
        // Above the ceiling, and the negative numbers kernel tasks carry —
        // neither may index the table, and neither may panic.
        for proc_nr in [
            NR_SERVED_PROCS as i32,
            NR_SERVED_PROCS as i32 + 1,
            -1,
            -2,
            i32::MIN,
            i32::MAX,
        ] {
            assert_eq!(resolve(proc_nr, 1), Err(EBADF), "proc {proc_nr}");
        }
    }

    #[test]
    fn resolve_in_reads_the_row_it_is_given() {
        // The pure helper must not reach the static: hand it a table whose fd 1
        // is closed and whose fd 3 is open, the opposite of the real one.
        let mut rows = [DEFAULT_ROW; 2];
        rows[1][1] = Fd::Unused;
        rows[1][3] = Fd::CharDev { minor: 7 };

        assert_eq!(resolve_in(&rows, 0, 1), Ok(CONSOLE));
        assert_eq!(resolve_in(&rows, 1, 1), Err(EBADF));
        assert_eq!(resolve_in(&rows, 1, 3), Ok(Fd::CharDev { minor: 7 }));
        // ...and the static is unchanged, so the two really are independent.
        assert_eq!(resolve(1, 1), Ok(CONSOLE));
    }

    #[test]
    fn an_empty_table_resolves_nothing() {
        // Degenerate borrow: no rows at all. `resolve_in` must answer `EBADF`
        // rather than index into a zero-length slice.
        assert_eq!(resolve_in(&[], 0, 1), Err(EBADF));
    }
}

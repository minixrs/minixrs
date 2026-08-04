// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The per-process file-descriptor table (slice 5.4, mutable since 5.8).
//!
//! VFS's whole job is turning a small integer into a thing that can be read from
//! or written to. This module is that mapping and nothing else: pure, total, and
//! host-tested, with the IPC glue in `main.rs` — the `servers/ds` `registry.rs` /
//! `main.rs` split.
//!
//! ## Every process starts with the same three descriptors
//!
//! fds 0, 1, and 2 name the console character device in every row. That is the
//! POSIX convention — a process inherits stdin/stdout/stderr rather than opening
//! them — and it is what lets init write a banner before a filesystem exists.
//! Everything above them starts closed and is handed out by [`alloc_in`].
//!
//! ## The storage became interior-mutable, and that is the hazard
//!
//! Until slice 5.8 nothing mutated, so the table was an ordinary immutable
//! `static` and this crate carried no `unsafe` at all. `open`/`close` changed
//! that: it is now an `UnsafeCell<[FdRow; N]>` + `unsafe impl Sync` newtype, the
//! shape `servers/vm/src/region.rs` and `servers/ds/src/registry.rs` already use,
//! under the same single-mutator invariant (VFS is one EL0 thread running a
//! straight-line receive loop, with no interrupt handlers of its own).
//!
//! **The rule that comes with it: never hold a borrow of the table across a
//! SENDREC.** A handler resolves a descriptor, sends a request to MFS or TTY —
//! which blocks, and during which nothing else touches the table, but the borrow
//! would still be live across a call that can reach `alloc_in`/`close_in` in a
//! later refactor — and then updates the position. [`Fd`] is `Copy` precisely so
//! that is easy to obey: the borrow dies at the destructuring `let`, and the
//! handler carries values.
//!
//! This is not a theoretical concern. CLAUDE.md records aliasing-through-an
//! -`UnsafeCell`-newtype as a class of bug this repo has *shipped* (slice 5.3's
//! `free_frame` / `is_usable_pa`), caught in review rather than by a test. The
//! pure `*_in` helpers below all take the rows as a borrowed slice, which is what
//! keeps every decision testable without touching the static at all.
//!
//! **The same rule binds the tests, where it is not even single-threaded.**
//! libtest runs `#[test]` functions on several threads at once, so two tests that
//! both name the static really can hold `&` and `&mut` to it simultaneously —
//! aliasing UB under a `cargo test` that no assertion would notice. Exactly one
//! test below therefore names it ([`tests::the_live_table_round_trips_through_its_wrappers`],
//! which exists to cover the four `unsafe` wrappers); everything else runs against
//! the local fixture, `servers/vm/src/region.rs`'s arrangement.
//!
//! ## What this table still does not know about
//!
//! **Nothing resets a row when a process exits, and proc numbers are recycled.**
//! Rows became mutable in slice 5.8, so a [`Fd::File`] entry outlives the process
//! that opened it: PM hands the fork pool's proc numbers back out every init
//! cycle, and VFS is told nothing about exit (`signal_handler: None`, and no
//! PM→VFS request exists). Today only init opens files, so no row is ever
//! inherited — but the first forked child that calls `open` would leave a live
//! descriptor onto an arbitrary inode for whatever process lands on that slot
//! next. **`fork` also does not duplicate the parent's descriptors**, which POSIX
//! requires.
//!
//! **Slice 5.9 was predicted to force this and did not.** exec-from-FS resolves a
//! binary by *inode*, through [`VFS_EXEC_STAGE`], with PM staging on the child's
//! behalf — so no forked child ever holds a descriptor and no row is inherited by
//! anything. The debt is real and still deferred; what changed is that it now has
//! no scheduled forcing slice, and closing it needs a PM→VFS exit notification the
//! ABI does not have. The first thing that will force it is a program that opens a
//! file *after* being exec'd.
//!
//! [`VFS_EXEC_STAGE`]: minixrs_kernel_shared::callnr::VFS_EXEC_STAGE

use minixrs_kernel_shared::callnr::CDEV_MINOR_CONSOLE;
use minixrs_kernel_shared::com::NR_SERVED_PROCS;
use minixrs_kernel_shared::error::{EBADF, EMFILE};

use core::cell::UnsafeCell;

/// Descriptors per process.
///
/// Three are pre-opened (stdin/stdout/stderr), which leaves five for `open` to
/// hand out. Five rather than one is what makes "the lowest free descriptor" and
/// "the row is full" *separate* things a test can tell apart: with `NR_FDS = 4`
/// the first `open` would also be the last, and an allocator that scanned from
/// the wrong end would be indistinguishable from one that scanned correctly.
///
/// VFS-local, deliberately **not** part of the ABI: a client learns a descriptor
/// is bad from `EBADF` and that the table is full from `EMFILE`, never from
/// arithmetic on a published constant.
pub const NR_FDS: usize = 8;

/// What a descriptor refers to.
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
    /// A regular file on the mounted filesystem.
    File {
        /// The inode, as MFS handed it out from an `FS_LOOKUP`.
        ino: u32,
        /// The seek position. Advanced by [`advance_in`] after each read.
        pos: u64,
    },
}

/// One process's descriptors, indexed by descriptor number.
pub type FdRow = [Fd; NR_FDS];

/// The descriptors every process is born with: 0, 1, and 2 on the console.
const DEFAULT_ROW: FdRow = {
    let mut row = [Fd::Unused; NR_FDS];
    row[0] = Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    };
    row[1] = Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    };
    row[2] = Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    };
    row
};

/// The first descriptor `open` may hand out. Everything below is pre-opened.
pub const FIRST_FREE_FD: usize = 3;

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
const _: () = assert!(NR_FDS > FIRST_FREE_FD);

/// `UnsafeCell`-wrapped static table. See the module note for the single-mutator
/// invariant and the borrow rule that comes with it.
#[repr(transparent)]
struct Table(UnsafeCell<[FdRow; NR_FD_ROWS]>);

// SAFETY: VFS is a single EL0 thread running a straight-line receive loop with no
// interrupt handlers of its own, so there is never a second accessor. Every path
// to the table goes through the four wrappers below, each of which takes its
// borrow and releases it within one call. Under `cargo test` "single thread" is
// not free: libtest is multi-threaded, so the invariant is upheld there by exactly
// one test naming the static — see the module note.
unsafe impl Sync for Table {}

static ROWS: Table = Table(UnsafeCell::new([DEFAULT_ROW; NR_FD_ROWS]));

/// Resolve `(proc_nr, fd)` against a borrowed table.
///
/// The pure half — `rows` is passed in so this is testable without touching the
/// static, which is what let it survive slice 5.8's switch to interior-mutable
/// storage completely unchanged.
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

/// Install `entry` at the **lowest free descriptor** of `proc_nr`'s row and
/// return its number.
///
/// Lowest-free is POSIX's rule, and it is the one thing about `open` a client can
/// observe without reading the file: close a descriptor and the next `open`
/// reuses that number. It is also why the search must run *upwards* — a
/// `.rposition()` would hand out `NR_FDS - 1` first, pass every "did I get a
/// descriptor" check, and silently break the contract.
///
/// `EMFILE` when the row is full, `EBADF` for a proc number outside the table.
/// The scan starts at [`FIRST_FREE_FD`] rather than 0: 0/1/2 are pre-opened and
/// nothing ever closes them, so starting lower would only waste three
/// comparisons — but stating it means a future `close(1)` cannot silently make
/// `open` hand out a standard descriptor.
pub fn alloc_in(rows: &mut [FdRow], proc_nr: i32, entry: Fd) -> Result<i32, i32> {
    let Ok(slot) = usize::try_from(proc_nr) else {
        return Err(EBADF);
    };
    let row = rows.get_mut(slot).ok_or(EBADF)?;
    for (fd, cell) in row.iter_mut().enumerate().skip(FIRST_FREE_FD) {
        if *cell == Fd::Unused {
            *cell = entry;
            // `NR_FDS` is 8, so this cannot exceed `i32::MAX`; the guard below is
            // for the day it is raised from a shared constant.
            return i32::try_from(fd).map_err(|_| EMFILE);
        }
    }
    Err(EMFILE)
}

/// Close `fd` in `proc_nr`'s row.
///
/// `EBADF` for a descriptor that was not open — closing twice is an error, not a
/// no-op, which is what makes the `fs.fd` boot probe's re-open meaningful.
pub fn close_in(rows: &mut [FdRow], proc_nr: i32, fd: i32) -> Result<(), i32> {
    let (Ok(slot), Ok(idx)) = (usize::try_from(proc_nr), usize::try_from(fd)) else {
        return Err(EBADF);
    };
    let cell = rows
        .get_mut(slot)
        .and_then(|row| row.get_mut(idx))
        .ok_or(EBADF)?;
    if *cell == Fd::Unused {
        return Err(EBADF);
    }
    *cell = Fd::Unused;
    Ok(())
}

/// Advance a file descriptor's position by `n` bytes.
///
/// A no-op for anything that is not a [`Fd::File`] — a character device has no
/// position — and `saturating_add`, because a position that wrapped would send
/// the next read back to the start of the file rather than reporting EOF.
///
/// Silently ignoring a bad descriptor is right here and only here: this is called
/// *after* a successful transfer, so the descriptor was resolved a moment ago and
/// there is nothing left to tell the caller. Every path that can refuse has
/// already refused.
pub fn advance_in(rows: &mut [FdRow], proc_nr: i32, fd: i32, n: u64) {
    let (Ok(slot), Ok(idx)) = (usize::try_from(proc_nr), usize::try_from(fd)) else {
        return;
    };
    if let Some(Fd::File { pos, .. }) = rows.get_mut(slot).and_then(|row| row.get_mut(idx)) {
        *pos = pos.saturating_add(n);
    }
}

/// Resolve `(proc_nr, fd)` against the live table.
pub fn resolve(proc_nr: i32, fd: i32) -> Result<Fd, i32> {
    // SAFETY: single-mutator invariant (see the module note). The borrow is taken
    // and released entirely within this call, so it cannot be held across a
    // SENDREC — `Fd` is `Copy`, so the value the caller keeps borrows nothing.
    resolve_in(unsafe { &*ROWS.0.get() }, proc_nr, fd)
}

/// Install `entry` at the lowest free descriptor of the live table.
pub fn alloc(proc_nr: i32, entry: Fd) -> Result<i32, i32> {
    // SAFETY: as `resolve` — one mutator, and the borrow does not escape.
    alloc_in(unsafe { &mut *ROWS.0.get() }, proc_nr, entry)
}

/// Close `fd` in the live table.
pub fn close(proc_nr: i32, fd: i32) -> Result<(), i32> {
    // SAFETY: as `resolve` — one mutator, and the borrow does not escape.
    close_in(unsafe { &mut *ROWS.0.get() }, proc_nr, fd)
}

/// Advance a descriptor's position in the live table.
pub fn advance(proc_nr: i32, fd: i32, n: u64) {
    // SAFETY: as `resolve` — one mutator, and the borrow does not escape.
    advance_in(unsafe { &mut *ROWS.0.get() }, proc_nr, fd, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONSOLE: Fd = Fd::CharDev {
        minor: CDEV_MINOR_CONSOLE,
    };
    const FILE: Fd = Fd::File { ino: 5, pos: 0 };

    /// A private table, built from the same expression as the live static, so no
    /// test has to name that static to say something about it.
    ///
    /// Full-size rather than a two-row stub: the pre-open contract below is about
    /// the whole proc-number range, and a fixture narrower than the real table
    /// could not state it. Every test here works on its own copy — libtest is
    /// multi-threaded, so sharing the static between a reader and a writer would
    /// be aliasing UB (see the module note).
    fn rows() -> [FdRow; NR_FD_ROWS] {
        [DEFAULT_ROW; NR_FD_ROWS]
    }

    #[test]
    fn stdin_stdout_and_stderr_are_the_console_for_every_process() {
        // The pre-open contract, checked across the whole proc-number range
        // rather than for one process: init, a boot server, and a forked child
        // must all find fd 1 without anything having opened it.
        let r = rows();
        for proc_nr in 0..NR_SERVED_PROCS as i32 {
            for fd in 0..3 {
                assert_eq!(
                    resolve_in(&r, proc_nr, fd),
                    Ok(CONSOLE),
                    "proc {proc_nr} fd {fd}"
                );
            }
        }
    }

    #[test]
    fn every_row_starts_identical_and_open_is_what_makes_them_diverge() {
        // Slice 5.4's `every_row_is_identical_today`, which slice 5.8 was always
        // going to break: the rows now *can* diverge. What is still true — and
        // what the loop above depends on — is that they start identical, so this
        // states the invariant that survived rather than deleting the test.
        let mut r = rows();
        assert_eq!(r[0], r[1]);
        assert_eq!(r[0], DEFAULT_ROW);

        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(3));
        assert_ne!(r[0], r[1], "open must affect only the caller's row");
        assert_eq!(r[1], DEFAULT_ROW);
    }

    #[test]
    fn an_unopened_descriptor_is_ebadf() {
        // fd 3 exists in the row but holds `Unused` — the case that proves
        // "in range" and "open" are separate questions.
        assert_eq!(resolve_in(&rows(), 0, 3), Err(EBADF));
    }

    #[test]
    fn a_descriptor_past_the_end_of_the_row_is_ebadf() {
        let r = rows();
        for fd in [NR_FDS as i32, NR_FDS as i32 + 1, 64, i32::MAX] {
            assert_eq!(resolve_in(&r, 0, fd), Err(EBADF), "fd {fd}");
        }
    }

    #[test]
    fn a_negative_descriptor_is_ebadf() {
        // A client that passed through an errno by mistake, or a wrapped count.
        let r = rows();
        for fd in [-1i32, -3, i32::MIN] {
            assert_eq!(resolve_in(&r, 0, fd), Err(EBADF), "fd {fd}");
        }
    }

    #[test]
    fn a_proc_number_outside_the_table_is_ebadf() {
        // Above the ceiling, and the negative numbers kernel tasks carry —
        // neither may index the table, and neither may panic. Every entry point,
        // not just `resolve`: a bad proc number reaching `alloc` or `close` would
        // otherwise index wild.
        for proc_nr in [
            NR_SERVED_PROCS as i32,
            NR_SERVED_PROCS as i32 + 1,
            -1,
            -2,
            i32::MIN,
            i32::MAX,
        ] {
            let mut r = rows();
            assert_eq!(resolve_in(&r, proc_nr, 1), Err(EBADF), "proc {proc_nr}");
            assert_eq!(alloc_in(&mut r, proc_nr, FILE), Err(EBADF), "{proc_nr}");
            assert_eq!(close_in(&mut r, proc_nr, 1), Err(EBADF), "{proc_nr}");
            // `advance_in` has no way to report, so the assertion is that it
            // changes nothing rather than panicking.
            advance_in(&mut r, proc_nr, 1, 8);
            assert_eq!(r, rows());
        }
    }

    #[test]
    fn resolve_in_reads_the_row_it_is_given() {
        // The pure helper must not reach the static: hand it a table whose fd 1
        // is closed and whose fd 3 is open, the opposite of the real one. The two
        // `Err`/`Ok` answers below are the proof on their own — the live table's
        // fd 1 is open and its fd 3 is not, so a helper that consulted the static
        // could not produce either. (It used to be stated by reading the static
        // back as well; that is the borrow this test must not take, since a
        // sibling test holds `&mut` to it on another libtest thread.)
        let mut r = rows();
        r[1][1] = Fd::Unused;
        r[1][3] = Fd::CharDev { minor: 7 };

        assert_eq!(resolve_in(&r, 0, 1), Ok(CONSOLE));
        assert_eq!(resolve_in(&r, 1, 1), Err(EBADF));
        assert_eq!(resolve_in(&r, 1, 3), Ok(Fd::CharDev { minor: 7 }));
        // ...and no other row moved, so the write really was row-local.
        assert_eq!(r[0], DEFAULT_ROW);
    }

    #[test]
    fn an_empty_table_resolves_nothing() {
        // Degenerate borrow: no rows at all. `resolve_in` must answer `EBADF`
        // rather than index into a zero-length slice.
        assert_eq!(resolve_in(&[], 0, 1), Err(EBADF));
        assert_eq!(alloc_in(&mut [], 0, FILE), Err(EBADF));
        assert_eq!(close_in(&mut [], 0, 1), Err(EBADF));
    }

    #[test]
    fn open_hands_out_the_lowest_free_descriptor() {
        // POSIX's rule, and the only thing about `open` a client can observe
        // without reading the file. A `.rposition()` scan would return 7 here and
        // pass every "did I get a descriptor" check.
        let mut r = rows();
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(3));
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(4));
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(5));
    }

    #[test]
    fn a_closed_descriptor_is_reused_by_the_next_open() {
        // The round trip the `fs.fd` boot probe drives: open, open, close both,
        // re-open must land back on 3.
        let mut r = rows();
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(3));
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(4));
        assert_eq!(close_in(&mut r, 0, 3), Ok(()));
        assert_eq!(close_in(&mut r, 0, 4), Ok(()));
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(3));
        // ...and with 3 taken again, the next is 4 — so `close` really freed both
        // rather than leaving a hole the scan skipped.
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(4));
    }

    #[test]
    fn open_never_hands_out_a_standard_descriptor() {
        // Even with the row otherwise empty: 0/1/2 are pre-opened, and the scan
        // starts above them.
        let mut r = rows();
        for _ in FIRST_FREE_FD..NR_FDS {
            let fd = alloc_in(&mut r, 0, FILE).unwrap();
            assert!(fd >= FIRST_FREE_FD as i32, "open returned fd {fd}");
        }
    }

    #[test]
    fn a_full_row_is_emfile_not_a_silent_overwrite() {
        // `EMFILE` rather than `EBADF`: "you have too many files open" is a
        // different thing to tell a caller than "that descriptor is nonsense",
        // and overwriting an open descriptor would lose a file silently.
        let mut r = rows();
        for _ in FIRST_FREE_FD..NR_FDS {
            assert!(alloc_in(&mut r, 0, FILE).is_ok());
        }
        assert_eq!(alloc_in(&mut r, 0, FILE), Err(EMFILE));
        // The other row is untouched, so "full" is per process.
        assert_eq!(alloc_in(&mut r, 1, FILE), Ok(3));
    }

    #[test]
    fn closing_an_unopened_descriptor_is_ebadf() {
        // Closing twice is an error, not a no-op.
        let mut r = rows();
        assert_eq!(close_in(&mut r, 0, 3), Err(EBADF));
        assert_eq!(alloc_in(&mut r, 0, FILE), Ok(3));
        assert_eq!(close_in(&mut r, 0, 3), Ok(()));
        assert_eq!(close_in(&mut r, 0, 3), Err(EBADF));
        for fd in [NR_FDS as i32, -1, i32::MAX] {
            assert_eq!(close_in(&mut r, 0, fd), Err(EBADF), "fd {fd}");
        }
    }

    #[test]
    fn advance_moves_a_files_position_and_nothing_else() {
        let mut r = rows();
        assert_eq!(alloc_in(&mut r, 0, Fd::File { ino: 5, pos: 0 }), Ok(3));
        advance_in(&mut r, 0, 3, 31);
        assert_eq!(resolve_in(&r, 0, 3), Ok(Fd::File { ino: 5, pos: 31 }));
        advance_in(&mut r, 0, 3, 9);
        assert_eq!(resolve_in(&r, 0, 3), Ok(Fd::File { ino: 5, pos: 40 }));
        // The inode is not touched, and the other descriptors are not either.
        assert_eq!(resolve_in(&r, 0, 1), Ok(CONSOLE));
    }

    #[test]
    fn advance_is_a_no_op_on_a_character_device_or_a_closed_descriptor() {
        // A console has no position. Advancing one must not silently turn it into
        // a file, and advancing a closed descriptor must not open one.
        let mut r = rows();
        advance_in(&mut r, 0, 1, 100);
        assert_eq!(resolve_in(&r, 0, 1), Ok(CONSOLE));
        advance_in(&mut r, 0, 3, 100);
        assert_eq!(resolve_in(&r, 0, 3), Err(EBADF));
    }

    #[test]
    fn a_position_saturates_rather_than_wrapping() {
        // A wrapped position would send the next read back to the start of the
        // file instead of reporting EOF — silently returning the wrong bytes,
        // which is the failure mode a filesystem must not have.
        let mut r = rows();
        assert_eq!(
            alloc_in(
                &mut r,
                0,
                Fd::File {
                    ino: 5,
                    pos: u64::MAX - 3
                }
            ),
            Ok(3)
        );
        advance_in(&mut r, 0, 3, 100);
        assert_eq!(
            resolve_in(&r, 0, 3),
            Ok(Fd::File {
                ino: 5,
                pos: u64::MAX
            })
        );
    }

    #[test]
    fn the_live_table_round_trips_through_its_wrappers() {
        // The four `unsafe` wrappers, exercised once each against the real static
        // — the only thing here that is not covered by the pure helpers, and for
        // that reason **the only test in this module that names `ROWS`**. It holds
        // `&mut` to it; libtest would run any other test touching the static
        // concurrently, and `&`/`&mut` at once is UB no assertion here would see.
        // Uses a high proc number so a future second accessor collides loudly
        // rather than silently sharing proc 0's row.
        let p = NR_SERVED_PROCS as i32 - 1;
        let fd = alloc(p, Fd::File { ino: 9, pos: 0 }).expect("a free descriptor");
        assert_eq!(resolve(p, fd), Ok(Fd::File { ino: 9, pos: 0 }));
        advance(p, fd, 7);
        assert_eq!(resolve(p, fd), Ok(Fd::File { ino: 9, pos: 7 }));
        assert_eq!(close(p, fd), Ok(()));
        assert_eq!(resolve(p, fd), Err(EBADF));
    }
}

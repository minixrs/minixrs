// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! MINIX-style negative errno codes.
//!
//! Two bands, per phase-5 decision D7 (`docs/plans/phase-5-musl-fs.md`):
//!
//! 1. **POSIX block, magnitudes `1..=40`** — classic book-era MINIX numbering,
//!    which is identical to Linux/musl's `asm-generic` numbering across the
//!    whole contiguous range. Adopting it verbatim is what lets musl's stock
//!    `bits/errno.h` and `syscall_ret.c`'s `r > -4096UL` convention work
//!    unmodified. Where classic MINIX and Linux ever diverge, musl's value wins.
//! 2. **MINIX-specific IPC band, magnitudes `>= 200`** — modern MINIX 3's
//!    `sys/sys/errno.h` values. Linux's errno range never reaches 200, so these
//!    can never collide with a musl-visible errno.
//!
//! Nothing may land in the `41..=199` gap: that is where Linux/musl defines
//! errnos minix.rs has *not* adopted, so a value there could silently mean two
//! different things once C links against musl. The [`ALL`] table plus the
//! `const _` guards below enforce that at compile time.
//!
//! Values are stored **negated** (`EPERM == -1`): returning a negative `m_type`
//! is how an IPC reply signals failure. [`OK`] is success and is deliberately
//! not part of [`ALL`].
//!
//! Reserved-but-unadopted modern-MINIX values in the 200 band, recorded so no
//! one re-uses the numbers for something else: `ERESTART` 200, `EPACKSIZE` 205,
//! `EURG` 206, `ENOURG` 207, `EBADMODE` 213, `ENOCONN` 214, `EDEADEPT` 215,
//! `EBADCPU` 217.

/// Success. Not an errno, and deliberately absent from [`ALL`].
pub const OK: i32 = 0;

/// Highest POSIX errno magnitude minix.rs defines.
///
/// musl and classic MINIX agree on every value in `1..=40`, so the whole
/// contiguous block is adopted rather than a subset — no later slice has to
/// come back and add "one more errno".
pub const POSIX_ERRNO_MAX: i32 = 40;

/// First magnitude of the MINIX-specific IPC band (modern MINIX 3
/// `sys/sys/errno.h`), chosen clear of Linux's entire errno range.
pub const MINIX_ERRNO_BASE: i32 = 200;

/// Define an errno band from its **positive** magnitude, emitting the negated
/// `pub const` plus the derived [`ALL`] name/value table.
///
/// One line per errno is the whole edit surface: the table, the compile-time
/// band guards, `tools/gen-c-headers`, and the host tests all derive from it,
/// so they cannot fall out of sync with the constants.
macro_rules! errnos {
    ($(
        $(#[$meta:meta])*
        $name:ident = $magnitude:literal,
    )*) => {
        $(
            $(#[$meta])*
            pub const $name: i32 = -$magnitude;
        )*

        /// Every errno in this module as `(name, value)` pairs, in declaration
        /// order: the POSIX block first, then the MINIX-specific band.
        ///
        /// The single source of truth for `tools/gen-c-headers`, the band
        /// guards below, and the host tests. A `const` rather than a `static`
        /// so the bare-metal kernel — which never references it — pays nothing
        /// for it in `.rodata`.
        pub const ALL: &[(&str, i32)] = &[
            $( (stringify!($name), $name), )*
        ];
    };
}

errnos! {
    // -- POSIX block: magnitudes 1..=40, identical in classic MINIX and musl --
    /// Operation not permitted.
    EPERM = 1,
    /// No such file or directory.
    ENOENT = 2,
    /// No such process.
    ESRCH = 3,
    /// Interrupted system call.
    EINTR = 4,
    /// Input/output error.
    EIO = 5,
    /// No such device or address.
    ENXIO = 6,
    /// Argument list too long.
    E2BIG = 7,
    /// Exec format error.
    ENOEXEC = 8,
    /// Bad file descriptor.
    EBADF = 9,
    /// No child processes.
    ECHILD = 10,
    /// Resource temporarily unavailable. musl aliases `EWOULDBLOCK` to this.
    EAGAIN = 11,
    /// Cannot allocate memory.
    ENOMEM = 12,
    /// Permission denied.
    EACCES = 13,
    /// Bad address.
    EFAULT = 14,
    /// Block device required.
    ENOTBLK = 15,
    /// Device or resource busy.
    EBUSY = 16,
    /// File exists.
    EEXIST = 17,
    /// Invalid cross-device link.
    EXDEV = 18,
    /// No such device.
    ENODEV = 19,
    /// Not a directory.
    ENOTDIR = 20,
    /// Is a directory.
    EISDIR = 21,
    /// Invalid argument.
    EINVAL = 22,
    /// Too many open files in system.
    ENFILE = 23,
    /// Too many open files.
    EMFILE = 24,
    /// Inappropriate ioctl for device.
    ENOTTY = 25,
    /// Text file busy.
    ETXTBSY = 26,
    /// File too large.
    EFBIG = 27,
    /// No space left on device.
    ENOSPC = 28,
    /// Illegal seek.
    ESPIPE = 29,
    /// Read-only file system.
    EROFS = 30,
    /// Too many links.
    EMLINK = 31,
    /// Broken pipe.
    EPIPE = 32,
    /// Numerical argument out of domain.
    EDOM = 33,
    /// Numerical result out of range.
    ERANGE = 34,
    /// Resource deadlock avoided. musl aliases `EDEADLOCK` to this.
    EDEADLK = 35,
    /// File name too long.
    ENAMETOOLONG = 36,
    /// No locks available.
    ENOLCK = 37,
    /// Function not implemented.
    ENOSYS = 38,
    /// Directory not empty.
    ENOTEMPTY = 39,
    /// Too many levels of symbolic links.
    ELOOP = 40,

    // -- MINIX-specific IPC band: modern MINIX 3 `sys/sys/errno.h` values ----
    /// Destination is not ready to receive (`SENDNB`).
    ENOTREADY = 201,
    /// Source or destination is dead, or its endpoint generation is stale.
    EDEADSRCDST = 202,
    /// Pseudo-code: the handler produced no reply.
    EDONTREPLY = 203,
    /// Generic MINIX error.
    EGENERIC = 204,
    /// Cannot send: doing so would deadlock.
    ELOCKED = 208,
    /// Illegal IPC primitive number.
    EBADCALL = 209,
    /// No permission for this system call or destination.
    ECALLDENIED = 210,
    /// The caller's trap mask forbids this IPC primitive.
    ETRAPDENIED = 211,
    /// Destination cannot handle this request number.
    EBADREQUEST = 212,
    /// Bad source or destination endpoint.
    ///
    /// Modern MINIX 3 spells this same condition `EBADEPT`; minix.rs keeps the
    /// classic MINIX name at the modern value, so the number is ABI-compatible
    /// with the reference tree either way.
    EBADSRCDST = 216,
}

// D7's core invariant, enforced at compile time: every errno is negative, the
// POSIX block covers 1..=POSIX_ERRNO_MAX exactly once with no holes, and every
// other magnitude sits at or above MINIX_ERRNO_BASE — nothing in the 41..=199
// gap reserved for the musl errnos minix.rs has not adopted.
//
// `while` + indexing rather than iterators (not const-evaluable), and literal
// assert messages (const panics cannot take format arguments).
const _: () = {
    let mut seen = [false; POSIX_ERRNO_MAX as usize + 1];
    let mut i = 0;
    while i < ALL.len() {
        let mag = -ALL[i].1;
        assert!(mag > 0, "errno constants must be stored negated");
        if mag <= POSIX_ERRNO_MAX {
            assert!(!seen[mag as usize], "duplicate POSIX errno magnitude");
            seen[mag as usize] = true;
        } else {
            assert!(
                mag >= MINIX_ERRNO_BASE,
                "errno magnitude lands in the 41..=199 gap reserved for musl"
            );
        }
        i += 1;
    }
    let mut mag = 1;
    while mag <= POSIX_ERRNO_MAX {
        assert!(
            seen[mag as usize],
            "the POSIX errno block 1..=40 has a hole"
        );
        mag += 1;
    }
};

// Values must be pairwise distinct: two names sharing a value make an IPC reply
// ambiguous, and would emit two colliding `#define`s into the C headers.
const _: () = {
    let mut i = 0;
    while i < ALL.len() {
        let mut j = i + 1;
        while j < ALL.len() {
            assert!(ALL[i].1 != ALL[j].1, "duplicate errno value");
            j += 1;
        }
        i += 1;
    }
};

// OK is the success sentinel and must stay 0: several sites still spell the
// success check `sef.receive(&mut msg) != 0` rather than `!= OK`.
const _: () = assert!(OK == 0);
const _: () = assert!(MINIX_ERRNO_BASE > POSIX_ERRNO_MAX);

#[cfg(test)]
mod tests {
    use super::*;

    /// musl `arch/generic/bits/errno.h` — the asm-generic numbering shared by
    /// aarch64 and x86_64, which is also classic book-era MINIX's.
    ///
    /// Pinned literally rather than derived: this table is the drift tripwire
    /// for the whole C bridge, so changing an adopted value has to be a
    /// deliberate two-place edit.
    const MUSL_POSIX: [(&str, i32); 40] = [
        ("EPERM", 1),
        ("ENOENT", 2),
        ("ESRCH", 3),
        ("EINTR", 4),
        ("EIO", 5),
        ("ENXIO", 6),
        ("E2BIG", 7),
        ("ENOEXEC", 8),
        ("EBADF", 9),
        ("ECHILD", 10),
        ("EAGAIN", 11),
        ("ENOMEM", 12),
        ("EACCES", 13),
        ("EFAULT", 14),
        ("ENOTBLK", 15),
        ("EBUSY", 16),
        ("EEXIST", 17),
        ("EXDEV", 18),
        ("ENODEV", 19),
        ("ENOTDIR", 20),
        ("EISDIR", 21),
        ("EINVAL", 22),
        ("ENFILE", 23),
        ("EMFILE", 24),
        ("ENOTTY", 25),
        ("ETXTBSY", 26),
        ("EFBIG", 27),
        ("ENOSPC", 28),
        ("ESPIPE", 29),
        ("EROFS", 30),
        ("EMLINK", 31),
        ("EPIPE", 32),
        ("EDOM", 33),
        ("ERANGE", 34),
        ("EDEADLK", 35),
        ("ENAMETOOLONG", 36),
        ("ENOLCK", 37),
        ("ENOSYS", 38),
        ("ENOTEMPTY", 39),
        ("ELOOP", 40),
    ];

    /// Modern MINIX 3 `sys/sys/errno.h`. `EBADSRCDST` is the one name modern
    /// MINIX lacks — it spells that condition `EBADEPT` (216).
    const MINIX_BAND: [(&str, i32); 10] = [
        ("ENOTREADY", 201),
        ("EDEADSRCDST", 202),
        ("EDONTREPLY", 203),
        ("EGENERIC", 204),
        ("ELOCKED", 208),
        ("EBADCALL", 209),
        ("ECALLDENIED", 210),
        ("ETRAPDENIED", 211),
        ("EBADREQUEST", 212),
        ("EBADSRCDST", 216),
    ];

    #[test]
    fn ok_is_zero() {
        assert_eq!(OK, 0);
    }

    #[test]
    fn all_is_posix_block_then_minix_band() {
        assert_eq!(ALL.len(), MUSL_POSIX.len() + MINIX_BAND.len());
    }

    #[test]
    fn all_errors_are_negative() {
        for &(name, value) in ALL {
            assert!(value < 0, "errno {name} = {value} should be negative");
        }
    }

    #[test]
    fn all_values_distinct() {
        for (i, &(name_a, a)) in ALL.iter().enumerate() {
            for &(name_b, b) in &ALL[i + 1..] {
                assert_ne!(a, b, "{name_a} and {name_b} share the value {a}");
            }
        }
    }

    #[test]
    fn posix_block_matches_musl() {
        for (&(got_name, got), &(want_name, want)) in ALL.iter().zip(MUSL_POSIX.iter()) {
            assert_eq!(got_name, want_name);
            assert_eq!(got, -want, "{want_name} diverges from musl's value");
        }
    }

    #[test]
    fn minix_band_matches_modern_minix() {
        for (&(got_name, got), &(want_name, want)) in
            ALL[MUSL_POSIX.len()..].iter().zip(MINIX_BAND.iter())
        {
            assert_eq!(got_name, want_name);
            assert_eq!(got, -want, "{want_name} diverges from modern MINIX 3");
        }
    }

    #[test]
    fn minix_band_clear_of_musl_range() {
        for &(name, value) in &ALL[MUSL_POSIX.len()..] {
            assert!(
                -value >= MINIX_ERRNO_BASE,
                "{name} = {value} could collide with a musl errno"
            );
        }
    }

    #[test]
    fn posix_block_stays_below_the_minix_band() {
        for &(name, value) in &ALL[..MUSL_POSIX.len()] {
            assert!(
                -value <= POSIX_ERRNO_MAX,
                "{name} = {value} escaped the POSIX block"
            );
        }
    }

    /// The macro pairs `stringify!($name)` with the constant it declared; a
    /// mis-pairing would be invisible to every other test here.
    #[test]
    fn table_names_resolve_to_the_named_constants() {
        fn value_of(name: &str) -> i32 {
            ALL.iter().find(|e| e.0 == name).expect("name in ALL").1
        }
        assert_eq!(value_of("EFAULT"), EFAULT);
        assert_eq!(value_of("EINVAL"), EINVAL);
        assert_eq!(value_of("ENOSYS"), ENOSYS);
        assert_eq!(value_of("EGENERIC"), EGENERIC);
        assert_eq!(value_of("EBADSRCDST"), EBADSRCDST);
    }
}

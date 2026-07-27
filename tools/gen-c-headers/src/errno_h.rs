// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minixrs/errno.h` — the MINIX-specific errno band, plus an opt-in check that
//! the C library's POSIX errnos agree with `kernel-shared`.
//!
//! The POSIX block (magnitudes 1..=40) is deliberately **not** defined here:
//! minix.rs adopts musl's numbering verbatim (phase-5 decision D7), so those
//! values must come from the C library's own `<errno.h>`. Defining them here
//! would create a second numbering system, which is exactly what D7 exists to
//! avoid.

use minixrs_kernel_shared::error;

use crate::builder::CFile;

/// Include guard for the generated header.
pub const GUARD: &str = "_MINIXRS_ERRNO_H";

/// Guard macro that opts a translation unit into the POSIX errno check.
pub const CHECK_MACRO: &str = "MINIXRS_ABI_CHECK_POSIX_ERRNO";

/// `(name, positive magnitude)` for the POSIX block, in declaration order.
fn posix_block() -> Vec<(&'static str, i32)> {
    error::ALL
        .iter()
        .map(|&(name, value)| (name, -value))
        .filter(|&(_, mag)| mag <= error::POSIX_ERRNO_MAX)
        .collect()
}

/// `(name, positive magnitude)` for the MINIX-specific band.
fn minix_band() -> Vec<(&'static str, i32)> {
    error::ALL
        .iter()
        .map(|&(name, value)| (name, -value))
        .filter(|&(_, mag)| mag >= error::MINIX_ERRNO_BASE)
        .collect()
}

/// Render `minixrs/errno.h`.
pub fn render() -> String {
    let mut f = CFile::new(
        "MINIX-specific errno values, and a check on the C library's POSIX ones.",
        &["kernel-shared/src/error.rs"],
    );
    f.guard_open(GUARD);

    f.block_comment(&[
        "Errno policy (phase-5 decision D7):",
        "",
        "  * The POSIX block (1..40) is deliberately NOT defined here. minix.rs",
        "    adopts musl's numbering verbatim, so those values come from the C",
        "    library's own <errno.h> and musl's syscall_ret.c convention works",
        "    unmodified.",
        "  * The MINIX-specific IPC errnos below live in modern MINIX 3's 200",
        "    band (sys/sys/errno.h), clear of Linux's entire range, so they can",
        "    never collide with a musl-visible errno.",
        "",
        "Values here are POSITIVE magnitudes, matching the <errno.h> convention.",
        "The kernel and the Rust servers carry them NEGATED on the wire (see",
        "kernel-shared/src/error.rs); a libc wrapper negates on the way back out.",
    ]);

    f.section("MINIX-specific errnos");
    for (name, magnitude) in minix_band() {
        f.define_dec(name, magnitude.into());
    }

    f.blank();
    f.define_dec("MINIX_ERRNO_BASE", error::MINIX_ERRNO_BASE.into());
    f.define_dec("POSIX_ERRNO_MAX", error::POSIX_ERRNO_MAX.into());

    f.block_comment(&[
        "Libc-independent and always on: the band separation is what makes a",
        "collision with a musl errno impossible.",
    ]);
    for (name, _) in minix_band() {
        f.static_assert(
            &format!("{name} >= MINIX_ERRNO_BASE"),
            &format!("{name} escaped the MINIX errno band"),
        );
    }

    f.section("POSIX block verification (opt-in)");
    f.block_comment(&[
        "Verification of the POSIX block against the C library actually being",
        "compiled against. Opt-in because it is only meaningful against the musl",
        "fork: a HOST <errno.h> uses different numbers (Darwin: EDEADLK 11,",
        "EAGAIN 35), so asserting against it would be wrong -- or, on glibc,",
        "right for the wrong reason.",
        "",
        "tools/build-musl.sh (slice 5.6) defines the macro below so these fire",
        "against the fork's real bits/errno.h. Until then CI compiles this block",
        "against a generated stand-in, which checks the syntax and the macro",
        "spellings but not the values.",
    ]);
    f.line(&format!("#ifdef {CHECK_MACRO}"));
    f.include("errno.h", "the C library's own POSIX errno values");
    for (name, magnitude) in posix_block() {
        f.static_assert(
            &format!("{name} == {magnitude}"),
            &format!("libc {name} disagrees with kernel-shared error.rs"),
        );
    }
    f.line(&format!("#endif /* {CHECK_MACRO} */"));

    f.guard_close(GUARD);
    f.finish()
}

/// Render the CI-only musl stand-in, `abi-check/errno.h`.
///
/// Without this the 40 assertions inside the `#ifdef` are never parsed until
/// slice 5.6, so a misspelling like `EDEALDK` would sit undetected for six
/// slices. It is generated from the same Rust table those assertions check, so
/// it proves the syntax and the macro names -- not the values.
pub fn render_standin() -> String {
    let mut f = CFile::new(
        "CI-only stand-in for the C library's <errno.h>.",
        &["kernel-shared/src/error.rs"],
    );
    f.guard_open("_MINIXRS_ABI_CHECK_ERRNO_H");

    f.block_comment(&[
        "NOT A REAL LIBC HEADER.",
        "",
        "A stand-in so CI can compile <minixrs/errno.h>'s guarded POSIX",
        "verification block without a musl sysroot. It is generated from the same",
        "Rust table those assertions check, so it is a SYNTAX and MACRO-SPELLING",
        "check, not a value check -- the real value check runs against the fork's",
        "bits/errno.h from slice 5.6 on.",
        "",
        "Never installed into a sysroot; reachable only via an explicit -I.",
    ]);

    f.blank();
    for (name, magnitude) in posix_block() {
        f.define_dec(name, magnitude.into());
    }

    f.guard_close("_MINIXRS_ABI_CHECK_ERRNO_H");
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder;

    #[test]
    fn the_two_bands_partition_error_all() {
        assert_eq!(posix_block().len() + minix_band().len(), error::ALL.len());
        assert_eq!(posix_block().len(), error::POSIX_ERRNO_MAX as usize);
    }

    #[test]
    fn minix_band_renders_from_error_all() {
        let text = render();
        for (name, magnitude) in minix_band() {
            assert_eq!(
                builder::define_value(&text, name).as_deref(),
                Some(magnitude.to_string().as_str()),
                "{name} did not render from error::ALL"
            );
        }
    }

    #[test]
    fn minix_band_values_are_positive_magnitudes() {
        // The wire carries them negated; <errno.h> convention is positive.
        for (name, magnitude) in minix_band() {
            assert!(magnitude >= error::MINIX_ERRNO_BASE, "{name}");
        }
        assert_eq!(
            builder::define_value(&render(), "EDEADSRCDST").as_deref(),
            Some("202")
        );
    }

    /// The core of D7: the POSIX values must come from the C library, so the
    /// header asserts them but must never define them.
    #[test]
    fn posix_block_is_asserted_but_never_defined() {
        let text = render();
        for (name, magnitude) in posix_block() {
            assert!(
                builder::define_value(&text, name).is_none(),
                "{name} must not be #defined by minixrs/errno.h"
            );
            assert!(
                text.contains(&format!("_Static_assert({name} == {magnitude},")),
                "{name} is not verified against the C library"
            );
        }
    }

    #[test]
    fn posix_verification_is_behind_the_opt_in_guard() {
        let text = render();
        let ifdef = text.find(&format!("#ifdef {CHECK_MACRO}")).unwrap();
        let endif = text.find(&format!("#endif /* {CHECK_MACRO} */")).unwrap();
        let epermin = text.find("_Static_assert(EPERM ==").unwrap();
        assert!(ifdef < epermin && epermin < endif);
        // <errno.h> must only be pulled in inside the guard.
        assert!(ifdef < text.find("#include <errno.h>").unwrap());
    }

    #[test]
    fn standin_covers_exactly_the_posix_block() {
        let text = render_standin();
        let defs = builder::defines(&text);
        assert_eq!(defs.len(), posix_block().len());
        for (name, magnitude) in posix_block() {
            assert_eq!(
                builder::define_value(&text, name).as_deref(),
                Some(magnitude.to_string().as_str())
            );
        }
        assert!(text.contains("NOT A REAL LIBC HEADER."));
    }

    #[test]
    fn standin_defines_no_minix_band_errno() {
        let text = render_standin();
        for (name, _) in minix_band() {
            assert!(
                builder::define_value(&text, name).is_none(),
                "the stand-in must not shadow {name}"
            );
        }
    }
}

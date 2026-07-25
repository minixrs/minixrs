// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Generates the C headers that describe the minix.rs ABI to the musl fork.
//!
//! Phase-5 decision D8: the headers are produced from the **live constants** in
//! `minixrs-kernel-shared` by an ordinary Rust program, written into the musl
//! sysroot at build time, and **never committed**. Drift between the Rust and C
//! views of the ABI is therefore impossible by construction rather than caught
//! by a test. cbindgen was rejected because it cannot const-eval the
//! `ProcNr::new()` endpoint constants that `minix/com.h` is mostly made of.
//!
//! Four headers plus two check artifacts:
//!
//! | Artifact | Contents |
//! |---|---|
//! | `include/minix/ipc.h` | `message`, endpoint packing, IPC primitives |
//! | `include/minix/com.h` | proc numbers and the endpoints derived from them |
//! | `include/minix/callnr.h` | kernel calls and the server request bands |
//! | `include/minix/errno.h` | the MINIX errno band; opt-in POSIX check |
//! | `abi-check/errno.h` | CI-only stand-in for the C library's `<errno.h>` |
//! | `abi-selftest.c` | the translation unit that fires every assertion |
//!
//! **Determinism rule:** no timestamps, hostnames, absolute paths, or
//! environment reads ever reach the generated text. The same commit must
//! produce byte-identical output, so the musl sysroot cache (slice 5.6) and the
//! CI "nothing was written into the tree" check both stay meaningful.
//!
//! The ABI freezes at slice 5.6, when the first C file appears. Until then the
//! headers simply grow as later slices add request bands.
#![forbid(unsafe_code)]

pub mod builder;
pub mod callnr_h;
pub mod com_h;
pub mod errno_h;
pub mod ipc_h;
pub mod selftest;

/// One generated file: where it goes under the output directory, and its text.
pub struct Artifact {
    /// Path relative to the output directory, using `/` separators.
    pub path: &'static str,
    /// The complete file contents.
    pub text: String,
}

/// Render every artifact.
pub fn artifacts() -> Vec<Artifact> {
    vec![
        Artifact {
            path: "include/minix/ipc.h",
            text: ipc_h::render(),
        },
        Artifact {
            path: "include/minix/com.h",
            text: com_h::render(),
        },
        Artifact {
            path: "include/minix/callnr.h",
            text: callnr_h::render(),
        },
        Artifact {
            path: "include/minix/errno.h",
            text: errno_h::render(),
        },
        Artifact {
            path: "abi-check/errno.h",
            text: errno_h::render_standin(),
        },
        Artifact {
            path: "abi-selftest.c",
            text: selftest::render(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_artifact_carries_spdx_and_a_do_not_edit_banner() {
        for a in artifacts() {
            assert!(
                a.text
                    .starts_with("/* SPDX-License-Identifier: BSD-3-Clause */\n"),
                "{} is missing its SPDX header",
                a.path
            );
            assert!(
                a.text.contains("GENERATED FILE -- DO NOT EDIT."),
                "{} is missing its generated-file banner",
                a.path
            );
            assert!(
                a.text.contains("cargo gen-c-headers"),
                "{} does not say how to regenerate it",
                a.path
            );
        }
    }

    /// These files are copied into a musl sysroot and compiled by whatever
    /// toolchain a contributor has. A stray em dash or smart quote from a Rust
    /// doc comment would be a mystifying failure.
    #[test]
    fn every_artifact_is_ascii() {
        for a in artifacts() {
            assert!(a.text.is_ascii(), "{} contains non-ASCII bytes", a.path);
        }
    }

    #[test]
    fn every_artifact_ends_with_exactly_one_newline() {
        for a in artifacts() {
            assert!(a.text.ends_with('\n'), "{} has no trailing newline", a.path);
            assert!(
                !a.text.ends_with("\n\n"),
                "{} has a trailing blank line",
                a.path
            );
        }
    }

    /// `CFile::section` and `CFile::block_comment` both open with a blank line,
    /// so an extra explicit `blank()` in front of one silently doubles it.
    #[test]
    fn no_artifact_has_consecutive_blank_lines() {
        for a in artifacts() {
            assert!(
                !a.text.contains("\n\n\n"),
                "{} has a doubled blank line",
                a.path
            );
        }
    }

    #[test]
    fn artifact_paths_are_relative_distinct_and_contained() {
        let mut seen = Vec::new();
        for a in artifacts() {
            assert!(!a.path.starts_with('/'), "{} is absolute", a.path);
            assert!(!a.path.contains(".."), "{} escapes the output dir", a.path);
            assert!(!seen.contains(&a.path), "{} is emitted twice", a.path);
            seen.push(a.path);
        }
    }

    /// Two headers sharing an include guard would silently blank one of them.
    #[test]
    fn header_guards_are_unique() {
        let guards = [
            ipc_h::GUARD,
            com_h::GUARD,
            callnr_h::GUARD,
            errno_h::GUARD,
            "_MINIX_ABI_CHECK_ERRNO_H",
        ];
        for (i, g) in guards.iter().enumerate() {
            assert!(!guards[i + 1..].contains(g), "duplicate include guard {g}");
        }
    }

    /// The real bug class as bands multiply: a name emitted twice within one
    /// header is a redefinition warning at best and a silent wrong value at
    /// worst.
    #[test]
    fn no_header_defines_a_name_twice() {
        for a in artifacts() {
            let defs = builder::defines(&a.text);
            for (i, (name, _)) in defs.iter().enumerate() {
                assert!(
                    !defs[i + 1..].iter().any(|(n, _)| n == name),
                    "{} defines {name} twice",
                    a.path
                );
            }
        }
    }

    /// Nothing enforces that a Rust constant name is a legal C identifier, so
    /// check it here rather than in a clang error message.
    #[test]
    fn every_define_name_is_a_c_identifier() {
        for a in artifacts() {
            for (name, _) in builder::defines(&a.text) {
                let mut chars = name.chars();
                let first = chars.next().expect("non-empty define name");
                assert!(
                    first.is_ascii_alphabetic() || first == '_',
                    "{}: {name} does not start with a letter or underscore",
                    a.path
                );
                assert!(
                    chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{}: {name} is not a C identifier",
                    a.path
                );
            }
        }
    }

    /// D8's sysroot cache and the CI "nothing changed" check both assume the
    /// output is a pure function of the commit.
    #[test]
    fn rendering_is_deterministic() {
        let a = artifacts();
        let b = artifacts();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.path, y.path);
            assert_eq!(x.text, y.text, "{} is not deterministic", x.path);
        }
    }

    #[test]
    fn the_selftest_includes_exactly_the_generated_headers() {
        let emitted: Vec<String> = artifacts()
            .iter()
            .filter_map(|a| a.path.strip_prefix("include/"))
            .map(str::to_string)
            .collect();
        for header in selftest::HEADERS {
            assert!(
                emitted.iter().any(|e| e == header),
                "the selftest includes {header}, which is not generated"
            );
        }
        assert_eq!(emitted.len(), selftest::HEADERS.len());
    }
}

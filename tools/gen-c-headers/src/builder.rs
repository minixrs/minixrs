// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Append-only writer for a generated C file.
//!
//! Deliberately not built on `core::fmt::Write`: `write!` into a `String`
//! returns a `Result` that has to be handled at every one of a few hundred call
//! sites for an error that cannot occur. `push_str` plus `format!` keeps the
//! renderers readable.

/// Column the value in a `#define` is padded out to.
const DEFINE_WIDTH: usize = 24;

/// A C translation unit or header under construction.
pub struct CFile {
    out: String,
}

impl CFile {
    /// Start a file with the shared SPDX + generated-artifact banner.
    ///
    /// `sources` names the `kernel-shared` files the content is derived from,
    /// so a reader of the generated header knows where the real definition
    /// lives.
    pub fn new(title: &str, sources: &[&str]) -> Self {
        let mut f = Self { out: String::new() };
        f.line("/* SPDX-License-Identifier: BSD-3-Clause */");
        f.line("/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */");
        f.line("/*");
        f.line(" * GENERATED FILE -- DO NOT EDIT.");
        f.line(" *");
        f.line(&format!(" * {title}"));
        f.line(" *");
        f.line(" * Produced by tools/gen-c-headers from the live constants in the");
        f.line(" * minixrs-kernel-shared crate:");
        for src in sources {
            f.line(&format!(" *     {src}"));
        }
        f.line(" * Regenerate with `cargo gen-c-headers`.");
        f.line(" *");
        f.line(" * A build artifact, deliberately never committed (phase-5 decision D8):");
        f.line(" * the Rust constants are the single source of truth, so drift is");
        f.line(" * impossible by construction rather than caught by a test.");
        f.line(" */");
        f
    }

    /// Append one line.
    pub fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// Append a blank line.
    pub fn blank(&mut self) {
        self.out.push('\n');
    }

    /// Open an include guard.
    pub fn guard_open(&mut self, guard: &str) {
        self.blank();
        self.line(&format!("#ifndef {guard}"));
        self.line(&format!("#define {guard}"));
    }

    /// Close an include guard opened by [`CFile::guard_open`].
    pub fn guard_close(&mut self, guard: &str) {
        self.blank();
        self.line(&format!("#endif /* {guard} */"));
    }

    /// A one-line section banner.
    pub fn section(&mut self, title: &str) {
        self.blank();
        self.line(&format!("/* ---- {title} ---- */"));
    }

    /// A multi-line block comment.
    pub fn block_comment(&mut self, lines: &[&str]) {
        self.blank();
        self.line("/*");
        for l in lines {
            if l.is_empty() {
                self.line(" *");
            } else {
                self.line(&format!(" * {l}"));
            }
        }
        self.line(" */");
    }

    /// `#include <header>` with a trailing rationale comment.
    pub fn include(&mut self, header: &str, why: &str) {
        self.line(&format!("#include <{header}>   /* {why} */"));
    }

    /// `#define NAME value` in decimal. Negative values are parenthesized, so
    /// `x-SYSTEM_PROC_NR` cannot mis-parse at the use site.
    pub fn define_dec(&mut self, name: &str, value: i64) {
        let text = if value < 0 {
            format!("({value})")
        } else {
            value.to_string()
        };
        self.define_raw(name, &text);
    }

    /// `#define NAME 0xVALUE`, for call numbers and request bands.
    pub fn define_hex(&mut self, name: &str, value: i64) {
        assert!(value >= 0, "hex defines are for non-negative values");
        self.define_raw(name, &format!("0x{value:X}"));
    }

    /// `#define NAME ((endpoint_t) value)` — an endpoint, never a proc number.
    pub fn define_ep(&mut self, name: &str, value: i32) {
        self.define_raw(name, &format!("((endpoint_t) {value})"));
    }

    /// `#define NAME <text>` with the value column aligned.
    pub fn define_raw(&mut self, name: &str, text: &str) {
        self.line(&format!("#define {name:<DEFINE_WIDTH$} {text}"));
    }

    /// A C11 `_Static_assert`. `msg` must not contain a double quote.
    pub fn static_assert(&mut self, expr: &str, msg: &str) {
        assert!(
            !msg.contains('"'),
            "assert message must not contain a quote"
        );
        self.line(&format!("_Static_assert({expr}, \"{msg}\");"));
    }

    /// Finish the file.
    pub fn finish(self) -> String {
        self.out
    }
}

/// Parse the `#define NAME value` lines out of a rendered file.
///
/// Object-like defines only: a function-like `#define NAME(args) ...` has no
/// value column to report, so it is skipped. Used by the cross-artifact tests
/// to check for duplicate names and non-identifier spellings.
pub fn defines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.strip_prefix("#define "))
        .filter_map(|rest| {
            let (name, value) = rest.split_once(char::is_whitespace)?;
            if name.contains('(') {
                return None; // function-like macro
            }
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// The value of one object-like `#define`, or `None` if it is absent.
pub fn define_value(text: &str, name: &str) -> Option<String> {
    defines(text)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_carries_spdx_and_do_not_edit() {
        let text = CFile::new("Test header.", &["kernel-shared/src/com.rs"]).finish();
        assert!(text.starts_with("/* SPDX-License-Identifier: BSD-3-Clause */\n"));
        assert!(text.contains("GENERATED FILE -- DO NOT EDIT."));
        assert!(text.contains("kernel-shared/src/com.rs"));
    }

    #[test]
    fn negative_decimals_are_parenthesized() {
        let mut f = CFile::new("t", &[]);
        f.define_dec("SYSTEM_PROC_NR", -2);
        f.define_dec("PM_PROC_NR", 0);
        let text = f.finish();
        assert!(text.contains("#define SYSTEM_PROC_NR           (-2)"));
        assert!(text.contains("#define PM_PROC_NR               0"));
    }

    #[test]
    fn hex_defines_are_uppercase_with_an_0x_prefix() {
        let mut f = CFile::new("t", &[]);
        f.define_hex("SYS_EXEC", 0x603);
        assert!(
            f.finish()
                .contains("#define SYS_EXEC                 0x603")
        );
    }

    #[test]
    fn endpoints_are_cast_to_endpoint_t() {
        let mut f = CFile::new("t", &[]);
        f.define_ep("SYSTEM_EP", 32766);
        assert!(f.finish().contains("((endpoint_t) 32766)"));
    }

    #[test]
    fn guards_open_and_close_the_same_name() {
        let mut f = CFile::new("t", &[]);
        f.guard_open("_MINIXRS_IPC_H");
        f.guard_close("_MINIXRS_IPC_H");
        let text = f.finish();
        assert!(text.contains("#ifndef _MINIXRS_IPC_H\n#define _MINIXRS_IPC_H\n"));
        assert!(text.contains("#endif /* _MINIXRS_IPC_H */"));
    }

    #[test]
    fn static_assert_renders_a_quoted_message() {
        let mut f = CFile::new("t", &[]);
        f.static_assert("sizeof(message) == 104", "message size drifted");
        assert!(
            f.finish()
                .contains("_Static_assert(sizeof(message) == 104, \"message size drifted\");")
        );
    }

    #[test]
    #[should_panic(expected = "assert message must not contain a quote")]
    fn static_assert_rejects_a_quote_in_the_message() {
        CFile::new("t", &[]).static_assert("1", "say \"no\"");
    }

    #[test]
    #[should_panic(expected = "hex defines are for non-negative values")]
    fn hex_defines_reject_negatives() {
        CFile::new("t", &[]).define_hex("BAD", -1);
    }

    #[test]
    fn defines_parses_object_like_macros_and_skips_function_like_ones() {
        let mut f = CFile::new("t", &[]);
        f.define_dec("A", 1);
        f.define_ep("B", 7);
        f.line("#define _ENDPOINT(g, p)  ((g) | (p))");
        let text = f.finish();

        assert_eq!(
            defines(&text),
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "((endpoint_t) 7)".to_string()),
            ]
        );
        assert_eq!(
            define_value(&text, "B").as_deref(),
            Some("((endpoint_t) 7)")
        );
        assert_eq!(define_value(&text, "MISSING"), None);
    }

    #[test]
    fn defines_ignores_the_include_guard_shape() {
        // `#define _GUARD` has no value column and must not be misparsed.
        let mut f = CFile::new("t", &[]);
        f.guard_open("_G");
        f.define_dec("A", 1);
        f.guard_close("_G");
        assert_eq!(
            defines(&f.finish()),
            vec![("A".to_string(), "1".to_string())]
        );
    }
}

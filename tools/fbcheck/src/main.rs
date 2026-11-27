// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minixrs-fbcheck` — command-line front end for the screen check.
//!
//! ```text
//! minixrs-fbcheck check [--scrolled] <dump.ppm> <qemu-log>
//! minixrs-fbcheck dump <qmp-sock> <out.ppm>
//! minixrs-fbcheck dump-and-check [--scrolled] <qmp-sock> <out.ppm> <qemu-log>
//! ```
//!
//! * `check` applies F10 to a dump already taken.
//! * `dump` takes one from a running QEMU over its QMP socket (`stop`,
//!   `screendump`, `cont`).
//! * `dump-and-check` does both, reading the log after the dump.
//!
//! One line says what happened:
//!
//! | line                                           | on     | exit |
//! | ---------------------------------------------- | ------ | ---- |
//! | `fbcheck: ok <fields> scrolled=yes` (or `=no`) | stdout | 0    |
//! | `fbcheck: early <fields> (not scrolled yet)`   | stdout | 2    |
//! | `fbcheck: FAIL <reason>`                       | stderr | 1    |
//! | `fbcheck: dumped <file>` (`dump` only)         | stdout | 0    |
//!
//! `<fields>` is `<w>x<h> <cols>x<rows> rows=<non-blank> wraps=<n> stray=<n>
//! worst=<n>`: the geometry, the screen's non-blank rows, the lines that
//! wrapped, the rows that matched the log only by allowing the kernel's tick
//! bytes inside them, and the most tick bytes one row needed (the cap is
//! `check::MAX_STRAY`).
//!
//! **`--scrolled`** asks for a screen that has scrolled, which is the only
//! kind that proves the scroll path. Without it a screen that is right but
//! has not scrolled yet is `ok … scrolled=no`, exit 0. With it, that screen
//! is `early`, exit 2 — not a failure and not a pass: look again later. A
//! wrong screen is `FAIL`, exit 1, either way. The flag may stand anywhere
//! after the subcommand.
//!
//! Exit status: 0 passed (or dumped); 1 failed; 2 right so far but not
//! scrolled (`--scrolled` only); 64 usage.
//!
//! Argv parsing is hand-rolled because `deny.toml`'s allow-list keeps
//! third-party crates out of this workspace. Everything with logic worth
//! testing lives in the library; this file is argv and printing, and is
//! Sonar-coverage-excluded by the `tools/**/src/main.rs` glob.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use minixrs_fbcheck::qmp::{self, TIMEOUT};
use minixrs_fbcheck::{Outcome, Report, check_files, dump_and_check, outcome};

const USAGE: &str = "usage: minixrs-fbcheck check [--scrolled] <dump.ppm> <qemu-log>
       minixrs-fbcheck dump <qmp-sock> <out.ppm>
       minixrs-fbcheck dump-and-check [--scrolled] <qmp-sock> <out.ppm> <qemu-log>";

/// BSD's `EX_USAGE`, as the scripts in `tools/` use it.
const EX_USAGE: u8 = 64;
/// Right so far, but not what `--scrolled` asked for.
const EX_EARLY: u8 = 2;

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let command = if args.is_empty() {
        OsString::new()
    } else {
        args.remove(0)
    };
    let before = args.len();
    args.retain(|arg| arg != "--scrolled");
    let want_scrolled = args.len() != before;
    let paths: Vec<&Path> = args.iter().map(Path::new).collect();

    let checked: Result<Report, String> = match (command.to_str(), paths.as_slice()) {
        (Some("check"), [dump, log]) => check_files(dump, log),
        (Some("dump-and-check"), [sock, out, log]) => dump_and_check(sock, out, log, TIMEOUT),
        (Some("dump"), [sock, out]) if !want_scrolled => {
            return match qmp::dump(sock, out, TIMEOUT) {
                Ok(()) => {
                    println!("fbcheck: dumped {}", out.display());
                    ExitCode::SUCCESS
                }
                Err(reason) => fail(&reason),
            };
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(EX_USAGE);
        }
    };

    match checked.map(|report| outcome(&report, want_scrolled)) {
        Ok(Outcome::Pass(line)) => {
            println!("fbcheck: {line}");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Early(line)) => {
            println!("fbcheck: {line}");
            ExitCode::from(EX_EARLY)
        }
        Err(reason) => fail(&reason),
    }
}

fn fail(reason: &str) -> ExitCode {
    eprintln!("fbcheck: FAIL {reason}");
    ExitCode::FAILURE
}

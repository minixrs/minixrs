// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minixrs-fbcheck` — command-line front end for the screen check.
//!
//! ```text
//! minixrs-fbcheck check <dump.ppm> <qemu-log>
//! minixrs-fbcheck dump <qmp-sock> <out.ppm>
//! minixrs-fbcheck dump-and-check <qmp-sock> <out.ppm> <qemu-log>
//! ```
//!
//! * `check` applies F10 to a dump already taken.
//! * `dump` takes one from a running QEMU over its QMP socket (`stop`,
//!   `screendump`, `cont`).
//! * `dump-and-check` does both, reading the log after the dump.
//!
//! One line of verdict: `fbcheck: ok <w>x<h> <cols>x<rows> rows=<non-blank>
//! wraps=<n> stray=<n> worst=<n>` on stdout, or `fbcheck: FAIL <reason>` on
//! stderr. `rows` is the screen's non-blank rows, `wraps` the wrapped lines
//! seen whole, `stray` the rows found in the log only by allowing the
//! kernel's tick bytes inside them, and `worst` the most tick bytes one row
//! needed (the cap is `check::MAX_STRAY`).
//!
//! Exit status: 0 passed (or dumped); 1 failed; 64 usage.
//!
//! Argv parsing is hand-rolled because `deny.toml`'s allow-list keeps
//! third-party crates out of this workspace. Everything with logic worth
//! testing lives in the library; this file is argv and printing, and is
//! Sonar-coverage-excluded by the `tools/**/src/main.rs` glob.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use minixrs_fbcheck::qmp::{self, TIMEOUT};
use minixrs_fbcheck::{check_files, dump_and_check};

const USAGE: &str = "usage: minixrs-fbcheck check <dump.ppm> <qemu-log>
       minixrs-fbcheck dump <qmp-sock> <out.ppm>
       minixrs-fbcheck dump-and-check <qmp-sock> <out.ppm> <qemu-log>";

/// BSD's `EX_USAGE`, as the scripts in `tools/` use it.
const EX_USAGE: u8 = 64;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let paths: Vec<&Path> = args.iter().skip(1).map(Path::new).collect();
    let command = args.first().and_then(|c| c.to_str());

    let verdict = match (command, paths.as_slice()) {
        (Some("check"), [dump, log]) => check_files(dump, log).map(|r| r.to_string()),
        (Some("dump"), [sock, out]) => {
            qmp::dump(sock, out, TIMEOUT).map(|()| format!("dumped {}", out.display()))
        }
        (Some("dump-and-check"), [sock, out, log]) => {
            dump_and_check(sock, out, log, TIMEOUT).map(|r| r.to_string())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(EX_USAGE);
        }
    };

    match verdict {
        Ok(line) => {
            println!("fbcheck: {line}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("fbcheck: FAIL {reason}");
            ExitCode::FAILURE
        }
    }
}

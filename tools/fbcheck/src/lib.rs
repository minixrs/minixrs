// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `fbcheck` — proves a framebuffer console drew what the boot wrote (slice
//! 6.7, decisions F10 and F11).
//!
//! The console's screen is never quiet — init's rotation prints for ever — so
//! there is no fixed picture to compare a screen dump with. This tool instead
//! checks a dump against two things that *are* known: the renderer, and the
//! boot's own serial log.
//!
//! * [`ppm`] reads the dump: the binary PPM a QMP `screendump` writes.
//! * [`check`] is the proof. The dump is the size TTY reported; every cell is
//!   a glyph, decoded with the TTY library that painted it (`minixrs-tty`'s
//!   `paint::decode_cell`); and the text decoded is in the serial log, which
//!   `pl011` wrote and the renderer never touched. That last step is what
//!   makes this more than the renderer agreeing with itself. The serial
//!   line has other writers, so the log is read as it really is: the
//!   kernel's own lines are taken out first, and a row may then have the
//!   kernel's per-tick bytes — letters, singly or in runs, up to
//!   [`check::MAX_STRAY`] a row — inside it and nothing else. [`check`]'s
//!   module docs give the rule exactly, and what it costs.
//! * [`qmp`] takes the dump from a running QEMU: `stop`, `screendump`, `cont`.
//!
//! The two functions here join them for the binary, which is argv and
//! printing and nothing else (`src/main.rs`, coverage-excluded by the
//! `tools/**/src/main.rs` glob like every host tool's). `tools/screendump.sh`
//! is the caller that knows *when* to dump and how often to retry.
//!
//! Host-only, `std`, Unix-only (the QMP socket is a Unix socket), and — like
//! the library it leans on — free of `unsafe`.

#![forbid(unsafe_code)]

pub mod check;
pub mod ppm;
pub mod qmp;

use std::path::Path;
use std::time::Duration;

pub use check::{CheckError, MIN_ROWS, Report};

/// Apply the check to a dump file and a serial-log file.
pub fn check_files(dump: &Path, log: &Path) -> Result<Report, String> {
    let image = std::fs::read(dump).map_err(|e| format!("reading {}: {e}", dump.display()))?;
    let image = ppm::Ppm::parse(&image).map_err(|e| format!("{}: {e}", dump.display()))?;
    let log = std::fs::read(log).map_err(|e| format!("reading {}: {e}", log.display()))?;
    check::check(&image, &log).map_err(|e| e.to_string())
}

/// Dump the running guest's screen to `out`, then check it against `log`.
///
/// The order matters: the log is read **after** the guest has been stopped
/// and dumped. TTY writes each byte to the UART before it draws it (F9), so
/// everything on the dumped screen was already on the serial line; reading the
/// log first could miss the newest rows.
pub fn dump_and_check(
    sock: &Path,
    out: &Path,
    log: &Path,
    timeout: Duration,
) -> Result<Report, String> {
    qmp::dump(sock, out, timeout)?;
    check_files(out, log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qmp::testing::{GREETING, RETURN, Scratch, executed, healthy, serve};
    use minixrs_tty::grid::Grid;

    const SHORT: Duration = Duration::from_millis(300);
    const TEXT: &[u8] = b"one 1\ntwo 2\nthree 3\nfour 4\nfive 5\nsix 6\nseven 7\neight 8\n";
    const FB_OK: &[u8] = b"[diag tty] fb ok w=80 h=160 cols=10 rows=10\n";

    /// An 80×160 screen showing `text`, as a PPM file's bytes.
    fn screen(text: &[u8]) -> Vec<u8> {
        let fmt = check::dump_format(80, 160).unwrap();
        let mut grid = Grid::new(fmt.cols(), fmt.rows());
        for &byte in text {
            grid.put(byte);
        }
        check::render(&fmt, &grid).to_ppm()
    }

    fn log_file(scratch: &Scratch, body: &[u8]) -> std::path::PathBuf {
        let path = scratch.path("qemu.log");
        std::fs::write(&path, [FB_OK, body].concat()).unwrap();
        path
    }

    #[test]
    fn check_files_passes_a_dump_that_matches_its_log() {
        let scratch = Scratch::new();
        let dump = scratch.path("screen.ppm");
        std::fs::write(&dump, screen(TEXT)).unwrap();
        let report = check_files(&dump, &log_file(&scratch, TEXT)).unwrap();
        assert_eq!(
            report.to_string(),
            "ok 80x160 10x10 rows=8 wraps=0 stray=0 worst=0"
        );
    }

    #[test]
    fn check_files_names_the_file_it_could_not_use() {
        let scratch = Scratch::new();
        let log = log_file(&scratch, TEXT);

        let missing = scratch.path("missing.ppm");
        let err = check_files(&missing, &log).unwrap_err();
        assert!(err.contains("missing.ppm"), "{err}");

        let png = scratch.path("screen.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        let err = check_files(&png, &log).unwrap_err();
        assert!(err.contains("screen.png") && err.contains("P6"), "{err}");

        let dump = scratch.path("screen.ppm");
        std::fs::write(&dump, screen(TEXT)).unwrap();
        let err = check_files(&dump, &scratch.path("no.log")).unwrap_err();
        assert!(err.contains("no.log"), "{err}");

        // And the check's own refusal comes through as its message.
        let err = check_files(&dump, &log_file(&scratch, b"another boot\n")).unwrap_err();
        assert_eq!(
            err,
            "screen row 0 is not in the serial log's console text, even allowing 32 tick bytes inside it: \"one 1\""
        );
    }

    #[test]
    fn dump_and_check_takes_the_dump_and_passes_it() {
        let scratch = Scratch::new();
        let (sock, out) = (scratch.path("q.sock"), scratch.path("screen.ppm"));
        let server = serve(&sock, Some(GREETING), healthy(screen(TEXT)));

        let report = dump_and_check(&sock, &out, &log_file(&scratch, TEXT), SHORT).unwrap();
        assert_eq!(report.non_blank_rows, 8);
        assert_eq!(report.text[7], "eight 8");

        let commands = server.join().unwrap();
        assert_eq!(executed(&commands[1]), "stop");
        assert_eq!(executed(&commands[3]), "cont");
    }

    #[test]
    fn dump_and_check_refuses_a_screen_the_log_does_not_explain_and_keeps_the_dump() {
        let scratch = Scratch::new();
        let (sock, out) = (scratch.path("q.sock"), scratch.path("screen.ppm"));
        let image = screen(TEXT);
        let server = serve(&sock, Some(GREETING), healthy(image.clone()));

        let log = log_file(&scratch, b"one 1\ntwo 2\n");
        let err = dump_and_check(&sock, &out, &log, SHORT).unwrap_err();
        assert_eq!(
            err,
            "screen row 2 is not in the serial log's console text, even allowing 32 tick bytes inside it: \"three 3\""
        );
        assert_eq!(std::fs::read(&out).unwrap(), image, "the dump is kept");
        // The guest was let go before the check ran.
        assert_eq!(executed(server.join().unwrap().last().unwrap()), "cont");
    }

    #[test]
    fn dump_and_check_does_not_check_a_stale_dump_when_the_dump_fails() {
        let scratch = Scratch::new();
        let (sock, out) = (scratch.path("q.sock"), scratch.path("screen.ppm"));
        // An earlier try's passing dump is still on disk.
        std::fs::write(&out, screen(TEXT)).unwrap();
        let server = serve(&sock, Some(GREETING), |command| {
            match executed(command).as_str() {
                "screendump" => {
                    vec![r#"{"error": {"class": "GenericError", "desc": "no console"}}"#.into()]
                }
                _ => vec![RETURN.into()],
            }
        });
        let err = dump_and_check(&sock, &out, &log_file(&scratch, TEXT), SHORT).unwrap_err();
        assert!(err.contains("no console"), "{err}");
        server.join().unwrap();
    }
}

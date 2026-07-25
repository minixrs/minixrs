// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Entry point for `cargo gen-c-headers`.
//!
//! Argument handling and file I/O only — every byte of the generated content
//! comes from the library modules, which carry the unit tests.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use minixrs_gen_c_headers::artifacts;

/// Where the headers land when no output directory is given. Under `target/`,
/// so the D8 "never committed" rule holds without a `.gitignore` entry.
const DEFAULT_OUTDIR: &str = "target/gen-c-headers";

const USAGE: &str = "\
usage: cargo gen-c-headers [OUTDIR]
       cargo gen-c-headers --stdout
       cargo gen-c-headers --help

Generates the C headers describing the minix.rs ABI from the live constants in
the minixrs-kernel-shared crate.

  OUTDIR     directory to write into (default: target/gen-c-headers)
  --stdout   concatenate every artifact to stdout instead of writing files

The output is a build artifact and is deliberately never committed: regenerate
it rather than editing it.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.as_slice() {
        [] => write_all(Path::new(DEFAULT_OUTDIR)),
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        [flag] if flag == "--stdout" => dump(),
        [outdir] if !outdir.starts_with('-') => write_all(Path::new(outdir)),
        _ => {
            eprintln!("gen-c-headers: unrecognized arguments\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Write every artifact under `outdir`, creating parent directories.
fn write_all(outdir: &Path) -> ExitCode {
    for artifact in artifacts() {
        let path = outdir.join(artifact.path);
        let Some(parent) = path.parent() else {
            eprintln!("gen-c-headers: {} has no parent directory", path.display());
            return ExitCode::FAILURE;
        };
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("gen-c-headers: {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
        if let Err(e) = fs::write(&path, &artifact.text) {
            eprintln!("gen-c-headers: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}

/// Concatenate every artifact to stdout, each under a `==> path <==` banner.
fn dump() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for artifact in artifacts() {
        if writeln!(out, "==> {} <==\n{}", artifact.path, artifact.text).is_err() {
            // A closed pipe (`| head`) is not an error worth reporting.
            return ExitCode::SUCCESS;
        }
    }
    ExitCode::SUCCESS
}

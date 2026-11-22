// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `mkimage` — command-line front end for the boot-disk builder.
//!
//! ```text
//! mkimage <out.img> [--root <rootfs.img>] <esp-path>=<host-file>...
//!
//!   mkimage target/disk.img --root target/aarch64-unknown-none/release/rootfs.img \
//!       EFI/BOOT/BOOTAA64.EFI=external/limine/dist/BOOTAA64.EFI \
//!       limine.conf=tools/limine.conf \
//!       boot/kernel=target/aarch64-unknown-none/release/minixrs-kernel
//! ```
//!
//! Without `--root` the image has no root partition (and is the same size):
//! that is the disk the ramdisk boot attaches.
//!
//! Argv parsing is hand-rolled because `deny.toml`'s allow-list keeps third-party
//! crates out of this workspace. Everything with logic worth testing lives in the
//! library; this file is argv and file I/O, and is Sonar-coverage-excluded by the
//! `tools/**/src/main.rs` glob.

use std::fs::File;
use std::process::ExitCode;

use minixrs_mkimage::{Spec, build_image, write_sparse};

const USAGE: &str = "usage: mkimage <out.img> [--root <rootfs.img>] <esp-path>=<host-file>...";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut args = args.into_iter();
    let out = match args.next() {
        Some(out) if !out.starts_with("--") && !out.contains('=') => out,
        _ => return Err(USAGE.to_string()),
    };

    let mut spec = Spec::default();
    let mut root_given = false;
    while let Some(arg) = args.next() {
        if arg == "--root" {
            let Some(path) = args.next() else {
                return Err(format!("mkimage: --root needs a file\n{USAGE}"));
            };
            if root_given {
                return Err(format!("mkimage: --root given twice\n{USAGE}"));
            }
            root_given = true;
            spec.root = Some(read(&path)?);
        } else if arg.starts_with("--") {
            return Err(format!("mkimage: unknown option {arg:?}\n{USAGE}"));
        } else {
            let Some((esp_path, host_path)) = arg.split_once('=') else {
                return Err(format!(
                    "mkimage: {arg:?} is not <esp-path>=<host-file>\n{USAGE}"
                ));
            };
            spec.esp_files
                .push((esp_path.to_string(), read(host_path)?));
        }
    }
    if spec.esp_files.is_empty() {
        return Err(format!("mkimage: no ESP files given\n{USAGE}"));
    }

    let image = build_image(&spec).map_err(|e| format!("mkimage: {e}"))?;

    // `File::create` truncates, which `write_sparse` relies on: the ranges it
    // skips must read back as zeros.
    let writing = |e| format!("mkimage: writing {out}: {e}");
    let mut file = File::create(&out).map_err(writing)?;
    write_sparse(&mut file, &image).map_err(writing)?;
    file.sync_all().map_err(writing)?;

    println!(
        "mkimage: wrote {out} ({} bytes, {} ESP files, {})",
        image.len(),
        spec.esp_files.len(),
        if spec.root.is_some() {
            "with a root partition"
        } else {
            "no root partition"
        }
    );
    Ok(())
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("mkimage: reading {path}: {e}"))
}

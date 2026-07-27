// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `mkfs-mfs` — command-line front end for the root-image builder.
//!
//! ```text
//! mkfs-mfs <out.img> <fspath>=<hostfile> ...
//!
//!   mkfs-mfs rootfs.img /bin/hello=target/hello/hello /etc/motd=etc/motd
//! ```
//!
//! `kernel/build.rs` does not run this binary — it calls the library directly, so
//! the kernel build needs no nested host cargo invocation. This exists to inspect
//! an image by hand, which is the one thing a build-script call cannot do.
//!
//! Argv parsing is hand-rolled because `deny.toml`'s allow-list keeps third-party
//! crates out of this workspace. Everything with logic worth testing lives in the
//! library modules; this file is argv and file I/O, and is Sonar-coverage-excluded
//! by the `tools/**/src/main.rs` glob.

use std::process::ExitCode;

use minixrs_mkfs_mfs::{Manifest, build_image};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: mkfs-mfs <out.img> <fspath>=<hostfile> ...");
        return ExitCode::FAILURE;
    }
    let out = &args[0];

    let mut manifest = Manifest::new();
    for spec in &args[1..] {
        let Some((fs_path, host_path)) = spec.split_once('=') else {
            eprintln!("mkfs-mfs: {spec:?} is not <fspath>=<hostfile>");
            return ExitCode::FAILURE;
        };
        let bytes = match std::fs::read(host_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("mkfs-mfs: reading {host_path}: {e}");
                return ExitCode::FAILURE;
            }
        };
        manifest.add(fs_path, bytes);
    }

    let image = match build_image(&manifest) {
        Ok(image) => image,
        Err(e) => {
            eprintln!("mkfs-mfs: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(out, &image) {
        eprintln!("mkfs-mfs: writing {out}: {e}");
        return ExitCode::FAILURE;
    }
    println!("mkfs-mfs: wrote {out} ({} bytes)", image.len());
    ExitCode::SUCCESS
}

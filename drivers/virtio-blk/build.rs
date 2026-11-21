// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors

fn main() {
    println!("cargo:rerun-if-changed=user.ld");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("minixrs") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{dir}/user.ld");
    }
}

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// Assembles the per-arch entry path and exception vectors into the kernel.
//
// .S files go through clang (cross-targeted at the kernel triple) and the
// resulting .o files are passed straight to the linker via cargo:rustc-link-arg.
// We deliberately *avoid* the cc crate's static-library packaging because the
// linker's archive member resolution wouldn't pull in `_start` (it's only
// referenced via the linker-script `ENTRY` directive, not from any Rust
// symbol). Direct .o linkage sidesteps that, and it also dodges the
// macOS-ar/ELF-object mismatch entirely.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Host builds (cargo check / cargo test on macOS or Linux) compile the
    // kernel crate as a no-op (main.rs gates every real module on
    // `target_os = "none"`). Skip assembly entirely for those — the ELF .o
    // files clang would produce here aren't link-compatible with the
    // host's mach-o or glibc-flavored ELF and would only break tests.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "none" {
        return;
    }

    let arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH unset");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR unset"));

    match arch.as_str() {
        "aarch64" => {
            let sources = [
                "src/arch/aarch64/entry.S",
                "src/arch/aarch64/vectors.S",
                "src/arch/aarch64/trap.S",
                "src/arch/aarch64/interrupt.S",
                "src/arch/aarch64/user_stub.S",
            ];
            for src in &sources {
                println!("cargo:rerun-if-changed={src}");
                let stem = std::path::Path::new(src)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy();
                let obj = out_dir.join(format!("{stem}.o"));
                let status = Command::new("clang")
                    .args([
                        "--target=aarch64-unknown-none",
                        "-ffreestanding",
                        "-c",
                        src,
                        "-o",
                    ])
                    .arg(&obj)
                    .status()
                    .expect("failed to spawn clang");
                if !status.success() {
                    panic!("clang failed assembling {src}");
                }
                println!("cargo:rustc-link-arg={}", obj.display());
            }
            println!("cargo:rerun-if-changed=src/arch/aarch64/linker.ld");

            // Build the VM server as a freestanding EL0 ELF and embed it
            // (slice 3.4). `boot_image::VM_ELF` is `include_bytes!(env!(...))`.
            build_vm_server(&out_dir);
        }
        "x86_64" => {
            // Phase 8 territory -- nothing to assemble yet.
        }
        other => panic!("unsupported target arch: {other}"),
    }
}

/// Build the VM server crate for the aarch64 EL0 user target and tell rustc
/// where the resulting ELF is, so `boot_image::VM_ELF` can `include_bytes!` it.
///
/// We reuse the builtin `aarch64-unknown-none` triple but must NOT inherit the
/// kernel's linker script: the workspace `.cargo/config.toml` forces
/// `-Tkernel/.../linker.ld` on that triple via `rustflags`. We override it with
/// `CARGO_ENCODED_RUSTFLAGS` (highest-precedence; replaces config rustflags
/// entirely) pointing at `servers/vm/user.ld`, and isolate the build in a
/// separate `CARGO_TARGET_DIR` so nesting cargo inside this build script does
/// not deadlock on the outer kernel build's target-dir lock.
fn build_vm_server(out_dir: &std::path::Path) {
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset"),
    );
    let workspace = manifest.parent().expect("kernel manifest has no parent");
    let vm_dir = workspace.join("servers/vm");
    let user_ld = vm_dir.join("user.ld");
    let vm_target_dir = out_dir.join("vm-target");

    // Rebuild the kernel (and re-embed) whenever the VM crate or the IPC
    // library it links against changes.
    for path in [
        vm_dir.join("src/main.rs"),
        user_ld.clone(),
        vm_dir.join("Cargo.toml"),
        workspace.join("minix-ipc/src/lib.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // `-C link-arg=-T<user.ld>`, encoded with the \x1f separator cargo expects.
    let encoded_rustflags = format!("-Clink-arg=-T{}", user_ld.display());

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(workspace)
        .args([
            "build",
            "-p",
            "minixrs-vm",
            "--target",
            "aarch64-unknown-none",
            "--release",
        ])
        .env("CARGO_TARGET_DIR", &vm_target_dir)
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags)
        .status()
        .expect("failed to spawn cargo for minixrs-vm");
    if !status.success() {
        panic!("building minixrs-vm (VM server ELF) failed");
    }

    let elf = vm_target_dir.join("aarch64-unknown-none/release/minixrs-vm");
    assert!(elf.exists(), "VM ELF missing at {}", elf.display());
    println!("cargo:rustc-env=VM_ELF_PATH={}", elf.display());
}

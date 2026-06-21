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

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH unset");
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

            // Build every boot server as a freestanding EL0 ELF, pack them into
            // the MXBI boot-image archive, and emit `BOOT_IMAGE_PATH` for
            // `boot_image` to `include_bytes!` (slice 4.2, generalizing the
            // slice-3.4 single-VM embed).
            build_boot_image(&out_dir);
        }
        "x86_64" => {
            // Phase 8 territory -- nothing to assemble yet.
        }
        other => panic!("unsupported target arch: {other}"),
    }
}

/// Build every boot server, pack the resulting ELFs into the MXBI boot-image
/// archive in `OUT_DIR`, and emit `BOOT_IMAGE_PATH` so `boot_image` can
/// `include_bytes!` it (slice 4.2). Generalizes the slice-3.4 single-VM embed.
///
/// The server list is the single source of truth for which servers boot and at
/// which proc number; the proc numbers must match `kernel-shared/src/com.rs`.
/// VM is built first so it takes ASID 1 and is enqueued first (its
/// `RECEIVE(ANY)` blocks immediately, matching the pre-4.2 boot behavior).
fn build_boot_image(out_dir: &std::path::Path) {
    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR unset"));
    let workspace = manifest.parent().expect("kernel manifest has no parent");

    // Libraries every server links against. Watched once (not per-server) so a
    // change re-runs build.rs and re-embeds. Each is watched as a directory so
    // cargo covers every submodule recursively — otherwise an edit to e.g. a new
    // `minix-ipc` module or DS request number would embed stale ELFs.
    for path in [
        workspace.join("minix-ipc/src"),
        workspace.join("server-rt/src"),
        workspace.join("kernel-shared/src"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // (cargo package, crate dir, boot proc number). Proc numbers come from
    // `kernel-shared/src/com.rs`; the archive carries them so the loader writes
    // the right proc slot.
    let servers: [(&str, std::path::PathBuf, i32); 3] = [
        ("minixrs-vm", workspace.join("servers/vm"), 7), // VM_PROC_NR
        ("minixrs-ds", workspace.join("servers/ds"), 5), // DS_PROC_NR
        ("minixrs-vfs", workspace.join("servers/vfs"), 1), // VFS_PROC_NR
    ];

    let mut modules: Vec<(i32, String, Vec<u8>)> = Vec::with_capacity(servers.len());
    for (crate_name, crate_dir, proc_nr) in &servers {
        let elf = build_server(crate_name, crate_dir, workspace, out_dir);
        let bytes = std::fs::read(&elf)
            .unwrap_or_else(|e| panic!("reading {crate_name} ELF {}: {e}", elf.display()));
        let name = crate_name.strip_prefix("minixrs-").unwrap_or(crate_name);
        modules.push((*proc_nr, name.to_string(), bytes));
    }

    let archive = pack_mxbi(&modules);
    let archive_path = out_dir.join("boot_image.mxbi");
    std::fs::write(&archive_path, &archive).expect("writing boot-image archive");
    println!("cargo:rustc-env=BOOT_IMAGE_PATH={}", archive_path.display());
}

/// Build one server crate for the aarch64 EL0 user target and return the path to
/// the produced ELF.
///
/// We reuse the builtin `aarch64-unknown-none` triple but must NOT inherit the
/// kernel's linker script: the workspace `.cargo/config.toml` forces
/// `-Tkernel/.../linker.ld` on that triple via `rustflags`. We override it with
/// `CARGO_ENCODED_RUSTFLAGS` (highest-precedence; replaces config rustflags
/// entirely) pointing at the server's `user.ld`, and isolate each build in its
/// own `CARGO_TARGET_DIR` so nesting cargo inside this build script does not
/// deadlock on the outer kernel build's target-dir lock.
fn build_server(
    crate_name: &str,
    crate_dir: &std::path::Path,
    workspace: &std::path::Path,
    out_dir: &std::path::Path,
) -> PathBuf {
    let user_ld = crate_dir.join("user.ld");
    let target_dir = out_dir.join(format!("{crate_name}-target"));

    // Rebuild + re-embed whenever this server's sources, linker script, or
    // manifest change. `src` is watched as a directory so submodules (e.g.
    // `servers/ds/src/registry.rs`) are covered — watching only `main.rs` would
    // silently embed a stale ELF after a submodule edit.
    for path in [
        crate_dir.join("src"),
        user_ld.clone(),
        crate_dir.join("Cargo.toml"),
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
            crate_name,
            "--target",
            "aarch64-unknown-none",
            "--release",
        ])
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo for {crate_name}: {e}"));
    if !status.success() {
        panic!("building {crate_name} (server ELF) failed");
    }

    let elf = target_dir.join(format!("aarch64-unknown-none/release/{crate_name}"));
    assert!(
        elf.exists(),
        "{crate_name} ELF missing at {}",
        elf.display()
    );
    elf
}

/// Pack server ELFs into the minix.rs boot-image (MXBI) archive:
///
/// ```text
///   16-byte header: magic "MXBI" (LE u32), version, entry_count, total_size
///   entry_count × 32-byte records: { proc_nr:i32, offset:u32, len:u32, name:[u8;20] }
///   then the ELF payloads back-to-back, each at its recorded offset
/// ```
///
/// All multi-byte fields are little-endian (build host and aarch64 target are
/// both LE); `boot_image::BootImage` parses this with matching `from_le_bytes`.
fn pack_mxbi(modules: &[(i32, String, Vec<u8>)]) -> Vec<u8> {
    const MAGIC: u32 = 0x4942_584D; // "MXBI" as little-endian bytes M,X,B,I
    const VERSION: u32 = 1;
    const HDR_LEN: usize = 16;
    const REC_LEN: usize = 32;
    const NAME_LEN: usize = 20;

    let n = modules.len();
    let payload_start = HDR_LEN + n * REC_LEN;

    // Build the record table, assigning each payload an offset past the table.
    let mut offset = payload_start;
    let mut records = Vec::with_capacity(n * REC_LEN);
    for (proc_nr, name, bytes) in modules {
        let name_bytes = name.as_bytes();
        assert!(
            name_bytes.len() < NAME_LEN,
            "server name {name:?} too long for MXBI {NAME_LEN}-byte name field"
        );
        records.extend_from_slice(&proc_nr.to_le_bytes());
        records.extend_from_slice(&(offset as u32).to_le_bytes());
        records.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        let mut name_field = [0u8; NAME_LEN];
        name_field[..name_bytes.len()].copy_from_slice(name_bytes);
        records.extend_from_slice(&name_field);
        offset += bytes.len();
    }
    let total_size = offset;

    let mut archive = Vec::with_capacity(total_size);
    archive.extend_from_slice(&MAGIC.to_le_bytes());
    archive.extend_from_slice(&VERSION.to_le_bytes());
    archive.extend_from_slice(&(n as u32).to_le_bytes());
    archive.extend_from_slice(&(total_size as u32).to_le_bytes());
    archive.extend_from_slice(&records);
    for (_, _, bytes) in modules {
        archive.extend_from_slice(bytes);
    }
    assert_eq!(archive.len(), total_size, "MXBI archive size mismatch");
    archive
}

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
    // The kernel is bare-metal only. It used to collapse to a no-op `main` on
    // host targets so `cargo check --workspace` stayed green, at the cost of
    // hiding every module behind `#[cfg(target_os = "none")]` — and therefore
    // from every lint gate.
    //
    // `forced-target` in Cargo.toml now pins this package to
    // aarch64-unknown-none for *every* cargo invocation, so in practice this
    // arm is unreachable. It stays as defense-in-depth: if that unstable
    // feature is ever dropped, this turns a cascade of `no_main` /
    // unset-`BOOT_IMAGE_PATH` errors into one actionable message, and it fires
    // before rustc runs.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "none" {
        println!(
            "cargo::error=minixrs-kernel is a bare-metal crate (target_os = \"none\"); it \
             cannot be built for the host (got target_os = \"{target_os}\"). Build it with \
             `cargo kernel-aarch64`. If you are seeing this at all, the `forced-target` key \
             in kernel/Cargo.toml is not taking effect — check that cargo still supports the \
             unstable `per-package-target` feature."
        );
        return;
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH unset");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR unset"));

    // The `boot-stubs` feature (default-on) gates the demo stubs A–D. Build
    // scripts can't see `#[cfg(feature = ...)]`, so read the env var cargo sets
    // for enabled features. Off ⇒ skip `user_stub.S` assembly and build PM
    // stub-free so the two crates agree on `NR_STUB_PROCS`.
    let stubs = std::env::var_os("CARGO_FEATURE_BOOT_STUBS").is_some();

    match arch.as_str() {
        "aarch64" => {
            let mut sources = vec![
                "src/arch/aarch64/entry.S",
                "src/arch/aarch64/vectors.S",
                "src/arch/aarch64/trap.S",
                "src/arch/aarch64/interrupt.S",
            ];
            if stubs {
                sources.push("src/arch/aarch64/user_stub.S");
            }
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
            build_boot_image(&out_dir, stubs);
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
fn build_boot_image(out_dir: &std::path::Path, stubs: bool) {
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
        workspace.join("minixrs-abi-note/src"),
        workspace.join("tools/targets/aarch64-unknown-minixrs.json"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // (cargo package, crate dir, boot proc number). Proc numbers come from
    // `kernel-shared/src/com.rs`; the archive carries them so the loader writes
    // the right proc slot. `worker` is packed with proc_nr -1
    // (`com::EXEC_ONLY_PROC_NR`): it is not a boot server — the loader skips any
    // negative proc_nr — but it is resolvable by name for `SYS_EXEC` (slice 4.7).
    let servers: [(&str, std::path::PathBuf, i32); 9] = [
        ("minixrs-vm", workspace.join("servers/vm"), 7), // VM_PROC_NR
        ("minixrs-ds", workspace.join("servers/ds"), 5), // DS_PROC_NR
        // TTY sits between DS and VFS on purpose. DS must come first so every
        // later server's `DS_PUBLISH` lands; the console driver should reach its
        // receive loop before its first client, which is VFS today (slice 5.3's
        // demo) and stays VFS for 5.4's fd 1/2 and 5.6's musl `printf`.
        ("minixrs-tty", workspace.join("drivers/tty"), 4), // TTY_PROC_NR
        ("minixrs-vfs", workspace.join("servers/vfs"), 1), // VFS_PROC_NR
        ("minixrs-sched", workspace.join("servers/sched"), 9), // SCHED_PROC_NR
        ("minixrs-rs", workspace.join("servers/rs"), 2),   // RS_PROC_NR
        ("minixrs-pm", workspace.join("servers/pm"), 0),   // PM_PROC_NR
        ("minixrs-init", workspace.join("userland/init"), 10), // INIT_PROC_NR — PID 1
        ("minixrs-worker", workspace.join("userland/worker"), -1), // EXEC_ONLY_PROC_NR
    ];

    let mut modules: Vec<(i32, String, Vec<u8>)> = Vec::with_capacity(servers.len());
    for (crate_name, crate_dir, proc_nr) in &servers {
        // PM seeds mproc slots for the stubs, so it must resolve the same
        // `NR_STUB_PROCS` as the kernel. This nested build has its own cargo
        // feature resolution, so when the kernel is stub-free force PM's
        // `boot-stubs` off too. Only PM (of the servers) depends on the count.
        let extra: &[&str] = if !stubs && *crate_name == "minixrs-pm" {
            &["--no-default-features"]
        } else {
            &[]
        };
        let elf = build_server(crate_name, crate_dir, workspace, extra);
        let bytes = std::fs::read(&elf)
            .unwrap_or_else(|e| panic!("reading {crate_name} ELF {}: {e}", elf.display()));
        // Pack-time gate: refuse to embed an unbranded/foreign ELF. Runtime
        // enforcement in load_exec_image stays authoritative — exec-from-FS
        // (slice 5.9) bypasses this assertion entirely.
        if let Err(e) = minixrs_kernel_shared::brand::scan_brand(&bytes) {
            panic!("{}: missing/bad minixrs brand: {e:?}", elf.display());
        }
        let name = crate_name.strip_prefix("minixrs-").unwrap_or(crate_name);
        modules.push((*proc_nr, name.to_string(), bytes));
    }

    let archive = pack_mxbi(&modules);
    let archive_path = out_dir.join("boot_image.mxbi");
    std::fs::write(&archive_path, &archive).expect("writing boot-image archive");
    println!("cargo:rustc-env=BOOT_IMAGE_PATH={}", archive_path.display());
}

/// Build one user-space crate for the custom `aarch64-unknown-minixrs` target
/// (`tools/targets/aarch64-unknown-minixrs.json`, via `-Zbuild-std`) and return
/// the path to the produced ELF.
///
/// The `-T<user.ld>` link arg comes from each crate's own build.rs, cfg-gated
/// on `target_os = "minixrs"` — nothing is injected here. Inherited rustflags
/// are scrubbed instead, so the kernel's `-Tlinker.ld` (keyed on
/// `aarch64-unknown-none` in `.cargo/config.toml`, which the JSON target name
/// doesn't match anyway) or a developer's env can't leak in.
fn build_server(
    crate_name: &str,
    crate_dir: &std::path::Path,
    workspace: &std::path::Path,
    extra_args: &[&str],
) -> PathBuf {
    // One shared nested target dir (not per-crate): -Zbuild-std would otherwise
    // rebuild core+alloc 9x per kernel build. Nested invocations run
    // sequentially from this script; cargo's own locking covers any overlap
    // with developer commands. Still separate from the outer `target/` root,
    // so nesting cargo here cannot deadlock on the kernel build's lock.
    let target_dir = workspace.join("target/minixrs-user");
    let target_json = workspace.join("tools/targets/aarch64-unknown-minixrs.json");

    // Rebuild + re-embed whenever this server's sources, linker script, manifest,
    // or own build script change. `src` is watched as a directory so submodules
    // (e.g. `servers/ds/src/registry.rs`) are covered — watching only `main.rs`
    // would silently embed a stale ELF after a submodule edit. `build.rs` is
    // watched because it emits the crate's `-T<user.ld>` link arg (M1): cargo
    // reruns the *nested* build for it, but only this line makes the *outer*
    // kernel build notice and re-pack.
    for path in [
        crate_dir.join("src"),
        crate_dir.join("user.ld"),
        crate_dir.join("Cargo.toml"),
        crate_dir.join("build.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(workspace)
        .args(["build", "-p", crate_name, "--target"])
        .arg(&target_json)
        .args([
            "--release",
            // Custom target ⇒ no prebuilt core/alloc; `compiler-builtins-mem`
            // provides memcpy/memset (harmless if a future nightly defaults it).
            "-Zbuild-std=core,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .args(extra_args)
        .env("CARGO_TARGET_DIR", &target_dir)
        // The servers' -T<user.ld> now comes from each crate's own build.rs
        // (cfg-gated on target_os = "minixrs"); scrub any inherited rustflags
        // so the kernel's -Tlinker.ld (or a user's env) can't leak in.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo for {crate_name}: {e}"));
    if !status.success() {
        panic!("building {crate_name} (server ELF) failed");
    }

    let elf = target_dir.join(format!("aarch64-unknown-minixrs/release/{crate_name}"));
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

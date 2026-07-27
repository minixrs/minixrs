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
    // `minixrs-ipc` module or DS request number would embed stale ELFs.
    for path in [
        workspace.join("minixrs-ipc/src"),
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
    let servers: [(&str, std::path::PathBuf, i32); 10] = [
        ("minixrs-vm", workspace.join("servers/vm"), 7), // VM_PROC_NR
        ("minixrs-ds", workspace.join("servers/ds"), 5), // DS_PROC_NR
        // The drivers sit between DS and VFS on purpose. DS must come first so
        // every later server's `DS_PUBLISH` lands; each driver should reach its
        // receive loop before its first client, which is VFS today (slice 5.3's
        // console demo and 5.7's block demo) and stays VFS for 5.4's fd 1/2 and
        // 5.6's musl `printf`. MEM specifically precedes VFS so 5.7's demo lookup
        // — and 5.8's MFS lookup, which is the one that matters — resolve through
        // DS deterministically by construction rather than by luck.
        ("minixrs-tty", workspace.join("drivers/tty"), 4), // TTY_PROC_NR
        ("minixrs-memory", workspace.join("drivers/memory"), 3), // MEM_PROC_NR
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

    // `hello` — the first C program (slice 5.6), built from userland/hello
    // against the musl fork rather than by cargo. Packed under proc_nr -1 like
    // `worker`: resolvable by name for SYS_EXEC, never boot-loaded.
    //
    // When the musl sysroot is absent or stale, fall back to packing the
    // `worker` ELF *under the name* `hello`. Presence-checking the SYSROOT
    // rather than the submodule is deliberate: a fresh clone that ran
    // `git submodule update` but not `tools/build-musl.sh` would otherwise
    // trigger a multi-minute musl build from inside a cargo build script. The
    // fallback keeps every boot green with no feature flag — only the
    // hello-specific markers go missing.
    let hello_bytes = match build_hello(workspace) {
        Some(bytes) => bytes,
        None => {
            println!(
                "cargo::warning=musl sysroot missing or stale; packing `worker` as `hello`. \
                 Run tools/build-musl.sh to build the real C program (slice 5.6)."
            );
            modules
                .iter()
                .find(|(_, name, _)| name == "worker")
                .map(|(_, _, bytes)| bytes.clone())
                .expect("worker must be packed before the hello fallback")
        }
    };
    // Same pack-time brand gate the servers get. For the real C build this is
    // what turns a regressed crt1.c brand block into a *build* failure rather
    // than a boot failure.
    //
    // (The gate is written out at all three sites rather than factored into a
    // per-module helper on purpose: the `rootfs` blob below is NOT an ELF, and a
    // helper that branded every module would panic on it.)
    if let Err(e) = minixrs_kernel_shared::brand::scan_brand(&hello_bytes) {
        panic!("hello: missing/bad minixrs brand: {e:?}");
    }
    modules.push((-1, "hello".to_string(), hello_bytes.clone())); // EXEC_ONLY_PROC_NR

    // The root filesystem image (slice 5.7, decision D3): a MinixFS v3 image built
    // here and packed as a non-ELF blob, which the kernel copies into RAM frames
    // and maps into the `memory` driver's address space at boot.
    //
    // Deliberately a separate statement outside the server loop, and with **no**
    // `scan_brand` gate: it is not an ELF and has no `.note.minixrs.ident`.
    //
    // `hello_bytes` is reused rather than calling `build_hello` a second time.
    // Building it twice would let the image and the `hello` module disagree in the
    // musl-sysroot-absent configuration, where `build_hello` returns `None` and the
    // fallback substitutes the `worker` ELF — the image would then hold whichever
    // one the second call produced.
    modules.push((
        -1,
        "rootfs".to_string(), // com::ROOTFS_MODULE_NAME
        build_rootfs(&hello_bytes),
    ));

    let archive = pack_mxbi(&modules);
    let archive_path = out_dir.join("boot_image.mxbi");
    std::fs::write(&archive_path, &archive).expect("writing boot-image archive");
    println!("cargo:rustc-env=BOOT_IMAGE_PATH={}", archive_path.display());
}

/// Build the MinixFS v3 root filesystem image (slice 5.7).
///
/// The image is a **fixed** 1 MiB regardless of its contents (see
/// `kernel_shared::rootfs::ROOTFS_IMAGE_BLOCKS`), so every size-derived boot
/// marker is a literal in every build configuration — including the one where
/// `build_hello` fell back to the 15 KB `worker` ELF.
///
/// Three files, and each earns its place:
///
///   * `/bin/hello` — what slice 5.9 execs out of the filesystem. Passed in rather
///     than rebuilt, so the image and the archive's `hello` module can never
///     disagree.
///   * `/etc/motd` — a greppable line for slice 5.8's read proof, which needs a
///     file whose *contents* it can assert on rather than just its length.
///   * `/etc/pattern` — 40 KiB, and **mandatory rather than filler**. It is what
///     keeps the single-indirect zone arm (and mkfs's indirect writer) live in
///     *both* configurations: the real `hello` is ~200 KB and needs the indirect
///     block, but the fallback `worker` is 15 KB and fits inside the seven direct
///     zones, so without a constant-size file past that boundary the indirect path
///     would be dead code in exactly the configuration CI's non-QEMU jobs build.
fn build_rootfs(hello_bytes: &[u8]) -> Vec<u8> {
    use minixrs_mkfs_mfs::Manifest;

    // Position-dependent and non-repeating over a block, so a copy that lost,
    // duplicated, or reordered a block changes the bytes. Same generator the
    // mkfs round-trip tests use.
    const PATTERN_LEN: usize = 40 * 1024;
    let pattern: Vec<u8> = (0..PATTERN_LEN).map(|i| (i % 251) as u8).collect();

    let mut manifest = Manifest::new();
    manifest
        .add("/bin/hello", hello_bytes.to_vec())
        .add("/etc/motd", b"minix.rs rootfs: motd from MFS\n".to_vec())
        .add("/etc/pattern", pattern);

    minixrs_mkfs_mfs::build_image(&manifest)
        .unwrap_or_else(|e| panic!("building the root filesystem image: {e}"))
}

/// Build `userland/hello` — the slice-5.6 C milestone — against the musl fork's
/// sysroot, returning the linked ELF's bytes.
///
/// Returns `None` when the sysroot is missing or stale, so the caller can fall
/// back to packing `worker` under the name `hello`. It deliberately does **not**
/// build the sysroot itself: that is `tools/build-musl.sh`'s job, and kicking off
/// a multi-minute musl build from inside a cargo build script would turn a fresh
/// clone's first `cargo kernel-aarch64` into a mystery.
///
/// This is not a cargo crate — it is a `.c` and a `.ld` compiled and linked here
/// directly, because cargo has no notion of a C program linked against a
/// foreign libc. clang is already a hard build requirement (the kernel's `.S`
/// files go through it) and `rust-lld` ships with the pinned toolchain, so no
/// platform linker and no Homebrew LLVM are involved.
fn build_hello(workspace: &std::path::Path) -> Option<Vec<u8>> {
    let src = workspace.join("userland/hello/hello.c");
    let script = workspace.join("userland/hello/hello.ld");
    let sysroot = workspace.join("target/musl-sysroot");
    let stamp = sysroot.join(".stamp");

    for path in [&src, &script, &stamp] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    if !stamp.is_file() || !sysroot.join("lib/libc.a").is_file() {
        return None;
    }

    let sysroot_inc = sysroot.join("include");
    let lib = sysroot.join("lib");
    let out = workspace.join("target/hello");
    std::fs::create_dir_all(&out).expect("creating target/hello");
    let obj = out.join("hello.o");
    let elf = out.join("hello");

    // -nostdinc + -isystem <sysroot>/include: the fork's headers and nothing
    // else. The host libc must not creep in — its errno values differ.
    let cc = std::process::Command::new("clang")
        .args(["--target=aarch64-unknown-linux-musl", "-nostdinc"])
        .arg("-isystem")
        .arg(&sysroot_inc)
        .args(["-ffreestanding", "-O2", "-Wall", "-Wextra", "-Werror", "-c"])
        .arg("-o")
        .arg(&obj)
        .arg(&src)
        .status()
        .expect("running clang for userland/hello");
    assert!(
        cc.success(),
        "clang failed to compile userland/hello/hello.c"
    );

    // musl's vfprintf pulls in soft-float binary128 helpers (__multf3,
    // __floatsitf, …): aarch64's `long double` is IEEE quad with no hardware
    // support, and musl's configure finds no runtime library on this host. The
    // pinned toolchain's own `compiler_builtins`, built for the custom target by
    // the server builds above, exports exactly those C-ABI names — so the
    // dependency is satisfied from the toolchain rather than by adding an
    // LLVM/compiler-rt build to the tree. (The *prebuilt* aarch64-unknown-none
    // rlib in the rustup sysroot does NOT export them; only the build-std one
    // does, which is why this globs the nested target dir.)
    let builtins = find_compiler_builtins(workspace)
        .expect("compiler_builtins rlib not found under target/minixrs-user");

    let lld = rustlib_bin("rust-lld").expect("rust-lld not found in the Rust sysroot");
    let link = std::process::Command::new(&lld)
        .args(["-flavor", "gnu", "-T"])
        .arg(&script)
        .arg("-o")
        .arg(&elf)
        .arg(lib.join("crt1.o"))
        .arg(lib.join("crti.o"))
        .arg(&obj)
        .arg(lib.join("libc.a"))
        .arg(&builtins)
        .arg(lib.join("crtn.o"))
        // D13: 4 KiB pages, and no two PT_LOADs sharing a page — the kernel's
        // loader maps segment-by-segment with per-segment permissions.
        .args([
            "-z",
            "max-page-size=4096",
            "-z",
            "separate-loadable-segments",
        ])
        .status()
        .expect("running rust-lld for userland/hello");
    assert!(link.success(), "rust-lld failed to link userland/hello");

    Some(std::fs::read(&elf).expect("reading the linked hello ELF"))
}

/// Locate the `compiler_builtins` rlib the nested `-Zbuild-std` builds produced.
fn find_compiler_builtins(workspace: &std::path::Path) -> Option<PathBuf> {
    let deps = workspace.join("target/minixrs-user/aarch64-unknown-minixrs/release/deps");
    let mut hits: Vec<PathBuf> = std::fs::read_dir(&deps)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libcompiler_builtins-") && n.ends_with(".rlib"))
        })
        .collect();
    // Deterministic pick if a stale hash lingers beside a fresh one.
    hits.sort();
    hits.pop()
}

/// Path to a tool shipped in the Rust sysroot's `lib/rustlib/*/bin`.
fn rustlib_bin(name: &str) -> Option<PathBuf> {
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = PathBuf::from(String::from_utf8(sysroot.stdout).ok()?.trim());
    let rustlib = sysroot.join("lib/rustlib");
    for entry in std::fs::read_dir(&rustlib).ok()?.flatten() {
        let candidate = entry.path().join("bin").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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
            // The pinned nightly gates `.json` target specs behind this flag.
            // It must be passed explicitly here: a developer's *global*
            // `~/.cargo/config.toml` may carry `[unstable] json-target-spec`
            // (RustRover needs it), which masks its absence locally while CI —
            // which has no such config — fails the nested build.
            "-Zjson-target-spec",
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
        // The record's offset and length are u32 on the wire. These casts were
        // unchecked until slice 5.7 added a 1 MiB non-ELF blob to the archive —
        // the same latent-cast class the workspace's `checked_add` convention
        // exists to kill. A silent truncation here would produce an archive whose
        // table points into the middle of a payload.
        assert!(
            bytes.len() <= u32::MAX as usize,
            "MXBI module {name:?} is {} bytes, past the u32 length field",
            bytes.len()
        );
        assert!(
            offset <= u32::MAX as usize,
            "MXBI archive exceeds the u32 offset field at module {name:?}"
        );
        records.extend_from_slice(&proc_nr.to_le_bytes());
        records.extend_from_slice(&(offset as u32).to_le_bytes());
        records.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        let mut name_field = [0u8; NAME_LEN];
        name_field[..name_bytes.len()].copy_from_slice(name_bytes);
        records.extend_from_slice(&name_field);
        offset += bytes.len();
    }
    // The loop checks `offset` *before* adding the module's length, so the final
    // total is the one value it cannot have covered: the last module may start
    // inside the u32 range and end past it. `total_size` is itself a u32 header
    // field, so check it here or the cast below truncates the archive's own
    // recorded size while every per-record assert passes.
    assert!(
        offset <= u32::MAX as usize,
        "MXBI archive is {offset} bytes, past the u32 total-size field"
    );
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

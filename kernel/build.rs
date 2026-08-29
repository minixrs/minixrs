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

use minixrs_kernel_shared::callnr::VFS_EXEC_MAX;

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
                let status = clang_command("clang")
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

/// Which toolchain built the packed `hello` module, in strict preference order.
///
/// `Musl` is **not** a fallback, and the distinction matters. CI's blocking
/// `qemu-smoke` job cannot install an SDK — an LLVM build is hours — while
/// `tests/qemu-boot.expected` *requires* the five C markers, so the in-tree
/// `tools/build-musl.sh` sysroot is that gate's real dependency and has to keep
/// working. Selection is genuinely three-way: SDK, then in-tree musl, then no C
/// toolchain at all.
///
/// Only `Worker` is a fallback, and only it loses markers.
enum HelloFlavor {
    /// `$MINIXRS_SDK` (M3): one `clang --target=aarch64-unknown-minixrs` call
    /// does the whole job. Carries the prefix and the sysroot stamp for the
    /// host-side report — the boot log cannot distinguish flavors.
    Sdk { prefix: PathBuf, stamp: String },
    /// `target/musl-sysroot` (slice 5.6): the stand-in triple
    /// `aarch64-unknown-linux-musl`, `hello.ld`, and an explicit `rust-lld` line.
    Musl,
    /// No C toolchain: the `worker` ELF packed *under the name* `hello`.
    Worker,
}

/// A `clang` invocation with the host's C environment scrubbed.
///
/// This is **required, not hygiene.** clang's driver folds `CPATH` and the
/// `*_INCLUDE_PATH` family into the *front* of the include search list — ahead of
/// its own resource dir and ahead of the sysroot — and `-nostdinc` does **not**
/// suppress them. On a machine with `C_INCLUDE_PATH` set, the search order for
/// `--target=aarch64-unknown-minixrs` really begins with that foreign directory:
///
/// ```text
///   $ clang -E -v --target=aarch64-unknown-minixrs -x c /dev/null
///    /Users/…/.wasmedge/include                    <- from C_INCLUDE_PATH, FIRST
///    …/toolchains/minixrs/lib/clang/22/include
///    …/toolchains/minixrs/sysroot/usr/include
/// ```
///
/// A foreign `errno.h` reachable there would shadow musl's, and phase-5 decision
/// D7 turns on the fork's errno *values* being the ones the kernel agrees with.
/// (The pre-SDK musl path leaks the same directory; it is harmless there only by
/// accident, because its explicit `-isystem` happens to sort ahead of it.)
/// `LIBRARY_PATH` and `SDKROOT` get the same treatment for the link step.
///
/// Same reflex as `build_server`'s `.env_remove("RUSTFLAGS")`: a cross build must
/// not inherit the host's idea of where things live.
fn clang_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    for var in [
        "CPATH",
        "C_INCLUDE_PATH",
        "CPLUS_INCLUDE_PATH",
        "OBJC_INCLUDE_PATH",
        "OBJCPLUS_INCLUDE_PATH",
        "LIBRARY_PATH",
        "SDKROOT",
    ] {
        cmd.env_remove(var);
    }
    cmd
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
    let servers: [(&str, std::path::PathBuf, i32); 11] = [
        ("minixrs-vm", workspace.join("servers/vm"), 7), // VM_PROC_NR
        ("minixrs-ds", workspace.join("servers/ds"), 5), // DS_PROC_NR
        // **This ordering is load-bearing, and it is a chain, not a preference.**
        // DS must come first so every later server's `DS_PUBLISH` lands. After
        // that, each server has to reach its receive loop before its first client
        // does a `DS_RETRIEVE`, because publish-before-retrieve is not guaranteed
        // by construction — it holds only because the archive is packed in this
        // order. The chain as of slice 5.9 is:
        //
        //     ds  <  tty  <  memory  <  mfs  <  vfs  <  pm
        //
        // TTY and MEM before VFS/MFS (their clients), **MEM before MFS**
        // (MFS's `bdev.ds` lookup), **MFS before VFS** (VFS's `fs.ds` lookup),
        // and — since slice 5.9 made PM a client of VFS for `VFS_EXEC_STAGE` —
        // **VFS before PM** (PM's `vfs.ds` lookup). Each of those three lookups
        // has a `boot_endpoint` fallback and a distinguishable diag line, so a
        // regression here turns CI red on the `bdev.ds ok` / `fs.ds ok` /
        // `vfs.ds ok` markers specifically rather than hanging.
        ("minixrs-tty", workspace.join("drivers/tty"), 4), // TTY_PROC_NR
        ("minixrs-memory", workspace.join("drivers/memory"), 3), // MEM_PROC_NR
        ("minixrs-mfs", workspace.join("fs/mfs"), 6),      // MFS_PROC_NR
        ("minixrs-vfs", workspace.join("servers/vfs"), 1), // VFS_PROC_NR
        ("minixrs-sched", workspace.join("servers/sched"), 9), // SCHED_PROC_NR
        ("minixrs-rs", workspace.join("servers/rs"), 2),   // RS_PROC_NR
        ("minixrs-pm", workspace.join("servers/pm"), 0),   // PM_PROC_NR
        ("minixrs-init", workspace.join("userland/init"), 10), // INIT_PROC_NR — PID 1
        ("minixrs-worker", workspace.join("userland/worker"), -1), // EXEC_ONLY_PROC_NR
    ];

    let mut modules: Vec<(i32, String, Vec<u8>)> = Vec::with_capacity(servers.len());
    for (crate_name, crate_dir, proc_nr) in &servers {
        // Per-crate feature flags for the nested build, which has its own feature
        // resolution and inherits nothing from the kernel's.
        //
        //   * PM seeds mproc slots for the stubs, so it must resolve the same
        //     `NR_STUB_PROCS` as the kernel: when the kernel is stub-free, force
        //     PM's `boot-stubs` off too. Only PM (of the servers) depends on the
        //     count.
        //   * MFS keeps its server half behind a `server` feature so the *format
        //     library* — which this very build script reaches through the
        //     `tools/mkfs-mfs` build-dependency — stays a one-dependency crate.
        //     Without this flag the nested build would produce a library and no
        //     ELF, and `build_server`'s existence assertion would fire.
        let extra: &[&str] = match *crate_name {
            "minixrs-pm" if !stubs => &["--no-default-features"],
            "minixrs-mfs" => &["--features", "server"],
            _ => &[],
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

    // `hello` — the first C program (slice 5.6), built from userland/hello rather
    // than by cargo. Packed under proc_nr -1 like `worker`: resolvable by name for
    // SYS_EXEC, never boot-loaded.
    //
    // With neither C toolchain available, fall back to packing the `worker` ELF
    // *under the name* `hello`. Presence-checking the SYSROOTS rather than the
    // musl submodule is deliberate: a fresh clone that ran `git submodule update`
    // but not `tools/build-musl.sh` would otherwise trigger a multi-minute musl
    // build from inside a cargo build script. The fallback keeps every boot green
    // with no feature flag — only the hello-specific markers go missing.
    let src = workspace.join("userland/hello/hello.c");
    println!("cargo:rerun-if-changed={}", src.display());
    let (hello_flavor, hello_bytes) = match build_hello(workspace, &src) {
        Some(pair) => pair,
        None => {
            let bytes = modules
                .iter()
                .find(|(_, name, _)| name == "worker")
                .map(|(_, _, bytes)| bytes.clone())
                .expect("worker must be packed before the hello fallback");
            (HelloFlavor::Worker, bytes)
        }
    };

    // Which toolchain built it, reported **host-side only**. There is deliberately
    // no boot marker and no kernel change: the five C markers are byte-identical
    // across the SDK and musl flavors, so the log physically cannot distinguish
    // them and these lines plus the host-side ELF checks are what do.
    //
    // `Musl` stays silent on purpose. It is what every CI job builds, and warning
    // on the norm is how people learn to ignore build-script warnings — which
    // would cost us the two that actually mean something.
    match &hello_flavor {
        HelloFlavor::Sdk { prefix, stamp } => println!(
            "cargo::warning=hello: built with the minix.rs SDK at {} ({stamp})",
            prefix.display()
        ),
        HelloFlavor::Musl => {}
        HelloFlavor::Worker => println!(
            "cargo::warning=no C toolchain for `hello`; packing `worker` under that name. \
             Run tools/build-musl.sh for the in-tree musl sysroot, or install the SDK and \
             set $MINIXRS_SDK (slice 5.6 / P3c)."
        ),
    }
    // Same pack-time brand gate the servers get. For the real C build this is
    // what turns a regressed crt1.c brand block into a *build* failure rather
    // than a boot failure.
    //
    // (The gate is written out at all three sites rather than factored into a
    // per-module helper on purpose: the `rootfs` blob below is NOT an ELF, and a
    // helper that branded every module would panic on it.)
    //
    // Note this is now the *only* pack-time check `hello` gets, and it no longer
    // covers the copy that runs: slice 5.9 execs `/bin/hello` out of the
    // filesystem, where the runtime `scan_brand` in `elf::load_into` is the sole
    // gate. It stays because the two copies are the same bytes, so a regressed
    // `crt1.c` brand block is still a *build* failure rather than a boot one.
    if let Err(e) = minixrs_kernel_shared::brand::scan_brand(&hello_bytes) {
        panic!("hello: missing/bad minixrs brand: {e:?}");
    }

    // **`hello` is deliberately NOT packed as a boot-archive module** (slice 5.9).
    // It was, from 5.6 until now, and keeping it would have made the C markers
    // unable to distinguish a boot-archive exec from a filesystem one — the same
    // bytes reach the console either way, which is the 5.5/5.6 "byte-identical
    // markers" trap. With the module gone, `/bin/hello` in the image below is the
    // only copy, so `minix.rs hello: Hello from C!` is now *proof* that the whole
    // stage-and-grant chain worked. `worker` stays boot-embedded as the name-form
    // regression, which is why `EXEC_ONLY_PROC_NR` is still in use.
    assert!(
        hello_bytes.len() <= VFS_EXEC_MAX,
        "hello is {} bytes, past VFS's staging buffer ({VFS_EXEC_MAX}). \
         Raise `callnr::VFS_EXEC_MAX` — it is the one constant that bounds this.",
        hello_bytes.len(),
    );

    // The root filesystem image (slice 5.7, decision D3): a MinixFS v3 image built
    // here and packed as a non-ELF blob, which the kernel copies into RAM frames
    // and maps into the `memory` driver's address space at boot.
    //
    // Deliberately a separate statement outside the server loop, and with **no**
    // `scan_brand` gate: it is not an ELF and has no `.note.minixrs.ident`.
    //
    // `hello_bytes` is reused rather than calling `build_hello` a second time.
    // Building it twice would let two copies disagree in the musl-sysroot-absent
    // configuration, where `build_hello` returns `None` and the fallback
    // substitutes the `worker` ELF — the image would then hold whichever one the
    // second call produced. As of slice 5.9 the image is the *only* copy, so this
    // is the one that runs.
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
///
/// **Every path and every byte comes from `kernel_shared::rootfs`** as of slice
/// 5.8, not from literals here. The MFS server reads the same constants back over
/// BDEV and compares, so its `fs.selfcheck` / `fs.indirect` boot markers are a
/// *check* rather than a transcription — the failure mode where both sides get
/// edited together and the proof keeps passing while the content silently changed.
fn build_rootfs(hello_bytes: &[u8]) -> Vec<u8> {
    use minixrs_kernel_shared::rootfs::{
        ROOTFS_DENY_PATH, ROOTFS_FULL_DIR, ROOTFS_FULL_ENTRIES, ROOTFS_HELLO_PATH,
        ROOTFS_HOLEY_LEN, ROOTFS_HOLEY_PATH, ROOTFS_MOTD, ROOTFS_MOTD_PATH, ROOTFS_PATTERN_LEN,
        ROOTFS_PATTERN_PATH, ROOTFS_RUNTIME_INODES, ROOTFS_RUNTIME_ZONES, ROOTFS_SCRATCH_PATH,
        rootfs_holey_byte, rootfs_pattern_byte,
    };
    use minixrs_mkfs_mfs::Manifest;

    let pattern: Vec<u8> = (0..ROOTFS_PATTERN_LEN).map(rootfs_pattern_byte).collect();
    let holey: Vec<u8> = (0..ROOTFS_HOLEY_LEN).map(rootfs_holey_byte).collect();

    let mut manifest = Manifest::new();
    manifest
        .add(ROOTFS_HELLO_PATH, hello_bytes.to_vec())
        .add(ROOTFS_MOTD_PATH, ROOTFS_MOTD.to_vec())
        .add(ROOTFS_PATTERN_PATH, pattern)
        .add(ROOTFS_SCRATCH_PATH, Vec::new())
        // Slice 5.10b. `/etc/holey` is sparse: its first block is a hole, so a
        // write at position 0 assigns a zone without moving the file's size --
        // the only way to reach the `dirty` half of MFS's write-back condition,
        // since with no `lseek` every write runs forward and extends the file.
        .add_sparse(ROOTFS_HOLEY_PATH, holey, minixrs_mfs::MFS_BLOCK_SIZE)
        // The `EEXIST` probe's target, read by nothing else.
        .add(ROOTFS_DENY_PATH, Vec::new());

    // `/full` ships exactly enough zero-length files that its single directory
    // block is full, so the one create init makes in it *must* allocate a second
    // directory zone. Without it, directory growth is an arm no QEMU boot
    // executes -- the failure mode `/etc/pattern` and the device-teardown
    // selftest exist to prevent. Names are formatted here rather than named in
    // `kernel-shared` because nothing at run time resolves them; only the count
    // is shared, and `rootfs.rs` const-asserts the arithmetic.
    for i in 0..ROOTFS_FULL_ENTRIES {
        manifest.add(format!("{ROOTFS_FULL_DIR}/f{i:02}"), Vec::new());
    }

    let img = minixrs_mkfs_mfs::build_image(&manifest)
        .unwrap_or_else(|e| panic!("building the root filesystem image: {e}"));

    // The scratch file ships empty, so every zone it ends up with is allocated by
    // MFS at *runtime*. Checked here, against the bytes just built, because it
    // cannot be settled anywhere else: the image's largest file is `/bin/hello`,
    // whose size is a property of the toolchain flavour (~200 KB with in-tree
    // musl, ~47 KB with the SDK, ~15 KB in the sysroot-absent fallback), so a unit
    // test over a fixture manifest would be measuring something that is not this
    // image. Without it, contents growing past the headroom is `ENOSPC` on the
    // first write and surfaces only as `fs.write FAIL short` in a QEMU boot.
    let free = minixrs_mkfs_mfs::verify::free_zones(&img)
        .expect("the image just built decodes its own layout");
    assert!(
        free >= ROOTFS_RUNTIME_ZONES,
        "the root image leaves {free} free zones, but the boot-time probes need \
         {ROOTFS_RUNTIME_ZONES} to grow at runtime. Its contents have outgrown \
         ROOTFS_IMAGE_BLOCKS -- raise that constant, or shrink what the image ships."
    );

    let free = minixrs_mkfs_mfs::verify::free_inodes(&img)
        .expect("the image just built decodes its own layout");
    assert!(
        free >= ROOTFS_RUNTIME_INODES,
        "the root image leaves {free} free inodes, but the boot-time probes create \
         {ROOTFS_RUNTIME_INODES} files. Raise ROOTFS_NINODES."
    );

    img
}

/// Locate a usable minix.rs SDK (`$MINIXRS_SDK`), or `None`.
///
/// The prefix is `$MINIXRS_SDK` when set, else `$HOME/toolchains/minixrs`. That
/// default is **contractual**, not a guess — it is the layout tooling's
/// `docs/sysroot-layout.md` documents and the shape `cmake/minixrs.cmake` uses.
/// Nothing else about the SDK may be hard-coded here.
///
/// "Usable" is exactly three files: `bin/clang`, `sysroot/.stamp`, and
/// `sysroot/usr/lib/libc.a`. The crt objects, `libclang_rt.builtins.a` and the
/// `lib/clang/<ver>` resource dir are deliberately **not** probed — the driver
/// names those itself, and one driver invocation never mentions the version
/// component, which is tooling's rule: derive it, never hard-code the `22`. A
/// sysroot missing them is a *broken* SDK, and a broken SDK must fail loudly
/// rather than quietly demote to another flavor (see `build_hello_sdk`).
///
/// An **explicitly set** but unusable `MINIXRS_SDK` warns once and falls through
/// — which is also how `MINIXRS_SDK=/nonexistent` forces the musl flavor. An
/// unset variable whose default prefix does not exist stays silent, since that is
/// the ordinary case for everyone without an SDK.
fn usable_sdk() -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed=MINIXRS_SDK");

    let explicit = std::env::var_os("MINIXRS_SDK");
    let prefix = match &explicit {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var_os("HOME")?).join("toolchains/minixrs"),
    };

    // Watched **unconditionally**, exactly as the musl stamp is. Watching it only
    // when it exists looks like a free optimization — it would stop build.rs
    // re-running for the SDK-less builds that are the common case — but it would
    // break the documented flow "run tooling's build-sysroot.sh, then
    // `cargo kernel-aarch64`": a declared-but-missing rerun-if-changed path is
    // precisely what makes cargo notice the stamp *appearing*.
    let stamp = prefix.join("sysroot/.stamp");
    println!("cargo:rerun-if-changed={}", stamp.display());

    for path in [
        prefix.join("bin/clang"),
        stamp,
        prefix.join("sysroot/usr/lib/libc.a"),
    ] {
        if !path.is_file() {
            if explicit.is_some() {
                println!(
                    "cargo::warning=MINIXRS_SDK is set to {} but {} is missing; \
                     falling back to the in-tree musl sysroot.",
                    prefix.display(),
                    path.display()
                );
            }
            return None;
        }
    }
    Some(prefix)
}

/// Pick a `hello` flavor and build it: the SDK if one is usable, else the in-tree
/// musl sysroot. `None` means no C toolchain at all, and the caller substitutes
/// the `worker` ELF.
fn build_hello(
    workspace: &std::path::Path,
    src: &std::path::Path,
) -> Option<(HelloFlavor, Vec<u8>)> {
    if let Some(prefix) = usable_sdk() {
        let stamp_path = prefix.join("sysroot/.stamp");
        let stamp = std::fs::read_to_string(&stamp_path)
            .unwrap_or_else(|e| panic!("reading the SDK stamp {}: {e}", stamp_path.display()))
            .trim()
            .to_string();
        let bytes = build_hello_sdk(&prefix, workspace, src, &stamp);
        return Some((HelloFlavor::Sdk { prefix, stamp }, bytes));
    }
    build_hello_musl(workspace, src).map(|bytes| (HelloFlavor::Musl, bytes))
}

/// Build `userland/hello` with the minix.rs SDK: **one** driver invocation that
/// compiles and links, on the real triple.
///
/// ```text
///   <sdk>/bin/clang --target=aarch64-unknown-minixrs -O2 -Wall -Wextra -Werror \
///       -o target/hello/hello userland/hello/hello.c
/// ```
///
/// That is the entire command, and the absences are the milestone. The patched
/// driver supplies, from the triple alone: `-static`; `--image-base=0x100000`
/// (patch 0006, which is why this flavor needs no linker script); `-z
/// max-page-size=4096 -z separate-loadable-segments` (decision D13 — 4 KiB pages
/// and no two `PT_LOAD`s sharing one, because the kernel's loader maps
/// segment-by-segment with per-segment permissions); `crt1.o`/`crti.o`/`crtn.o`
/// and `-lc` out of `<sdk>/sysroot/usr/lib`; and `libclang_rt.builtins.a` from
/// its own resource dir, which is where this flavor gets the soft-float
/// `binary128` helpers that `build_hello_musl` has to scrape out of a nested
/// `compiler_builtins` rlib.
///
/// So there is no `-T`, `-nostdinc`, `-isystem`, `--sysroot`, `-L`, `-static`,
/// `-ffreestanding`, explicit crt object, builtins glob, or separate `rust-lld`
/// step — and no `-std=` either, since the musl path passes none and both
/// flavors should stay on one dialect. Tooling's rule applies:
/// **anything that has to be added back here is a bug to fix in the fork, not a
/// flag to paper over.**
///
/// Once the SDK is usable, every failure is a `panic!` and never a demotion to
/// `HelloFlavor::Musl`. The boot markers are byte-identical across flavors, so a
/// silent demotion would turn "your patched clang regressed" into "nothing looks
/// wrong" — the one outcome that makes this whole path untestable.
fn build_hello_sdk(
    prefix: &std::path::Path,
    workspace: &std::path::Path,
    src: &std::path::Path,
    stamp: &str,
) -> Vec<u8> {
    let clang = prefix.join("bin/clang");
    let out = workspace.join("target/hello");
    std::fs::create_dir_all(&out).expect("creating target/hello");
    let elf = out.join("hello");

    let status = clang_command(&clang)
        .args([
            "--target=aarch64-unknown-minixrs",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-o",
        ])
        .arg(&elf)
        .arg(src)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", clang.display()));

    if !status.success() {
        panic!(
            "the minix.rs SDK failed to build userland/hello.\n\
             \x20 SDK prefix: {prefix}\n\
             \x20 SDK stamp:  {stamp}\n\
             A usable SDK never falls back to the in-tree musl sysroot: the boot \
             markers are byte-identical across flavors, so demoting here would \
             report a regressed toolchain as a healthy build.\n\
             Reproduce with the driver's own view of the link:\n\
             \x20 {clang} -### --target=aarch64-unknown-minixrs -O2 -Wall -Wextra \
             -Werror -o /dev/null {src}\n\
             To build against the in-tree musl sysroot instead, point the variable \
             at nothing:\n\
             \x20 MINIXRS_SDK=/nonexistent cargo kernel-aarch64",
            prefix = prefix.display(),
            clang = clang.display(),
            src = src.display(),
        );
    }

    std::fs::read(&elf).unwrap_or_else(|e| panic!("reading {}: {e}", elf.display()))
}

/// Build `userland/hello` — the slice-5.6 C milestone — against the **in-tree
/// musl sysroot** (`tools/build-musl.sh` → `target/musl-sysroot`), returning the
/// linked ELF's bytes.
///
/// This is the stand-in-triple flavor: `--target=aarch64-unknown-linux-musl`, a
/// hand-written `hello.ld`, a `compiler_builtins` rlib scraped out of the nested
/// build dir, and a hand-assembled `rust-lld` line. It is what CI's blocking
/// `qemu-smoke` job builds, which makes it a real dependency rather than a
/// fallback: that job cannot install an SDK, and `tests/qemu-boot.expected`
/// requires the five C markers.
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
fn build_hello_musl(workspace: &std::path::Path, src: &std::path::Path) -> Option<Vec<u8>> {
    let script = workspace.join("userland/hello/hello.ld");
    let sysroot = workspace.join("target/musl-sysroot");
    let stamp = sysroot.join(".stamp");

    for path in [&script, &stamp] {
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
    let cc = clang_command("clang")
        .args(["--target=aarch64-unknown-linux-musl", "-nostdinc"])
        .arg("-isystem")
        .arg(&sysroot_inc)
        .args(["-ffreestanding", "-O2", "-Wall", "-Wextra", "-Werror", "-c"])
        .arg("-o")
        .arg(&obj)
        .arg(src)
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

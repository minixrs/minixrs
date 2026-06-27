# minix.rs

minix.rs — "MINIX 3, in Rust, for the 64-bit era" — is a 64-bit-only reimplementation of MINIX 3 in Rust, preserving the original ABI. It is a learning OS built around a greenfield Rust microkernel.

## Project Overview

- **Kernel:** Rust (no_std, no_main), greenfield microkernel
- **Architecture:** aarch64 primary (Apple Silicon / QEMU virt), x86_64 secondary
- **Servers:** PM, VFS, VM, RS, DS, SCHED as user-space Rust processes
- **Drivers:** VirtIO (MMIO for aarch64, PCI for x86_64) as user-space processes
- **C library:** musl-libc fork with MINIX IPC syscall wrappers
- **License:** BSD-3-Clause only (no GPL code)
- **Platform:** QEMU as primary target

## Reference Codebase

minix.rs's architecture is based on MINIX 3. When the docs reference "MINIX 3 source",
they mean paths within the MINIX 3 source tree (e.g., `kernel/proc.c` means the
`kernel/proc.c` file in a MINIX 3 checkout). Key reference files:

- `kernel/proc.c` -- IPC implementation (mini_send, mini_receive, deadlock detection)
- `kernel/proc.h`, `priv.h` -- Process and privilege structures
- `kernel/system.c` -- Kernel call dispatch
- `include/minix/ipc.h` -- Message structure definitions
- `include/minix/com.h`, `callnr.h` -- Server endpoints, call numbers
- `lib/libc/sys/*.c` -- POSIX syscall wrappers (template for musl adaptation)
- `lib/libsys/sef.c` -- SEF framework (template for server-rt)

The MINIX 3 source is available at https://github.com/Stichting-MINIX-Research-Foundation/minix

## Build

```sh
# Build kernel for aarch64 (primary target)
cargo kernel-aarch64

# Boot in QEMU (cargo runner wires tools/qemu-run.sh). The kernel runs
# indefinitely once EL0 starts (slice 2.4+), so `timeout` is mandatory.
# Redirect to a file when you need to grep tick output -- live tail loses lines.
# The log interleaves raw single-char tick bytes; grep it with `grep -a`
# (force text) or matches read as "Binary file matches".
# QEMU under TCG advances *guest* time slower than wall-clock, so a `timeout N`
# run reaches far fewer than N x 100 ticks. For time-based features (alarms,
# quantum/scheduling) read uptime-stamped traces (e.g. `[alarm ... at=N]`) as the
# real clock, and run 20-25 s to observe several periods.
timeout 8 cargo run -p minixrs-kernel --target aarch64-unknown-none --release

# Build kernel for x86_64
cargo kernel-x86_64

# Run host-side unit tests (note the package name, not the dir name)
cargo test -p minixrs-kernel-shared
```

## CI

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. Seven gates run in
parallel — `fmt`, `clippy`, `audit` (cargo-audit), `deny` (cargo-deny, config in `deny.toml`),
`geiger`, `miri`, `coverage` (cargo-llvm-cov → `lcov.info`) — then a `sonar` job feeds the LCOV
report to SonarQube Cloud (org `minixrs`, project `minixrs_minixrs`, config in
`sonar-project.properties`). The Sonar scan auto-detects PR vs branch: PRs get decoration, `main`
pushes refresh the whole-project picture.

- `geiger` and `miri` are **advisory** (`continue-on-error`); the rest block. miri only covers the
  two host-testable crates (`-p minixrs-kernel-shared -p minixrs-vm`) — `minix-ipc` has inline asm
- Before pushing, the blocking gates must be green: `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets -- -D warnings`. Run `cargo fmt --all` to fix formatting
- The blocking `clippy --workspace` gate runs on the **host** target, where the kernel's real
  modules are `#[cfg(target_os = "none")]`-gated out — so kernel code is *not* clippy-linted by CI.
  `cargo clippy -p minixrs-kernel --target aarch64-unknown-none` surfaces those lints, but it
  currently reports pre-existing ones that ship on `main` (nomem-asm pointers, `manual_is_multiple_of`,
  interior-mutable-const); don't "fix" them as part of an unrelated slice. `cargo kernel-aarch64` is
  the real compile gate for kernel code
- The toolchain is **pinned to a dated nightly** in `rust-toolchain.toml` (bare `nightly` let new
  lints/fmt rules break CI with no code change); bump it deliberately, not incidentally
- `Cargo.lock` **is committed** (so audit/deny are reproducible) — do not re-add it to `.gitignore`
- Third-party actions are pinned to full commit SHAs with `# vN` comments; keep that when editing
- SonarCloud needs the `SONAR_TOKEN` repo secret and Automatic Analysis disabled (CI-based instead)
- **Publishing:** `.github/workflows/release.yml` runs on a `v*` tag push and `cargo publish`es the
  five library crates to crates.io in dependency order (`minixrs-kernel-shared` → `minixrs-ipc` →
  `minixrs-server-rt` → `minixrs-driver-rt` → `minixrs` facade). All other members carry
  `publish = false` (freestanding binaries, unbuildable on registry infra). Needs the
  `CARGO_REGISTRY_TOKEN` repo secret. Bottom-up order is mandatory — crates.io forbids `path`-only
  deps, so the libs' path deps carry an explicit `version`. Verify locally with
  `cargo package -p minixrs-kernel-shared -p minixrs-ipc -p minixrs-server-rt -p minixrs-driver-rt
  -p minixrs` (verify-builds against packaged siblings) — `cargo publish --dry-run` resolves deps
  against the registry so it can't chain, and `cargo package --workspace` aborts on the
  `publish = false` binaries. See `RELEASING.md`

## Architecture

See `docs/architecture.md` for the full system design. Key concepts:

- **Microkernel:** Only IPC, scheduling, interrupt dispatch, and memory protection in kernel
- **Message passing:** 6 IPC primitives (SEND, RECEIVE, SENDREC, NOTIFY, SENDNB, SENDA)
- **User-space servers:** All OS services (file system, process management, memory management) run as separate processes communicating via IPC
- **Privilege model:** Fine-grained bitmaps control which processes can communicate and what kernel calls they can make

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `kernel` | Microkernel (no_std) |
| `kernel-shared` | Message types, endpoints, call numbers shared between kernel and userspace |
| `minix-ipc` | User-space IPC library (SVC/SYSCALL asm stubs) |
| `server-rt` | Server runtime / SEF framework |
| `servers/*` | System servers (PM, VFS, VM, RS, DS, SCHED) |
| `drivers/*` | Device drivers (VirtIO block/net/console, memory) |
| `fs/*` | File system servers (MFS, PFS) |
| `userland/*` | User programs (init, sh, coreutils) |

## Code Conventions

- Every new `.rs`/`.S` source file must begin with the SPDX + copyright header before any other content. Rust (line-comment form): `// SPDX-License-Identifier: BSD-3-Clause` then `// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors`. Assembly `.S` (block-comment form): `/* SPDX-License-Identifier: BSD-3-Clause */` then `/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */`. Update the year as needed; `.toml`/`.ld`/`.conf` files get no header
- The kernel ships `--release` only, so `debug_assert!` is compiled out. Use a hard `assert!` for invariants whose violation would silently corrupt (e.g. a null TTBR0/ASID reaching the scheduler); reserve `debug_assert!` for cheap "can't happen" documentation that's fine to drop in release
- Kernel `unsafe` blocks require `// SAFETY:` comments documenting the invariant
- IPC linked lists use `Option<ProcNr>` indices into static arrays, not raw pointers
- Message types are defined in `kernel-shared` and shared across all crates
- Assembly is confined to `.S` files (assembled via `cc` crate in `build.rs`); use `core::arch::asm!` only for single-instruction operations
- New `.S` files must be added to `kernel/build.rs`'s `sources` array; offset blocks (`.equ REGS_*_OFFSET …`) are duplicated per-file since there is no cross-`.S` include
- To end a `&mut` borrow before an `unsafe` call that re-borrows the same static, capture state into locals (bool / scalar) and rely on NLL — `drop(&mut x)` is a no-op and triggers a `dropping_references` warning
- Run-queue admission is decoupled from boot: `IMAGE.runnable` marks IPC reachability; only `proc::sched::enqueue` puts a proc in the scheduler's run queue
- Static mutable tables use `UnsafeCell<[T; N]>` inside a `#[repr(transparent)]` newtype with `unsafe impl Sync`; document the single-threaded-boot invariant in the `// SAFETY:` comment
- Custom `Display` impls that must honor `{:<width$}` render through a stack buffer (`arrayvec::ArrayString<N>`) and call `f.pad(s)` — `write!(f, ...)` from inside `Display::fmt` ignores the outer width spec
- Forward declarations intended for later slices (constants, fields, re-exports) get module-level `#![allow(dead_code)]` with a one-line comment naming the consuming slice
- IPC primitives take an explicit `&mut [Proc; N_PROC_SLOTS]` (and `&mut [Priv; NR_SYS_PROCS]`) slice; only `ipc::do_ipc` materializes those from `PROC_TABLE` / `PRIV_TABLE` via `proc_table_mut_slice` / `priv_table_mut_slice`. Keeps each primitive testable in isolation and dodges the two-`&mut`-from-one-`UnsafeCell` UB hazard
- Every EL1 → EL0 transition (SVC tail via `el1_svc_tail`, `sched::reschedule`, `sched::run`) calls `sched::schedule_next`, which flushes `Proc::deliver_msg` to the user buffer at `Proc::deliver_msg_vir` and clears `MF_DELIVERMSG` before resuming
- IPC blocking pairs with the new `sched::rts_set` / `rts_unset` helpers — they capture `nr`, end the `&mut Proc` borrow, then call `enqueue` / `dequeue` so RTS state and the run queue stay in sync. Same NLL-capture pattern slice 2.4 used in `clock::tick`
- Kernel-call handlers that act on a *target* proc named in the message (e.g. `system::do_vmctl`, `system::do_schedule`'s `do_schedule`/`do_schedctl`) take the whole `&mut [Proc; N_PROC_SLOTS]` slice + `caller_nr`; caller-only handlers (e.g. `do_getinfo`) get a single `&mut Proc` / `&Priv`. `system::kernel_call_dispatch` routes `SYS_VMCTL` / `SYS_SCHEDULE` / `SYS_SCHEDCTL` to the table-taking form (a small `match` before `dispatch_caller_local`) and the rest through `dispatch_caller_local`. Run-queue transitions on a target use the same `sched::rts_set` / `rts_unset` capture-then-borrow-end pattern the IPC primitives use
- The kernel scheduler is **delegatable** (slice 4.3): `Proc::scheduler == NONE` (the boot default, set by `populate_proc`) means kernel-scheduled — `sched::reschedule` refills the quantum and rotates as before. A non-`NONE` `scheduler` endpoint means SCHED-scheduled: on quantum exhaustion `reschedule` dequeues the proc, leaves `RTS_NO_QUANTUM` set, and sends `SCHEDULING_NO_QUANTUM` to the scheduler via `ipc::send::mini_sched_no_quantum_send` (a `mini_pf_send` clone; wrapper `ipc::send_no_quantum` materializes the proc-table slice like `send_pagefault_to_vm`). The proc stays off the run queue until the scheduler calls real `SYS_SCHEDULE` (`do_schedule` sets priority/quantum + `rts_unset(RTS_NO_QUANTUM)`). Kernel tasks **and SCHED itself stay `NONE`** — a scheduler must not schedule itself. `SYS_SCHEDCTL` claims (`scheduler = caller`) / releases (`SCHEDCTL_FLAG_KERNEL` → `NONE`) a target; the kernel pre-delegates stub C in `userland.rs` as the live demo until PM/RS drive `SCHEDULING_START` (4.5/4.6). SCHED's `SCHEDULING_*` request range is `SCHED_RQ_BASE = 0xF00` (clear of VM `0xC00` / SEF `0xD00` / DS `0xE00`, below `NOTIFY_MESSAGE`)
- Per-proc one-shot alarms (slice 4.4): `Proc::alarm_at` holds an absolute uptime tick (0 = disarmed); `clock::EARLIEST_ALARM` is an O(1) fast-path gate so `tick()` only pays the O(N) scan in `ipc::fire_expired_alarms` when one is due. Expiry delivers a kernel-originated `NOTIFY` from `CLOCK` via `ipc::notify::deliver_alarm` — **no `ipc_to` check** (kernel-originated, like `mini_pf_send`; `CLOCK`'s `ipc_to` is empty so `mini_notify` would deny it), immediate if the owner is `RECEIVE`-blocked else deferred via `notify_pending` against CLOCK's priv slot. A user-space periodic alarm is a re-arm per fire (RS). `SYS_SETALARM` (now real, caller-local) payload: relative `delta` ticks in `0..8` (0 cancels), reply = previous time-left; no new kernel-shared constants (reuses `NOTIFY_MESSAGE` + `CLOCK`). RS (`servers/rs`) heartbeats peers by `ipc_notify` (peers ack via `server-rt`'s SEF ping); RS keys its alarm on `m_source == boot_endpoint(CLOCK)` (classified `Application`, not the RS-sourced SEF ping)
- `kernel/build.rs` skips assembly when `CARGO_CFG_TARGET_OS != "none"` so `cargo check --workspace` / `cargo test --workspace` keep working on host. The kernel's real modules are gated by `#[cfg(target_os = "none")]` in `main.rs` regardless
- `cargo test -p minixrs-kernel` runs zero tests by design — every kernel module is gated on `#[cfg(target_os = "none")]` and host-test infra is not yet built; host-runnable tests live in `kernel-shared`. QEMU is the primary verification for kernel code (`timeout 8 cargo run -p minixrs-kernel --target aarch64-unknown-none --release`)
- `no_std` library crates that host-test via `#![cfg_attr(not(test), no_std)]` get linted in their std test-config too (`clippy --all-targets`): a const-only `assert!(A > B)` trips `assertions_on_constants` (use a module-level `const _: () = assert!(…)` like `callnr.rs`), and a bare `loop {}` in a function present under `test` trips `empty_loop` (use `loop { core::hint::spin_loop() }`; the `#[cfg(not(test))]` panic handler's `loop {}` is exempt because it's absent under test)
- User-space servers build as freestanding `#![no_std]`/`#![no_main]` ELFs linked with their own `user.ld` (page-aligned PT_LOADs, base `0x10_0000`; kernel sets `sp_el0` so `_start` needs no stack setup). The kernel's `build.rs` builds them for `aarch64-unknown-none` in a *separate* `CARGO_TARGET_DIR` (dodges the nested-cargo build-lock deadlock), overrides the kernel linker script via `CARGO_ENCODED_RUSTFLAGS` → the server's `user.ld`, and emits `VM_ELF_PATH` so `kernel/src/boot_image/mod.rs` can `include_bytes!` it. `boot_image/elf.rs` is the minimal ET_EXEC/AArch64 loader (PT_LOAD → `alloc_frame` + HHDM copy + map; BSS via zeroed frames). The multi-module MXBI archive is deferred until Phase 4 loads more than one server
- When a boot server gains a new path dependency, add that crate's `src` dir to `kernel/build.rs`'s server `rerun-if-changed` list (a directory is watched recursively) — otherwise edits to the dep silently embed a stale server ELF
- ELF-only attributes on server crates (`#[unsafe(link_section = ".text._start")]`, etc.) must be `#[cfg_attr(target_os = "none", ...)]`-gated — `cargo check --workspace` also builds servers for the Mach-O host, which rejects ELF section specifiers
- User-space servers run at EL0 with no console access — they cannot print. Verify server behavior through kernel-side traces (`[pf]` from `do_page_fault`, `[ksys …]` from `do_vmctl`/`system`, `[ipc N]` from `ipc::dispatch`), never server-side logging. Trace sampling is asymmetric: `[ipc N]` head-traces the first ~12 calls *plus* every 100th, but `[ksys N]` samples only every 100th (no head carve-out) — a server's first/rare kernel call (e.g. a startup `SYS_GETINFO`) shows on `[ipc]`, not `[ksys]`
- System servers drive their receive loop through `server-rt`'s SEF (slice 4.1+): `sef_startup(SefConfig { init_fresh, signal_handler })` then `loop { if sef.receive(&mut msg) != 0 { continue } match msg.m_type { … } }`. Callbacks pass via the config struct and are carried in the returned `Sef` handle (no global `setcb`/static state, so `server-rt` is `#![forbid(unsafe_code)]`); `sef_startup` learns the server's endpoint/name via `SYS_GETINFO(GET_WHOAMI)` and `sef.receive` filters SEF control messages, returning only application messages. The pure classifier lives in `server-rt/src/classify.rs` (host-tested; the IPC glue in `sef.rs` is coverage-excluded like the server `main.rs`es) and gates each control event on `m_source`, not `m_type` alone — NOTIFY ping from RS, `SEF_SIGNAL` from PM/RS, `SEF_INIT` from RS — so a client holding only an `ipc_to` bit to the server can't spoof a signal/init
- `init_boot_image` fills a boot server's `ipc_to` only for active boot priv slots `[0, n_active)` (~0–15). A hand-installed stub in a higher priv slot (16+) that a server must *reply* to needs the reverse `ipc_to` bit opened explicitly — see `install_stub_d_priv` opening VM→D after setting D→VM
- VM (`servers/vm/`) tracks per-process memory as a static `[ClientRegions; 16]` keyed by proc number (no heap allocator — the kernel owns frames), each region a half-open `[start, end)` tagged `Kind::{Heap, Mmap, Unused}`. A page fault is satisfied only when its address lies inside a region; out-of-region faults are a silent SIGSEGV (faulter left blocked on `RTS_PAGEFAULT` — real signals are Phase 4). `VM_BRK`/`VM_MMAP`/`VM_MUNMAP` all ride the single D→VM SENDREC edge, so adding an mmap client needs no new priv wiring beyond the brk one
- `SYS_VMCTL(VMCTL_PT_UNMAP)` returns `EINVAL` (no panic, no frame freed) when nothing is mapped at the target VA — so VM's `munmap` can sweep a region page-by-page with `VMCTL_PT_UNMAP` and ignore the never-faulted pages. Keep the unmap sweep capped at the region's own `end` so an overstated `len` can't reach a neighbor's frames
- Boot servers are packed into a single **MXBI archive** (slice 4.2+): `kernel/build.rs`'s `build_server(name, dir, …)` builds each server crate into its *own* isolated `CARGO_TARGET_DIR` (nested-cargo lock), `pack_mxbi` concatenates them under a 16-byte header (`magic "MXBI"`/ver/count/total) + 32-byte records `{proc_nr:i32, offset:u32, len:u32, name:[u8;20]}` (all LE), and emits `BOOT_IMAGE_PATH`. To add a boot server, append a `(crate, dir, proc_nr)` row to the `servers` array (proc_nr from `kernel-shared/com.rs`) and watch its `src` dir. The `env!("BOOT_IMAGE_PATH")` `include_bytes!` lives only in `boot_image/mod.rs`, which is `#[cfg(target_os = "none")]` — that gate is what keeps host `cargo check`/`test` (env unset) compiling, so never reference `BOOT_IMAGE_PATH` from a host-compiled module. `boot_image::BootImage::iter()` drives `userland::load_boot_server(nr, elf, stack_va)` (the generalized `vm_bootstrap`); all servers share one `SERVER_STACK_VA` since each has its own TTBR0. No new boot priv wiring — `init_boot_image` already grants every boot server `SRV_T` `ipc_to` over `[0, n_active)`
- Servers discover each other via **DS** (the name→endpoint registry, `servers/ds/`): every server publishes its endpoint at SEF init by setting `init_fresh: Some(...)` to a callback that calls `server-rt::sef_publish_to_ds(endpoint, name)` (a `DS_PUBLISH` SENDREC; key = 16-byte NUL-padded name in payload `0..16`, endpoint i32 in `16..20`). **DS is the exception** — it seeds its *own* entry in-process in `ds_init` (`registry::publish(name, endpoint)`), because a SENDREC to itself before reaching its receive loop would deadlock. DS request numbers live at `DS_RQ_BASE = 0xE00` (clear of VM `0xC00` / SEF `0xD00`, below `NOTIFY_MESSAGE`). The registry is a static `[Entry; 16]` `UnsafeCell` newtype like `vm/region.rs`; its pure `publish/retrieve/check` are host-tested, the SEF/IPC `main.rs` is coverage-excluded (add new server `main.rs` glob-covered, plus any new `server-rt` IPC-trap module like `ds.rs` explicitly, to `sonar.coverage.exclusions`)

## Documentation

Canonical docs are an **mdBook in `book/`** (content under `book/src/`, TOC in
`book/src/SUMMARY.md`), published to GitHub Pages on push to `main` via
`.github/workflows/docs.yml` (path-filtered to `book/**`; mdBook pinned to 0.5.3; Pages
actions SHA-pinned like `ci.yml`). Write new documentation there, derived from source —
the `docs/*.md` files are legacy bootstrap notes being retired (with `docs/plan.md` still
the live slice tracker, below). Build locally with `mdbook build book`; output `book/book/`
is gitignored.

mdBook isn't installed by default. Install the **prebuilt** binary pinned to CI's 0.5.3
(`cargo install mdbook` compiles slowly from source) — for Apple Silicon:
`curl -fsSL https://github.com/rust-lang/mdBook/releases/download/v0.5.3/mdbook-v0.5.3-aarch64-apple-darwin.tar.gz | tar xz && mv mdbook ~/.cargo/bin/`.
Live preview with reload: `mdbook serve book -n 127.0.0.1 -p 3000`.

`docs/plan.md` tracks slice status with three markers: `◀ next` (unstarted), `◀ ready (branch ..., pending merge)` (implemented but unmerged), `✓ shipped (PR #N, merged YYYY-MM-DD)` (merged). Flip the previous slice forward and slide `◀ next` ahead as part of each slice's PR. When opening a new slice PR, also reconcile any older `◀ ready` markers against `git log` — stale "pending merge" labels on already-merged PRs accumulate otherwise.

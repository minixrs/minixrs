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

# Verify a captured boot log against the standard acceptance markers:
#   tools/check-boot-log.sh <log>   (tests/qemu-boot.expected/.forbidden;
# update those marker files in the same PR when trace formats or the boot
# roster change, or the qemu-smoke CI job goes red)

# Build kernel for x86_64
cargo kernel-x86_64

# Run host-side unit tests (note the package name, not the dir name)
cargo test -p minixrs-kernel-shared
```

## CI

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. Eight gates run in
parallel — `fmt`, `clippy`, `audit` (cargo-audit), `deny` (cargo-deny, config in `deny.toml`),
`geiger`, `miri`, `qemu-smoke` (aarch64 QEMU boot smoke), `coverage` (cargo-llvm-cov →
`lcov.info`) — then a `sonar` job feeds the LCOV report to SonarQube Cloud (org `minixrs`,
project `minixrs_minixrs`, config in `sonar-project.properties`). The Sonar scan auto-detects
PR vs branch: PRs get decoration, `main` pushes refresh the whole-project picture.

- `geiger`, `miri`, and `qemu-smoke` are **advisory** (`continue-on-error`); the rest block. miri
  only covers the host-testable crates (`-p minixrs-kernel-shared -p minixrs-vm -p minixrs-pm`) —
  `minix-ipc` has inline asm
- `qemu-smoke` runs on the free `ubuntu-24.04-arm` runner: boots the kernel for 45 s wall clock
  via the cargo runner, then `tools/check-boot-log.sh` greps the serial log (`grep -aF`) against
  `tests/qemu-boot.expected` / `tests/qemu-boot.forbidden`. Keep expectations timing-robust —
  first occurrences only, never counts (CI TCG is slower than local). Flip to blocking once
  stable across a few PRs
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
- **Message passing:** MINIX 3's 6 IPC primitives — 5 live (SEND, RECEIVE, SENDREC, NOTIFY, SENDNB); SENDA still an `ENOSYS` stub
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
- Minimal signals (slice 4.5): the kernel half is `system/do_sig.rs` — `SYS_KILL` → `cause_sig` sets a bit in `Proc::sig_pending` (u32 bitmap; zeroed on slot free and again on fork's child populate) plus `RTS_SIGNALED|RTS_SIG_PENDING`, then wakes PM with `ipc::notify::deliver_ksig` (kernel-originated `NOTIFY` from `SYSTEM`, no `ipc_to` check — the `deliver_alarm` pattern; `do_kill`'s deferred-notify write is why `kernel_call_dispatch` takes `&mut [Priv]`). Drain contract (amended in 4.6a): `SYS_GETKSIG` hands off the bitmap (its scan gates on `sig_pending != 0`, not the RTS bit alone, so a handed-off proc isn't re-returned) and PM disposes of every returned endpoint — `SYS_ENDKSIG` for survivors, `SYS_EXIT` (with **no** ENDKSIG after) for terminations, since 4.6's full exit zeroes signal state and frees the slot so a post-exit acknowledge just bounces off `okendpt` with `EDEADSRCDST`. `SYS_EXIT` is a **full teardown** as of 4.6a (`do_exit.rs`): stop + `caller_q` unlink as before, then `unblock_dependents` (every proc blocked SENDING-to/RECEIVING-from the dead endpoint resumes with `EDEADSRCDST` patched into its parked `x0`; dedicated-priv `notify_pending` bits purged, shared `USER_PRIV_ID` deliberately skipped), leaf frames freed via `addrspace::walk_leaves` + `flush_tlb_asid` + `AddrSpace::destroy` + `asid::free_asid` (ASIDs recycle through a free-list now), and `free_slot` (endpoint `bump_generation` — wraps to 1, never 0 — all per-proc state zeroed, `RTS_SLOT_FREE` stored). Every user-supplied-endpoint resolution goes through `table::okendpt` (stored-endpoint + not-free check → `EDEADSRCDST` on stale generations): `mini_send`/`mini_notify` dst, `mini_receive`'s non-ANY filter, and the target-taking kernel calls via `system::resolve_target` (`SELF` short-circuits first; `do_exit` rejects SELF and caller-named-self outright — tearing down the active TTBR0 mid-call is the hazard). `SYS_PRIVCTL` has one subcode, `PRIVCTL_SET_USER`: requires the target frozen on `RTS_NO_PRIV` (the freeze doubles as the authorization gate; EPERM on a live target) and points it at the shared `table::USER_PRIV_ID` = 20 (`USR_T`, `ipc_to` = {PM}, empty `k_call_mask`, `sig_mgr` = PM; `populate_user_priv` opens the reverse PM(priv 5)→20 `ipc_to` edge at boot). Frozen-stub pattern: `build_stub(nr, None, .., frozen: true)` = full AS built (ttbr0/asid set, satisfying `schedule_next`'s asserts), no priv, not enqueued — `PRIVCTL_SET_USER` releases it. PM's request range is `PM_RQ_BASE = 0x700` (the gap below VM `0xC00` — SCHED's `0xF00` was the last block under `NOTIFY_MESSAGE`); `PM_GETPID` replies `m_type` = pid (MINIX result-is-pid, errors negative — PM's own pid 0 is indistinguishable from `OK` by design) with ppid in payload `0..4`. Stub proc numbers are shared as `com::STUB_A..E_PROC_NR` (kernel `userland.rs` and PM's `mproc` seeding must agree; retired in 4.8). VM's out-of-region fault arm now raises `SYS_KILL(faulter, SIGSEGV)` instead of silently returning; the faulter stays `RTS_PAGEFAULT`-blocked until PM terminates it
- PM-driven fork/exit/wait (slice 4.6b): user procs drive the process lifecycle entirely through PM (POSIX shape — user → PM, never user → kernel; the shared `USER_PRIV_ID` only opens `ipc_to = {PM}`), via `PM_FORK`/`PM_EXIT`/`PM_WAIT` (`PM_RQ_BASE + 1..3`) and `VM_FORK` (`VM_RQ_BASE + 4 = 0xC04`). PM's `handle_fork` owns the whole tree in a **fixed order that the frozen-child invariant depends on**: alloc an `mproc` child slot (fork pool `[FORK_POOL_BASE = NR_BOOT_PROCS+NR_STUB_PROCS = 16, NR_MPROCS = 32)`; slot index *is* the child's kernel proc-nr) → `SYS_FORK(parent_e, child_nr)` → `VM_FORK(parent_e, child_e)` → `SCHEDULING_START` → `SYS_PRIVCTL(PRIVCTL_SET_USER)` release → reply to **both** halves of the shared SENDREC (child `m_type = 0`, parent `m_type = child_pid`; MINIX fork-returns-twice). Why the order is safe: `do_fork` creates the child `RTS_RECEIVING | RTS_NO_PRIV` (frozen), and `sched::rts_unset` only enqueues on the *last* block bit clearing — so `SYS_SCHEDULE` (via `SCHEDULING_START`) and `SYS_PRIVCTL` (clears `RTS_NO_PRIV`, leaving `RTS_RECEIVING`) both leave the child a blocked receiver off the run queue; **only PM's reply** (clearing `RTS_RECEIVING`) makes it runnable, so it can't run before its identity/memory/scheduling are built. On any mid-fork failure PM rolls back at **every** step — `SYS_FORK`/`VM_FORK`/`SCHEDULING_START`/`SYS_PRIVCTL` each check their result and, on error, `SYS_EXIT` the child + `mproc::cleanup` the slot before returning the errno to the parent (the post-`SCHEDULING_START` rollback does `SCHEDULING_STOP` first, endpoint-still-valid, mirroring `handle_exit`); the last two are boot-server-backed and can't fail with correct wiring, so their guards are defense-in-depth against ever recording an unrunnable child (which would hang the parent's `wait()`). `handle_exit` = `SCHEDULING_STOP` **before** `SYS_EXIT` (once `SYS_EXIT` bumps the generation, `okendpt` rejects the endpoint) + mark the `mproc` slot a zombie holding the encoded status (`W_EXITCODE = (status & 0xff) << 8`); PM sends the dead child **no** reply. `handle_wait` reaps a zombie child (reply pid + status, `cleanup` the slot) or, with a live child, sets `MF_WAITING` and **suspends** the parent (no reply) until `handle_exit` wakes it directly. **No async `SIGCHLD`** — the kernel signal path default-*terminates*, which would kill the handler-less parent stub, so parent-notify is the zombie + wait-reap handshake only (async signals wait for Phase 5 handlers). `mproc` (`servers/pm/src/mproc.rs`) now stores a generation-aware `endpoint` per slot (`boot_endpoint(slot)` for seeded procs, the `SYS_FORK` reply for children) + `exit_status` + `MF_WAITING`, with the free-slot allocator / zombie / reap logic in pure host-tested `*_in` helpers. VM's `region.rs` `MAX_CLIENTS` is widened 16 → 32 so the fork pool's child proc-nrs are addressable, and `region::fork(parent_nr, child_nr)` copies the whole `ClientRegions` (a `Copy` snapshot). **No new priv wiring** — PM↔VM and PM↔SCHED are boot-server `[0,n_active)` edges, child↔PM is the `USER_PRIV_ID` edge. Stub E is the live demo (fork/exit/wait loop; branches on the fork reply `m_type`: child → `PM_EXIT`, parent → `PM_WAIT` → loop). Recycling proof in QEMU: every fork reuses child slot 16 with a monotonically advancing endpoint generation, so a fresh fork implies the prior child was torn down *and* reaped
- PM-driven exec (slice 4.7): a user proc `SENDREC`s `PM_EXEC` (`PM_RQ_BASE + 4 = 0x704`, `NR_PM_MSGS` 4→5) to PM; PM issues `SYS_EXEC` naming that proc as the **target** (POSIX shape — exec is done *to* the caller, so `SYS_EXEC` moved from the caller-local arm to the target-taking `match` beside `SYS_FORK` in `kernel_call_dispatch`; `SYS_EXEC` was already numbered `0x603` and blanket-granted to PM by the SRV_T `k_call_mask` fill, so `NR_KERN_CALLS_PHASE4` stays 18 and no priv wiring changes). `kernel/src/system/do_exec.rs`: resolve the target (reject `SELF`/self-target — the active-TTBR0 teardown hazard, the `do_exit` stance), read the binary name from payload `4..4+EXEC_NAME_LEN` (16, new in `callnr.rs`; MINIX-renames the proc to it — `EXEC_NAME_LEN <= PROC_NAME_LEN`), resolve it via `BootImage::module_by_name`, gate the target exactly like `do_fork`'s parent (a clean `RTS_RECEIVING` receiver — in the live flow mid-`SENDREC` to PM), build the new AS via `userland::load_exec_image`, reset the frame (`ArchRegisterFrame::EMPTY` + `elr_el1`/`sp_el0`/`spsr_el1 = STUB_SPSR_EL0`), swap `(ttbr0_pa, asid)`, `do_exit::teardown_addrspace` (now `pub(super)`) the **old** image (safe: target ≠ caller), then `sched::rts_unset(RTS_RECEIVING)` to resume it at `_start`. exec preserves pid/priv/scheduler; the target gets **no reply** on success (kernel resumes it at the new entry), errno reply on failure — so PM's `handle_exec` replies only on `rc != OK`. `userland::load_exec_image(elf) -> Option<ExecImage>` is factored out of `load_boot_server` (`AddrSpace::new` + `elf::load_into` + one RW stack page at `SERVER_STACK_VA` + `alloc_asid`, `mem::forget` the tree; `None` + `destroy_addrspace_with_leaves` cleanup on OOM — the `do_fork` copy_addrspace no-leak contract). The exec target is a freestanding `userland/worker` ELF (getpid loop + `PM_EXIT`; **no** `server-rt`/SEF — a plain user program, deps `minix-ipc` + `kernel-shared` only), packed into the MXBI archive by `build.rs` with sentinel proc_nr `com::EXEC_ONLY_PROC_NR = -1` — the boot loader (`userland.rs` load loop) skips any negative proc_nr, so `worker` is resolvable by name but **never boot-loaded** (no `[as]` line, no proc/priv slot). PM's `execve` hardcodes `EXEC_TARGET = "worker"` for the demo (a user-supplied path arrives with Phase-5 musl/filesystem; the kernel path is already name-driven). Stub E's child branch flips `PM_EXIT` → `PM_EXEC`: fork → child execs `worker` → worker exits → parent reaps → loop, so a `[ksys SYS_EXEC] target=16 name=worker` per cycle sits between the `SYS_FORK`/`SYS_EXIT nr=16` twins with a monotonically advancing endpoint generation + recycled ASID (exec teardown + reap proof). `userland/**/src/main.rs` added to `sonar.coverage.exclusions` (freestanding entry point, QEMU-verified, no host-testable logic — like the server `main.rs`es)
- init + Phase-4 wrap-up (slice 4.8): `init` (PID 1, `INIT_PROC_NR = 10`) becomes a **real boot process**, replacing the slice-4.6/4.7 demo stub E. The `userland/init` crate is a freestanding fork/exec/wait respawn loop (`minix-ipc` + `kernel-shared` only, **no** `server-rt`/SEF — a plain user program like `worker`; `_start` shim + panic handler `not(test)`-gated; `user.ld` is `worker`'s verbatim) — `loop { PM_FORK; if reply m_type==0 → PM_EXEC (child becomes `worker`); elif >0 → PM_WAIT (reap); else brief spin }`. It is packed into the MXBI archive by adding `("minixrs-init", …/init, 10)` to `build.rs`'s `servers` array (bump `; 7`→`; 8`); the ordinary `userland.rs` load loop then loads it, clears `RTS_NO_PRIV`, and enqueues it — **no** PM hand-release (contrast stub E's `PRIVCTL_SET_USER`). **User-grade priv:** init's `IMAGE` `BootEntry.trap_mask` is `SRV_T`→`USR_T`, and `init_boot_image` special-cases `entry.nr == INIT_PROC_NR` to point its proc slot at the shared `USER_PRIV_ID` (slot 20, filled by `populate_user_priv`, which already opens the PM↔USER edge) instead of populating a dedicated server-grade slot — so init SENDRECs PM only and makes no kernel calls, exactly the forked-child profile; its would-be dedicated priv slot 15 stays free. `MF_PRIV_PROC` stays set on init's `mproc` seed (unkillable PID 1 — that flag gates only the kill path, not fork/wait/getpid, so PM still serves init as a client). **Stub E retired (only E; A–D kept** as the live regression battery for IPC primitives / SCHED delegation / VM page-faults, which init+worker don't exercise): removed its `user_stub.S` blob, `userland.rs` `build_stub` call + `USER_CODE/STACK_VA_E` + `_user_stub_e_*` externs + `print_addrspace_summary` line, `com.rs` `STUB_E_PROC_NR` + `NR_STUB_PROCS` 5→4 (which shifts `FORK_POOL_BASE = NR_BOOT_PROCS + NR_STUB_PROCS` 16→15, so forked-child kernel proc-nrs now start at 15), and PM `pm_init`'s `privctl_set_user(SYSTEM, STUB_E)` release. `mproc` host tests that keyed on stub E (slot 15) rebase onto stub D (slot 14, the new last-seeded proc) or `INIT`. Docs: new mdBook `book/src/servers/overview.md` chapter + `SUMMARY.md` entry. Verified in QEMU over 30 s: 11 `[as]` lines (vm/ds/vfs/sched/rs/pm/init asid 1–7, stubs A–D asid 8–11; stub E + `worker` **absent**), init (`parent=i nr=10`) driving `SYS_FORK child_nr=15` → `SYS_EXEC target=15 name=worker` → `SYS_EXIT target=w nr=15 freed=2` with monotonically advancing child endpoint gen (`0xf → 0x800f → 0x1000f`) + recycled ASIDs, worker `PM_GETPID` SENDRECs (`caller=15/16 target=0x0`) surfacing; A↔B ping-pong, C `[noq]` SCHED delegation, D's three `[pf]` + SIGSEGV kill chain, six RS `[alarm]` fires all intact; every `result=0` (bar D's designed `sig=11`); zero panic / `el0_sync_unexpected`. **Phase 4 complete.**
- QEMU trace forensics: the `[ipc]` modulo sampler almost never catches low-rate callers — a blocking SENDREC client (e.g. stub E's fork/wait loop) round-trips once per band-8 rotation, thousands of times rarer than stub C's synchronous kernel-call flood — and a server packed late in the MXBI archive (PM) boots after C's flood starts, so its SEF handshake never lands in `TRACE_HEAD` either. Zero sampled lines for such a caller is NOT evidence it's stuck: verify via its downstream head-carve `[ksys …]` traces (e.g. `[ksys SYS_FORK]`/`SYS_EXIT` are head-carved at 6, so raise the head const temporarily to count real cycles), or a temporary unconditional `[DBG]` trace in `ipc::do_ipc` keyed on the caller nr (remove before committing)
- `kernel/build.rs` skips assembly when `CARGO_CFG_TARGET_OS != "none"` so `cargo check --workspace` / `cargo test --workspace` keep working on host. The kernel's real modules are gated by `#[cfg(target_os = "none")]` in `main.rs` regardless
- `cargo test -p minixrs-kernel` runs zero tests by design — every kernel module is gated on `#[cfg(target_os = "none")]` and host-test infra is not yet built; host-runnable tests live in `kernel-shared`. QEMU is the primary verification for kernel code (`timeout 8 cargo run -p minixrs-kernel --target aarch64-unknown-none --release`; CI smoke-boots it in the advisory `qemu-smoke` job)
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
the `docs/*.md` files are legacy bootstrap notes being retired. The planning tree is the
exception and stays: `docs/plan.md` is the lean live tracker (phase status + slice
summaries), and `docs/plans/` holds the full per-phase slice histories
(`phase-2-ipc.md` / `phase-3-vm.md` / `phase-4-servers.md`) plus the pre-Phase-5 cleanup
tracker (`phase-5-prep.md` — one PR-sized chunk per session, same markers). Build locally
with `mdbook build book`; output `book/book/` is gitignored.

mdBook isn't installed by default. Install the **prebuilt** binary pinned to CI's 0.5.3
(`cargo install mdbook` compiles slowly from source) — for Apple Silicon:
`curl -fsSL https://github.com/rust-lang/mdBook/releases/download/v0.5.3/mdbook-v0.5.3-aarch64-apple-darwin.tar.gz | tar xz && mv mdbook ~/.cargo/bin/`.
Live preview with reload: `mdbook serve book -n 127.0.0.1 -p 3000`.

`docs/plan.md` and the `docs/plans/*` files track slice/chunk status with three markers: `◀ next` (unstarted), `◀ ready (branch ..., pending merge)` (implemented but unmerged), `✓ shipped (PR #N, merged YYYY-MM-DD)` (merged). Flip the previous slice forward and slide `◀ next` ahead as part of each slice's PR — in **both** plan.md's summary line and the corresponding `docs/plans/` detail file. When opening a new slice PR, also reconcile any older `◀ ready` markers against `git log` — stale "pending merge" labels on already-merged PRs accumulate otherwise.

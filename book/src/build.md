# Build & Toolchain

minix.rs builds as a single Cargo workspace. There is no separate C build today
(the musl fork is future work — see [Roadmap](roadmap.md)); the only non-Cargo
step is fetching the prebuilt Limine binary, and a couple of shell scripts in
`tools/` stage the boot ESP and launch QEMU.

## Prerequisites

- **Rust nightly**, pinned in `rust-toolchain.toml` (a bare `nightly` would let new
  lints or fmt rules break the build with no code change).
- **QEMU** with `qemu-system-aarch64`.
- **aarch64 UEFI firmware** (edk2 / OVMF). `tools/qemu-run.sh` auto-detects it in
  common locations, or set `QEMU_EFI_AARCH64=/path/to/edk2-aarch64-code.fd`.

## Quick start

```sh
# One-time: fetch the pinned Limine binary into external/limine/dist/
make -C external/limine

# Build the kernel for aarch64 (the primary target)
cargo kernel-aarch64

# Build + boot under QEMU. The kernel runs indefinitely once EL0 starts, so a
# timeout is mandatory. Redirect to a file when you need to grep the log.
# Budget ~5 s for the rebuild + UEFI firmware startup before the kernel's first
# byte -- `timeout 8` can yield a log with no kernel output at all, so use 25 s
# for anything you intend to verify.
timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release

# Clean, stub-free boot for debugging: --no-default-features disables the
# `boot-stubs` feature, so only the servers + init/worker boot (no demo stubs
# A-D flooding the trace). See "Boot stubs" under Cargo workspace below.
timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features
```

`cargo run` invokes the cargo runner (`tools/qemu-run.sh`), which stages an ESP
directory at `target/esp/`, drops Limine and the freshly built kernel in, and
boots QEMU with the directory-as-FAT helper — no disk-image scripting needed (see
[Boot](boot/overview.md) for the ESP layout and the exact QEMU command). Early
serial output looks like:

```text
minix.rs booting on aarch64
HHDM offset: 0xffff000000000000
```

## Cargo workspace

The root `Cargo.toml` declares every crate as a workspace member: `kernel`,
`kernel-shared`, `minix-ipc`, `server-rt`, the six `servers/*`, the (stub)
`drivers/*` and `fs/*`, and `userland/{init,worker,sh,coreutils}`.

The kernel builds against the **builtin `aarch64-unknown-none` target** — not a
custom JSON spec. `.cargo/config.toml` wires the details:

```toml
[target.aarch64-unknown-none]
runner   = "tools/qemu-run.sh"
rustflags = ["-C", "link-arg=-Tkernel/src/arch/aarch64/linker.ld"]

[alias]
kernel-aarch64 = "build -p minixrs-kernel --target aarch64-unknown-none --release"
```

The `x86_64-unknown-none` target and its `kernel-x86_64` alias are scaffolding for
the planned port; the kernel does not boot on x86_64 yet.

### Assembly and the boot image (`kernel/build.rs`)

The kernel's `build.rs` does two build-time jobs:

- **Assembly** — it assembles the kernel's `.S` files with `clang` and passes the
  resulting objects straight to the linker. When `CARGO_CFG_TARGET_OS != "none"` it
  instead emits a `cargo::error=` line and stops, because a host build of the kernel
  is always a mistake (see [The kernel is not host-buildable](#the-kernel-is-not-host-buildable)).
  The demo-stub blob `user_stub.S` is assembled only when the `boot-stubs` feature is
  on (see [Boot stubs](#boot-stubs-boot-stubs-feature)).
- **Boot-image packing** — it builds each boot server for the EL0 user target in
  its own isolated `CARGO_TARGET_DIR`, packs the ELFs into the MXBI archive
  (`pack_mxbi`), and emits `BOOT_IMAGE_PATH` for the kernel to `include_bytes!`.
  There is no separate `mkbootimage` tool. See [Boot](boot/overview.md) for the
  archive format and module set.

### Boot stubs (`boot-stubs` feature)

The kernel installs four hand-written EL0 demo stubs A–D at boot — a live
regression battery for the IPC primitives (A↔B ping-pong), SCHED delegation (C),
and the VM page-fault / SIGSEGV path (D). They are useful but noisy: stub C's
`SYS_GETINFO` loop floods the trace. The **`boot-stubs` cargo feature (default-on)**
gates them, so `--no-default-features` yields a clean boot of servers + init/worker
only.

The feature lives on two crates — the **kernel** (gates the stub code in
`arch::aarch64::userland`) and **PM** (gates the stub `mproc` seeding). Because
`build.rs` builds each server in a *separate* nested cargo invocation with its own
feature resolution, it reads `CARGO_FEATURE_BOOT_STUBS` and, when the kernel is
stub-free, passes `--no-default-features` to the nested PM build too — keeping the
two in lockstep. The feature is intentionally **not** placed on `kernel-shared`: a
shared-crate default feature is force-enabled by other dependents (`minix-ipc`,
`server-rt`) through cargo *feature unification* and could not be turned off. So
`NR_STUB_PROCS` (= 4) and `FORK_POOL_BASE` (= 15) are constant regardless of the
feature — disabling stubs merely leaves proc slots 11–14 unoccupied; it does not
renumber the fork pool.

## The kernel is not host-buildable

`minixrs-kernel` compiles for `target_os = "none"` and nothing else: the ELF-only
`link_section` attributes, the `_start` entry path, the panic handler, and the
assembled `.S` objects all require it. It used to collapse to an empty `fn main() {}`
on the host so that `cargo check --workspace` stayed green — at the cost of hiding
every module behind `#[cfg(target_os = "none")]`, and therefore hiding all 48 kernel
source files from every lint gate. That arrangement is gone.

The crate stays a workspace `members` entry — one shared `Cargo.lock` keeps `audit` /
`deny` covering its dependencies, and `cargo fmt --all` still formats it (cargo-fmt
enumerates all members, and rustfmt follows `mod` declarations without evaluating
`cfg`) — but it is **omitted from `default-members`**:

```sh
cargo clippy --all-targets -- -D warnings          # skips the kernel
cargo clippy --workspace --all-targets             # DOES build the kernel -> error
cargo clippy --workspace --exclude minixrs-kernel --all-targets -- -D warnings   # ok
```

`--workspace` overrides `default-members`, so workspace-wide invocations must pass
`--exclude minixrs-kernel`; CI's `clippy` and `coverage` jobs do. A host build that
slips through fails with a single message from `build.rs` naming `cargo kernel-aarch64`
(a `#[cfg(not(target_os = "none"))] compile_error!` in `main.rs` states the same
invariant at the source). No `cfg(target_os = ...)` gates remain under `kernel/src/`.

> **rust-analyzer:** its default check is `cargo check --workspace`, which will report
> the kernel's guard as an error. Add
> `"rust-analyzer.check.extraArgs": ["--exclude", "minixrs-kernel"]` to your editor
> settings.

The `cfg_attr(target_os = "none", …)` attributes in `servers/*` and `userland/*` are a
different thing: those crates *are* host-built and host-tested, and the attribute only
hides an ELF section specifier from a Mach-O host.

## Host tests

Logic that can run off-target lives in `kernel-shared` and in the host-testable
server crates:

```sh
cargo test -p minixrs-kernel-shared
```

There is no `#[cfg(test)]` code under `kernel/src/` — the crate cannot be host-tested
and in-QEMU test infrastructure does not exist yet, so such tests would never run.
Pure predicates over shared ABI types belong in `kernel-shared` instead (`user_va_ok`
in `kernel-shared/src/message.rs` is the worked example); hardware and raw-pointer
behaviour stays in the kernel. QEMU is the primary verification for kernel code, and
CI smoke-boots it (below).

## CI

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. Ten jobs run
in parallel (`sonar` waits on `coverage`):

| Job | Blocking? | What it checks |
|-----|-----------|----------------|
| `fmt` | yes | `cargo fmt --all --check` (covers the kernel too) |
| `clippy` | yes | `cargo clippy --workspace --exclude minixrs-kernel --all-targets -- -D warnings` (host target) |
| `clippy-kernel` | yes | `cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings`, twice: default features and `--no-default-features` |
| `audit` | yes | `cargo-audit` advisory scan |
| `deny` | yes | `cargo-deny` (licenses / bans, config in `deny.toml`) |
| `geiger` | advisory | `unsafe` surface report (per package, kernel filtered out) |
| `miri` | advisory | UB check on the host-testable crates |
| `qemu-smoke` | yes | boots the kernel and greps the serial log |
| `coverage` | yes | `cargo-llvm-cov` → `lcov.info` (kernel excluded) |
| `sonar` | — | feeds LCOV to SonarQube Cloud |

Notes: since the kernel is excluded from every host gate, **`clippy-kernel` is the only
job that compiles kernel code** — which is why it blocks and runs on a native
`ubuntu-24.04-arm` runner (no cross toolchain needed for the `.S` files). It passes no
`--all-targets`: the kernel is `no_std`/`no_main`, so there is no test harness to build.
`qemu-smoke` (also `ubuntu-24.04-arm`) boots for 45 s wall clock, requires exit status
124 — the `timeout(1)` status a healthy, never-exiting kernel must produce — and then
runs `tools/check-boot-log.sh` against `tests/qemu-boot.expected` / `.forbidden`; keep
those expectations timing-robust (first occurrences, never counts), because CI's TCG is
slower than a local run. `Cargo.lock` is committed so `audit` / `deny` are reproducible,
and third-party actions are pinned to commit SHAs.

## Debugging: QEMU trace forensics

User-space servers run at EL0 with no console — they cannot print. All server
behavior is observed through kernel-side traces (`[as]`, `[ipc]`, `[ksys]`,
`[pf]`, `[alarm]`). Reading those logs has some sharp edges worth knowing:

- **`grep -a`.** The serial log interleaves raw single-character tick bytes, so
  tools treat it as binary ("Binary file matches"). Force text mode with `grep -a`
  (or `grep -aF`). Redirect the run to a file and grep that — a live tail loses
  lines.
- **TCG time skew.** QEMU under TCG advances *guest* time slower than wall clock,
  so a `timeout N` run reaches far fewer than `N × 100` ticks. For time-based
  behavior (alarms, quanta) read uptime-stamped traces (e.g. `[alarm … at=N]`) as
  the real clock, and run 20–25 s to observe several periods.
- **Sampling asymmetry.** `[ipc N]` head-traces the first ~12 calls *plus* every
  100th; `[ksys N]` samples only every 100th, with no head carve-out. A server's
  first or rare kernel call (e.g. a startup `SYS_GETINFO`) shows on `[ipc]`, not
  `[ksys]`.
- **Zero `[ipc]` samples ≠ a stuck caller.** A blocking `SENDREC` client (init's
  fork/wait loop, say) round-trips far too rarely for the modulo sampler to catch.
  Confirm liveness through its downstream head-carved `[ksys …]` traces
  (`SYS_FORK` / `SYS_EXIT` are head-carved), or add a temporary `[DBG]` trace in
  `ipc::do_ipc` keyed on the caller's proc number — and remove it before
  committing.
- **The acceptance harness.** `tools/check-boot-log.sh <log>` greps a captured log
  against `tests/qemu-boot.expected` and `tests/qemu-boot.forbidden` — the same
  check the `qemu-smoke` CI job runs. Update those marker files in the same change
  when trace formats or the boot roster shift.
- **Quiet the stubs.** Stub C's `SYS_GETINFO` loop dominates the `[ipc]`/`[ksys]`
  sample stream. When you're chasing a server or init/musl issue, boot
  `--no-default-features` to drop the demo stubs A–D entirely (see
  [Boot stubs](#boot-stubs-boot-stubs-feature)) — the trace then shows only the
  servers + init/worker. Note the `qemu-smoke` markers assume the default
  (stubs-on) boot, so don't run `check-boot-log.sh` against a stub-free log.

## Debugging with GDB

QEMU's GDB stub works through the runner's pass-through args:

```sh
# Terminal 1 — QEMU paused, waiting for a debugger (-S), stub on :1234 (-s)
cargo run -p minixrs-kernel --target aarch64-unknown-none --release -- -s -S

# Terminal 2
rust-gdb target/aarch64-unknown-none/release/minixrs-kernel \
    -ex "target remote :1234" -ex "break kmain" -ex "continue"
```

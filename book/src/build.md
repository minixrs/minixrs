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

The `x86_64-unknown-none` target block is scaffolding for the planned port; the
kernel does not boot on x86_64 yet. There is deliberately **no `kernel-x86_64`
alias** — `forced-target` (below) pins the kernel to aarch64 and overrides the
`--target` in an alias, so one would silently build aarch64 rather than fail.
Phase 8 must relax `forced-target` before adding it back.

### Assembly and the boot image (`kernel/build.rs`)

The kernel's `build.rs` does two build-time jobs:

- **Assembly** — it assembles the kernel's `.S` files with `clang` and passes the
  resulting objects straight to the linker. When `CARGO_CFG_TARGET_OS != "none"` it
  instead emits a `cargo::error=` line and stops, because a host build of the kernel
  is always a mistake (see [The kernel is not host-buildable](#the-kernel-is-not-host-buildable)).
  The demo-stub blob `user_stub.S` is assembled only when the `boot-stubs` feature is
  on (see [Boot stubs](#boot-stubs-boot-stubs-feature)).
- **Boot-image packing** — it builds each boot server for the custom EL0 user
  target `tools/targets/aarch64-unknown-minixrs.json` (via `-Zbuild-std`) into one
  shared nested `CARGO_TARGET_DIR` (`target/minixrs-user`, so `core`/`alloc` are
  compiled once rather than per crate), checks each ELF carries the minixrs
  identity note, packs them into the MXBI archive (`pack_mxbi`), and emits
  `BOOT_IMAGE_PATH` for the kernel to `include_bytes!`. There is no separate
  `mkbootimage` tool. See [Boot](boot/overview.md) for the archive format and
  module set.

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

Rather than hide the crate from workspace commands, `kernel/Cargo.toml` pins its
build target:

```toml
cargo-features = ["per-package-target"]

[package]
forced-target = "aarch64-unknown-none"

[[bin]]
name = "minixrs-kernel"
path = "src/main.rs"
test = false      # no_std/no_main: a --test build needs the `test` crate (std)
bench = false
```

Every cargo invocation therefore cross-compiles the kernel instead of failing on it —
bare or `--workspace`, and from any IDE:

```sh
cargo check --workspace --all-targets     # ok: kernel cross-compiled
cargo clippy --workspace --all-targets    # ok, and it genuinely LINTS kernel code
cargo test --workspace                    # ok: kernel has no test target
```

That last point is the payoff: kernel code is now visible to the lint gates instead of
merely hidden from them. `forced-target` wins even over an explicit
`--target x86_64-apple-darwin`, so a host build is unreachable, and no `--exclude` or
per-developer editor setting is required.

Three things to know:

- **`per-package-target` is unstable** ([cargo#9406](https://github.com/rust-lang/cargo/issues/9406)),
  hence the `cargo-features` opt-in and the nightly pin in `rust-toolchain.toml`. If a
  future bump drops it, cargo fails loudly on the manifest; the fallback is a workspace
  `default-members` list omitting `"kernel"`.
- **`test = false` is required.** Without it, `cargo check --all-targets` builds a
  phantom test harness for the kernel bin and fails with `E0463: can't find crate for test`.
- `build.rs`'s `cargo::error=` and `main.rs`'s `#[cfg(not(target_os = "none"))] compile_error!`
  are now unreachable defense-in-depth, kept in case `forced-target` ever stops applying.

No `cfg(target_os = ...)` gates remain under `kernel/src/`.

The `cfg_attr(target_os = "minixrs", …)` attributes in `servers/*` and `userland/*` are a
different thing: those crates *are* host-built and host-tested, and the attribute only
hides an ELF section specifier from a Mach-O host. (They keyed on `target_os = "none"`
until M1 moved the user-space binaries onto the `aarch64-unknown-minixrs` target.)

## Host tests

Logic that can run off-target lives in `kernel-shared` and in the host-testable
server crates:

```sh
cargo test -p minixrs-kernel-shared
cargo test -p minixrs-gen-c-headers    # the C ABI header generator
```

There is no `#[cfg(test)]` code under `kernel/src/` — the crate cannot be host-tested
and in-QEMU test infrastructure does not exist yet, so such tests would never run.
Pure predicates over shared ABI types belong in `kernel-shared` instead (`user_va_ok`
in `kernel-shared/src/message.rs` is the worked example); hardware and raw-pointer
behaviour stays in the kernel. QEMU is the primary verification for kernel code, and
CI smoke-boots it (below).

## CI

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. Eleven jobs run
in parallel (`sonar` waits on `coverage`):

| Job | Blocking? | What it checks |
|-----|-----------|----------------|
| `fmt` | yes | `cargo fmt --all --check` (covers the kernel too) |
| `clippy` | yes | `cargo clippy --workspace --exclude minixrs-kernel --all-targets -- -D warnings` (host target; kernel excluded for runner cost, see below) |
| `clippy-kernel` | yes | `cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings`, twice: default features and `--no-default-features` |
| `c-headers` | yes | regenerates the C ABI headers from `kernel-shared` and compiles them with `clang -std=c11 -fsyntax-only` (host + both musl triples) |
| `audit` | yes | `cargo-audit` advisory scan |
| `deny` | yes | `cargo-deny` (licenses / bans, config in `deny.toml`) |
| `geiger` | advisory | `unsafe` surface report (per package, kernel filtered out) |
| `miri` | advisory | UB check on the host-testable crates |
| `qemu-smoke` | yes | boots the kernel and greps the serial log |
| `coverage` | yes | `cargo-llvm-cov` → `lcov.info` (kernel excluded) |
| `sonar` | — | feeds LCOV to SonarQube Cloud |

Notes: CI's `clippy` and `coverage` exclude the kernel for **runner cost, not
correctness** — `forced-target` means they could build it, but only by having the x86
runner cross-assemble the `.S` files and run 8 nested server builds on a blocking gate.
So **`clippy-kernel` is the only CI job that compiles kernel code** (a local
`cargo clippy --workspace` does lint it) — which is why it blocks and runs on a native
`ubuntu-24.04-arm` runner. It passes no
`--all-targets`: the kernel is `no_std`/`no_main`, so there is no test harness to build.
`qemu-smoke` (also `ubuntu-24.04-arm`) boots for 45 s wall clock, requires exit status
124 — the `timeout(1)` status a healthy, never-exiting kernel must produce — and then
runs `tools/check-boot-log.sh` against `tests/qemu-boot.expected` / `.forbidden`; keep
those expectations timing-robust (first occurrences, never counts), because CI's TCG is
slower than a local run. `Cargo.lock` is committed so `audit` / `deny` are reproducible,
and third-party actions are pinned to commit SHAs.

## Generated C headers

The musl fork's view of the minix.rs ABI is **generated, never committed**
(phase-5 decision D8): `tools/gen-c-headers` depends on `kernel-shared` as an
ordinary Rust crate and prints C from the live constants, so the Rust and C
views cannot drift.

```sh
cargo gen-c-headers                 # -> target/gen-c-headers/
cargo gen-c-headers /some/sysroot   # explicit output directory
cargo gen-c-headers --stdout        # eyeball the output
```

It emits `include/minix/{ipc,com,callnr,errno}.h`, plus two artifacts that make
the CI gate real: `abi-selftest.c` — a header is never a translation unit on its
own, so without a `.c` file none of the generated `_Static_assert`s would ever
fire — and `abi-check/errno.h`, a CI-only stand-in for the C library's
`<errno.h>`.

Two things the headers are careful about:

- **`_PROC_NR` vs `_EP`.** Every process gets both. minix.rs sign-extends the
  endpoint proc field instead of using MINIX 3's offset bias, so for the kernel
  tasks the boot endpoint is *not* the process number (`SYSTEM_PROC_NR` is −2,
  `SYSTEM_EP` is 32766). Naming a task by its `_PROC_NR` in an IPC call is a bug,
  and the header asserts the C decode macro against the Rust-computed endpoints.
- **The POSIX errno block is asserted, never defined.** minix.rs adopts musl's
  numbering verbatim, so those values must come from the C library's own
  `<errno.h>`; `minix/errno.h` defines only the MINIX 200-band and puts the
  POSIX checks behind `MINIX_ABI_CHECK_POSIX_ERRNO`, which the musl build
  defines. See [System Calls & ABI](reference/syscalls.md).

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

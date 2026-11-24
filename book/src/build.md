# Build & Toolchain

minix.rs builds as a single Cargo workspace. The non-Cargo steps are fetching the prebuilt Limine
binary and, for the C programs in the root filesystem, building the musl fork into a sysroot with
`tools/build-musl.sh` (or pointing the build at the minix.rs SDK — see
[The C toolchain and the musl port](libc/overview.md)); `kernel/build.rs` then compiles and links
the C programs itself. A host tool, `tools/mkimage`, writes the boot disk image, and
`tools/qemu-run.sh` runs it and launches QEMU.

## Prerequisites

- **Rust nightly**, pinned in `rust-toolchain.toml` (a bare `nightly` would let new lints or fmt
  rules break the build with no code change).
- **QEMU** with `qemu-system-aarch64`.
- **aarch64 UEFI firmware** (edk2 / OVMF). `tools/qemu-run.sh` auto-detects it in common locations,
  or set `QEMU_EFI_AARCH64=/path/to/edk2-aarch64-code.fd`.

## Quick start

```sh
# One-time: fetch the pinned Limine binary into external/limine/dist/
make -C external/limine

# Build the kernel for aarch64 (the primary target)
cargo kernel-aarch64

# Build + boot under QEMU. The kernel runs indefinitely once EL0 starts, so a
# timeout is mandatory. Redirect to a file when you need to grep the log.
# Budget ~6 s for the rebuild, the disk image and UEFI firmware startup before
# the kernel's first byte -- `timeout 8` can yield a log with no kernel output
# at all. A boot meant to be checked with `tools/check-boot-log.sh` is given
# 600 s to reach the last marker the script requires, and is taken in the
# flavour CI builds: `MINIXRS_SDK=/nonexistent` selects the in-tree musl build
# of the C programs even when a minix.rs SDK is installed (CI gives each full
# boot 1200 s). A shorter run shows the early boot only. How much of the 600 s
# a boot uses depends on how the C programs were built: about 400 s for a disk
# boot with the musl build, about 250 s when `hello` comes from an installed
# SDK (slice 6.4's measurements) -- the SDK boot is fine for iterating, but it
# is not the boot the marker files and the timeouts are sized for. A checked
# boot is also FED its console input: stdin is tools/feed-console.sh, pointed
# at the log the boot is writing (see "Feeding the console" below).
MINIXRS_SDK=/nonexistent timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  < <(tools/feed-console.sh disk.log) > disk.log 2>&1

# Clean, stub-free boot for debugging: --no-default-features disables the
# `boot-stubs` feature, so only the servers + init/worker boot (no demo stubs
# A-D flooding the trace). It reaches the same markers far sooner, so 30 s is
# ample. See "Boot stubs" under Cargo workspace below.
timeout 30 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features
```

`cargo run` invokes the cargo runner (`tools/qemu-run.sh`), which builds one GPT disk image —
`target/disk.img`, written by the host tool `tools/mkimage`: a FAT32 ESP holding Limine, its
configuration and the freshly built kernel, plus the root filesystem partition — and boots QEMU with
that image as its only drive (see [Boot](boot/overview.md#disk-image-and-qemu) for the image layout
and the exact QEMU command). The firmware loads the kernel from the disk and MFS mounts its root
from the same disk through `virtio-blk`. `MINIXRS_ROOT=ramdisk` in the environment builds the image
without the root partition instead, so `virtio-blk` declines it and MFS falls back to the ramdisk;
`MINIXRS_ROOT=nodisk` puts the disk where only the firmware can see it (see
[The root backend](boot/overview.md#the-root-backend)). The image is rebuilt on every run; to boot
one again as it was left — the persistence proof — see
[Booting the same image twice](boot/overview.md#booting-the-same-image-twice). Early serial output
looks like:

```text
minix.rs booting on aarch64
HHDM offset: 0xffff000000000000
```

## Cargo workspace

The root `Cargo.toml` declares every crate as a workspace member: `kernel`, `kernel-shared`,
`minixrs-ipc`, `minixrs-abi-note`, `server-rt`, the six `servers/*`, `drivers/{driver-rt,tty,memory,
virtio-blk}` and `fs/mfs`, `userland/{init,worker,bigprog}`, the umbrella crate `minixrs`, and the
three host tools `tools/{gen-c-headers,mkfs-mfs,mkimage}`. Five more members are still empty stubs:
`drivers/virtio-console`, `drivers/virtio-net`, `fs/pfs`, `userland/sh` and `userland/coreutils`.
The C programs `userland/hello` and `userland/fileio` are not Cargo members; `kernel/build.rs`
builds them.

The kernel builds against the **builtin `aarch64-unknown-none` target** — not a custom JSON spec.
`.cargo/config.toml` wires the details:

```toml
[target.aarch64-unknown-none]
runner   = "tools/qemu-run.sh"
rustflags = ["-C", "link-arg=-Tkernel/src/arch/aarch64/linker.ld"]

[alias]
kernel-aarch64 = "build -p minixrs-kernel --target aarch64-unknown-none --release"
```

The `x86_64-unknown-none` target block is scaffolding for the planned port; the kernel does not boot
on x86_64 yet. There is deliberately **no `kernel-x86_64` alias** — `forced-target` (below) pins the
kernel to aarch64 and overrides the `--target` in an alias, so one would silently build aarch64
rather than fail. Phase 8 must relax `forced-target` before adding it back.

### Assembly and the boot image (`kernel/build.rs`)

The kernel's `build.rs` does two build-time jobs:

- **Assembly** — it assembles the kernel's `.S` files with `clang` and passes the resulting objects
  straight to the linker. When `CARGO_CFG_TARGET_OS != "none"` it instead emits a `cargo::error=`
  line and stops, because a host build of the kernel is always a mistake (see
  [The kernel is not host-buildable](#the-kernel-is-not-host-buildable)). The demo-stub blob
  `user_stub.S` is assembled only when the `boot-stubs` feature is on (see
  [Boot stubs](#boot-stubs-boot-stubs-feature)).
- **Boot-image packing** — it builds each boot server for the custom EL0 user target
  `tools/targets/aarch64-unknown-minixrs.json` (via `-Zbuild-std`) into one shared nested
  `CARGO_TARGET_DIR` (`target/minixrs-user`, so `core`/`alloc` are compiled once rather than per
  crate), checks each ELF carries the minixrs identity note, packs them into the MXBI archive
  (`pack_mxbi`), and emits `BOOT_IMAGE_PATH` for the kernel to `include_bytes!`. There is no
  separate `mkbootimage` tool. See [Boot](boot/overview.md) for the archive format and module set.
  The root filesystem image it packs as the `rootfs` module is also written, byte for byte, to
  `rootfs.img` in the cargo profile directory that holds the kernel ELF — outside `OUT_DIR`, which
  is unusual for a build script, because that is the one place the runner can find it from the
  kernel path it is handed. The runner passes it to `tools/mkimage` as the root partition's contents
  (see [The disk image](boot/overview.md#the-disk-image)).

### Boot stubs (`boot-stubs` feature)

The kernel installs four hand-written EL0 demo stubs A–D at boot — a live regression battery for the
IPC primitives (A↔B ping-pong), SCHED delegation (C), and the VM page-fault / SIGSEGV path (D). They
are useful but noisy: stub C's `SYS_GETINFO` loop floods the trace. The **`boot-stubs` cargo feature
(default-on)** gates them, so `--no-default-features` yields a clean boot of servers + init/worker
only.

The feature lives on two crates — the **kernel** (gates the stub code in `arch::aarch64::userland`)
and **PM** (gates the stub `mproc` seeding). Because `build.rs` builds each server in a *separate*
nested cargo invocation with its own feature resolution, it reads `CARGO_FEATURE_BOOT_STUBS` and,
when the kernel is stub-free, passes `--no-default-features` to the nested PM build too — keeping
the two in lockstep. The feature is intentionally **not** placed on `kernel-shared`: a shared-crate
default feature is force-enabled by other dependents (`minixrs-ipc`, `server-rt`) through cargo
*feature unification* and could not be turned off. So `NR_STUB_PROCS` (= 4) and `FORK_POOL_BASE`
(= 19) are constant regardless of the feature — disabling stubs merely leaves proc slots 15–18
unoccupied; it does not renumber the fork pool.

## The kernel is not host-buildable

`minixrs-kernel` compiles for `target_os = "none"` and nothing else: the ELF-only `link_section`
attributes, the `_start` entry path, the panic handler, and the assembled `.S` objects all require
it. It used to collapse to an empty `fn main() {}` on the host so that `cargo check --workspace`
stayed green — at the cost of hiding every module behind `#[cfg(target_os = "none")]`, and therefore
hiding all 48 kernel source files from every lint gate. That arrangement is gone.

Rather than hide the crate from workspace commands, `kernel/Cargo.toml` pins its build target:

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

Every cargo invocation therefore cross-compiles the kernel instead of failing on it — bare or
`--workspace`, and from any IDE:

```sh
cargo check --workspace --all-targets     # ok: kernel cross-compiled
cargo clippy --workspace --all-targets    # ok, and it genuinely LINTS kernel code
cargo test --workspace                    # ok: kernel has no test target
```

That last point is the payoff: kernel code is now visible to the lint gates instead of merely hidden
from them. `forced-target` wins even over an explicit `--target x86_64-apple-darwin`, so a host
build is unreachable, and no `--exclude` or per-developer editor setting is required.

Three things to know:

- **`per-package-target` is unstable**
  ([cargo#9406](https://github.com/rust-lang/cargo/issues/9406)), hence the `cargo-features` opt-in
  and the nightly pin in `rust-toolchain.toml`. If a future bump drops it, cargo fails loudly on the
  manifest; the fallback is a workspace `default-members` list omitting `"kernel"`.
- **`test = false` is required.** Without it, `cargo check --all-targets` builds a phantom test
  harness for the kernel bin and fails with `E0463: can't find crate for test`.
- `build.rs`'s `cargo::error=` and `main.rs`'s `#[cfg(not(target_os = "none"))] compile_error!` are
  now unreachable defense-in-depth, kept in case `forced-target` ever stops applying.

No `cfg(target_os = ...)` gates remain under `kernel/src/`.

The `cfg_attr(target_os = "minixrs", …)` attributes in `servers/*` and `userland/*` are a different
thing: those crates *are* host-built and host-tested, and the attribute only hides an ELF section
specifier from a Mach-O host. (They keyed on `target_os = "none"` until M1 moved the user-space
binaries onto the `aarch64-unknown-minixrs` target.)

## Host tests

Logic that can run off-target lives in `kernel-shared` and in the host-testable server crates:

```sh
cargo test -p minixrs-kernel-shared
cargo test -p minixrs-gen-c-headers    # the C ABI header generator
cargo test -p minixrs-mkimage          # the disk-image writer, read back by independent readers
```

There is no `#[cfg(test)]` code under `kernel/src/` — the crate cannot be host-tested and in-QEMU
test infrastructure does not exist yet, so such tests would never run. Pure predicates over shared
ABI types belong in `kernel-shared` instead (`user_va_ok` in `kernel-shared/src/message.rs` is the
worked example); hardware and raw-pointer behaviour stays in the kernel. QEMU is the primary
verification for kernel code, and CI smoke-boots it (below).

## CI

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`. It has thirteen jobs. Eleven
are gates that run in parallel — the same eleven the repository's `docs/conventions/ci.md` counts;
`qemu-smoke` first waits on `boot-scope`, which is a classifier rather than a gate, and `sonar`
waits on `coverage`:

| Job             | Blocking? | What it checks                                                                                                                             |
| --------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `fmt`           | yes       | `cargo fmt --all --check` (covers the kernel too)                                                                                          |
| `clippy`        | yes       | `cargo clippy --workspace --exclude minixrs-kernel --all-targets -- -D warnings` (host target; kernel excluded for runner cost, see below) |
| `clippy-kernel` | yes       | `cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings`, twice: default features and `--no-default-features`         |
| `c-headers`     | yes       | regenerates the C ABI headers from `kernel-shared` and compiles them with `clang -std=c11 -fsyntax-only` (host + both musl triples)        |
| `audit`         | yes       | `cargo-audit` advisory scan                                                                                                                |
| `deny`          | yes       | `cargo-deny` (licenses / bans, config in `deny.toml`)                                                                                      |
| `dco`           | yes       | every commit in the PR carries a `Signed-off-by:` trailer (pull requests only)                                                             |
| `geiger`        | advisory  | `unsafe` surface report (per package, kernel filtered out)                                                                                 |
| `miri`          | advisory  | UB check on the host-testable crates                                                                                                       |
| `boot-scope`    | —         | classifies the change with `tools/boot-relevant.sh`; `qemu-smoke` skips the boots when it says docs-only                                   |
| `qemu-smoke`    | yes       | boots the kernel three times — disk, a reboot of that image, the rootless-disk fallback — and greps the serial logs                        |
| `coverage`      | yes       | `cargo-llvm-cov` → `lcov.info` (kernel excluded)                                                                                           |
| `sonar`         | —         | feeds LCOV to SonarQube Cloud                                                                                                              |

Notes: CI's `clippy` and `coverage` exclude the kernel for **runner cost, not correctness** —
`forced-target` means they could build it, but only by having the x86 runner cross-assemble the `.S`
files and run 8 nested server builds on a blocking gate. So **`clippy-kernel` is the only CI job
that compiles kernel code** (a local `cargo clippy --workspace` does lint it) — which is why it
blocks and runs on a native `ubuntu-24.04-arm` runner. It passes no `--all-targets`: the kernel is
`no_std`/`no_main`, so there is no test harness to build. `qemu-smoke` (also `ubuntu-24.04-arm`)
boots three times: `MINIXRS_ROOT=disk` for 1200 s, then a 180 s reboot of the image that boot wrote
(`MINIXRS_VBLK_IMAGE=target/disk.img`), then `MINIXRS_ROOT=ramdisk` for 1200 s. The two full boots
are [fed their console input](#feeding-the-console); the reboot is not. It requires exit status 124
of all three — the `timeout(1)` status a healthy, never-exiting kernel must produce. It then runs
`tools/check-boot-log.sh <disk|reboot|ramdisk> <log>` on each log. The two full boots are checked
against the common marker files `tests/qemu-boot.expected` / `.forbidden` plus their backend's own
pair (`tests/qemu-boot.disk.*` or `tests/qemu-boot.ramdisk.*`); the reboot is checked against
`tests/qemu-boot.reboot.*` alone. `tools/diff-backends.sh` then requires the filesystem's own output
to be the same on both full boots. There is no separate `cargo test` job: the host tests run inside
`coverage`. Keep the expectations timing-robust (first occurrences, never counts), because CI's TCG
is slower than a local run. `Cargo.lock` is committed so `audit` / `deny` are reproducible, and
third-party actions are pinned to commit SHAs.

## Generated C headers

The musl fork's view of the minix.rs ABI is **generated, never committed** (phase-5 decision D8):
`tools/gen-c-headers` depends on `kernel-shared` as an ordinary Rust crate and prints C from the
live constants, so the Rust and C views cannot drift.

```sh
cargo gen-c-headers                 # -> target/gen-c-headers/
cargo gen-c-headers /some/sysroot   # explicit output directory
cargo gen-c-headers --stdout        # eyeball the output
```

It emits `include/minixrs/{ipc,com,callnr,errno}.h`, plus two artifacts that make the CI gate real:
`abi-selftest.c` — a header is never a translation unit on its own, so without a `.c` file none of
the generated `_Static_assert`s would ever fire — and `abi-check/errno.h`, a CI-only stand-in for
the C library's `<errno.h>`.

Two things the headers are careful about:

- **`_PROC_NR` vs `_EP`.** Every process gets both. minix.rs sign-extends the endpoint proc field
  instead of using MINIX 3's offset bias, so for the kernel tasks the boot endpoint is *not* the
  process number (`SYSTEM_PROC_NR` is −2, `SYSTEM_EP` is 32766). Naming a task by its `_PROC_NR` in
  an IPC call is a bug, and the header asserts the C decode macro against the Rust-computed
  endpoints.
- **The POSIX errno block is asserted, never defined.** minix.rs adopts musl's numbering verbatim,
  so those values must come from the C library's own `<errno.h>`; `minixrs/errno.h` defines only the
  MINIX 200-band and puts the POSIX checks behind `MINIXRS_ABI_CHECK_POSIX_ERRNO`, which the musl
  build defines. See [System Calls & ABI](reference/syscalls.md).

## Debugging: QEMU trace forensics

User-space servers run at EL0 with no console — they cannot print. All server behavior is observed
through kernel-side traces (`[as]`, `[ipc]`, `[ksys]`, `[pf]`, `[alarm]`). Reading those logs has
some sharp edges worth knowing:

- **`grep -a`.** The serial log interleaves raw single-character tick bytes, so tools treat it as
  binary ("Binary file matches"). Force text mode with `grep -a` (or `grep -aF`). Redirect the run
  to a file and grep that — a live tail loses lines.
- **TCG time skew.** QEMU under TCG advances *guest* time slower than wall clock, so a `timeout N`
  run reaches far fewer than `N × 100` ticks. For time-based behavior (alarms, quanta) read
  uptime-stamped traces (e.g. `[alarm … at=N]`) as the real clock, and run 20–25 s to observe
  several periods.
- **Sampling asymmetry.** `[ipc N]` head-traces the first ~12 calls *plus* every 100th; `[ksys N]`
  samples only every 100th, with no head carve-out. A server's first or rare kernel call (e.g. a
  startup `SYS_GETINFO`) shows on `[ipc]`, not `[ksys]`.
- **Zero `[ipc]` samples ≠ a stuck caller.** A blocking `SENDREC` client (init's fork/wait loop,
  say) round-trips far too rarely for the modulo sampler to catch. Confirm liveness through its
  downstream head-carved `[ksys …]` traces (`SYS_FORK` / `SYS_EXIT` are head-carved), or add a
  temporary `[DBG]` trace in `ipc::do_ipc` keyed on the caller's proc number — and remove it before
  committing.
- **The acceptance harness.** `tools/check-boot-log.sh <disk|ramdisk> <log>` greps a captured log
  against `tests/qemu-boot.expected` and `tests/qemu-boot.forbidden`, which hold for every full
  boot, and against the named backend's own `tests/qemu-boot.<backend>.expected` / `.forbidden` —
  the same check the `qemu-smoke` CI job runs on each of its two full boots. The backend is the
  `MINIXRS_ROOT` the boot ran with (`disk` when unset) and the argument is required: the common
  files alone cannot tell a disk boot that silently fell back to the ramdisk from a healthy one. A
  third value, `reboot`, checks the second boot of a used disk image against
  `tests/qemu-boot.reboot.*` and nothing else. (A `MINIXRS_ROOT=nodisk` boot has no pair of its
  own.) Given one full-boot log of each backend, `tools/diff-backends.sh <disk-log> <ramdisk-log>`
  checks that nothing else differs: it takes each log's `[diag vfs]` lines and `[diag mfs]` lines as
  two separate streams, keeps the first occurrence of each line, and fails unless the VFS streams
  are identical and the MFS streams differ only in three places — the `bdev.ds ok name=` line that
  names the driver, the one `bdev.root vblk rc=-6 fallback=memory` line the ramdisk boot may carry,
  and the verdict of the `bdev.minor` probe (`ok minor=1 mbr=1` on the disk, `absent minor=1 rc=-6`
  on the ramdisk). Update the marker files in the same change when trace formats or the boot roster
  shift.
- **Feeding the console.** See [the next section](#feeding-the-console): a full boot that was not
  fed fails exactly one required line, `minix.rs init: stdin ok n=15`, and that is the feeder
  missing, not a regression.
- **Quiet the stubs.** Stub C's `SYS_GETINFO` loop dominates the `[ipc]`/`[ksys]` sample stream.
  When you're chasing a server or init/musl issue, boot `--no-default-features` to drop the demo
  stubs A–D entirely (see [Boot stubs](#boot-stubs-boot-stubs-feature)) — the trace then shows only
  the servers + init/worker. Note the `qemu-smoke` markers assume the default (stubs-on) boot, so
  don't run `check-boot-log.sh` against a stub-free log.

## Feeding the console

QEMU runs with `-serial stdio`, so the boot's standard input is the PL011's receive side, and since
slice 6.5 something reads it: a child of `init` blocks in `read(0)` and reports what arrives
([Drivers](drivers/overview.md#what-the-boot-log-proves) has the whole path). A boot that is to pass
`tools/check-boot-log.sh` is therefore given its input by `tools/feed-console.sh`:

```sh
MINIXRS_SDK=/nonexistent timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  < <(tools/feed-console.sh disk.log) > disk.log 2>&1
```

The script is the boot's stdin and is pointed at the log the same boot is writing. It polls that log
for `[diag vfs] cons.beside ok` — VFS's line saying it served a file read while a console read was
parked — and then writes `minix.rs stdin` and a newline, once, and exits. It is a process
substitution rather than a pipe so that the shell does not wait for it and the command's exit status
is still `timeout`'s.

Waiting for a line in the log, rather than for a delay, does two jobs. It makes the proof causal:
the reading process cannot be woken before VFS has been seen serving somebody else beside it. And it
keeps the bytes away from the boot loader — anything on stdin before the kernel starts is consumed
by the firmware or Limine and never reaches TTY. The script wants to see the line on two polls in a
row, because the log path is usually one an earlier boot wrote and the shell starts the script an
instant before truncating it.

- If the line never appears the script gives up after `FEED_CONSOLE_CEILING` seconds (1800 by
  default) having written nothing. CI sets 1300 for its 1200 s boots.
- Closing QEMU's stdin is harmless — QEMU carries on at EOF — so the script does not linger after
  its one write.
- The string the script sends and the one `init` expects are two copies; a host test in
  `userland/init` reads the script and fails if they differ.
- An **unfed** boot is healthy and prints everything except `stdin ok`; the reader stays parked. The
  reboot of a disk image is run unfed, since its marker set needs no input.
- A boot run by hand with a terminal attached delivers whatever is typed to that same reader, which
  will almost certainly print `stdin FAIL` — correct, and harmless outside a checked boot. Nothing
  is echoed: the console has no line discipline yet.

## Debugging with GDB

QEMU's GDB stub works through the runner's pass-through args:

```sh
# Terminal 1 — QEMU paused, waiting for a debugger (-S), stub on :1234 (-s)
cargo run -p minixrs-kernel --target aarch64-unknown-none --release -- -s -S

# Terminal 2
rust-gdb target/aarch64-unknown-none/release/minixrs-kernel \
    -ex "target remote :1234" -ex "break kmain" -ex "continue"
```

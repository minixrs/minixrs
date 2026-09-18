# Build and boot

How to build minix.rs, how to boot it under QEMU, and how to inspect what came out. Rules that bind
every build live here; see [`ci.md`](./ci.md) for what CI enforces.

```sh
# Build kernel for aarch64 (primary target)
cargo kernel-aarch64

# Boot in QEMU (cargo runner wires tools/qemu-run.sh). The kernel runs
# indefinitely once EL0 starts (slice 2.4+), so `timeout` is mandatory.
# Redirect to a file when you need to grep tick output -- live tail loses lines.
# The log interleaves raw single-char tick bytes; grep it with `grep -a`
# (force text) or matches read as "Binary file matches".
# Budget ~5 s of every run for the cargo rebuild + UEFI firmware startup before
# the kernel's first byte: `timeout 8` yields a log with NO kernel output at all
# (check-boot-log.sh then fails all markers).
#
# HOW LONG: **300 s** for anything you mean to verify against the marker files
# (qemu-smoke itself uses 600 s as of 5.10b). The number is NOT a constant, it
# tracks how far into the boot the LAST required marker sits. It was 25 s through
# slice 5.8 and 120 s through 5.10a, and it is stale advice the moment a slice
# makes the boot longer. Measured on the musl flavour (`MINIXRS_SDK=/nonexistent`,
# the one CI builds), `hello: errno ok` -- the last required marker -- sits at
# 26.90% of a 240 s log before slice 5.10b and 61.61% after it, i.e. ~148 s in.
# Re-measure rather than trusting this paragraph: `grep -abo <marker> log | head -1`
# divided by `wc -c log`, and raise the budget when the fraction climbs. The two
# things that move it most are the `hello` flavour (~47 KB with the SDK vs ~200 KB
# with in-tree musl, all of it read through MFS and VFS) and whether the demo stubs
# are on -- `--no-default-features` reaches the same markers **dramatically**
# faster, because stub C's kernel-call flood dominates a default boot: 5.10b
# measured the `fs.*` markers landing at ~0.14% of a 60 s stub-free log, so
# `timeout 30` is ample there. That is the configuration to iterate and
# mutation-test in; just remember `check-boot-log.sh` reports FAIL on the stub
# A-D markers there by design, so judge a stub-free run by grepping the specific
# marker rather than by the script's overall verdict.
#
# QEMU under TCG also advances *guest* time slower than wall-clock, so a
# `timeout N` run reaches far fewer than N x 100 ticks. For time-based features
# (alarms, quantum/scheduling) read uptime-stamped traces (e.g. `[alarm ... at=N]`)
# as the real clock, and run long enough to observe several periods.
timeout 300 cargo run -p minixrs-kernel --target aarch64-unknown-none --release

# Clean, stub-free boot for debugging (servers + init/worker only, no demo
# stubs A-D): add --no-default-features to disable the `boot-stubs` feature.
timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features

# Inspect a built user ELF's segments (macOS ships no `readelf`; the pinned
# toolchain ships llvm-readobj, and --elf-output-style=GNU matches readelf's
# output). Mandatory whenever a `user.ld` changes -- a segment layout claim
# ("the headers are mapped now") must be checked, not assumed:
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-readobj --program-headers \
  --elf-output-style=GNU target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-worker

# Largest stack frame in a built server -- the one-page-stack check (a server gets
# exactly `uspace::SERVER_STACK_BYTES`, and overrunning it faults into VM's SIGSEGV
# arm, which prints nothing the forbidden list catches). Run it whenever a handler
# grows a buffer. `sort -u` is LEXICAL and reports the wrong maximum -- convert to
# decimal first:
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump -d \
  target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-mfs \
  | grep -oE 'sub[[:space:]]+sp, sp, #0x[0-9a-f]+' | grep -oE '0x[0-9a-f]+' \
  | while read h; do printf '%d\n' "$h"; done | sort -n | tail -1

# Verify a captured boot log against the standard acceptance markers:
#   tools/check-boot-log.sh <log>   (tests/qemu-boot.expected/.forbidden;
# update those marker files in the same PR when trace formats or the boot
# roster change, or the qemu-smoke CI job goes red)
# For the marker-file contract itself (partial-log behavior, PASS-total counting,
# etc.), see ./testing-and-markers.md.

# (There is no `kernel-x86_64` alias: `forced-target` pins the kernel to
# aarch64, so such an alias would silently build aarch64 instead of failing.
# x86_64 is Phase 8 -- there is no kernel/src/arch/x86_64/ yet either.)

# Run host-side unit tests (note the package name, not the dir name)
cargo test -p minixrs-kernel-shared
cargo test -p minixrs-gen-c-headers

# Regenerate the C ABI headers for the musl fork (slice 5.0). Output is a build
# artifact under target/ and is NEVER committed; `--stdout` dumps it instead.
cargo gen-c-headers

# Build the musl fork into target/musl-sysroot (slice 5.6). Needed for the C
# `hello` program; without it kernel/build.rs warns and packs `worker` under the
# name `hello`, so boots stay green but the C markers vanish. Cached via a
# .stamp of submodule SHA + toolchain; --force rebuilds. Leaves external/musl
# pristine. Also runs the real D7 errno *value* check against the fork's
# bits/errno.h. MUSL_SRC=<dir> points it at a local fork checkout instead.
git submodule update --init --recursive
tools/build-musl.sh

# The C `hello` program has THREE possible toolchains, in strict preference order
# (P3c): the minix.rs SDK at $MINIXRS_SDK (default $HOME/toolchains/minixrs, a
# contractual path from tooling's docs/sysroot-layout.md) -> the in-tree musl
# sysroot above -> the `worker` ELF packed under the name `hello`. The middle one
# is NOT a fallback: no CI job installs an SDK, so it is qemu-smoke's real
# dependency. Only the third loses markers. A usable SDK that fails to build
# PANICS rather than demoting -- the boot markers are byte-identical across
# flavors, so a silent demotion would report a regressed toolchain as healthy.
# Report is host-side only: SDK warning => sdk, fallback warning => worker,
# neither => musl (silent on purpose; warning on the norm trains people to ignore
# build-script warnings). Force a flavor:
MINIXRS_SDK=/nonexistent cargo kernel-aarch64     # in-tree musl; deletes nothing
MINIXRS_SDK=~/toolchains/minixrs cargo kernel-aarch64
# NEVER write inside the SDK prefix -- tooling's build-musl.sh does
# `rm -rf $SDK/sysroot`. Everything this repo produces goes to target/hello/.

# Reproduce the blocking `c-headers` CI gate locally (hermetic: -nostdlibinc
# keeps the host libc out, so it cannot validate against wrong errno values):
clang -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsyntax-only \
  -ffreestanding -nostdlibinc --target=aarch64-unknown-linux-musl \
  -Itarget/gen-c-headers/include target/gen-c-headers/abi-selftest.c
```

The kernel crate is **bare-metal only** and pins its own build target via `forced-target` (see the
conventions list), so `cargo check` / `clippy` / `test` — bare or `--workspace`, and from any IDE —
cross-compile it instead of failing. No `--exclude` or per-developer editor setting is needed.
(Separately and pre-existing: bare `cargo build` fails on the *server*/userland crates — they are
`#![no_main]` ELFs that can't link against the host libc. `check`/`clippy`/`test` are the host
gates, not `build`.)

## Feature toggles and dependency claims

- The demo stubs A–D are gated behind a **`boot-stubs` cargo feature (default-on)**. The feature
  lives on **two** crates — the kernel (gates `arch::aarch64::userland`'s stub code) and PM (gates
  `mproc::seed`'s stub loop) — because those are the only two that install/seed stubs.
  `kernel/build.rs` reads `CARGO_FEATURE_BOOT_STUBS` to drop `user_stub.S` from the assembly
  `sources` and to pass `--no-default-features` through to the *nested* PM build (the nested build
  has its own feature resolution, so the flag must be threaded through explicitly), keeping kernel
  and PM in lockstep. The feature is deliberately **not** on `kernel-shared`: a shared-crate default
  feature gets force-enabled by other dependents (`minixrs-ipc`, `server-rt`) via cargo **feature
  unification**, which would make it impossible to turn off — so `NR_STUB_PROCS` stays a constant
  `4`, and `FORK_POOL_BASE` (= 15) is therefore stable: disabling stubs leaves slots 11–14
  **unoccupied**, it does **not** renumber the fork pool.
- When a `--no-default-features` build doesn't actually drop a feature, suspect cargo **feature
  unification** — diagnose with `cargo tree -p <crate> --no-default-features -e features -i
  <shared-crate>` (the inverted tree shows *who* still activates it) or `cargo tree -p <crate> -f
  "{p} {f}"` (feature set per crate).
- Before accepting any "this adds a dependency/compile cost" claim — a review finding included —
  check it with `cargo tree -p minixrs-kernel -e build`, which prints the build-script graph.
- Adding a workspace crate: append it to `members` in the root `Cargo.toml`, use literal manifest
  fields plus `publish = false` for anything internal, and, if its entry point is pure I/O with the
  testable logic in sibling modules, add its `main.rs` to `sonar.coverage.exclusions`.
- The C `hello` program has three possible toolchains, in strict preference order: the minix.rs SDK
  at `$MINIXRS_SDK` → the in-tree musl sysroot → the `worker` ELF packed under the name `hello`.
  Never write inside `$MINIXRS_SDK`; all output this repo produces goes to `target/hello/`. A usable
  SDK that fails to build **panics rather than demoting** to the next flavor.

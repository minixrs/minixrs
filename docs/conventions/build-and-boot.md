# Build and boot

How to build minix.rs, how to boot it under QEMU, and how to inspect what came out. Rules that bind
every build live here; see [`ci.md`](./ci.md) for what CI enforces.

**`cargo kernel-aarch64` is not a kernel-only build.** `kernel/Cargo.toml` build-depends on
`minixrs-mkfs-mfs` → `minixrs-mfs`, and `kernel/build.rs` shells out to build every server ELF — so
it transitively builds most of the workspace, and an error in `servers/*` or `fs/*` from this
command is not necessarily a kernel bug. `build.rs` scrubs `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS` but
**not** `RUSTC_WORKSPACE_WRAPPER`, so `cargo clippy -p minixrs-kernel -- -D warnings` runs clippy
with `-D warnings` over those nested server builds too. Two consequences: a neighbouring crate's
not-yet-called code fails your build, and work split across agents cannot be isolated by `-p` target
alone.

```sh
# Build kernel for aarch64 (primary target)
cargo kernel-aarch64

# Boot in QEMU (cargo runner wires tools/qemu-run.sh). The kernel runs
# indefinitely once EL0 starts (slice 2.4+), so `timeout` is mandatory.
# Redirect to a file when you need to grep tick output -- live tail loses lines.
# The log interleaves raw single-char tick bytes; grep it with `grep -a`
# (force text) or matches read as "Binary file matches".
# Budget ~6 s of every run for the cargo rebuild, the disk image build (below)
# and UEFI firmware startup before the kernel's first byte: `timeout 8` yields a
# log with NO kernel output at all (check-boot-log.sh then fails all markers).
#
# HOW LONG: **600 s, in the MUSL flavour** (`MINIXRS_SDK=/nonexistent`, as on
# the command below) for anything you mean to verify against the marker files
# (qemu-smoke itself gives each full boot 1200 s as of slice 6.4). Musl because
# that is what CI builds and what the marker files and the timeouts are sized
# for: an installed SDK is used even with MINIXRS_SDK unset, and its smaller
# `hello` makes a different, faster boot; if you build in a separate command,
# set the variable there too. The number is NOT a constant, it tracks how far
# into the boot the LAST required marker sits. It was 25 s through slice 5.8,
# 120 s through 5.10a and 300 s through 6.3, and it is stale advice the moment a
# slice makes the boot longer. Slice 6.4 raised it because a musl disk boot now
# reaches its last required marker ~396 s in: 600 s is 1.5x that, the factor
# 300 s had at 6.3. Every measurement, the factors, and which flavour each
# reading was taken on are in ./ci.md (the bullet "A slice can break the
# boot-timing budget") and are not restated here. An SDK-flavour boot (drop the
# variable) gets there in ~248 s and is fine for ITERATION, but it is neither
# the budget measurement nor a marker-file verification.
# Re-measure rather than trusting this paragraph: `grep -abo <marker> log | head -1`
# divided by `wc -c log`, and raise the budget when the fraction climbs. The three
# things that move it most are the `hello` flavour (~47 KB with the SDK vs ~200 KB
# with in-tree musl, all of it read through MFS and VFS), the root backend (below:
# a disk boot completes every block request by a real interrupt instead of a
# memcpy, and since 6.4 follows every write with a flush, a second interrupt
# round trip) and whether the demo stubs
# are on -- `--no-default-features` reaches the same markers **dramatically**
# faster, because stub C's kernel-call flood dominates a default boot: 5.10b
# measured the `fs.*` markers landing at ~0.14% of a 60 s stub-free log, and 6.4
# measured every one of its markers inside the first 0.33% of a 60-75 s one, so
# `timeout 30` after a separate build is ample there. That is the configuration
# to iterate and mutation-test in; just remember `check-boot-log.sh` reports FAIL
# on the stub A-D markers there by design, so judge a stub-free run by grepping
# the specific marker rather than by the script's overall verdict.
#
# QEMU under TCG also advances *guest* time slower than wall-clock, so a
# `timeout N` run reaches far fewer than N x 100 ticks. For time-based features
# (alarms, quantum/scheduling) read uptime-stamped traces (e.g. `[alarm ... at=N]`)
# as the real clock, and run long enough to observe several periods.
#
# A boot checked against the marker files is a FED boot (slice 6.5): its stdin
# is tools/feed-console.sh, pointed at the log the same boot is writing, in
# exactly this form -- `< <(tools/feed-console.sh <log>) > <log> 2>&1`. What the
# feeder does, why it is a process substitution, and what an unfed boot prints
# are under "Fed boots" below.
MINIXRS_SDK=/nonexistent timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  < <(tools/feed-console.sh disk.log) > disk.log 2>&1

# ONE DISK, THREE ROOT BACKENDS (slice 6.4; two backends since 6.3). The machine
# boots from ONE disk: a GPT image holding a FAT32 ESP (Limine, limine.conf, the
# kernel) and, on a disk boot, the MinixFS root partition. The firmware reads
# the kernel from it and `vblk` then drives the same device. `MINIXRS_ROOT`
# selects what is on the image and which transport it sits on:
#
#   MINIXRS_ROOT=disk     (the default, i.e. the command above) target/disk.img:
#                         ESP + root partition, on virtio-mmio. `vblk` finds the
#                         root by its GPT type GUID; MFS mounts through `vblk`.
#   MINIXRS_ROOT=ramdisk  target/disk-rootless.img: the same image WITHOUT the
#                         root partition (same size), on virtio-mmio. `vblk`
#                         brings it up, prints `root FAIL no root partition`, MFS
#                         prints `bdev.root vblk rc=-6 fallback=memory` and mounts
#                         the `memory` ramdisk. This is the no-root FALLBACK boot.
#   MINIXRS_ROOT=nodisk   the rootless image as virtio-blk-PCI: the firmware
#                         reads it, minix.rs has no PCI, so `vblk` prints `no
#                         device` and MFS finds no `vblk` in DS. Hand-run only --
#                         CI does not boot it and it has no marker pair (how its
#                         log fares against the marker files is under "The
#                         no-device state has no checked boot" in
#                         ./testing-and-markers.md#markers).
#
# A slice that touches the block path, MFS or VFS owes both full boots AND the
# reboot below (the Definition of done, ./docs-and-workflow.md#slice-workflow
# step 4, is the rule) -- the ramdisk backend stays green for the whole phase
# (H7). Keep the logs apart; the checker and tools/diff-backends.sh each want to
# be told which is which.
MINIXRS_SDK=/nonexistent MINIXRS_ROOT=ramdisk timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  < <(tools/feed-console.sh ramdisk.log) > ramdisk.log 2>&1

# The image is BUILT ON EVERY RUN by the host tool tools/mkimage, which the
# runner starts through a nested `cargo run -q --release -p minixrs-mkimage`
# (about a second once built; it does not wait on the outer cargo's lock). Its
# inputs are the kernel ELF cargo handed the runner, Limine's BOOTAA64.EFI,
# tools/limine.conf and -- for `disk` -- `rootfs.img`, which kernel/build.rs
# writes beside the kernel ELF: the very bytes it packs into the boot archive as
# the ramdisk, so the two backends serve one filesystem image. Rebuilt rather
# than reused because the boot WRITES to its root: a reused image would make the
# second run a different test from the first. The images always land in the
# repository's own target/ (CARGO_TARGET_DIR is not honoured for them).
# `MINIXRS_ROOT` is an environment variable, so like `$MINIXRS_SDK` it does not
# survive into a separate shell invocation: set it on the command that boots.
#
# Two more knobs (a disk boot only; `ramdisk` and `nodisk` reject them, exit 64):
#   MINIXRS_VBLK_IMAGE=<file>       attach that file AS-IS: no build, no copy, and
#                                   the boot's writes land in it. The machine
#                                   boots ENTIRELY from that file, so it needs
#                                   its own ESP with a kernel on it -- the kernel
#                                   ELF cargo just built is NOT used, and a file
#                                   without an ESP gives no kernel at all, only
#                                   the firmware's boot-failure output.
#   MINIXRS_VBLK_DRIVE_OPTS=<opts>  appended to the -drive argument, e.g.
#                                   `readonly=on`: `vblk` then prints `root FAIL
#                                   readonly` and MFS falls back. Without
#                                   MINIXRS_VBLK_IMAGE the fresh image is still
#                                   built and the options apply to it.
#
# THE REBOOT (the persistence proof): boot the image a disk boot just wrote,
# as-is, and check it against the reboot marker pair. It must print
# `minix.rs init: persist gen=1`. THE REBOOT-ORDERING RULE: it MUST run before
# anything rebuilds target/disk.img -- every plain disk boot does -- so it comes
# immediately after the disk boot whose image it reads. And RUN IT ONCE PER DISK
# BOOT: each boot of the image stores the next digit, so a second reboot of the
# same image prints gen=2 and fails the reboot set by design; take a fresh disk
# boot first. It needs only that one line (~34 s in locally); CI gives it 180 s.
# The reboot is NOT fed: nothing in its marker pair depends on console input.
MINIXRS_VBLK_IMAGE=target/disk.img timeout 60 cargo run -p minixrs-kernel \
  --target aarch64-unknown-none --release > reboot.log 2>&1
tools/check-boot-log.sh reboot reboot.log
#
# The runner's QEMU command line: one `-drive file=<image>,format=raw,if=none,id=vblk0`
# plus `-device virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.0` (for `nodisk`,
# one `-drive ...,if=virtio` instead), and `-global virtio-mmio.force-legacy=false`.
# The runner attaches ONE other device, on every boot whatever the root backend
# (slice 6.6): `-chardev pipe,id=hvc0,path=<base>` with
# `-device virtio-serial-device,bus=virtio-mmio-bus.8` and
# `-device virtconsole,chardev=hvc0` -- the second console, /dev/hvc0, whose host
# side is the FIFO pair <base>.in / <base>.out (target/hvc0 unless MINIXRS_HVC0
# is set; see "Fed boots" below). Every boot therefore also prints
# `[virtio] slot=8 pa=0xa001000 id=3 ver=2 drv=vcon` and
# `[diag vcon] virtio ok features=0x100000000 rxq=8 txq=8 slot=8`. Nothing else
# is attached (`-M virt` still creates its default devices, a virtio-net-pci card
# among them: the runner passes no `-nodefaults`). The
# runner prints the exact argv on stderr as one
# `qemu-run: ...` line (and `qemu-run: attaching <file> as-is ...` when it built
# nothing) -- the first thing to read when a boot surprises. Extra arguments are
# appended last, so a later `-global` overrides the default. Both full boots, and
# the reboot, print `[virtio] slot=0 pa=0xa000000 id=2 ver=2 drv=vblk` and the driver's
# `[diag vblk] virtio ok features=0x100000200 capacity=75776 ...` line; 75776 is
# the whole disk in sectors (tools/mkimage's `layout::DISK_SECTORS`), the same on
# both images, and tests/qemu-boot.{disk,ramdisk}.expected are the other holders
# of that number.
#
# A SECOND DISK, for a driver verdict the firmware would not let you reach (a disk
# with no ESP, a damaged GPT): boot `nodisk`, so the firmware has the rootless
# image on PCI, and pass the disk under test as the mmio drive after `--`:
#   MINIXRS_ROOT=nodisk timeout 60 cargo run ... -- \
#     -drive file=<img>,format=raw,if=none,id=vblk0 \
#     -device virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.0
# Caveat: the firmware tries the mmio disk FIRST. If the disk under test has a
# usable ESP it boots from THAT, with that image's own kernel. And on a WRITABLE
# disk edk2 repairs a damaged primary GPT from the backup before the kernel runs
# (see ./testing-and-markers.md#mutation-testing) -- add `readonly=on`.

# Clean, stub-free boot for debugging (servers + init/worker only, no demo
# stubs A-D): add --no-default-features to disable the `boot-stubs` feature.
timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features

# Prove the kernel idle loop (slice 6.1, spec I14): the opt-in `idle-probe` feature builds init so
# its rotation ends and it parks, letting the run queue empty. Only meaningful stub-free --
# `build.rs` refuses it together with the default-on `boot-stubs` (the stubs spin forever, so the
# queue could never empty) with a message naming `--no-default-features`. Expect `[idle] wake ok`
# and `[diag tty] irq ok` in the log. CI does not build this configuration. It does not exercise
# console input either: init forks neither proof child under this feature (the rule is under
# "init: PID 1" in ./servers-and-drivers.md), so feeding it proves nothing.
timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  --no-default-features --features idle-probe

# Inspect a built user ELF's segments (macOS ships no `readelf`; the pinned
# toolchain ships llvm-readobj, and --elf-output-style=GNU matches readelf's
# output). Mandatory whenever a `user.ld` changes -- a segment layout claim
# ("the headers are mapped now") must be checked, not assumed:
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-readobj --program-headers \
  --elf-output-style=GNU target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-worker

# Largest stack frame in a built server -- the stack-budget check. A server gets
# exactly `uspace::USER_STACK_BYTES` (64 KiB, 16 pages); overrunning it lands in
# the unmapped guard page below, which VM turns into a SIGSEGV that prints
# nothing the forbidden list catches. Run it whenever a handler grows a buffer.
#
# Do NOT reduce this to `grep -oE 'sub sp, sp, #0x[0-9a-f]+' | sort -n | tail -1`.
# That pattern is what this file used to carry and it LIES: LLVM encodes a frame
# >= 4 KiB as a *run* of subtractions, and the shifted form
# `sub sp, sp, #0x1, lsl #12` does not match it at all. Against minixrs-mfs the
# naive grep reports 1248 where `main`'s real frame is 9440 (0x1<<12 twice, plus
# 0x4e0) -- it under-reports by 7.5x on exactly the large frames it exists to
# catch. Sum the run per function, and decode both encodings:
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump -d \
  target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-mfs \
  | awk '
      function hx(s,   i, n, d) {
        gsub(/[#,]/, "", s); sub(/^0x/, "", s); n = 0
        for (i = 1; i <= length(s); i++) {
          d = index("0123456789abcdef", substr(tolower(s), i, 1)) - 1
          n = n * 16 + d
        }
        return n
      }
      /^[0-9a-f]+ </       { if (cur > max) { max = cur; who = fn }
                             fn = $2; cur = 0 }
      /sub[ \t]+sp, sp, #/ { cur += hx($6) * (/lsl #12/ ? 4096 : 1) }
      END                  { if (cur > max) { max = cur; who = fn }
                             print max, who }'
# -> 9440 <_RNvCs..._11minixrs_mfs4main>   (the symbol hash varies per build)
#
# Two caveats the one-liner cannot express. It counts `sub sp, sp` only, so the
# callee-save pre-index (`stp x29, x30, [sp, #-0x60]!`) is a further 96 bytes on
# top -- read the prologue directly when a frame is close enough to 64 KiB for
# that to matter. And `hx` decodes hex by hand because `strtonum` is a gawk
# extension that macOS's awk does not have.

# Verify a captured boot log against the standard acceptance markers. The first
# argument names the boot the log came from and is REQUIRED:
#   tools/check-boot-log.sh disk    <log>   (a default boot)
#   tools/check-boot-log.sh ramdisk <log>   (a MINIXRS_ROOT=ramdisk boot)
#   tools/check-boot-log.sh reboot  <log>   (the reboot above)
# `disk` and `ramdisk` each check tests/qemu-boot.expected/.forbidden plus that
# backend's own tests/qemu-boot.<backend>.expected/.forbidden; `reboot` checks
# tests/qemu-boot.reboot.expected/.forbidden and nothing else. Then, with one
# full-boot log of each backend:
#   tools/diff-backends.sh <disk-log> <ramdisk-log>
# Update the marker files in the same PR when trace formats or the boot roster
# change, or the qemu-smoke CI job goes red. For the marker-file contract itself
# (which file a marker belongs in, partial-log behavior, PASS-total counting,
# what the diff can and cannot see), see ./testing-and-markers.md.

# (There is no `kernel-x86_64` alias: `forced-target` pins the kernel to
# aarch64, so such an alias would silently build aarch64 instead of failing.
# x86_64 is Phase 8 -- there is no kernel/src/arch/x86_64/ yet either.)

# Run host-side unit tests (note the package name, not the dir name)
cargo test -p minixrs-kernel-shared
cargo test -p minixrs-gen-c-headers
cargo test -p minixrs-mkimage   # the disk-image writer, read back by independent readers

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

## Fed boots

QEMU's serial is `-serial stdio` (`tools/qemu-run.sh`), so a boot's stdin **is** the PL011's receive
side. Since slice 6.5 the console can be read, and since slice 6.6 there is a second console,
`/dev/hvc0`, whose host side is a pair of FIFOs. A boot that is to be checked against the marker
files has to be given its input on **both** — by `tools/feed-console.sh`, never by hand:

```sh
MINIXRS_SDK=/nonexistent timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  < <(tools/feed-console.sh <log>) > <log> 2>&1
```

- **The form is exact: `< <(tools/feed-console.sh <log>) > <log> 2>&1`**, with the same log path
  twice. The feeder reads the log the boot is writing. It is a **process substitution**, not a pipe,
  so the shell does not wait on the feeder and the command's exit status is still `timeout`'s (124
  for a healthy boot). Process substitution is bash and zsh, not POSIX `sh`.
- **What the feeder does: four stages, the second console first** (slice 6.6, C12; before it the
  feeder had stages 3 and 4 only, and stage 3 waited for one line).
  1. It reads `<base>.out` until the guest's own line, `minix.rs hvc0 out`, has come out of it —
     init's second proof child writes that through `/dev/hvc0`, so seeing it is the guest-to-host
     direction observed on the host.
  2. It waits `SETTLE` (3) seconds, then writes `minix.rs hvc0 in` and a newline — 17 bytes, one
     time only — to `<base>.in`, in two chunks.
  3. It polls the log for **both** `[diag vfs] cons.beside ok` and `minix.rs init: hvc0 ok`.
  4. It writes the serial probe — `minix.rs stdin` and a newline, 15 bytes, one time only — to its
     stdout, in two chunks, and exits 0. init's first proof child is blocked in `read(0)` for
     exactly those bytes and prints `minix.rs init: stdin ok n=15`.

  The script's `PROBE`, `HVC0_OUT`, `HVC0_IN` and `HVC0_OK` and init's `CONSOLE_PROBE`, `HVC0_OUT`,
  `HVC0_IN` and `HVC0_OK_LINE` are copies of each other; a host test in `userland/init` reads the
  script and fails if any differs, so keep each assignment on one single-quoted line. stdout carries
  the serial probe and nothing else: the second console is written through its own descriptor, and
  every diagnostic goes to stderr, one line per stage.
- **The second console's host side is a FIFO pair, `target/hvc0.in` and `target/hvc0.out`.**
  `tools/qemu-run.sh` attaches `-chardev pipe,id=hvc0,path=<base>` to a `virtio-serial-device` on
  `virtio-mmio-bus.8` with one `virtconsole` port, on every boot, and makes both FIFOs with `mkfifo`
  when they are missing; QEMU appends `.in` and `.out` itself. The host writes guest input to `.in`
  and reads guest output from `.out`. **`MINIXRS_HVC0=<base>`** moves the pair, for the runner and
  the feeder alike (the feeder inherits the boot's environment) — set it when two boots run at once,
  or they share one console. Something that is not a FIFO at either path is refused, by the runner
  (exit 73) and by the feeder (exit 1), never replaced. Nobody has to be on either end: QEMU starts
  with no peer, and a guest write with no reader completes, so an unfed boot is unaffected.
- **One feeder per FIFO pair.** A fed boot whose build fails never starts QEMU, so nothing closes
  the FIFO under its feeder, which then waits out its ceiling. Two readers on one FIFO split the
  guest's line, and the old feeder could take the next boot's. Each feeder therefore writes its pid
  to `<base>.feeder` and kills the feeder it finds named there (checked by command line, so a
  recycled pid is not signalled), saying so on stderr. Found by the slice's review, not by a boot.
- **The feeder empties both FIFOs before it looks at them — the stale-bytes drain.** A pipe is not
  emptied when QEMU exits: on macOS about 826 bytes of an unread boot — the firmware's text and the
  guest's line — were still in `.out` afterwards with no process holding it. Left there, the next
  fed boot's feeder finds that **old** line at once, writes its answer into a machine that is still
  in its firmware, and the answer is lost: no `hvc0 ok`, and by the coupling below no `stdin ok`
  (observed, exactly so). So a FIFO that already exists when the feeder starts is read for `DRAIN`
  (0.5) seconds and the bytes discarded, with one stderr line saying how many. It is the two-polls
  rule's twin, and it is safe only because the feeder starts **with** the boot command — QEMU is a
  build and a firmware away. Started against a boot that is already running it can throw the guest's
  line away and then never feed. Unread boots with no fed boot between them accumulate in the pipe;
  that is a carried debt, not something the runner fixes
  ([`phase-6-virtio.md`](../plans/phase-6-virtio.md#slice-66-virtio-console-as-a-second-cdev-backend)).
- **The guest's line is looked for as a substring, never as the first bytes.** edk2 and Limine use
  the port too and put about 800 bytes of escape-laden boot text on it before the kernel starts. The
  feeder copies `.out` into a temporary file with a background `cat` and greps that, which also
  keeps the port drained for the rest of the boot; the `cat`'s end-of-file after it has read
  something means QEMU is gone, and the feeder then exits 1 instead of polling a dead boot's log.
- **`SETTLE` is there so that the child's read is parked when the bytes arrive.** The host sees the
  guest's line the moment the driver hands it to the device, which is before the child's write has
  returned; written at once, the first chunk beat the child's read on a stubs-on boot, and the read
  was refused directly without ever having parked. The boot passes either way, but only a parked
  read is retried by VFS, and that retry — a raw request with an unmapped buffer, re-granted and
  refused — is what the read is there to exercise. Like `GAP` it is a guess: nothing in the log says
  whether the read was parked.
- **The coupling: a boot whose `vcon` is broken loses `stdin ok` as well as `hvc0 ok`.** Stage 4
  waits for stage 3, deliberately: keeping the serial reader parked for the whole of the `hvc0`
  exchange is what lets VFS be seen passing over it (`[diag vfs] cons.other ok`). Read the first
  missing line; the order is `cons.other ok`, `hvc0 ok`, `stdin ok`.
- **Each probe is written in two chunks, and the gap between them is load-bearing.** The feeder
  writes the first `SPLIT` (9) bytes, sleeps `GAP` (3) seconds, and writes the rest — to `.in` in
  stage 2 and to stdout in stage 4. For the second console the second chunk needs a second interrupt
  **and** a re-posted receive buffer: `vcon` has one, and the chunk arrives only if the driver gave
  it back to the device and re-armed after the first was drained. For the serial console: one write
  reaches TTY as one interrupt, and a probe that arrives in one interrupt says nothing about what
  TTY does *after* it: with the probe fed in one piece, a TTY that never re-armed its line after the
  drain still passed the boot (slice 6.5, mutation 5). The second chunk needs a second interrupt,
  which arrives only if `IRQ_ENABLE` followed the first drain, so `stdin ok` now proves the re-arm
  too — and the reading child's loop over short counts runs on every fed boot. **Do not merge the
  two writes or shorten the gap to tidy the script.** `GAP` is a guess, not a measurement: if both
  chunks ever land in one drain the boot still passes and the re-arm simply goes unproved, with
  nothing to say so. The same holds for a hand-rolled feed — a `printf` of the whole string into
  QEMU proves one interrupt.
- **The serial probe waits for a marker, not for a delay, and that is load-bearing twice over.** VFS
  prints `cons.beside ok` the first time it completes a file read while a console read is parked, so
  the child's read cannot return before VFS has been seen serving somebody beside it — the proof is
  causal. And bytes written earlier do not arrive at all (the third serial measurement below).
- **Stage 3's two lines must be seen, together, on two consecutive polls** (0.2 s apart). The log
  path is usually one a previous boot wrote, and the shell starts the feeder an instant before `>
  <log>` truncates it; one look could find the old boot's line and write into the firmware.
- **`FEED_CONSOLE_CEILING=<seconds>`** (default 1800) is how long the feeder polls before it gives
  up and exits 1 — whichever stage it is in, with nothing written anywhere in stage 1 and nothing
  written to stdout in stage 3 — so a boot that never gets that far cannot leave a poller behind for
  good. There is no stall detection — it is a ceiling and nothing else. Set it a little over the
  boot's `timeout`; CI uses 1300 against 1200 (see [`ci.md`](./ci.md)).
- **The gate is ordered, not synchronised.** init forks the reading child before its first file
  read, and `fileio` reads `/etc/motd` again on every fourth turn of the rotation for as long as the
  boot runs, so a file read after the park is never long in coming — but nothing makes init wait for
  the child to park first. A `cons.beside ok` that came late would only delay the feeder.
- **The reboot is not fed**, and neither is a boot nobody is going to check. An unfed boot — stdin
  at EOF, or a terminal nobody types at, and nobody on the FIFOs — prints `cons.park` and
  `cons.beside ok`, never `cons.other ok`, `hvc0 ok` or `stdin ok`, and so **fails
  `check-boot-log.sh` on those three lines by design**; both reading children stay parked for the
  whole boot, which costs the boot nothing but two fork slots. Type at a hand-run boot and the first
  input lands in that child's read and prints `stdin FAIL`, which is correct there. Which marker
  lines hold for which boot is in [`testing-and-markers.md`](./testing-and-markers.md#markers).
- **Nothing is echoed.** The console delivers raw bytes with no line discipline, so typing into a
  hand-run boot shows nothing on screen.

Three facts about QEMU's serial input were measured before the design was fixed (2026-11-23, QEMU
11, Homebrew's edk2, stub-free boots), and the rules above lean on them:

1. **The firmware leaves the PL011's FIFO enabled, and one byte still interrupts.** `UARTLCR_H`
   reads `0x70` (`FEN` set) and `UARTIFLS` `0x12` (receive trigger at half), yet a single byte
   raised the line at once — one notification, `UARTRIS = 0x30`, the receive-timeout bit clear — so
   QEMU raises RX on the first byte whatever the trigger level says. Three bytes written together
   arrived in one notification; 36 written together were drained in one, more than the 16-deep FIFO
   holds, because QEMU refills it as the driver empties it. TTY's loop does not depend on any of
   this.
2. **EOF on stdin is harmless.** With `< /dev/null` QEMU carries on and the boot is unchanged, which
   is why the feeder may exit once it has written instead of holding its stdout open.
3. **Bytes on stdin before the kernel starts never reach TTY.** Five bytes present from time zero
   were consumed by the firmware or Limine. A feeder that wrote at once, or after a short fixed
   delay, would feed the boot loader — so input for a test is always gated on a line from the log.

And five about the second console's `pipe` chardev (2026-11-24, QEMU 11.0.3, Homebrew's edk2, macOS
on Apple Silicon — one machine; the CI runner's answer is the branch's first run):

1. **QEMU starts with nobody on either FIFO**, and the boot is unchanged.
2. **A guest write with no reader on the host side completes**, with its transmit interrupt; the
   bytes sit in the pipe.
3. **The firmware writes to the port before the kernel starts**: 808 bytes, with the guest's line
   straight after them. And bytes written to `.in` while the firmware is running are lost, as on the
   serial console.
4. **Bytes written while the guest's one receive buffer is held are kept** and delivered when the
   driver posts it again; a 600-byte host write arrived as three used elements of 256, 256 and 88
   bytes.
5. **The pipe keeps an unread boot's bytes after QEMU has exited** — about 826 on macOS — which is
   what the feeder's drain is for.

Slice 6.6's reading, fed and in the musl flavour: the last required marker at 55.38% of a 600 s disk
log and 37.65% of the ramdisk one. The second console did not push the budget; see the
[phase file](../plans/phase-6-virtio.md) for the comparison with 6.5 and what was not established.

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
  `4`, and `FORK_POOL_BASE` (= 19) is therefore stable: disabling stubs leaves slots 15–18
  **unoccupied**, it does **not** renumber the fork pool.
- When a `--no-default-features` build doesn't actually drop a feature, suspect cargo **feature
  unification** — diagnose with `cargo tree -p <crate> --no-default-features -e features -i
  <shared-crate>` (the inverted tree shows *who* still activates it) or `cargo tree -p <crate> -f
  "{p} {f}"` (feature set per crate).
- Before accepting any "this adds a dependency/compile cost" claim — a review finding included —
  check it with `cargo tree -p minixrs-kernel -e build`, which prints the build-script graph.
- Adding a workspace crate: append it to `members` in the root `Cargo.toml`, use literal manifest
  fields plus `publish = false` for anything internal (`release.yml` publishes a hardcoded
  five-crate list — `minixrs-kernel-shared`, `minixrs-ipc`, `minixrs-server-rt`,
  `minixrs-driver-rt`, `minixrs` — and only those five inherit `[workspace.package]` and carry
  crates.io metadata), and, if its entry point is pure I/O with the testable logic in sibling
  modules, add its `main.rs` to `sonar.coverage.exclusions`.
- The C `hello` program has three possible toolchains, in strict preference order: the minix.rs SDK
  at `$MINIXRS_SDK` → the in-tree musl sysroot → the `worker` ELF packed under the name `hello`.
  Never write inside `$MINIXRS_SDK`; all output this repo produces goes to `target/hello/`. A usable
  SDK that fails to build **panics rather than demoting** to the next flavor.
- A C program's linker script ends with `PROVIDE(_end = .)` after `.bss`: lld defines `_end` only
  for a link with no `SECTIONS` command, so the musl flavour fails with "undefined symbol: _end"
  without it. `hello.ld` and `fileio.ld` stay identical below their headers (chunk 5).
- The SDK flavour compiles C programs hosted at `-O2`; the in-tree musl flavour is `-ffreestanding`.
  A hosted build may delete a `malloc`/`free` pair whose result is never used, so a probe must make
  the allocation observable (`void *volatile p`).

# Slice 6.6 — `virtio-console` as a second CDEV backend — Implementation Plan

**Goal:** a write to `/dev/hvc0` leaves the machine on a second QEMU chardev, bytes fed to that
chardev come back from a `read()` of the node, and a reader parked on the other console is not in
the way.

**Spec:**
[`../specs/2026-11-24-virtio-console-design.md`](../specs/2026-11-24-virtio-console-design.md),
decisions `C1…C14`, under the phase decisions `H1…H12` of
[`../../plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md) and 6.5's `R1…R12`. Where this plan
and the spec disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **Branch `feature/6.6-virtio-console`.** Every commit `--signoff`ed and GPG-signed; never
  `--no-gpg-sign`, never `--no-verify`. **No push, no PR, no CI trigger.**
- **SPDX header** first in every new `.rs` file.
- **No ABI value moves** (D8, C13). `cargo gen-c-headers` output is diffed against `main` and must
  be byte-identical.
- **No kernel change.** If Task 0 or a later task finds one is needed, that is a finding: it goes in
  the ledger and to the user before any kernel code.
- **No polling** (I12): `vcon` blocks in `receive` between requests and while a transmit is in
  flight; VFS never spins on `EAGAIN`.
- **`unsafe` in `virtio-console`:** the two `driver-rt` constructors in `main.rs` and nothing else.
  `bringup.rs` and `port.rs` have none.
- **Overflow-safe arithmetic** for every offset and length derived from a request or from the
  device.
- **Boots.** `$S` is the session scratch directory. Build first (`MINIXRS_SDK=/nonexistent cargo
  build -p minixrs-kernel --target aarch64-unknown-none --release`), then `MINIXRS_SDK=/nonexistent
  MINIXRS_ROOT=<disk|ramdisk> timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none
  --release < <(tools/feed-console.sh $S/<name>.log) > $S/<name>.log 2>&1`. One boot at a time.
  Iterate and mutation-test stub-free (`--no-default-features`), judging by the specific marker.
  Read `check-boot-log.sh`'s whole output; apply the starved-boot check before believing a missing
  marker.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing).
- **Counts in markers come from constants**, never typed into a driver.
- **Markdown** wraps at 100; `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.

## Marker text (fixed here; the kernel's two lines are read from Task 0's boot, not derived)

```text
[virtio] slot=8 pa=<…> id=3 ver=2 drv=vcon        kernel probe (exact text from Task 0)
[dma] vcon va=<…> pages=16                        kernel (exact text from Task 0)
[diag vcon] virtio ok features=0x100000000 rxq=<n> txq=<n> slot=8
[diag vcon] no device                             forbidden
[diag vcon] virtio FAIL step=<step> …             forbidden (substring `[diag vcon] virtio FAIL`)
[diag vcon] rx FAIL notify rc=<rc>                forbidden (substring `[diag vcon] rx FAIL`)
[diag vfs] vcon.ds ok ep=<ep>
[diag vfs] vcon.ds FAIL rc=<rc> fallback=<ep>     forbidden (substring `vcon.ds FAIL`)
[diag vfs] cons.other ok                          a walk passed over another driver's reader, then answered one
minix.rs init: hvc0 ok n=<HVC0_IN.len()>
minix.rs init: hvc0 FAIL step=<step> …            forbidden (substring `hvc0 FAIL`)
```

`HVC0_OUT` is `minix.rs hvc0 out\n` and `HVC0_IN` is `minix.rs hvc0 in\n`; each lives once in
`userland/init`, and a host test there compares them with the strings `tools/feed-console.sh` holds.
The fork-cycle markers move one slot up (the second long-lived child) and are recomputed, not
edited.

## Review Focus

1. **Advance after the copy** — `off` moves only once `SYS_SAFECOPY` returned `OK`. Pinned by
   `port.rs` host tests and, on a boot, by the child's bad-buffer read (`hvc0 ok`; mutation 1).
2. **Re-post when drained, and only then** — a partial read leaves the buffer held; the re-post and
   the queue notify precede the reply. `port.rs` tests; mutation 2 through the feeder's second
   chunk.
3. **Device-supplied length** — clamped to the buffer; zero re-posts and holds nothing. `port.rs`
   tests with a length above the buffer and with zero.
4. **`CDEV_READ` answer order** — validate, zero-length `0`, broken `EIO`, posted `EAGAIN` with the
   waiter recorded first, held copy. `driver-rt::cdev` tests; the order in `main.rs` by reading.
5. **Both walks on every interrupt, ack first, wake after re-arm** — including the interrupt taken
   inside the transmit wait. By reading against `driver-rt`'s crate docs; mutations 3, 4 and 11.
6. **A used element out of turn ends the device** — a transmit completion with nothing in flight, a
   receive completion while held. `port.rs` tests; the `break` path by reading.
7. **A stopped device wakes its waiter, in both drivers** (C9) — host tests on `read_plan` and on
   TTY's `IrqService`; mutation 12 moves nothing on a boot, which is expected.
8. **The moved code is a move** — `Waiter` / `read_plan` / the validator in `driver-rt::cdev` behave
   as they did in TTY; TTY's tests moved with them and none was dropped. Compare test names before
   and after.
9. **The per-driver walk** — skip another driver's entry; the first `EAGAIN` from the notifier ends
   it; a count or an error frees; `EBADF` for a non-device on any walk; it terminates. The step
   function's host tests, `park.rs`'s in-order walk tests, `cons.other ok`, mutations 7 and 8.
10. **Never reply to a notification** — the widened arm still sits before the dispatch and still
    `continue`s for every source.
11. **Tripwires grown, not left** — `NR_DEV_NODES`, the "uses every driver" test in `dev.rs`, any
    `match` on `CharDriver`, the fork-cycle slot numbers, init's child count and wait-again logic.
12. **The feeder** — `hvc0` first, gated on the guest's own output; two chunks; the serial probe
    gated on `cons.beside ok` **and** `hvc0 ok`; an unfed boot leaves nothing blocked.
13. **Layout arithmetic** — four pages, each queue alone on its page, both buffers in the remainder;
    verify the offsets by hand against `USER_PAGE_SIZE`.

## Tasks

### Task 0: Measure first (C14)

Throwaway changes to `qemu-run.sh` and a throwaway probe driver, never committed; restored from a
scratchpad snapshot. The five items of C14. Results in the ledger; a finding that contradicts the
spec rewrites the spec before Task 1.

### Task 1: R1's pure half moves to `driver-rt`; a stopped TTY line says `EIO` (C6, C9)

**Files:** `drivers/driver-rt/src/cdev.rs` (new), `drivers/driver-rt/src/lib.rs`,
`drivers/driver-rt/Cargo.toml` if a dependency is needed, `drivers/tty/src/{cdev,irq,main}.rs`.

- Move `Waiter`, `ReadPlan` / `read_plan`, `read_len` and the validator (parameterised by minor)
  with their tests. `read_plan`'s third argument becomes "input can still arrive".
- TTY: `IrqService` reports whether receive can still happen; the transition to `Stopped` wakes the
  waiter once.

**Proves it:** `cargo test -p minixrs-driver-rt -p minixrs-tty`; `cargo miri test -p
minixrs-driver-rt`; a fed stub-free boot with every marker unmoved.

### Task 2: VFS — the node, the third driver, the per-driver walk (C10)

**Files:** `kernel-shared/src/callnr.rs`, `servers/vfs/src/{dev,fd,park,main}.rs`.

- `DEV_HVC0_PATH`, `CDEV_MINOR_HVC0`; the fourth row; `CharDriver::Vcon`.
- `park.rs`: the in-order walk. The walk's step function, pure, with tests written first.
- `main.rs`: `vcon_endpoint()` with fallback; the peers struct; the notifier set; the walk;
  `cons.other ok`.
- Inert at boot until Task 4: slot 12 has no ELF yet, so the lookup takes its fallback. `vcon.ds ok`
  is not required until Task 4.

**Proves it:** `cargo test -p minixrs-vfs`; a fed stub-free boot with every prior marker present.

### Task 3: the driver's pure half (C5, C6, C7)

**Files:** `drivers/virtio-console/{Cargo.toml,build.rs,user.ld}`,
`drivers/virtio-console/src/{bringup,port}.rs`, `src/main.rs` reduced to the module lines and the
brand.

**Proves it:** `cargo test -p minixrs-virtio-console`, tests first (Review Focus 1–3, 6, 13).

### Task 4: the driver, packed and attached (C1–C9)

**Files:** `drivers/virtio-console/src/main.rs`, `kernel/build.rs`, `tools/qemu-run.sh`, the common
marker files, `sonar-project.properties` if `main.rs` needs its exclusion.

**Proves it:** an unfed stub-free boot prints the kernel's two lines, `[diag vcon] virtio ok …` and
`[diag vfs] vcon.ds ok`, with every prior marker present.

### Task 5: the proof child and the feeder (C11, C12)

**Files:** `userland/init/src/main.rs`, `tools/feed-console.sh`, marker files.

**Proves it:** a fed stub-free boot prints `hvc0 ok`, `cons.other ok` and `stdin ok n=15`, in that
order; an unfed one prints none of the three and no `FAIL`.

### Task 6: CI

**Files:** `.github/workflows/ci.yml` only if the invocation has to change (C12 says it does not);
`actionlint` / `shellcheck` on what was touched.

### Task 7: recorded boots (spec §3)

Both full musl-flavour boots fed, `check-boot-log.sh` for each, `diff-backends.sh`, the unfed
reboot, the budget reading against Task 0's baseline, the header diff, every gate
[`ci.md`](../../conventions/ci.md) lists as run-locally.

### Task 8: mutations (spec §3)

The spec's thirteen rows, each applied, observed and reverted from a scratchpad snapshot, on fed
stub-free boots. A row that behaves differently is recorded as seen.

### Task 9: documentation, trackers, the whole-branch sweep (spec §4)

Book, conventions, the 6.5 spec's R12 sentence, the phase file, `docs/plan.md`, the stale-claim
grep, then the fresh whole-branch review of the diff.

## Ledger

One entry per task: its commit, what was verified, and every ruling made in it.

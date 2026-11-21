# Slice 6.3 — `virtio-blk` behind BDEV + root backend selection — Implementation Plan

**Goal:** virtio-blk serves `BDEV_READ` / `BDEV_WRITE` under completion interrupts, and MFS mounts
its root from it when a disk carrying a minix.rs root is attached; the ramdisk boot stays green.

**Spec:** [`../specs/2026-11-21-virtio-blk-design.md`](../specs/2026-11-21-virtio-blk-design.md),
decisions `B1…B11`, under the phase decisions `H1…H12` of
[`../../plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md). Where this plan and the spec
disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **Branch `feature/6.3-virtio-blk`.** Every commit `--signoff`ed and GPG-signed; never
  `--no-gpg-sign`, never `--no-verify`. **No push, no PR, no CI trigger.**
- **SPDX header** first in every new `.rs` file.
- **No ABI value moves** (D8). `BDEV_MINOR_ROOT` is additive.
- **No kernel behaviour change.** No `#[cfg(test)]` under `kernel/src/`.
- **No polling** (I12): the driver blocks in `receive` for every completion.
- **Overflow-safe arithmetic** in every sector, offset and layout computation.
- **`unsafe`** only in `driver-rt`'s `mmio.rs`, `queue.rs`, `dma.rs` and at virtio-blk's existing
  bring-up call sites, each block with a `// SAFETY:` comment. `cargo miri test -p
  minixrs-driver-rt` after touching any of it.
- **Boots.** `$S` is the session scratch directory. Build first (`cargo build -p minixrs-kernel
  --target aarch64-unknown-none --release`), then `MINIXRS_ROOT=<disk|ramdisk> timeout <n> cargo run
  -p minixrs-kernel --target aarch64-unknown-none --release > $S/<name>.log 2>&1`. One boot at a
  time: two at once starve each other. Read `check-boot-log.sh`'s whole output, never through
  `tail`; apply the starved-boot check before believing a missing marker.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing): snapshot to
  `$S` (including files this plan adds), restore from the snapshot, `diff -q`, `grep -rn MUTATION`
  clean, `grep -a 'error\[E'` before recording.
- **Counts in markers come from constants.**
- **Markdown** wraps at 100; `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.

## Marker text (fixed here)

```text
[diag vblk] virtio ok features=0x100000000 capacity=2048 qsize=8 slot=0
[diag vblk] root ok blocks=256 tail=1
[diag vblk] root FAIL <label|blocks|geometry|tail|tail label|head rc=…|tail rc=…>
[diag vblk] virtio FAIL step=io …
[diag vblk] no device
[diag mfs] bdev.ds ok name=vblk ep=…
[diag mfs] bdev.ds ok name=memory ep=…
[diag mfs] bdev.root vblk rc=<rc> fallback=memory
```

`2048` is `ROOTFS_IMAGE_BYTES / 512` and `256` is `ROOTFS_IMAGE_BLOCKS`; both are printed from the
device's capacity and the image header, never typed into the driver.

## Review Focus

1. **The H5 race** (B1). The DS publish must precede every blocking step; the verdict must precede
   the first reply. Pinned by mutations 3 and 5 and the disk boot's forbidden `name=memory`.
2. **A lost completion.** Acknowledge exactly the bits read, then walk, then re-arm; `rearm` after
   *every* interrupt, including one that completes nothing. Mutation 1.
3. **Sector arithmetic.** `block * 8` checked; the last block's last sector is `capacity - 1`; a
   capacity not a multiple of 8 is refused. Host tests in Task 4; mutation 2.
4. **Partial writes.** `0 < len < 4096` preserves the block's remaining bytes; a failed
   `SAFECOPY_FROM` leaves the device untouched in both write shapes. Host test in Task 4.
5. **The status byte.** Preset `0xFF`; only `0` is success. Host test in Task 4; mutation 6.
6. **A faulty device.** An unsolicited, out-of-range or stale used id is a `QueueFault`, never
   skipped; the driver goes `EIO`-only and stays in its loop. Host tests in Task 3.
7. **Identical refusals.** Both drivers answer MFS's `bdev.deny` battery from one
   `server-rt::bdev::validate_read`. `diff-backends.sh`.
8. **`DmaBuf` bounds.** `DmaRegion::buf` refuses a range outside the run, including on overflow.
   Host tests in Task 3, under Miri.
9. **Stale prose.** Every "`EIO` stays reserved for Phase 6", "nothing is submitted in 6.2", "blank
   disk" and "no boot exercises this arm" claim. Task 10's grep.

---

## Tasks

### Task 0: Measure what can be measured first (B11)

- Two default boots at the merge base, 300 s each, logs kept in `$S`.
- **Budget baseline:** the last required marker's byte fraction in each.
- **Determinism control:** the ordered `[diag mfs]` and `[diag vfs]` lines of the two logs compared.
  Identical → `diff-backends.sh` keeps the spec's scope; otherwise narrow it to MFS's prologue and
  amend the spec.

No commit unless the spec is amended.

### Task 1: `server-rt::bdev` and `BDEV_MINOR_ROOT` (B5)

**Files:** `kernel-shared/src/callnr.rs`, `server-rt/src/{bdev.rs,lib.rs}`,
`drivers/memory/src/{bdev.rs,main.rs}`, `tools/gen-c-headers` if it enumerates the constant.

**Proof:** the moved tests pass unchanged in `server-rt`; `memory`'s geometry tests still pass; the
ramdisk boot's `bdev.deny ok n=10` is unmoved.

### Task 2: the disk image and the runner (B9)

**Files:** `kernel/build.rs`, `tools/qemu-run.sh`.

**Proof:** `cmp` of `rootfs.img` against the archive's `rootfs` blob length and header; a
`MINIXRS_ROOT=disk` boot prints `capacity=2048`; a `MINIXRS_ROOT=ramdisk` boot prints `[diag vblk]
no device`. The marker files are not yet split, so the common file's `capacity=2560` line is updated
here and the no-disk boot is judged by grep.

### Task 3: `driver-rt` — DMA handles, queue over `NonNull`, queue fault, notify barrier (B8)

**Files:** `drivers/driver-rt/src/{dma.rs,queue.rs,mmio.rs,transport.rs,lib.rs}`,
`drivers/virtio-blk/src/{main.rs,bringup.rs}` (call sites only), `.github/workflows/ci.yml` only if
the Miri `-p` list needs nothing new (it should not).

**Proof:** new host tests for `DmaRegion::buf` bounds and overflow, typed `add`, and each
`QueueFault`; every existing queue test still passes; `cargo miri test -p minixrs-driver-rt` clean;
the disk boot still prints `virtio ok`.

### Task 4: virtio-blk's pure half (B4, B6, B7)

**Files:** `drivers/virtio-blk/src/blk.rs` (new), `bringup.rs`.

Sector arithmetic, request-header encoding, status → errno, the write plan (whole / partial /
empty), the root-check decisions, device blocks from capacity.

**Proof:** host tests, written first, one per row of the spec's host-test table that names
`virtio-blk`.

### Task 5: virtio-blk serves (B1, B3, B6, B7)

**Files:** `drivers/virtio-blk/src/main.rs`.

`init_fresh` publish; bring-up keeps its queue and line; `serve()`; the root check; the BDEV arms;
the broken-device state; deletion of `irq-extra`.

**Proof:** disk boot prints `root ok blocks=256 tail=1`. **Measure here (B11.3, B11.4):** which
`deliver_irq` arm completions take (temporary trace, removed before commit), and that `SYS_SAFECOPY`
into the DMA window succeeds — the second is exercised for real in Task 6.

### Task 6: MFS selects its device (B2)

**Files:** `fs/mfs/src/main.rs`; a small decision function in the library half with host tests.

**Proof:** disk boot prints `bdev.ds ok name=vblk` and the whole MFS battery; ramdisk boot prints
`name=memory` and the same battery.

### Task 7: marker files, the checker, the diff, CI (B10, B11.1)

**Files:** `tests/qemu-boot.*`, `tools/check-boot-log.sh`, `tools/diff-backends.sh` (new),
`.github/workflows/ci.yml`.

**Proof:** both boots PASS with their backend; each FAILs when checked as the other;
`diff-backends.sh` passes across them and fails on a doctored log. **Measure here:** the disk boot's
last-marker fraction against Task 0's baseline; set the per-boot timeout, `timeout-minutes` and the
local timeout from the ratio, and record the numbers.

### Task 8: refusal boots (spec §3)

Blank disk and `readonly=on`, each run once, observed, recorded in the phase file. **Measure B11.4's
second half** on the read-only boot.

### Task 9: mutations (spec §3)

The nine rows, each applied, observed and reverted; the table in the spec rewritten to say what was
observed.

### Task 10: documentation, trackers, the whole-branch sweep (spec §4)

`book/`, `docs/conventions/`, the phase file (H5 rewritten, debts 4–8, the box), `docs/plan.md`; the
stale-claim grep; every touched `book/` chapter read end to end; all local gates of
[`ci.md`](../../conventions/ci.md); then the fresh whole-branch review against the diff.

---

## Ledger

| When   | Decision                                                                                                                                         | If wrong                                                                                                                              |
| ------ | ------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------- |
| plan   | Spec B11 said every measurement precedes the code; three of the four need a serving driver. The spec's paragraph was rewritten.                  | none — an ordering claim, not a design choice                                                                                         |
| plan   | Task 2 (image, runner) precedes the driver tasks so each has a disk to boot against.                                                             | none                                                                                                                                  |
| Task 0 | Baseline (merge base, 300 s, two boots): PASS 148 both; last required marker at 48.1% and 55.7% of the log; final IPC counter 32.3 M and 31.9 M. | —                                                                                                                                     |
| Task 0 | Determinism control failed on the raw stream and passed per server by first occurrence; spec B10 and B11.2 rewritten to that scope.              | a backend-dependent *ordering* or *repeat count* inside one server's stream would go unseen; the marker sets still require every line |

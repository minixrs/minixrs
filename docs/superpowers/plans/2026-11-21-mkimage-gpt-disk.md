# Slice 6.4 — `tools/mkimage`: one GPT disk, partitions as minors — Implementation Plan

**Goal:** the machine boots from one GPT virtio disk; `virtio-blk` serves the GUID-selected root
partition as minor 0, flushes after every write, and a write survives into a second boot.

**Spec:**
[`../specs/2026-11-21-mkimage-gpt-disk-design.md`](../specs/2026-11-21-mkimage-gpt-disk-design.md),
decisions `P1…P11`, under the phase decisions `H1…H12` of
[`../../plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md). Where this plan and the spec
disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **Branch `feature/6.4-mkimage`.** Every commit `--signoff`ed and GPG-signed; never
  `--no-gpg-sign`, never `--no-verify`. **No push, no PR, no CI trigger.**
- **SPDX header** first in every new `.rs` file.
- **No ABI value moves** (D8). The root partition type GUID is additive.
- **No kernel behaviour change**; no `#[cfg(test)]` under `kernel/src/`. **MFS's FS-band handlers
  and VFS are not edited.**
- **No polling** (I12): the driver blocks in `receive` for every completion, the flush's included.
- **Overflow-safe arithmetic** in every LBA, block, extent and layout computation.
- **No new `unsafe`** in `driver-rt::gpt`, `virtio-blk`'s pure modules or `mkimage`. `cargo miri
  test -p minixrs-driver-rt` after touching `driver-rt`.
- **No third-party crate** in `mkimage` (P2).
- **Boots.** `$S` is the session scratch directory. Build first (`cargo build -p minixrs-kernel
  --target aarch64-unknown-none --release`), then `MINIXRS_ROOT=<disk|ramdisk> timeout 300 cargo run
  -p minixrs-kernel --target aarch64-unknown-none --release > $S/<name>.log 2>&1`. One boot at a
  time. Read `check-boot-log.sh`'s whole output; apply the starved-boot check before believing a
  missing marker.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing).
- **Counts in markers come from constants**, never typed into a driver.
- **Markdown** wraps at 100; `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.

## Layout numbers (fixed here)

512-byte sectors. ESP: LBA 2048, 69 632 sectors (34 MiB). Root: LBA 71 680, 2048 sectors. Disk: 75
776 sectors (37 MiB) for **both** images — the rootless one leaves the root's range unused — so the
capacity marker is the same on both boots. Backup entries at LBA 75 743–75 774, backup header at
75 775. The tool derives every one of these from constants and asserts the FAT32 cluster bound.

## Marker text (fixed here)

```text
[diag vblk] virtio ok features=0x100000200 capacity=75776 qsize=8 slot=0
[diag vblk] root ok blocks=256 tail=1 part=2 lba=71680
[diag vblk] root FAIL <geometry capacity=…|gpt <why>|gpt ambiguous|no root partition|align|readonly|label|blocks hdr=… dev=…|tail label|head rc=…|tail rc=…>
[diag vblk] flush ok n=1
[diag vblk] flush off
[diag vblk] flush FAIL status=<s>
[diag mfs] bdev.minor ok minor=1 mbr=1
[diag mfs] bdev.minor absent minor=1 rc=-6
[diag mfs] bdev.minor FAIL <read rc=…|mbr>
minix.rs init: persist gen=<n>
minix.rs init: persist FAIL <step>
```

`features=` is the measured word (P11.4) and is corrected here if QEMU offers otherwise.

## Review Focus

1. **GPT header refusals** — each check in P5 has a host test that flips exactly that field and
   recomputes the CRC, so the refusal is for the stated reason and not for a stale CRC.
2. **CRC-32** — checked against the published `123456789` → `0xCBF43926` vector in both crates,
   computed by hand-independent code paths (writer and parser share nothing).
3. **Extent arithmetic** — partition LBA → block (`/ 8`, exact), `block + first_block` by
   `checked_add` *after* the range check; a host test with a request at the extent's last block and
   one past it, and an extent ending at `u64::MAX`.
4. **Minor table** — minor 0 and `1 + root index` name the same extent; an unaligned partition has
   no minor; no root → every minor `ENXIO`, minor 1 included.
5. **FAT32** — cluster count ≥ 65 525 asserted; FAT entry 0/1 reserved values; directory entries'
   long-name checksum; the read-back test walks the FAT chain of a multi-cluster file (the kernel).
6. **Flush** — issued only after a device write that succeeded; a failed flush turns the reply into
   `EIO`; not issued for a zero-length write or a refused grant.
7. **Refusal order is unchanged** for a well-formed minor-0 request on both drivers
   (`diff-backends.sh`).
8. **The reboot attaches the image as-is** — `MINIXRS_VBLK_IMAGE` must not rebuild or copy.
9. **Stale claims** — `fat:rw:`, `capacity=2048`, "nothing persists", `no device` in the ramdisk
   commentary, "only `BDEV_MINOR_ROOT` exists".

## Tasks

### Task 0: Measure first (P11.1, P11.2, P11.5)

Done before the plan was written; results in the ledger.

### Task 1: the type GUID and the split range check (P1, P6)

**Files:** `kernel-shared/src/rootfs.rs`, `server-rt/src/bdev.rs`.

- `ROOT_PARTITION_TYPE_GUID`: the 16 on-disk bytes (GPT's mixed-endian encoding of
  `10FDAC43-2E55-4E0E-8D18-2D46479E1B44`), with a test pinning the byte order.
- `bdev::validate_range(req, blocks)`: `validate_read` minus the minor check; `validate_read` calls
  it. Existing tests unchanged; new tests for the range half alone.

**Proves it:** `cargo test -p minixrs-kernel-shared -p minixrs-server-rt`.

### Task 2: `driver-rt::gpt` (P5)

**Files:** `drivers/driver-rt/src/gpt.rs`, `lib.rs`.

The parser as P5 specifies, with its own CRC-32. Tests build headers and entry arrays by hand in the
test module (not with `mkimage`).

**Proves it:** `cargo test -p minixrs-driver-rt`, `cargo miri test -p minixrs-driver-rt`, clippy.

### Task 3: `tools/mkimage` (P1, P2)

**Files:** `tools/mkimage/{Cargo.toml,src/lib.rs,src/main.rs,src/gpt.rs,src/fat32.rs,src/crc.rs}`,
workspace `Cargo.toml`.

Library + thin binary; CLI per P2. Dev-dependency on `minixrs-driver-rt` for the round-trip test.

**Proves it:** `cargo test -p minixrs-mkimage`; `cargo deny check`; an image built by hand from the
real kernel and Limine boots to the kernel banner under the **old** driver (P11.6) — recorded.

### Task 4: virtio-blk's pure half (P6, P7, P8)

**Files:** `drivers/virtio-blk/src/{blk.rs,bringup.rs}`, a new `part.rs`.

Minor → extent table built from parsed GPT entries; the verdict enum grows P7's arms; flush header
encoding; `VBLK_WANTED_FEATURES` = `F_FLUSH | F_RO` and the accepted-word decisions.

**Proves it:** `cargo test -p minixrs-virtio-blk`.

### Task 5: virtio-blk serves partitions and flushes (P5–P8)

**Files:** `drivers/virtio-blk/src/main.rs`.

Read blocks 0–4, parse, build the table, run the root check on minor 0's extent, serve every minor,
flush after writes, decline a read-only disk.

**Proves it:** with the old runner the disk boot reads `root FAIL gpt signature` and falls back
(observed and recorded — the raw image is no longer a root); the ramdisk boot still passes.

### Task 6: the runner, the two images, the marker files (P3, P4)

**Files:** `tools/qemu-run.sh`, `tests/qemu-boot.{disk,ramdisk}.{expected,forbidden}`,
`tools/diff-backends.sh`.

**Proves it:** both boots pass `check-boot-log.sh`; `diff-backends.sh` passes with the one allowed
fallback line, and its self-test covers the allowance. P11.4 recorded.

### Task 7: the minor-1 probe (P6)

**Files:** `fs/mfs/src/main.rs` (prologue only), marker files, `tools/diff-backends.sh`.

**Proves it:** `bdev.minor ok minor=1 mbr=1` on the disk boot, `absent` on the ramdisk boot.

### Task 8: persistence (P9)

**Files:** `userland/init/src/main.rs`, `kernel/build.rs` (manifest), `kernel-shared/src/rootfs.rs`
(path and any geometry constant), `tools/check-boot-log.sh`,
`tests/qemu-boot.reboot.{expected,forbidden}`, `tests/qemu-boot.expected`.

**Proves it:** disk boot `persist gen=0`; reboot with `MINIXRS_VBLK_IMAGE=target/disk.img` prints
`gen=1` and passes `check-boot-log.sh reboot`; ramdisk boot `gen=0`.

### Task 9: CI and the budget (P10, P11.3)

**Files:** `.github/workflows/ci.yml`.

Measure both full boots against the merge base; set the reboot's timeout and the job's
`timeout-minutes` from the numbers.

### Task 10: recorded boots (spec §3)

`nodisk`, `readonly=on`, the raw image, a damaged entry-array CRC.

### Task 11: mutations (spec §3)

All eleven, stub-free where the marker allows.

### Task 12: documentation, trackers, the whole-branch sweep (spec §4)

Book, conventions, phase file, `docs/plan.md`, the stale-claim grep, the fresh whole-branch review.

## Ledger

- **Task 0 (2026-11-21).** P11.1: edk2 boots from a modern virtio-mmio disk — see the spec. P11.2: a
  nested `cargo build` inside the cargo runner completes in about a second; no lock wait, so the
  runner uses the shared target directory. P11.5: `init`'s first line lands at 21.6% of a 45 s
  default boot (about 10 s in); the reboot's timeout is set in Task 9 from the probe's own position.

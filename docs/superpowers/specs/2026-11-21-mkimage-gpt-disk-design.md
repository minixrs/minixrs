# Slice 6.4 — `tools/mkimage`: one GPT disk, partitions as minors

**Date:** 2026-11-21 **Status:** approved 2026-11-21 **Branch:** `feature/6.4-mkimage`
**Predecessor:** slice 6.3 (`virtio-blk`, PR #73) **Tracker:**
[`docs/plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md) slice 6.4

Slice 6.4 makes the machine boot from its disk. A host tool writes one GPT image holding a FAT32 ESP
(Limine, its config, the kernel) and the MinixFS root; the firmware and `virtio-blk` read the same
virtio-mmio device; the driver finds the root partition by its type GUID and serves it as minor 0;
every `BDEV_WRITE` is followed by a flush; and a write to the root is shown to survive into a second
boot of the same image.

The phase decisions `H1…H12` are locked and are not re-decided here. Slice-local decisions are
labelled `P1…P11`. MFS's filesystem code and VFS do not change. The slice changes no kernel
behaviour and no ABI value (D8): one additive constant, the root partition's type GUID, lands in
`kernel-shared`.

---

## 1. What exists, and what 6.4 changes

| Component                  | Today                                                          | After 6.4                                                                                      |
| -------------------------- | -------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `tools/mkimage`            | does not exist                                                 | host crate `minixrs-mkimage`: GPT + FAT32 ESP + root partition, zero third-party crates (P2)   |
| `drivers/driver-rt`        | no partition knowledge                                         | `+ gpt.rs`: a pure, host-tested GPT parser (P5)                                                |
| `drivers/virtio-blk`       | minor 0 is the whole raw image; root check reads LBA 0         | minors per H6 (P6); root check on the GUID-selected partition (P7); flush and `F_RO` (P8)      |
| `server-rt::bdev`          | `validate_read` checks the minor and the range together        | the range check is callable without the minor check; `memory` keeps `validate_read` unchanged  |
| `kernel-shared`            | `rootfs` labels, `BDEV_MINOR_ROOT`                             | `+` the root partition type GUID                                                               |
| `userland/init`            | write battery only                                             | `+` one persistence probe before the battery (P9)                                              |
| `kernel/build.rs`          | writes `rootfs.img`; manifest has no `/etc/boots`              | manifest gains `/etc/boots`; `rootfs.img` is still written and is `mkimage`'s input            |
| `tools/qemu-run.sh`        | `fat:rw:` ESP on virtio-blk-pci; raw root image on virtio-mmio | builds `target/disk.img` with `mkimage` and attaches it alone, on `virtio-mmio-bus.0` (P3, P4) |
| `tools/check-boot-log.sh`  | backends `disk`, `ramdisk`                                     | `+ reboot`, which checks its own pair only (P9)                                                |
| `tools/diff-backends.sh`   | MFS streams differ in the `bdev.ds ok name=` line only         | also allows the ramdisk side's one `bdev.root vblk … fallback=memory` line (P4)                |
| `tests/`                   | six marker files                                               | disk and ramdisk pairs rewritten; `+ qemu-boot.reboot.{expected,forbidden}`                    |
| `.github/workflows/ci.yml` | `qemu-smoke` boots twice                                       | boots three times: disk, a short reboot of the same image, ramdisk (P10)                       |
| `fs/mfs`, `servers/vfs`    | —                                                              | unchanged                                                                                      |

---

## 2. Decisions

### P1 — the image layout

512-byte sectors. In order:

| LBA                 | Holds                                                                               |
| ------------------- | ----------------------------------------------------------------------------------- |
| 0                   | protective MBR (one `0xEE` entry covering the disk)                                 |
| 1                   | primary GPT header                                                                  |
| 2–33                | 128 partition entries of 128 bytes                                                  |
| 2048 (1 MiB)        | partition 1: the ESP, FAT32, one sector per cluster                                 |
| next 1 MiB boundary | partition 2: the root, exactly `ROOTFS_IMAGE_BYTES` (omitted from a rootless image) |
| last 33 sectors     | backup entries and backup header                                                    |

The ESP is sized so its cluster count is at least 65 525 — below that a FAT driver reads the volume
as FAT16 whatever the boot sector says — which at one sector per cluster is about 34 MiB; a const
assertion in the tool holds the bound. The whole image is about 37 MiB, padded so its sector count
is a multiple of 8, and written sparsely.

Both partitions start on a 1 MiB boundary, so each starts on a 4096-byte block boundary, which P6
requires of anything served as a minor.

**The image is reproducible.** The disk GUID and the two partitions' unique GUIDs are fixed
constants in the tool, FAT timestamps are zero and the volume serial is fixed, so the same inputs
give the same bytes. The root partition's **type** GUID is `10FDAC43-2E55-4E0E-8D18-2D46479E1B44`,
generated for this slice, defined once in `kernel-shared` and used by the tool and the driver.

*Rejected:* random GUIDs per build (an image that differs with no input change cannot be compared
across runs); a FAT16 ESP of a few MiB (H6 says FAT32, and the UEFI specification asks for it).

### P2 — `tools/mkimage` is hand-written, with no third-party crate

`minixrs-mkimage` is a workspace host crate with a library and a thin binary, in `mkfs-mfs`'s shape:

```text
mkimage <out.img> [--root <rootfs.img>] <esp-path>=<host-file>...
```

It writes the protective MBR, both GPT copies with their CRC-32s, and a FAT32 volume — boot sector,
FSInfo, backup boot sector, two FATs, directories with long-name entries (`limine.conf` does not fit
8.3) — and copies the root image into its partition byte for byte. It refuses a `--root` file that
is not `ROOTFS_IMAGE_BYTES` long or does not carry `IMAGE_LABEL`.

It depends on `kernel-shared` only. It takes the root image as a **file**, the `rootfs.img`
`kernel/build.rs` already writes, rather than calling the `mkfs-mfs` library again: the bytes on the
disk are then the bytes in the boot archive by construction, which is what the two-backend
comparison rests on. H6's "produced by the existing `mkfs-mfs` library" still holds, one step
removed.

The writer shares no code with `driver-rt`'s parser (P5). A host test in the tool builds an image
and parses it with `driver-rt` as a dev-dependency, so encoder and decoder are derived separately
and checked against each other; edk2 reading the same GPT to find the ESP is a third, independent
reader, observed on every boot.

*Rejected:* the `fatfs` and `gpt` crates — H6 permits them, but each brings a dependency tree to a
tool whose whole output format is a few hundred lines, and `mkfs-mfs` set the precedent of none.
Also rejected: `hdiutil` / `mtools` / `parted` (H6).

### P3 — the runner builds the image on every run

`qemu-run.sh` runs `mkimage` (through `cargo run -q --release -p minixrs-mkimage`) with the kernel
ELF it was handed, Limine's `BOOTAA64.EFI`, `tools/limine.conf` and — for a disk boot —
`rootfs.img`, writing `target/disk.img`. It is rebuilt on every run, as 6.3's copy was: the boot
writes to its root, and a reused image would make the second run a different test.

`MINIXRS_VBLK_IMAGE=<file>` still attaches a file as-is with no build, which is now also how the
reboot (P9) boots the image the previous run left behind. `MINIXRS_VBLK_DRIVE_OPTS` is unchanged.

Whether a nested `cargo run` is safe under the `cargo run` that invoked the runner is measured
before anything is built on it (P11.2). If it deadlocks on the build-directory lock, the tool is
built into its own `--target-dir` instead.

### P4 — two images, and the ramdisk boot becomes the fallback boot

There is no `fat:rw:` drive in any mode, and no virtio-blk-pci device. The one drive is
`virtio-blk-device` on `virtio-mmio-bus.0`.

- `MINIXRS_ROOT=disk` (default): ESP and root partition. MFS mounts through `vblk`.
- `MINIXRS_ROOT=ramdisk`: the **same image without the root partition**. The firmware still needs
  somewhere to read the kernel from, so a disk is attached; `vblk` brings it up, finds no partition
  of the root type, prints `root FAIL no root partition`, and MFS prints `bdev.root vblk rc=-6
  fallback=memory` and mounts the ramdisk.

This pays 6.3's debt 7: the no-root fallback — the driver's `ENXIO` gate, MFS's revoke-and-regrant,
the fallback line — gets a standing CI boot. The cost is that the **no-device** state (`[diag vblk]
no device`, a DS miss in MFS) loses its standing boot, because a machine with no disk at all has
nothing to boot from. It is the smaller path — one `ENOENT` branch and a lookup miss — and stays
reachable by hand with `MINIXRS_ROOT=nodisk`, which attaches the rootless image as virtio-blk-pci
(the firmware reads it, minix.rs does not see it). That boot is run once in this slice and recorded.

The ramdisk marker pair changes accordingly: it requires the `root FAIL no root partition` and
fallback lines and `bdev.ds ok name=memory`, and no longer requires `no device`. `diff-backends.sh`
allows exactly that one extra MFS line on the ramdisk side.

*Rejected:* keeping `fat:rw:` for the ramdisk boot (two ways to load a kernel, and the fallback
still unbooted); a third full CI boot for the no-device state (ten more minutes per run for the
smallest of the three paths).

### P5 — the GPT parser

`driver-rt::gpt` is `no_std`, free of I/O and of `unsafe`, and works on byte slices the driver read:

- `parse_header(sector, disk_sectors)` checks the signature, revision 1.0, a header size of at least
  92 and at most one sector, the header CRC-32, `MyLBA == 1`, an entry size of 128, at most 128
  entries, the entry array at LBA 2, and `FirstUsableLBA ≤ LastUsableLBA < disk_sectors`.
- `check_entries(bytes, header)` checks the entry array's CRC-32.
- `partition(bytes, n)` decodes entry *n*; an all-zero type GUID is an unused slot. An entry whose
  range is inverted or leaves the usable area is an error for the whole table.
- `find_type(bytes, header, guid)` returns the matching partition, `None`, or `Ambiguous` when more
  than one entry carries the type.

All arithmetic is `checked_*`. Only the **primary** header is consulted: a disk whose primary is
damaged has no root as far as this driver is concerned, and MFS falls back. Recovering from the
backup means deciding which copy to believe and writing the repair, which is a tool's job.

Two partitions of the root type is `root FAIL gpt ambiguous`, not "the first one": H6 selects by
type so that position does not matter, and picking by position on a tie brings it back.

The driver reads the header and all 128 entries as blocks 0–4 of the whole disk (LBA 0–39) through
the same `serve` path as every other request, into a static buffer.

### P6 — minors

After a successful bring-up the driver holds a table of extents, each a first block and a block
count on the whole disk:

| Minor   | Extent                                                 |
| ------- | ------------------------------------------------------ |
| 0       | the partition `find_type` selected — `BDEV_MINOR_ROOT` |
| 1       | the whole disk                                         |
| `1 + n` | GPT partition *n*, counted from 1, for each used entry |

So on the image P1 describes minor 2 is the ESP and minors 0 and 3 are the same blocks. A partition
whose start or length is not a whole number of 4096-byte blocks has no minor (`ENXIO`); if it is the
root partition, the verdict is `root FAIL align`. A disk whose capacity is not a multiple of 8
sectors fails as it does today (`root FAIL geometry`).

Reads and writes are served alike on every minor; the request path is the one 6.3 built with the
extent's first block added, by `checked_add`, after the range check. A driver with a device but **no
root answers `ENXIO` on every minor**, as H5 says — the whole disk included.

`server-rt::bdev` exposes the range half of `validate_read` on its own, taking the extent's block
count. `virtio-blk` resolves the minor first (`ENXIO`), then calls it, which keeps the refusal order
both drivers share. `memory` calls `validate_read` as before and is not edited.

Nothing in the system opens a minor other than 0. So that the table is not unexercised code, MFS's
prologue gains one probe beside `bdev.partial`: it reads block 0 of minor 1 and reports whether the
protective MBR's `55 AA` signature is there. `vblk` answers it; `memory` answers `ENXIO`, which has
always been its reply for a minor it does not have. The two outcomes are one line each in the
per-backend marker files and the second line `diff-backends.sh` treats as backend-specific.

*Rejected:* read-only non-root minors (a second rule in the request path, for a client that does not
exist); serving minor 1 when there is no root (contradicts H5's text for no consumer).

### P7 — the root check moves onto the partition

The check is 6.3's, unchanged in what it decides — `IMAGE_LABEL`, the header's block count equal to
the extent's, `IMAGE_TAIL_LABEL` — and run against minor 0's extent rather than the device's. Its
line gains where the root is:

```text
[diag vblk] root ok blocks=256 tail=1 part=2 lba=<first sector>
```

The `lba=` value is the proof H6 asks for that the root's first block is not LBA 0, and the disk
marker file requires it literally. The verdicts, in the order they can occur: `root FAIL geometry`,
`root FAIL gpt <why>`, `root FAIL gpt ambiguous`, `root FAIL no root partition`, `root FAIL align`,
`root FAIL readonly` (P8), then 6.3's `label` / `blocks` / `tail label`.

**A raw, whole-disk MinixFS image is no longer a root.** 6.3's `rootfs.img` attached directly now
reads `root FAIL gpt signature` and falls back. That is H5 as amended: the check runs against minor
0 as H6 defines it.

### P8 — flush, and a read-only disk

The driver requests `VIRTIO_BLK_F_FLUSH` (bit 9) and `VIRTIO_BLK_F_RO` (bit 5). With QEMU's raw
drive the accepted word is expected to be `0x100000200`, which becomes the marker's exact text
(measured, P11.4).

- **Flush.** When the feature was accepted, every `BDEV_WRITE` that reached the device is followed
  by a `VIRTIO_BLK_T_FLUSH` — a two-descriptor chain, header and status — before the reply. A flush
  that fails makes the write's reply `EIO`. The first successful flush prints `[diag vblk] flush ok
  n=1`; every failure prints `flush FAIL status=<s>`. When the device does not offer the feature the
  driver prints `flush off` once and serves writes without it.
- **Read-only.** When the device offers `F_RO` the driver accepts it and declines the disk as a
  root: `root FAIL readonly`, and MFS falls back. This pays 6.3's debt 1. Mounting such a disk
  read-only would need a read-only mount in MFS and VFS, which nothing in Phase 6 asks for.

Requesting an optional feature for the first time is also where 6.2's debt 9 was sent. This slice
adds the mutation that omits `VERSION_1` from the accepted word and records what moves; it does not
add a new guard for a register page one off, which `DeviceID` already refuses.

### P9 — the persistence proof

The root image gains `/etc/boots`, one byte, `0`. Before its write battery `init` reads it, prints

```text
minix.rs init: persist gen=<n>
```

and rewrites the file with `n + 1` (open with `O_TRUNC`, write, close — paths the battery already
uses, so no new request).

- A fresh disk boot and every ramdisk boot print `gen=0`; that line is in the common expected set.
- A **reboot** — `MINIXRS_VBLK_IMAGE=target/disk.img`, the image the disk boot just wrote, attached
  with no rebuild — must print `gen=1`. The ramdisk could never show this.

The reboot is checked against its own marker pair only (`check-boot-log.sh reboot`): `persist
gen=1`, the `root ok … lba=` line and `bdev.ds ok name=vblk` required; `persist gen=0` forbidden.
The common set is not applied, because the write battery was designed against a fresh image and its
behaviour on its own leftovers is not something this slice defines. The reboot therefore runs only
until the probe's line, under a short timeout fixed by measurement (P11.5).

As the phase file says, this shows that writes reach the device and nothing about the flush. The
flush is evidenced by `flush ok n=1` and by a mutation recorded as moving nothing else.

### P10 — CI

`qemu-smoke` keeps its name and runs, in order: the disk boot (full check), the reboot of that image
(short), the ramdisk boot (full check), then `diff-backends.sh`. The job's `timeout-minutes` is
re-derived from the measured budget (P11.3). `mkimage`'s host tests run in the existing `cargo test`
job, and `cargo deny` sees no new third-party crate.

### P11 — measure first

1. **edk2 boots from a modern virtio-mmio disk** (the phase file's unverified fact 2). **Measured
   2026-11-21, QEMU 11.0.3:** with the ESP attached only as `virtio-blk-device` on
   `virtio-mmio-bus.0` and `force-legacy=false`, the firmware loads Limine from
   `VenHw(…,0000000A00000000)`, the kernel boots, and `vblk` then brings the same device up (`virtio
   ok features=0x100000000 … slot=0`). No firmware-only attachment is needed, and the driver's reset
   recovers the device from whatever state the firmware left it in.
2. **A nested `cargo run` from the runner** completes rather than waiting on the build lock.
3. **Budget.** The last required marker's position in a fixed-timeout log for both full boots,
   against the merge base — each write now costs a second interrupt round trip, and the ramdisk boot
   now brings a device up.
4. **What QEMU offers**: the feature word with a raw drive, and with `readonly=on`.
5. **Where `persist gen=` lands** in a default boot, which fixes the reboot's timeout.
6. **edk2 and Limine read this tool's FAT32 and GPT.** Item 1 used QEMU's own FAT; the first image
   `mkimage` writes is the first test of ours.

Item 1 is done. Items 2 and 5 are Task 0 of the plan; the others belong to the task that first
produces what they measure.

---

## 3. Verification

**Host tests:**

| Behaviour                                                                          | Where        |
| ---------------------------------------------------------------------------------- | ------------ |
| CRC-32 against published vectors                                                   | both crates  |
| header refusals, one per check in P5; entry CRC; inverted and out-of-range entries | `driver-rt`  |
| `find_type`: found, absent, ambiguous, unused slots skipped                        | `driver-rt`  |
| minor → extent, including an unaligned partition and overflow                      | `virtio-blk` |
| flush header encoding; feature word → flush on/off, read-only                      | `virtio-blk` |
| the range check without the minor check; `validate_read` unchanged                 | `server-rt`  |
| image layout: alignment, sizes, both GPT copies, the cluster-count bound           | `mkimage`    |
| FAT32: every file read back by walking the written FATs and directories            | `mkimage`    |
| the written image parsed by `driver-rt::gpt`; root bytes equal the input           | `mkimage`    |
| the same inputs give the same bytes; a bad `--root` file is refused                | `mkimage`    |

Miri keeps running over `driver-rt`.

**Boots.** The disk and ramdisk boots pass `check-boot-log.sh` with their backend, the reboot passes
its own pair, and `diff-backends.sh` passes across the two full boots. The disk boot's banner is
itself the proof that the firmware read the image, since the runner attaches nothing else;
`/bin/hello` exec'd from the root is already in the common set.

**Boots run once and recorded in the phase file:**

| Boot                                      | Expected                                                           |
| ----------------------------------------- | ------------------------------------------------------------------ |
| `MINIXRS_ROOT=nodisk`                     | `[diag vblk] no device`; ramdisk root; common set passes           |
| the disk image attached `readonly=on`     | `root FAIL readonly`; fallback; image unchanged afterwards         |
| 6.3's raw `rootfs.img` as the disk        | no kernel at all — nothing to boot from; recorded as such          |
| an image whose entry-array CRC is damaged | to observe: whether edk2 still boots it; if so `root FAIL gpt crc` |

**Mutations**, each applied, observed and reverted:

| #  | Mutation                                               | Predicted                                                        |
| -- | ------------------------------------------------------ | ---------------------------------------------------------------- |
| 1  | minor 0 served from disk block 0 (extent base dropped) | `root FAIL label` on the disk boot                               |
| 2  | select the root by index 1, not by type                | `root FAIL label` (the ESP is not a root)                        |
| 3  | skip the header CRC check                              | host tests only; the damaged-image boot if it reaches the kernel |
| 4  | drop the no-root `ENXIO` gate (6.3's mutation 5)       | ramdisk boot: `mount FAIL`, now in CI rather than by hand        |
| 5  | accept a read-only disk as a root                      | `readonly=on` boot: mounts, writes fail `EIO`                    |
| 6  | drop the flush after a write                           | `flush ok n=1` missing; nothing else, including `gen=1`          |
| 7  | treat a failed flush as success                        | host test only — QEMU's flush does not fail                      |
| 8  | omit `VERSION_1` from the accepted word                | the exact `features=` marker text and two host tests             |
| 9  | `init` skips the rewrite of `/etc/boots`               | reboot: `gen=0`, which its forbidden file catches                |
| 10 | `mkimage` writes the root partition one sector late    | `mkimage` host test; `root FAIL align` if booted                 |
| 11 | minor 1 answered from the root extent                  | the minor-1 probe reports no MBR signature                       |

Rows 6 and 7 are expected to show how little a boot can say about the flush, and are recorded
whichever way they come out.

---

## 4. Documentation owed in the same PR

- **`book/`**: the virtio-blk chapter (GPT, minors, the root verdicts, flush, read-only); a section
  on the disk image and `mkimage`; the build-and-boot pages (one image, the three `MINIXRS_ROOT`
  values, the reboot). Each touched chapter re-read end to end.
- **`docs/conventions/`**: `build-and-boot.md` (the runner's command line, `target/disk.img`, the
  reboot recipe); `ci.md` (three boots, the measured budget); `testing-and-markers.md` (the reboot
  pair, eight marker files); `servers-and-drivers.md` (minors, the split validation, the
  flush-before-reply rule).
- **`docs/plans/phase-6-virtio.md`**: unverified fact 2 marked measured; 6.2's debt 9 and 6.3's
  debts 1 and 7 marked as this slice leaves them; 6.4's measurements, recorded boots and mutation
  table; the box checked. `docs/plan.md`: the box.
- **In-tree prose this slice falsifies**: every `fat:rw:` and virtio-blk-pci mention, "nothing
  persists across boots", `capacity=2048`, "only `BDEV_MINOR_ROOT` exists today",
  `VBLK_WANTED_FEATURES`'s "6.4 adds" comment, the `[diag vblk] no device` commentary in the ramdisk
  marker file. Found by grep in the last task, not from this list.

---

## 5. Rulings to check

| #  | Ruling                                                                                  | If wrong                                                                                      |
| -- | --------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| 1  | P4: the ramdisk CI boot becomes the rootless-disk fallback boot; no-device is hand-run  | a third full boot in CI (about ten minutes), or `fat:rw:` kept for one mode                   |
| 2  | P2: FAT32 and GPT writers are hand-written, no `fatfs` / `gpt` crate                    | swap the writer's internals for the crates; the CLI and the image layout do not move          |
| 3  | P2: `mkimage` takes `rootfs.img` as a file instead of calling the `mkfs-mfs` library    | one path dependency and a manifest argument added to the tool                                 |
| 4  | P1: fixed disk and partition GUIDs, zero timestamps                                     | two images attached to one machine would collide; nothing here attaches two                   |
| 5  | P5: primary GPT header only; a damaged primary means no root                            | a backup-header path in the parser and a decision about which copy wins                       |
| 6  | P5: two root-type partitions is a refusal, not first-wins                               | one arm changes; multi-root disks need a selector that does not exist                         |
| 7  | P6: every minor is read-write, and all answer `ENXIO` when there is no root             | a per-minor access rule in the request path; `EROFS` or `EACCES` joins the driver's errno set |
| 8  | P6: an MFS prologue probe of minor 1, a second backend-specific line in `diff-backends` | the minor table is covered by host tests alone                                                |
| 9  | P7: a raw whole-disk MinixFS image is no longer accepted as a root                      | a no-GPT arm in the root check, and two disk formats to keep booting                          |
| 10 | P8: a read-only disk is declined as a root rather than mounted read-only                | read-only mount support in MFS and VFS                                                        |
| 11 | P8: `flush ok n=1` once, not a running count                                            | a periodic line whose value varies with boot length and cannot be a marker                    |
| 12 | P9: the probe lives in `init` and seeds `/etc/boots` in the root image                  | the image's geometry-derived markers move once more if it is relocated                        |
| 13 | P9: the reboot is checked against its own pair, not the common set                      | the write battery must be made idempotent on a used image                                     |
| 14 | 6.3's debt 3 (`fs.trunc` cannot see a dropped truncate) is **not** paid here            | it stays owed; the persistence probe is beside the write battery, not a change to it          |

---

## 6. Out of scope

- Any change to MFS's FS-band handlers or to VFS; a read-only mount.
- GPT repair, the backup header, MBR-only disks, more than one disk.
- A mount of any minor other than 0, and device nodes for partitions.
- A queue deeper than one request; a request timeout (6.2 debt 1 stays owed).
- Proving the flush reaches stable storage, which a raw image under QEMU cannot show.
- Removing the ramdisk or the `rootfs` blob (H7).

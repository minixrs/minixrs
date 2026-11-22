# Slice 6.4 — `tools/mkimage`: one GPT disk, partitions as minors

**Date:** 2026-11-21 **Status:** approved 2026-11-21; built 2026-11-21 and 2026-11-22, with as-built
notes inline **Branch:** `feature/6.4-mkimage` **Predecessor:** slice 6.3 (`virtio-blk`, PR #73)
**Tracker:** [`docs/plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md) slice 6.4

Slice 6.4 makes the machine boot from its disk. A host tool writes one GPT image holding a FAT32 ESP
(Limine, its config, the kernel) and the MinixFS root; the firmware and `virtio-blk` read the same
virtio-mmio device; the driver finds the root partition by its type GUID and serves it as minor 0;
every `BDEV_WRITE` is followed by a flush; and a write to the root is shown to survive into a second
boot of the same image.

The phase decisions `H1…H12` are locked and are not re-decided here. Slice-local decisions are
labelled `P1…P11`. MFS's filesystem code and VFS do not change. The slice changes no kernel
behaviour and no ABI value (D8): the additions to `kernel-shared` — the root partition's type GUID,
`BDEV_MINOR_WHOLE_DISK` (P6) and the boot counter's path, contents and two helpers (P9) — move no
existing value, and none is emitted into the generated C headers.

Where what was built differs from what this document first said, the operative text has been
rewritten and the difference is marked *(As built: …)*. §3 keeps the predictions and adds what was
observed beside them; §5 keeps the fourteen rulings approved with the spec and adds the ones made
during the run and in the whole-branch review round that followed it (rows 31–36).

---

## 1. What exists, and what 6.4 changes

| Component                  | Today                                                          | After 6.4                                                                                     |
| -------------------------- | -------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `tools/mkimage`            | does not exist                                                 | host crate `minixrs-mkimage`: GPT + FAT32 ESP + root partition, zero third-party crates (P2)  |
| `drivers/driver-rt`        | no partition knowledge                                         | `+ gpt.rs`: a pure, host-tested GPT parser (P5)                                               |
| `drivers/virtio-blk`       | minor 0 is the whole raw image; root check reads LBA 0         | minors per H6 (P6); root check on the GUID-selected partition (P7); flush and `F_RO` (P8)     |
| `server-rt::bdev`          | `validate_read` checks the minor and the range together        | the range check is callable without the minor check; `memory` keeps `validate_read` unchanged |
| `kernel-shared`            | `rootfs` labels, `BDEV_MINOR_ROOT`                             | `+` the root partition type GUID, `BDEV_MINOR_WHOLE_DISK`, the `/etc/boots` constants (P9)    |
| ↳ review round             | denial probes name the literal minor 7                         | `+ BDEV_MINOR_NONE` (`i32::MAX`), the minor no block driver serves (ruling 31)                |
| `userland/init`            | write battery only                                             | `+` one persistence probe before the battery (P9)                                             |
| `kernel/build.rs`          | writes `rootfs.img`; manifest has no `/etc/boots`              | manifest gains `/etc/boots`; `rootfs.img` is still written and is `mkimage`'s input           |
| `tools/qemu-run.sh`        | `fat:rw:` ESP on virtio-blk-pci; raw root image on virtio-mmio | builds `target/disk.img` (or `disk-rootless.img`) with `mkimage`, attaches it alone (P3, P4)  |
| `tools/check-boot-log.sh`  | backends `disk`, `ramdisk`                                     | `+ reboot`, which checks its own pair only (P9); refuses a boot kind with no required marker  |
| `tools/diff-backends.sh`   | MFS streams differ in the `bdev.ds ok name=` line only         | also allows the ramdisk side's fallback line (P4) and each side's `bdev.minor` verdict (P6)   |
| `tests/`                   | six marker files                                               | eight: disk and ramdisk pairs rewritten; `+ qemu-boot.reboot.{expected,forbidden}`            |
| `.github/workflows/ci.yml` | `qemu-smoke` boots twice                                       | boots three times: disk, a short reboot of the same image, ramdisk (P10)                      |
| `fs/mfs`                   | prologue probes `bdev.tail`, `bdev.partial`, the denials       | `+` the `bdev.minor` probe and its library module `minor.rs` (P6); FS-band handlers unchanged |
| ↳ review round             | `bad-minor` / `wr-minor` aim at minor 7                        | both aim at `BDEV_MINOR_NONE`; the battery is still ten (`bdev.deny ok n=10`)                 |
| `servers/vfs`              | —                                                              | unchanged                                                                                     |

---

## 2. Decisions

### P1 — the image layout

512-byte sectors. In order:

| LBA                 | As built    | Holds                                                                               |
| ------------------- | ----------- | ----------------------------------------------------------------------------------- |
| 0                   | 0           | protective MBR (one `0xEE` entry covering the disk)                                 |
| 1                   | 1           | primary GPT header                                                                  |
| 2–33                | 2–33        | 128 partition entries of 128 bytes                                                  |
| 2048 (1 MiB)        | 2048–71679  | partition 1: the ESP, FAT32, one sector per cluster                                 |
| next 1 MiB boundary | 71680–73727 | partition 2: the root, exactly `ROOTFS_IMAGE_BYTES` (omitted from a rootless image) |
| last 33 sectors     | 75743–75775 | backup entries and backup header                                                    |

The ESP is sized so its cluster count is at least 65 525 — below that a FAT driver reads the volume
as FAT16 whatever the boot sector says; a const assertion in the tool holds the bound. (As built:
the ESP's size is a named input, 34 MiB, which gives 68 520 clusters; the FAT is sized by
Microsoft's formula, 540 sectors per copy, which over-provisions slightly.) The whole image is 37
MiB and written sparsely. (As built: the disk is rounded up to a whole mebibyte — 75 776 sectors, 38
797 312 bytes — not merely to a multiple of 8 sectors, and is the same size with and without the
root partition, so the `capacity=` marker is the same on both boots. Every number is derived in
`tools/mkimage/src/layout.rs` and held by const assertions.)

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

(As built: the library is `Spec { esp_files, root }`, `build_image(&Spec) -> Result<Vec<u8>, Error>`
and `write_sparse`, over four modules — `layout.rs`, `gpt.rs`, `fat32.rs`, `crc.rs`. The CLI
requires at least one ESP file. FAT details the text above does not fix: short aliases follow
Windows, with a numeric tail only when upper-casing loses something (`LIMINE~1.CON`); names are
ASCII only and case-insensitive duplicates are refused; the volume label is `MINIXRS ESP` and the
serial `0x6D780604`; a third boot-record sector is written and backed up; directories may span
clusters; the protective MBR's CHS fields are `FF FF FF`.)

It depends on `kernel-shared` only. It takes the root image as a **file**, the `rootfs.img`
`kernel/build.rs` already writes, rather than calling the `mkfs-mfs` library again: the bytes on the
disk are then the bytes in the boot archive by construction, which is what the two-backend
comparison rests on. H6's "produced by the existing `mkfs-mfs` library" still holds, one step
removed.

The writer shares no code with `driver-rt`'s parser (P5) — not even the CRC-32, which is
table-driven in the tool and bit-at-a-time in the driver. A host test in the tool builds an image
and parses it with `driver-rt` as a dev-dependency, so encoder and decoder are derived separately
and checked against each other; edk2 reading the same GPT to find the ESP is a third, independent
reader, observed on every boot. (As built: that test is an integration test,
`tools/mkimage/tests/image.rs`, which reaches only the crate's public API and carries its own FAT32
reader, so no check can borrow the writer's helpers.)

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
(As built: with `MINIXRS_VBLK_IMAGE` the machine boots entirely from that file, so the runner also
skips the Limine fetch and does not use the kernel ELF it was handed; it says so on stderr. The
images are `target/disk.img` and `target/disk-rootless.img` under the repository's own `target/` —
`CARGO_TARGET_DIR` is not honoured for them. The runner prints its QEMU argv on stderr as one
`qemu-run:` line, and validates the backend before it searches for firmware.)

A nested `cargo run` is safe under the `cargo run` that invoked the runner: measured (P11.2, with a
nested `cargo build`), it completes in about a second with no wait on the build lock, so the tool is
built in the shared target directory and the `--target-dir` alternative was not needed. The runner
unsets `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS` and `CARGO_BUILD_TARGET` for
the nested build, as `kernel/build.rs` does for its own.

### P4 — two images, and the ramdisk boot becomes the fallback boot

There is no `fat:rw:` drive in any mode, and no virtio-blk-pci device in either checked mode; the
hand-run `nodisk` mode below is the one exception. In the two checked modes the one drive is
`virtio-blk-device` on `virtio-mmio-bus.0`.

- `MINIXRS_ROOT=disk` (default): ESP and root partition. MFS mounts through `vblk`.
- `MINIXRS_ROOT=ramdisk`: the **same image without the root partition**. The firmware still needs
  somewhere to read the kernel from, so a disk is attached; `vblk` brings it up, finds no partition
  of the root type, prints `root FAIL no root partition`, and MFS prints `bdev.root vblk rc=-6
  fallback=memory` and mounts the ramdisk.

This pays 6.3's debt 7 **in part**. The no-root fallback — the driver's `ENXIO` gate, MFS's
revoke-and-regrant, the fallback line — gets a boot CI is configured to run. (As built and as
measured: the gate and the fallback line are covered — mutation 4 below, the ramdisk marker file,
and since the review round three host tests of `part::locate`. Two things the debt named are not: a
root check that accepts a bad label still passes both checked boots, because the rootless disk is
refused at `no root partition` before any label is read, and no mutation was run against MFS's
revoke. The phase file carries the remainder as 6.4's debt 10.) The cost is that the **no-device**
state (`[diag vblk] no device`, a DS miss in MFS) loses its standing boot, because a machine with no
disk at all has nothing to boot from. It is the smaller path — one `ENOENT` branch and a lookup miss
— and stays reachable by hand with `MINIXRS_ROOT=nodisk`, which attaches the rootless image as
virtio-blk-pci (the firmware reads it, minix.rs does not see it). That boot is run once in this
slice and recorded. (As built: `ramdisk` and `nodisk` both reject `MINIXRS_VBLK_IMAGE` and
`MINIXRS_VBLK_DRIVE_OPTS`.)

The ramdisk marker pair changes accordingly: it requires the `root FAIL no root partition` and
fallback lines and `bdev.ds ok name=memory`, and no longer requires `no device`. `diff-backends.sh`
allows exactly that one extra MFS line on the ramdisk side. (As built: the ramdisk pair also
requires the device's four bring-up lines, character for character the disk pair's, so the boot
cannot quietly turn back into a device-less one; it forbids `no device`, `root ok`, `flush ok` and
`bdev.minor ok`. `flush off` and `flush FAIL` are forbidden in the **common** file. The diff
*allows* the fallback line — at most once, that exact text — and does not require it; requiring it
is the marker file's job.)

*Rejected:* keeping `fat:rw:` for the ramdisk boot (two ways to load a kernel, and the fallback
still unbooted); a third full CI boot for the no-device state (fifteen more minutes per run, a full
boot being 900 s since P10, for the smallest of the three paths).

### P5 — the GPT parser

`driver-rt::gpt` is `no_std`, free of I/O and of `unsafe`, and works on byte slices the driver read:

- `parse_header(sector, disk_sectors)` checks, in this order, the signature, revision 1.0, a header
  size of at least 92 and at most one sector, the header CRC-32, `MyLBA == 1`, an entry size of 128,
  between 1 and 128 entries, the entry array at LBA 2, and `FirstUsableLBA ≤ LastUsableLBA <
  disk_sectors`.
- `check_entries(bytes, header)` checks the entry array's CRC-32.
- `partition(bytes, header, n)` decodes entry *n*, **counted from 1** — a slot number, so minor `1 +
  n` is partition *n*; an all-zero type GUID is an unused slot. An entry whose range is inverted or
  leaves the usable area is an error for the whole table.
- `find_type(bytes, header, guid)` returns `Found { index, partition }`, `Absent`, or `Ambiguous`
  when more than one entry carries the type.

(As built: fourteen refusals, each with a stable word for the marker — `header short`, `signature`,
`revision`, `header size`, `crc`, `my lba`, `entry size`, `entry count`, `entry lba`, `usable
range`, `entries crc`, `entries short`, `entry range`, `entry index`. Two are beyond the list above:
`header short`, for a slice shorter than a sector, and `entry index`, for a partition number outside
the table, which is a caller's mistake. `entry range` covers both the inverted and the
out-of-usable-range entry. **Not checked:** that `FirstUsableLBA` lies past the entry array, and
that partitions do not overlap — neither is on the list above, and both are recorded as a debt in
the phase file.)

All arithmetic is `checked_*`. Only the **primary** header is consulted: a disk whose primary is
damaged has no root as far as this driver is concerned, and MFS falls back. Recovering from the
backup means deciding which copy to believe and writing the repair, which is a tool's job.

(As observed, §3: on a **writable** disk the firmware does that job before the kernel runs. edk2
repairs a damaged primary header or entry array from the backup, so the driver sees a good table and
the refusals above are reachable only on a read-only disk.)

Two partitions of the root type is `root FAIL gpt ambiguous`, not "the first one": H6 selects by
type so that position does not matter, and picking by position on a tie brings it back.

The driver reads the header and all 128 entries as blocks 0–4 of the whole disk (LBA 0–39) through
the same `serve` path as every other request, into a static buffer. (As built: the buffer is a 20
480-byte `.bss` static in `main.rs`, with one `unsafe impl Sync` and one borrow; the driver's
largest stack frame is `main`'s, 10 080 bytes.)

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
extent's first block added, by `checked_add`, after the range check. (As built: one function,
`MinorTable::locate`, owns the whole refusal order — minor, range, zero length, translation — and
`Extent::sector`, which does the addition, also refuses a block outside the extent on its own.) A
driver with a device but **no root answers `ENXIO` on every minor**, as H5 says — the whole disk
included. (As built since the review round: that gate is the free function `part::locate(table,
req)`, which answers `ENXIO` for everything when `table` is `None` — a zero-length request too, the
one MFS probes with — and otherwise defers to `MinorTable::locate`. The main loop calls it before
anything else, so the gate is host-tested; it was first a `match` arm in `main.rs` that nothing
tested.)

`server-rt::bdev` exposes the range half of `validate_read` on its own (`validate_range`), taking
the extent's block count. `virtio-blk` resolves the minor first (`ENXIO`), then calls it, which
keeps the refusal order both drivers share. `memory` calls `validate_read` as before and is not
edited.

Nothing in the system opens a minor other than 0. So that the table is not unexercised code, MFS's
prologue gains one probe beside `bdev.partial`: it reads block 0 of minor 1 and reports whether a
**protective MBR** is there — the `55 AA` signature at offset 510, a first partition record of type
`0xEE` at offset 450, and no image label at offset 0. The signature alone would not do: the ESP's
boot sector ends in the same two bytes, and with the `0xEE` check removed a driver serving the ESP
as minor 1 gets a false `ok` (§3, row E1). `vblk` answers it; `memory` answers `ENXIO`, which has
always been its reply for a minor it does not have. The two outcomes are one line each in the
per-backend marker files and the second line `diff-backends.sh` treats as backend-specific.

(As built: the lines are `bdev.minor ok minor=1 mbr=1`, `bdev.minor absent minor=1 rc=-6`, and the
failures `bdev.minor FAIL read rc=<rc>` and `bdev.minor FAIL mbr`. The minor is a new additive
constant, `BDEV_MINOR_WHOLE_DISK = 1`, in `kernel-shared`. The verdict is `minor::classify` in a new
library module, `fs/mfs/src/minor.rs`. The prologue order is tail, partial, minor, denials. The diff
rewrites each side's own verdict to a placeholder, so a pair with the line missing on *both* sides
passes the diff; the marker files require it.)

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
marker file requires it literally. The verdicts, in the order they can occur: `root FAIL geometry
capacity=<c>` (decided before any read — an empty device, one that is not a whole number of blocks,
and, since the review round, one shorter than the five blocks the table read names:
`part::geometry`), `root FAIL gpt rc=<rc>` (a read of the table's blocks failed), `root FAIL gpt
<why>`, `root FAIL gpt ambiguous`, `root FAIL no root partition`, `root FAIL align`, `root FAIL
readonly` (P8), then 6.3's `head rc=<rc>` / `label` / `blocks hdr=… dev=…` / `tail rc=<rc>` / `tail
label`.

(As built: `gpt rc=<rc>` was not on the first list; it was added by ruling so that an I/O failure is
not reported as a parse failure. `<why>` is the parser's word, so a damaged header is `gpt crc` and
a damaged entry array `gpt entries crc`. The driver holds the outcome as `Option<MinorTable>` — the
table, or nothing — rather than the `Root` enum 6.3 had.)

**A raw, whole-disk MinixFS image is no longer a root.** 6.3's `rootfs.img` attached directly now
reads `root FAIL gpt signature` and falls back. That is H5 as amended: the check runs against minor
0 as H6 defines it.

### P8 — flush, and a read-only disk

The driver requests `VIRTIO_BLK_F_FLUSH` (bit 9) and `VIRTIO_BLK_F_RO` (bit 5). With QEMU's writable
raw drive the accepted word is `0x100000200`, which is the marker's exact text, and `0x100000220`
with `readonly=on` (measured, P11.4).

- **Flush.** When the feature was accepted, every `BDEV_WRITE` that reached the device is followed
  by a `VIRTIO_BLK_T_FLUSH` — a two-descriptor chain, header and status — before the reply. A flush
  that fails makes the write's reply `EIO`. The first successful flush prints `[diag vblk] flush ok
  n=1`; every flush the device completes with a bad status prints `flush FAIL status=<s>`, the raw
  status byte in hexadecimal. When the device does not offer the feature the driver prints `flush
  off` once, at the end of bring-up, and serves writes without it. (As built, narrowing "every
  failure prints": a flush lost to a *broken* device has no status byte and prints no `flush FAIL`
  line — `virtio FAIL step=io` has already been printed and the write still replies `EIO`.)
- **Read-only.** When the device offers `F_RO` the driver accepts it and declines the disk as a
  root: `root FAIL readonly`, and MFS falls back. This pays 6.3's debt 1 — on the evidence of one
  hand-run `readonly=on` boot (mutation 5): no host test fails with the gate deleted and no CI boot
  attaches a read-only disk, which the phase file carries as 6.4's debt 9. Mounting such a disk
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

(As built: the file must hold exactly one byte in `'0'..='9'`; anything else is `persist FAIL
value`. The counter saturates at 9. `gen=<n>` is printed after the decode and before any write, and
no "ok" line follows the rewrite. A failing step prints `persist FAIL <step>`, one of `open`,
`read`, `value`, `close`, `reopen`, `write`, `short`, `close2`. The rewrite is now the first
filesystem write of every boot, ahead of `fs.write`. No geometry constant and no existing marker
moved.)

- A fresh disk boot and every ramdisk boot print `gen=0`; that line is in the common expected set.
- A **reboot** — `MINIXRS_VBLK_IMAGE=target/disk.img`, the image the disk boot just wrote, attached
  with no rebuild — must print `gen=1`. The ramdisk could never show this.

The reboot is checked against its own marker pair only (`check-boot-log.sh reboot`): `persist
gen=1`, the `root ok … lba=` line and `bdev.ds ok name=vblk` required; `persist gen=0` forbidden.
(As built the pair is seven lines: those three required, and `persist gen=0`, `persist FAIL`,
`bdev.root vblk` and `[diag vblk] root FAIL` forbidden. The common forbidden list is not applied
either, so a panic in a reboot is caught only by `gen=1` going missing.) The common set is not
applied, because the write battery was designed against a fresh image and its behaviour on its own
leftovers is not something this slice defines. The reboot therefore runs only until the probe's
line, under a short timeout fixed by measurement (P11.5: 180 s in CI).

As the phase file says, this shows that writes reach the device and nothing about the flush. The
flush is evidenced by `flush ok n=1` and by a mutation recorded as moving nothing else.

### P10 — CI

`qemu-smoke` keeps its name and runs, in order: the disk boot (full check), the reboot of that image
(short), the ramdisk boot (full check), then `diff-backends.sh`. The job's `timeout-minutes` is
re-derived from the measured budget (P11.3). `mkimage`'s host tests run in the `coverage` job, and
`cargo deny` sees no new third-party crate.

(As built: this paragraph first said "the existing `cargo test` job". There is none — the host tests
of the whole workspace run inside `coverage`'s `cargo llvm-cov`. `mkimage` is deliberately not added
to the Miri list: it has no `unsafe`, and its tests build 37 MiB images. The two full boots get 900
s each, up from 600; the reboot 180 s; `timeout-minutes` 80, up from 65. `mkimage` is built in a
step of its own before the first boot, so its compile is not charged to that boot's timeout.
`ipxe-qemu` stays installed, untested without it; the reason the workflow's comment used to give for
it — the virtio-blk-PCI ESP drive — looks like a mis-attribution, since on QEMU 11.0.3
`efi-virtio.rom` is the ROM of `-M virt`'s default `virtio-net-pci` card and not of
`virtio-blk-pci`.)

### P11 — measure first

1. **edk2 boots from a modern virtio-mmio disk** (the phase file's unverified fact 2). **Measured
   2026-11-21, QEMU 11.0.3:** with the ESP attached only as `virtio-blk-device` on
   `virtio-mmio-bus.0` and `force-legacy=false`, the firmware loads Limine from
   `VenHw(…,0000000A00000000)`, the kernel boots, and `vblk` then brings the same device up (`virtio
   ok features=0x100000000 … slot=0`). No firmware-only attachment is needed, and the driver's reset
   recovers the device from whatever state the firmware left it in.
2. **A nested `cargo run` from the runner** completes rather than waiting on the build lock.
   **Measured 2026-11-21:** a nested cargo does, in about a second — the measurement was a `cargo
   build -q --release` of another host crate from a stand-in runner, `mkimage` not existing yet; the
   real `cargo run -p minixrs-mkimage` has run inside every boot since. The runner is handed no
   `RUSTFLAGS` or `CARGO_BUILD_TARGET` by `cargo run`; it unsets them for the nested build all the
   same.
3. **Budget.** The last required marker's position in a fixed-timeout log for both full boots,
   against the merge base — each write now costs a second interrupt round trip, and the ramdisk boot
   now brings a device up. **Measured 2026-11-22, 300 s local boots:** the last required marker
   (`[diag vfs] pm-exit reset closed=1`) is at 82.0–82.7% of the disk log across three boots of the
   final code (82.00%, 82.71%, 82.68% — about 246–248 s) and 59.30% of the ramdisk log (about 178
   s), against 66.8% / 48.1% for 6.3 on `main`. Readings across the run's tasks ranged 68.4%–82.7%
   (disk) and 47.7%–59.3% (ramdisk). CI's full boots go 600 → 900 s (3.63x disk, 5.06x ramdisk, on
   those readings); the job's `timeout-minutes` 65 → 80; the documented local timeout 300 → 450 s.
   **Found in the review round: all of those readings are the SDK flavour**, not the musl flavour
   the method requires and CI builds — the SDK was installed and `MINIXRS_SDK` unset, and every log
   of the run shows `hello` at 46 696 bytes. **The one musl reading (2026-11-22, a 450 s boot with
   `MINIXRS_SDK=/nonexistent`; seconds, since a fraction of a 450 s log is not comparable with a
   fraction of a 300 s one):** the last required marker at 88.03% of the disk log, about 396 s, and
   57.87% of the ramdisk log, about 260 s. On that reading 900 s is 2.27x for the disk boot (3.46x
   ramdisk) and 450 s locally is 1.14x. No timeout was changed on one reading (ruling 36).
4. **What QEMU offers**: the feature word with a raw drive, and with `readonly=on`. **Measured
   2026-11-21:** `0x100000200` (`VERSION_1 | FLUSH`) and `0x100000220` (`+ RO`).
5. **Where `persist gen=` lands** in a default boot, which fixes the reboot's timeout. **Measured
   2026-11-22:** about 34 s of local wall clock after `cargo run` starts, the kernel banner at about
   6 s; a 45 s reboot passed twice. CI's reboot timeout is 180 s.
6. **edk2 and Limine read this tool's FAT32 and GPT.** Item 1 used QEMU's own FAT; the first image
   `mkimage` writes is the first test of ours. **Measured 2026-11-21:** they do, on the first image
   built. macOS's `fsck_msdos -n` finds the volume clean, and `gpt -r show` and `hdiutil imageinfo`
   read the table.

All six are done, on Homebrew's edk2 (edk2-stable202408) under QEMU 11.0.3. **None has been taken on
CI's firmware and runner**, where items 1, 3 and 6 can differ; the first CI run of the branch is
that measurement.

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

`driver-rt` stays in the advisory `miri` job's `-p` list, so the parser is among what that job is
configured to check; no CI run of this branch has happened, so that is a configuration, not a
result. Run locally: `cargo miri test -p minixrs-driver-rt` on the parser's commit (Task 2) and
again on the final tree in the review round, 2026-11-22 — 113 passed, 0 failed, both times. (As
built: `mkimage`'s rows are the integration test `tools/mkimage/tests/image.rs`, read back by a
FAT32 reader written for the test; the unit tests in the crate's modules cover the pieces.)

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

*Observed (2026-11-22, QEMU 11.0.3, Homebrew edk2-stable202408), each boot run once by hand:*

| Boot                                                       | Observed                                                                                                                                                                                                                             |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `MINIXRS_ROOT=nodisk`, 300 s                               | As predicted: `[diag vblk] no device`, `bdev.ds ok name=memory ep=3` with no `bdev.root vblk` line, `bdev.minor absent minor=1 rc=-6`, `persist gen=0`. Common set passes (107 expected, 45 forbidden); both per-backend sets fail.  |
| the disk image attached `readonly=on`, 300 s               | As predicted: `virtio ok features=0x100000220 …`, `root FAIL readonly`, the fallback, no flush line; the image's SHA-256 equals a reference built with the same arguments.                                                           |
| 6.3's raw `rootfs.img` as the only disk                    | As predicted: no kernel, no Limine — edk2 prints `failed to load Boot0001 … Not Found` and drops to the EFI shell. With the raw image as a second (mmio) drive under `nodisk`: `capacity=2048`, `root FAIL gpt signature`, fallback. |
| primary entry array damaged (one bit, byte 1720), writable | **Not predicted:** edk2 repairs it from the backup before the kernel runs. `root ok`; the disk set passes (165); LBA 3 of the image was rewritten by the firmware.                                                                   |
| primary header damaged (one bit, byte 532), writable       | **Not predicted:** the same — repaired, `root ok`, LBA 1 rewritten.                                                                                                                                                                  |
| entry array damaged, the one disk `readonly=on`            | edk2 still boots it; the driver prints `root FAIL gpt entries crc` — not the `gpt crc` predicted, which is the header's word.                                                                                                        |
| header damaged, the one disk `readonly=on`                 | edk2 refuses to boot it (`Not Found`, the EFI shell).                                                                                                                                                                                |
| header damaged, a read-only second drive under `nodisk`    | `root FAIL gpt crc`.                                                                                                                                                                                                                 |

The firmware tries the mmio disk before the PCI one, so the two-drive form works only while the mmio
disk has no usable ESP; otherwise the firmware boots from it, with that image's own kernel.

Against the per-backend marker sets, which both logs are meant to fail: the `nodisk` log misses 6
lines of the ramdisk set and trips its forbidden `no device`, and misses 8 lines of the disk set and
trips 3 of its forbidden ones; the `readonly=on` log misses exactly two lines of the ramdisk set,
the feature word and the `root FAIL` reason.

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

*Observed (2026-11-22), every boot stub-free, the tree proved clean afterwards.* Rows 7 and 10 were
run in more than one form, and row E1 was added for the probe's `0xEE` check:

| #   | Mutation as applied                                                           | Observed                                                                                                                                                                                   | Host tests failing                                       |
| --- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------- |
| 1   | `Extent::sector` does not add `first_block`                                   | As predicted: `root FAIL label`, fallback.                                                                                                                                                 | 4 in virtio-blk `part::tests`                            |
| 2   | `scan` takes partition 1 as the root, not `find_type`                         | As predicted: `root FAIL label`, fallback.                                                                                                                                                 | 8 in `part::tests`                                       |
| 3   | header CRC check deleted in `driver-rt::gpt`                                  | Booted as `nodisk` plus a header-damaged read-only second drive. Control: `root FAIL gpt crc`. Mutated: `root FAIL readonly` — the next gate catches the disk.                             | 5 driver-rt, 2 virtio-blk                                |
| 4   | no-root arm answers 0 instead of `ENXIO`                                      | Ramdisk boot: `bdev.ds ok name=vblk ep=11`, `mount FAIL rc=-5`, a cascade of `FAIL`s, and the boot stalls. Trips the forbidden `bdev.ds ok name=vblk` and several common forbidden lines.  | none (`main.rs` has no host tests)                       |
| 5   | read-only gate deleted                                                        | `readonly=on` boot: `root ok`, the mount passes, then `bdev.partial FAIL write rc=-5`, `persist gen=0` followed by `persist FAIL reopen rc=-5`, `fs.write FAIL short`, …; image unchanged. | none                                                     |
| 6   | flush skipped at the call site in `do_write`                                  | As predicted: only `flush ok n=1` goes missing; the reboot still prints `persist gen=1` and passes its set.                                                                                | none (the same mutation inside `flush_after`: 1)         |
| 7   | `write_reply` ignores a failed flush                                          | Nothing moves in a boot.                                                                                                                                                                   | 1 (`a_write_replies_its_length_unless_its_flush_failed`) |
| 7b  | `main.rs`'s `flush()` returns `Ok` on a bad status                            | **Nothing moves anywhere — uncovered.** QEMU's flush cannot be made to fail.                                                                                                               | none                                                     |
| 8   | `accept_features` omits `VERSION_1`                                           | `virtio ok features=0x200 …`; QEMU keeps the device fully working.                                                                                                                         | 3 (2 driver-rt, 1 virtio-blk)                            |
| 9   | init skips the `/etc/boots` rewrite                                           | As predicted: the reboot prints `persist gen=0` and fails its set.                                                                                                                         | —                                                        |
| 10a | `ROOT_FIRST_LBA + 1` in `mkimage`'s layout                                    | Does not compile (a const assertion in `layout.rs`).                                                                                                                                       | —                                                        |
| 10b | root bytes copied 512 bytes late, the GPT entry unchanged                     | `root FAIL label`.                                                                                                                                                                         | 1 mkimage                                                |
| 10c | GPT entry shifted one sector, bypassing the layout constants                  | `root FAIL align`.                                                                                                                                                                         | 2 mkimage                                                |
| 11  | minor 1 answered from the root extent                                         | As predicted: `bdev.minor FAIL mbr`.                                                                                                                                                       | —                                                        |
| E1  | minor 1 answered from the ESP; then with the probe's `0xEE` check removed too | With the check: `bdev.minor FAIL mbr`. Without it: a false `ok`.                                                                                                                           | 3 virtio-blk; +2 mfs                                     |

**Where the predictions were wrong.** Five rows and one recorded boot:

- **Row 3** predicted a boot would show the damaged header "if it reaches the kernel". On a writable
  disk it never reaches the driver damaged — the firmware repairs it — and on a read-only single
  disk the firmware refuses to boot. The mutation could be booted only on a read-only second drive,
  and there the mutated driver still refuses the disk, at the read-only gate. The CRC check is held
  by seven host tests, not by a boot.
- **Row 4** predicted `mount FAIL`. It came, with `rc=-5`, and with it a cascade of `FAIL` lines and
  a stalled boot. Several of those lines (`mount FAIL`, `bdev.tail FAIL`, `bdev.deny FAIL`, `[diag
  vfs] fs.mount FAIL`, `fs.read FAIL`, `exec.stage FAIL`, `fs.deny FAIL`, `exec.deny FAIL`) are not
  in the common forbidden file; the boot is caught by missing `ok` lines and other forbidden hits
  instead.
- **Row 5** predicted "mounts, writes fail `EIO`". It does mount, but the first failures are the
  `bdev.partial` probe's write and the persistence probe's truncating *reopen*, ahead of any
  `write()`.
- **Row 8** predicted "two host tests"; three fail.
- **Row 10** was one row with two predictions in it. "One sector late" is three different mutations
  with three different outcomes: the layout constant cannot be moved at all (10a), bytes copied late
  under an unmoved entry fail the *label* check (10b), and only an entry moved behind the layout's
  back fails `align` (10c).
- **The damaged-CRC boot** asked whether edk2 would still boot the image, and predicted `root FAIL
  gpt crc` if so. The answer has three parts nobody predicted: on a writable disk the firmware
  repairs the table and the driver sees nothing wrong; read-only, entry-array damage boots and reads
  `gpt entries crc`; read-only, header damage does not boot at all.

Rows 6 and 7 came out as expected: a boot says almost nothing about the flush. Row 7b is the honest
"nothing moves" row.

Every row above was observed on a developer machine. "In CI" in row 4's prediction is a statement
about which boot the mutation fails — the ramdisk boot, which CI is configured to run — and not a CI
result: the branch had not run on CI when this was written.

**After the whole-branch review round (2026-11-22).** Five things in this section changed or were
found wanting; code moved for three of them and a script for a fourth:

- **Row 4's gate now has host tests.** It was a `match` arm in `main.rs`, and "none" in the last
  column was the finding: on the rootless boot MFS's `bdev.minor` probe goes to `memory`, so no
  marker saw this driver's no-root answer for any minor but the root. The gate is now
  `part::locate(None, …)`. Making that arm answer through instead of `ENXIO` fails
  `with_no_root_every_request_is_enxio_on_every_minor`,
  `with_no_root_a_zero_length_request_is_enxio_not_empty` and
  `with_no_root_enxio_comes_before_every_other_refusal` (applied, observed, reverted; no boot).
- **A disk shorter than five blocks is `geometry`.** `find_root` read whole-disk blocks 0–4 of any
  device whose capacity was a whole number of blocks, so a one-to-four-block device was sent reads
  past its end and the verdict was whatever the device made of them. `part::geometry` refuses it
  before any read; `a_device_shorter_than_the_table_is_refused_for_its_geometry` (which failed
  before the fix) and `a_five_block_device_reaches_the_gpt_checks` hold the boundary at 40 sectors.
- **The denial probes named a minor that exists.** MFS's `bad-minor` and `wr-minor` used the literal
  7, which on `vblk` is GPT partition 6: `ENXIO` only because the image has two partitions. They
  name `BDEV_MINOR_NONE` now (ruling 31). No marker text carried the number.
- **`check-boot-log.sh` passed an empty required set.** The reboot leans on one three-line file;
  with it emptied the script printed `PASS … 4 marker checks`. It now exits 1 before reading the log
  when a boot kind requires nothing (shown on a scratch copy; the tracked files were not touched).
- **Review Focus 6 is less covered than the host tests suggest — recorded, not fixed.** Three of
  `blk::flush_after`'s tests exercise arms production never reaches: `do_write` always passes
  `Some(wrote)` and a non-empty plan, because a zero-length write, a refused grant and a failed
  read-before-write have all returned earlier, in `main.rs`, where no host test and no marker looks.
  "Not issued for a zero-length write or a refused grant" therefore rests on reading. Same seam as
  row 7b; the phase file's 6.4 debt 1 carries both.

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

| #  | Ruling                                                                                  | If wrong                                                                                   |
| -- | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| 1  | P4: the ramdisk CI boot becomes the rootless-disk fallback boot; no-device is hand-run  | a third full boot in CI (fifteen minutes at 900 s), or `fat:rw:` kept for one mode         |
| 2  | P2: FAT32 and GPT writers are hand-written, no `fatfs` / `gpt` crate                    | swap the writer's internals for the crates; the CLI and the image layout do not move       |
| 3  | P2: `mkimage` takes `rootfs.img` as a file instead of calling the `mkfs-mfs` library    | one path dependency and a manifest argument added to the tool                              |
| 4  | P1: fixed disk and partition GUIDs, zero timestamps                                     | two images attached to one machine would collide; nothing here attaches two                |
| 5  | P5: primary GPT header only; a damaged primary means no root                            | a backup-header path in the parser and a decision about which copy wins                    |
| 6  | P5: two root-type partitions is a refusal, not first-wins                               | one arm changes; multi-root disks need a selector that does not exist                      |
| 7  | P6: every minor is read-write, and all answer `ENXIO` when there is no root             | a per-minor access rule in the request path; until then any server can write any minor (†) |
| 8  | P6: an MFS prologue probe of minor 1, a second backend-specific line in `diff-backends` | the minor table is covered by host tests alone                                             |
| 9  | P7: a raw whole-disk MinixFS image is no longer accepted as a root                      | a no-GPT arm in the root check, and two disk formats to keep booting                       |
| 10 | P8: a read-only disk is declined as a root rather than mounted read-only                | read-only mount support in MFS and VFS                                                     |
| 11 | P8: `flush ok n=1` once, not a running count                                            | a periodic line whose value varies with boot length and cannot be a marker                 |
| 12 | P9: the probe lives in `init` and seeds `/etc/boots` in the root image                  | the image's geometry-derived markers move once more if it is relocated                     |
| 13 | P9: the reboot is checked against its own pair, not the common set                      | the write battery must be made idempotent on a used image                                  |
| 14 | 6.3's debt 3 (`fs.trunc` cannot see a dropped truncate) is **not** paid here            | it stays owed; the persistence probe is beside the write battery, not a change to it       |

† *Ruling 7, in full.* Every `SRV_T` row of the boot image is given `ipc_to` over all active priv
slots, so PM, VFS, RS, `memory`, TTY, DS, MFS, VM and SCHED can each send `vblk` a `BDEV_WRITE`; no
user process can. In 6.3 such a write could reach only the root image, because the ESP was on a PCI
drive minix.rs could not see. With every minor read-write, any of those servers can now rewrite the
kernel on the ESP (minor 2) and the GPT and its backup (minor 1), and that survives a reboot. The
ruling stands; what it costs is stated here because the review round found it unstated (phase file,
6.4 debt 11).

**Rulings made during the run**, grouped by the task that made them, and after it in the
whole-branch review round (31–36). The plan's ledger has each with its commit.

| #  | Ruling                                                                                                                                                                                                                                                                                                                                | If wrong                                                                                                                |
| -- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| 15 | Task 2: partition numbers are 1-based slot numbers, so minor = 1 + *n*                                                                                                                                                                                                                                                                | 0-based numbering moves every partition minor and the `part=` marker by one                                             |
| 16 | Task 2: the parser does not require `FirstUsableLBA` past the entry array and does not detect overlapping partitions; two refusals beyond P5 (`header short`, `entry index`); `entry range` covers inverted and out-of-range                                                                                                          | a table that overlaps itself is accepted — harmless while only the root is mounted (phase file, 6.4 debt 5)             |
| 17 | Tasks 2 and 3: the driver's CRC-32 is bit-at-a-time, the tool's table-driven, and they share nothing                                                                                                                                                                                                                                  | none in behaviour; a kilobyte of table in every driver that links `driver-rt` if swapped                                |
| 18 | Tasks 4 and 5: a failed read of the table's blocks is its own verdict, `root FAIL gpt rc=<rc>` (coordinator ruling, not in the plan's first list); `Option<MinorTable>` replaces the `Root` enum; geometry is checked before any read; `MinorTable::locate` owns the refusal order                                                    | one more marker spelling to keep stable                                                                                 |
| 19 | Task 5: `flush FAIL status=` prints the raw status byte in hex; a flush lost to a broken device prints no `flush FAIL` line (narrows P8); `flush off` is printed at the end of bring-up                                                                                                                                               | a flush that failed because the device broke is told from any other breakage only by its place in the log               |
| 20 | Task 5: the GPT table buffer is a 20 480-byte `.bss` static in `main.rs`, with one `unsafe impl Sync` and one borrow                                                                                                                                                                                                                  | one more `unsafe` site in the driver; the alternatives are a block-at-a-time parser or a 20 KiB stack frame             |
| 21 | Task 3: the disk rounds up to a whole MiB (75 776 sectors); the ESP's 34 MiB is a named input; the FAT is sized by Microsoft's formula (540 sectors, over-provisioned)                                                                                                                                                                | the `capacity=` and `lba=` markers move if any of the three changes                                                     |
| 22 | Task 3: FAT details — protective MBR CHS `FF FF FF`; short aliases as Windows makes them; ASCII-only names; case-insensitive duplicates refused; label `MINIXRS ESP`, serial `0x6D780604`; a third boot-record sector; zero dates; multi-cluster directories                                                                          | a firmware stricter than edk2 or Limine rejects the volume; each is one constant or one function                        |
| 23 | Task 3: the CLI requires at least one ESP file; images are written sparsely; the independent FAT reader lives in an integration test                                                                                                                                                                                                  | an image with an empty ESP can be built only through the library                                                        |
| 24 | Task 6: the runner prints its QEMU argv on stderr; `MINIXRS_VBLK_IMAGE` skips the Limine fetch and ignores the kernel ELF; the backend is validated before the firmware search; `nodisk` rejects the disk knobs; the images are `target/disk.img` and `target/disk-rootless.img`, and `CARGO_TARGET_DIR` is not honoured for them     | a caller with a custom target directory finds the images in the repository's `target/`                                  |
| 25 | Task 6: `flush off` is forbidden in the **common** file; the ramdisk pair requires all four bring-up lines; `slot=0` is still not asserted; `diff-backends` allows, and does not require, the fallback line                                                                                                                           | a device without `FLUSH` fails every boot's check, not one backend's                                                    |
| 26 | Task 7: `mbr=1` means a *protective* MBR (`55 AA`, type `0xEE`, no image label); `BDEV_MINOR_WHOLE_DISK = 1` is added to `kernel-shared` (additive, not in the C headers); the classifier is `fs/mfs/src/minor.rs`; probe order tail, partial, minor, denials; the diff passes a pair with the verdict missing on both sides          | a second additive constant under D8; a disk with a non-protective MBR at LBA 0 fails the probe                          |
| 27 | Task 8: `/etc/boots` must be exactly one decimal digit; the counter saturates at 9; eight `FAIL` steps; `gen=` is printed after the decode and before any write, with no "ok" line after the rewrite; the reboot pair is 7 lines and the common forbidden set is not applied; the rewrite is the first filesystem write of every boot | the tenth boot of one image stops counting; a panic in a reboot is caught only by `gen=1` going missing (6.4 debt 4)    |
| 28 | Task 9: CI's two full boots go 600 → 900 s, one number budgeted for the slower; the reboot gets 180 s; `timeout-minutes` 80; `mkimage` is pre-built in its own step and tested in `coverage`, not under Miri; `ipxe-qemu` is kept                                                                                                     | 13 more minutes of CI per run; if the runner's TCG is slower than the factor over local, raise both (ruling 36)         |
| 29 | Coordinator: the documented local timeout for a marker-file verification goes 300 → 450 s                                                                                                                                                                                                                                             | two and a half more minutes per local full boot; a musl-flavour disk boot clears it by only 1.14x (ruling 36)           |
| 30 | Task 11: mutation 7b (a `flush()` that reports a bad status as success) is recorded as uncovered rather than given a host-test seam; no new guard is added for a missing `VERSION_1`                                                                                                                                                  | 6.4's debt 1 and 6.2's debt 9 stay owed                                                                                 |
| 31 | Review round (C1): MFS's `bad-minor` / `wr-minor` probes name `BDEV_MINOR_NONE = i32::MAX`, a third additive `kernel-shared` constant (not in the C headers) that each block driver const-asserts outside its own minors — not a constant local to MFS, and not a negative minor (note below)                                         | a third additive constant under D8; undone by inlining `i32::MAX` in MFS and deleting two const asserts and their tests |
| 32 | Review round (C2, C3): the no-root gate and the short-disk refusal are pure functions in `part.rs` (`locate`, `geometry`), called from the main loop and `find_root`; `do_read` / `do_write` take an already-located request; a disk under five blocks is `root FAIL geometry`, not a new verdict                                     | one marker spelling has three causes, told apart by `capacity=`; a new verdict is one more spelling to keep stable      |
| 33 | Review round (C5): `check-boot-log.sh` exits 1, before reading the log, when a boot kind's expected files hold no marker                                                                                                                                                                                                              | a boot kind checked by forbidden lines alone cannot be expressed; none exists                                           |
| 34 | Review round (D10): the Definition of done in `docs-and-workflow.md`, as this branch changed it, requires the reboot check as well as the two full boots — of every slice that owes the boots, not of 6.4 alone                                                                                                                       | one sentence removed from the Definition of done; the reboot stays a CI step                                            |
| 35 | Review round (C6, C7, C8, D2): recorded, not fixed — three `flush_after` arms production never reaches; every server's reach over the boot disk; `MINIXRS_VBLK_IMAGE` resolving against the caller's directory; `mkimage`'s `basis()` on a name like `". .txt"`; 6.3's debts 1 and 7 restated as hand-held and partly paid            | each stays owed: phase file, 6.4 debts 1, 9, 10 and 11                                                                  |
| 36 | Review round: its boots ran in the musl flavour (`MINIXRS_SDK=/nonexistent`), which showed every budget reading of the slice to be SDK-flavour; the musl disk boot's last required marker is about 396 s in. No timeout was changed on one reading: 900 s, 180 s and the local 450 s stand                                            | CI's disk boot has 2.27x, not 3.63x: a slow runner fails the first CI run by timeout (note below; 6.4 debt 13)          |

*Ruling 31, the alternatives.* A constant local to MFS would have fixed the probes and nothing else:
MFS and the drivers share no crate but `kernel-shared`, so nothing could tie a local value to
`virtio-blk`'s table bound, and the next table to grow would falsify it as silently as this one did.
A negative minor is absent from both drivers today (`memory` compares against the root minor;
`virtio-blk` fails the `usize` conversion), but a minor is non-negative by convention, as in MINIX,
and a driver is entitled to refuse a negative one as *malformed* (`EINVAL`) — which is not what a
"no such device" probe is for. `i32::MAX` is well-formed and out of every table: `virtio-blk`
asserts `BDEV_MINOR_NONE >= MINORS` and `server-rt::bdev` asserts it is not the root minor.

*Ruling 36, what it costs.* If the runner's TCG is more than about 2.2x slower than the developer
machine, the branch's first CI run fails its disk boot by timeout, with markers missing from some
point to the end of the boot — the starved-boot shape, not a regression. The fix is the two full
boots and `timeout-minutes` raised together. The earlier raises were forced at 1.6–1.8x and 6.3 kept
its budget at 3.0x, so 2.27x on a single reading is a judgement the user has not made; it is 6.4's
debt 13.

---

## 6. Out of scope

- Any change to MFS's FS-band handlers or to VFS; a read-only mount.
- GPT repair, the backup header, MBR-only disks, more than one disk.
- A mount of any minor other than 0, and device nodes for partitions.
- A queue deeper than one request; a request timeout (6.2 debt 1 stays owed).
- Proving the flush reaches stable storage, which a raw image under QEMU cannot show.
- Removing the ramdisk or the `rootfs` blob (H7).

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
  --target aarch64-unknown-none --release`), then `MINIXRS_ROOT=<disk|ramdisk> timeout 450 cargo run
  -p minixrs-kernel --target aarch64-unknown-none --release > $S/<name>.log 2>&1`. One boot at a
  time. Read `check-boot-log.sh`'s whole output; apply the starved-boot check before believing a
  missing marker. (Corrected: this said `timeout 300`, which is what the run's boots used. Task 9
  measured the disk boot's last required marker at 82.7% of a 300 s log, about 1.2x of headroom, so
  the documented local timeout became 450 s — see the ledger. Corrected again in the review round:
  none of the run's boots set `MINIXRS_SDK=/nonexistent`, so all of them were the SDK flavour; the
  commands above measure the musl flavour only with that variable set on the build *and* on the
  boot.)
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
[diag vblk] root FAIL <geometry capacity=…|gpt rc=<rc>|gpt <why>|gpt ambiguous|no root partition|align|readonly|head rc=…|label|blocks hdr=… dev=…|tail rc=…|tail label>
[diag vblk] flush ok n=1
[diag vblk] flush off
[diag vblk] flush FAIL status=<s in hex, e.g. 0x1, 0xff>
[diag mfs] bdev.minor ok minor=1 mbr=1
[diag mfs] bdev.minor absent minor=1 rc=-6
[diag mfs] bdev.minor FAIL read rc=<rc>
[diag mfs] bdev.minor FAIL mbr
minix.rs init: persist gen=<n>
minix.rs init: persist FAIL <open rc=-N|read rc=-N|value|close rc=-N|reopen rc=-N|write rc=-N|short|close2 rc=-N>
```

`features=` is the measured word (P11.4): `0x100000200` for a writable raw drive, `0x100000220` with
`readonly=on`. `<why>` is one of `driver-rt::gpt`'s fourteen words — `header short`, `signature`,
`revision`, `header size`, `crc`, `my lba`, `entry size`, `entry count`, `entry lba`, `usable
range`, `entries crc`, `entries short`, `entry range`, `entry index` — so a damaged header is `gpt
crc` and a damaged entry array `gpt entries crc`. The `root FAIL` alternatives are in the order the
checks run. (Corrected against the code: the block first written here had no `gpt rc=<rc>`, listed
the label checks out of order, and gave the two `bdev.minor` failures and the `persist` steps only
as placeholders.)

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
workspace `Cargo.toml`. (As built, also `src/layout.rs`, which holds every layout number, and
`tests/image.rs`, the integration test that reads an image back.)

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

**Files:** `fs/mfs/src/main.rs` (prologue only), marker files, `tools/diff-backends.sh`. (As built,
also `fs/mfs/src/minor.rs` for the host-tested verdict, and `kernel-shared/src/callnr.rs` for
`BDEV_MINOR_WHOLE_DISK`.)

**Proves it:** `bdev.minor ok minor=1 mbr=1` on the disk boot, `absent` on the ramdisk boot.

### Task 8: persistence (P9)

**Files:** `userland/init/src/main.rs`, `kernel/build.rs` (manifest), `kernel-shared/src/rootfs.rs`
(path and any geometry constant), `tools/check-boot-log.sh`,
`tests/qemu-boot.reboot.{expected,forbidden}`, `tests/qemu-boot.expected`.

**Proves it:** disk boot `persist gen=0`; reboot with `MINIXRS_VBLK_IMAGE=target/disk.img` prints
`gen=1` and passes `check-boot-log.sh reboot`; ramdisk boot `gen=0`.

### Task 9: CI and the budget (P10, P11.3)

**Files:** `.github/workflows/ci.yml`. (As built, also one self-test case in
`tools/boot-relevant.sh`.)

Measure both full boots against the merge base; set the reboot's timeout and the job's
`timeout-minutes` from the numbers. `mkimage`'s host tests need no CI edit: there is no `cargo test`
job (the spec's P10 first said there was), and the workspace-wide `coverage` job already runs them.

### Task 10: recorded boots (spec §3)

`nodisk`, `readonly=on`, the raw image, a damaged entry-array CRC. (As run: the damaged-table boot
became five boots — header and entry-array damage, writable and read-only, one and two drives — once
the first showed the firmware repairing the table.)

### Task 11: mutations (spec §3)

All eleven, stub-free where the marker allows. (As run: fifteen rows — 7 and 10 in more than one
form, and E1 added for the probe's `0xEE` check.)

### Task 12: documentation, trackers, the whole-branch sweep (spec §4)

Book, conventions, phase file, `docs/plan.md`, the stale-claim grep, the fresh whole-branch review.

## Ledger

One entry per task: its commit, what was verified, and every ruling made in it. Each ruling's cost
if it was wrong is in the spec's §5 (rows 15–36); the measurements are in the spec's P11 and §3 and
in the phase file's 6.4 section. "As predicted" and "not predicted" refer to the spec's §3.

- **Task 0 (2026-11-21; no commit of its own).** P11.1: edk2 boots from a modern virtio-mmio disk —
  see the spec. P11.2: a nested `cargo build -q --release -p minixrs-mkfs-mfs` inside a stand-in
  cargo runner completes in about a second with no lock wait, so the runner uses the shared target
  directory. (What was measured is `cargo build` of another host crate — `mkimage` did not exist
  yet; the runner as built issues `cargo run -p minixrs-mkimage`, which every boot since has
  exercised.) P11.5, first reading: `init`'s first line lands at 21.6% of a 45 s default boot (about
  10 s in); the reboot's timeout is set in Task 9 from the probe's own position.
- **Task 1 (`ea70c7f`).** `ROOT_PARTITION_TYPE_GUID` as its 16 on-disk bytes, with a test pinning
  the mixed-endian bytes to the text form; `bdev::validate_range`, with `validate_read` reduced to
  the minor check followed by it. Verified: the two crates' host tests; `validate_read`'s existing
  tests unchanged. No ruling beyond the spec. Noticed, not fixed: an unresolved rustdoc link at
  `kernel-shared/src/execstack.rs:135`, which predates the slice (phase file, 6.4 debt 8).
- **Task 2 (`6d8cac4`).** `driver-rt::gpt`. Verified: host tests that flip one header field at a
  time and recompute the CRC; the `123456789` → `0xCBF43926` vector. *Rulings:* partition numbers
  are 1-based slot numbers (§5 row 15); `FirstUsableLBA` past the entry array and overlapping
  partitions are not checked, two refusals are added beyond P5's list, `entry range` covers both the
  inverted and the out-of-range entry (16); the CRC is bit-at-a-time (17). Miri: `cargo miri test -p
  minixrs-driver-rt` on this commit, 113 passed, 0 failed (Miri 0.1.0, the 2026-09-30 toolchain;
  134.75 s by Miri's own clock). Re-run on the final tree in the review round, below.
- **Task 3 (`97693dd`).** `tools/mkimage`. Verified: the integration test reads every image back
  with a FAT32 reader of its own, with `driver-rt::gpt`, and field by field; P11.6 — macOS's
  `fsck_msdos -n` passes the ESP, and edk2 and Limine boot the kernel from the first image.
  *Rulings:* the layout's three choices (21); the FAT details (22); the CLI requires an ESP file,
  images are written sparsely, the independent reader is an integration test (23); a table-driven
  CRC that shares nothing with the parser's (17).
- **Task 4 (`35c7329`).** virtio-blk's pure half: `part.rs`, `RootFail` and its `Display`, the flush
  header and the two flush decisions, the feature bits. Verified: host tests; `main.rs` did not call
  any of it yet. *Rulings:* I/O failures are `RootFail` arms; `Extent::sector` refuses a block
  outside the extent itself; `MinorTable::locate` owns the refusal order (18).
- **Task 5 (`aab5166`).** `main.rs` wired to the pure half. Verified: a raw whole-disk MinixFS image
  now reads `root FAIL gpt signature` and MFS falls back. *Rulings:* `root FAIL gpt rc=<rc>` for a
  failed table read — a coordinator ruling, and a marker the plan's list did not have;
  `Option<MinorTable>` instead of the `Root` enum; geometry before any read (18). The flush's status
  in hex, no `flush FAIL` line for a flush lost to a broken device, `flush off` at the end of
  bring-up (19). The table buffer as a `.bss` static with one `unsafe impl Sync` (20); largest stack
  frame `main`, 10 080 bytes.
- **Task 6 (`1506b6e`).** The runner, the two images, the per-backend marker files, the diff's first
  allowance. Verified: 300 s disk and ramdisk boots pass `check-boot-log.sh` (160 checks each at
  this commit) and fail each other's set; `diff-backends.sh` passes; P11.4 — `0x100000200`, and
  `0x100000220` read-only. *Rulings:* the runner's argv line, `MINIXRS_VBLK_IMAGE` semantics,
  validation order, `nodisk` refusing the knobs, the image names (24); `flush off` forbidden in the
  common file, four bring-up lines required of the ramdisk pair, `slot=0` unasserted, the fallback
  line allowed rather than required (25).
- **Task 7 (`7cd8f73`).** The `bdev.minor` probe. Verified: `ok minor=1 mbr=1` on the disk boot,
  `absent minor=1 rc=-6` on the ramdisk boot; mutation 11 applied, booted and reverted (`bdev.minor
  FAIL mbr`). *Rulings:* a protective MBR, not a bare signature; `BDEV_MINOR_WHOLE_DISK`; the
  classifier in the library; the probe's place in the prologue; the diff's placeholder (26).
- **Task 8 (`82652be`).** Persistence. Verified: `persist gen=0` on the disk and ramdisk boots;
  three successive reboots of one image print `gen=1`, `gen=2`, `gen=3`; both full boots pass 165
  checks and the reboot its 7; mutation 9 applied and reverted (the reboot prints `gen=0`).
  *Rulings:* the strict one-digit value, saturation, the eight `FAIL` steps, the print-before-write
  order, the seven-line reboot pair without the common forbidden set, the rewrite as the first write
  of the boot (27). No geometry constant and no existing marker moved.
- **Task 9 (`70c9bd0`).** CI and the budget. Verified by measurement, not by a CI run: P11.3 and
  P11.5 as the spec records them — on SDK-flavour boots, as the review round found (below).
  *Rulings:* 600 → 900 s, 180 s, `timeout-minutes` 80, `mkimage` pre-built, `ipxe-qemu` kept, tests
  in `coverage` and not under Miri (28). Not validated without a push: CI's firmware, the budgets on
  the runner, the pre-built tool being seen as fresh on Linux, `ipxe-qemu` (phase file, 6.4 debt 7).
- **Task 10 (observation only, no commit).** The recorded boots. `nodisk`, `readonly=on` and the raw
  image as predicted. **The damaged-table boot was not:** edk2 repairs a damaged primary GPT on a
  writable disk before the kernel runs, so the driver's refusals appear only read-only (spec §3,
  phase file debt 3).
- **Task 11 (observation only, no commit).** Fifteen mutation rows, each applied, observed and
  reverted, the tree proved clean afterwards. Rows 3, 4, 5, 8 and 10 did not match their predictions
  (spec §3 says how). *Ruling:* row 7b — a `flush()` that reports a bad status as success — moves
  nothing and is recorded as uncovered, with no test seam added; no new guard for a missing
  `VERSION_1` (30).
- **Task 12 (`182641d`, `eddd33e`).** `book/`, `docs/conventions/`, the phase file, `docs/plan.md`,
  this plan and the spec; the stale-claim sweep over comments, marker commentary and scripts
  (comment-only edits). *Ruling (coordinator):* the documented local timeout for a marker-file
  verification is 450 s (29).

- **Review-fix round (2026-11-22; code in `ea41698`, documentation in the commit that carries this
  entry).** Two fresh reviewers read the whole branch — one the code diff, one the documentation
  against the code — and found no blocker. Each finding, and what was done with it:
  - *C1, fixed.* MFS's `bad-minor` and `wr-minor` named minor 7, GPT partition 6 on `vblk`. They
    name `BDEV_MINOR_NONE` now; `bdev.deny ok n=10` is unchanged and no marker text carried the
    number. *Ruling (31):* a shared constant, `i32::MAX`, with a const assert in each driver.
  - *C2, fixed.* The no-root `ENXIO` gate moved from an untested `match` arm in `main.rs` to
    `part::locate`, with host tests for no table against minors 0, 1, 2, 3 and −1, a normal and a
    zero-length request; `do_read` and `do_write` take the located request. Mutation: the `None` arm
    answering through fails `with_no_root_every_request_is_enxio_on_every_minor`,
    `with_no_root_a_zero_length_request_is_enxio_not_empty` and
    `with_no_root_enxio_comes_before_every_other_refusal`. *Ruling (32).*
  - *C3, fixed.* `part::geometry` refuses a device with fewer than five blocks before the table is
    read; `a_device_shorter_than_the_table_is_refused_for_its_geometry` was written first and seen
    to fail (capacity 8 read as one block), and `a_five_block_device_reaches_the_gpt_checks` holds
    the boundary. *Ruling (32).*
  - *C4, fixed.* Two comments in `blk.rs` that described the device as the root image, and two doc
    lines far over width, in `blk.rs` and `main.rs`.
  - *C5, fixed.* `check-boot-log.sh` fails a boot kind with no required marker. Shown on a scratch
    copy: with the reboot's expected file emptied the old script printed `PASS … 4 marker checks`
    and the new one exits 1. *Ruling (33).*
  - *C6, recorded.* Three `blk::flush_after` tests exercise arms `do_write` never reaches; the "no
    flush after a zero-length write or a refused grant" half of Review Focus 6 rests on early
    returns no test or marker covers (spec §3, phase debt 1).
  - *C7, recorded.* Every `SRV_T` boot process can send `vblk` a write, and since 6.4 that reaches
    the ESP and the partition table and survives a reboot (spec ruling 7, phase debt 11, the drivers
    chapter).
  - *C8, known and left.* `MINIXRS_VBLK_IMAGE` given as a relative path resolves against the
    caller's directory, not the repository's; and `mkimage`'s `basis()` can yield an empty base for
    a name like `". .txt"`, which none of the runner's three files is.
  - *D1, fixed.* H6 no longer says MFS sends every request as minor 0 and is "literally unchanged".
  - *D2, fixed.* 6.3's debt 7 is "partly paid", with what is covered and what is not, and its
    remainder is 6.4's debt 10; 6.3's debt 1 says it is held by a hand-run boot (debt 9). The spec's
    P4 and P8 say the same.
  - *D3, fixed.* The two sentences that read as CI results say what was observed and where.
  - *D4–D6, fixed.* The book's "partitions as minors … when the disk carries one", "both boots CI
    runs", and the spec's P4 contradiction about PCI.
  - *D7, fixed — and found worse than reported.* The readings on final code are 82.00%, 82.71% and
    82.68% of 300 s logs, stated the same in six places. They are all **SDK-flavour** boots: see the
    verification below, and ruling 36.
  - *D8, fixed.* Miri is recorded under Task 2 and re-run here; the documents say `driver-rt` is in
    the advisory job's list and was run locally, not that CI ran it.
  - *D9, D10, fixed.* The reboot recipe says it is once per disk boot; `build-and-boot.md` says a
    slice owes both full boots and the reboot. *Ruling (34).*
  - *D11, fixed.* The MFS comment that called `bdev.ds` the only backend-specific line.
  - *D12, fixed.* One home each: the budget in `ci.md`, the reboot-ordering rule in
    `build-and-boot.md`, the `nodisk` verdict in `testing-and-markers.md`, the recorded boots in the
    spec's *Observed* table with a summary and a link from the phase file.
  - *D13, fixed, with one fact added.* `ipxe-qemu`: on QEMU 11.0.3 `info qtree` names
    `efi-virtio.rom` as the ROM of `-M virt`'s default `virtio-net-pci` card and gives
    `virtio-blk-pci` none, so the workflow comment's old reason was a mis-attribution there. Not
    checked on the runner's QEMU; the package stays.
  - *D14, fixed* — the nits, including `sef_publish_to_ds(name)` in two documents. Left and listed
    as debt 12: two contradictions that predate the slice.
  - *Beyond the findings.* The same sentence that had the wrong `sef_publish_to_ds` signature also
    put the publisher's endpoint in the `DS_PUBLISH` payload, in both documents; DS takes it from
    `m_source`. `servers-and-drivers.md`, read end to end, also had a rule list headed "Three rules"
    over six, and gained the `BDEV_MINOR_NONE` rule and the gate's home.

  *Verification.* Host tests: `kernel-shared` 217, `server-rt` 61, `mfs` 148, `memory` 8,
  `virtio-blk` 86 (79 before the round), all passing; the three clippy gates, `cargo fmt --check`
  and `shellcheck` clean. Miri: `cargo miri test -p minixrs-driver-rt` on the final tree, 2026-11-22
  — 113 passed, 0 failed, 134.75 s by Miri's own (virtual) clock, the same figure Task 2 recorded,
  and about 46 s of wall clock. Boots, one at a time, 450 s, QEMU 11.0.3, **musl flavour**:
  `check-boot-log.sh disk` — PASS, all 165 marker checks; the reboot of that image, 60 s: PASS, all
  7 marker checks (`persist gen=1`); the ramdisk boot: `check-boot-log.sh ramdisk` — PASS, all 165
  marker checks; `diff-backends.sh`: PASS (`--self-test` too). `bdev.deny ok n=10` on both full
  boots.

  *What the boots found.* The round's boots set `MINIXRS_SDK=/nonexistent`; none of the run's had.
  With the SDK installed an unset variable selects the SDK flavour, so every log of Tasks 5–11 — and
  the coordinator's re-measurement — shows `hello` at 46 696 bytes, where CI builds the 234 280-byte
  musl one. On the musl flavour the last required marker is at 88.03% of the 450 s disk log (about
  396 s) and 57.87% of the ramdisk log (about 260 s), against about 248 s and 178 s on the SDK
  flavour. *Ruling (36):* no timeout was changed on one reading. CI's 900 s is 2.27x for the disk
  boot (3.46x for the ramdisk boot) and the local 450 s is 1.14x; the phase file's debt 13 owns the
  decision.

**Where the plan or the spec was wrong, and which was corrected.**

- *Plan wrong, plan corrected:* the Marker text block (no `gpt rc=`, label checks out of order,
  placeholder failure forms); the boot timeout in Global Constraints; the file lists of Tasks 3, 7
  and 9.
- *Spec wrong, spec corrected:* P10's "existing `cargo test` job" (there is none); §1's "`fs/mfs` …
  unchanged" (P6 itself adds a probe to it); P1's "padded to a multiple of 8" (a whole mebibyte);
  P6's probe of "the `55 AA` signature" (not enough — the ESP has it too); P8's "every failure
  prints" (not a flush lost to a broken device); P5's list of refusals (two more exist).
- *Spec's predictions wrong, reality recorded:* §3 rows 3, 4, 5, 8 and 10, and the damaged-CRC boot.
- *Found by the review round, both corrected:* the plan's Global Constraints gave boot commands that
  measure the SDK flavour on a machine with the SDK installed; the spec's P11.3 called its readings
  the budget without saying which flavour they were. The phase file's "Paid by 6.4" on two of 6.3's
  debts said more than the evidence did.

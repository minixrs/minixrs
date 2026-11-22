# Slice 6.3 — `virtio-blk` behind BDEV + root backend selection

**Date:** 2026-11-21 **Status:** approved 2026-11-21 **Branch:** `feature/6.3-virtio-blk`
**Predecessor:** slice 6.2 (`driver-rt`, PR #71) **Tracker:**
[`docs/plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md) slice 6.3

Slice 6.3 makes virtio-blk serve `BDEV_READ` and `BDEV_WRITE` from its request queue under real
completion interrupts, and lets MFS mount its root from it when a disk carrying a minix.rs root is
attached. MFS's filesystem code and VFS do not change; the ramdisk boot stays green as the second
backend (H7).

The phase decisions `H1…H12` are locked and are not re-decided here. Slice-local decisions are
labelled `B1…B11`. One of them, B1, **amends H5**, because H5 as written cannot be made
deterministic; it is marked **(finding)** and listed first in [§5](#5-rulings-to-check).

The slice changes no kernel behaviour and no ABI value (D8): no new kernel call, selector, request
number, endpoint or errno.

---

## 1. What exists, and what 6.3 changes

| Component                  | Today                                                         | After 6.3                                                                                    |
| -------------------------- | ------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `drivers/virtio-blk`       | brings the device up, publishes nothing, answers `ENOSYS`     | publishes `vblk` when it has a device; checks for a root; serves `BDEV_READ` / `BDEV_WRITE`  |
| `drivers/driver-rt`        | `VirtQueue<'a>` over `&mut [u8]`; `add` takes raw `(pa, len)` | queue over `NonNull`; typed DMA handles; a queue-fault signal; `notify` carries its barrier  |
| `server-rt`                | `cdev.rs` (shared CDEV parse)                                 | `+ bdev.rs`: the BDEV parse **and** request validation, moved from `drivers/memory`          |
| `drivers/memory`           | owns `bdev.rs` parse + validate + geometry                    | keeps geometry; parse and validate come from `server-rt::bdev`                               |
| `fs/mfs` (`main.rs` only)  | resolves `memory` through DS                                  | resolves `vblk` first, probes it, falls back to `memory` (B2). Library half untouched        |
| `kernel-shared`            | `BDEV_MINOR_RAMDISK`                                          | `+ BDEV_MINOR_ROOT`, an alias with the same value (H6)                                       |
| `kernel/build.rs`          | packs the `rootfs` blob into the archive                      | also writes the same bytes to `rootfs.img` beside the kernel ELF (B9)                        |
| `tools/qemu-run.sh`        | always attaches a blank 2560-sector disk                      | `MINIXRS_ROOT=disk` (default) attaches a fresh copy of `rootfs.img`; `ramdisk` attaches none |
| `tools/check-boot-log.sh`  | one expected file, one forbidden file                         | common files plus a per-backend pair, selected by a required argument (B10)                  |
| `tests/`                   | `qemu-boot.{expected,forbidden}`                              | `+ qemu-boot.{disk,ramdisk}.{expected,forbidden}`                                            |
| `.github/workflows/ci.yml` | `qemu-smoke` boots once                                       | boots twice, disk then ramdisk, each against its own marker set                              |
| `kernel/`                  | —                                                             | comments only (`notify.rs`'s "no boot exercises this arm")                                   |

---

## 2. Decisions

### B1 — `vblk` publishes when it has a device, and answers `ENXIO` when it has no root **(finding, amends H5)**

H5 says virtio-blk publishes to DS *only when the device carries a minix.rs root*, and MFS falls
back when the name is absent. Finding out whether the device carries a root takes two reads, each a
completion interrupt the driver must block for. While it is blocked, MFS — next in packing order —
runs, asks DS for `vblk`, finds nothing, and mounts the ramdisk. H5's design therefore loses the
race it relies on, on every boot. DS has no blocking lookup and a retry loop in MFS would be
polling.

So the publish and the verdict are separated:

- In `init_fresh`, `vblk` calls `GET_VIRTIO_MMIO`. If the kernel assigned it a device it publishes
  `vblk` to DS, before anything can block; if not (`ENOENT`), it publishes nothing. This is the same
  packing-order guarantee `memory` has, and H2's "an absent device publishes nothing" survives.
- After `sef_startup`, bring-up and the root check run (B6). Only then does the driver enter its
  receive loop.
- A driver with a device but no root — bring-up failed, or the labels did not match — answers
  **`ENXIO` to every BDEV request**. That is H6's definition read literally: minor 0 is "the root
  filesystem this driver serves", and there is none.

MFS's first SENDREC to `vblk` blocks in the kernel until the driver reaches `receive`, which is
after the verdict. The DS lookup cannot race; the request cannot be answered early.

*Rejected:* the root check inside `init_fresh` (still blocks before the publish, same race, and
6.2's rule keeps bring-up out of that callback); publishing unconditionally (contradicts H2 for no
gain); a `DS_RETRIEVE` retry loop in MFS (polling, and "absent" never terminates it).

### B2 — MFS selects its device with one zero-length read

MFS's prologue becomes:

1. `DS_RETRIEVE("vblk")`. A miss is the healthy no-disk path and prints nothing.
2. On a hit, grant the block buffer to that endpoint and send `BDEV_READ(minor 0, len 0)`. The
   shared validation (B5) answers a zero-length read `0` on a live minor and `ENXIO` on a missing
   one, with no device I/O. `0` selects `vblk`.
3. Anything else prints `bdev.root vblk rc=<rc> fallback=memory` and falls through to today's
   `memory` lookup, unchanged, including its `bdev.ds FAIL … fallback=` line.

The selected device is named on **one** line, the existing one with a name added: `[diag mfs]
bdev.ds ok name=vblk ep=…` or `name=memory ep=…`. That is the single line the two boots' MFS output
differs by. Every later request uses `BDEV_MINOR_ROOT`.

The change is in `main.rs`'s prologue and `Blocks`' endpoint field name. No FS-band handler, no
library module and no VFS line moves. On fallback MFS revokes the grant it issued to `vblk`, so a
driver that declined holds nothing over the block buffer; the fallback boot peaks at five of the
eight `GRANT_SLOTS`.

### B3 — one request in flight, waited for with a source-filtered receive

`serve()` in the driver handles one BDEV request end to end:

1. build the request in the DMA run (header, data page, status byte preset to `0xFF`);
2. `add` the three-descriptor chain, `notify`;
3. `ipc_receive(HARDWARE)` — blocked, not polling — then, in `driver-rt`'s documented order: read
   `InterruptStatus`, acknowledge exactly those bits, `pop_used` until `None`, `rearm`;
4. repeat 3 until the request's own head has come back.

Receiving from `HARDWARE` alone is what makes "one in flight" need no queue in the driver: a second
client's request stays queued in the kernel as a blocked sender, and an RS ping stays pending, until
the main loop's `sef.receive` runs again. A device that never completes hangs the driver here, with
its client in SENDREC behind it; RS's unanswered heartbeat is the only report. There is no timeout
because no user-space driver holds an alarm yet.

Bring-up's root check (B6) uses the same `serve()` path, so the interrupt path is proved before the
first client request and by the same code.

A `HARDWARE` notification that arrives in the main loop, with nothing in flight, gets the same
acknowledge / walk / rearm and is otherwise ignored (a config-change interrupt is legal). A *used
element* arriving with nothing in flight is a queue fault (B7). 6.2's `step=irq-extra` rule is
deleted with the idle driver it described.

### B4 — eight sectors per block; a partial write is read-modify-write

The device's unit is the 512-byte sector; BDEV's is the 4096-byte block. Block `b` is sectors `8b …
8b+7`, computed with `checked_mul`. Every device transfer is one whole block through the one bounce
page (H3).

- **Read:** device → bounce, then `SAFECOPY_TO` the client's `len` bytes.
- **Write, `len == 4096`:** `SAFECOPY_FROM` into the bounce first, then the device write. A bad
  grant is relayed verbatim and the device is never touched.
- **Write, `0 < len < 4096`:** read the block, `SAFECOPY_FROM` over its first `len` bytes, write it
  back. `memory` gives this request those semantics (the rest of the block survives), so `vblk`
  must, at the cost of two round trips.
- **`len == 0`:** answered `0` with no grant use and no device I/O, as `memory` does.

The device's usable size is `capacity / 8` blocks; a capacity that is not a multiple of 8 sectors
fails the root check (`root FAIL geometry`).

### B5 — BDEV parse and validation move to `server-rt::bdev`

The proof is that MFS's denial battery reads identically on both backends, so both drivers must
refuse the same requests with the same errno in the same order. Two copies of `validate_read` would
drift. `parse_read` and `validate_read` (with their tests) move to `server-rt::bdev`, taking the
device's block count; `validate_geometry` stays in `drivers/memory`, being about the ramdisk window.
The precedent is `server-rt::cdev`, which shares the parse — BDEV shares the validation too because,
unlike CDEV's, it has no per-driver part. `memory` keeps not depending on `driver-rt`.

### B6 — the root check is `memory`'s, through the queue

After a successful bring-up the driver reads block 0 and the last block and applies exactly the
checks `memory`'s `selfcheck` does: `IMAGE_LABEL`, the header's block count equal to the device's,
`IMAGE_TAIL_LABEL`. It prints `root ok blocks=<n> tail=1` (mirroring `ramdisk ok blocks=256 tail=1`)
or `root FAIL <what>`. It decodes no superblock and does not depend on `minixrs-mfs`.

This read is also what discharges 6.2's debt 8: the cookie proved the run's first page is the memory
the kernel reported; the label arriving in the bounce page through the device's DMA proves it for
the page the device actually writes.

The 6.2 marker's `capacity=` becomes 2048 (`ROOTFS_IMAGE_BYTES / 512`), no longer deliberately
unlike the ramdisk's length. Content, not size, is now what tells the two apart.

### B7 — `EIO`, and a broken device

- The status byte is preset to `0xFF`. After completion, `0` is success; `IOERR`, `UNSUPP`, or a
  byte the device never wrote is **`EIO`** — the errno the conventions reserved for this.
- `pop_used` reports a faulty device instead of skipping silently (6.2 debt 4): a used id out of
  range or not in flight, or a completion with nothing in flight, is a `QueueFault`. The driver then
  writes `FAILED`, prints `virtio FAIL step=io …` once, and answers `EIO` to every later request.
- Errno summary for a well-formed request: no root `ENXIO`; device failure `EIO`; a refused grant or
  unmapped buffer relayed verbatim; everything else as B5.

### B8 — `driver-rt` pays 6.2's debts 4, 5 and 6

- **Typed DMA handles (debt 5).** A new `dma.rs`: `DmaRegion` is built by one `unsafe` constructor
  from the kernel-reported run, whose contract is that the memory is mapped for the life of the
  process and that no Rust reference to it ever exists; `DmaRegion::buf(offset, len)` hands out
  bounds-checked `DmaBuf`s. `DmaBuf` carries volatile `read_into` / `write_from`, which is how the
  driver touches header, status and bounce bytes without its own `unsafe`.
- **Queue memory (debt 5).** `VirtQueue` holds a `DmaBuf`, not `&mut [u8]`: the device writes that
  memory, so a unique reference was the wrong contract once the queue outlives bring-up — and 6.3
  keeps it for the life of the process. `VirtQueue::add` takes `DmaBuf`s and
  `Transport::queue_enable` takes the `VirtQueue`, so safe code can no longer aim the device at an
  arbitrary physical address and neither function needs to be `unsafe`: every address the device is
  given derives from the one `unsafe` constructor.
- **Notify barrier (debt 6).** `MmioTransport::notify` issues the fence itself before the
  `QueueNotify` store, so the ordering does not depend on what the caller did last.
- **Queue fault (debt 4).** As B7.

`driver-rt`'s `unsafe` is confined to `mmio.rs` and `dma.rs` (as built, `queue.rs` holds none once
it takes handles); Miri runs over the crate. The mock transport is extended to model a device that
completes, fails and misbehaves — it must be able to say no.

### B9 — the disk image

`kernel/build.rs` writes the bytes `build_rootfs` returns to `rootfs.img` in the directory that
holds the kernel ELF, so the runner finds it as `$(dirname "$1")/rootfs.img` and the image always
matches the kernel being booted.

`qemu-run.sh`:

- `MINIXRS_ROOT=disk` (default): copy `rootfs.img` to `target/vblk-root.img` **on every run** and
  attach that. The boot writes to it, so a reused file would make the second run a different test.
  Nothing persists; persistence is 6.4's proof.
- `MINIXRS_ROOT=ramdisk`: no virtio-blk device at all.
- `MINIXRS_VBLK_IMAGE=<file>`: attach that file instead, for the refusal boots.

The blank image and `VBLK_IMAGE_SECTORS` go.

### B10 — marker files and CI

`tests/qemu-boot.{expected,forbidden}` stay as the common set. Two pairs are added:

- `qemu-boot.disk.*` — requires the `[virtio]` / `[devmap] vblk` / `[dma] vblk` lines, `[diag vblk]
  virtio ok … capacity=2048 …`, `[diag vblk] root ok blocks=256 tail=1` and `[diag mfs] bdev.ds ok
  name=vblk`; forbids `bdev.ds ok name=memory`, `bdev.root vblk` and `[diag vblk] no device`.
- `qemu-boot.ramdisk.*` — requires `[diag vblk] no device` and `bdev.ds ok name=memory`; forbids
  `name=vblk`.

The forbidden halves are what close H5's silent fallback. `check-boot-log.sh` takes the backend as a
**required** argument, so no caller can check the common set alone and call it a pass.

"Byte-identical" is checked mechanically by a new `tools/diff-backends.sh <disk-log> <ramdisk-log>`.
It takes the `[diag mfs]` lines and the `[diag vfs]` lines of each log as two separate streams,
keeps the first occurrence of each line in order, and fails unless the VFS streams are identical and
the MFS streams differ in exactly the `bdev.ds ok name=` line. The two streams are compared
separately because the servers interleave differently from boot to boot, and by first occurrence
because VFS prints one `pm-exit reset` line per exit for as long as the boot runs (both measured,
B11.2). A line is matched wherever it starts, since a tick character can precede it.

CI's `qemu-smoke` keeps its name (branch protection requires it) and runs the two boots in sequence,
then the diff. See B11 for the budget.

### B11 — measure first

Each measurement is taken at the first point it can be, and before anything is built on its answer.
Only the control (2) and the merge-base half of the budget (1) can be taken before the driver serves
a request, and those are Task 0 of the plan; the other items need a disk boot and belong to the task
that first produces one.

1. **Budget.** The disk boot's last required marker as a fraction of a fixed-timeout log, against
   the merge base ([`ci.md`](../../conventions/ci.md)'s method). This decides the per-boot timeout,
   the job's `timeout-minutes`, and the local iteration timeout (6.2 debt 7).
2. **Determinism control.** Two boots of the *same* backend through `diff-backends.sh` must come out
   identical. **Measured 2026-11-21 at the merge base:** the raw interleaved stream is not (VFS and
   MFS lines change order, and the count of VFS's per-exit lines follows the run length); each
   server's first-occurrence stream is, which is the scope B10 states.
3. **Which `deliver_irq` arm completions take.** 6.1 left the immediate-delivery arm unexercised. A
   temporary trace counts both arms during a disk boot.
4. **Two assumptions about QEMU and the kernel:** `SYS_SAFECOPY` into the DMA window works as it
   does into the ramdisk window; and a `readonly=on` drive answers a write `VIRTIO_BLK_S_IOERR`
   rather than refusing `DRIVER_OK`.

---

## 3. Verification

**Host tests** (`cargo test`, and Miri for `driver-rt`):

| Behaviour                                                      | Where            |
| -------------------------------------------------------------- | ---------------- |
| block → sector arithmetic, including overflow                  | `virtio-blk`     |
| request header encoding; status byte → errno, `0xFF` → `EIO`   | `virtio-blk`     |
| the partial-write plan (read, overlay, write)                  | `virtio-blk`     |
| root-check decisions, each failure                             | `virtio-blk`     |
| `DmaRegion::buf` bounds; `add` with typed buffers              | `driver-rt`      |
| `QueueFault` for out-of-range, stale and unsolicited used ids  | `driver-rt`      |
| the moved BDEV parse/validate tests, unchanged                 | `server-rt`      |
| MFS's device-selection decision (`0`, `ENXIO`, other, DS miss) | `fs/mfs` library |

**Boots.** Both pass `check-boot-log.sh` with their backend, and `diff-backends.sh` passes across
them. The full MFS battery — read, `fs.write ok`, create/truncate, the `/etc/holey` hole probe — is
in the common set and therefore required of both.

**Refusal boots**, run once and recorded in the phase file:

| Boot                                  | Expected                                                                       |
| ------------------------------------- | ------------------------------------------------------------------------------ |
| a blank disk (`MINIXRS_VBLK_IMAGE`)   | `root FAIL label`; `bdev.root vblk rc=-6 fallback=memory`; ramdisk root        |
| the root image attached `readonly=on` | mount and reads pass; the first write surfaces `EIO` in the `fs.write` markers |

*Observed (2026-11-21).* Blank disk: as predicted, and the common marker set passes on the ramdisk
root. `readonly=on`: QEMU lets the driver reach `DRIVER_OK` without `VIRTIO_BLK_F_RO` negotiated;
mount and every read pass; each write fails per request with no `step=io` line (the device is not
broken). The prediction's wording was loose: `fs.write` prints `FAIL short` with no errno. The lines
that show the errno are `[diag mfs] bdev.partial FAIL write rc=-5` and `fileio: seek-hole FAIL open
rc=-1 errno=5`. The image is byte-identical afterwards.

**Mutations**, each applied, observed and reverted:

| # | Mutation                                          | Predicted                                                          |
| - | ------------------------------------------------- | ------------------------------------------------------------------ |
| 1 | drop `rearm`                                      | disk boot hangs at the second request; `root ok` missing           |
| 2 | sector = block (not `8 *`)                        | `root FAIL tail label`                                             |
| 3 | `vblk` skips the DS publish                       | disk boot: `name=vblk` missing **and** forbidden `name=memory` hit |
| 4 | MFS asks for `memory` first                       | same pair as 3                                                     |
| 5 | drop the no-root `ENXIO` gate                     | blank-disk boot: `mount FAIL` instead of the fallback line         |
| 6 | treat every status byte as success                | `readonly=on` boot: no `EIO`                                       |
| 7 | break `deliver_irq`'s immediate arm               | depends on B11.3 — a hang if completions take it, else nothing     |
| 8 | skip the read in a partial write                  | host test; a marker only if MFS issues partial writes (to observe) |
| 9 | drop `notify`'s fence; acknowledge after the walk | expected to move nothing under TCG — recorded as such              |

*Observed (2026-11-21), every boot with a healthy IPC counter:*

| # | Observed                                                                                                                                                                                                                                         |
| - | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1 | As predicted. `virtio ok`, then no `root` line; MFS prints only `sef ready`; 56 markers missing, none forbidden.                                                                                                                                 |
| 2 | As predicted: `root FAIL tail label`, then the fallback. Three `blk.rs` host tests fail too.                                                                                                                                                     |
| 3 | As predicted: exactly that pair, everything else passing.                                                                                                                                                                                        |
| 4 | As predicted: the same pair.                                                                                                                                                                                                                     |
| 5 | As predicted: `bdev.ds ok name=vblk`, `mount FAIL rc=-22`, no fallback line.                                                                                                                                                                     |
| 6 | No `errno=5` anywhere; the write probes fail on read-back instead (`fs.write FAIL read off=0`, …). **`fs.trunc ok n=0` still passes** — a dropped truncate is invisible to its marker. Three `blk.rs` host tests fail too.                       |
| 7 | A hang: completions take the immediate arm (B11.3). Disk boot identical to row 1's; ramdisk boot and TTY's `irq ok rounds=2` unmoved. **Rows 1 and 7 are indistinguishable by markers** — the driver prints nothing between its two reads.       |
| 8 | **First run: nothing moved, anywhere** — no host test reaches `do_write`, and MFS issues no partial write that stores. The `bdev.partial` probe was added for it; second run: `bdev.partial FAIL clobbered` on the disk boot, ramdisk boot `ok`. |
| 9 | Nothing moved, either half, as expected. Both orderings rest on reading alone.                                                                                                                                                                   |

Rows 7–9 were to be recorded honestly whichever way they came out; row 8's prediction was the wrong
one.

---

## 4. Documentation owed in the same PR

- **`book/`**: the drivers overview and virtio chapters (the request path, the interrupt order in
  use, the trust model unchanged); the MFS chapter (device selection); build-and-boot pages (the two
  backends). Each touched chapter re-read end to end.
- **`docs/conventions/`**: `servers-and-drivers.md` (block drivers: `EIO` is now real, the shared
  validation, `driver-rt`'s third `unsafe` module, the DS chain's `vblk < mfs` link now
  load-bearing, the no-root `ENXIO` rule); `testing-and-markers.md` (per-backend marker files);
  `build-and-boot.md` and `ci.md` (two boots, `MINIXRS_ROOT`, the measured budget).
- **`docs/plans/phase-6-virtio.md`**: H5's operative paragraphs **rewritten** per B1; 6.2's debts
  4–8 marked paid; 6.3's measurements, refusal boots and mutation record; the box checked.
  `docs/plan.md`: the box.
- **In-tree prose falsified by this slice**: "`EIO` stays reserved for Phase 6" (several copies),
  `notify.rs`'s unexercised-arm comment, `qemu-run.sh`'s and `qemu-boot.expected`'s blank-disk
  commentary, the driver's and `driver-rt`'s crate docs. Found by grep in the last task, not from
  this list.

---

## 5. Rulings to check

| # | Ruling                                                                                          | If wrong                                                                                                    |
| - | ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| 1 | **B1 amends H5:** publish on device, `ENXIO` on no root, MFS probes                             | H5 as written needs a blocking DS lookup — a DS change and a new request; MFS's prologue is rewritten again |
| 2 | B5: BDEV validation shared in `server-rt`                                                       | move two functions back; the two drivers' refusals must then be kept identical by hand                      |
| 3 | B3: source-filtered receive, no queue, no timeout                                               | a second BDEV client that must not wait needs a pending-request slot in the driver                          |
| 4 | B4: partial writes are read-modify-write rather than `EINVAL`                                   | refusing them changes `memory` too, and the BDEV contract in `abi.md`                                       |
| 5 | B8: `add`, `VirtQueue::new` and `queue_enable` take typed DMA handles; one `unsafe` constructor | API churn in `driver-rt` before 6.6 adds the second consumer                                                |
| 6 | B9: the image is written beside the kernel ELF by `kernel/build.rs`                             | a build script writing outside `OUT_DIR`; the alternative is a host tool the runner invokes                 |
| 7 | B10: CI runs the boots in sequence in one job, not as a matrix                                  | roughly 10 more minutes per run; a matrix renames the required check and needs a branch-protection edit     |
| 8 | B10: `check-boot-log.sh`'s backend argument is required                                         | every documented invocation changes; an optional argument would let the common set pass alone               |
| 9 | `BDEV_MINOR_ROOT` lands now, not in 6.4                                                         | one additive constant earlier than H6 scheduled it                                                          |

---

## 6. Out of scope

- Flush, partitions, GPT, minors other than 0, and persistence across boots — 6.4.
- A queue deeper than one request, `EVENT_IDX`, indirect descriptors (H3, H4).
- Any change to VFS, to MFS's FS-band handlers, or to the kernel's behaviour.
- A request timeout or device reset path (6.2 debt 1 stays owed).
- The two Phase 5 write-back orderings, which stay on
  [`phase-6-prep.md`](../../plans/phase-6-prep.md).

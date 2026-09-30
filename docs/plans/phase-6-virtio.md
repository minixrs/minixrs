# Phase 6: VirtIO Drivers — design + slice plan

Produced by the chunk-4 design session ([`phase-6-prep.md`](phase-6-prep.md)), 2026-09-30. Every
design decision below is **locked** (decided with rationale, alternatives recorded); the slice list
is the working decomposition. Decisions are labelled `H1…H12` — distinct from Phase 5's `D1…D13`,
chunk 2's `V1…V14` and chunk 3's `I1…I13`, all of which stay in force and are cited by their own
labels.

> **Status convention.** A slice's status is a GFM checkbox — `- [ ]` not started, `- [x]` done —
> checked by the PR that does the work, in that same PR. The first unchecked box in plan order is
> the next slice. Full rule: [`docs/conventions/git-and-prs.md`](../conventions/git-and-prs.md).

**Milestone:** a QEMU **window** boot. Firmware, Limine and the kernel load from one GPT virtio
disk; MFS mounts the MinixFS partition on that same disk through virtio-blk, under real completion
interrupts; init execs `/bin/hello` from it; console output is rendered on the framebuffer; and a
key typed into the window travels virtio-keyboard → TTY → `CDEV_READ` → `read(0)`. virtio-console
and virtio-net each pass a round-trip proof. The serial console keeps working throughout — it is the
CI path and every boot marker stays on it.

This is wider than the five crate-path boxes [`../plan.md`](../plan.md) carried before this session:
the framebuffer console and the keyboard driver were added here, and all of it sits inside the
milestone bar rather than after it.

## Status

- [ ] **6.1** — `SYS_IRQCTL` + the kernel idle path + `HARDWARE` NOTIFY
- [ ] **6.2** — `driver-rt`: virtio-mmio transport, virtqueues, DMA window, reserved driver slots
- [ ] **6.3** — `virtio-blk` behind BDEV + root backend selection
- [ ] **6.4** — `tools/mkimage`: one GPT disk, partitions as minors — **sub-milestone: disk root**
- [ ] **6.5** — TTY RX: PL011 receive interrupt + `CDEV_READ` + a VFS that does not block on it
- [ ] **6.6** — `virtio-console` as a second CDEV backend
- [ ] **6.7** — framebuffer console: Limine framebuffer → TTY
- [ ] **6.8** — `virtio-input` keyboard driver — **sub-milestone: non-headless boot**
- [ ] **6.9** — `virtio-net`, packet I/O only — **milestone; Phase 6 complete**
- [ ] **6.10** — *stretch:* `virtio-gpu` under the same TTY renderer

---

## Where Phase 5 left the ground

Facts the design rests on, each verified against `main` at session time.

- **`SYS_IRQCTL` is an `ENOSYS` stub** (`kernel/src/system/stubs.rs`); its design is complete and
  reviewed in
  [`2026-09-29-sys-irqctl-design.md`](../superpowers/specs/2026-09-29-sys-irqctl-design.md). The
  kernel has no idle path and takes IRQs only from EL0 (I11).
- **The four virtio crates are placeholders.** `drivers/driver-rt/src/lib.rs` is 4 lines;
  `drivers/virtio-{blk,console,net}/src/main.rs` are 5 lines each. All four are workspace members.
  There is no `virtio-input` or `virtio-gpu` crate.
- **No proc slot exists for a new driver.** Boot slots `0..=10` are all assigned (`PM` … `INIT`),
  the demo stubs hold `11..=14`, and PM's `FORK_POOL_BASE` is `NR_BOOT_PROCS + NR_STUB_PROCS` = 15.
  `PFS_PROC_NR` (8) has a slot and no ELF. RS is a heartbeat monitor: it cannot fork, exec, or grant
  a privilege, so it cannot start a driver.
- **Nothing gives a process a physical address.** `mm::frame` exposes `alloc_frame` / `free_frame` —
  one frame at a time, no contiguous allocation — and no kernel call reports the PA behind a user
  VA. A virtqueue needs both.
- **Device memory is boot-pre-mapped, and only that.** `load_boot_server` maps the PL011 page into
  TTY at `TTY_UART_VA` and the ramdisk into `memory` at `RAMDISK_VA`; `do_vmctl`'s `pt_map` answers
  `device: false` permanently (D1). `kernel-shared::uspace` documents that a kernel-owned window
  placed **above** the device window needs no VM edit, because `USER_REGION_LIMIT` derives from the
  lowest one.
- **`SYS_GETINFO` already carries per-driver boot facts**: `GET_RAMDISK` (64) reports `(va, len)` to
  `MEM_PROC_NR` alone. That is the precedent H2, H3 and H9 extend.
- **MFS and VFS find the block driver through DS**, under the name `memory`, and fall back to
  `boot_endpoint(MEM_PROC_NR)` when the lookup fails. Neither names a device number.
- **Every request band below `NOTIFY_MESSAGE` (`0x1000`) is taken**: PM `0x700`, VFS `0x800`, FS
  `0x900`, BDEV `0xA00`, CDEV `0xB00`, VM `0xC00`, SEF `0xD00`, DS `0xE00`, SCHED `0xF00`.
- **The kernel parses no command line**, and `tools/limine.conf` passes none.
- **`tools/qemu-run.sh` boots headless from a directory**: `-drive
  file=fat:rw:…,format=raw,if=virtio` plus `-display none`. On `-M virt`, `if=virtio` is
  virtio-blk-**pci** — the firmware reads it, and nothing in minix.rs does. There is no
  `tools/mkimage`.
- **The Limine request block has no framebuffer request** — base revision, HHDM, memmap, paging
  mode, stack size and kernel address only.
- **`NR_IRQ` is 8 per process**; QEMU `virt` has 32 virtio-mmio transport slots at `0x0A00_0000 +
  n * 0x200`, SPI `16 + n`, INTID `48 + n` (I9).
- **`CDEV_READ` exists and TTY refuses it** from its unknown-request arm (5.11); VFS already routes
  a console `read()` there.
- **The qemu-smoke boot budget is 600 s**, raised twice during the Phase 5 stretch slices; see
  [Boot budget](#boot-budget).

Three things the design depends on that are **not** verified, because only running the slice can
verify them. Each is a measure-first step of the slice named, in the manner of I11's two
measurements:

1. **Which transport slot QEMU gives a device, and whether several share a page** (6.2). Slots are
   `0x200` apart, so eight fit in one 4 KiB page; if QEMU fills them from one end, every virtio
   driver's registers are in the same page and H2's mapping cannot isolate them from each other.
2. **Whether edk2 boots from a modern virtio-mmio disk** (6.4). H4 forces version 2; the firmware
   has to read the ESP from the same device.
3. **Whether the ramfb framebuffer is usable from EL0** (6.7) — that Limine reports it, that it
   survives `ExitBootServices`, and which mapping attributes `map_page_in`'s RAM/device invariant
   permits for memory that is RAM-backed but absent from the usable map.

## Locked design decisions

### H1. Four driver boot slots, reserved together in 6.2

`VBLK_PROC_NR` 11, `VCON_PROC_NR` 12, `VINPUT_PROC_NR` 13, `VNET_PROC_NR` 14 are appended after
`INIT`. `NR_BOOT_PROCS` goes 11 → 15, the stubs move to `15..=18`, and the fork pool base to 19,
leaving 13 fork slots under `NR_SERVED_PROCS = 32`. Each slot gets its `BootEntry` row, priv slot
and `ipc_to` wiring in 6.2; a later slice only adds the crate, its `user.ld`, the `kernel/build.rs`
row and its markers. This is the shape Phase 4 left for Phase 5 — VFS, MEM, TTY and MFS all had
wired slots before they had ELFs.

Reserving all four at once means the stub markers (`[as] stub A nr=11` …) and every
fork-pool-derived number in `tests/qemu-boot.expected` change **once**. The constant names above are
the working names; 6.2's plan fixes them.

*Rejected:* a slot per slice (four rounds of marker churn); RS-started drivers (MINIX-authentic, but
needs RS fork/exec, dynamic `SYS_PRIVCTL` allow-lists and VM-mediated device mapping before the
first register read); reusing the unloaded PFS slot (the name lies until Phase 7 forces the renumber
anyway). A `virtio-gpu` slot is **not** reserved: 6.10 is a stretch and can pay for its own.

### H2. The kernel probes the virtio-mmio slots at boot — amends I4 and I9

Before loading the boot servers the kernel reads `MagicValue`, `Version` and `DeviceID` from each of
the 32 transport slots through the HHDM. For each boot driver that declares a `DeviceID`, it maps
the page containing the matching slot into that driver's device window, puts that slot's one INTID
on its `Priv::irqs` allow-list, and reports `(va, intid)` through a new `SYS_GETINFO` selector gated
on the caller's proc number, as `GET_RAMDISK` is.

This amends chunk 3. I4 had the kernel map a set of slots and the driver probe them; I9 made finding
the device "slice 6.2's problem". With five virtio devices that arithmetic fails: a driver that may
probe any slot needs every slot's SPI allowed, and 32 exceeds `NR_IRQ = 8`. Probing in the kernel
keeps I4's actual rule — **the allow-list follows the mapping** — and makes the list exactly one
entry long. The cost is three register reads of virtio knowledge in the kernel; feature negotiation
and everything else stay in `driver-rt`.

Isolation between virtio drivers is page-granular at best (see the first unverified fact above). The
tracker records that honestly rather than claiming a per-driver register boundary the hardware does
not offer.

A device that is absent is not an error: the driver is told so, publishes nothing to DS, and idles.
That is what makes H5 work.

*Rejected:* driver-side probing with every slot mapped; `VMCTL_MAP_PHYS` with a per-driver PA
whitelist (D1's deferred alternative — still deferred, until a driver is started after boot).

### H3. DMA memory is a third kernel-owned window

A virtqueue's descriptor table, rings and data buffers must be at guest-physical addresses the
driver knows. At boot the kernel allocates a run of **physically contiguous** frames for each virtio
driver, maps it `RW_DATA` at a fixed VA in a new window above the ramdisk window, and reports `(va,
pa, len)` through `SYS_GETINFO`. Contiguity is cheap at boot and is asserted, not assumed.

Drivers **bounce**: a BDEV or CDEV request's payload moves between the client's grant and a DMA
buffer with `SYS_SAFECOPY`, exactly as `memory` moves it to and from the ramdisk today. The device
never sees a client's pages.

*Rejected:* a VM `alloc_contig` plus a `umap`-style kernel call (MINIX-authentic, but two new calls
and a PA disclosed on request rather than at boot); pinning and mapping the client's granted pages
for zero-copy I/O (a real optimisation with no consumer that needs it).

### H4. Modern virtio-mmio only

Version 2 of the MMIO register layout, which QEMU provides under `-global
virtio-mmio.force-legacy=false`. `VIRTIO_F_VERSION_1` is required; queues are split virtqueues; no
indirect descriptors, no `EVENT_IDX`, no packed rings. A version-1 transport is refused with a
`[diag]` line naming the flag that was forgotten — the define-and-refuse rule in
[`servers-and-drivers.md`](../conventions/servers-and-drivers.md#define-and-refuse-never-fold-into-enosys).

*Rejected:* supporting QEMU's legacy default, which is a second register layout and a second queue
setup path for a transport no current device needs.

### H5. The root backend is chosen by what is attached

virtio-blk publishes itself to DS only when H2 found it a device and the device carries a minix.rs
root. MFS asks DS for the virtio name first and the `memory` name second. So the QEMU command line
is the selector: with a virtio disk attached the root is on it, without one the root is the ramdisk.
No kernel command line, no compile-time feature, no ABI change.

The risk is a silent fallback that reports a broken virtio driver as a healthy ramdisk boot. It is
closed by the marker files, not by the code: the disk boot's expected set **requires** a root line
naming the virtio backend, so a fallback fails that boot.

*Rejected:* a `root=` key on the Limine command line (the kernel parses none, and the answer would
have to travel to MFS through a new payload field); a cargo feature (a flag day per build, and two
kernels to keep green).

### H6. One GPT disk, built by a host Rust tool

`tools/mkimage` is a host crate in the `tools/gen-c-headers` / `tools/mkfs-mfs` mould, not the shell
script [`../plan.md`](../plan.md) originally named: it has to run identically on macOS and the
ubuntu runner, and `parted` / `mtools` / `hdiutil` are what `qemu-run.sh` was written to avoid. It
writes a GPT, a FAT32 ESP holding Limine, its config and the kernel, and a MinixFS partition
produced by the existing `mkfs-mfs` library.

Partitions are **BDEV minors**: minor 0 is the whole disk, minor *n* is partition *n*. The GPT
parser is a host-tested module of `driver-rt`. The root is found by **partition type GUID**, never
by index. Any crate the tool pulls in is BSD/MIT/Apache and passes `cargo deny`.

*Rejected:* two drives — a `fat:rw:` directory for the firmware and a raw MinixFS disk for the root
— which is where 6.3 starts and deliberately not where the phase ends: the milestone is a machine
that boots from its disk.

### H7. The ramdisk stays for the whole phase

D3's contract and the prep tracker's sequencing rule both hold to the end of Phase 6: the `rootfs`
blob is packed, `memory` serves it, and the no-disk boot stays green as the second backend. Whether
to stop packing it is a Phase 7 decision, taken when nothing boots without a disk.

### H8. Keyboard to TTY is notify-then-pull

The keyboard is its own driver process (`virtio-input`), not part of TTY. When it has events it
`NOTIFY`s TTY; TTY pulls them with the existing `CDEV_READ`, which the driver answers from its
buffer without ever blocking. No new request number. The driver delivers raw key codes and TTY owns
the keymap and line discipline — so a second keyboard source is a second driver, not a second
keymap.

Both edges have to be opened in `ipc_to`, each with its reverse reply edge; the 5.4 lesson is that
every entry costs a pair of bits.

*Rejected:* a new push message on the CDEV band (a request number whose only purpose is to avoid a
`NOTIFY`); the keyboard inside TTY (MINIX's own shape, and no new slot — declined in this session in
favour of one driver per device).

### H9. The framebuffer comes from Limine, and TTY owns it

The kernel adds a Limine framebuffer request; QEMU supplies the device with `-device ramfb`, which
edk2 exposes as GOP. The kernel pre-maps the framebuffer into TTY in its own window and reports
base, width, height, pitch and pixel format through `SYS_GETINFO`. TTY writes every console byte to
the UART **and** to the framebuffer, so no serial marker moves and CI — which keeps `-display none`
but still gets a framebuffer from `ramfb` — runs the same code path a window boot does.

The text renderer is a host-tested library half of `drivers/tty`, in the `fs/mfs` lib/bin shape. The
font is a bitmap font under a BSD or MIT licence, vendored with its attribution recorded — Spleen
(BSD-2-Clause) is the working choice. Kernel messages and `[diag]` lines stay serial-only: the
screen shows what user space writes to fd 1 and 2, which is what a console is.

*Rejected:* virtio-gpu as the milestone path (nothing visible until virtqueues work, and a whole
driver between TTY and its first pixel — it is 6.10 instead); a kernel log tap onto the screen.

### H10. virtio-net gets no request band in Phase 6

Every band below `NOTIFY_MESSAGE` is allocated and there is no network stack to be a client. 6.9's
proof is driver-local. A band is allocated by the slice that brings the first consumer, as a
deliberate ABI decision about where it lives.

*Rejected:* opening a band above `0x1000` now for a protocol nothing speaks.

### H11. The transport is a trait

`driver-rt` separates the transport (register access, feature negotiation, queue setup, notify,
interrupt acknowledge) from the virtqueue and from each device. Phase 8's PCI transport is a second
implementation. Nothing else is done for x86_64 in this phase.

### H12. D8 ruling: all of this is additive

New endpoints after `INIT`, new `SYS_GETINFO` selectors, new `/dev` nodes, the `IRQ_*` constants of
I2: none moves a value the generated C headers already emit for an existing name, so none is an ABI
bump and the musl fork needs only regenerated headers. `NR_BOOT_PROCS` **does** change value and is
emitted into `com.h`; no C reads it.

This is a ruling. **If it is wrong** — if some C in the fork depends on `NR_BOOT_PROCS` or on a stub
endpoint — the cost is a coordinated fork bump and submodule bump in 6.2's PR. 6.2 checks the fork
for both before relying on it.

### Standing rules carried in, not re-decided

- **No polling driver** (I12). Every driver blocks in `receive` between requests; a driver that
  spins on a used ring passes under TCG and proves nothing.
- **Mask before EOI; acknowledge at the device, then `IRQ_ENABLE`** (I7, I12).
- **A block driver does not depend on the filesystem format**
  ([`servers-and-drivers.md`](../conventions/servers-and-drivers.md#block-drivers)). virtio-blk's
  "is this a minix.rs root" check in H5 is the device-level image header `memory` already verifies,
  not a superblock read.
- **Counts in markers are recomputed from constants**, never copied from a design document
  ([`testing-and-markers.md`](../conventions/testing-and-markers.md#markers)).

---

## Slice decomposition

Ordering rationale: **interrupts first**, because every driver after it is interrupt-driven and
I11's idle path is a precondition of the first one that blocks. **Transport second**, proved against
a device that then becomes the first real driver. **Disk before console**, because the disk root is
the phase's riskiest path and the ramdisk covers for it until it is green. **Serial input before the
framebuffer and keyboard**, because 6.5 settles how VFS waits on a console, and the keyboard reuses
the answer. **Net last**: it has no consumer and nothing waits on it.

Each slice gets its own `docs/superpowers/specs/` design and `docs/superpowers/plans/` plan; this
file links to them as they land and does not restate them.

### Slice 6.1: `SYS_IRQCTL` + the kernel idle path + `HARDWARE` NOTIFY

**Design:**
[`2026-09-29-sys-irqctl-design.md`](../superpowers/specs/2026-09-29-sys-irqctl-design.md), decisions
`I1…I13`. H2 amends I4 and I9 for slices 6.2 onward; 6.1 itself is unaffected — its one allow-list
entry is INTID 33 on TTY.

**Scope:** as the design's §1 table. The idle path (I11) may land as its own PR ahead of the call if
its two measurements say the empty run queue is reachable today — the 5.10a / 5.10b precedent for
splitting a slice whose halves prove different things.

**Proof:** `[diag tty] irq ok` after two interrupt rounds; `do_irq: unexpected INTID` forbidden; the
four mutations of I13, each observed and reverted.

### Slice 6.2: `driver-rt` — transport, virtqueues, DMA, driver slots

**Goal:** everything a virtio driver stands on, proved by one device answering.

**Scope:** H1's four slots; H2's boot probe and `SYS_GETINFO` selector; H3's DMA window; H4's
transport and split virtqueue in `driver-rt` behind H11's trait, with the queue arithmetic
host-tested; the `IrqLine` type (claim in `new`, `rearm()` for `IRQ_ENABLE`) moved into `driver-rt`,
and TTY migrated onto it. That migration owes the three things I12 lists: the dependency and its
`Cargo.toml` comment, the convention line in `servers-and-drivers.md`, and the
`drivers/driver-rt/src` watch entry in `kernel/build.rs`. `qemu-run.sh` gains a virtio-blk-device on
a raw image and the `force-legacy=false` global. `virtio-blk` is loaded as the first consumer but
serves nothing yet.

**Measure first:** unverified fact 1; H12's check of the fork.

**Proof:** virtio-blk reports the negotiated feature bits and the disk's capacity read from device
configuration space, matching the image's size; a version-1 transport produces the refusal line; the
stub and fork-pool markers carry their new numbers.

### Slice 6.3: `virtio-blk` behind BDEV + root backend selection

**Goal:** D3's promise — virtio-blk under an unchanged MFS.

**Scope:** `BDEV_READ` / `BDEV_WRITE` served from the request queue, completion by interrupt; the
bounce path of H3; `EIO` for a device-reported failure, the errno
[`servers-and-drivers.md`](../conventions/servers-and-drivers.md#block-drivers) reserved for it;
H5's DS-name selection in MFS and VFS. The disk is a **raw, whole-disk** MinixFS image — the bytes
`build_rootfs` already produces — written to a file under `target/`. No partitions yet.

**Proof:** the full MFS battery — read, `fs.write ok`, create/truncate, the `/etc/holey` hole probe
— is **byte-identical** on the two backends apart from the one line naming the backend. Two boots,
with and without the disk, each with a marker file that requires its own backend. A mutation that
drops the `IRQ_ENABLE` hangs the disk boot at its second request.

The two write-back orderings [`phase-6-prep.md`](phase-6-prep.md) carries from Phase 5 are **not**
discharged by this slice or by 6.4's persistence proof: no slice here adds `lseek` or a second
truncate consumer, which is what probing them needs. They stay owed on that tracker.

### Slice 6.4: `tools/mkimage` — one GPT disk — sub-milestone: disk root

**Goal:** the machine boots from its disk.

**Scope:** H6 — the host tool, the GPT module in `driver-rt`, partition minors in virtio-blk, root
found by type GUID. `qemu-run.sh` attaches the one image as a virtio-blk-device and drops the
`fat:rw:` directory.

**Measure first:** unverified fact 2. If edk2 cannot boot from a modern virtio-mmio disk, the
fallback is a second, firmware-only attachment of the **same** image file — recorded as a deviation,
not adopted quietly.

**Proof:** no `fat:rw:` in the QEMU command line; `/bin/hello` exec'd from the MinixFS partition; a
write to the root survives into a second boot of the same image, which the ramdisk could never show.

### Slice 6.5: TTY RX — PL011 receive interrupt + `CDEV_READ`

**Goal:** the console can be read.

**Scope:** the steady-state loop of I12 in TTY against the PL011 receive interrupt, feeding an input
buffer; a `CDEV_READ` arm. And the part the prep tracker undersold as "one new arm in TTY": a
`CDEV_READ` that arrives with nothing buffered leaves VFS blocked in `SENDREC` for as long as TTY
withholds the reply, which stalls every other file operation in the system. This slice owes VFS a
way to park a read and reply later. That design is the slice's own; what is locked here is only that
**VFS does not block on a console**, and that 6.6 and 6.8 reuse whatever 6.5 builds.

**Proof:** bytes piped to QEMU's stdin come back from `read(0)`; a second process's file read
completes **while** a console read is parked.

### Slice 6.6: `virtio-console` as a second CDEV backend

**Scope:** the driver on slot 12, one port, no multiport feature; a `/dev/hvc0` node in VFS's device
table. fd 0, 1 and 2 stay on TTY.

**Proof:** a write to `/dev/hvc0` appears on a second QEMU chardev, and bytes fed to that chardev
come back through a read of the node.

### Slice 6.7: framebuffer console

**Scope:** H9 — the Limine request, the framebuffer window, the `SYS_GETINFO` selector, the renderer
library, the vendored font and its licence note. `qemu-run.sh` gains `-device ramfb` always and a
windowed mode that drops `-display none`.

**Measure first:** unverified fact 3.

**Proof:** a QMP `screendump` of the running guest matches an image the host renders from the same
text with the same renderer library — a mechanical check that pixels reached the display, not a
marker TTY prints about itself.

### Slice 6.8: `virtio-input` keyboard — sub-milestone: non-headless boot

**Scope:** the driver on slot 13 against a `virtio-keyboard-device`; H8's notify-then-pull into TTY;
a keymap in TTY.

**Proof:** a QMP `send-key` sequence arrives as the corresponding bytes from `read(0)`, and its echo
is in the next `screendump`. In windowed mode, typing does the same.

### Slice 6.9: `virtio-net`, packet I/O only — milestone

**Scope:** the driver on slot 14; RX and TX queues; MAC from configuration space. No band (H10), no
stack.

**Proof:** a frame the driver transmits is captured by QEMU's `filter-dump`; a frame injected from
the host is reported by the driver in `[diag]` with its length and ethertype.

**Phase 6 closes here.** The closing PR checks the phase's box in `../plan.md` and records the
milestone boot.

### Slice 6.10 (stretch): `virtio-gpu`

A `virtio-gpu-device` driver presenting the same surface H9's renderer draws into, so TTY's
rendering code is unchanged. **Proof:** 6.7's `screendump` check with `ramfb` removed from the
command line. Not in the milestone bar.

---

## Boot budget

Each slice that adds device I/O measures before it raises anything, the way
[`ci.md`](../conventions/ci.md) prescribes: the last required marker's byte position as a fraction
of a fixed-timeout log, against the same number at the merge base. Two pressures are new in this
phase. H5 means **two boots** per CI run from 6.3 on, and the smoke job's wall clock is per boot.
And real completion interrupts under TCG are slower than the ramdisk's `memcpy`. If two full boots
do not fit, the answer to reach for first is a shorter marker set for the second backend, not a
larger timeout.

## Non-goals for Phase 6

- **No TCP/IP, no sockets, no network band** (H10).
- **No driver started after boot**, so no RS-driven `SYS_PRIVCTL` allow-lists and no
  `VMCTL_MAP_PHYS`.
- **No PCI transport and no x86_64** — Phase 8.
- **No shell, no job control, no pipes** — Phase 7. 6.5 and 6.8 deliver input to `read(0)` and stop.
- **No graphics beyond a text console.** No cursor addressing beyond what the renderer needs, no
  escape-sequence terminal.
- **No removal of the ramdisk** (H7).
- **No legacy virtio, indirect descriptors, `EVENT_IDX` or packed rings** (H4).

Pre-Phase-6 chunks 5 (musl syscall surface) and 6 (SDK flavor CI coverage) are independent of every
slice here and may land between any two of them; chunk 2's tooling hand-off is likewise outstanding
and tracked in [`phase-6-prep.md`](phase-6-prep.md).

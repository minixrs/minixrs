# Phase 6: VirtIO Drivers — design + slice plan

Produced by the chunk-4 design session ([`phase-6-prep.md`](phase-6-prep.md)), 2026-09-30. Every
design decision below is **locked** (decided with rationale, alternatives recorded); the slice list
is the working decomposition. Decisions are labelled `H1…H12` — distinct from Phase 5's `D1…D13`,
chunk 2's `V1…V14` and chunk 3's `I1…I14`, all of which stay in force and are cited by their own
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

- [x] **6.1** — `SYS_IRQCTL` + the kernel idle path + `HARDWARE` NOTIFY
- [x] **6.2** — `driver-rt`: virtio-mmio transport, virtqueues, DMA window, reserved driver slots
- [x] **6.3** — `virtio-blk` behind BDEV + root backend selection
- [x] **6.4** — `tools/mkimage`: one GPT disk, partitions as minors — **sub-milestone: disk root**
- [x] **6.5** — TTY RX: PL011 receive interrupt + `CDEV_READ` + a VFS that does not block on it
- [x] **6.6** — `virtio-console` as a second CDEV backend
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
- **The four virtio crates were placeholders** (6.2 filled `driver-rt` and `virtio-blk`).
  `drivers/driver-rt/src/lib.rs` was 4 lines; `drivers/virtio-{blk,console,net}/src/main.rs` were 5
  lines each. All four were workspace members. There is no `virtio-input` or `virtio-gpu` crate.
- **No proc slot existed for a new driver.** Boot slots `0..=10` were all assigned (`PM` … `INIT`),
  the demo stubs held `11..=14`, and PM's `FORK_POOL_BASE` was `NR_BOOT_PROCS + NR_STUB_PROCS` = 15
  (6.2 moved them: [H1](#h1-four-driver-boot-slots-reserved-together-in-62)). `PFS_PROC_NR` (8) has
  a slot and no ELF. RS is a heartbeat monitor: it cannot fork, exec, or grant a privilege, so it
  cannot start a driver.
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

1. **Which transport address a pinned bus name denotes** (6.2). Where a device lands is a
   command-line choice, not a measurement — H2 pins every device with `bus=virtio-mmio-bus.N` — but
   the mapping from `N` to `0x0A00_0000 + slot * 0x200` is read off the kernel's boot probe, not
   assumed. 6.2 attaches **two** devices on buses meant to be in different pages and confirms they
   are, so the check covers the case one device cannot show.
2. **Whether edk2 boots from a modern virtio-mmio disk** (6.4). H4 forces version 2; the firmware
   has to read the ESP from the same device. **Measured by 6.4 (2026-11-21, QEMU 11.0.3, Homebrew's
   edk2): it does**, and the driver brings the same device up afterwards — see 6.4's figures below.
   CI's firmware build has not been measured.
3. **Whether the ramfb framebuffer reaches the kernel** (6.7) — that Limine reports it and that it
   survives `ExitBootServices`. How it is then mapped is not a measurement: H9 decides it.

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
final; 6.2 adopted them.

**This overturned a written contract, and 6.2 amended it where it was written.** `com.rs` had
reserved `11..=14` for the stubs "regardless of the `boot-stubs` feature" and kept `FORK_POOL_BASE`
at 15 so that feature never renumbers a forked child; PM's `mproc.rs` documented the base as 15; and
`proc/table.rs` asserted `N_IMAGE == 16`, now 20. The property the contract protects survives — with
the stubs on or off, the pool stays at 19 — and the numbers in all three places were rewritten.

**A boot process's priv id is its `IMAGE` index, which H1 did not count.** The four new rows take
priv ids `16..=19`, which had been the stubs' dedicated ids, so `install_stub_privs` would have
overwritten the driver slots' privileges at boot. 6.2 moved the stub priv ids above `USER_PRIV_ID`
(now `N_IMAGE`, 20) to `USER_PRIV_ID + 1..=4`, and two compile-time asserts pin the order. The
collision is invisible stub-free and shows as a hang, not a `FAIL`, on the default boot. Any later
image row moves `USER_PRIV_ID` and is caught by the asserts.

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
the device "slice 6.2's problem". With four virtio devices (five with 6.10) that arithmetic fails: a
driver that may probe any slot needs every slot's SPI allowed, and 32 exceeds `NR_IRQ = 8`. Probing
in the kernel keeps I4's actual rule — **the allow-list follows the mapping** — and makes the list
exactly one entry long. The cost is three register reads of virtio knowledge in the kernel; feature
negotiation and everything else stay in `driver-rt`.

**Every device is pinned to a transport in its own page.** Slots are `0x200` apart, so eight share a
4 KiB page and H2 maps whole pages: two devices in one page would each be mapped the other's
registers. `qemu-run.sh` therefore attaches every virtio device with an explicit
`bus=virtio-mmio-bus.N`, choosing buses eight slots apart, and the kernel probe **refuses** to map a
page that holds **any second present device**, whether or not a driver claims it (a stray
`virtio-rng-device` would otherwise be mapped into its page-mate's driver) — a `[virtio] refuse`
line per device and an absent driver device, not a silently shared page. The per-driver register
boundary is then real as **bug containment** — a driver cannot be handed a neighbour's registers by
mistake — and it is the command line that makes it so. It is not isolation: with no IOMMU a driver's
device can DMA anywhere (debt 10).

A device that is absent is not an error: the driver is told so and idles. A **block** driver then
publishes nothing to DS, which is what makes H5 work — MFS reads the miss as "no disk". That is not
a rule for every driver: `vcon` (6.6, C4) publishes whether or not it has a device and answers
`ENXIO` without one, because nothing selects between consoles and a miss would carry no meaning.

*Rejected:* driver-side probing with every slot mapped; `VMCTL_MAP_PHYS` with a per-driver PA
whitelist (D1's deferred alternative — still deferred, until a driver is started after boot).

### H3. DMA memory is a third kernel-owned window

A virtqueue's descriptor table, rings and data buffers must be at guest-physical addresses the
driver knows. The kernel gives each virtio driver a run of **physically contiguous** frames, maps it
`RW_DATA` at a fixed VA in a new window above the ramdisk window, and reports `(va, pa, len)`
through `SYS_GETINFO`.

The run is **not** a loop of `alloc_frame`. That call takes the free list first and one frame at a
time, so successive calls are contiguous only by accident — not once anything has been freed, and
not across the end of a Limine region. The kernel instead **reserves one bump run per driver at
boot, before the first `free_frame`**, from a single usable region, and asserts the run is
contiguous. A new `mm::frame` entry point does this; nothing frees it. The run confines a *buggy*
driver's buffers, not a hostile one's descriptors: the device DMAs to whatever physical address the
driver writes (debt 10).

**One request in flight per driver.** A BDEV transfer is at most one 4096-byte block
(`BDEV_MAX_IO`), so virtio-blk's bounce buffer is one page; the device's 512-byte sectors are the
driver's translation (eight sectors per block), not a second block size anywhere above it. The run
is sized for the queue structures plus that one buffer, and a deeper queue is a later optimisation
with no consumer.

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

virtio-blk publishes itself to DS **whenever H2 found it a device**, in `init_fresh` and before
anything that can block; whether the device carries a minix.rs root is decided afterwards. A driver
with a device but no root answers `ENXIO` to every BDEV request, on every minor. MFS asks DS for the
virtio name first, **probes it with one zero-length `BDEV_READ`**, and takes the `memory` name when
the lookup misses or the probe is refused. So what is attached is the selector: with a virtio disk
carrying a root the root is on it, and otherwise the root is the ramdisk. Since 6.4 "otherwise" has
two forms. A disk **without a root** — the machine boots from it, the driver declines it, the probe
is refused — is the `MINIXRS_ROOT=ramdisk` boot CI runs. **No virtio-mmio disk at all** — the lookup
misses — was 6.3's second CI boot and can no longer be one, because a machine with no disk has
nothing to load a kernel from; it is reached by hand, with the boot disk on a transport only the
firmware reads (`MINIXRS_ROOT=nodisk`). No kernel command line, no compile-time feature, no ABI
change.

*Amended by 6.3 (its decision B1).* This paragraph first read "publishes itself to DS only when …
the device carries a minix.rs root". That cannot be built: the root check is two device reads, each
of which blocks in `RECEIVE` for a completion interrupt, and `init_fresh` — the only place a publish
can precede MFS's lookup in the DS chain — must not block. Publishing after the check lets MFS look
the name up before it exists and fall back on a healthy disk, depending on scheduling. The publish
therefore says "a device is here" and the probe asks "is it a root"; the answer to the second is
synchronous, because a `SENDREC` to a driver still in its root check simply waits.

**The selection lives in MFS alone.** VFS also looks up `memory` in DS, but as the **character**
peer behind `/dev/null` and `/dev/zero` (5.11), not as a block device. Pointing that lookup at
virtio-blk would send those nodes to the block driver. VFS's lookup does not change.

**"Carries a minix.rs root" is the image-header check `memory` already performs** — the 32-byte
header in block 0's boot area and the tail label in the reserved last zone — run against **minor 0
as H6 defines it**, not against the raw device. In 6.3 minor 0 was the whole raw image, so the check
read LBA 0. Since 6.4 minor 0 is the partition the GPT lookup selected, so the same check reads that
partition's first and last blocks — after the driver has accepted the partition table, found exactly
one partition of the root type, checked its alignment and declined a read-only device (6.4's P7).
Run against LBA 0 of a GPT disk it would find the protective MBR, decide the disk is not a root, and
fall back — which the marker files would catch, but which the design must not invite. A raw,
whole-disk MinixFS image is consequently no longer a root: it reads `root FAIL gpt signature`.

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
writes a GPT, a FAT32 ESP holding Limine, its config and the kernel, and a MinixFS partition. The
partition's bytes are `rootfs.img` — the image `kernel/build.rs` already produces with the
`mkfs-mfs` library and packs into the boot archive — copied in byte for byte: the tool takes the
file and does not call that library itself, so the disk's root and the ramdisk's are one image by
construction (6.4's P2).

**Minor 0 is the root filesystem, on every block driver.** MFS mounts and serves the filesystem
through minor 0 alone and answers `ENXIO` to a `FS_READSUPER` for any other minor; VFS mounts with
minor 0. Rather than change both, minor 0 is *defined* as "the root filesystem this driver serves":
the whole ramdisk on `memory` (as today), the whole raw image on virtio-blk in 6.3, and since 6.4
the partition **virtio-blk** finds by its **partition type GUID** — never by index. The whole disk
is minor 1 and GPT partition *n* is minor `1 + n`. `BDEV_MINOR_RAMDISK` gains a `BDEV_MINOR_ROOT`
alias with the same value, so new code says what it means and no existing value moves. MFS's FS-band
handlers and VFS are unchanged, which is D3's promise; the one request for another minor is the
`bdev.minor` boot probe 6.4 added to MFS's prologue (its P6), a read of block 0 of minor 1 that
exists so the minor table is run by a boot and not by host tests alone. (MFS's denial battery also
names a minor — `BDEV_MINOR_NONE`, the one no driver serves — but that is a request for a device
that does not exist, on purpose.)

The GPT parser is a host-tested module of `driver-rt`; the type GUID is a minix.rs-specific constant
in `kernel-shared`, shared with `tools/mkimage`. Any crate the tool pulls in is BSD/MIT/Apache and
passes `cargo deny` (as built it pulls in none: the GPT and FAT32 writers are hand-written).

*Rejected:* two drives — a `fat:rw:` directory for the firmware and a raw MinixFS disk for the root
— which is where 6.3 starts and deliberately not where the phase ends: the milestone is a machine
that boots from its disk. Also rejected: MINIX-shaped minors (0 = whole disk) with VFS passing the
root's minor in `FS_READSUPER` — more honest numbering, but MFS and VFS both change in 6.4, which
D3's "unchanged MFS" exists to avoid.

### H7. The ramdisk stays for the whole phase

D3's contract and the prep tracker's sequencing rule both hold to the end of Phase 6: the `rootfs`
blob is packed, `memory` serves it, and the ramdisk root stays green as the second backend. Since
6.4 the boot that keeps it green is the rootless-disk **fallback** boot rather than a boot with no
disk: the firmware needs a disk to read the kernel from, so every boot now attaches one. Whether to
stop packing the blob is still a Phase 7 decision.

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

**The mapping class follows from `map_page_in`'s invariant**, which asserts that a device mapping is
not usable RAM and a normal mapping is. A framebuffer outside the frame allocator's usable regions
can only be mapped **device**. If Limine reports one that overlaps a usable region, the kernel first
**removes that range from the allocator** — otherwise a normal mapping would pass the assert and
teardown would `free_frame` the screen — and then maps it device. Either way it is a device mapping,
and the renderer is written for that: aligned stores only, never an unaligned or merged access,
which Device memory faults or mishandles.

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
- **Mask before EOI; quiesce the device, then `IRQ_ENABLE`** (I7, I12). I12 was first written as
  drain, acknowledge at the device, `IRQ_ENABLE`. A virtio-mmio line is level-triggered on
  `InterruptStatus`, so for virtio the acknowledgement precedes the used-ring walk: a completion
  landing between the last pop and a late acknowledgement would be acked away and the re-arm would
  find the line low. `driver-rt`'s crate documentation is the one place the virtio order is written.
  This rule used to say the drain-first order still fitted the PL011; 6.5 found the same race there
  — a byte landing between the last `RXFE` test and a later `UARTICR` write — and TTY's receive loop
  acknowledges first as well (6.5's R4). I12's text now states the one order.
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
`I1…I14`. H2 amends I4 and I9 for slices 6.2 onward; 6.1 itself is unaffected — its one allow-list
entry is INTID 33 on TTY.

**Scope:** as the design's §1 table. The idle path (I11) may land as its own PR ahead of the call if
its two measurements say the empty run queue is reachable today — the 5.10a / 5.10b precedent for
splitting a slice whose halves prove different things.

**Proof:** `[diag tty] irq ok` after two interrupt rounds; `do_irq: unexpected INTID` forbidden; the
four mutations of I13, each observed and reverted. Under the opt-in `idle-probe` feature (I14),
`[idle] wake ok` plus I14's two idle-path mutations.

**Measured (2026-11-08):** the empty run queue is unreachable in today's boot, and `wfi` returns
with `PSTATE.I` set. So 6.1 is one PR, not split.

**Plan:** [`2026-11-08-sys-irqctl.md`](../superpowers/plans/2026-11-08-sys-irqctl.md). **Status:
done** (checked in the Status list above).

**Debt carried: CI does not exercise the idle path.** It is proved only under `idle-probe`, which CI
does not build. Correct it when the default boot first idles, at the latest Phase 7's shell: promote
`[idle] wake ok` to a required marker and delete `idle-probe`. Details in I14.

**Debt carried: three items from 6.1's execution.**

- ~~`deliver_irq`'s immediate-delivery path (owner `RECEIVE`-blocked when the line fires) is
  verified by reading only.~~ **Paid by 6.3:** virtio-blk's completions take it whenever the driver
  is already blocked in its source-filtered `RECEIVE` when the line fires — measured for the two
  root-check completions (2 immediate, 0 deferred) — and 6.3's mutation 7 disables it and hangs the
  disk boot at the root check while the ramdisk boot and TTY's `irq ok` are unmoved. A completion
  landing between `notify` and the driver's receive takes the deferred arm instead, which is equally
  correct.
- ~~6.5 must retire or rescope TTY's post-proof `irq FAIL extra` guard, since RX interrupts will
  legitimately arrive after the TX proof.~~ **Paid by 6.5:** the guard and its `extra_reported`
  field are deleted. The proof's last round now sets the receive masks and the same object
  (`IrqService`) services the line from then on; what the guard caught — a line that keeps firing
  with nothing to service — is rescoped to 64 **consecutive** notifications that drain no byte,
  which print the forbidden `[diag tty] rx FAIL storm` and leave the line masked. The threshold is a
  guess (6.5's ruling R5), held by host tests on the counter, not by a boot.
- `Priv::irqs` is never cleared on exit or exec, contradicting I4 ("the allow-list follows the
  mapping"). Unreachable today: only `USER_PRIV_ID` is assigned at runtime and no device owner
  execs. The slice that reuses dedicated slots must clear `irqs` on exit and at exec's point of no
  return.

### Slice 6.2: `driver-rt` — transport, virtqueues, DMA, driver slots

**Goal:** everything a virtio driver stands on, proved by one device answering.

**Scope:** H1's four slots; H2's boot probe and `SYS_GETINFO` selector; H3's DMA window; H4's
transport and split virtqueue in `driver-rt` behind H11's trait, with the queue arithmetic
host-tested; the `IrqLine` type (claim in `new`, `rearm()` for `IRQ_ENABLE`) moved into `driver-rt`,
and TTY migrated onto it. That migration owes the three things I12 lists: the dependency and its
`Cargo.toml` comment, the convention line in `servers-and-drivers.md`, and the
`drivers/driver-rt/src` watch entry in `kernel/build.rs`. H1's three amended texts (`com.rs`,
`mproc.rs`, `table.rs`'s `N_IMAGE` assert). `qemu-run.sh` gains a virtio-blk-device on a raw image,
pinned to its bus per H2, and the `force-legacy=false` global. `virtio-blk` is loaded as the first
consumer but serves nothing yet.

**Measure first:** unverified fact 1, with a second device attached for the measurement; H12's check
of the fork.

**Proof:** virtio-blk reports the negotiated feature bits and the disk's capacity read from device
configuration space, matching the image's size; a version-1 transport produces the refusal line; two
devices pinned into one page produce the probe's refusal line; the stub and fork-pool markers carry
their new numbers.

**Design:** [`2026-11-09-driver-rt-design.md`](../superpowers/specs/2026-11-09-driver-rt-design.md),
decisions `T1…T14`; the findings beyond H1…H12 (the stub priv ids, the any-second-device sharing
rule) are listed in its §8. **Plan:**
[`2026-11-09-driver-rt.md`](../superpowers/plans/2026-11-09-driver-rt.md). **Status: done** (checked
in the Status list above).

**Measured (2026-11-21, QEMU 11.0.3):** `bus=virtio-mmio-bus.N` is transport slot `N` at
`0x0A00_0000 + N * 0x200` (buses 0 and 8 observed, in different pages, with no reversal; the rest
inferred), so `BUS_VBLK = 0`. The HHDM reaches all 32 transports with no fault. edk2 boots Limine
from the `fat:rw:` drive with the blank disk attached and no `bootindex`; the kernel banner arrives
in about 7–8 s either way. An empty transport reads magic `0x74726976`, `Version 2` and `DeviceID 0`
under `force-legacy=false`; without that flag every slot reads `Version 1`. The musl fork reads none
of `NR_BOOT_PROCS`, `STUB_*`, `NR_STUB` or `FORK_POOL` (H12 stands). The default boot passes 148
marker checks. Full figures: the design's §5 T14 and §6.

**Amendments to phase text.** H1 gains the priv-id consequence, H2's shared-page rule is "any second
present device" (not "more than one driver's device"), and the acknowledge-before-walk order for
virtio is in the standing rules — each rewritten where it is stated above. One finding touches a
later slice: all four transport pages are taken by the four pinned devices (buses 0, 8, 16, 24), so
**6.10's `virtio-gpu` has no page of its own**. Its options are a page shared under a narrower
mapping granule (none exists below 4 KiB), dropping a device from that boot, or the PCI transport.

**Debts carried: the DMA run's lifetime, and what 6.2 left for its consumers.**

1. **A virtio driver cannot exit or exec** (owner: whichever slice first lets one exit or restart —
   no Phase 6 slice does). `do_exit` and `do_exec` halt the kernel, and `VMCTL_PT_UNMAP` answers
   `EPERM` inside an assigned driver's DMA window, because the run is ordinary RAM and teardown
   would free it while the device may still write to it. Pay with a device-reset path (`Status`
   written 0 before the run is freed), which removes the three guards. The same slice owns `do_fork`
   of an assigned driver, which would share the register page with a child that holds no assignment
   (unreachable today), and the 6.1 item that `Priv::irqs` is never cleared.
2. **Sub-word config access** (owner: 6.8 — 6.6's `virtio-console` takes no feature that makes a
   configuration field valid and reads none, so the debt passed it by). `config_read_raw` performs
   aligned 32-bit reads, valid only for 32- and 64-bit fields; virtio requires matching widths for
   narrower ones, so typed reads are needed.
3. **Queue depth** (owner: 6.9). `VirtQueue`'s bookkeeping arrays are fixed at `QUEUE_SIZE` (8); a
   deeper receive queue needs a const generic.
4. ~~**No "device broken" signal** (owner: 6.3).~~ **Paid by 6.3:** `pop_used` returns `Result<_,
   QueueFault>` (`BadId`, `NotInFlight`, `Runaway`), a fault is sticky, and the driver answers one
   with `virtio FAIL step=io` and `EIO` from then on.
5. ~~**`VirtQueue<'a>` holds `&mut [u8]` over memory the device writes** (owner: 6.3), and
   `VirtQueue::add` / `Transport::queue_enable` are safe functions taking raw physical addresses.~~
   **Paid by 6.3** with the typed handle: `driver-rt`'s `dma.rs` has one `unsafe` constructor
   (`DmaRegion::new`, over the kernel's `GET_DMA` report), the queue's memory is split off it as a
   `QueueMem` no buffer handle can overlap, every `DmaBuf` is carved from the remainder with bounds
   checked, and the queue and transport take handles — no reference into device-written memory
   exists and no physical address is typed by a driver.
6. ~~**`notify` has no barrier of its own** (owner: 6.3).~~ **Paid by 6.3:** `notify` issues its own
   `SeqCst` fence before the `QueueNotify` write. Verified by reading only — removing it moves
   nothing under TCG (6.3's mutation 9).
7. ~~**The local iteration timeout** (owner: 6.3, which adds a second boot).~~ **Paid by 6.3** by
   measurement: 300 s stays. See 6.3's figures below.
8. ~~**`[dma] vblk` does not assert `pa=`** (owner: 6.3).~~ **Paid by 6.3:** `root ok blocks=256
   tail=1` is two blocks the device wrote through the run's physical address and the driver read
   through its virtual one, so a `pa` that named other memory fails the root check.
9. **A wrong register page and a missing `VERSION_1` are caught only indirectly** (owner: 6.4, which
   first requests an optional feature). On QEMU an empty slot reads good magic, so the `DeviceID`
   check is what refuses a page one off; and a driver that omits `VERSION_1` is *not* refused by
   QEMU, so only the exact `features=` marker text and the host tests catch it. **Recorded by 6.4,
   not paid:** its mutation 8 omitted `VERSION_1` with optional features requested — the driver
   printed `virtio ok features=0x200 …`, QEMU kept the device fully working, and three host tests
   failed (two in `driver-rt`, one in `virtio-blk`). The marker text is now `features=0x100000200`.
   No new guard was added; the debt stands as described.
10. **No IOMMU: a virtio driver is in the TCB for memory integrity** (owner: Phase 8, or whichever
    platform gains an IOMMU). A queue enabled with `DRIVER_OK` carries raw physical addresses the
    device DMAs to, so the one-page register mapping, the one-entry allow-list and the 64 KiB DMA
    window contain a buggy driver, not a hostile one. Every `SRV_T` image row, the four driver rows
    included, gets the full kernel-call mask (`kernel/src/proc/table.rs`), the same statement from
    the other side. Pay with an IOMMU (or an SMMU stage-2 per driver) and descriptor addresses the
    kernel validates or translates.

### Slice 6.3: `virtio-blk` behind BDEV + root backend selection

**Goal:** D3's promise — virtio-blk under an unchanged MFS.

**Scope:** `BDEV_READ` / `BDEV_WRITE` served from the request queue, completion by interrupt; the
bounce path of H3; `EIO` for a device-reported failure, the errno
[`servers-and-drivers.md`](../conventions/servers-and-drivers.md#block-drivers) reserved for it;
H5's DS-name selection (as built: the name plus a zero-length probe the driver answers — B1), **in
MFS only** — VFS's `memory` lookup is the `/dev/null` / `/dev/zero` peer and does not move. The disk
is a **raw, whole-disk** MinixFS image — the bytes `build_rootfs` already produces — written to a
file under `target/`. No partitions yet: minor 0 is the whole image (H6), so H5's root check reads
LBA 0.

**Proof:** the full MFS battery — read, `fs.write ok`, create/truncate, the `/etc/holey` hole probe
— is **byte-identical** on the two backends apart from the one line naming the backend (as built and
measured: `[diag mfs]` and `[diag vfs]`, each compared as that server's first-occurrence stream, are
identical apart from the `bdev.ds ok name=` line; the raw interleaved logs are not byte-comparable).
Two boots, with and without the disk, each with a marker file that requires its own backend. A
mutation that drops the `IRQ_ENABLE` hangs the disk boot at its second request.

**Design:**
[`2026-11-21-virtio-blk-design.md`](../superpowers/specs/2026-11-21-virtio-blk-design.md), decisions
`B1…B11`. **Plan:** [`2026-11-21-virtio-blk.md`](../superpowers/plans/2026-11-21-virtio-blk.md),
whose ledger records every ruling made during the run. **Status: done** (checked in the Status list
above).

**Measured (2026-11-21, QEMU 11.0.3, 300 s local boots).** At the merge base both of two boots pass
148 checks, the last required marker at 48.1% and 55.7% of the log, the IPC counter at 32.3 M and
31.9 M. On the branch the disk boot passes 157 checks and the ramdisk boot 152; the last required
marker lands at 66.8% of the disk log and 48.1% of the ramdisk log (measured before the
`bdev.partial` probe, which adds three BDEV requests — read block 0, the partial write, read the
tail back — and so four device transfers on `vblk`, where the partial write is a read and a write).
The local 300 s and CI's 600 s per boot both stay; CI's job `timeout-minutes` goes 50 → 65 for the
second boot. `[diag mfs]` and `[diag vfs]`, compared as each server's first-occurrence stream, are
identical on the two backends apart from the `bdev.ds ok name=` line (`tools/diff-backends.sh`).
Both completion interrupts counted for the driver (proc 11) — the root check's two, on a 90 s disk
boot — were delivered immediately, none deferred. That run counted only those two; a completion
landing before the driver's receive would be deferred, which is equally correct.

**Refusal boots.** A blank disk: `root FAIL label`, then MFS's `bdev.root vblk rc=-6
fallback=memory` and a fully working ramdisk root; that log fails both backends' marker sets, as it
should. The root image attached `readonly=on`: bring-up, the root check, the mount and every read
pass, and each write fails with `EIO` (`bdev.partial FAIL write rc=-5`) without the device being
marked broken; the image is unchanged afterwards.

**Mutations.** Nine, each applied, booted and reverted; the full table is in the design's §3. Seven
moved as predicted. Dropping `notify`'s fence and acknowledging after the walk each moved nothing
under TCG, as expected. **Skipping the read in a partial write moved nothing at first** — no host
test and no marker reached it — so the slice added the `bdev.partial` boot probe, and the mutation
now gives `bdev.partial FAIL clobbered` on the disk boot.

**Amendments to phase text.** H5's first paragraph is rewritten above (publish on a device, probe
for a root). 6.1's `deliver_irq` debt and 6.2's debts 4–8 are marked paid where they are listed.

**Debts carried.**

1. ~~**`VIRTIO_BLK_F_RO` is not negotiated** (owner: 6.4, the first slice to request an optional
   feature — 6.2's debt 9 already sends it there). A read-only disk mounts as a root and fails at
   the first write instead of being declined or mounted read-only.~~ **Paid by 6.4, and held by a
   hand-run boot only:** the driver requests the feature and declines a read-only disk as a root
   (`root FAIL readonly`); MFS falls back. 6.4's mutation 5 removes the gate and the old behaviour
   returns — in a `readonly=on` boot run by hand. Under that mutation no host test fails, and no CI
   boot attaches a disk `readonly=on`, so nothing that runs unasked would notice the gate gone. That
   gap is 6.4's debt 9.
2. **The interrupt orderings are proved by reading only** (owner: whichever slice first boots under
   a non-TCG accelerator or a multi-queue device). Acknowledge-before-walk and the fence before
   `QueueNotify` cannot be made to fail under TCG.
3. **`fs.trunc ok n=0` cannot see a dropped truncate** (owner: the next slice that touches `init`'s
   write battery). With every device status read as success on a read-only disk it still passes; it
   should read the file back.
4. **A hang in the root check has one signature** (owner: none — recorded so the next reader does
   not hunt for a difference). A lost re-arm and a lost immediate delivery both show `virtio ok`
   followed by silence from `vblk` and MFS.
5. **One request in flight, completed synchronously** (owner: the slice that first needs throughput
   — none in Phase 6). The driver does not return to its main loop while a request is outstanding,
   so a second client waits in `SENDREC`.
6. **`vblk` is a DS name any server could publish** (owner: Phase 8, with the rest of the DS trust
   model). MFS trusts whoever answers the name and the zero-length probe. Not reachable from user
   processes, whose IPC mask excludes DS.
7. **The no-root fallback has no standing boot** (owner: 6.4, which builds disk images with a host
   tool and can attach a rootless one cheaply). CI boots a good disk and no disk; the third state, a
   disk without a root, was booted once by hand. Deleting the driver's no-root `ENXIO` arm, or a
   root check that accepts a bad label, leaves both CI boots and `diff-backends.sh` green — mutation
   5 was observed only on the blank-disk boot. `root::select` is host-tested; the driver's gate,
   MFS's revoke-and-regrant and the `bdev.root vblk … fallback=memory` line are not. **Partly paid
   by 6.4.** *Covered:* the `ENXIO` gate and the fallback line. The ramdisk boot CI is configured to
   run is now the rootless-disk fallback boot; answering `0` in place of the gate's `ENXIO` (6.4's
   mutation 4) fails that boot's marker set (observed locally, stub-free), and since 6.4's review
   round the gate is a pure function, `part::locate`, that three host tests hold. The fallback line
   is required by the ramdisk marker file. *Still owed* (6.4's debt 10): a root check that accepts a
   bad label still passes every CI boot, because the rootless disk is refused at `no root partition`
   before any label is read — `root FAIL label` was seen only under the hand-run mutations 1, 2 and
   10b; and no mutation was run against MFS's revoke of the declined driver's grant. *Given up in
   exchange:* the standing boot of the *no-device* state, which is 6.4's debt 2.

The two write-back orderings [`phase-6-prep.md`](phase-6-prep.md) carries from Phase 5 are **not**
discharged by this slice or by 6.4's persistence proof. `lseek` itself landed in pre-Phase-6 chunk 5
and does not reach them (they need a failure injected between two steps of one FS operation, not a
seek), and no slice here adds that. They stay owed on that tracker.

### Slice 6.4: `tools/mkimage` — one GPT disk — sub-milestone: disk root

**Goal:** the machine boots from its disk.

**Scope:** H6 — the host tool, the GPT module in `driver-rt`, the minor layout in virtio-blk (minor
0 the GUID-selected root partition, 1 the whole disk, `1 + n` partition *n*), and H5's root check
moved onto that partition. `qemu-run.sh` attaches the one image as a virtio-blk-device and drops the
`fat:rw:` directory.

**Flush.** virtio-blk negotiates `VIRTIO_BLK_F_FLUSH` and issues `VIRTIO_BLK_T_FLUSH` before
replying to a `BDEV_WRITE` — as built, after every device write that succeeded, and not after a
zero-length write, a refused grant or a failed device write — write-through, with no new BDEV
request. A device that does not offer the feature is used read-write anyway, with a `[diag]` line
saying so.

**Measure first:** unverified fact 2 — measured, and edk2 can (below), so no second attachment was
needed. CI's firmware has not been measured (debt 7). If it cannot boot from a modern virtio-mmio
disk, the fallback is a second, firmware-only attachment of the **same** image file — recorded as a
deviation, not adopted quietly.

**Proof:** no `fat:rw:` in the QEMU command line; `/bin/hello` exec'd from the MinixFS partition; a
write to the root survives into a second boot of the same image, which the ramdisk could never show;
the GPT disk's boot reports a root minor whose first block is not LBA 0.

The persistence proof shows that writes **reach the device**, and nothing more. It cannot prove the
flush: with a raw image, QEMU's writes land in the host page cache at once, so a second boot sees
them with or without a guest flush, and `cache=writethrough` would only hide the difference. The
flush is evidenced by its completion status in one `[diag]` line (`flush ok n=1`), and dropping it
is recorded as a mutation that moves that one line and nothing else — the reboot still passes — with
the honest record [`testing-and-markers.md`](../conventions/testing-and-markers.md#mutation-testing)
asks for, rather than claimed as covered.

**Design:**
[`2026-11-21-mkimage-gpt-disk-design.md`](../superpowers/specs/2026-11-21-mkimage-gpt-disk-design.md),
decisions `P1…P11`. **Plan:**
[`2026-11-21-mkimage-gpt-disk.md`](../superpowers/plans/2026-11-21-mkimage-gpt-disk.md), whose
ledger records every ruling made during the run. **Status: done** (checked in the Status list
above).

**Measured (2026-11-21 and 2026-11-22, QEMU 11.0.3, Homebrew edk2-stable202408, macOS on Apple
Silicon).**

- *Unverified fact 2.* edk2 boots from a modern (version 2) virtio-mmio disk: it loads Limine from
  `VenHw(837DCA9E-…,0000000A00000000)`, the kernel boots, and `vblk` then brings the same device up.
  The driver's reset recovers the device from whatever state the firmware left it in.
- *The image.* edk2 and Limine read `mkimage`'s FAT32 and GPT on the first image built. macOS's
  `fsck_msdos -n` finds the ESP clean, and `gpt -r show` and `hdiutil imageinfo` read the partition
  table. The image is 38 797 312 bytes (75 776 sectors) and about 7.4 MiB on disk, written sparsely;
  the ESP has two FATs of 540 sectors, its first data sector at 1112, and 68 520 clusters against
  the FAT32 bound of 65 525.
- *The runner.* A nested cargo inside the cargo runner completes in about a second with no wait on
  the build lock, so the tool is built in the shared target directory. (Measured before `mkimage`
  existed, as a `cargo build` of another host crate from a stand-in runner; the real `cargo run -p
  minixrs-mkimage` has run inside every boot since.)
- *What QEMU offers.* The accepted feature word is `0x100000200` (`VERSION_1 | FLUSH`) for a
  writable raw drive and `0x100000220` (`+ RO`) with `readonly=on`.
- *The boots.* The disk and ramdisk boots each pass 165 marker checks, the reboot passes its 7, and
  `tools/diff-backends.sh` passes across the two full boots with its three allowances. Booting one
  image repeatedly prints `persist gen=1`, then `gen=2`, then `gen=3`. In a 120 s reboot of a used
  image every write-battery probe up to `fs.hole ok` printed `ok` and none printed `FAIL`; `fs.leak`
  and later were not reached in that time. That is an observation, not an assertion (debt 4).
- *The budget.* Measured in the musl flavour, the one CI builds (`MINIXRS_SDK=/nonexistent`), and
  compared in seconds because the logs differ in length. The last required marker (`[diag vfs]
  pm-exit reset closed=1` on both boots) is about **396 s** into a disk boot (88.03% of a 450 s log)
  and about **393 s** on a second reading (65.57% of a 600 s log), and about **260 s** into a
  ramdisk boot (57.87% of a 450 s log, read once). Under 600 s that is 1.5x for the disk boot, so
  CI's two full boots went **600 → 1200 s**: 3.03x for the disk boot, 4.6x for the ramdisk boot. The
  reboot gets **180 s**: its one line lands about 34 s of local wall clock after `cargo run` starts
  (the kernel banner at about 6 s), 45 s passed locally twice, and the line prints before any C
  program runs, so the flavour does not move it. The job's `timeout-minutes` went **65 → 90**
  (1200 + 180 + 1200 s is 43 minutes, plus the 45 of build headroom, rounded up from 88). The
  documented local timeout for a marker-file verification became **600 s, in the musl flavour** —
  1.5x, the factor 300 s had at 6.3. Stub-free boots still reach every 6.4 marker in the first 0.33%
  of a 60–75 s log. *As first written:* the slice set 900 s, 80 minutes and a local 450 s from boots
  that turned out to be SDK-flavour — every boot before the review round ran with the SDK installed
  and `MINIXRS_SDK` unset — which put the marker at 82.0–82.7% of a 300 s disk log across three
  boots of the final code (82.00%, 82.71%, 82.68% — about 246–248 s) and 59.30% of the ramdisk log
  (about 178 s), against 66.8% / 48.1% for 6.3 (whose flavour could not be re-checked); readings
  across the slice's tasks ranged 68.4%–82.7% (disk) and 47.7%–59.3% (ramdisk). 900 s read as 3.63x
  on those and is 2.27x on the musl readings, so all three numbers were corrected within the slice,
  before the branch ran on CI. [`ci.md`](../conventions/ci.md) has the full statement. The runner's
  own TCG factor is still unmeasured (debt 13).
- *The driver's stack.* The 20 480-byte partition-table buffer is a `.bss` static; the largest frame
  in the built driver is `main`'s, 10 080 bytes of the 64 KiB stack.

**Recorded boots.** Each run once by hand; none is a CI boot. What each printed is in the design's
[*Observed* table](../superpowers/specs/2026-11-21-mkimage-gpt-disk-design.md#3-verification) and is
not restated here. In outline: `MINIXRS_ROOT=nodisk`, the disk attached `readonly=on` and 6.3's raw
`rootfs.img` came out as predicted. A damaged primary GPT did not: on a **writable** disk edk2
repairs the primary header or entry array from the backup *before the kernel runs*, so the driver
prints `root ok`, and its refusals of a damaged table are reachable only on a read-only disk (debt
3).

**Mutations.** Eleven were planned and fifteen rows were run (rows 7 and 10 split, and one added for
the probe's `0xEE` check), every boot stub-free, the tree proved clean afterwards. The table —
predicted beside observed — is in the design's
[§3](../superpowers/specs/2026-11-21-mkimage-gpt-disk-design.md#3-verification), with what the
whole-branch review round added under it. Six predictions (five mutation rows and the damaged-table
boot) were wrong or too loose and are listed there. Two results a later reader should know without
opening it: skipping the flush at its call site moves only `flush ok n=1`, and a `flush()` that
reports a bad status as success moves nothing anywhere (debt 1); and dropping the no-root `ENXIO`
gate — 6.3's mutation 5, which no CI boot could see — now fails the ramdisk boot's marker set
(observed locally, stub-free); that boot is one CI is configured to run.

**Amendments to phase text.** H5's selector sentence and its paragraph on where the root check
reads, H6's account of where the root partition's bytes come from, and H7's "no-disk boot" are
rewritten where they are stated above. H6's minor paragraph no longer says MFS sends every BDEV
request as minor 0 and is "literally unchanged": MFS mounts and serves through minor 0 alone, its
FS-band handlers and VFS are unchanged, and its prologue gained the one `bdev.minor` request for
minor 1. The **Flush** paragraph above says when a flush is issued as built, not "every
`BDEV_WRITE`". Unverified fact 2 is marked measured. 6.2's debt 9 is marked recorded, not paid;
6.3's debt 1 is marked paid with what holds it, and 6.3's debt 7 **partly** paid, each where it is
listed.

**Debts carried.**

1. **The flush is evidenced by one line** (owner: whichever slice first has a device whose flush can
   fail, or a backing store that is not a raw file). Dropping the flush moves only `flush ok n=1`; a
   `flush()` in `main.rs` that treats a bad status as success is caught by no host test and no boot,
   because QEMU's flush cannot be made to fail. The same seam hides the other half of the rule. "Not
   issued for a zero-length write or a refused grant" (the plan's Review Focus 6) rests on early
   returns in `main.rs` that no host test and no marker covers: three of `blk::flush_after`'s host
   tests exercise arms production never reaches, because `do_write` always passes `Some(wrote)` and
   a non-empty plan — the zero-length write, the refused grant and the failed read-before-write have
   all returned before the call.
2. **The no-device state has no standing boot** (owner: none in Phase 6; Phase 7, when the ramdisk
   question of H7 is decided). `[diag vblk] no device` and MFS's DS miss are reached only by a
   hand-run `MINIXRS_ROOT=nodisk`. This is what paying the covered part of 6.3's debt 7 cost.
3. **The firmware writes to the boot disk** (owner: none — recorded so the next reader does not hunt
   for it). edk2 repairs a damaged primary GPT on a writable disk, so "the image is unchanged
   afterwards" must allow for it, and the driver's primary-only GPT refusals are reachable only on a
   read-only disk.
4. **The reboot is checked against 7 lines** (owner: a slice that makes init's write battery
   idempotent on a used image). A panic in a reboot is caught only by `persist gen=1` going missing,
   and the battery's behaviour on a used image is observed, not asserted.
5. **Two gaps in the GPT parser** (owner: a slice that mounts a second partition). It does not
   require `FirstUsableLBA` to lie past the entry array, and it does not detect overlapping
   partitions.
6. **Several `FAIL` lines are not forbidden** (owner: the next marker-file pass). Under mutation 4
   `mount FAIL`, `bdev.tail FAIL`, `bdev.deny FAIL`, `[diag vfs] fs.mount FAIL`, `fs.read FAIL`,
   `exec.stage FAIL`, `fs.deny FAIL` and `exec.deny FAIL` all printed and none is in the common
   forbidden file. The mutation is still caught several times over, by missing `ok` lines and by
   other forbidden hits.
7. **CI's firmware has not been measured** (owner: the first CI run of this branch, which is the
   measurement). Debian's `QEMU_EFI.fd` booting from a version-2 virtio-mmio disk and reading
   `mkimage`'s FAT32 and GPT, the 1200 s / 180 s budgets against the runner's TCG, whether the
   pre-built `mkimage` is seen as fresh by the nested cargo on Linux, and whether the runner's QEMU
   still needs `ipxe-qemu` are all untested until then.
8. **An unresolved rustdoc link** (owner: any slice touching `kernel-shared`).
   `kernel-shared/src/execstack.rs:135` links to `sp`, which rustdoc cannot resolve. It predates
   this slice and was only noticed by it.
9. **The read-only gate is held by a hand-run boot** (owner: the first slice that gives `find_root`
   a host-test seam, or CI a boot with drive options). Deleting it (mutation 5) fails no host test —
   `bringup::read_only`, the decision, is tested; the three lines of `main.rs` that act on it are
   not — and no CI boot attaches a disk `readonly=on`. This is the unpaid edge of 6.3's debt 1.
10. **What is left of 6.3's debt 7** (owner: a slice that can build a third image kind — a root-type
    partition holding no root image — or that gives `find_root` a seam; and, for the revoke, the
    next slice that edits MFS's `root_device`). Two things. A root check that accepts a bad label
    passes every CI boot: `blk::check_head` and `check_tail` are host-tested, but their call sites
    in `find_root` are reached on a refusal only by a disk that has a root-type partition with the
    wrong bytes in it, and CI boots none. And MFS's revoke of the grant it issued to a driver that
    declined runs on every fallback boot with nothing that would notice it missing; no mutation was
    run against it.
11. **Every server can rewrite the boot disk** (owner: Phase 8, with the rest of the trust model —
    6.2's debt 10 and 6.3's debt 6 are its neighbours). Every `SRV_T` row of the boot image is given
    `ipc_to` over all active priv slots (`populate_priv` in `kernel/src/proc/table.rs`), so PM, VFS,
    RS, `memory`, TTY, DS, MFS, VM, SCHED and the driver rows can each send `vblk` a `BDEV_WRITE`;
    no user process can (`USER_IPC_TO` is PM, VFS and VM). That was true in 6.3, but there the only
    thing behind `vblk` was the root image: the ESP sat on a PCI drive minix.rs could not see. Since
    6.4 every minor is read-write (the design's ruling 7), so any of those servers can overwrite the
    kernel and Limine on the ESP (minor 2), and the partition table and its backup (minor 1) — and
    unlike a write to the ramdisk, that survives a reboot. Nothing does; nothing but the IPC mask
    stops one.
12. **Two contradictions outside this slice, noticed and not fixed** (owner: the next documentation
    pass). `book/src/servers/overview.md` says at one point that the exec target is hardcoded as
    `worker`, and further down the same chapter that init names the binary and alternates between
    two; and [`phase-6-prep.md`](phase-6-prep.md) still lists the `qemu-smoke` budget as 600 s. A
    third, in [`servers-and-drivers.md`](../conventions/servers-and-drivers.md): the nested build is
    described as shared across "the 9 crates", and the boot archive has held thirteen ELFs for some
    time.
13. **The budget was derived from the wrong flavour — paid within the slice, except for the runner's
    own factor** (owner of what remains: whoever reads the timing of this branch's first CI run —
    before merge, not after). The first 900 s and local 450 s were set from boots whose `hello` was
    the SDK's 47 KB, because an installed SDK is used when `MINIXRS_SDK` is unset; CI builds the 234
    KB in-tree-musl one. *Paid:* the disk boot was re-measured in the musl flavour twice (about 396
    s and about 393 s to the last required marker) and the timeouts raised to match — CI's two full
    boots to 1200 s (3.03x), `timeout-minutes` to 90, the local verification to 600 s in the musl
    flavour (1.5x). *Still owed:* the runner's real TCG factor, which nothing has measured — no 6.4
    code has run on CI. Pay by reading the last required marker's position in the first CI disk-boot
    log: if it is thin, raise the two full boots and `timeout-minutes` together; if there is far
    more than 3x to spare, lower them, since each full boot burns its whole timeout and 1200 s costs
    every booting run 10 minutes over 900 s. The ramdisk boot has one musl reading, not two. 6.3's
    own 66.8% / 48.1% were not re-checked for flavour and may be the same mistake.

6.3's debt 3 (`fs.trunc ok n=0` cannot see a dropped truncate) is **not** paid here, by ruling: the
persistence probe sits beside the write battery and does not change it.

### Slice 6.5: TTY RX — PL011 receive interrupt + `CDEV_READ`

**Goal:** the console can be read.

**Scope:** the steady-state loop of I12 in TTY against the PL011 receive interrupt, feeding an input
buffer; a `CDEV_READ` arm. And the part the prep tracker undersold as "one new arm in TTY": a
`CDEV_READ` that arrives with nothing buffered leaves VFS blocked in `SENDREC` for as long as TTY
withholds the reply, which stalls every other file operation in the system. This slice owes VFS a
way to park a read and reply later. That design is the slice's own; what is locked here is only that
**VFS does not block on a console**, and that 6.6 and 6.8 reuse whatever 6.5 builds.

It also owes two prose edits, because both still say the opposite: the console paragraph in
[`servers-and-drivers.md`](../conventions/servers-and-drivers.md) ("Phase 6 adds one TTY arm and
touches VFS not at all") and its copy in `book/src/drivers/overview.md` ("Phase 6 changes TTY and
nothing else"). Each is replaced, not appended to.

**Proof:** bytes piped to QEMU's stdin come back from `read(0)`; a second process's file read
completes **while** a console read is parked.

**Design:** [`2026-11-23-tty-rx-design.md`](../superpowers/specs/2026-11-23-tty-rx-design.md),
decisions `R1…R12`. **Plan:** [`2026-11-23-tty-rx.md`](../superpowers/plans/2026-11-23-tty-rx.md),
whose ledger records every ruling made during the run. **Status: done** (checked in the Status list
above). Both prose edits owed above are made: each paragraph was replaced.

**What it settled.** A character driver never withholds a `CDEV_READ` reply: with nothing buffered
it answers `EAGAIN`, records the caller, and sends it one `NOTIFY` when input arrives (R1). VFS
neither relays that answer nor waits on it: it parks the raw request — no reply, no grant held — and
on the driver's notification retries its parked reads oldest first with a fresh grant each, stopping
at the first `EAGAIN` (R2; since 6.6 the walk covers only the notifying driver's readers). No
`Message` layout, call number, endpoint or errno value moved (R10); the generated C headers were
diffed against `origin/main` and are byte-identical. **The kernel did change, by one guard** (R9): a
pending notification is no longer delivered as the reply to a `SENDREC`, which the design assumed
and the kernel did not do — see the finding under *Measured*. The working rules are in
[`servers-and-drivers.md`](../conventions/servers-and-drivers.md#a-character-driver-never-withholds-a-cdev_read-reply)
and [`kernel.md`](../conventions/kernel.md#a-pending-notification-is-never-the-reply-to-a-sendrec).

**What 6.6 and 6.8 inherit (R12).** 6.6 implements the driver side of R1 in `virtio-console`, adds
`/dev/hvc0` to VFS's device table, widens VFS's notifier check from the one `tty` endpoint to the
set of character drivers that can say `EAGAIN`, and makes the retry walk **per driver**, which
`park.rs` needs an in-order step (`oldest_after`) for. (As first written this said "`park.rs` does
not change" and stopped at the notifier check; 6.6 found that a walk which asks the oldest reader's
driver, whoever notified, hangs once two drivers have readers parked — corrected 2026-11-24, see
[6.6](#slice-66-virtio-console-as-a-second-cdev-backend).) 6.8 makes TTY a *client* of the same
contract against `virtio-input` — on that driver's `NOTIFY`, TTY sends `CDEV_READ` until it hears
`EAGAIN`, pushes through the keymap into the ring its own interrupt already fills, and notifies its
waiter as it does today. That is H8's notify-then-pull, and it is the same three lines in both
directions. 6.8 also inherits the shape that exposed 6.5's kernel defect — TTY will `SENDREC` a
driver that notifies it — and relies on the kernel rule for it; its notification arm goes where
TTY's loop today drops any `NOTIFY_MESSAGE` that is not from `HARDWARE`, ahead of the request
dispatch and its `ENOSYS` arm. Echo arrives with 6.8 and canonical mode with Phase 7, both in
`drivers/tty`'s `input.rs`.

**Measured (2026-11-23, QEMU 11, Homebrew's edk2, macOS on Apple Silicon).**

- *The UART as the firmware leaves it.* `UARTLCR_H = 0x70` — the FIFO is **enabled** — and
  `UARTIFLS = 0x12`, receive trigger at half. One byte fed on its own still raised the line: one
  notification, one byte, `UARTRIS = 0x30` with the receive-timeout bit clear, so QEMU raises RX on
  the first byte whatever the trigger level says. Three bytes written together arrived in one
  notification, and 36 written together were drained in one — more than the 16-deep FIFO, because
  QEMU refills it as the loop empties it. `IRQ_ENABLE` answered `OK` each time with the `UARTICR`
  write placed before the drain.
- *EOF on stdin.* With `< /dev/null` QEMU carries on and the boot is unchanged, so the feeder exits
  once it has written instead of holding its stdout open.
- *Bytes on stdin from time zero.* Five of them never reached TTY: the firmware or Limine consumes
  what is on stdin before the kernel starts. The feeder's gate on a log line is therefore
  load-bearing, not only what makes the proof causal.
- *The path, once.* With two throwaway probes in place the first boot to exercise the park and the
  retry printed `cons.park`, then `rx got n=5`, then `stdin got=hello`.
- *The fed boots.* Four fed stub-free boots each printed `cons.park` (log line 622), `cons.beside
  ok` (625) and `stdin ok n=15` (between lines 925 and 984), with no `FAIL`; an unfed one printed
  the first two and no `stdin ok`. Those were boots of the code before the kernel fix below, and so
  were passes by timing; after it, four more fed stub-free boots each printed `cdev.short ok n=256`,
  `cons.notify ok`, `cons.park`, `cons.beside ok` and `stdin ok n=15`, with no `FAIL`. The marker
  files as committed hold 174 checks for each full boot (113 + 48 common, 8 + 5 per backend) and 7
  for the reboot. The recorded musl-flavour set on the final code (`efc8d22`):
  - the fed disk boot passes `check-boot-log.sh disk` on all 174 checks;
  - the fed ramdisk boot passes `check-boot-log.sh ramdisk` on all 174;
  - the unfed reboot of the disk boot's image passes its 7, and its log shows `cons.notify ok`,
    `cons.park` and `cons.beside ok` with no `stdin ok` — the reader parked for the whole boot;
  - `diff-backends.sh` is clean across the pair; the feeder started writing 27 s into the disk boot
    and 23 s into the ramdisk one.
- *The budget, and the defect it found.* A boot of the merge base on this machine put the last
  required marker (`[diag vfs] pm-exit reset closed=1`) at 64.71% of a 600 s disk log. As first
  built the slice moved it: 76.62% of the fed disk log and 50.59% of the ramdisk one, a whole
  rotation turn later, because the `wait()` that reaped the console-read child left its turn with no
  child of its own reaped and the rotation ran one child deeper for the rest of the boot. init now
  waits once more in that turn, and the boots recorded with that fix read **64.94%** (about 390 s)
  and **42.73%** (about 256 s) — back at the baseline, no timeout moved. Those two figures predate
  the kernel fix and the notify probe; on the final code (`efc8d22`):
  - **63.20%** of the fed disk log, about 379 s;
  - **41.41%** of the fed ramdisk log, about 248 s.

  So the slice does not move the budget and no timeout changes.
- *The fork-cycle markers moved a slot.* init's first fork is now the console-read child, so the
  rotation's first `worker` runs in slot 20, not 19 (slot 21 since 6.6, which forks a second such
  child); `tests/qemu-boot.expected` says why that is fixed on a fed and an unfed boot alike.
- *A kernel defect, found twice.* The privilege wiring needed nothing — INTID 33 was already on
  TTY's allow-list, TTY could already notify VFS, VFS could already withhold a reply — and the spec
  concluded from that that the kernel would not change. It was wrong. `mini_receive` handed a
  pending notification to a caller in the receive half of a `SENDREC`; MINIX 3's `proc.c` guards
  that check with `MF_REPLY_PEND` ("pending notifications, except for SENDREC") and the translation
  had dropped the guard. It was latent until this slice, the first in which a server `SENDREC`s a
  process that also notifies it. A notification latched at VFS from TTY came back as the "reply" to
  VFS's next `CDEV_WRITE` or `CDEV_READ`: the write was cut short, the parked reader was never
  woken, and TTY's real reply arrived later as a stray message that VFS and TTY answered `ENOSYS` to
  each other — 32,525 rounds in one boot, counted with a diagnostic-only probe. The fresh
  whole-branch review found it by reading the kernel; the mutation run hit it independently at row 4
  and reproduced it on the unmutated tree, where 2 of 4 fed stub-free boots printed no `stdin ok`
  and no `FAIL` anywhere. So every pass recorded before the fix was a pass by timing.
- *The fix and its proof.* The guard is restored in `kernel/src/ipc/receive.rs`. The proof was
  written first: TTY notifies its first client — VFS — before replying to that client's first
  request, so VFS's second prologue request meets a latched notification on every boot. Without the
  guard `[diag vfs] cdev.short FAIL rc=4096` printed in place of `cdev.short ok n=256` (observed);
  with it `cdev.short ok n=256` prints, and so does a new required line, `[diag vfs] cons.notify ok`
  — VFS's first notification from the console driver, received by its ordinary `RECEIVE` loop, on
  every boot, fed or not. The proof is free of *input* timing and not of scheduling: it works
  because TTY and VFS share a priority band (debt 17, and
  [`kernel.md`](../conventions/kernel.md#a-pending-notification-is-never-the-reply-to-a-sendrec)).
- *The flag behind the guard could go stale, in two ways, and now cannot matter.* Once
  `MF_REPLY_PEND` withheld notifications at receive time, a flag left set became a starved process.
  The commit review found the first way: a `SENDREC` whose receive half is refused left it set, and
  `do_sendrec` now clears it (the one reachable refusal is `ELOCKED`, after a send half that
  blocked). The re-review of the fix found the second and larger one: **every successful `exec` left
  it set**, because the caller is restarted with no reply ever delivered. Nothing broke — an exec'd
  process is `USR_T`, `SENDREC`-only and never notified — but the first server or driver started by
  fork and exec would have stopped receiving notifications. `do_exec` now clears the flag, and a
  plain `RECEIVE` clears it at entry, which is MINIX 3's own backstop (`do_sync_ipc`, `case
  RECEIVE`) and had been dropped in translation with the guard. With that last clear a stale flag
  from any path lasts no longer than its process's next plain `RECEIVE`. None of the three clears
  has a boot or a mutation that proves it; they are verified by reading (debt 14).
- *From the same review.* A wake-up `ipc_notify` that fails now keeps the waiter and prints `[diag
  tty] rx FAIL notify rc=…` once (forbidden) — it used to be discarded with the waiter already
  forgotten. `tools/feed-console.sh` parses `FEED_CONSOLE_CEILING` base 10 (`08` was an arithmetic
  error). TTY drops any `NOTIFY_MESSAGE` that is not from `HARDWARE` instead of answering it.
- *Mutations.* Sixteen rows, run on the fixed kernel after eight clean fed boots; the spec's §3
  table has each one, and this is what the classes showed.
  - *Confirmed, as predicted.* Remove the kernel guard: `cdev.short FAIL rc=4096`, and `cons.notify
    ok` moves to the real wake-up. Remove TTY's probe: `cons.notify ok` moves to real input and is
    absent unfed. TTY withholds its reply: the whole system freezes about 637 lines in. VFS relays
    `EAGAIN`: `stdin FAIL n=0 first=none rc=-11`. No wake-up `NOTIFY`, or the notification dropped
    without a retry: `stdin ok` absent, the rest unmoved. The storm guard disabled: three
    `irq::tests` fail. A forced `NOTIFY` failure: `rx FAIL notify rc=-1` once, reader still served.
  - *Four that behaved differently.* **4a**, a notification reaching VFS's dispatch, was not
    absorbed: a sustained `ENOSYS` ping-pong with TTY, VFS's console `SENDREC`s answering `ELOCKED`,
    userland output down from 8,489 lines to 54, and no `FAIL` line (debt 9). **5**, no `IRQ_ENABLE`
    after the drain, **passed** — the feeder wrote the probe in one piece, which is one interrupt —
    so the feeder now writes two chunks 3 s apart, and with it the mutation loses `stdin ok`. **7**
    (consume before the copy's result) and **8** (`do_pm_exit` does not free) were each predicted to
    fail a host test and failed none, with no marker moved: the tests pin the ring's and the table's
    operations, not the calls in the two `main.rs` files (debts 11 and 12).
  - *Nothing moves.* Skipping the drain leaves bytes stuck in silence, not a storm, so `rx FAIL
    storm` has host coverage only. Draining before acknowledging moves nothing. Removing
    `do_sendrec`'s clear-on-error moves nothing. Removing init's second `wait()` passes all 174
    checks and shows only as a budget of 73.30% against 62.72% (debts 13 to 16).

**Debt carried from 6.5.**

1. ~~**`EIO` covers only a refused claim** (owner: 6.6, which writes the second driver against this
   contract, or the first slice that sees a reader hang).~~ **Paid by 6.6**, in TTY and in `vcon`: a
   stopped line answers `EIO` before its buffer is looked at and wakes its waiter once, and a failed
   proof round now enters `Stopped` (it did not before). Held by host tests and reading — no boot
   reaches it (6.6's debt 5). As 6.5 recorded it: a failed TX proof, `rx FAIL storm` and `rx FAIL
   enable` also end reception, and a read of the empty ring still answers `EAGAIN`, so a reader
   stays parked for good — each behind a forbidden marker that already fails the boot. The fix is
   about five lines: answer `EIO` from the stopped phase and send the waiter one wake-up `NOTIFY`.
2. **No proof child under `idle-probe`** (owner: whoever pays 6.1's idle debt and deletes the
   feature). `idle_park` waits until no child is left and a parked child never exits, so the fork is
   `cfg`'d out and that hand-run configuration does not exercise receive.
3. **`cons.beside ok` is ordered, not synchronised** (owner: none unless it flakes). init forks the
   reader before its first file read and `fileio` reads `/etc/motd` every fourth rotation turn for
   ever, but nothing makes init wait for the child to park. A sync point would have to be visible to
   VFS, which this slice deliberately did not add.
4. **CI's fed boots are unmeasured** (owner: this branch's first CI run, which is the measurement).
   `ci.yml` feeds the disk and ramdisk boots with `FEED_CONSOLE_CEILING=1300`; none of it has run on
   the runner's bash, QEMU or firmware. `actionlint` reports shellcheck's informational SC2094 on
   the read-and-write of one log file, which is the design.
5. **No echo and no line discipline** (owner: 6.8 for echo, Phase 7 for canonical mode). A `read()`
   returns raw bytes as received; typing at a hand-run boot shows nothing.
6. **One waiter, last asker wins** (owner: the slice that gives TTY a second CDEV client). A second
   endpoint asking while a waiter is recorded replaces it, and the first is never notified.
   Unreachable while VFS is TTY's only CDEV client; pinned by a host test. The fix is a two-slot set
   in TTY's `main.rs`, no protocol change.
7. **The storm threshold is a guess** (owner: whoever first sees a false `rx FAIL storm`). 64
   consecutive empty notifications; too low gives a false failure on a noisy line.
8. **The feeder has a ceiling and no stall detection** (owner: none unless a hung boot is seen to
   leave one behind). It polls until `FEED_CONSOLE_CEILING`, 1800 s by default, whatever the boot is
   doing.
9. **Two servers that both answer unknown messages can answer each other for ever, and nothing stops
   them** (owner: 6.8, which builds the second `SENDREC`-and-notify pair, or the first slice that
   sees a stray `ENOSYS` exchange in a trace). The hazard is stated once, under
   [Always reply](../conventions/servers-and-drivers.md#always-reply). Deliberately not hardened in
   6.5, and on a weaker footing than first recorded: the kernel defect above was thought the only
   way in, and mutation 4a showed it is **one slip in VFS's notification arm away** (the re-review
   found one further theoretical path, through `ELOCKED`, that no current process pair reaches). The
   fix — a reply-shaped message is never answered — changes every server's unknown-message arm and
   is its own change. Until then the cost of that slip is a boot that degrades silently: console
   calls answering `ELOCKED`, userland output collapsing, and no `FAIL` line.
10. **The kernel rule's only proof is a probe in TTY's production loop** (owner: whoever gives the
    kernel a test that can provoke the interleaving some other way). One spurious notification per
    boot; removing it is a flag and six lines and leaves `mini_receive`'s guard with no boot proof.
11. **The consume-after-copy order in TTY's `do_read` is unproved** (owner: the first slice that can
    make `SYS_SAFECOPY` refuse a console read with bytes buffered, or 6.6, which writes the same
    step in a second driver). **Paid by 6.6 for `vcon` only**: its proof child's read into an
    unmapped buffer fails the boot if `vcon` consumes before its copy returns. TTY's `do_read` is
    exactly as unproved as it was; the recipe for it is 6.6's debt 4. `peek` and `consume` are
    host-tested as ring operations; consuming before the copy's result is known fails no test and
    moves no marker.
12. **`do_pm_exit`'s free of a parked read is unproved** (owner: the slice that first kills a
    process blocked in VFS). `ParkTable::free` is host-tested; deleting the call fails no test and
    moves no marker, because no boot kills a parked reader. `evict_stale` stands behind it.
13. **init's wait-again is guarded only by a hand-measured budget** (owner: whoever next re-measures
    the boot budget, or gives the rotation a depth marker). Without the second `wait()` every check
    passes and the last required marker moves from 62.72% to 73.30% of the disk log — a regression
    only [`ci.md`](../conventions/ci.md)'s boot-timing reading would catch.
14. **`MF_REPLY_PEND`'s clears off the delivery path are verified by reading only** (owner: the
    slice that first starts a server or driver by fork and exec, or whoever gives the kernel a test
    that can fail a receive half). `do_sendrec`'s clear-on-error moves nothing when removed; the
    `exec` and plain-`RECEIVE` clears have no boot that would miss them.
15. **R4's acknowledge-before-drain order rests on reasoning under QEMU** (owner: the first run on
    hardware, or on a UART model that holds its level). Reversing the order moves nothing.
16. **`rx FAIL storm` is host-tested only** (owner: none unless a storm is seen). QEMU drops the
    line on the `UARTICR` write, so skipping the drain produces stuck bytes, not a storm, and no
    boot has seen the marker print.
17. **The kernel rule's probe depends on TTY and VFS sharing a priority band** (owner: whoever
    changes a boot server's priority or quantum). The guard is exercised only if TTY reaches
    `RECEIVE` before VFS's next `SENDREC`; rank VFS above TTY and `cdev.short ok` prints with the
    guard or without it, on every boot, with nothing to say the proof has gone. Re-run "remove the
    guard" after any such change and expect `cdev.short FAIL rc=4096`.
18. **The feeder's 3 s gap is a guess** (owner: this branch's first CI run, with debt 4). If both
    chunks of the probe land in one drain the re-arm goes unproved and the boot still passes.

### Slice 6.6: `virtio-console` as a second CDEV backend

**Goal:** a second character device, served by a virtio driver.

**Scope:** the driver on slot 12, one port, no multiport feature; a `/dev/hvc0` node in VFS's device
table. fd 0, 1 and 2 stay on TTY.

**Proof:** a write to `/dev/hvc0` appears on a second QEMU chardev, and bytes fed to that chardev
come back through a read of the node — while a reader parked on the serial console stays parked and
is not in the way.

**Design:**
[`2026-11-24-virtio-console-design.md`](../superpowers/specs/2026-11-24-virtio-console-design.md),
decisions `C1…C14`. **Plan:**
[`2026-11-24-virtio-console.md`](../superpowers/plans/2026-11-24-virtio-console.md), whose ledger
records every ruling made during the run. **Status: done** (checked in the Status list above).

**What it settled.** `drivers/virtio-console` is a real driver, packed for proc slot 12 directly
after `vblk` (`vcon < vfs` is the one link it adds to the packing chain) and attached to **every**
boot: `tools/qemu-run.sh` puts a `virtio-serial-device` with one `virtconsole` port on
`virtio-mmio-bus.8`, its host side a `pipe` chardev over the FIFO pair `target/hvc0.{in,out}`
(`MINIXRS_HVC0` moves it). The driver negotiates no optional feature and reads no device
configuration (C3); it has two queues on two pages of its DMA run and one buffer each way on two
more (C5). Receive has no ring: the one receive buffer is either *posted* to the device or *held*
with bytes a reader has not taken, reads are served straight from it, and while it is held the host
is made to wait, so nothing is dropped (C6). Transmit is one chain in flight, waited for in a
receive filtered to `HARDWARE` (C7). One handler serves both queues on every interrupt (C8). `vcon`
publishes to DS unconditionally and answers `ENXIO` to every CDEV request when it has no working
device (C4); a device that breaks later answers `EIO` and wakes its recorded waiter (C9).

In VFS, `/dev/hvc0` is the fourth device node and `CharDriver::Vcon` the third character driver, and
**the retry walk is per driver** (C10): on a driver's notification VFS takes the parked reads oldest
first, passes over every one whose descriptor resolves to another driver, asks the notifier for its
own, and ends at the first `EAGAIN` the notifier gives. The decision is a pure step function in a
new `servers/vfs/src/retry.rs` (`CharPeers`, `step`, `Walk`); `park.rs` gained `oldest_after(seq)`
so a walk can leave an entry where it is. R12 had said `park.rs` would not change and that widening
the notifier check was the whole of VFS's part; with two drivers that design hangs — a reader parked
on TTY ahead of one parked on `vcon` turns every `vcon` notification into a question to TTY, an
`EAGAIN`, and a return. The 6.5 section above and the 6.5 spec's R12 are corrected.

R1's pure half — `Waiter`, `read_plan`, `read_len` and the validators, now parameterised by the
driver's one minor — moved from `drivers/tty/src/cdev.rs` to `driver-rt::cdev`, and both drivers
call it. `read_plan`'s third argument is now "input can still arrive".

No `Message` layout, call number, endpoint or errno value moved (C13). `DEV_HVC0_PATH` and
`CDEV_MINOR_HVC0` (0 — the driver's own namespace, not the console's) are new names in
`kernel-shared` and are deliberately not emitted into the generated C headers, which were diffed
before and after Task 2 and are identical. Nothing under `kernel/src` changed. The working rules are
in
[`servers-and-drivers.md`](../conventions/servers-and-drivers.md#a-second-backend-on-the-cdev-band);
how the driver works is in [the book](../../book/src/drivers/overview.md#virtio-console).

**Debts this slice paid.**

- **6.5's debt 1, in both drivers.** A character driver whose device has stopped answers `EIO` and
  sends its recorded waiter one `NOTIFY`, so a reader already parked is retried and hears the `EIO`.
  `vcon` was written that way; TTY was changed to match: `IrqService::can_receive` turns `false` for
  good on any of the four ways its line stops (`irq FAIL bits`, `irq FAIL enable`, `rx FAIL storm`,
  `rx FAIL enable`), and `main` wakes the waiter on that one transition. Two things the spec had not
  seen came with it. A failed proof round did **not** stop TTY's line before this slice — it printed
  its `FAIL` and stayed in `Proof`, and could go on to print `irq ok` — so it now enters `Stopped`.
  And `read_plan` puts a stopped device *before* its buffer, so bytes still in TTY's ring when the
  line stops are no longer readable.
- **6.5's debt 11, for `vcon` only.** init's second proof child issues one read of `/dev/hvc0` into
  an address that is in the user range and not mapped. With nothing fed it parks; when the first
  bytes arrive VFS's retry reaches the driver with bytes held, the kernel refuses the copy, and the
  child hears `EFAULT` — and then has to read all 17 bytes of the probe, the refused chunk included.
  A `vcon` that consumed before its copy returned comes up short and `hvc0 ok` is absent. **TTY's
  `do_read` has no such proof and its copy of the debt stands** (debt 4 below).

**6.2's debt 2 moves to 6.8.** It named "6.6 or 6.8, the first driver with 8- or 16-bit config
fields". This driver takes none of the three features that make a console configuration field valid,
so it has no field it may read (C3).

**Measured (2026-11-24, QEMU 11.0.3, Homebrew's edk2, macOS on Apple Silicon).**

- *The pipe with nobody on it.* With `-chardev pipe` over two FIFOs and no process on either, QEMU
  starts and the boot is unchanged. A guest write with no reader on the host side completes, with
  one transmit interrupt, and the bytes sit in the pipe.
- *What the firmware does with the port.* edk2 binds the console and writes to it: 808 bytes reached
  the host side before the kernel banner — `BdsDxe: loading Boot0001 …` and Limine's menu text, one
  cursor escape per character. The guest's own line lands at offset 808, straight after. So a reader
  of `target/hvc0.out` must look for its line among other output. The firmware leaves the device
  **reset**: `Status` is 0 at kernel entry. Bytes written to `target/hvc0.in` while the firmware is
  still running are lost.
- *The kernel's lines.* With no ELF in slot 12 the probe printed `[virtio] slot=8 pa=0xa001000 id=3
  ver=2 skip=no-module for=vcon` and no `[dma] vcon` line. With the driver packed: `[virtio] slot=8
  pa=0xa001000 id=3 ver=2 drv=vcon`, `[devmap] vcon va=0x40001000 pa=0xa001000 intid=56` and `[dma]
  vcon va=0xc0000000 pages=16`.
- *The device.* `QueueNumMax` is 1024 for both queues, clamped to `driver-rt`'s 8. Queue 0 receives
  and queue 1 transmits. A 600-byte host write arrived as used elements of 256, 256 and 88 bytes —
  one per re-post of the one buffer. **Bytes written while the buffer is held are kept** and
  delivered at the re-post (4.6 s later in the measurement). Nothing contradicted C5–C7, and C2's
  four acceptance conditions hold on this machine.
- *The driver's verdict.* `[diag vcon] virtio ok features=0x100000000 rxq=8 txq=8 slot=8`, and
  `[diag vfs] vcon.ds ok ep=12`.
- *The pipe is not emptied when QEMU exits.* On macOS about 826 bytes of an unread boot — the
  firmware's 808 and the guest's 18-byte line — were still in `target/hvc0.out` after QEMU had
  exited and no process held it. A fed boot started after an unfed one found that **old** line at
  once, wrote its answer into a machine still in its firmware, and lost it: no `hvc0 ok`, and by the
  feeder's coupling no `stdin ok`. The feeder now empties both FIFOs when it starts (debt 1).
- *The first chunk can beat the read.* With the stubs on, the feeder's first chunk — written the
  moment the guest's line was seen — reached the driver before the child's unmapped read had been
  issued, so the read was refused directly and never parked. The boot passed, but the retry of a
  parked raw request into an unmapped buffer went unexercised. The feeder now waits `SETTLE` (3)
  seconds first (debt 2).
- *The markers' order.* Three fed stub-free boots and one fed full disk boot: always `[diag vfs]
  cons.other ok`, then `minix.rs init: hvc0 ok n=17`, then `minix.rs init: stdin ok n=15`. An unfed
  stub-free boot printed none of the three and no `FAIL`. The spec's §3 table and the plan had `hvc0
  ok` first; VFS prints `cons.other ok` at the first chunk's walk, when the answer behind the
  passed-over serial reader is the `EFAULT`.
- *The marker count.* The files as committed hold 190 checks for each full boot (122 expected and 55
  forbidden in the common pair, 8 and 5 per backend) and still 7 for the reboot: nine expected and
  seven forbidden lines were added to the common pair.
- *The fork-cycle markers moved a slot again.* init's first two forks are its proof children, so the
  `read(0)` child holds slot 19, the `/dev/hvc0` child slot 20, and the rotation's first `worker`
  runs in slot 21; the common expected file gained a third `SYS_FORK … child_nr=21` line.
- *One fed full boot from Task 5.* A fed full disk boot of the musl flavour passed all 190 checks,
  the feeder reporting the guest's line 26 s after it started, `hvc0` fed at 32 s and the serial
  probe at 36 s.
- *A defect this slice introduced, and found.* With the boot stubs on, PM's four grant lines
  (`grant.test ok`, `copy ok`, `magic ok`, `grant.deny ok`) were missing on 3 boots of 3. VFS's
  `grant_test` is its one `ipc_send` to PM. The fourth DS lookup added ahead of it gave stub D time
  to fault first; PM then blocked in its `VFS_PM_EXIT` `SENDREC` to VFS, the kernel refused VFS's
  send, and VFS discarded the result. Tasks 2 to 4 were verified on stub-free boots only and never
  saw it. `grant_test` now runs before the lookups and prints `[diag vfs] grant.test FAIL send rc=…`
  (forbidden) if the send is refused; the rule it left is under
  [Prologue ordering](../conventions/servers-and-drivers.md#prologue-ordering).

- *The recorded boots* (final code `5a55b7d`, the musl flavour CI builds, 600 s for the two full
  boots and 180 s for the reboot, on the development machine):
  - the fed disk boot and the fed ramdisk boot each pass all **190** checks;
  - the unfed reboot of the image the disk boot left passes its seven;
  - `diff-backends.sh` is clean across the pair;
  - the feeder saw the guest's `hvc0` line 23 s into the disk boot and 18 s into the ramdisk one,
    and wrote the serial probe at 34 s and 28 s;
  - the generated C headers are byte-identical to `origin/main`.

  An earlier set, one commit before the review round, passed the same checks; a first set was
  discarded because it had been booted in the SDK flavour, which is not a marker verification.
- *The budget.* The last required marker (`[diag vfs] pm-exit reset closed=1`) sits at **57.51%** of
  the fed disk log (about 345 s) and **36.46%** of the fed ramdisk log (about 219 s); the earlier
  set read 55.38% and 37.65%. **The merge base, booted the same day in the same checkout, reads
  54.91%** (about 329 s) for the fed disk boot. So the slice moves the disk reading by between half
  a point and two and a half, which is the spread between its own two runs, and no timeout changes.
  Both are well under 6.5's recorded 63.20%: the machine reads lower than it did then, for the merge
  base too, so that figure is not a baseline to compare against. In every log the marker's first
  occurrence follows fileio's own `malloc ok`, so it is not the vacuous form Task 5 found. A first
  attempt at the baseline, from a `git worktree`, was thrown away: its build packed the 15 KB
  fallback in place of the C programs and the marker never printed.

- *The mutations* — fourteen, each applied, booted fed and stub-free (14 with the stubs on),
  observed and reverted; a clean baseline printed `cons.park`, `cons.beside ok`, `cons.other ok`,
  `hvc0 ok n=17`, `stdin ok n=15` in that order. As predicted: 2 (no re-post), 5 (no transmit notify
  — the boot stops at 640 lines behind VFS), 7 and 8 (VFS drops `vcon`'s notification; the walk
  stops at the first `EAGAIN` — `cons.other ok`, `hvc0 ok` and `stdin ok` all go, with two `retry`
  host tests failing each), 9 (the row names TTY — `minix.rs hvc0 out` appears on the serial log),
  11 (ack after the walks — nothing moves), 13 (one-piece feed makes mutation 2 pass, which is why
  the feed is two chunks) and 14 (`grant_test` after the lookups — `grant.test FAIL send rc=-208`
  and PM's four lines gone). **Not as predicted:**
  - **10, the archive row moved after `vfs`, moved nothing.** `vcon.ds ok ep=12` still printed: the
    lookup is VFS's fourth blocking call and `vcon` has published by then. So `vcon.ds ok` does not
    police the packing order, as the spec's §3 and `kernel/build.rs` had claimed; both are
    corrected. A wrong lookup key does print `vcon.ds FAIL rc=-3 fallback=12` with everything else
    passing, so the fallback itself is sound.
  - **12, the stopped-device wake-up, is caught only in the shared helper.** Dropping the wake at
    either driver's call site left every host test green and every marker unmoved. TTY had no tested
    helper for the decision at all; it now shares `driver-rt::cdev::wake_due` with `vcon`. The call
    sites remain proved by reading only (debt 5).
  - **1, advance before the copy, removes `hvc0 ok` — but by hanging, not by a short count.** The
    refused read consumes the first chunk and the child waits for ever; no `hvc0 FAIL` line is
    printed in either form tried. The proof holds; the probe cannot say "bytes were lost".
  - **3, no re-arm, hangs the first write**, and is indistinguishable from 5 in the serial log; only
    the feeder's stderr tells them apart (in 3 the host did see the guest's line).
  - **4 and 6 also remove `cons.other ok`**, which the walk that the `NOTIFY` triggers prints; the
    spec's "everything before it unmoved" had the order wrong.

**Rulings made during the run** — the spec's §5 table has the fifteen it started with; these are the
ledger's, each with what it costs if it was wrong.

- *A stopped device answers `EIO` before its buffer is looked at* (Task 1). Bytes left in TTY's ring
  when its line stops are unreadable. If wrong: swap two arms of `read_plan`, and `vcon` then has to
  serve a held buffer from a broken device.
- *A failed proof round moves TTY to `Stopped`* (Task 1). If wrong: one line in `proof_round`.
- *During TTY's delivery proof `can_receive` is true* (Task 1): a read then parks, and is woken by
  the first byte or by the proof failing.
- *The step function lives in a new `retry.rs`; `cdev_endpoint` left `main.rs` and is
  `CharPeers::endpoint`* (Task 2), so the driver-to-endpoint match and the notifier check are
  host-tested. If wrong: local to VFS.
- *`cons.other ok` needs a real `CDEV_READ` answer behind the pass-over* (Task 2); an `EBADF` step
  or a grant errno does not count. If wrong: one line.
- *The layout refuses a run shorter than four whole pages, and `Rx::advance` by more than is held is
  a fault that changes nothing* (Task 3).
- *`DmaBuf::prefix`, a narrowing-only handle in `driver-rt`, is how a write chains exactly its
  bytes* (Task 4). The alternative was carrying the remainder region in the layout.
- *A read whose copy succeeded and whose re-post then failed replies the count, not `EIO`* (Task 4):
  the caller has the bytes; the next request hears `EIO`.
- *The `ENXIO` gate is consulted before the request is parsed, and a zero-length write to a broken
  device is `0`* (Task 4).
- *`[diag vcon] no device` is forbidden in the common file* (Task 4): no boot the set is applied to
  attaches no console. Two expected lines beyond the spec's table — `[as] vcon nr=12` and `[devmap]
  vcon … intid=56` — and `[diag vcon] ds FAIL` forbidden.
- *C14.3 was measured on the real driver's first boot, not with a throwaway probe* (Task 0). A
  contradiction of C5–C7 would have cost a rewrite of `port.rs`; none appeared.
- *The grant-test fix is an ordering fix, not a retry* (Task 5; debt 3).
- *The feeder discards whatever is in the FIFOs when it starts, reads `target/hvc0.out` through a
  background `cat` whose end-of-file means "QEMU gone", and waits `SETTLE` before its first chunk*
  (Task 5; debts 1 and 2). Each is a knob in the script.
- *The proof child closes `/dev/hvc0` on every path before its verdict* (Task 5), a sixth step
  `step=close`. Exiting with the descriptor open printed `[diag vfs] pm-exit reset closed=1` early —
  at 3.6% of a fed disk log instead of about 63% — and left `fileio`'s marker vacuous. If wrong: one
  call.

**Debt carried from 6.6.**

1. **Unfed boots accumulate unread bytes in `target/hvc0.out`** (owner: whoever first sees a boot
   stall behind a full pipe, or makes the runner drain it). macOS keeps about 826 bytes per unread
   boot after QEMU exits; only a feeder's start-up drain empties the pipe, and a full pipe — some 80
   unfed boots with no fed one between, by the ledger's estimate — is untested. The same drain is
   why a feeder started against an already-running boot can throw the guest's line away and then
   never feed: start it with the boot.
2. **`SETTLE` and `GAP` are guesses** (owner: this branch's first CI run, with 6.5's debts 4 and
   18). If both chunks of the `hvc0` probe land in one interrupt, the re-post and re-arm go
   unproved; if the child's read is not parked `SETTLE` seconds after its line, the retry of a
   parked request into an unmapped buffer goes unexercised. The boot passes either way and nothing
   in the log says so. Making "parked when the bytes arrive" deterministic needs a VFS-side marker
   the feeder can gate on — a park on `vcon` specifically, since `cons.park` is printed once per
   boot and the serial reader takes it.
3. **VFS's `grant_test` fix is an ordering fix, not a retry** (owner: whoever next adds a blocking
   call to VFS's prologue, or changes when the boot stubs first fault). A stub that faulted before
   VFS's first instruction after `sef_startup` would still win the race. The forbidden `[diag vfs]
   grant.test FAIL` line now names the cause instead of leaving four markers missing in silence.
4. **TTY's consume-after-copy order is still unproved** (6.5's debt 11, the TTY half; owner: the
   slice that next touches TTY's `do_read`). The recipe is this slice's: one `read(0)` into an
   unmapped address in the first proof child, before its real reads, expecting `EFAULT` and then the
   whole probe. Deliberately not applied here — it would have re-shaped 6.5's proof in the slice
   after it settled.
5. **The stopped-device path has no boot and no mutation, in either driver** (owner: none unless a
   device is seen to stop). `EIO` from a stopped device and the wake-up of its waiter are held by
   host tests on `read_plan`, on TTY's `Phase::after` and on `vcon`'s `wake_due`, and by reading the
   call sites in the two `main.rs` files and `irq.rs`. Every trigger is already behind a forbidden
   marker, and QEMU cannot be made to produce one.
6. **A device that never completes a transmit hangs `vcon` with VFS behind it** (owner: whoever
   gives a user-space driver an alarm). The stated cost of every waited request in this tree; RS
   pings no driver, and the symptom is silence.
7. **The pipe chardev is measured on one machine** (owner: this branch's first CI run). C2's four
   conditions hold on macOS with QEMU 11.0.3; the runner's QEMU, firmware and FIFOs are unmeasured,
   and both of CI's full boots share the default FIFO pair, which is safe only because they run one
   after the other.
8. **One waiter, last asker wins, now in two drivers** (6.5's debt 6; owner unchanged). `vcon` uses
   the same `driver-rt::cdev::Waiter`. Unreachable while VFS is each driver's only CDEV client.
9. **Neither proof child runs under `idle-probe`** (6.5's debt 2, extended; owner unchanged).
10. **`cookie_ok` and `reply` are copied into `virtio-console`** (owner: 6.8, the third virtio
    driver). A few lines each, duplicated on purpose so that this slice edited neither `virtio-blk`
    nor `driver-rt` for them.
11. **A device that returns zero-length used elements for ever spins `vcon`'s receive walk** (owner:
    whoever hardens the driver against a hostile host). Each one is re-posted inside the handler;
    nothing caps the re-posts per interrupt.
12. **A feed cut short can leave bytes in `target/hvc0.in`** that reach the next *unfed* local boot,
    whose `/dev/hvc0` child may then print `hvc0 FAIL` (owner: with debt 1). Only fed boots drain.
13. **PM discards the result of its `VFS_PM_EXIT` send** (owner: whoever next touches PM's exit
    path). With `grant_test` first, a process that dies while VFS is blocked sending to PM gets its
    exit SENDREC refused instead, and VFS never resets that descriptor row. Harmless for a boot
    stub, which holds none.
14. **`kernel/src/proc/table.rs` still says only `vblk` has a boot module** (owner: the next slice
    that touches the kernel). Left alone to keep `kernel/src` unchanged by this slice.
15. **`vcon.ds ok` does not police `vcon < vfs`** (mutation 10; owner: none unless the lookup moves
    earlier in VFS's prologue). The order is kept by convention.
16. **The `hvc0` probe reports lost bytes as a hang** (mutation 1; owner: whoever gives the feeder
    or the child a timeout).

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
stack — so **nothing asks the driver to transmit**. The TX proof's frame is built and sent by the
driver itself, once, at boot: a broadcast frame from its own MAC. A slice that finds itself adding a
request so a client can trigger a send has outgrown H10.

**Proof:** the driver's boot frame is captured by QEMU's `filter-dump`; a frame injected from the
host is reported by the driver in `[diag]` with its length and ethertype.

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
larger timeout. *6.3's outcome:* they fit — the per-boot timeout did not move, and the disk boot's
last marker lands at about two thirds of the local log. *6.4's outcome:* three boots per run — the
disk boot, a short reboot of the image it wrote, the rootless-disk fallback boot — and the per-boot
timeout did move, 600 → 1200 s for the two full boots: with a flush after every write the disk
boot's last marker lands about 396 s into a local musl-flavour boot and the ramdisk boot's about 260
s in. A shorter marker set for the second backend was not the answer this time, because the slower
boot is the first backend's. The reboot is the short one, 180 s against its own seven-line set. *As
first written,* the raise was to 900 s, from two figures (82.7% and 59.3% of a 300 s log) that the
slice's review round found to be SDK-flavour boots; re-measured in the flavour CI builds, 900 s was
2.27x rather than the 3.63x it was chosen for, and the slice corrected it to 1200 s (3.03x) before
its first CI run. That run is still the only measurement of the runner's own factor (6.4's debt 13).

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

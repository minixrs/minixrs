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
   has to read the ESP from the same device.
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

A device that is absent is not an error: the driver is told so, publishes nothing to DS, and idles.
That is what makes H5 work.

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
with a device but no root answers `ENXIO` to every BDEV request. MFS asks DS for the virtio name
first, **probes it with one zero-length `BDEV_READ`**, and takes the `memory` name when the lookup
misses or the probe is refused. So the QEMU command line is the selector: with a virtio disk
carrying a root attached the root is on it, otherwise the root is the ramdisk. No kernel command
line, no compile-time feature, no ABI change.

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
as H6 defines it**, not against the raw device. In 6.3 minor 0 is the whole raw image, so the check
reads LBA 0. In 6.4 minor 0 is the partition the GPT lookup selected, so the same check reads that
partition's first and last blocks. Run against LBA 0 of a GPT disk it would find the protective MBR,
decide the disk is not a root, and fall back — which the marker files would catch, but which the
design must not invite.

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

**Minor 0 is the root filesystem, on every block driver.** MFS sends every `BDEV_READ` /
`BDEV_WRITE` as minor 0 and answers `ENXIO` to a `FS_READSUPER` for any other minor; VFS mounts with
minor 0. Rather than change both, minor 0 is *defined* as "the root filesystem this driver serves":
the whole ramdisk on `memory` (as today), the whole raw image on virtio-blk in 6.3, and in 6.4 the
partition **virtio-blk** finds by its **partition type GUID** — never by index. The whole disk is
minor 1 and GPT partition *n* is minor `1 + n`. `BDEV_MINOR_RAMDISK` gains a `BDEV_MINOR_ROOT` alias
with the same value, so new code says what it means and no existing value moves. MFS and VFS are
literally unchanged, which is D3's promise.

The GPT parser is a host-tested module of `driver-rt`; the type GUID is a minix.rs-specific constant
in `kernel-shared`, shared with `tools/mkimage`. Any crate the tool pulls in is BSD/MIT/Apache and
passes `cargo deny`.

*Rejected:* two drives — a `fat:rw:` directory for the firmware and a raw MinixFS disk for the root
— which is where 6.3 starts and deliberately not where the phase ends: the milestone is a machine
that boots from its disk. Also rejected: MINIX-shaped minors (0 = whole disk) with VFS passing the
root's minor in `FS_READSUPER` — more honest numbering, but MFS and VFS both change in 6.4, which
D3's "unchanged MFS" exists to avoid.

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
- **Mask before EOI; quiesce the device, then `IRQ_ENABLE`** (I7, I12). I12's order — drain,
  acknowledge at the device, `IRQ_ENABLE` — fits a device whose acknowledgement *is* the drain, as
  the PL011's is. A virtio-mmio line is level-triggered on `InterruptStatus`, so for virtio the
  acknowledgement precedes the used-ring walk: a completion landing between the last pop and a late
  acknowledgement would be acked away and the re-arm would find the line low. `driver-rt`'s crate
  documentation is the one place the virtio order is written.
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
  verified by reading only.~~ **Paid by 6.3:** every virtio-blk completion takes it (the driver is
  blocked in a source-filtered `RECEIVE` when the line fires), and 6.3's mutation 7 disables it and
  hangs the disk boot at the root check while the ramdisk boot and TTY's `irq ok` are unmoved.
- 6.5 must retire or rescope TTY's post-proof `irq FAIL extra` guard, since RX interrupts will
  legitimately arrive after the TX proof.
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
2. **Sub-word config access** (owner: 6.6 or 6.8, the first driver with 8- or 16-bit config fields).
   `config_read_raw` performs aligned 32-bit reads, valid only for 32- and 64-bit fields; virtio
   requires matching widths for narrower ones, so typed reads are needed.
3. **Queue depth** (owner: 6.9). `VirtQueue`'s bookkeeping arrays are fixed at `QUEUE_SIZE` (8); a
   deeper receive queue needs a const generic.
4. ~~**No "device broken" signal** (owner: 6.3).~~ **Paid by 6.3:** `pop_used` returns `Result<_,
   QueueFault>` (`BadId`, `NotInFlight`, `Runaway`), a fault is sticky, and the driver answers one
   with `virtio FAIL step=io` and `EIO` from then on.
5. ~~**`VirtQueue<'a>` holds `&mut [u8]` over memory the device writes** (owner: 6.3), and
   `VirtQueue::add` / `Transport::queue_enable` are safe functions taking raw physical addresses.~~
   **Paid by 6.3** with the typed handle: `driver-rt`'s `dma.rs` has one `unsafe` constructor
   (`DmaRegion::new`, over the kernel's `GET_DMA` report), every `DmaBuf` is carved from it with
   bounds checked, and the queue and transport take handles — no reference into device-written
   memory exists and no physical address is typed by a driver.
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
   QEMU, so only the exact `features=0x100000000` marker text and two host tests catch it.
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
H5's DS-name selection, **in MFS only** — VFS's `memory` lookup is the `/dev/null` / `/dev/zero`
peer and does not move. The disk is a **raw, whole-disk** MinixFS image — the bytes `build_rootfs`
already produces — written to a file under `target/`. No partitions yet: minor 0 is the whole image
(H6), so H5's root check reads LBA 0.

**Proof:** the full MFS battery — read, `fs.write ok`, create/truncate, the `/etc/holey` hole probe
— is **byte-identical** on the two backends apart from the one line naming the backend. Two boots,
with and without the disk, each with a marker file that requires its own backend. A mutation that
drops the `IRQ_ENABLE` hangs the disk boot at its second request.

**Design:**
[`2026-11-21-virtio-blk-design.md`](../superpowers/specs/2026-11-21-virtio-blk-design.md), decisions
`B1…B11`. **Plan:** [`2026-11-21-virtio-blk.md`](../superpowers/plans/2026-11-21-virtio-blk.md),
whose ledger records every ruling made during the run. **Status: done** (checked in the Status list
above).

**Measured (2026-11-21, QEMU 11.0.3, 300 s local boots).** At the merge base both of two boots pass
148 checks, the last required marker at 48.1% and 55.7% of the log, the IPC counter at 32.3 M and
31.9 M. On the branch the disk boot passes 157 checks and the ramdisk boot 152; the last required
marker lands at 66.8% of the disk log and 48.1% of the ramdisk log (measured before the
`bdev.partial` probe, which adds two device requests). The local 300 s and CI's 600 s per boot both
stay; CI's job `timeout-minutes` goes 50 → 65 for the second boot. `[diag mfs]` and `[diag vfs]`,
compared as each server's first-occurrence stream, are identical on the two backends apart from the
`bdev.ds ok name=` line (`tools/diff-backends.sh`). Every completion interrupt of the driver's (proc
11) is delivered immediately, none deferred.

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

1. **`VIRTIO_BLK_F_RO` is not negotiated** (owner: 6.4, the first slice to request an optional
   feature — 6.2's debt 9 already sends it there). A read-only disk mounts as a root and fails at
   the first write instead of being declined or mounted read-only.
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
replying to every `BDEV_WRITE` — write-through, with no new BDEV request. A device that does not
offer the feature is used read-write anyway, with a `[diag]` line saying so.

**Measure first:** unverified fact 2. If edk2 cannot boot from a modern virtio-mmio disk, the
fallback is a second, firmware-only attachment of the **same** image file — recorded as a deviation,
not adopted quietly.

**Proof:** no `fat:rw:` in the QEMU command line; `/bin/hello` exec'd from the MinixFS partition; a
write to the root survives into a second boot of the same image, which the ramdisk could never show;
the GPT disk's boot reports a root minor whose first block is not LBA 0.

The persistence proof shows that writes **reach the device**, and nothing more. It cannot prove the
flush: with a raw image, QEMU's writes land in the host page cache at once, so a second boot sees
them with or without a guest flush, and `cache=writethrough` would only hide the difference. The
flush is evidenced by its completion status in a `[diag]` count, and dropping it is recorded as a
mutation that moves nothing, with the honest record
[`testing-and-markers.md`](../conventions/testing-and-markers.md#mutation-testing) asks for, rather
than claimed as covered.

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
last marker lands at about two thirds of the local log.

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

# Drivers

A **driver** in minix.rs is an ordinary user-space process that happens to own a piece of hardware.
It is not a special kind of kernel module and it runs no code in the kernel — it is a
`#![no_std]`/`#![no_main]` ELF, loaded from the MXBI archive into its own address space, driving a
SEF receive loop exactly like [a server](../servers/overview.md). Its privilege is a server's too: a
boot driver's image row carries the same full kernel-call mask every boot server gets, so what
confines it is its address space and its interrupt allow-list, not a narrower set of kernel calls.

What separates a driver from a server is one page in its address space that no other process has:
the device's memory-mapped registers. Everything interesting about this chapter follows from that
page.

There are three drivers with an ELF today: **TTY** (`drivers/tty/`), the console; **`memory`**
(`drivers/memory/`), the boot ramdisk plus, since slice 5.11, the
[`/dev/null` and `/dev/zero` character minors](#the-character-minors); and, since slice 6.2,
**`virtio-blk`** (`drivers/virtio-blk/`), which [brings a virtio disk up](#virtio-drivers) and,
since slice 6.3, [serves it as a block device](#virtio-blk) — since slice 6.4 the machine's GPT boot
disk, whose partitions it serves as minors, the root filesystem's among them when the disk carries
one. Three more slots (`vcon`, `vinput`, `vnet`) are wired in the kernel but hold no ELF, and
`drivers/virtio-console` and `drivers/virtio-net` are still empty placeholders. `drivers/driver-rt`
is the runtime the virtio drivers share: the interrupt-line helper, the virtio transport, the typed
handles on DMA memory, the virtqueue, and a GPT parser for block drivers.

`memory` is worth contrasting with the other two up front, because it is the counter-example to the
paragraph above: **its window is ordinary RAM, not MMIO**. It owns no hardware at all. What makes it
a driver rather than a server is its *protocol* — it answers
[BDEV](#the-bdev-protocol-and-the-memory-ramdisk) requests for the ramdisk and
[CDEV](#the-cdev-protocol) requests for its null/zero minors, and knows nothing about what its
blocks contain. That is the property that lets `virtio-blk` serve the same root image underneath an
unchanged MFS: the filesystem asks whichever of the two it selected at boot for blocks, in the same
band, and cannot tell them apart.

## The device window

The kernel and user space agree on a range of virtual addresses reserved for MMIO, declared in
`kernel-shared/src/uspace.rs`:

```rust
pub const USER_DEVICE_WINDOW_BASE: u64 = 0x4000_0000;   // 1 GiB — one whole L1 slot
pub const USER_DEVICE_WINDOW_SIZE: u64 = 0x0100_0000;   // 16 MiB
pub const TTY_UART_VA: u64 = USER_DEVICE_WINDOW_BASE;   // page 0 of the window
```

That module is deliberately separate from `message.rs`, which holds every other shared constant. The
rest of `kernel-shared` describes a *message* ABI — bytes on the wire between two processes, where a
wrong value gets a request rejected. This is an *address* ABI: the kernel installs a mapping and
user code dereferences it, with no message in between, and a wrong value is a data abort.

`0x4000_0000` is chosen to be a whole L1 slot (1 GiB-aligned, so the window costs exactly one L1
entry) and to sit clear of every occupied user VA — server images at 1 MiB, server stacks at 2 MiB,
demo-stub code and stacks at 4 and 8 MiB, VM's heap origin at 16 MiB, VM's mmap arena at 32 MiB.

Two of those grow on request, and a compile-time assert on their *bases* would prove only where they
start. The heap's end is whatever `brk` last asked for, and the mmap arena is a bump allocator that
never reuses addresses — so `servers/vm/src/region.rs` bounds both with a runtime `REGION_LIMIT`
check (`ENOMEM` past it) rather than trusting the 992 MiB of slack. That matters even though nothing
is exploitable today: the window's whole purpose is to be kernel-owned in *every* address space, so
the kernel can pre-map a device page into any driver without first asking whether VM already
promised that VA to the process's heap. Page 0 is TTY's UART by convention; page 1,
`VIRTIO_MMIO_PAGE_VA`, is where every virtio driver finds its register page — each has its own
address space, so one constant serves all four.

## The boot pre-map

TTY has no way to *ask* for its register page. There is no `VMCTL_MAP_PHYS` subcall — a subcall
letting a user-space server name an arbitrary physical address would hand it the whole machine (for
the CPU's view of memory; a virtio driver's *device* has a wider reach, see
[the trust model](#what-the-boundaries-do-not-stop)) — so decision D1 rejected VM-mediated device
mapping and made each device mapping a kernel bring-up step. Slice 6.2's virtio pages are installed
by the same arm, [described below](#virtio-drivers).

The kernel installs it in `load_boot_server` (`arch/aarch64/userland.rs`), right after the ELF and
stack are in place and just before the process is enqueued:

```rust
if nr == TTY_PROC_NR {
    map_page_in(img.ttbr0_pa, TTY_UART_VA, uart::PL011_PHYS_BASE as u64, Prot::DEVICE_RW)
        .expect("TTY UART pre-map");
}
```

Two placement details matter:

- **Not in `load_exec_image`.** That helper is shared with `system::do_exec`, so a device mapping
  there would be inherited by every binary any process ever exec'd. The consequence is worth knowing
  in the other direction too: a process that exec'd would *lose* its device window.
- **No TLB maintenance.** The address space was built moments ago and has never been installed in
  TTBR0 (and a recycled ASID is always clean, because address-space teardown flushes before
  returning the ASID to the pool). Beyond that, `switch_ttbr0_with_asid` — which runs on TTY's first
  schedule — already issues `isb; tlbi aside1; dsb ish; isb`. This is the same reasoning the server
  stack mapping already relies on.

The boot log records it as `[devmap] tty va=0x40000000 pa=0x9000000 attr_idx=<n>`. The two addresses
are fixed constants and the boot-log checker asserts both; `<n>` is whichever MAIR index the scan
below settled on, which depends on what the bootloader programmed, so it is deliberately *not*
asserted.

## Device memory: read the MAIR, never write it

An MMIO mapping must be **Device** memory, not the Normal write-back the rest of user space uses:
the CPU must not cache a register read, must not merge two register writes into one, and must not
reorder a data-register store ahead of the flag-register poll that gates it.

On aarch64 the memory type of a page comes from its descriptor's 3-bit `AttrIndx` field, which
selects one of eight bytes in `MAIR_EL1`. The obvious move — program a byte with a Device encoding —
is the one thing the kernel must not do. Changing byte *i* retroactively changes the memory type of
**every live mapping** that uses `AttrIndx=i`, and that includes Limine's TTBR1 kernel and HHDM
mappings, whose indices this codebase has no way to enumerate. Turning the HHDM into Device memory
would be silent, unrecoverable corruption.

So `mmu::init_device_attr_idx()` **reads** `MAIR_EL1` and reuses an index that already encodes a
Device type. The observation that makes reading sufficient: an *unprogrammed* MAIR byte reads
`0x00`, and `0x00` is itself a valid MMIO encoding (Device-nGnRnE). "An index that already encodes
Device" and "an index nobody uses" therefore coincide — either way, mapping through it is correct.
Reading changes nothing, so no barrier or TLB invalidation is needed.

Only two encodings qualify. `0x04` (Device-nGnRE) is preferred; `0x00` (Device-nGnRnE) is stricter
and accepted. Device-nGRE would permit *gathering*, so two data-register stores could merge into one
and lose a character; Device-GRE would additionally permit reordering the store ahead of its poll.
On QEMU with Limine today the scan settles on index 1 — reported as `[mair] device attr_idx=1
byte=0x00`, which is forensic output and deliberately *not* a boot-log marker, since which index is
free depends on the bootloader.

## `Prot.device` and the RAM/device invariant

`Prot` — the kernel's "what may EL0 do with this page" type — carries a third flag beside `writable`
and `executable`:

```rust
pub struct Prot { pub writable: bool, pub executable: bool, pub device: bool }
```

Adding it was a compile error at every struct literal, which was the point: there were exactly two,
and both had to make a decision. `do_vmctl`'s `VMCTL_PT_MAP` answers `device: false` permanently —
VM may not mint device mappings.

The flag exists because a device leaf's physical address is **not a frame the allocator owns**, so
every path that tears down an address space must not hand it to `free_frame`. Rather than leave that
as a convention, `map_page_in` makes it a total invariant:

```rust
if prot.device { assert!(!is_usable_pa(pa), "device mapping of RAM PA {pa:#x}"); }
else           { assert!( is_usable_pa(pa), "normal mapping of non-RAM PA {pa:#x}"); }
```

Every mapped leaf is therefore provably either `(RAM ∧ ¬device)` or `(device ∧ ¬RAM)`, with no third
case — which is what makes `if !prot.device { free_frame(…) }` *sound* in each of the five leaf
sweeps (exit teardown, fork's copy loop, fork's out-of-memory unwind, `VMCTL_PT_UNMAP`, and the
exec-load error path) rather than merely plausible. `free_frame` keeps its loud out-of-range assert
for the same reason: a catch-all that silently skipped non-RAM frames would demote a forged-address
or double-free bug into an untraceable leak.

Two further consequences:

- **Fork re-maps a device leaf, it does not copy it.** MMIO is inherently shared, and copying 4 KiB
  of live device registers through the cacheable HHDM alias would read side-effecting registers into
  RAM.
- **`mm::uaccess` refuses a device leaf as copy source or destination** (`EFAULT`), closing the hole
  where a process grants a peer its own register window and the kernel then touches MMIO through a
  cacheable alias.

Because TTY never exits, the teardown path's device arm would be untested code sitting on a live
landmine — and a missing guard is a kernel panic, not a leak. So `userland_bootstrap` runs a small
unconditional selftest at boot: build a throwaway address space holding exactly one device leaf,
tear it down, and assert the result. It reports `[devmap] selftest ok freed=0 devs=1`, asserting
**both** numbers — `freed=0` proves the leaf was not freed, and `devs=1` proves it was actually seen
(a guard that skipped every leaf would report `devs=0`).

## The CDEV protocol

Character drivers answer requests in the `CDEV_RQ_BASE = 0xB00` band. Phase 5 defines two, sharing
one payload:

| Field    | Payload offset | Meaning                                                                    |
| -------- | -------------- | -------------------------------------------------------------------------- |
| minor    | `0..4` (i32)   | which device; `CDEV_MINOR_CONSOLE = 0` is the UART                         |
| grant id | `4..8` (i32)   | names the client's buffer (`CPF_READ` for a write, `CPF_WRITE` for a read) |
| length   | `8..12` (i32)  | bytes requested                                                            |
| offset   | `16..24` (u64) | where in the granted range to start                                        |

Five properties of that table are load-bearing.

**There is no granter field.** The driver takes the granter from the kernel-stamped `m_source`. TTY
holds `SYS_SAFECOPY` and its clients do not, so a caller-supplied granter endpoint would let any
client aim a privileged cross-address-space copy at a third party's memory *through* the driver — a
confused deputy. This is the same anti-spoof property `DS_PUBLISH` relies on, and it binds every
grant-id-carrying request in the CDEV, BDEV, and FS bands.

**The reply is a byte count, not a status.** `m_type` comes back as the number of bytes transferred
(written or read; `>= 0`, zero is legal) or a negative errno — which direction depends on the
request, covered in its own paragraph below. A driver replying `OK` would be telling its client that
the whole buffer went out.

**A driver MAY answer short — never must.** The client's contract, POSIX `write()`'s, is to re-send
with `offset` advanced until the request is out; that is what lets a driver stage through a fixed
buffer in its `main` frame with no allocator at all. TTY clamps to `CDEV_MAX_IO` (256 bytes) for
exactly that reason. The memory driver's `/dev/null` and `/dev/zero` (slice 5.11) stage nothing and
never clamp — a `CDEV_WRITE` longer than `CDEV_MAX_IO` still comes back reporting the whole count.

**`CDEV_READ` is the same payload, copy reversed** (slice 5.11). The reply is the byte count read,
`0` is EOF, and a short read is legal — POSIX `read()`'s contract and the one VFS already assumes
for `FS_READ`, so VFS sends one request and reports what came back. It existed only as a plan note
until `/dev/zero` needed it: the 5.3 text said the two devices would be "new minors, not new
requests", which is true of `/dev/null` and of writing `/dev/zero` and false of reading it. TTY does
not serve it until Phase 6 gives it RX (slice 6.5; `SYS_IRQCTL` itself is live as of 6.1, see
[Interrupts reach a driver](#interrupts-reach-a-driver-as-a-hardware-notification)), and answers it
`ENOSYS` from its unknown-request arm until then — VFS routes a console `read()` there anyway, so
Phase 6 changes TTY and nothing else.

**Minors are a per-driver namespace.** TTY's console is 0; the memory driver's `/dev/null` and
`/dev/zero` are CDEV minors 3 and 5 (MINIX 3's `NULL_DEV` and `ZERO_DEV`), on the same driver as
BDEV minor 0's ramdisk. The request band, not the minor value, tells them apart.

## Interrupts reach a driver as a `HARDWARE` notification

Slice 6.1 gives a driver its interrupt line. The mechanism has three parts, all in the kernel, and
one pattern in the driver.

**Claiming.** A driver calls `SYS_IRQCTL` with `IRQ_SETPOLICY`, naming a raw GIC INTID (an SPI, `32
<= vector < nr_intids`) and a `notify_id` in `0..=31`; the reply carries a 1-based hook id. Two
gates apply: the caller's kernel-call mask must admit `SYS_IRQCTL`, and the vector must be on the
caller's per-line allow-list (`Priv::irqs`), which the kernel fills at boot and which has no opt-out
— an empty list denies every line. TTY holds exactly one entry, INTID 33 (the PL011); each virtio
driver holds exactly one, its transport's INTID (`48 + slot`). The hook table (`NR_IRQ_HOOKS` = 16)
allows one owner per line, and a claim under the same `(owner, notify_id)` replaces that hook in
place (answering with the same hook id). The claim unmasks the line at the GIC with priority `0x80`.
A process sharing a privilege slot is refused (`EPERM`), because `int_pending` lives in the slot.
`IRQ_RMPOLICY` masks the line and frees the hook; `IRQ_ENABLE` / `IRQ_DISABLE` unmask and mask it.
`IRQ_REENABLE` (`0x001`) is defined but refused. A hook is also released, with its line masked, when
its owner exits or `exec`s — except that a virtio driver cannot do either,
[see below](#a-virtio-driver-cannot-exit).

**Delivery.** When a hooked SPI fires, `irq_dispatch` **masks the line at the distributor before the
EOI**, sets the hook's `notify_id` bit in the owner's `int_pending`, and `deliver_irq` sends a
`NOTIFY` from `HARDWARE`. The line is level-triggered; EOI on an unmasked line would re-enter
immediately, and the driver cannot have quiesced the device yet. If the owner is blocked in
`RECEIVE` the message is built at once; otherwise the notification is deferred through
`notify_pending` and built by the owner's next `RECEIVE`. Either way the bitmap is *taken* — read
and zeroed — so every line that fired in between arrives in one message, at payload `0..4`.

**The driver pattern.** The line stays masked until the driver says otherwise, so the order inside
the receive loop is load-bearing:

```text
m = receive(ANY)
if m.m_source == boot_endpoint(HARDWARE) && m.m_type == NOTIFY_MESSAGE:
    for each set bit in payload[0..4]:
        drain the device           (RX FIFO until empty / walk the used ring)
        acknowledge at the device  (whatever makes it drop its line)
        irqctl(IRQ_ENABLE, hook)   (only now unmask)
```

A driver identifies the interrupt by the kernel-stamped `m_source`, never by payload: a process
cannot forge `HARDWARE`, but it could set a payload bit. Re-enabling before the device is quiet
would re-enter at once.

That order fits a device whose acknowledgement *is* the drain, as the PL011's is. A virtio-mmio
device's line is level-triggered on its `InterruptStatus` register, so a virtio driver acknowledges
first and walks afterwards (see [the virtio loop](#the-driver-loop)).

**The kernel idles.** With the run queue empty, `schedule_next` no longer returns towards `eret`: it
sets an idling flag, executes `wfi`, polls the GIC through `irq_dispatch` on the kernel stack with
`PSTATE.I` still set, and tries again until a process is picked. The timer tick does only
timekeeping while idling, and a due alarm is what wakes the system. The default boot never reaches
this (init's rotation always leaves something runnable); the opt-in `idle-probe` kernel feature (a
stub-free boot whose `init` parks) proves it with a one-shot `[idle] wake ok`.

## VirtIO drivers

A virtio driver is the same kind of process as TTY, with three things delivered by the kernel
instead of discovered: a register page, an interrupt line and a block of DMA memory. Slice 6.2 built
that machinery around one device, `virtio-blk`; slice 6.3 put block requests through it; slice 6.4
made that device the disk the machine boots from.

### Four slots, one device each

`kernel-shared/src/com.rs` names four driver slots, appended after `init`: `VBLK_PROC_NR` 11
(`vblk`, virtio `DeviceID` 2), `VCON_PROC_NR` 12 (`vcon`, 3), `VINPUT_PROC_NR` 13 (`vinput`, 18) and
`VNET_PROC_NR` 14 (`vnet`, 1). `NR_BOOT_PROCS` is therefore 15, the demo stubs sit at 15–18 and PM's
fork pool starts at 19. Every slot has a `BootEntry` row and a privilege slot, but only `vblk` has
an ELF; the other three stay blocked on `RTS_NO_PRIV` like `PFS`. A boot process's priv id is its
`IMAGE` index, so the four rows take priv ids 16–19 and the stubs' dedicated priv ids sit above the
shared user slot (`USER_PRIV_ID` = 20), at 21–24; two compile-time asserts pin that ordering:
`USER_PRIV_ID >= N_IMAGE` in `kernel/src/proc/table.rs`, and the stub assert in
`kernel/src/arch/aarch64/userland.rs` (gated on `boot-stubs`).

### The kernel finds the device

Nothing in a driver probes for hardware. `userland_bootstrap` runs
`arch::aarch64::virtio_mmio::probe` before it loads any boot server. QEMU `virt` has 32 virtio-mmio
transports at `0x0A00_0000`, `0x200` apart, SPI `16 + slot` (INTID `48 + slot`); the probe reads
`MagicValue`, `Version` and `DeviceID` of each through the HHDM and hands the triples to a pure
function, `kernel-shared::virtio::assign`, which applies four rules in order:

1. A 4 KiB page with more than one *present* device (`MagicValue` right, `DeviceID` non-zero)
   assigns none of them. Eight slots share a page and the kernel maps whole pages, so a driver would
   otherwise be handed a neighbour's registers — and that holds for a neighbour no driver claims,
   such as a stray `virtio-rng-device`.
2. A device no driver serves (an unknown `DeviceID`) is reported and left alone.
3. A device whose driver has no module in the boot archive is reported and left alone.
4. Of the devices left, the lowest slot wins each `DeviceID`; a later one is reported and left
   alone.

The probe prints one line per present device, and a refusal is one line per refused device:

```text
[virtio] slot=0 pa=0xa000000 id=2 ver=2 drv=vblk
[virtio] slot=1 pa=0xa000200 id=4 ver=2 skip=no-driver
[virtio] refuse slot=0 id=2 page=0xa000000 shared
```

(`skip=no-module for=<name>` and `skip=duplicate for=<name>` are the other two skips.) The kernel
does not judge `Version`: it prints it and leaves a legacy transport's refusal to the driver, which
is the one place that knows what the version means and can name the QEMU flag that fixes it. For
that reason `tools/qemu-run.sh` passes `-global virtio-mmio.force-legacy=false` and pins every
device with `bus=virtio-mmio-bus.N`; `N` is the transport slot, and the buses are eight apart so
each device owns its page. All four pages are taken by the four planned devices, so a fifth device
would have no page of its own.

### What the kernel hands over

For a driver the probe assigned, `load_boot_server` adds one arm beside TTY's and the ramdisk's. It
maps the register page `Prot::DEVICE_RW` at `VIRTIO_MMIO_PAGE_VA`, pushes the device's INTID onto
the driver's `Priv::irqs` — so the allow-list is exactly one line long — and maps the driver's
[DMA run](../memory/overview.md#the-dma-window) at `DMA_VA`. It logs two lines, which are required
markers of both boots CI runs, since each attaches the boot disk:

```text
[devmap] vblk va=0x40001000 pa=0xa000000 intid=48
[dma] vblk va=0xc0000000 pages=16 pa=0x...
```

The driver learns the addresses through two more `SYS_GETINFO` selectors,
[`GET_VIRTIO_MMIO` (65) and `GET_DMA` (66)](../reference/syscalls.md#sys_getinfo-selectors). Both
answer `EPERM` to any process that is not one of the four driver slots, and a driver slot with no
device gets `ENOENT` — which is how a driver is told it has no device, not an error: `virtio-blk`
publishes nothing to DS and prints `no device`. The register base a driver uses is the reply's VA,
which already includes the device's offset inside its page.

### What the boundaries do not stop

The register mapping (one page), the interrupt allow-list (one line) and the DMA window (one 64 KiB
run) stop a *buggy* driver from touching another device, another line or another process's memory
*through the CPU*. They do not stop a hostile one. QEMU `virt` as run here has no IOMMU, and once a
queue is enabled with `DRIVER_OK` the descriptor addresses the driver writes are raw guest-physical
addresses that the device will read or write by DMA. A driver can therefore point its device at any
physical page, the kernel's included. A virtio driver is in the trusted computing base for memory
integrity, which fits how the image table treats it: every `SRV_T` row, the four driver rows
included, gets the full kernel-call mask (`kernel/src/proc/table.rs`). The boundaries contain
mistakes; isolating a driver from the rest of the machine needs an IOMMU, which this platform
configuration does not offer.

### `driver-rt`

`drivers/driver-rt/` has five pieces and no `std`:

- **`IrqLine`** (`irq.rs`) is a claim on one interrupt line: `new(intid, notify_id)` issues
  `IRQ_SETPOLICY`, `rearm()` issues `IRQ_ENABLE`, `fired(pending)` tests the line's bit in a
  `HARDWARE` notification. It owns nothing else; the order of draining, acknowledging and re-arming
  stays the caller's, because only the caller knows what acknowledging its device means. TTY uses it
  too.
- **The transport** (`transport.rs`, `mmio.rs`) is the `Transport` trait — status, features, queue
  setup, notify, interrupt status and acknowledge, configuration — with `MmioTransport` as its only
  implementation (a PCI transport for x86_64 is a second one, later). `MmioTransport::new` checks
  the magic and refuses any version but 2. `negotiate` writes the virtio 1.x sequence once for every
  device: reset (and one read-back that the reset took), `ACKNOWLEDGE`, `DRIVER`, the feature
  exchange, `FEATURES_OK` and a read-back. `VIRTIO_F_VERSION_1` is always requested and required; a
  device that clears `FEATURES_OK` is left `FAILED`. `driver_ok` refuses to write if the device
  already shows `FAILED` or `NEEDS_RESET`; `fail` marks a device that was negotiated and then
  abandoned; `config_read` re-reads until the configuration generation is stable, a bounded number
  of times. `MmioTransport::notify` issues a full barrier before its `QueueNotify` write, so the
  store that published the available index is ordered before the device is told to look, whatever
  the caller did in between. Nothing in the crate waits on a register or a ring.
- **Typed DMA handles** (`dma.rs`). The device reads and writes the driver's
  [DMA run](../memory/overview.md#the-dma-window) too, so no Rust reference to that memory may ever
  exist. `DmaRegion::new` is the one `unsafe` constructor: it adopts the run the kernel reported
  (refusing a null, empty or wrapping one from the numbers alone), on the caller's word that the run
  stays mapped for the life of the process, is never referenced, and is adopted only once.
  `DmaRegion::split_queue(len)` consumes the region and returns its first `len` bytes as a
  `QueueMem` — a handle that is not `Copy` and offers no way to write — plus the remainder as a new
  region; it refuses a region that has already handed out a buffer. `DmaRegion::buf(offset, len)` on
  the remainder then hands out bounds-checked `DmaBuf`s, each a virtual address, a physical address
  and a length; a `DmaBuf`'s `read_into` and `write_from` are bounds-checked volatile copies, which
  is how a driver touches a request header or a status byte without an `unsafe` block of its own.
  `DmaRegion::peek` reads the run without creating a handle.
- **The split virtqueue** (`queue.rs`) is `queue_layout` / `clamp_queue_size` (pure arithmetic over
  a power-of-two size up to 32768; `clamp_queue_size` and `VirtQueue::new` cap it at `QUEUE_SIZE` =
  8 entries) and `VirtQueue`, the driver's half of the descriptor table and the available and used
  rings, built from a `QueueMem` rather than a `&mut [u8]` — the device writes that memory, so a
  unique reference was never a contract the driver could keep. `VirtQueue::add` takes `DmaBuf`s
  (device-readable ones, then device-writable ones) and `Transport::queue_enable` takes the
  `VirtQueue`, so every address the device is ever given comes out of a handle. The device is
  untrusted, and a faulty one is reported, never skipped: `pop_used` returns `Err(QueueFault)` for a
  used element naming an out-of-range chain head (`BadId`) or one that is not in flight
  (`NotInFlight`), and for a used index further ahead than the chains in flight (`Runaway`, which is
  also what a completion with nothing in flight is). A fault consumes nothing, so the next
  `pop_used` reports it again, and the driver stops using the device. The driver's own free list and
  chain lengths live outside the shared memory, so a device that scribbles on the descriptor table
  cannot make the walk loop or leave the queue.
- **The GPT parser** (`gpt.rs`, slice 6.4) is apart from the rest: pure functions over byte slices a
  block driver has already read, with no I/O, no device memory and no `unsafe`. `parse_header`
  checks the primary header at LBA 1 against the device's capacity; `check_entries` checks the entry
  array's CRC-32; `partition(n)` decodes entry *n*, **counted from 1** by slot (an all-zero type
  GUID is an unused slot, `Ok(None)`); `find_type` returns the one partition carrying a type GUID,
  `Absent`, or `Ambiguous` when more than one does. All arithmetic is `checked_*`, and its CRC-32 is
  computed a bit at a time rather than from a table, since the input is at most 16 KiB once per
  boot. It shares no code with `tools/mkimage`, which writes the tables. What each refusal means to
  a driver is under [the root check](#the-root-check).

`unsafe` is confined to `mmio.rs` (volatile register accesses) and `dma.rs` (every access to the
memory the device shares); `queue.rs` has none, because its ring accesses go through the handle it
holds, and neither has `gpt.rs`. Because `VirtQueue::new`, `add` and `queue_enable` take handles
instead of addresses, all three are safe functions, and safe driver code cannot aim the device at
memory outside its own run through them — nor behind their backs, because the ring memory was split
off before any buffer handle existed and no `DmaBuf` can overlap it. That is a guard against
mistakes and not a new boundary: [the trust model above](#what-the-boundaries-do-not-stop) is
unchanged, and two data buffers can still overlap each other. `irq.rs` is the only piece that traps
into the kernel. The rest is host-tested against a mock transport and hand-written rings, with a
second handle on the same memory playing the device's half; the parser's tests build headers and
entry arrays by hand, flipping one field at a time and recomputing the CRC so each refusal is for
its stated reason.

### The driver loop

Every virtio driver's receive loop follows one order, written once in `driver-rt`'s crate
documentation: when a `HARDWARE` notification names the line (`IrqLine::fired`), read
`Transport::interrupt_status`, `interrupt_ack` exactly those bits, **then** walk the used ring with
`VirtQueue::pop_used` until it returns `Ok(None)`, and only then `IrqLine::rearm`. An `Err` from the
walk is a [`QueueFault`](#driver-rt): the driver stops using the device and does not re-arm. The
acknowledgement comes before the walk because a virtio-mmio line is level-triggered on
`InterruptStatus`: a completion landing after the last empty `pop_used` but before an
acknowledgement placed after the walk would be cleared by that acknowledgement, and the re-arm would
find the line low and lose the interrupt. Acknowledged first, any later completion re-raises the
line, and the walk, which runs afterwards, still sees everything already published.

### `virtio-blk`

`drivers/virtio-blk/` is the first consumer of `driver-rt` and the second driver on the
[BDEV band](#the-bdev-protocol-and-the-memory-ramdisk). It publishes itself under its proc name,
`vblk`. Since slice 6.4 its disk is the machine's
[boot disk](../boot/overview.md#disk-image-and-qemu): a GPT image whose ESP the firmware has already
read the kernel from, and whose partitions the driver serves as minors. Its pure half is host-tested
— block-to-sector arithmetic, the request header's bytes, what a status byte means, the shape of a
write, when a write is flushed and what a failed flush does to its reply, every root verdict and its
text (`blk.rs`); the minor table and the translation from a request to a sector (`part.rs`); the
feature bits and where the buffers sit in the DMA run (`bringup.rs`). `main.rs` does the kernel
calls, the queue and the interrupt around it.

#### Publishing, then the verdict

A client needs to know two things about this driver, and the driver learns them at different times.

**Whether there is a device** is one kernel call. The `init_fresh` callback asks `GET_VIRTIO_MMIO`
and publishes `vblk` to DS if the kernel assigned the driver a transport; with no virtio-mmio disk
attached (`ENOENT`) it publishes nothing. Nothing in that callback touches the device, so nothing in
it can block.

**Whether the device carries a minix.rs root** takes several block reads — the partition table, then
the root partition's first and last blocks — each a completion interrupt the driver blocks for.
Bring-up and the root check therefore run after `sef_startup`, before the receive loop.

The order is the point. Publishing only after the verdict would lose a race on every boot: while the
driver blocked for its first completion, MFS — next in
[packing order](../boot/overview.md#boot-modules) — would ask DS for `vblk`, find nothing, and mount
the ramdisk. Published first, the name is there when MFS looks, and MFS's first SENDREC to the
driver then blocks in the kernel until the driver reaches its receive loop, which is after the
verdict. So a DS entry for `vblk` means "there is a disk", not "there is a root": a driver with a
device and no root answers every BDEV request `ENXIO`, and that answer is what
[MFS's selection](../servers/overview.md#choosing-the-root-device) reads.

#### Bring-up

Bring-up runs once, in eight steps, and prints exactly one verdict line:

1. `GET_VIRTIO_MMIO`; `ENOENT` prints `no device`. Since the machine boots from its virtio-mmio
   disk, only the hand-run [`MINIXRS_ROOT=nodisk`](../boot/overview.md#the-root-backend) boot, whose
   disk is on PCI, prints it.
2. `GET_DMA`, then check the kernel's stamp. The kernel wrote `pa ^ DMA_COOKIE` into the run's first
   eight bytes through the HHDM; the driver adopts the run as a `DmaRegion`, reads the word through
   it and compares it with the PA it was told. That proves the VA the driver holds and the PA it
   will hand the device are the same memory. (The stamp is zeroed in step 6, when the queue takes
   that page.)
3. `MmioTransport::new`, then `DeviceID` must be 2.
4. `negotiate`, asking for two optional features: `VIRTIO_BLK_F_FLUSH` (bit 9) and `VIRTIO_BLK_F_RO`
   (bit 5). The accepted word is `VIRTIO_F_VERSION_1` plus whichever of the two the device offers,
   and the driver keeps it: it decides [the flush](#flush-before-the-reply) and whether the disk is
   [declined as read-only](#the-root-check).
5. Read the capacity from configuration space, generation-stable.
6. Lay out queue 0 in the first page of the run, enable it, set `DRIVER_OK`, read it back.
7. Claim the INTID, then claim `INTID + 1`, which must be refused with `EPERM`: that is the proof
   the allow-list is one line long.
8. `[diag vblk] virtio ok features=0x100000200 capacity=75776 qsize=8 slot=0`.

`features=0x100000200` is `VERSION_1 | FLUSH`, what QEMU accepts for a writable raw drive; attached
`readonly=on` the same drive gives `0x100000220`. `capacity=` is the whole disk in 512-byte sectors:
75 776 is the 37 MiB image, with or without a root partition. A failing step prints `virtio FAIL
step=<step> …` and abandons the rest without resetting the device or releasing anything; a failure
in step 5 or 6 also marks the device `FAILED`. No step waits on a register or a ring. The driver
still enters its receive loop, with no root to serve. A device brought up without the flush feature
gets one more line, `flush off`, and its writes are served unflushed; QEMU offers the feature on
every drive the runner attaches, so that line is a forbidden marker on every boot.

The run's page 0 is the queue, page 1 holds the 16-byte request header with the one status byte
right after it, and page 2 is the one 4096-byte bounce buffer — a BDEV block on its way to or from
the device. The transport, the queue, the interrupt line, the three buffer handles and the accepted
feature word are kept in `main`'s frame for the life of the process.

`main.rs` holds the crate's `unsafe`: the two `driver-rt` constructors (the DMA run in step 2, the
register window in step 3), and the static buffer the partition table is read into — a
`#[repr(transparent)]` `UnsafeCell` newtype with a hand-written `Sync`, borrowed once per boot. The
buffer is 20 480 bytes, five blocks, which is why it is a static and not a local in a 64 KiB stack.

#### The root check

After a successful bring-up the driver decides, once, whether the disk carries a minix.rs root. It
prints exactly one line, and every read it makes goes through the same function a client request
does, so the interrupt path is proved before the first client and by the same code:

```text
[diag vblk] root ok blocks=256 tail=1 part=2 lba=71680
[diag vblk] root FAIL no root partition
```

In `root ok`, `blocks` is the root partition's size in 4096-byte blocks, `part` is its number in the
partition table (its slot, counted from 1) and `lba` is its first 512-byte sector. `lba=71680` is
the proof that the root's block 0 is not the disk's: a driver that dropped the partition's offset
would read the protective MBR instead of the image label.

The refusals, in the order the checks run — the first that holds is the one printed:

| Line                               | What was wrong                                                                         |
| ---------------------------------- | -------------------------------------------------------------------------------------- |
| `root FAIL geometry capacity=<c>`  | the device is empty or not a whole number of 4096-byte blocks; decided before any read |
| `root FAIL gpt rc=<rc>`            | a read of the partition table's blocks failed                                          |
| `root FAIL gpt <why>`              | the parser refused the primary header, the entry array or an entry (below)             |
| `root FAIL gpt ambiguous`          | more than one partition carries the root type GUID                                     |
| `root FAIL no root partition`      | none does                                                                              |
| `root FAIL align`                  | the root partition does not start and end on a 4096-byte block boundary                |
| `root FAIL readonly`               | the device is read-only (`VIRTIO_BLK_F_RO` was accepted)                               |
| `root FAIL head rc=<rc>`           | the read of the root partition's first block failed                                    |
| `root FAIL label`                  | that block does not start with the image label                                         |
| `root FAIL blocks hdr=<h> dev=<d>` | the image header's block count is not the partition's                                  |
| `root FAIL tail rc=<rc>`           | the read of the root partition's last block failed                                     |
| `root FAIL tail label`             | that block does not start with the tail label                                          |

**The partition table.** The driver reads whole-disk blocks 0–4 (LBA 0–39), which hold the
protective MBR, the primary GPT header and all 128 partition entries, and hands the bytes to
`driver-rt`'s [GPT parser](#driver-rt). The parser checks the header in a fixed order, and `<why>`
is the first check that fails: `header short`, `signature`, `revision` (must be 1.0), `header size`
(92 bytes up to one sector), `crc` (the header's CRC-32), `my lba` (must be 1), `entry size` (must
be 128), `entry count` (1 to 128), `entry lba` (the array must start at LBA 2), `usable range`
(`FirstUsableLBA ≤ LastUsableLBA <` the disk's size). Then the entry array: `entries short`,
`entries crc`. Then each used entry — an all-zero type GUID is an unused slot — whose range must not
be inverted and must lie inside the usable range: `entry range`, which condemns the whole table
rather than the one entry. (`entry index`, the parser's refusal of a partition number outside the
table, is a caller's mistake the driver does not make.) The signature is checked first, so a disk
with no GPT at all — a raw filesystem image, for instance — reads as `gpt signature` and not as a
bad checksum.

**Only the primary header and entry array are consulted.** The backup copy at the end of the disk is
never read. A disk whose primary table is damaged has no root as far as this driver is concerned,
and MFS falls back; recovering from the backup means deciding which copy to believe and writing the
repair, which is a tool's job. In practice the firmware
[does that job first](../boot/overview.md#the-firmware-writes-to-the-disk-too) on a writable disk,
so these refusals are seen only on a read-only one.

**The root is selected by type, never by position.** The driver looks for the one partition whose
type GUID is `kernel-shared`'s `ROOT_PARTITION_TYPE_GUID`. Two partitions of that type is a refusal,
not "the first one": selecting by type exists so that position does not matter, and breaking a tie
by position would bring it back.

**A read-only disk is declined, not mounted.** Nothing above the driver can mount a filesystem
read-only, so a disk that reports `VIRTIO_BLK_F_RO` is refused as a root and MFS falls back to the
ramdisk. The check comes after the table and before the labels: a disk with no acceptable table is
refused for that first.

**The label checks are the `memory` driver's**
[device-level checks](#the-driver-has-no-unsafe-block), applied to the root *partition*: its first
block must start with the image label and carry a block count equal to the partition's own, and its
last block must start with the tail label. The driver decodes no superblock and does not depend on
the filesystem format. These reads are also the first proof that the run's *data* page is the memory
the device writes by DMA; the cookie in step 2 proved that only for the run's first page.

#### Minors

A disk that passed the root check is served as a table of extents — runs of whole 4096-byte blocks
on the disk — indexed by minor:

| Minor   | Extent                                                              |
| ------- | ------------------------------------------------------------------- |
| 0       | the root partition — `BDEV_MINOR_ROOT`, selected by its type GUID   |
| 1       | the whole disk — `BDEV_MINOR_WHOLE_DISK`                            |
| `1 + n` | GPT partition *n*, its slot counted from 1, for each **used** entry |

On the boot disk that makes minor 2 the ESP, and minors 0 and 3 the same blocks. A partition whose
first sector or length is not a multiple of eight sectors has no minor at all (`ENXIO`); if that
partition is the root, the disk is refused (`root FAIL align`). Reads and writes are served alike on
every minor.

A request's block number is relative to its minor's extent. The driver decides everything about a
request before it touches the device, in one function (`MinorTable::locate`) and in this order:

1. the minor has no extent → `ENXIO`;
2. `server-rt`'s shared `bdev::validate_range`, against **that extent's** block count → `EINVAL` for
   a bad length, grant id or block — the same checks, in the same order, that `memory` runs;
3. a zero-length request → answered `0`;
4. otherwise the extent's first block is added, by `checked_add`, and the result converted to a
   sector.

The offset is added only after the range check, and the translation refuses a block outside the
extent a second time on its own, so a request can never reach past its partition into the next one.

**With no root there are no minors at all.** A driver with a device but no root answers `ENXIO` on
every minor — the whole disk included.

Nothing in the system mounts any minor but 0. So that the table is not code no boot runs, MFS's
prologue issues the system's one request for another minor,
[`bdev.minor`](#what-the-boot-log-proves-1): it reads block 0 of minor 1 and checks that a
protective MBR came back.

#### One request in flight

Every device transfer is one whole block, eight 512-byte sectors, through the bounce buffer; block
`b` starts at sector `8 * b`, computed with `checked_mul`. One request to the device goes like this:

1. Write the header (`type`, a reserved word, `sector`, all little-endian) and preset the status
   byte to `0xFF`, a value the specification defines no meaning for.
2. `add` the chain and `notify` the device. A read or a write is three buffers — header, data,
   status, with the data buffer device-writable for a read and device-readable for a write. A flush
   is two: header and status, no data.
3. Block in a receive **filtered to `HARDWARE`**. For each notification naming the line, follow
   [the driver loop](#the-driver-loop): read `InterruptStatus`, acknowledge exactly those bits, walk
   the used ring, re-arm.
4. Repeat 3 until the request's own chain head has come back, then read the status byte.

There is one wait loop; a read, a write and a flush all complete through it. The filter in step 3 is
what lets "one request in flight" need no queue in the driver. A second client's request stays a
blocked sender in the kernel until the main loop receives again. The wait is a blocked receive,
never a poll. A device that never completes a request hangs the driver there, with its client in
SENDREC behind it, and nothing reports it: RS's heartbeat covers DS, VM, SCHED, VFS and PM, not
drivers. The symptom is silence — from `vblk` and from everything behind MFS, so the boot's marker
set goes missing — and there is no timeout because no user-space driver holds an alarm.

An interrupt that arrives in the main loop, with nothing in flight, gets the same acknowledge, walk
and re-arm and is otherwise ignored — a configuration-change interrupt is legal. A *used element*
arriving with nothing in flight is not: the queue reports it as a fault.

On top of that, the three request shapes:

- **`BDEV_READ`**: the device fills the bounce buffer, then `SAFECOPY_TO` pushes the client's `len`
  bytes into its grant.
- **`BDEV_WRITE` of a whole block**: `SAFECOPY_FROM` pulls the client's bytes into the bounce buffer
  first, then the device write. A refused grant or an unmapped buffer is relayed and the device is
  never written.
- **`BDEV_WRITE` of part of a block**: read the block into the bounce buffer, pull the client's
  bytes over its first `len`, write it back — two device round trips, so that the rest of the block
  survives as [the protocol requires](#the-bdev-protocol-and-the-memory-ramdisk).

A zero-length request of either kind is answered `0` with no grant use and no device I/O.

#### Flush before the reply

When the device accepted `VIRTIO_BLK_F_FLUSH`, every `BDEV_WRITE` whose device write succeeded is
followed by a `VIRTIO_BLK_T_FLUSH` — request type 4, sector 0, the two-descriptor chain above —
**before the client is answered**. A client is told its write succeeded only after the device
reported its cache committed, and there is no BDEV flush request for a client to forget. A flush
that fails turns the write's reply into `EIO`. No flush follows a zero-length write, a refused
grant, a failed read-before-write or a failed device write: nothing new is in the device's cache
then.

The first successful flush of a boot prints one line, and no later one prints anything:

```text
[diag vblk] flush ok n=1
```

`n` is the driver's own count at that moment, never a running total, so the line is a stable marker.
A flush the device completes with any other status prints `flush FAIL status=<s>` every time, the
raw status byte in hexadecimal (`0xff` is the preset: the device never wrote it). A flush lost to a
[broken device](#what-a-client-hears) has no status byte and prints no line of its own; `virtio FAIL
step=io` has already said why, and the write still answers `EIO`.

**What `flush ok n=1` proves, and what it does not.** It proves that a write reached the device and
that a flush request was issued and completed with status 0. It does not prove that anything reached
stable storage: QEMU serves a raw image through the host's page cache, so a second boot of the same
image sees the first boot's writes with or without a guest flush. Dropping the flush from the write
path moves this one marker and nothing else — the image still
[reboots with its writes intact](../boot/overview.md#booting-the-same-image-twice) — and a driver
that treated a failed flush's status as success would move nothing at all under QEMU, whose flush
cannot be made to fail. The decisions around the flush are host-tested; the device's side of it is
taken on its status byte.

#### What a client hears

- **`ENXIO`** for every BDEV request, on every minor, when there is no root — no device, a failed
  bring-up, or any `root FAIL` above — and for a minor that has no extent on a disk that has a root.
- **`EINVAL`** for a malformed request on a minor that exists, decided by the range checks `memory`
  shares.
- **`EIO`** when the device failed a well-formed request: a status byte other than `0` — the
  device's `IOERR` or `UNSUPP`, or the preset `0xFF` still there because the device completed the
  request without writing it — a failed flush after a write, or a broken device.
- A negative `SYS_SAFECOPY` result, **verbatim**.
- **`ENOSYS`** for any other request number.

A device is *broken* once it can no longer be believed or waited for: a `QueueFault` from the walk,
a line that cannot be re-armed, a failed receive, or a queue that refuses a chain with nothing in
flight. The driver then writes `FAILED`, prints one `virtio FAIL step=io …` line naming the cause,
leaves the line masked, and answers `EIO` to every later request.

### A virtio driver cannot exit

The DMA run is ordinary RAM, so every address-space teardown would free it while the device may
still be writing to it. No device-reset path exists yet, so the kernel guards the three ways the run
could be freed. `do_exit` and `do_exec` `assert!` that the process holds no virtio assignment, so
the kernel halts with a message naming the missing reset path; and `VMCTL_PT_UNMAP` answers `EPERM`
for an address inside the DMA window of a process that holds one. Boot drivers never exit, so none
of the guards fires on a healthy boot; a mutation that made `vblk` send `PM_EXIT` showed the exit
guard firing.

## TTY

`drivers/tty/` is three files:

- **`cdev.rs`** — the pure, host-tested half: `validate_write` applies the checks in order (unknown
  minor → `ENXIO`, negative length → `EINVAL`, invalid grant id → `EINVAL`, then clamp to
  `CDEV_MAX_IO`) (the four-field parse moved to `server-rt::cdev` in 5.11, when the memory driver
  became its second user). It is a total function, so a malformed request becomes an invalid *value*
  the validator rejects, never a panic.
- **`pl011.rs`** — the crate's only `unsafe`: volatile accesses to `FR` and `DR` at `TTY_UART_VA`,
  polling `FR.TXFF` before each store, translating LF to CRLF. The register offsets are deliberately
  duplicated from the kernel's own PL011 writer; they cannot be shared, because the kernel crate is
  bare-metal-only and pinned by `forced-target`, so it can never be a user-space dependency — and a
  register layout is a hardware fact, not a shared ABI.
- **`main.rs`** — the SEF loop and the handler.

The handler captures `m_source` **first** (it is both the reply target and the granter), validates,
pulls the bytes across with `SYS_SAFECOPY(SAFECOPY_FROM, caller, gid, offset, staging, n)`,
transmits, and replies `n`. A negative `SYS_SAFECOPY` result is relayed **verbatim**: `EPERM` ("your
grant does not authorize this") and `EFAULT` ("your buffer is not mapped") are different bugs on the
client's side.

Two departures from the server template are worth naming. The staging buffer lives in `main`'s
frame, never in the init callback's — the kernel writes into it while TTY is blocked inside the
`SYS_SAFECOPY` SENDREC, so the frame must outlive every call that names it (the same rule
`GrantPool` follows). And an **unknown `m_type` gets a reply**, where DS harmlessly drops one: a
driver's clients all SENDREC, so a dropped request blocks the caller forever.

## What the boot log proves

TTY writes its own banner straight to the UART once its mapping is in place:

```
minix.rs console: tty online (EL0)
```

That line is the milestone, and it is identifiable for a specific reason: it carries **no kernel
trace prefix**. Every other line in the log is `[as]`, `[ipc]`, `[ksys …]`, `[diag …]`, `[pf]`,
`[devmap]` — kernel-formatted. This one was composed at EL0 and reached the wire through a
user-space store to a device register.

VFS then drives the protocol as the first client: it resolves TTY through DS (rather than
hard-coding its boot endpoint), writes a banner through a read-only direct grant and checks the
reply against the granted length, asks for `CDEV_MAX_IO + 8` bytes and requires exactly
`CDEV_MAX_IO` back, and finally issues two requests that must be *refused* — minor 7 with a
perfectly good grant (`ENXIO`, from TTY's own minor check) and a grant issued to PM instead of TTY
(`EPERM`, from the kernel's grantee check, the property that makes a grant id safe to pass around at
all).

One honest caveat: under QEMU's TCG the Device *attribute* is not observably load-bearing. QEMU's
PL011 works through a Normal write-back mapping — which is why the kernel's own HHDM alias has
always been one — so substituting the Normal attribute index changes no marker in the boot log. The
attribute is proved by construction and assertion, not empirically. The same is true of the
`FR.TXFF` poll (TCG's FIFO never fills) and of the LF→CRLF translation (the log checker matches
literal substrings and cannot express a carriage return).

## The BDEV protocol and the `memory` ramdisk

Block drivers answer requests in the `BDEV_RQ_BASE = 0xA00` band — between VFS (`0x800`) and CDEV
(`0xB00`), with FS (`0x900`) between VFS and BDEV. Two requests are defined:

| Field    | Payload offset | Meaning                                               |
| -------- | -------------- | ----------------------------------------------------- |
| minor    | `0..4` (i32)   | which device; `BDEV_MINOR_ROOT = 0` is the root image |
| grant id | `4..8` (i32)   | names the client's buffer                             |
| length   | `8..12` (i32)  | bytes requested; at most `BDEV_MAX_IO` = one block    |
| block    | `16..24` (u64) | which block of the device                             |

`BDEV_READ` fills the client's buffer (so the grant needs `CPF_WRITE`, and the driver pushes with
`SAFECOPY_TO`). `BDEV_WRITE`, real since slice 5.10a, is the same request read backwards: it pulls
the client's bytes into the device with `SAFECOPY_FROM`, so the grant has to carry `CPF_READ`
instead. One parse and one validation serve both — the payload is identical, and only the dispatch
arm knows which way the bytes go. The driver never checks the access bit itself, and must not: the
kernel's `verify_grant` does, and a driver that re-derived the grant rules would be a second place
for them to drift.

Two drivers serve the band: `memory`, described in the rest of this section, and
[`virtio-blk`](#virtio-blk). On both, minor 0 is "the root filesystem this driver serves" —
`BDEV_MINOR_ROOT`, an alias of the older `BDEV_MINOR_RAMDISK` with the same value — so a client
names the root the same way whichever driver backs it. Which *other* minors exist is each driver's
own: `memory` has none and answers `ENXIO` for any other, while `virtio-blk` serves the whole disk
as minor 1 (`BDEV_MINOR_WHOLE_DISK`) and each GPT partition after it ([Minors](#minors)), and
answers `ENXIO` for a minor with nothing behind it.

**The parse and the validation are shared, not copied.** `server-rt::bdev`'s `parse_read` serves
both drivers, and so do the checks, which run in a fixed order — minor, negative length, over-long
length, invalid grant id, zero length (answered `0`), then the block range — so both drivers refuse
the same request with the same errno, which is what lets MFS's denial battery read identically on
either. Since slice 6.4 they are two functions, because the first check is the one thing that
differs by driver. `validate_range(req, blocks)` is everything after the minor check, against a
block count, and never reads the minor. `validate_read(req, blocks)` is the minor check for a driver
whose only minor is the root, followed by `validate_range`; `memory` calls it with the image's block
count, as it always has. `virtio-blk` resolves the minor to a partition's extent itself and then
calls `validate_range` with that extent's block count. CDEV shares only its parse through
`server-rt::cdev`.

Most of that table repeats CDEV's rules — no granter field, and the reply `m_type` *is* the byte
count. Four things are deliberately different:

**An over-long request is `EINVAL`, not a short read.** A short *write* is a POSIX contract every
client already loops over. A short *block read* is useless: a filesystem cannot interpret half a
block, so clamping would push a retry loop into every caller for nothing.

**An out-of-range block is `EINVAL`, not `EIO`.** A block device's size is known to its client — MFS
reads it from the superblock's `s_zones` — so asking past the end is a caller bug. `EIO` means the
request was well-formed and the *device* failed. `virtio-blk` answers it for a failed transfer, a
failed flush after a write, or a broken device; `memory` has no device to fail and never does.

**A short write replaces the start of the block and leaves the rest alone.** A `BDEV_WRITE` of fewer
than 4096 bytes stores its bytes at the beginning of the block and must not disturb the remainder.
On `memory` that is simply a shorter copy into the ramdisk; on a sector device it is a
read-modify-write. MFS never issues one — it always stores a whole block — so its boot prologue
carries a probe that does, [`bdev.partial`](#what-the-boot-log-proves-1).

**`BDEV_WRITE` was numbered three slices before it worked.** From 5.7 to 5.10a the arm answered
`EROFS` rather than `ENOSYS`, because `ENOSYS` is already the unknown-`m_type` answer and reusing it
would have made "this driver has never heard of writes" and "this driver knows about writes and
refuses them" indistinguishable to a client. Keeping the arm dispatched also kept it probed — and
the prediction held exactly: making the ramdisk writable changed one line inside it, the direction
handed to `sys_safecopy`.

### Where the blocks come from

`kernel/build.rs` builds a MinixFS v3 image at compile time (`tools/mkfs-mfs`, called as a
build-dependency library) and packs it into the MXBI archive as a **non-ELF blob** named `rootfs`.
It writes the same bytes, from the same call, to `rootfs.img` beside the kernel ELF, which is the
file `tools/mkimage` copies into
[the boot disk's root partition](../boot/overview.md#the-disk-image) — so the two drivers serve one
image and cannot differ in content.

At boot the kernel copies the blob, page by page, into freshly allocated frames and maps them into
the `memory` driver's address space — in the same `load_boot_server` arm the UART page uses, and for
the same reason: the driver has no way to ask.

The window is declared beside the device window, and is **ordinary RAM**:

```rust
pub const RAMDISK_WINDOW_BASE: u64 = 0x8000_0000;   // 2 GiB — one whole L1 slot
pub const RAMDISK_WINDOW_SIZE: u64 = 0x0040_0000;   // 4 MiB
pub const RAMDISK_VA: u64 = RAMDISK_WINDOW_BASE;    // page 0 of the window
```

Two choices there are worth stating. It sits **above** the device window on purpose:
`region::REGION_LIMIT` is the base of the *lowest* kernel-owned window, so placing every new window
above that low-water mark means VM needs no edit at all when one is added — and
`assert!(USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= RAMDISK_WINDOW_BASE)` is what keeps
that true. And the pages are mapped `Prot::RW_DATA`, not device: none of the `prot.device` machinery
above applies, and these frames take the ordinary `free_frame` path in every leaf sweep. (RW rather
than RO so slice 5.10's write path is a change in the driver rather than in the kernel.)

The 4 MiB size comes from the *format*, not from today's image: seven direct zones plus one
single-indirect block address `7 + 1024` zones at 4 KiB, which is 4.03 MiB. An image that fits the
window is therefore an image `fs/mfs`'s reader can address without a double-indirect arm.

The driver learns where it is through a new `SYS_GETINFO` selector, `GET_RAMDISK`, which returns
`(va, len)` and is **gated on the caller being `MEM_PROC_NR`**: the ramdisk is mapped into exactly
one address space, so the VA is meaningless — and actively misleading — anywhere else.

### The driver has no `unsafe` block

`drivers/memory/` never dereferences its mapping. Client transfers go through `SYS_SAFECOPY`, and
even the boot self-check reads the image through `SYS_COPY(SELF → SELF)` rather than a raw load. So
a page the kernel failed to map surfaces as an `EFAULT` *return value* from a kernel call — a better
diagnostic than TTY's equivalent, which is an EL0 data abort — and there is no MMIO sibling module
to exclude from coverage.

The self-check is deliberately **device-level, not format-level**: it reads a 32-byte image header
that `mkfs-mfs` writes into block 0's boot block (bytes `0..1024`, which MinixFS never reads), never
a superblock. A block driver that decoded a superblock would depend on the filesystem format, and
`virtio-blk` would then have needed the same dependency to serve the same image; instead it applies
these same checks to the root partition it reads off the disk. What licenses that shortcut is a host
test in `tools/mkfs-mfs` asserting the header's three fields equal the real superblock's.

It also reads a **tail label** from the image's reserved last block, whose text differs from the
header's. That is not decoration: a kernel copy loop that failed to advance would map 256 pages of
block 0 and pass every header check. The tail is the only thing in the boot that proves the copy
reached the end of the blob — confirmed by mutation, where sourcing every page from block 0 moved
exactly one marker, `ramdisk FAIL tail label`.

### The character minors

Since slice 5.11 the same driver serves `/dev/null` (CDEV minor 3) and `/dev/zero` (minor 5), as
MINIX 3's memory driver does beside its ramdisks. Minors are a per-driver namespace, and on a driver
serving two bands, the band tells them apart — so `cdev::classify` refuses minor 0 here, which is
TTY's console, and the BDEV ramdisk's minor 0 never meets these two.

Both minors discard a `CDEV_WRITE` and answer the **whole** count with no copy at all; `/dev/null`
answers a `CDEV_READ` with `0`, and `/dev/zero` fills the whole request from a 256-byte static,
walking the grant in `CDEV_MAX_IO` steps. Nothing is clamped: `CDEV_MAX_IO` protects TTY's stack
staging buffer, and there is no staging here. Two consequences worth knowing. A `/dev/null` write
with an unmapped buffer *succeeds*, as it does on Linux, because nothing reads the buffer — so no
bad-buffer probe may ever be aimed at it. And the driver still has no `unsafe` block: the only copy
is a kernel call.

VFS probes the validator from its prologue (`[diag vfs] mem.deny ok n=5`), because VFS's own device
table maps only minors that exist and could never send a bad one. One of those five is the first
such refusal on the CDEV band: a `CDEV_READ` through a read-only grant, refused by the kernel's
`verify_grant` and relayed as `EPERM`. MFS's `bdev.deny` battery already exercises the same
`CPF_WRITE` check against a `BDEV_READ` (its `read-only` probe), so this is a CDEV-band first, not a
kernel-wide one.

### What the boot log proves

```
[ramdisk] mem va=0x80000000 len=1048576 pages=256
[diag memory] ramdisk ok blocks=256 tail=1
[diag mfs] bdev.root vblk rc=-6 fallback=memory
[diag mfs] bdev.ds ok name=memory ep=3
[diag mfs] bdev.tail ok match=1
[diag mfs] bdev.partial ok n=16
[diag mfs] bdev.minor absent minor=1 rc=-6
[diag mfs] bdev.deny ok n=10
```

Those are the lines of a `MINIXRS_ROOT=ramdisk` boot, where the boot disk carries no root partition
and MFS fell back. On the default boot, whose disk does carry one, the first two still print,
because the ramdisk is built and checked on every boot; the `bdev.root` line is absent; the
`bdev.ds` line reads `name=vblk ep=11`, naming the driver MFS
[selected](../servers/overview.md#choosing-the-root-device); and the `bdev.minor` line reads `ok
minor=1 mbr=1`. Every other `[diag mfs]` line is the same on both. (With no virtio-mmio disk at all
— the hand-run `nodisk` boot — the listing is the ramdisk boot's without its `bdev.root` line.)

`blocks=256` cross-checks the header's own block count against the length `GET_RAMDISK` reported —
two independently derived numbers, binding the build-time image geometry to the kernel's runtime
copy.

MFS, not VFS, is this driver's BDEV client — since slice 5.8, when the filesystem server took over
the band. Its own `mount ok root=1 bs=4096 blocks=256` marker, the superblock decoded out of the
block it asked for, is what retired the earlier `bdev.read`/`bdev.head` pair: a driver that replied
`OK` to the wrong page, or returned the header for both, fails to decode there instead of printing a
marker of its own. `bdev.tail` is the one thing `mount` cannot subsume — it reads the image's
reserved *last* block, whose label differs from the header's, and so proves that a request's `block`
field reaches the right block of the device. It is not the proof that the kernel's copy loop reached
the end of the blob: that is the driver's own `tail=1` (above), and on a disk boot `bdev.tail` never
touches the ramdisk at all. `bdev.partial` is the short-write contract: MFS reads block 0, which
leaves a superblock in whatever buffer the driver staged it through, then writes the tail block's
own first 16 bytes back to it as a partial `BDEV_WRITE`, reads the tail again, and requires the
label intact and every byte past the header still zero. A sector driver that skipped the read half
of its read-modify-write would have stored block 0's bytes there (`bdev.partial FAIL clobbered`);
the first read is what makes that visible, and the probe checks that block 0 really is non-zero past
its header rather than assuming it. `bdev.minor` is the one request in the system for a minor other
than the root: a whole-block read of block 0 of minor 1, the whole disk. `virtio-blk` answers it
with the disk's first block, and MFS prints `ok minor=1 mbr=1` only if that block starts with a
**protective** MBR — the `55 AA` signature at offset 510, a first partition record of type `0xEE` at
offset 450, and no image label at offset 0. The three checks tell the whole disk from the two
extents a wrong minor table could have served instead: the root partition's block 0 has neither the
signature nor the record, and the ESP's has the signature — a FAT boot sector ends in the same two
bytes — but no `0xEE` record (`bdev.minor FAIL mbr`). `memory` has no such minor and answers
`ENXIO`, which is the healthy `absent minor=1 rc=-6` of the ramdisk boot; any other reply is
`bdev.minor FAIL read rc=<rc>`. The verdict is `minor::classify` in MFS's library, host-tested. The
ten refusals in `bdev.deny` include the one grant check slice 5.3 could not reach — a
`CPF_READ`-only grant used as a copy *destination* — plus, since slice 5.10a, two `BDEV_WRITE`
probes aimed at the write path this driver now really implements, refused by the kernel's
grant-direction and minor checks rather than by a since-retired `EROFS` stub.

Every one of those markers is identical with and without the `boot-stubs` feature **and** with and
without the musl sysroot, and all but the three backend-specific lines — `bdev.root`, the `name=` of
`bdev.ds`, and the `bdev.minor` verdict — are identical on both root backends. That is what the
fixed 1 MiB image size buys: in the sysroot-absent build the archive packs a 15 KB `worker` ELF
under the name `hello`, so a content-sized image would make every size-derived marker
config-dependent — passing in one configuration and proving nothing in the other.

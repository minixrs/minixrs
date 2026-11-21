# Drivers

A **driver** in minix.rs is an ordinary user-space process that happens to own a piece of hardware.
It is not a special kind of kernel module and it holds no kernel privilege beyond a `k_call_mask`
bit or two — it is a `#![no_std]`/`#![no_main]` ELF, loaded from the MXBI archive into its own
address space, driving a SEF receive loop exactly like [a server](../servers/overview.md).

What separates a driver from a server is one page in its address space that no other process has:
the device's memory-mapped registers. Everything interesting about this chapter follows from that
page.

There are three drivers with an ELF today: **TTY** (`drivers/tty/`), the console; **`memory`**
(`drivers/memory/`), the boot ramdisk plus, since slice 5.11, the
[`/dev/null` and `/dev/zero` character minors](#the-character-minors); and, since slice 6.2,
**`virtio-blk`** (`drivers/virtio-blk/`), which [brings a virtio disk up](#virtio-drivers) and
serves nothing yet. Three more slots (`vcon`, `vinput`, `vnet`) are wired in the kernel but hold no
ELF, and `drivers/virtio-console` and `drivers/virtio-net` are still empty placeholders.
`drivers/driver-rt` is the runtime the virtio drivers share: the interrupt-line helper, the virtio
transport and the virtqueue.

The two are worth contrasting up front, because `memory` is the counter-example to the paragraph
above: **its window is ordinary RAM, not MMIO**. It owns no hardware at all. What makes it a driver
rather than a server is its *protocol* — it answers
[BDEV](#the-bdev-protocol-and-the-memory-ramdisk) requests for the ramdisk and
[CDEV](#the-cdev-protocol) requests for its null/zero minors, and knows nothing about what its
blocks contain. That is exactly the property Phase 6 needs, when virtio-blk replaces it underneath
an unchanged MFS.

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
instead of discovered: a register page, an interrupt line and a block of DMA memory. Slice 6.2
builds that machinery and proves it with one device, `virtio-blk`, answering.

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
boot markers:

```text
[devmap] vblk va=0x40001000 pa=0xa000000 intid=48
[dma] vblk va=0xc0000000 pages=16 pa=0x...
```

The driver learns the addresses through two more `SYS_GETINFO` selectors,
[`GET_VIRTIO_MMIO` (65) and `GET_DMA` (66)](../reference/syscalls.md#sys_getinfo-selectors). Both
answer `EPERM` to any process that is not one of the four driver slots, and a driver slot with no
device gets `ENOENT` — which is how a driver is told there is no disk, not an error: `virtio-blk`
prints `no device` and idles. The register base a driver uses is the reply's VA, which already
includes the device's offset inside its page.

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

`drivers/driver-rt/` has three pieces and no `std`:

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
  of times. Nothing in the crate waits on a register or a ring.
- **The split virtqueue** (`queue.rs`) is `queue_layout` / `clamp_queue_size` (pure arithmetic over
  a power-of-two size up to 32768; `clamp_queue_size` and `VirtQueue::new` cap it at `QUEUE_SIZE` =
  8 entries) and `VirtQueue`, the driver's half of the descriptor table and the available and used
  rings over a byte buffer it shares with the device. The device is untrusted: a used element naming
  an out-of-range or not-in-flight chain head is skipped, and the driver's own free list and chain
  lengths live outside the shared memory, so a device that scribbles on the descriptor table cannot
  make the walk loop or leave the queue.

`unsafe` is confined to `mmio.rs` (volatile register accesses) and `queue.rs` (volatile accesses to
the ring memory the device shares); `irq.rs` is the only piece that traps into the kernel. The rest
is host-tested against a mock transport and hand-written rings.

### The driver loop

Every virtio driver's receive loop follows one order, written once in `driver-rt`'s crate
documentation: when a `HARDWARE` notification names the line (`IrqLine::fired`), read
`Transport::interrupt_status`, `interrupt_ack` exactly those bits, **then** walk the used ring with
`VirtQueue::pop_used` until it returns `None`, and only then `IrqLine::rearm`. The acknowledgement
comes before the walk because a virtio-mmio line is level-triggered on `InterruptStatus`: a
completion landing after the last empty `pop_used` but before an acknowledgement placed after the
walk would be cleared by that acknowledgement, and the re-arm would find the line low and lose the
interrupt. Acknowledged first, any later completion re-raises the line, and the walk, which runs
afterwards, still sees everything already published.

### `virtio-blk` in 6.2

`drivers/virtio-blk/` does its bring-up once, after `sef_startup` (not inside the `init_fresh`
callback, where a failure would leave the driver spinning), in eight steps, and prints exactly one
verdict line:

1. `GET_VIRTIO_MMIO`; `ENOENT` prints `no device` and idles.
2. `GET_DMA`, then check the kernel's stamp. The kernel wrote `pa ^ DMA_COOKIE` into the run's first
   eight bytes through the HHDM; the driver reads it through `DMA_VA`, compares it with the PA it
   was told, and zeroes it. That proves the VA the driver holds and the PA it will hand the device
   are the same memory.
3. `MmioTransport::new`, then `DeviceID` must be 2.
4. `negotiate` with no optional feature, so the accepted word is exactly `VIRTIO_F_VERSION_1`.
5. Read the capacity from configuration space, generation-stable.
6. Lay out queue 0 in the first page of the run, enable it, set `DRIVER_OK`, read it back.
7. Claim the INTID, then claim `INTID + 1`, which must be refused with `EPERM`: that is the proof
   the allow-list is one line long.
8. `[diag vblk] virtio ok features=0x100000000 capacity=2560 qsize=8 slot=0`.

A failing step prints `virtio FAIL step=<step> …` and abandons the rest; the driver still enters its
receive loop, answers every request `ENOSYS` and publishes nothing to DS. The queue memory is the
run's page 0, the request header the run's page 1, and the bounce buffer page 2; nothing is
submitted yet, so a `HARDWARE` notification is unexpected and is reported once as `step=irq-extra`,
with the line left masked. Serving BDEV requests is slice 6.3.

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

| Field    | Payload offset | Meaning                                                  |
| -------- | -------------- | -------------------------------------------------------- |
| minor    | `0..4` (i32)   | which device; `BDEV_MINOR_RAMDISK = 0` is the boot image |
| grant id | `4..8` (i32)   | names the client's buffer                                |
| length   | `8..12` (i32)  | bytes requested; at most `BDEV_MAX_IO` = one block       |
| block    | `16..24` (u64) | which block of the device                                |

`BDEV_READ` fills the client's buffer (so the grant needs `CPF_WRITE`, and the driver pushes with
`SAFECOPY_TO`). `BDEV_WRITE`, real since slice 5.10a, is the same request read backwards: it pulls
the client's bytes into the device with `SAFECOPY_FROM`, so the grant has to carry `CPF_READ`
instead. One parse and one validation serve both — the payload is identical, and only the dispatch
arm knows which way the bytes go. The driver never checks the access bit itself, and must not: the
kernel's `verify_grant` does, and a driver that re-derived the grant rules would be a second place
for them to drift.

Most of that table repeats CDEV's rules — no granter field, and the reply `m_type` *is* the byte
count. Three things are deliberately different:

**An over-long request is `EINVAL`, not a short read.** A short *write* is a POSIX contract every
client already loops over. A short *block read* is useless: a filesystem cannot interpret half a
block, so clamping would push a retry loop into every caller for nothing.

**An out-of-range block is `EINVAL`, not `EIO`.** A block device's size is known to its client — MFS
reads it from the superblock's `s_zones` — so asking past the end is a caller bug. `EIO` stays
reserved for Phase 6's real media errors, where the request was well-formed and the *device* failed.

**`BDEV_WRITE` was numbered three slices before it worked.** From 5.7 to 5.10a the arm answered
`EROFS` rather than `ENOSYS`, because `ENOSYS` is already the unknown-`m_type` answer and reusing it
would have made "this driver has never heard of writes" and "this driver knows about writes and
refuses them" indistinguishable to a client. Keeping the arm dispatched also kept it probed — and
the prediction held exactly: making the ramdisk writable changed one line inside it, the direction
handed to `sys_safecopy`.

### Where the blocks come from

`kernel/build.rs` builds a MinixFS v3 image at compile time (`tools/mkfs-mfs`, called as a
build-dependency library) and packs it into the MXBI archive as a **non-ELF blob** named `rootfs`.
At boot the kernel copies it, page by page, into freshly allocated frames and maps them into the
`memory` driver's address space — in the same `load_boot_server` arm the UART page uses, and for the
same reason: the driver has no way to ask.

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
a superblock. A block driver that decoded a superblock would depend on the filesystem format, which
is precisely the dependency Phase 6 has to unwind when virtio-blk replaces the ramdisk. What
licenses that shortcut is a host test in `tools/mkfs-mfs` asserting the header's three fields equal
the real superblock's.

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
[diag mfs] bdev.ds ok ep=3
[diag mfs] bdev.tail ok match=1
[diag mfs] bdev.deny ok n=10
```

`blocks=256` cross-checks the header's own block count against the length `GET_RAMDISK` reported —
two independently derived numbers, binding the build-time image geometry to the kernel's runtime
copy.

MFS, not VFS, is this driver's BDEV client — since slice 5.8, when the filesystem server took over
the band. Its own `mount ok root=1 bs=4096 blocks=256` marker, the superblock decoded out of the
block it asked for, is what retired the earlier `bdev.read`/`bdev.head` pair: a driver that replied
`OK` to the wrong page, or returned the header for both, fails to decode there instead of printing a
marker of its own. `bdev.tail` is the one thing `mount` cannot subsume — it reads the image's
reserved *last* block, whose label differs from the header's, and is the only proof that the copy
loop filling the ramdisk reached the end of the blob rather than looping over block 0. The ten
refusals in `bdev.deny` include the one grant check slice 5.3 could not reach — a `CPF_READ`-only
grant used as a copy *destination* — plus, since slice 5.10a, two `BDEV_WRITE` probes aimed at the
write path this driver now really implements, refused by the kernel's grant-direction and minor
checks rather than by a since-retired `EROFS` stub.

Every one of those markers is identical with and without the `boot-stubs` feature **and** with and
without the musl sysroot. That is what the fixed 1 MiB image size buys: in the sysroot-absent build
the archive packs a 15 KB `worker` ELF under the name `hello`, so a content-sized image would make
every size-derived marker config-dependent — passing in one configuration and proving nothing in the
other.

# Drivers

A **driver** in minix.rs is an ordinary user-space process that happens to own a piece of hardware.
It is not a special kind of kernel module and it runs no code in the kernel — it is a
`#![no_std]`/`#![no_main]` ELF, loaded from the MXBI archive into its own address space, driving a
SEF receive loop exactly like [a server](../servers/overview.md). Its privilege is a server's too: a
boot driver's image row carries the same full kernel-call mask every boot server gets, so what
confines it is its address space and its interrupt allow-list, not a narrower set of kernel calls.

What separates a driver from a server is memory in its address space that no other process has: a
page of the device's memory-mapped registers and, for TTY since slice 6.7, the screen's framebuffer
as well. Everything interesting about this chapter follows from those mappings.

There are four drivers with an ELF today: **TTY** (`drivers/tty/`), the console — the serial line
and, since slice 6.7, [a screen](#the-screen); **`memory`** (`drivers/memory/`), the boot ramdisk
plus, since slice 5.11, the [`/dev/null` and `/dev/zero` character minors](#the-character-minors);
since slice 6.2, **`virtio-blk`** (`drivers/virtio-blk/`), which
[brings a virtio disk up](#virtio-drivers) and, since slice 6.3,
[serves it as a block device](#virtio-blk) — since slice 6.4 the machine's GPT boot disk, whose
partitions it serves as minors when the disk carries a minix.rs root partition, and none at all when
it does not; and, since slice 6.6, **`virtio-console`** (`drivers/virtio-console/`), a
[second character device](#virtio-console) behind `/dev/hvc0`. Two more slots (`vinput`, `vnet`) are
wired in the kernel but hold no ELF, and `drivers/virtio-net` is still an empty placeholder.
`drivers/driver-rt` is the runtime the drivers share: the interrupt-line helper, the virtio
transport, the typed handles on DMA memory, the virtqueue, a GPT parser for block drivers, and the
pure half of the character-driver read contract.

`memory` is worth contrasting with the others up front, because it is the counter-example to the
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

Since slice 6.7 the same arm installs TTY's second device mapping, when there is one to install: the
framebuffer the bootloader reported, in
[a window of its own](../memory/overview.md#the-framebuffer-window) rather than in this one. Both
placement details above apply to it unchanged. What TTY does with it is under
[The screen](#the-screen).

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
| minor    | `0..4` (i32)   | which of the driver's devices; `CDEV_MINOR_CONSOLE = 0` is TTY's console   |
| grant id | `4..8` (i32)   | names the client's buffer (`CPF_READ` for a write, `CPF_WRITE` for a read) |
| length   | `8..12` (i32)  | bytes requested                                                            |
| offset   | `16..24` (u64) | where in the granted range to start                                        |

Six properties of that table and its replies are load-bearing.

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
exactly that reason, and `virtio-console` for the same one: its transmit buffer is that long. The
memory driver's `/dev/null` and `/dev/zero` (slice 5.11) stage nothing and never clamp — a
`CDEV_WRITE` longer than `CDEV_MAX_IO` still comes back reporting the whole count.

**`CDEV_READ` is the same payload, copy reversed** (slice 5.11). The reply is the byte count read,
`0` is EOF, and a short read is legal — POSIX `read()`'s contract and the one VFS already assumes
for `FS_READ`, so VFS sends one request and reports what came back. It existed only as a plan note
until `/dev/zero` needed it: the 5.3 text said the two devices would be "new minors, not new
requests", which is true of `/dev/null` and of writing `/dev/zero` and false of reading it. TTY has
served it since slice 6.5, when the PL011's receive interrupt gave it something to answer with (see
[TTY](#tty)).

**A read is answered at once, even when there is nothing to give** (slice 6.5). The memory driver's
minors always have an answer. A console does not — its input arrives when somebody types, or when
the host writes to it — and it may not simply hold the request until then, because its client is
VFS: a VFS blocked in a `SENDREC` to a driver serves nobody else, so one process waiting on a
keyboard would stop every file operation in the system. The next section is what a driver does
instead.

**Minors are a per-driver namespace.** TTY's console is 0; the memory driver's `/dev/null` and
`/dev/zero` are CDEV minors 3 and 5 (MINIX 3's `NULL_DEV` and `ZERO_DEV`), on the same driver as
BDEV minor 0's ramdisk — the request band, not the minor value, tells those apart. And
`virtio-console`'s one port is minor 0 too, `CDEV_MINOR_HVC0`: the same number as the console's on a
different driver, with its own name so that neither is taken for the other. Which driver a request
goes to is decided by the descriptor, in [VFS](../servers/overview.md#the-descriptor-table).

### Notify-then-pull: a read with nothing to give

```text
caller ──CDEV_READ──► driver      nothing buffered
caller ◄──EAGAIN───── driver      …and the driver records the caller as its waiter

                      (input arrives at the driver)

caller ◄──NOTIFY───── driver      once; the waiter is then forgotten
caller ──CDEV_READ──► driver
caller ◄──── n ────── driver      n bytes, copied through the caller's grant
```

A character driver whose input arrives later answers an empty `CDEV_READ` with **`EAGAIN`**, and
with that answer takes on a debt: it records the caller's endpoint, and when input next arrives it
sends that endpoint one `NOTIFY` and forgets it. The caller then asks again with an ordinary
`CDEV_READ`. The notification means *ask again*, not *this read is ready* — it carries no data and
names no request — so the second ask may hear `EAGAIN` too, and a driver needs to remember exactly
one thing: who to notify.

Nothing can be lost between the refusal and the notification, and three things make that true. The
driver records the waiter before it replies, and it is single-threaded, so no interrupt is serviced
between the two. The caller receives the `EAGAIN` before it can receive anything else. And a
`NOTIFY` sent while the caller is busy is latched by the kernel and delivered at its next *plain*
`RECEIVE` — never as the reply to a `SENDREC` the caller has in flight, not even one addressed to
the driver that sent the notification.

That last clause is a kernel rule
([IPC](../ipc/overview.md#a-notification-is-never-the-reply-to-a-sendrec)), and this contract is the
first thing in the system to depend on it: the caller here both calls the driver and is notified by
it, so a wake-up can be waiting at the very moment the caller sends the driver its next request. If
the kernel handed that wake-up over as the request's reply, the request would appear to fail, the
wake-up would be spent, and the reader it was meant for would never be retried. The kernel did hand
it over, until the slice that introduced this contract found out; a guard in `mini_receive` now
prevents it, and the [boot log](#what-the-boot-log-proves) proves the guard on every boot.

The details that make the contract usable:

- **`0` keeps meaning EOF**, so a driver that can say `EAGAIN` never sends `0` for a non-zero
  request. A *zero-length* read answers `0` before the buffer is even looked at, and records no
  waiter.
- **A driver that cannot deliver the notification must not promise it.** Once no input can ever
  arrive the answer is `EIO`, not `EAGAIN`: for TTY when it failed to claim its interrupt line at
  boot or its line has stopped since, for `virtio-console` once its device has broken. A reader left
  waiting would hang with nothing in the log to say why.
- **A driver whose device stops wakes whoever is parked on it** (slice 6.6). A reader that was
  answered `EAGAIN` *before* the device stopped is waiting for a notification that input will never
  cause. So the driver sends its recorded waiter the one `NOTIFY` when the device stops; the
  waiter's retry then hears the `EIO`.
- **One waiter, the last to ask.** A second endpoint asking while one is recorded replaces it. Every
  reader of either console goes through VFS today, so each driver only ever has one.
- **No value moved for this.** `EAGAIN` (11) was already an errno, and the CDEV band's only clients
  are VFS and — when the keyboard driver lands — TTY. A C program never sees it from a console
  `read()`: VFS [parks the read](../servers/overview.md#parking-a-console-read) instead of relaying
  the errno.

Two drivers implement the driver side today, TTY and [`virtio-console`](#virtio-console), and the
decisions they share are code they share: `driver-rt`'s [`cdev` module](#driver-rt) holds the
request validator, the order a read is answered in (`read_plan`) and the one waiter. VFS, which
pulls from both, has to keep their notifications apart — a `NOTIFY` from one driver says nothing
about the other's input — and
[retries only the notifier's readers](../servers/overview.md#parking-a-console-read).

The contract runs in whichever direction a driver's input flows. The keyboard driver will implement
the driver side too, with TTY as the *caller* — on the keyboard's `NOTIFY`, TTY will read until it
hears `EAGAIN` and push what it got into the same input buffer its own interrupt fills.

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
immediately, and the driver cannot have quiesced the device yet. If the owner is blocked in a plain
`RECEIVE` the message is built at once; otherwise — the owner is running, or is inside a `SENDREC`
waiting for a reply — the notification is deferred through `notify_pending` and built by the owner's
next plain `RECEIVE`. Either way the bitmap is *taken* — read and zeroed — so every line that fired
in between arrives in one message, at payload `0..4`.

**The driver pattern.** The line stays masked until the driver says otherwise, so the order inside
the receive loop is load-bearing:

```text
m = receive(ANY)
if m.m_source == boot_endpoint(HARDWARE) && m.m_type == NOTIFY_MESSAGE:
    for each set bit in payload[0..4]:
        acknowledge at the device  (PL011 UARTICR / virtio InterruptACK)
        drain the device           (RX FIFO until empty / walk the used ring)
        irqctl(IRQ_ENABLE, hook)   (only now unmask)
```

A driver identifies the interrupt by the kernel-stamped `m_source`, never by payload: a process
cannot forge `HARDWARE`, but it could set a payload bit. Re-enabling before the device is quiet
would re-enter at once.

**Acknowledge first, then drain.** Both steps come before the re-arm, and their order matters for
any source that is an *event*. If the driver drained to empty and acknowledged afterwards, an event
landing in between — a byte arriving after the last "is the FIFO empty?" test, a completion after
the last empty pop — would have its interrupt acknowledged away with the work still sitting in the
device, and the re-arm would find the line low: one keystroke stuck until the next. Acknowledged
first, a late event raises a fresh interrupt that the re-arm takes. The price is at most one
notification that finds nothing to drain, which a driver handles by re-arming and doing nothing
else. TTY's receive loop ([below](#tty)) and [the virtio loop](#the-driver-loop) both take this
order. (Slice 6.1's design wrote the drain first; the virtio drivers reversed it, and slice 6.5 did
the same for the PL011.)

The exception is a source that is a *condition* rather than an event, and TTY has one: the PL011's
transmit interrupt, which TTY raises twice at boot purely to prove that delivery and `IRQ_ENABLE`
work. There is nothing to drain, and "acknowledge" means whatever makes the device drop its line —
for that interrupt, clearing its mask bit `UARTIMSC.TXIM`, since under QEMU the raw interrupt is
re-raised by every byte anyone transmits.

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
made that device the disk the machine boots from; slice 6.6 added a second device and a second
driver, [`virtio-console`](#virtio-console).

### Four slots, one device each

`kernel-shared/src/com.rs` names four driver slots, appended after `init`: `VBLK_PROC_NR` 11
(`vblk`, virtio `DeviceID` 2), `VCON_PROC_NR` 12 (`vcon`, 3), `VINPUT_PROC_NR` 13 (`vinput`, 18) and
`VNET_PROC_NR` 14 (`vnet`, 1). `NR_BOOT_PROCS` is therefore 15, the demo stubs sit at 15–18 and PM's
fork pool starts at 19. Every slot has a `BootEntry` row and a privilege slot, but only `vblk` and
`vcon` have an ELF; the other two stay blocked on `RTS_NO_PRIV` like `PFS`. A boot process's priv id
is its `IMAGE` index, so the four rows take priv ids 16–19 and the stubs' dedicated priv ids sit
above the shared user slot (`USER_PRIV_ID` = 20), at 21–24; two compile-time asserts pin that
ordering: `USER_PRIV_ID >= N_IMAGE` in `kernel/src/proc/table.rs`, and the stub assert in
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
each device owns its page. The disk is on bus 0 and the console on bus 8 — slot 8, the first slot of
the second page, `[virtio] slot=8 pa=0xa001000 id=3 ver=2 drv=vcon`. All four pages are taken by the
four planned devices, so a fifth device would have no page of its own.

### What the kernel hands over

For a driver the probe assigned, `load_boot_server` adds one arm beside TTY's and the ramdisk's. It
maps the register page `Prot::DEVICE_RW` at `VIRTIO_MMIO_PAGE_VA`, pushes the device's INTID onto
the driver's `Priv::irqs` — so the allow-list is exactly one line long — and maps the driver's
[DMA run](../memory/overview.md#the-dma-window) at `DMA_VA`. It logs two lines per driver, which are
required markers of both full boots, since each attaches the boot disk and the console:

```text
[devmap] vblk va=0x40001000 pa=0xa000000 intid=48
[dma] vblk va=0xc0000000 pages=16 pa=0x...
[devmap] vcon va=0x40001000 pa=0xa001000 intid=56
[dma] vcon va=0xc0000000 pages=16 pa=0x...
```

The two drivers' virtual addresses are the same numbers because each has its own address space;
`pa=` and `intid=` are what tell the lines apart.

The driver learns the addresses through two more `SYS_GETINFO` selectors,
[`GET_VIRTIO_MMIO` (65) and `GET_DMA` (66)](../reference/syscalls.md#sys_getinfo-selectors). Both
answer `EPERM` to any process that is not one of the four driver slots, and a driver slot with no
device gets `ENOENT` — which is how a driver is told it has no device, not an error. Either driver
then prints `no device` and answers `ENXIO` to every request. What it does about DS differs, because
a miss means something different to each one's client: `virtio-blk` publishes nothing, and
`virtio-console` [publishes regardless](#always-published). The register base a driver uses is the
reply's VA, which already includes the device's offset inside its page.

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

The trust runs the other way too. The same table gives every `SRV_T` row an IPC mask covering every
other boot process, so each server and driver — PM, VFS, RS, `memory`, TTY, DS, MFS, VM, SCHED,
`vcon` — may send `virtio-blk` a `BDEV_WRITE`; a user process may not (its mask is PM, VFS and VM).
Since slice 6.4 that reaches further than the root filesystem: [every minor is read-write](#minors),
the disk behind the driver is the one the machine boots from, and so any of those servers could
overwrite the kernel on the ESP (minor 2) or the partition table and its backup (minor 1), and the
damage would outlast a reboot. In 6.3 the same request could reach only the root image. Nothing in
the system sends such a write, and nothing but the IPC mask prevents it.

### `driver-rt`

`drivers/driver-rt/` has six pieces and no `std`:

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
  `DmaBuf::prefix(len)` is the first `len` bytes of a buffer as a handle of their own — what a
  descriptor names when a transfer is shorter than the buffer it is staged in; it can only narrow.
  `DmaRegion::peek` reads the run without creating a handle. A driver with two queues calls
  `split_queue` twice, on the region and then on its remainder, and so gets two queue pages no
  buffer can reach.
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
- **The character-driver read contract** (`cdev.rs`, moved here from TTY in slice 6.6) is the pure
  half of [notify-then-pull](#notify-then-pull-a-read-with-nothing-to-give), for drivers that serve
  one minor. One private `validate` serves `validate_write(req, minor)` and `validate_read(req,
  minor)`, so the two directions cannot drift: a minor other than the driver's own → `ENXIO`,
  negative length → `EINVAL`, invalid grant id → `EINVAL`, then clamp to `CDEV_MAX_IO` (the
  four-field parse itself is `server-rt::cdev`'s). `read_plan(want, buffered, can_receive)` decides
  how a validated read is answered — `Zero`, `Stopped`, `Empty` or `Copy(n)`, in that order of
  precedence — and `Waiter` is the one endpoint owed a `NOTIFY`. No IPC, no device, no `unsafe`:
  each driver's `main` does what these decide. TTY and `virtio-console` use it; the memory driver,
  which never says `EAGAIN`, does not.

`unsafe` is confined to `mmio.rs` (volatile register accesses) and `dma.rs` (every access to the
memory the device shares); `queue.rs` has none, because its ring accesses go through the handle it
holds, and neither have `gpt.rs` and `cdev.rs`. Because `VirtQueue::new`, `add` and `queue_enable`
take handles instead of addresses, all three are safe functions, and safe driver code cannot aim the
device at memory outside its own run through them — nor behind their backs, because the ring memory
was split off before any buffer handle existed and no `DmaBuf` can overlap it. That is a guard
against mistakes and not a new boundary: [the trust model above](#what-the-boundaries-do-not-stop)
is unchanged, and two data buffers can still overlap each other. `irq.rs` is the only piece that
traps into the kernel. The rest is host-tested against a mock transport and hand-written rings, with
a second handle on the same memory playing the device's half; the parser's tests build headers and
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

A driver with more than one queue keeps the order and adds a walk: `virtio-console` acknowledges,
walks its receive queue to empty, walks its transmit queue to empty, and re-arms — on every
interrupt, whichever queue caused it.

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
every drive the runner attaches, so that line is a forbidden marker on every full boot (the reboot
is checked against its own pair, which does not include the common forbidden file).

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

| Line                               | What was wrong                                                                        |
| ---------------------------------- | ------------------------------------------------------------------------------------- |
| `root FAIL geometry capacity=<c>`  | empty, not whole 4096-byte blocks, or under the table's five; decided before any read |
| `root FAIL gpt rc=<rc>`            | a read of the partition table's blocks failed                                         |
| `root FAIL gpt <why>`              | the parser refused the primary header, the entry array or an entry (below)            |
| `root FAIL gpt ambiguous`          | more than one partition carries the root type GUID                                    |
| `root FAIL no root partition`      | none does                                                                             |
| `root FAIL align`                  | the root partition does not start and end on a 4096-byte block boundary               |
| `root FAIL readonly`               | the device is read-only (`VIRTIO_BLK_F_RO` was accepted)                              |
| `root FAIL head rc=<rc>`           | the read of the root partition's first block failed                                   |
| `root FAIL label`                  | that block does not start with the image label                                        |
| `root FAIL blocks hdr=<h> dev=<d>` | the image header's block count is not the partition's                                 |
| `root FAIL tail rc=<rc>`           | the read of the root partition's last block failed                                    |
| `root FAIL tail label`             | that block does not start with the tail label                                         |

**The partition table.** The driver reads whole-disk blocks 0–4 (LBA 0–39), which hold the
protective MBR, the primary GPT header and all 128 partition entries — a device with fewer than five
blocks was already refused for its geometry, so no read names a sector past the end — and hands the
bytes to `driver-rt`'s [GPT parser](#driver-rt). The parser checks the header in a fixed order, and
`<why>` is the first check that fails: `header short`, `signature`, `revision` (must be 1.0),
`header size` (92 bytes up to one sector), `crc` (the header's CRC-32), `my lba` (must be 1), `entry
size` (must be 128), `entry count` (1 to 128), `entry lba` (the array must start at LBA 2), `usable
range` (`FirstUsableLBA ≤ LastUsableLBA <` the disk's size). Then the entry array: `entries short`,
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
every minor — the whole disk included, and a zero-length request too, which is the request MFS
probes the driver with. That gate is decided first, by the same pure function that then applies the
four steps above (`part::locate`), so host tests hold it; no boot marker can, because on the
rootless boot MFS's one request for another minor goes to `memory`.

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
image sees the first boot's writes with or without a guest flush. In one hand-run stub-free boot of
slice 6.4, dropping the flush from the write path moved this one marker and nothing else — the image
still [rebooted with its writes intact](../boot/overview.md#booting-the-same-image-twice) — and a
driver changed to treat a failed flush's status as success moved nothing at all, in a boot or in a
host test, because QEMU's flush cannot be made to fail. The decisions around the flush are
host-tested; the calls in `main.rs` that act on them, and the early returns that keep a zero-length
write or a refused grant from being flushed, are covered by that one marker and by reading; the
device's side of it is taken on its status byte.

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

### `virtio-console`

`drivers/virtio-console/` (slice 6.6) is the second consumer of `driver-rt` and the second driver
that can keep a reader waiting on the [CDEV band](#the-cdev-protocol). It publishes itself under its
proc name, `vcon`, and serves one device: `/dev/hvc0`, minor `CDEV_MINOR_HVC0`. Standard input,
output and error stay on TTY in every process; a program reaches this console only by opening the
node.

The device is a QEMU `virtio-serial-device` with one `virtconsole` port, attached to **every** boot
on `virtio-mmio-bus.8`. Its host side is not the terminal: it is a `pipe` chardev over two FIFOs,
`target/hvc0.out` (what the guest writes) and `target/hvc0.in` (what it reads), which is how a boot
is [fed on the second console](../build.md#feeding-the-console) without the two input streams
getting mixed.

The crate has `virtio-blk`'s shape. The pure half is host-tested: where the two queues and two
buffers sit in the DMA run (`bringup.rs`), and the port's state — who holds the receive buffer,
whether a transmit is in flight, and every step taken out of turn (`port.rs`). `main.rs` does the
kernel calls, the queues and the interrupt around them, and holds the crate's only two `unsafe`
blocks: the `driver-rt` constructors for the DMA run and for the register window.

#### Always published

`vcon_init`, the `init_fresh` callback, publishes the name to DS **unconditionally** — it does not
ask whether there is a device first. That is the opposite of [`virtio-blk`](#virtio-blk), and for a
reason: there, a DS miss is an answer ("no disk") that MFS acts on. Nothing selects between
consoles, so a miss here would mean nothing, and with the name always present VFS's lookup can
follow the ordinary rule — find `vcon` in DS, or fall back to the boot endpoint and say so (`[diag
vfs] vcon.ds ok ep=…` / `vcon.ds FAIL rc=… fallback=…`). The process is always in its receive loop
to answer, so the fallback is always sound. The one ordering this needs is `vcon` before `vfs` in
[the boot archive](../boot/overview.md#boot-modules).

A driver with no working device — none assigned, or a failed bring-up — answers **`ENXIO`** to every
`CDEV_READ` and `CDEV_WRITE`, well-formed or not. `open("/dev/hvc0")` still succeeds, because a
device open consults only VFS's table; the first `read` or `write` hears the `ENXIO`.

#### Bringing the console up

Bring-up runs once, after `sef_startup`, in straight-line code, and prints exactly one verdict line:

1. `GET_VIRTIO_MMIO`; `ENOENT` prints `no device`. The runner attaches the console to every boot, so
   on this driver that line is a forbidden marker, not a healthy path.
2. `GET_DMA`, then the kernel's stamp in the run's first eight bytes, checked as
   [`virtio-blk` checks it](#bring-up).
3. `MmioTransport::new`, then `DeviceID` must be 3.
4. `negotiate`, **asking for nothing**. The accepted word is `VIRTIO_F_VERSION_1` alone. The
   console's three optional features are a window size (`F_SIZE`), more than one port
   (`F_MULTIPORT`) and an emergency write register (`F_EMERG_WRITE`); every field of the console's
   configuration space is valid only under one of them, so the driver **reads no device
   configuration at all**. The firmware used this device before the kernel started; `negotiate`
   begins with a reset, which is what takes it back.
5. Size both queues under the device's `QueueNumMax`, lay the four pages out (next section), enable
   both queues, set `DRIVER_OK`, read it back. Without the multiport feature the device has exactly
   two queues: queue 0 receives, queue 1 transmits.
6. Claim the INTID, then claim `INTID + 1`, which must be refused with `EPERM`.
7. Post the one receive buffer: chain it device-writable into the receive queue and notify.
8. `[diag vcon] virtio ok features=0x100000000 rxq=8 txq=8 slot=8`.

`features=0x100000000` is bit 32, `VERSION_1`, and nothing else — a `…1`, `…2` or `…4` there would
be a feature the driver did not ask for. `rxq=` and `txq=` are the two queue sizes: `driver-rt`'s
cap of 8 under a `QueueNumMax` of 1024 on QEMU 11. A failing step prints `virtio FAIL step=<step> …`
and abandons the rest; one after the negotiation also marks the device `FAILED`. No step waits on a
register or a ring and none needs a completion, so the first client can be answered as soon as the
loop is reached — which is why nothing constrains this driver's place in the archive except `vcon`
before `vfs`.

#### Four pages

The driver uses four of its DMA run's sixteen pages, one for each thing:

| DMA page | Holds                                                         |
| -------- | ------------------------------------------------------------- |
| 0        | the receive queue (queue 0)                                   |
| 1        | the transmit queue (queue 1)                                  |
| 2        | the receive buffer, `CDEV_MAX_IO` (256) bytes, device-written |
| 3        | the transmit buffer, `CDEV_MAX_IO` bytes, device-read         |

`bringup::vcon_layout` consumes the `DmaRegion` and produces all four handles. The two queue pages
come off the front with two `split_queue` calls **before** either buffer is taken, so no `DmaBuf`
can reach a ring — the same guard [`driver-rt`](#driver-rt) gives `virtio-blk`, applied twice. A run
that is not page-aligned, or holds fewer than four whole pages, is refused (`virtio FAIL
step=queue`).

There is **one** buffer in each direction, not a pool. That is `virtio-blk`'s "one request in
flight" applied to both directions of a console, and it is what makes the receive state a
two-variant enum rather than a queue of buffers. Throughput is not a goal here.

#### Receive: one buffer, posted or held

`port::Rx` says who has the receive buffer:

- **`Posted`** — chained into the receive queue. The device may fill it.
- **`Held { len, off }`** — the device returned it with `len` bytes, and readers have taken `off` of
  them.

A used element in the receive queue moves `Posted` to `Held { len, off: 0 }`. The length is the
device's word, so it is clamped to the buffer's; a zero-length element holds nothing and the buffer
is chained straight back in. A read is then served **directly from the DMA buffer** — one
`SYS_SAFECOPY` from the buffer's own address to the caller's grant, with no staging copy in between
— and follows the order `read_plan` fixes for every character driver:

1. validate, by `CDEV_WRITE`'s rules in `CDEV_WRITE`'s order;
2. a zero-length read answers `0`, before the buffer is looked at;
3. a broken device answers `EIO`;
4. `Posted` records the caller as the waiter and *then* answers `EAGAIN`;
5. `Held` copies `min(asked, len - off)` bytes: `Rx::peek` says which, the copy is made, and **only
   if it returned `OK`** does `Rx::advance` move `off`. A refused copy — `EPERM`, `EFAULT`, relayed
   verbatim — leaves the bytes owed to whoever asks next.

When a read takes the last held byte, `advance` reports the buffer drained, and the driver chains it
into the receive queue again and notifies the device **before the reply goes out**. (If that re-post
fails the device is broken, but the caller has its bytes and still gets its count; the next request
hears `EIO`.)

**There is no input ring, and nothing is ever dropped.** TTY has a ring and an overflow line because
a UART cannot make its sender wait. A virtqueue can: while the buffer is held the device has nowhere
to put more input, so the bytes stay in the host's end of the chardev until the buffer is posted
again. Measured on QEMU 11: bytes written while the buffer was held were kept and delivered at the
re-post, and a 600-byte host write arrived as three used elements of 256, 256 and 88 bytes, one per
re-post. So this driver has no `rx overflow`, and no storm guard either — `InterruptStatus` is
cleared by the acknowledgement, so a line that keeps firing with nothing to receive has no virtio
form.

#### Transmit: one chain, waited for

A `CDEV_WRITE` is validated; a zero-length one answers `0` and touches nothing. Otherwise up to
`CDEV_MAX_IO` bytes are copied from the caller's grant **straight into the transmit buffer** — so a
refused grant or an unmapped buffer is relayed verbatim and never reaches the device — and a
`DmaBuf::prefix` of exactly that many bytes is chained into the transmit queue. The driver notifies
the device and then **waits for that chain to come back**, in a receive filtered to `HARDWARE`:
blocked, never polling, with a second client left queued in the kernel. The reply is the count
moved, so a longer write is a short write and VFS's loop re-sends the rest, as it does for TTY.

The wait is `virtio-blk`'s, with one difference that matters: **a receive completion can arrive
during it**. Every interrupt taken in the wait goes through the same handler the main loop uses, so
input that lands while a write is in flight is taken into the held buffer and its waiter notified
from inside the wait. VFS is in the middle of a `SENDREC` to this driver at that moment, and the
kernel keeps the notification for VFS's next plain `RECEIVE`
([IPC](../ipc/overview.md#a-notification-is-never-the-reply-to-a-sendrec)) — the shape that rule
exists for.

A device that never completes a transmit hangs the driver in that wait with VFS behind it, and
nothing reports it. QEMU completes a console write, with one interrupt, whether or not anything
reads the host side.

#### One handler for both queues

Every interrupt, wherever it is received, is handled the same way:

```text
read InterruptStatus; acknowledge exactly those bits
walk the receive queue until it is empty
walk the transmit queue until it is empty
IRQ_ENABLE                               re-arm the line
NOTIFY the waiter, if input arrived      after the re-arm; then forget the waiter
```

That is [the driver loop](#the-driver-loop) with a second walk, and the last line is after the
re-arm for the reason it is in TTY: the line is live before the waiter can come back with a read. A
configuration-change interrupt walks two empty rings and re-arms. A wake-up `NOTIFY` that fails
keeps the waiter and prints `[diag vcon] rx FAIL notify rc=…` once, exactly as TTY's does.

A used element nobody was waiting for is a fault, not something to skip: a transmit completion with
no write in flight or for the wrong chain head, a receive completion while the buffer is already
held, or any `QueueFault` from the queue itself. So is a line that cannot be re-armed, and a failed
receive during the transmit wait. On any of them the driver writes `FAILED`, prints one `[diag vcon]
virtio FAIL step=io …` line naming the cause, leaves the line masked, answers `EIO` from then on —
and sends its recorded waiter the one `NOTIFY`, so that a reader already parked on `/dev/hvc0` is
retried and hears the `EIO` instead of waiting for an interrupt that can no longer come. Whether a
wake-up is due is one small pure function, `port::wake_due`: input arrived, or the device has *just*
broken.

No healthy boot reaches the broken state, and QEMU cannot be made to produce it; it is held by host
tests on `port.rs` and by reading.

#### What a console client hears

- **`ENXIO`** for every `CDEV_READ` and `CDEV_WRITE` when there is no working device — decided
  before the request is parsed, so a malformed request and a zero-length one hear it too — and for
  any minor but `CDEV_MINOR_HVC0` on a device that came up.
- **`EINVAL`** for a malformed request, from the validator TTY also uses.
- **`EAGAIN`** for a read that finds no input; the caller is owed a `NOTIFY`. VFS never passes this
  on: it [parks the read](../servers/overview.md#parking-a-console-read).
- **`EIO`** for a read, or a non-empty write, once the device has broken.
- A negative `SYS_SAFECOPY` result, **verbatim**.
- **`ENOSYS`** for any other request number. A notification that is not the interrupt line's is
  dropped without a reply.

Like every virtio driver, [it cannot exit](#a-virtio-driver-cannot-exit).

#### The proof on a fed boot

Bring-up is proved on every boot by five lines — the probe's, the two mappings, the driver's verdict
and VFS's lookup:

```text
[virtio] slot=8 pa=0xa001000 id=3 ver=2 drv=vcon
[devmap] vcon va=0x40001000 pa=0xa001000 intid=56
[dma] vcon va=0xc0000000 pages=16 pa=0x...
[diag vcon] virtio ok features=0x100000000 rxq=8 txq=8 slot=8
[diag vfs] vcon.ds ok ep=12
```

The data path is proved by a second child of `init` ([init](../servers/overview.md#init-pid-1) has
its steps) and by the script that feeds the boot. The child opens `/dev/hvc0` and writes `minix.rs
hvc0 out` through it. That line leaves the machine on the second chardev, and the feeder sends
nothing at all until it has read it there, on the host — among some 800 bytes of boot text, because
edk2 and Limine use the port too before the kernel starts. The child meanwhile issues one read into
an address that is not mapped. Nothing has been fed, so the driver answers `EAGAIN` and VFS parks
the request, behind the `read(0)` child's, which parked first.

The feeder then writes `minix.rs hvc0 in` and a newline in two pieces a few seconds apart, and two
lines appear in the serial log, in this order:

```text
[diag vfs] cons.other ok
minix.rs init: hvc0 ok n=17
```

The first piece's interrupt makes the driver notify VFS. VFS walks its parked reads on this driver's
behalf, **passes over** the serial reader, and asks for the one behind it. The driver has bytes held
and tries the copy; the kernel refuses it, because the buffer is not mapped; and VFS, which now has
an answer for a reader that sat behind another driver's, prints `cons.other ok` and passes the
`EFAULT` on to the child. The child then reads into a real buffer until it holds all seventeen bytes
and compares them. That one line, `hvc0 ok n=17`, therefore says four things:

- **guest to host**: the feeder's bytes are the only way the line can print, and the feeder sends
  them only after seeing the guest's own line on the host;
- **host to guest**, twice;
- **the refused copy consumed nothing**: the first piece was still there to be read after the
  `EFAULT`. A driver that advanced before its copy returned would have thrown it away, and the child
  would come up short and stay parked;
- **the buffer was re-posted and the line re-armed**: the second piece has nowhere to land unless
  the driver gave its one buffer back to the device after the child drained the first.

The serial console is fed only after `hvc0 ok` is in the log, so [the serial proof](#tty) follows
this one, and a boot whose second console is broken loses `stdin ok` as well. A boot nobody feeds
prints the five bring-up lines and neither of these two: both children stay parked.

### A virtio driver cannot exit

The DMA run is ordinary RAM, so every address-space teardown would free it while the device may
still be writing to it. No device-reset path exists yet, so the kernel guards the three ways the run
could be freed. `do_exit` and `do_exec` `assert!` that the process holds no virtio assignment, so
the kernel halts with a message naming the missing reset path; and `VMCTL_PT_UNMAP` answers `EPERM`
for an address inside the DMA window of a process that holds one. Boot drivers never exit, so none
of the guards fires on a healthy boot; a mutation that made `vblk` send `PM_EXIT` showed the exit
guard firing.

## TTY

`drivers/tty/` is one crate in two halves since slice 6.7: a binary, which is the driver, and a
library, which is [the screen's renderer](#the-screen). The binary is four files. (Until slice 6.6
there was a fifth, `cdev.rs`: the request validator, `read_plan` and `Waiter`. They are
[`driver-rt`'s `cdev` module](#driver-rt) now, shared with `virtio-console`, and TTY passes
`CDEV_MINOR_CONSOLE` as its one minor.)

- **`input.rs`** — the input ring, also pure and host-tested: received bytes waiting to be read.
- **`irq.rs`** — the interrupt line: the claim, its denial battery, the delivery proof, and the
  receive service.
- **`pl011.rs`** — volatile accesses to the UART's registers at `TTY_UART_VA`, and all of the
  crate's `unsafe` but one store (the other is the framebuffer's, in `main.rs`). Transmit polls
  `FR.TXFF` before each `DR` store and translates LF to CRLF; receive is `getc` (one byte out of the
  FIFO, or "empty" when `FR.RXFE` is set), `set_rx_masks` and `ack_rx`. The register offsets are
  deliberately duplicated from the kernel's own PL011 writer; they cannot be shared, because the
  kernel crate is bare-metal-only and pinned by `forced-target`, so it can never be a user-space
  dependency — and a register layout is a hardware fact, not a shared ABI.
- **`main.rs`** — the SEF loop, the two handlers, and `console_write`, the one path every console
  byte takes out of the driver.

The library half is `lib.rs` and four modules — `font`, `grid`, `paint` and `reply` — described
under [The screen](#the-screen).

### Transmit

The write handler captures `m_source` **first** (it is both the reply target and the granter),
validates, pulls the bytes across with `SYS_SAFECOPY(SAFECOPY_FROM, caller, gid, offset, staging,
n)`, hands them to `console_write`, and replies `n`. A negative `SYS_SAFECOPY` result is relayed
**verbatim**: `EPERM` ("your grant does not authorize this") and `EFAULT` ("your buffer is not
mapped") are different bugs on the client's side. Transmission is polled; no interrupt is involved
in it.

`console_write` has **two sinks, and the UART comes first**. It writes the whole buffer to the
PL011, and only then, when TTY has a screen, draws the same bytes on it. The order means that no
byte reaches the serial line later for being painted first, and adding the screen changed the text
of no serial marker. (The boot as a whole does run slower for the painting done between writes;
[the cost is measured below](#a-scroll-repaints-only-what-changed).) The banner goes through the
same function, so there is one output path and not two. What does *not* go through it is everything
the kernel prints — its own trace lines and the `[diag …]` lines servers print through `SYS_DIAGCTL`
— because the kernel writes the UART itself. Those stay on the serial line only: the screen shows
what user space writes to the console, and nothing else.

Two departures from the server template are worth naming. The staging buffer lives in `main`'s
frame, never in the init callback's — the kernel reads and writes it while TTY is blocked inside the
`SYS_SAFECOPY` SENDREC, so the frame must outlive every call that names it (the same rule
`GrantPool` follows). One 256-byte buffer serves both directions, since a request is finished before
the next is received. And an **unknown `m_type` gets a reply**, where DS harmlessly drops one: a
driver's clients all SENDREC, so a dropped request blocks the caller forever.

A notification is the exception, because its sender is not waiting for anything. The loop looks for
one before it dispatches: a `NOTIFY_MESSAGE` from `HARDWARE` is the interrupt line (next section),
and a `NOTIFY_MESSAGE` from anybody else is dropped without a reply. Nothing sends TTY one today;
the keyboard driver will, and its arm goes there.

The loop also does one thing that serves no client. Before it replies to the **first** request it
ever receives, TTY sends that request's sender a `NOTIFY` — once per boot. The sender is VFS, in the
middle of a `SENDREC` to TTY, so the kernel cannot deliver the notification and latches it; VFS's
next request to TTY then meets a notification latched from the very process it is asking. That is
the collision the kernel rule in
[IPC](../ipc/overview.md#a-notification-is-never-the-reply-to-a-sendrec) is about, made to happen on
every boot instead of once in a while, and [the boot log](#what-the-boot-log-proves) shows its
outcome. To VFS it is one spurious "ask again", which the read contract already allows.

### The screen

Since slice 6.7 TTY draws the console as well as transmitting it. The framebuffer is the one the
firmware set a mode on and
[Limine reported to the kernel](../boot/overview.md#the-boot-framebuffer). The kernel validated it,
kept its pages out of the frame allocator and mapped them into TTY — and into nothing else — at
`FB_VA`, the base of [the framebuffer window](../memory/overview.md#the-framebuffer-window), as
device memory. TTY draws text on it in an 8×16 bitmap font: light grey (`0xAAAAAA`) on black,
wrapping and scrolling. There is no cursor, no colour and no escape-sequence terminal, and nothing
echoes input yet.

#### Asking for it

TTY cannot see the bootloader's description of the screen, so it asks the kernel: one
[`SYS_GETINFO(GET_FRAMEBUFFER)`](../reference/syscalls.md#sys_getinfo-selectors), made once, in
`main`, as soon as `sef_startup` has returned and before the banner. It then prints exactly one of
three lines:

```text
[diag tty] fb ok w=800 h=600 cols=100 rows=37
[diag tty] fb none rc=-2
[diag tty] fb FAIL <reason>
```

`fb none` is the kernel declining. `rc=-2` (`ENOENT`) means the kernel accepted no framebuffer,
which is the ordinary answer on a machine without a display; any other value is a defect — `rc=-1`
(`EPERM`) would mean the kernel's gate on the selector and the loader that mapped the screen name
two different processes.

`fb FAIL` is a reply TTY will not draw on. The reply is the only thing between a wrong number and a
store through a device mapping, so TTY **validates it again rather than trusting it**
(`reply::accept`). The checks run in this order and the first that fails is the `<reason>`:

| `<reason>`       | What was wrong                                                         |
| ---------------- | ---------------------------------------------------------------------- |
| `va`             | the reported address is not `FB_VA`, the one address TTY stores at     |
| `bpp`            | not 32 bits per pixel                                                  |
| `shift`          | a colour channel's shift is not 0, 8, 16 or 24                         |
| `shifts-overlap` | two channels share a shift                                             |
| `zero-size`      | the width or the height is zero                                        |
| `pitch`          | the pitch is not a multiple of 4, or is less than `width * 4`          |
| `window`         | `pitch * height` is more than the framebuffer window could hold mapped |
| `grid`           | the screen is smaller than one 8×16 cell                               |

The kernel applied the same conditions to Limine's answer before it mapped anything, so an `fb FAIL`
means the two validators disagree. After `fb none` or `fb FAIL` TTY is **serial-only** for the rest
of the boot: the screen half of `console_write` does nothing, and every serial marker still passes.
The runner gives every boot a display, so both lines are forbidden markers.

On acceptance TTY stores the background colour in every pixel of the screen, and only then prints
`fb ok` — so that line also says that a whole screen of stores through the mapping did not fault —
and only then writes the banner, which is therefore the screen's first line.

`cols` and `rows` are the width divided by 8 and the height by 16, rounded down, and capped at 160
and 64. QEMU's `ramfb` comes up at 800×600 under edk2, which is 100 columns by 37 rows with eight
rows of pixels left over at the bottom; a margin like that is cleared once and never drawn on again.
A screen larger than 160×64 cells is used that far from its top-left corner and the rest stays
background.

#### The library half

Everything that can be decided without touching a device is in the crate's library (`minixrs_tty`,
`src/lib.rs`), which is pure, host-tested, and declares `#![forbid(unsafe_code)]` — something the
binary cannot say for itself, since its `_start` needs `unsafe(no_mangle)`:

- **`font`** — glyph lookup over [the vendored font](#the-font): `glyph(byte)` returns sixteen
  bytes, one per pixel row, bit 7 leftmost.
- **`grid`** — the text model: a fixed 160×64 array of bytes, a cursor, and one operation,
  `Grid::put(byte)`, which is the whole of [what a byte does](#what-a-byte-does). `put` returns the
  *damage* — nothing visible, one cell, or "the grid scrolled" — so the caller knows what to paint.
- **`paint`** — turns grid cells into pixel stores, through a trait with a single method,
  `Surface::store(x, y, pixel)`. It also holds `Format`, the validated description of a framebuffer
  (geometry, channel shifts, and the grid that fits), and `decode_cell`, the inverse of painting,
  which [the screen check](../build.md#the-screen-check) uses on the host.
- **`reply`** — the `GET_FRAMEBUFFER` reply, decoded, and the accept-or-refuse decision in the table
  above.

Unlike [MFS](../servers/overview.md), whose library and binary are separated by a cargo feature,
there is no feature gate between the two halves: nothing reaches this library from a build script.
`tools/fbcheck` depends on it, so a screen dump is decoded by the code that painted it.

What is left in `main.rs` is one kernel call, the verdict line, and `Screen` — the binary's
implementation of `Surface`, and the only place the driver touches the framebuffer. The grid and the
record of what has been painted (below) are locals of `main`, passed down by `&mut`; at 10 240 bytes
each they are the largest things on TTY's stack, and `main`'s frame is 21 376 bytes of the 64 KiB a
driver gets.

#### What a byte does

`console_write` feeds each byte to `Grid::put`:

| Byte          | Effect                                                        |
| ------------- | ------------------------------------------------------------- |
| `0x20..=0x7E` | draw at the cursor and advance; at the last column, see below |
| `\n`          | column 0 of the next row; past the last row, scroll one row   |
| `\r`          | column 0                                                      |
| `\t`          | advance to the next multiple of 8, clamped to the last column |
| `\b`          | back one column unless already at column 0; erases nothing    |
| anything else | drawn as `?` — `ESC` included                                 |

**The wrap is deferred.** Writing in the last column does not move the cursor to the next row; it
leaves it where it is with a wrap *pending*, and the wrap happens when the next drawn byte arrives.
A line of exactly as many bytes as the screen has columns, followed by `\n`, therefore takes one row
and not two: the `\n` uses up the pending wrap instead of adding to it. `\n`, `\r` and `\b` cancel a
pending wrap; `\t`, which at the last column has nowhere to go, leaves it.

The grid stores bytes as they are drawn, so an unprintable byte is stored as `?` and a tab stores
nothing at all — it only moves the cursor. (The UART sink is separate and unaffected: it transmits
the bytes as given, with LF translated to CRLF.)

#### Stores only, never a read

The framebuffer is a **device** mapping, like the UART's page, and TTY touches it in exactly one
way: an aligned, volatile, 32-bit store of one pixel.

```text
write_volatile((FB_VA + y * pitch + x * 4) as *mut u32, pixel)
```

It never reads a pixel back, never views the mapping through a slice, and never copies or fills it
in bulk — no `copy_nonoverlapping`, no `memset` — because those are the operations a compiler is
free to turn into unaligned, merged or widened accesses, which device memory faults on or
mishandles. `volatile` keeps the one store that remains from being merged or elided. The rule is
structural rather than a habit: `Surface` has a `store` and no load, so the renderer has nothing to
call that would read.

`Screen::store` re-checks its bound on every call, `x < width` and `y < height`, although the
renderer never asks for a pixel outside them: it is the last line before the device, and a renderer
bug should cost a missing pixel, not a store somewhere else. The bound is in pixels, not in mapped
pages, and the difference is real — the kernel maps whole pages, 800×600 at 32 bits is 468.75 of
them, and the last quarter-page of the 469th belongs to whatever follows the framebuffer in physical
memory.

A drawn character is 128 stores: every pixel of its 8×16 cell, foreground or background.

#### A scroll repaints only what changed

With no reads there is no "move the pixels up one row". A scroll shifts the *grid*, and the screen
is brought up to date from it. Repainting every cell would do that, and on a full screen every
newline scrolls: 3700 cells, 473 600 device stores, for each line of output. Measured in slice 6.7,
that cut the console's throughput about sixfold.

So `paint` keeps a shadow, `Shown`: the byte last painted in each cell, a second array the size of
the grid, in ordinary memory. `paint_diff` walks the grid after a scroll and stores a cell only
where the grid and the shadow disagree — and console output is mostly short lines that look alike,
so most cells do not. A single drawn byte goes through the same comparison (`paint_cell_diff`), so
rewriting a cell with what it already shows costs nothing. By the same measurement the screen now
costs about a third of the throughput of a console that does not paint at all, where the full
repaint cost five sixths of it.

The shadow is only true if every paint goes through it. It starts as all blanks, which is what the
start-up clear left on the screen, and from then on `console_write` is the only painter and uses
only the two `_diff` functions. A host test in `paint` holds the invariant: it drives generated byte
streams through screens of several sizes and requires the result to be pixel-identical to clearing
the screen and painting the final grid, with the shadow agreeing cell for cell.

#### The font

The font is [Spleen](https://github.com/fcambus/spleen) 8×16, by Frederic Cambus, vendored as
`drivers/tty/font/spleen-8x16.psfu`: the file from the upstream 2.3.0 release, byte for byte, with
the upstream `LICENSE` beside it and a `README.md` recording the release, the tarball it came from
and the SHA-256 of both. Spleen is **BSD-2-Clause**. minix.rs is BSD-3-Clause and the two are
compatible; the font keeps its own licence and is not relicensed.

The file is a PC Screen Font, **version 1** — upstream writes version 2 for its wider faces and
version 1 for this one — with 512 glyphs and a Unicode table. `font.rs` embeds it with
`include_bytes!` and checks its header in a `const` context (the magic, sixteen rows per glyph, a
Unicode table present, the glyph data all there), so a wrong or truncated file fails the *build*.
The 95 printable glyphs are copied into a table at build time and the rest never reach the driver's
image.

The renderer indexes that table by byte, which is sound only because this font happens to put U+0020
to U+007E at glyphs `0x20` to `0x7E`. A PSF file's glyph order is its own business — the Unicode
table is what says which glyph draws which character — so a host test walks the table and pins
exactly that, and a second pins one glyph against a bitmap written out by hand, so that a font read
wrongly cannot agree with itself. The glyphs below `0x20` are symbols, not controls, which is the
other reason every byte outside the printable range draws `?`.

### The interrupt line: a proof, then receive

TTY claims INTID 33 at startup and runs a battery of claims that must each be refused. One object,
`IrqService`, then owns the line for the rest of the boot, in three phases:

- **`Proof`.** TTY sets `UARTIMSC.TXIM`; the PL011 raises its transmit interrupt, the kernel masks
  the line and notifies TTY, and TTY clears `TXIM` and re-arms with `IRQ_ENABLE`. It does this
  twice, because the second notification can only arrive if the first re-arm really unmasked the
  line, and then prints `[diag tty] irq ok rounds=2`. `TXIM` stays clear from then on: the kernel's
  own polled console writes the same UART, and every byte it sends would raise the line again.
- **`Rx`.** In the same step TTY sets `UARTIMSC.RXIM | RTIM` — the receive interrupt, and the
  receive *timeout*, which covers a FIFO holding fewer bytes than its trigger level — and prints
  `[diag tty] rx on`. From here every notification on the line is receive service.
- **`Stopped`.** A failure that has been reported; the line is left masked and nothing more is
  serviced. A proof round that goes wrong, a refused re-arm and a storm all end here, and nothing
  leaves it: `IrqService::can_receive` is `false` from then on. The step from one phase to the next
  is a pure function, `Phase::after`, host-tested for each way of stopping.

Receive service is the pattern from [above](#interrupts-reach-a-driver-as-a-hardware-notification),
in its acknowledge-first order:

```text
write UARTICR = RXIC | RTIC            acknowledge both sources
while FR.RXFE == 0: ring.push(DR)      drain until the FIFO reports empty
IRQ_ENABLE                             re-arm the line at the GIC
NOTIFY the waiter, if any              only if a byte was stored; then forget the waiter
```

The last line is `main`'s, not `irq.rs`'s: `IrqService::on_notify` re-arms and only *reports* that
bytes arrived, so the waiter is notified **after** the line is live again and cannot come back with
a read while it is still masked. If that `NOTIFY` is refused, TTY prints `[diag tty] rx FAIL notify
rc=…` once: it is the only wake-up a parked reader gets, and a reader left waiting with nothing in
the log would look exactly like one nobody typed at. It also records the waiter again rather than
forgetting it, but that retry is nominal — every reason the kernel has to refuse the `NOTIFY` (a
dead or unknown endpoint, no permission to reach it) is permanent for that endpoint, so the next
byte's attempt fails the same way until another reader's `EAGAIN` takes the slot. `DR`'s error flags
(framing, parity, break, overrun) are discarded and the byte kept. TTY writes neither `UARTLCR_H`
nor `UARTIFLS`: the kernel's console shares the UART, the firmware configured it, and the loop is
correct whether the FIFO is on or off. (As measured under QEMU 11 with edk2, it is on — and QEMU
raises the receive interrupt on the first byte regardless of the trigger level.)

Two guards stand behind the loop, each with a forbidden boot marker (the refused `NOTIFY` above is a
third). A notification that drains nothing is legal — it is the price of acknowledging first — but
`RX_EMPTY_STORM` (64) of them **in a row** is a device that is not dropping its line: TTY prints
`[diag tty] rx FAIL storm` and does not re-arm. A refused `IRQ_ENABLE` prints `rx FAIL enable rc=…`.
Either way the line has stopped: TTY wakes the reader parked on it, once, and answers every later
read `EIO` ([below](#receive-answering-cdev_read)) — including for bytes already in the ring. The
run of empty notifications is counted by a small pure type, `EmptyRun`, so the rule is pinned by
host tests rather than by a boot that would need a misbehaving UART. (QEMU's cannot be made to
misbehave this way from the driver's side: skip the drain and the bytes sit in the FIFO in silence,
because the acknowledgement alone drops the line.)

### The input ring

`input.rs`'s `Ring` is a fixed 256-byte FIFO (`TTY_INPUT_MAX`) with four operations: `push`, `peek`
into a slice, `consume(n)` and `len`. It lives in `main`'s frame beside the staging buffer — plain
data, no static, no `unsafe`.

- **`peek` and `consume` are separate on purpose.** A read copies the oldest bytes out, hands them
  to the client with `SYS_SAFECOPY`, and only then removes them. The copy can fail, and a failed
  read must consume nothing: the bytes are still owed to whoever asks next. A single
  pop-into-a-slice could not express that.
- **A full ring drops the newest byte.** Overwriting the oldest would tear a hole in the front of
  what the reader has not yet seen; refusing the new byte keeps everything already buffered, in
  order. The drop is counted, and the first of a boot prints `[diag tty] rx overflow`.
- **The index arithmetic cannot overflow.** Advancing an index is written as a subtraction from the
  capacity, because drivers ship with overflow checks off.

The bytes in the ring are exactly the bytes on the wire: no echo, no CR→LF, no erase, no line
buffering. Typing into a hand-run boot therefore shows nothing. A line discipline belongs with the
keyboard and the shell, and it will live in this module.

### Receive: answering `CDEV_READ`

The read handler validates as a write does and then follows `read_plan`, whose order is the
contract:

1. **A zero-length read answers `0`** — before the ring is looked at, and without recording a
   waiter.
2. **A line that can deliver nothing more answers `EIO`.** If the claim was refused at boot, or the
   line has stopped since, no byte can ever arrive. This comes before the ring is looked at.
3. **An empty ring answers `EAGAIN`**, and the caller is recorded as the waiter — see
   [notify-then-pull](#notify-then-pull-a-read-with-nothing-to-give).
4. **Otherwise copy** `min(asked, buffered, CDEV_MAX_IO)` bytes: `peek` into the staging buffer,
   `SYS_SAFECOPY(SAFECOPY_TO, …)` into the caller's grant, and `consume` only if the copy succeeded.
   The reply is the count, always at least one.

Until slice 6.6 `EIO` covered the refused claim and nothing else: if the delivery proof failed, or
the line stormed or could not be re-armed, a read of an empty ring still answered `EAGAIN` and its
reader waited for a notification that would never come. Now each of those stops the line for good (a
failed proof round did not even leave the `Proof` phase before), every later read answers `EIO`, and
the reader that was already parked is sent its one `NOTIFY` on the transition, so its retry hears
the `EIO` too. During the proof itself receive is merely not on *yet*, so a read then is still
answered `EAGAIN` — its waiter is woken by the first byte, or by the proof failing. Each way of
stopping has already printed a forbidden `FAIL` line, so no healthy boot shows any of this; it is
held by host tests.

## What the boot log proves

TTY writes its own banner as its first console output, once its mappings are in place — to the UART,
and then to the screen, of which it is the first line:

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

Two of VFS's lines in that exchange prove something about the kernel rather than about TTY:

```text
[diag vfs] cdev.short ok n=256
[diag vfs] cons.notify ok
```

The banner write is the first request TTY ever receives, so it is the one TTY's
[one-shot probe](#transmit) answers with a `NOTIFY` *before* the reply. The short write is VFS's
second request, and it is therefore sent with a notification from TTY already latched at VFS.
`cdev.short ok n=256` says the `SENDREC` came back with TTY's reply — 256, the clamp. Without the
kernel's guard the notification comes back instead and the line reads `cdev.short FAIL rc=4096`,
`0x1000` being `NOTIFY_MESSAGE`. `cons.notify ok` is the other half: once VFS's start-up is over and
it is in its ordinary receive loop, the notification arrives there, where it belongs, and VFS prints
the line for the first one of a boot. Nothing is parked yet, so the retry it triggers finds nothing
to do. Both lines print on every boot.

One honest caveat: under QEMU's TCG the Device *attribute* is not observably load-bearing. QEMU's
PL011 works through a Normal write-back mapping — which is why the kernel's own HHDM alias has
always been one — so substituting the Normal attribute index changes no marker in the boot log. The
attribute is proved by construction and assertion, not empirically. The same is true of the
`FR.TXFF` poll (TCG's FIFO never fills) and of the LF→CRLF translation (the log checker matches
literal substrings and cannot express a carriage return).

Receive is proved end to end, by a byte that really travels the wire. QEMU's serial port is its
standard input, so the two full boots CI checks are *fed*: a small script
([`tools/feed-console.sh`](../build.md#feeding-the-console)) watches the boot's own log and, at the
right moment, writes `minix.rs stdin` and a newline to QEMU — in two pieces, a few seconds apart,
for a reason that comes at the end. (Since slice 6.6 the script feeds
[the second console](#the-proof-on-a-fed-boot) first and the serial one after; what follows is the
serial half.) Four lines tell the story in order:

```text
[diag tty] rx on
[diag vfs] cons.park
[diag vfs] cons.beside ok
minix.rs init: stdin ok n=15
```

`rx on` is TTY's, printed when the receive masks are set; it needs no input and every boot prints
it. `cons.park` is VFS saying it has parked its first console read — a child of `init` blocked in
`read(0)`. `cons.beside ok` is VFS saying it has just completed a file read for somebody else *while
that read was parked*, which is the line that shows a process waiting on the console costs the rest
of the system nothing. Only then — and once the second console's `hvc0 ok` is in the log as well —
does the script write, and `stdin ok n=15` is the child reporting that the fifteen bytes it read are
the fifteen that were sent, unechoed and unrewritten. Nothing but TTY's `NOTIFY` can complete that
read, so the last line appearing at all is the proof that the interrupt, the ring, the notification
and VFS's retry each did their part. A boot nobody feeds prints the first three and never the
fourth.

The two pieces are what make that last line prove the *loop* and not just its first turn. Bytes
written together reach TTY as one interrupt, so a probe written in one go would be delivered whole
by a driver that serviced exactly one interrupt and never re-armed its line — and when the feeder
did write it in one go, deleting the `IRQ_ENABLE` after the drain changed nothing in the log. Split
in two, the second piece needs a second interrupt, which only a re-armed line can raise. The child
reads the first piece, asks again, and reports only when it holds all fifteen bytes.

What the log does *not* prove is worth stating, because the code looks more tested than it is.
Nothing shows that acknowledging before draining is the right order — under QEMU the reverse order
behaves identically, so that rests on the argument
[above](#the-interrupt-line-a-proof-then-receive). And nothing exercises the step that keeps a
failed copy from consuming bytes **in TTY**: `peek` and `consume` are each tested, but no boot
refuses a console copy with bytes waiting, so their order in TTY's read handler is checked by
reading it. (`virtio-console` has the same step and does prove it, with a read into an unmapped
buffer.)

Since slice 6.7 three more lines, required of every full boot, say that the console has a screen:

```text
[fb] ok w=800 h=600 pitch=3200 r=16 g=8 b=0 usable=0 pa=0x...
[devmap] tty fb va=0x100000000 pages=469 pa=0x...
[diag tty] fb ok w=800 h=600 cols=100 rows=37
```

The first is the kernel accepting
[the bootloader's framebuffer](../boot/overview.md#the-boot-framebuffer), the second the loader
mapping it into TTY, the third [TTY's own verdict](#asking-for-it). Each ends in the one field that
follows the firmware's memory map, `pa=`, so that everything before it can be asserted. None of the
numbers is a constant of this repository: they are the mode the firmware set, so a firmware with
another default moves all three lines together — the size in the first, `pages=` in the second, and
the size and the grid in the third.

Those lines are serial evidence, and the most they can say is that TTY believed it had a screen and
that its stores did not fault. **That the right pixels arrived is not something a driver can print
about itself.** It is proved from the host: [the screen check](../build.md#the-screen-check) takes a
dump of the running guest's display, decodes every character cell with the renderer's own
`decode_cell`, and looks each row of text up in the boot's serial log — which the UART sink wrote,
not the renderer.

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
calls `validate_range` with that extent's block count. On the CDEV band every driver shares the
parse through `server-rt::cdev`; TTY and `virtio-console` also share a validator, in `driver-rt`,
and the memory driver keeps its own.

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
grant-direction check and by the driver's own minor check rather than by a since-retired `EROFS`
stub. The two probes of the minor check, one read and one write, name `BDEV_MINOR_NONE` — a minor no
block driver serves, whatever is on its disk — and not a small literal, which `virtio-blk`'s
partition minors could grow to reach.

Every one of those markers is identical with and without the `boot-stubs` feature **and** with and
without the musl sysroot, and all but the three backend-specific lines — `bdev.root`, the `name=` of
`bdev.ds`, and the `bdev.minor` verdict — are identical on both root backends. That is what the
fixed 1 MiB image size buys: in the sysroot-absent build the archive packs a 15 KB `worker` ELF
under the name `hello`, so a content-sized image would make every size-derived marker
config-dependent — passing in one configuration and proving nothing in the other.

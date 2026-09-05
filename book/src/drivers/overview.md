# Drivers

A **driver** in minix.rs is an ordinary user-space process that happens to own a
piece of hardware. It is not a special kind of kernel module and it holds no
kernel privilege beyond a `k_call_mask` bit or two — it is a
`#![no_std]`/`#![no_main]` ELF, loaded from the MXBI archive into its own address
space, driving a SEF receive loop exactly like [a server](../servers/overview.md).

What separates a driver from a server is one page in its address space that no
other process has: the device's memory-mapped registers. Everything interesting
about this chapter follows from that page.

As of Phase 5 there are two drivers: **TTY** (`drivers/tty/`), the console, and
**`memory`** (`drivers/memory/`), the boot ramdisk. The VirtIO block, network, and
console drivers under `drivers/` are still empty placeholders (Phase 6), and so is
`drivers/driver-rt` — the shared driver runtime they will eventually use.

The two are worth contrasting up front, because `memory` is the counter-example to
the paragraph above: **its window is ordinary RAM, not MMIO**. It owns no hardware
at all. What makes it a driver rather than a server is its *protocol* — it answers
[BDEV](#the-bdev-protocol-and-the-memory-ramdisk) requests and knows nothing about
what its blocks contain. That is exactly the property Phase 6 needs, when
virtio-blk replaces it underneath an unchanged MFS.

## The device window

The kernel and user space agree on a range of virtual addresses reserved for MMIO,
declared in `kernel-shared/src/uspace.rs`:

```rust
pub const USER_DEVICE_WINDOW_BASE: u64 = 0x4000_0000;   // 1 GiB — one whole L1 slot
pub const USER_DEVICE_WINDOW_SIZE: u64 = 0x0100_0000;   // 16 MiB
pub const TTY_UART_VA: u64 = USER_DEVICE_WINDOW_BASE;   // page 0 of the window
```

That module is deliberately separate from `message.rs`, which holds every other
shared constant. The rest of `kernel-shared` describes a *message* ABI — bytes on
the wire between two processes, where a wrong value gets a request rejected. This
is an *address* ABI: the kernel installs a mapping and user code dereferences it,
with no message in between, and a wrong value is a data abort.

`0x4000_0000` is chosen to be a whole L1 slot (1 GiB-aligned, so the window costs
exactly one L1 entry) and to sit clear of every occupied user VA — server images
at 1 MiB, server stacks at 2 MiB, demo-stub code and stacks at 4 and 8 MiB, VM's
heap origin at 16 MiB, VM's mmap arena at 32 MiB.

Two of those grow on request, and a compile-time assert on their *bases* would prove
only where they start. The heap's end is whatever `brk` last asked for, and the mmap
arena is a bump allocator that never reuses addresses — so `servers/vm/src/region.rs`
bounds both with a runtime `REGION_LIMIT` check (`ENOMEM` past it) rather than
trusting the 992 MiB of slack. That matters even though nothing is exploitable
today: the window's whole purpose is to be kernel-owned in *every* address space, so
Phase 6 can pre-map a device page into any driver without first asking whether VM
already promised that VA to the process's heap.

## The boot pre-map

TTY has no way to *ask* for its register page. There is no `VMCTL_MAP_PHYS`
subcall — a subcall letting a user-space server name an arbitrary physical address
would hand it the whole machine — so decision D1 deferred VM-mediated device
mapping to Phase 6 and made the one mapping Phase 5 needs a kernel bring-up step.

The kernel installs it in `load_boot_server` (`arch/aarch64/userland.rs`), right
after the ELF and stack are in place and just before the process is enqueued:

```rust
if nr == TTY_PROC_NR {
    map_page_in(img.ttbr0_pa, TTY_UART_VA, uart::PL011_PHYS_BASE as u64, Prot::DEVICE_RW)
        .expect("TTY UART pre-map");
}
```

Two placement details matter:

- **Not in `load_exec_image`.** That helper is shared with `system::do_exec`, so a
  device mapping there would be inherited by every binary any process ever exec'd.
  The consequence is worth knowing in the other direction too: a process that
  exec'd would *lose* its device window.
- **No TLB maintenance.** The address space was built moments ago and has never
  been installed in TTBR0 (and a recycled ASID is always clean, because address-space
  teardown flushes before returning the ASID to the pool). Beyond that,
  `switch_ttbr0_with_asid` — which runs on TTY's first schedule — already issues
  `isb; tlbi aside1; dsb ish; isb`. This is the same reasoning the server stack-page
  mapping already relies on.

The boot log records it as `[devmap] tty va=0x40000000 pa=0x9000000 attr_idx=<n>`.
The two addresses are fixed constants and the boot-log checker asserts both; `<n>`
is whichever MAIR index the scan below settled on, which depends on what the
bootloader programmed, so it is deliberately *not* asserted.

## Device memory: read the MAIR, never write it

An MMIO mapping must be **Device** memory, not the Normal write-back the rest of
user space uses: the CPU must not cache a register read, must not merge two
register writes into one, and must not reorder a data-register store ahead of the
flag-register poll that gates it.

On aarch64 the memory type of a page comes from its descriptor's 3-bit `AttrIndx`
field, which selects one of eight bytes in `MAIR_EL1`. The obvious move — program a
byte with a Device encoding — is the one thing the kernel must not do. Changing byte
*i* retroactively changes the memory type of **every live mapping** that uses
`AttrIndx=i`, and that includes Limine's TTBR1 kernel and HHDM mappings, whose
indices this codebase has no way to enumerate. Turning the HHDM into Device memory
would be silent, unrecoverable corruption.

So `mmu::init_device_attr_idx()` **reads** `MAIR_EL1` and reuses an index that
already encodes a Device type. The observation that makes reading sufficient: an
*unprogrammed* MAIR byte reads `0x00`, and `0x00` is itself a valid MMIO encoding
(Device-nGnRnE). "An index that already encodes Device" and "an index nobody uses"
therefore coincide — either way, mapping through it is correct. Reading changes
nothing, so no barrier or TLB invalidation is needed.

Only two encodings qualify. `0x04` (Device-nGnRE) is preferred; `0x00`
(Device-nGnRnE) is stricter and accepted. Device-nGRE would permit *gathering*, so
two data-register stores could merge into one and lose a character; Device-GRE
would additionally permit reordering the store ahead of its poll. On QEMU with
Limine today the scan settles on index 1 — reported as
`[mair] device attr_idx=1 byte=0x00`, which is forensic output and deliberately
*not* a boot-log marker, since which index is free depends on the bootloader.

## `Prot.device` and the RAM/device invariant

`Prot` — the kernel's "what may EL0 do with this page" type — carries a third flag
beside `writable` and `executable`:

```rust
pub struct Prot { pub writable: bool, pub executable: bool, pub device: bool }
```

Adding it was a compile error at every struct literal, which was the point: there
were exactly two, and both had to make a decision. `do_vmctl`'s `VMCTL_PT_MAP`
answers `device: false` permanently — VM may not mint device mappings.

The flag exists because a device leaf's physical address is **not a frame the
allocator owns**, so every path that tears down an address space must not hand it
to `free_frame`. Rather than leave that as a convention, `map_page_in` makes it a
total invariant:

```rust
if prot.device { assert!(!is_usable_pa(pa), "device mapping of RAM PA {pa:#x}"); }
else           { assert!( is_usable_pa(pa), "normal mapping of non-RAM PA {pa:#x}"); }
```

Every mapped leaf is therefore provably either `(RAM ∧ ¬device)` or
`(device ∧ ¬RAM)`, with no third case — which is what makes
`if !prot.device { free_frame(…) }` *sound* in each of the five leaf sweeps
(exit teardown, fork's copy loop, fork's out-of-memory unwind, `VMCTL_PT_UNMAP`,
and the exec-load error path) rather than merely plausible. `free_frame` keeps its
loud out-of-range assert for the same reason: a catch-all that silently skipped
non-RAM frames would demote a forged-address or double-free bug into an
untraceable leak.

Two further consequences:

- **Fork re-maps a device leaf, it does not copy it.** MMIO is inherently shared,
  and copying 4 KiB of live device registers through the cacheable HHDM alias would
  read side-effecting registers into RAM.
- **`mm::uaccess` refuses a device leaf as copy source or destination** (`EFAULT`),
  closing the hole where a process grants a peer its own register window and the
  kernel then touches MMIO through a cacheable alias.

Because TTY never exits, the teardown path's device arm would be untested code
sitting on a live landmine — and a missing guard is a kernel panic, not a leak. So
`userland_bootstrap` runs a small unconditional selftest at boot: build a throwaway
address space holding exactly one device leaf, tear it down, and assert the result.
It reports `[devmap] selftest ok freed=0 devs=1`, asserting **both** numbers —
`freed=0` proves the leaf was not freed, and `devs=1` proves it was actually seen
(a guard that skipped every leaf would report `devs=0`).

## The CDEV protocol

Character drivers answer requests in the `CDEV_RQ_BASE = 0xB00` band. Phase 5
defines two, sharing one payload:

| Field | Payload offset | Meaning |
|---|---|---|
| minor | `0..4` (i32) | which device; `CDEV_MINOR_CONSOLE = 0` is the UART |
| grant id | `4..8` (i32) | names the client's buffer (`CPF_READ` for a write, `CPF_WRITE` for a read) |
| length | `8..12` (i32) | bytes requested |
| offset | `16..24` (u64) | where in the granted range to start |

Five properties of that table are load-bearing.

**There is no granter field.** The driver takes the granter from the
kernel-stamped `m_source`. TTY holds `SYS_SAFECOPY` and its clients do not, so a
caller-supplied granter endpoint would let any client aim a privileged
cross-address-space copy at a third party's memory *through* the driver — a
confused deputy. This is the same anti-spoof property `DS_PUBLISH` relies on, and
it binds every grant-id-carrying request in the CDEV, BDEV, and FS bands.

**The reply is a byte count, not a status.** `m_type` comes back as the number of
bytes written (`>= 0`; zero is legal) or a negative errno. A driver replying `OK`
would be telling its client that the whole buffer went out.

**A driver MAY answer short — never must.** The client's contract, POSIX
`write()`'s, is to re-send with `offset` advanced until the request is out; that
is what lets a driver stage through a fixed buffer in its `main` frame with no
allocator at all. TTY clamps to `CDEV_MAX_IO` (256 bytes) for exactly that
reason. The memory driver's `/dev/null` and `/dev/zero` (slice 5.11) stage
nothing and never clamp — a `CDEV_WRITE` longer than `CDEV_MAX_IO` still comes
back reporting the whole count.

**`CDEV_READ` is the same payload, copy reversed** (slice 5.11). The reply is
the byte count read, `0` is EOF, and a short read is legal — POSIX `read()`'s
contract and the one VFS already assumes for `FS_READ`, so VFS sends one request
and reports what came back. It existed only as a plan note until `/dev/zero`
needed it: the 5.3 text said the two devices would be "new minors, not new
requests", which is true of `/dev/null` and of writing `/dev/zero` and false of
reading it. TTY does not serve it until Phase 6 gives it RX (`SYS_IRQCTL`), and
answers it `ENOSYS` from its unknown-request arm until then — VFS routes a
console `read()` there anyway, so Phase 6 changes TTY and nothing else.

**Minors are a per-driver namespace.** TTY's console is 0; the memory driver's
`/dev/null` and `/dev/zero` are CDEV minors 3 and 5 (MINIX 3's `NULL_DEV` and
`ZERO_DEV`), on the same driver as BDEV minor 0's ramdisk. The request band, not
the minor value, tells them apart.

## TTY

`drivers/tty/` is three files:

- **`cdev.rs`** — the pure, host-tested half: `validate_write` applies the checks
  in order (unknown minor → `ENXIO`, negative length → `EINVAL`, invalid grant id
  → `EINVAL`, then clamp to `CDEV_MAX_IO`) (the four-field parse moved to
  `server-rt::cdev` in 5.11, when the memory driver became its second user). It is
  a total function, so a malformed request becomes an invalid *value* the
  validator rejects, never a panic.
- **`pl011.rs`** — the crate's only `unsafe`: volatile accesses to `FR` and `DR` at
  `TTY_UART_VA`, polling `FR.TXFF` before each store, translating LF to CRLF. The
  register offsets are deliberately duplicated from the kernel's own PL011 writer;
  they cannot be shared, because the kernel crate is bare-metal-only and pinned by
  `forced-target`, so it can never be a user-space dependency — and a register
  layout is a hardware fact, not a shared ABI.
- **`main.rs`** — the SEF loop and the handler.

The handler captures `m_source` **first** (it is both the reply target and the
granter), validates, pulls the bytes across with
`SYS_SAFECOPY(SAFECOPY_FROM, caller, gid, offset, staging, n)`, transmits, and
replies `n`. A negative `SYS_SAFECOPY` result is relayed **verbatim**: `EPERM`
("your grant does not authorize this") and `EFAULT` ("your buffer is not mapped")
are different bugs on the client's side.

Two departures from the server template are worth naming. The staging buffer lives
in `main`'s frame, never in the init callback's — the kernel writes into it while
TTY is blocked inside the `SYS_SAFECOPY` SENDREC, so the frame must outlive every
call that names it (the same rule `GrantPool` follows). And an **unknown `m_type`
gets a reply**, where DS harmlessly drops one: a driver's clients all SENDREC, so a
dropped request blocks the caller forever.

## What the boot log proves

TTY writes its own banner straight to the UART once its mapping is in place:

```
minix.rs console: tty online (EL0)
```

That line is the milestone, and it is identifiable for a specific reason: it carries
**no kernel trace prefix**. Every other line in the log is `[as]`, `[ipc]`,
`[ksys …]`, `[diag …]`, `[pf]`, `[devmap]` — kernel-formatted. This one was composed
at EL0 and reached the wire through a user-space store to a device register.

VFS then drives the protocol as the first client: it resolves TTY through DS (rather
than hard-coding its boot endpoint), writes a banner through a read-only direct
grant and checks the reply against the granted length, asks for `CDEV_MAX_IO + 8`
bytes and requires exactly `CDEV_MAX_IO` back, and finally issues two requests that
must be *refused* — minor 7 with a perfectly good grant (`ENXIO`, from TTY's own
minor check) and a grant issued to PM instead of TTY (`EPERM`, from the kernel's
grantee check, the property that makes a grant id safe to pass around at all).

One honest caveat: under QEMU's TCG the Device *attribute* is not observably
load-bearing. QEMU's PL011 works through a Normal write-back mapping — which is why
the kernel's own HHDM alias has always been one — so substituting the Normal
attribute index changes no marker in the boot log. The attribute is proved by
construction and assertion, not empirically. The same is true of the `FR.TXFF` poll
(TCG's FIFO never fills) and of the LF→CRLF translation (the log checker matches
literal substrings and cannot express a carriage return).

## The BDEV protocol and the `memory` ramdisk

Block drivers answer requests in the `BDEV_RQ_BASE = 0xA00` band — between VFS
(`0x800`) and CDEV (`0xB00`), with `0x900` reserved for the VFS↔FS band. Two
requests are defined:

| Field | Payload offset | Meaning |
|---|---|---|
| minor | `0..4` (i32) | which device; `BDEV_MINOR_RAMDISK = 0` is the boot image |
| grant id | `4..8` (i32) | names the client's buffer |
| length | `8..12` (i32) | bytes requested; at most `BDEV_MAX_IO` = one block |
| block | `16..24` (u64) | which block of the device |

`BDEV_READ` fills the client's buffer (so the grant needs `CPF_WRITE`, and the
driver pushes with `SAFECOPY_TO`). `BDEV_WRITE`, real since slice 5.10a, is the
same request read backwards: it pulls the client's bytes into the device with
`SAFECOPY_FROM`, so the grant has to carry `CPF_READ` instead. One parse and one
validation serve both — the payload is identical, and only the dispatch arm knows
which way the bytes go. The driver never checks the access bit itself, and must
not: the kernel's `verify_grant` does, and a driver that re-derived the grant
rules would be a second place for them to drift.

Most of that table repeats CDEV's rules — no granter field, and the reply `m_type`
*is* the byte count. Three things are deliberately different:

**An over-long request is `EINVAL`, not a short read.** A short *write* is a POSIX
contract every client already loops over. A short *block read* is useless: a
filesystem cannot interpret half a block, so clamping would push a retry loop into
every caller for nothing.

**An out-of-range block is `EINVAL`, not `EIO`.** A block device's size is known to
its client — MFS reads it from the superblock's `s_zones` — so asking past the end
is a caller bug. `EIO` stays reserved for Phase 6's real media errors, where the
request was well-formed and the *device* failed.

**`BDEV_WRITE` was numbered three slices before it worked.** From 5.7 to 5.10a
the arm answered `EROFS` rather than `ENOSYS`, because `ENOSYS` is already the
unknown-`m_type` answer and reusing it would have made "this driver has never
heard of writes" and "this driver knows about writes and refuses them"
indistinguishable to a client. Keeping the arm dispatched also kept it probed —
and the prediction held exactly: making the ramdisk writable changed one line
inside it, the direction handed to `sys_safecopy`.

### Where the blocks come from

`kernel/build.rs` builds a MinixFS v3 image at compile time (`tools/mkfs-mfs`,
called as a build-dependency library) and packs it into the MXBI archive as a
**non-ELF blob** named `rootfs`. At boot the kernel copies it, page by page, into
freshly allocated frames and maps them into the `memory` driver's address space —
in the same `load_boot_server` arm the UART page uses, and for the same reason: the
driver has no way to ask.

The window is declared beside the device window, and is **ordinary RAM**:

```rust
pub const RAMDISK_WINDOW_BASE: u64 = 0x8000_0000;   // 2 GiB — one whole L1 slot
pub const RAMDISK_WINDOW_SIZE: u64 = 0x0040_0000;   // 4 MiB
pub const RAMDISK_VA: u64 = RAMDISK_WINDOW_BASE;    // page 0 of the window
```

Two choices there are worth stating. It sits **above** the device window on
purpose: `region::REGION_LIMIT` is the base of the *lowest* kernel-owned window, so
placing every new window above that low-water mark means VM needs no edit at all
when one is added — and `assert!(USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE
<= RAMDISK_WINDOW_BASE)` is what keeps that true. And the pages are mapped
`Prot::RW_DATA`, not device: none of the `prot.device` machinery above applies, and
these frames take the ordinary `free_frame` path in every leaf sweep. (RW rather
than RO so slice 5.10's write path is a change in the driver rather than in the
kernel.)

The 4 MiB size comes from the *format*, not from today's image: seven direct zones
plus one single-indirect block address `7 + 1024` zones at 4 KiB, which is 4.03 MiB.
An image that fits the window is therefore an image `fs/mfs`'s reader can address
without a double-indirect arm.

The driver learns where it is through a new `SYS_GETINFO` selector, `GET_RAMDISK`,
which returns `(va, len)` and is **gated on the caller being `MEM_PROC_NR`**: the
ramdisk is mapped into exactly one address space, so the VA is meaningless — and
actively misleading — anywhere else.

### The driver has no `unsafe` block

`drivers/memory/` never dereferences its mapping. Client transfers go through
`SYS_SAFECOPY`, and even the boot self-check reads the image through
`SYS_COPY(SELF → SELF)` rather than a raw load. So a page the kernel failed to map
surfaces as an `EFAULT` *return value* from a kernel call — a better diagnostic
than TTY's equivalent, which is an EL0 data abort — and there is no MMIO sibling
module to exclude from coverage.

The self-check is deliberately **device-level, not format-level**: it reads a
32-byte image header that `mkfs-mfs` writes into block 0's boot block (bytes
`0..1024`, which MinixFS never reads), never a superblock. A block driver that
decoded a superblock would depend on the filesystem format, which is precisely the
dependency Phase 6 has to unwind when virtio-blk replaces the ramdisk. What
licenses that shortcut is a host test in `tools/mkfs-mfs` asserting the header's
three fields equal the real superblock's.

It also reads a **tail label** from the image's reserved last block, whose text
differs from the header's. That is not decoration: a kernel copy loop that failed to
advance would map 256 pages of block 0 and pass every header check. The tail is the
only thing in the boot that proves the copy reached the end of the blob — confirmed
by mutation, where sourcing every page from block 0 moved exactly one marker,
`ramdisk FAIL tail label`.

### The character minors

Since slice 5.11 the same driver serves `/dev/null` (CDEV minor 3) and
`/dev/zero` (minor 5), as MINIX 3's memory driver does beside its ramdisks.
Minors are a per-driver namespace, and on a driver serving two bands, the band
tells them apart — so `cdev::classify` refuses minor 0 here, which is TTY's
console, and the BDEV ramdisk's minor 0 never meets these two.

Both minors discard a `CDEV_WRITE` and answer the **whole** count with no copy
at all; `/dev/null` answers a `CDEV_READ` with `0`, and `/dev/zero` fills the
whole request from a 256-byte static, walking the grant in `CDEV_MAX_IO` steps.
Nothing is clamped: `CDEV_MAX_IO` protects TTY's stack staging buffer, and there
is no staging here. Two consequences worth knowing. A `/dev/null` write with an
unmapped buffer *succeeds*, as it does on Linux, because nothing reads the
buffer — so no bad-buffer probe may ever be aimed at it. And the driver still
has no `unsafe` block: the only copy is a kernel call.

VFS probes the validator from its prologue (`[diag vfs] mem.deny ok n=5`),
because VFS's own device table maps only minors that exist and could never send
a bad one. One of those five is the first such refusal on the CDEV band: a
`CDEV_READ` through a read-only grant, refused by the kernel's `verify_grant`
and relayed as `EPERM`. MFS's `bdev.deny` battery already exercises the same
`CPF_WRITE` check against a `BDEV_READ` (its `read-only` probe), so this is a
CDEV-band first, not a kernel-wide one.

### What the boot log proves

```
[ramdisk] mem va=0x80000000 len=1048576 pages=256
[diag memory] ramdisk ok blocks=256 tail=1
[diag mfs] bdev.ds ok ep=3
[diag mfs] bdev.tail ok match=1
[diag mfs] bdev.deny ok n=10
```

`blocks=256` cross-checks the header's own block count against the length
`GET_RAMDISK` reported — two independently derived numbers, binding the build-time
image geometry to the kernel's runtime copy.

MFS, not VFS, is this driver's BDEV client — since slice 5.8, when the
filesystem server took over the band. Its own `mount ok root=1 bs=4096
blocks=256` marker, the superblock decoded out of the block it asked for, is
what retired the earlier `bdev.read`/`bdev.head` pair: a driver that replied
`OK` to the wrong page, or returned the header for both, fails to decode there
instead of printing a marker of its own. `bdev.tail` is the one thing `mount`
cannot subsume — it reads the image's reserved *last* block, whose label
differs from the header's, and is the only proof that the copy loop filling the
ramdisk reached the end of the blob rather than looping over block 0. The ten
refusals in `bdev.deny` include the one grant check slice 5.3 could not reach —
a `CPF_READ`-only grant used as a copy *destination* — plus, since slice 5.10a,
two `BDEV_WRITE` probes aimed at the write path this driver now really
implements, refused by the kernel's grant-direction and minor checks rather
than by a since-retired `EROFS` stub.

Every one of those markers is identical with and without the `boot-stubs` feature
**and** with and without the musl sysroot. That is what the fixed 1 MiB image size
buys: in the sysroot-absent build the archive packs a 15 KB `worker` ELF under the
name `hello`, so a content-sized image would make every size-derived marker
config-dependent — passing in one configuration and proving nothing in the other.

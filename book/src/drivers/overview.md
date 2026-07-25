# Drivers

A **driver** in minix.rs is an ordinary user-space process that happens to own a
piece of hardware. It is not a special kind of kernel module and it holds no
kernel privilege beyond a `k_call_mask` bit or two — it is a
`#![no_std]`/`#![no_main]` ELF, loaded from the MXBI archive into its own address
space, driving a SEF receive loop exactly like [a server](../servers/overview.md).

What separates a driver from a server is one page in its address space that no
other process has: the device's memory-mapped registers. Everything interesting
about this chapter follows from that page.

As of Phase 5 there is exactly one driver, **TTY** (`drivers/tty/`), the console.
The VirtIO block, network, and console drivers under `drivers/` are still empty
placeholders (Phase 6), and so is `drivers/driver-rt` — the shared driver runtime
they will eventually use.

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
defines one:

| Field | Payload offset | Meaning |
|---|---|---|
| minor | `0..4` (i32) | which device; `CDEV_MINOR_CONSOLE = 0` is the UART |
| grant id | `4..8` (i32) | names the client's source buffer |
| length | `8..12` (i32) | bytes requested |
| offset | `16..24` (u64) | where in the granted range to start |

Three properties of that table are load-bearing.

**There is no granter field.** The driver takes the granter from the
kernel-stamped `m_source`. TTY holds `SYS_SAFECOPY` and its clients do not, so a
caller-supplied granter endpoint would let any client aim a privileged
cross-address-space copy at a third party's memory *through* the driver — a
confused deputy. This is the same anti-spoof property `DS_PUBLISH` relies on, and
it binds every grant-id-carrying request in the CDEV, BDEV, and FS bands.

**The reply is a byte count, not a status.** `m_type` comes back as the number of
bytes written (`>= 0`; zero is legal) or a negative errno. A driver replying `OK`
would be telling its client that the whole buffer went out.

**An over-long request is a short write, not a failure.** A request longer than
`CDEV_MAX_IO` (256 bytes) moves the first `CDEV_MAX_IO` bytes and reports that
count; the client re-sends with `offset` advanced. That is POSIX `write()`'s
contract, and it is what lets a driver stage through a fixed buffer in its `main`
frame with no allocator at all.

`CDEV_READ` is deliberately absent: receive needs interrupts (`SYS_IRQCTL`) and
arrives in Phase 6. The `/dev/null` and `/dev/zero` devices planned for slice 5.11
are new *minors* of `CDEV_WRITE`, not new request numbers.

## TTY

`drivers/tty/` is three files:

- **`cdev.rs`** — the pure, host-tested half: `parse_write` reads the four payload
  fields, `validate_write` applies the checks in order (unknown minor → `ENXIO`,
  negative length → `EINVAL`, invalid grant id → `EINVAL`, then clamp to
  `CDEV_MAX_IO`). Both are total functions, so a malformed request becomes an
  invalid *value* the validator rejects, never a panic.
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

# Slice 6.2 — `driver-rt`: virtio-mmio transport, virtqueues, DMA window, driver slots

**Date:** 2026-11-09 **Status:** design, pending review **Branch:** `feature/6.2-driver-rt`
**Predecessor:** slice 6.1 (`SYS_IRQCTL`, PR #70) **Tracker:**
[`docs/plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md) slice 6.2

Slice 6.2 builds everything a virtio driver stands on and proves it with one device answering:
virtio-blk boots as a real process, finds its device without probing, negotiates features, sets up a
queue against kernel-reserved DMA memory, claims its interrupt line, and reports the disk's
capacity. It serves no request — that is 6.3.

The phase-level decisions `H1…H12` are locked in the tracker and are **not re-decided here**; this
document cites them and settles what they left to the slice. Slice-local decisions are labelled
`T1…T14`. 6.1's `I1…I14` stay in force.

Three of the decisions below go beyond what the phase file says, because the code turned out to need
them. They are marked **(finding)** and listed again in [§8](#8-rulings-to-check).

---

## 1. What exists, and what 6.2 changes

| Component                              | Today                                                    | After 6.2                                                                              |
| -------------------------------------- | -------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `kernel-shared/src/com.rs`             | boot slots `0..=10`, stubs `11..=14`, `NR_BOOT_PROCS` 11 | four driver slots `11..=14`, stubs `15..=18`, `NR_BOOT_PROCS` 15 (T1)                  |
| `kernel/src/proc/table.rs`             | `N_IMAGE == 16`; image priv ids `0..=15`                 | `N_IMAGE == 20`; four `BootEntry` rows; image priv ids `0..=19` (T1, T2)               |
| `kernel/src/arch/aarch64/userland.rs`  | stub priv ids `16..=19`; pre-maps for TTY and `memory`   | stub priv ids above `USER_PRIV_ID` (T2); a virtio arm: register page, IRQ, DMA (T5–T7) |
| `kernel/src/arch/aarch64/` (new file)  | —                                                        | the boot probe of the 32 transport slots (T4)                                          |
| `kernel/src/mm/frame.rs`               | `alloc_frame` / `free_frame`, one frame at a time        | `+ reserve_contiguous(pages)` (T7)                                                     |
| `kernel/src/system/do_getinfo.rs`      | `GET_WHOAMI`, `GET_RAMDISK`                              | `+ GET_VIRTIO_MMIO`, `GET_DMA` (T8)                                                    |
| `kernel-shared/src/uspace.rs`          | device window, ramdisk window                            | `+ VIRTIO_MMIO_PAGE_VA`; `+` the DMA window (T5, T7)                                   |
| `kernel-shared` (new module)           | —                                                        | the pure probe/assignment logic, host-tested (T4)                                      |
| `servers/pm/src/mproc.rs`              | documents `FORK_POOL_BASE` as 15                         | 19; the prose and the test comments rewritten (T1)                                     |
| `drivers/driver-rt`                    | 4-line placeholder                                       | `IrqLine`, `Transport` + `MmioTransport`, split virtqueue, DMA layout (T9–T11)         |
| `drivers/tty`                          | local `SYS_IRQCTL` claim / re-arm                        | claims and re-arms through `driver_rt::IrqLine` (T9)                                   |
| `drivers/virtio-blk`                   | 5-line placeholder, not packed                           | a branded boot server at slot 11 that brings its device up and idles (T12)             |
| `kernel/build.rs`                      | 12 archive rows; watches four shared crates              | `+` the virtio-blk row; `+ drivers/driver-rt/src` in the shared watch list             |
| `tools/qemu-run.sh`                    | one `fat:rw:` virtio-blk-**pci** drive                   | `+` a raw image on a pinned `virtio-blk-device`; `force-legacy=false` (T13)            |
| `tools/gen-c-headers`                  | emits 16 process names                                   | emits 20; `NR_BOOT_PROCS` 15; the two selectors held back, with a test (T8)            |
| `tests/qemu-boot.{expected,forbidden}` | stubs at `11..=14`, fork pool from 15                    | stubs at `15..=18`, fork pool from 19; the new markers of [§6](#6-verification)        |

---

## 2. Slots

### T1 — the four driver slots, named

H1's working names are adopted as final:

| Constant         | Proc nr | Proc name | virtio `DeviceID` | Crate (slice)                  |
| ---------------- | ------- | --------- | ----------------- | ------------------------------ |
| `VBLK_PROC_NR`   | 11      | `vblk`    | 2                 | `drivers/virtio-blk` (6.2)     |
| `VCON_PROC_NR`   | 12      | `vcon`    | 3                 | `drivers/virtio-console` (6.6) |
| `VINPUT_PROC_NR` | 13      | `vinput`  | 18                | `drivers/virtio-input` (6.8)   |
| `VNET_PROC_NR`   | 14      | `vnet`    | 1                 | `drivers/virtio-net` (6.9)     |

`NR_BOOT_PROCS` becomes `VNET_PROC_NR + 1` = 15. The stubs move to `15..=18` and `FORK_POOL_BASE`
(still `NR_BOOT_PROCS + NR_STUB_PROCS`) to 19, leaving 13 fork slots. Each new slot gets a
`BootEntry` row with the profile `memory` and `tty` have (`SYS_PROC | PREEMPTIBLE`, `SRV_T`,
`SRV_Q`). Three of the four have no ELF in 6.2 and stay blocked on `RTS_NO_PRIV` exactly as
`PFS_PROC_NR` does today — that is the existing precedent for a wired slot with no image, and PM, VM
and SCHED already tolerate it.

`ipc_to` needs no hand-wiring: the new rows are inside the image, so `populate_priv`'s
`fill_bits(.., n_active)` covers them in both directions once `n_active` is 20.

`INIT_PROC_NR` stays 10. The drivers are appended after it, so `IMAGE` is no longer "servers, then
init last", and `NR_BOOT_PROCS` is no longer defined from `INIT_PROC_NR`. Every comment and test
that says otherwise (`nr_boot_procs_covers_init`, "init is the last slot allocated from the boot
image") is rewritten.

The three texts H1 names are rewritten, not annotated: the `com.rs` stub block ("slots 11..=14 are
reserved for the stubs"), `mproc.rs`'s `FORK_POOL_BASE` prose and its `(15)` test comments, and
`table.rs`'s `N_IMAGE == 16`. PM's stub pids move with the slots (PM numbers boot pids in slot
order); nothing in the marker files names a stub pid, and the plan confirms that by grep before
relying on it.

### T2 — the stub priv slots move above `USER_PRIV_ID` **(finding)**

H1 counts proc slots. It does not mention that a boot process's **priv id is its `IMAGE` index**
(`NR_TASKS + nr`), so the four new rows take priv ids `16..=19` — which are today
`STUB_A_PRIV_ID…STUB_D_PRIV_ID`. Left alone, `install_stub_privs` would overwrite the driver slots'
privileges at boot with the stubs on, and `--no-default-features` would hide it.

`USER_PRIV_ID` stays 20, which is now exactly `N_IMAGE`. The stub priv ids are redefined relative to
it — `USER_PRIV_ID + 1 ..= USER_PRIV_ID + 4` — and two compile-time asserts pin the ordering so the
next slot added to the image fails the build instead of colliding:

- `USER_PRIV_ID >= N_IMAGE` (the shared user slot is never an image slot);
- the lowest stub priv id is above `USER_PRIV_ID`.

`populate_user_priv` and `install_stub_*_priv` already open their reverse edges explicitly because
both are above `n_active`; that stays true and stays necessary.

*Rejected:* moving `USER_PRIV_ID` up and leaving the stubs at `16..=19` shifted by four — two
renumberings instead of one, and `USER_PRIV_ID`'s value appears in more prose.

### T3 — H12's check, done

The musl fork submodule (`external/musl`) was searched at design time for `NR_BOOT_PROCS`, `STUB_*`,
`NR_STUB` and `FORK_POOL`: no C, header or assembly reads any of them. H12's ruling stands and 6.2
needs no fork bump: generated headers are never committed, so the fork picks the new values up at
its next build. The plan re-runs the grep against the pinned submodule commit as its first task and
records the result; if it finds a hit, the slice stops and becomes a coordinated ABI PR.

**Measured (2026-11-21).** Re-run against the pinned submodule checkout (`4501ccf402fa`), the grep
for `NR_BOOT_PROCS|STUB_[A-D]|NR_STUB|FORK_POOL` over `*.h`, `*.c` and `*.S` printed nothing. H12
stands.

---

## 3. The kernel side

### T4 — the boot probe

A new step in `userland_bootstrap`, before `device_teardown_selftest` (T7 explains the ordering).
For each of the 32 transport slots the kernel reads `MagicValue` (`0x000`), `Version` (`0x004`) and
`DeviceID` (`0x008`) through the HHDM and prints one `[virtio]` line per **present** device
(`MagicValue == 0x7472_6976` and `DeviceID != 0`); empty slots print nothing.

The platform facts — base `0x0A00_0000`, stride `0x200`, 32 slots, INTID `48 + n` — live in the
aarch64 arch module beside `gic.rs`. The decision logic is **pure and host-tested** in
`kernel-shared` under the standing carve-out for predicates the kernel crate cannot test: given the
32 `(magic, version, device_id)` triples and the table of T1, produce at most one assignment `(slot,
page_pa, in_page_offset, intid)` per driver, plus the refusals.

Rules, in order:

1. **A page with more than one present device assigns nothing**, and each device in it gets a
   `[virtio] refuse` line naming the page and both slots. This is **stricter than H2's wording**
   ("the slot of more than one *driver's* device"): a device no driver claims — a stray
   `virtio-rng-device`, say — would otherwise have its registers mapped into its page-mate's driver.
   The boundary H2 wants is "a driver reaches one device's registers", and only the stricter rule
   gives it. **(finding)**
2. **The lowest slot wins a `DeviceID`.** A second device of the same type in another page is
   reported and left unassigned; multiple disks are out of scope.
3. **A device whose driver has no module in the boot archive is reported and left alone** — no
   mapping, no DMA run, no allow-list entry. The probe line says which driver it would belong to.

The kernel **does not judge `Version`**. It prints it and passes it on; refusing a legacy transport
is the transport's job (H4, T10), so the refusal line can name the forgotten QEMU flag from the one
place that knows what a version means. That keeps the kernel's virtio knowledge at H2's three
register reads.

The probe result is a small boot-written static (`[Option<Assignment>; 4]`, the usual `UnsafeCell`
newtype). Unlike `GET_RAMDISK`, this cannot be recomputed on demand without re-reading device
registers a driver now owns.

### T5 — the register page

`uspace::VIRTIO_MMIO_PAGE_VA` is page 1 of the device window (`USER_DEVICE_WINDOW_BASE + 0x1000`).
Every virtio driver has its own address space, so one constant serves all four; page 0 stays TTY's
UART by convention even though no virtio driver maps it.

`load_boot_server` gains one arm, taken when the probe assigned this `nr` a device. It maps the
assignment's page `Prot::DEVICE_RW` at that VA and prints a `[devmap]` line in the TTY line's shape.
The same four answers the TTY and ramdisk arms give apply unchanged — not in `load_exec_image`, no
TLB maintenance, every step `.expect()`s — and the arm's comment says so by reference rather than a
third copy of the argument. The VA a driver uses is `VIRTIO_MMIO_PAGE_VA + in_page_offset`: slots
are `0x200` apart and the device may sit anywhere in its page.

### T6 — the allow-list is one entry long

In the same arm, beside the mapping, the kernel pushes the assignment's INTID onto the driver's
`Priv::irqs` — I4's "the allow-list follows the mapping", with H2's consequence that the list is
exactly one line. A driver with no device has an empty list and can claim nothing.

### T7 — the DMA window and its reserved run

`uspace` gains the third kernel-owned window: `DMA_WINDOW_BASE = 0xC000_0000` (one L1 slot, above
the ramdisk window, so `USER_REGION_LIMIT` does not move and VM needs no edit), `DMA_WINDOW_SIZE = 1
MiB`, `DMA_VA` = page 0, with the same family of geometry asserts the ramdisk window carries plus
`RAMDISK_WINDOW_BASE + RAMDISK_WINDOW_SIZE <= DMA_WINDOW_BASE`.

`mm::frame::reserve_contiguous(pages) -> Option<Frame>` bumps `pages` frames from **one** region's
`next_free_pa` — never the free list, never across a region boundary — and returns the first. The
frames are zeroed like `alloc_frame`'s. Contiguity is true by construction rather than by the order
of calls, which makes H3's "before the first `free_frame`" a property the function no longer depends
on. The reservation is still made in the probe step, ahead of `device_teardown_selftest` (the first
caller of `free_frame`), so H3 holds as written and the run does not fragment the region the boot
servers then bump from.

**One uniform run per assigned driver: `DMA_RUN_PAGES = 16` (64 KiB).** virtio-blk needs three pages
(T11). A single size keeps the kernel free of per-device sizing knowledge, and 64 KiB covers the two
queues and frame buffers 6.6 and 6.9 will want without revisiting the kernel. A driver with no
assignment gets no run.

The arm of T5 maps the run `Prot::RW_DATA` at `DMA_VA`. These are ordinary RAM frames inside a
usable region, so `map_page_in`'s `¬device ⇒ RAM` assert passes and none of the device machinery is
involved — the ramdisk window's shape.

**The kernel stamps the run so the driver can check the mapping.** Before mapping, the kernel writes
`pa ^ DMA_COOKIE` into the run's first eight bytes through the HHDM. The driver reads it through
`DMA_VA`, compares it against the `pa` `GET_DMA` reported, and zeroes it. That proves the VA the
driver holds and the PA it will hand the device are the same memory — the one fact about this window
that nothing else in 6.2 would observe, since no request reaches the device until 6.3.

**Debt recorded, not fixed: a driver that exits frees its run.** `teardown_addrspace` frees every
non-device leaf, so an exiting driver's DMA frames return to the allocator while the device may
still hold their addresses. Unreachable today — boot drivers are `MF_PRIV_PROC` and nothing kills or
restarts them — and H3's "nothing frees it" is true only under that condition. The slice that makes
a driver restartable must reset the device before teardown or exempt the run. It joins the
`Priv::irqs`-never-cleared item under 6.1's debts in the phase file.

### T8 — two `SYS_GETINFO` selectors

| Selector          | Value | Reply payload                                                                     |
| ----------------- | ----- | --------------------------------------------------------------------------------- |
| `GET_VIRTIO_MMIO` | 65    | `0..8` register base VA (u64), `8..12` INTID (u32), `12..16` transport slot (u32) |
| `GET_DMA`         | 66    | `0..8` VA (u64), `8..16` PA (u64), `16..24` length in bytes (u64)                 |

Both follow `GET_RAMDISK`: offsets as named `GETINFO_*_OFF` constants with the same layout asserts,
values outside MINIX's `0..=31` block.

- **Gate:** the caller's proc number must be one of T1's four, else `EPERM` — the addresses mean
  nothing in any other address space.
- **Absent:** a caller in the set with no assignment gets `ENOENT`, `GET_RAMDISK`'s answer for a
  missing module. This is how a driver "is told so" (H2) and it is not an error path: the driver
  prints one line and idles.

Two selectors rather than one because they answer H2 and H3 separately and 6.7's framebuffer will
want the first shape without the second. The transport slot is reported only so the driver's
diagnostic line can name the bus; no driver computes anything from it.

**D8.** Both are additive (H12), and **neither is emitted into the generated C headers.** That is
`GET_RAMDISK`'s recorded rule in `tools/gen-c-headers`' `callnr_h.rs`: a selector gated to a Rust
driver's proc number can never be called from C, and emitting it would freeze its value under D8 for
no reader. Each gets the same "deliberately not emitted" test `GET_RAMDISK` has. The four new
process names and `NR_BOOT_PROCS` **are** emitted — `com.h` lists every boot process.

---

## 4. `driver-rt`

The crate gains a dependency on `minixrs-server-rt` (the kernel-call wrappers and `diag_fmt`) and
splits into modules by what can be host-tested. Anything that touches a register or traps is thin
and excluded from coverage in `sonar-project.properties`; everything else is pure and tested.

### T9 — `IrqLine`, and TTY's migration

```rust
pub struct IrqLine { hook: i32, notify_id: u8 }

impl IrqLine {
    pub fn new(intid: u32, notify_id: u8) -> Result<Self, i32>;  // IRQ_SETPOLICY, policy 0
    pub fn rearm(&self) -> i32;                                  // IRQ_ENABLE
    pub fn fired(&self, pending: u32) -> bool;                   // bit notify_id set
    pub fn hook_id(&self) -> i32;
}
```

It owns nothing beyond the hook id: the "drain, acknowledge at the device, then re-arm" order of I12
stays the caller's, because only the caller knows what acknowledging its device means. No `Drop` — a
boot driver never releases its line, and the kernel's release-on-exit (I8) is the real cleanup.

TTY's `irq.rs` keeps its denial battery and its two-round proof and changes only how it claims and
re-arms: both `IRQ_SETPOLICY` calls (the claim and the I6 replacement) and the `IRQ_ENABLE` go
through `IrqLine`. Its markers do not move. The migration pays the three debts I12 lists — the
dependency with its `Cargo.toml` comment rewritten, the convention line in `servers-and-drivers.md`
(the "deliberately does not depend on" bullet is replaced, not appended to), and
`drivers/driver-rt/src` in `kernel/build.rs`'s shared watch list.

**The battery's "free slot" row is re-aimed** (6.1's carried debt). `claimed + 1` assumed TTY was
the only claimer; with virtio-blk claiming a line it can name vblk's hook, and `IRQ_ENABLE` on
another owner's hook is `EPERM`, not the `EINVAL` the row expects — a boot-order-dependent flake.
The row becomes `NR_IRQ_HOOKS` itself, the highest valid id, which is free for as long as the boot
has fewer than 16 claimers (five at the end of Phase 6). The row's comment states that bound.

### T10 — the transport trait and its MMIO implementation

H11's seam. The trait is what a device driver and the virtqueue need, and nothing register-shaped
leaks through it:

```rust
pub trait Transport {
    fn device_id(&self) -> u32;
    fn status(&self) -> u32;
    fn set_status(&mut self, status: u32);            // 0 resets
    fn device_features(&mut self) -> u64;
    fn set_driver_features(&mut self, features: u64);
    fn queue_max(&mut self, queue: u16) -> u16;       // 0 = queue unavailable
    fn queue_enable(&mut self, queue: u16, size: u16, layout: &QueueAddrs);
    fn notify(&mut self, queue: u16);
    fn interrupt_status(&self) -> u32;
    fn interrupt_ack(&mut self, bits: u32);
    fn config_read(&self, offset: usize, buf: &mut [u8]);  // generation-stable
}
```

The exact signatures are the plan's; the split is the decision. `MmioTransport::new(base_va)` is the
virtio-mmio **version 2** implementation (H4) and the only `unsafe` in the crate: volatile, aligned,
32-bit accesses, since the page is Device memory. It checks `MagicValue` again from user space — the
cheap proof that the VA the kernel reported is the device the kernel probed — and refuses `Version
!= 2` with an error the caller prints as the refusal line.

**Bring-up is one free function over the trait**, `negotiate(transport, wanted) -> Result<u64, _>`,
so the sequence is written once for every device and every future transport: reset, `ACKNOWLEDGE`,
`DRIVER`, read the device's features, fail unless `VIRTIO_F_VERSION_1` is offered, write `wanted ∩
offered` plus `VERSION_1`, `FEATURES_OK`, **read status back** and fail if the device cleared it. On
any failure it sets `FAILED`. `DRIVER_OK` is a separate call the driver makes after its queues are
enabled. The decision inside — which bits to accept, what counts as failure — is a pure function
tested on the host against a mock `Transport`.

`config_read` loops on `ConfigGeneration` until two reads agree, as the specification requires.

The full register map lives in `driver-rt`. The kernel's probe needs three of its offsets; those
three and the `DeviceID` values sit in `kernel-shared` with the probe logic, and a `driver-rt` test
asserts its own copies agree. That is the PL011 rule ("a register layout is a hardware fact, not a
shared ABI") bent only as far as the kernel's three reads require.

### T11 — the split virtqueue and the DMA layout

One queue type, split rings only, no indirect descriptors, no `EVENT_IDX` (H4). **Queue size is a
driver constant, 8**, clamped to the device's `QueueNumMax`: H3 allows one request in flight, a
virtio-blk request is three descriptors, and at eight entries the descriptor table, available ring
and used ring total under 256 bytes — one page holds all three with their alignments.

Two things are pure and host-tested, which is what the tracker means by "the queue arithmetic":

- **Layout.** Given a queue size and a `(va, pa, len)` DMA run, compute where the descriptor table,
  available ring and used ring go, with the alignments the specification requires, and where the
  driver's buffers start. Overflow-safe arithmetic throughout
  ([`rust-style.md`](../../conventions/rust-style.md)); a run too small is an error, not a wrap.
- **The ring discipline.** A free-descriptor list, chain building (`out` buffers then `in` buffers),
  publishing a head on the available ring with its index wrapping modulo 2¹⁶, and walking the used
  ring from a remembered `last_used`. The queue operates on a `&mut [u8]`-shaped view of the DMA
  page so the tests drive it against an ordinary buffer and play the device's half by hand.

virtio-blk's use of its run: page 0 the queue, page 1 the request header and status byte, page 2 the
one 4096-byte bounce buffer (H3). 6.2 computes and reports that layout; 6.3 puts data in it.

The queue takes no part in interrupt handling: a driver calls `IrqLine::fired`, walks the used ring,
calls `Transport::interrupt_ack`, then `IrqLine::rearm` — I12's order, spelled once in the crate's
documentation as the pattern every virtio driver's loop follows.

---

## 5. `virtio-blk` in 6.2 and the QEMU command line

### T12 — the first consumer brings its device up and idles

`drivers/virtio-blk` becomes a real boot server in the `drivers/memory` mould: `user.ld`, a
`build.rs` for the link arg, `brand!()`, a SEF loop from `server-rt`, a row in `kernel/build.rs`'s
archive list. It is packed **after `memory` and before `mfs`**: nothing depends on that in 6.2, and
6.3's DS chain will (`vblk < mfs`), so the row goes where it will have to be.

**After `sef_startup` returns** — not inside the `init_fresh` callback, where a non-OK return makes
startup fail and the driver spin; `memory` and `tty` both do their kernel-call bring-up after it for
that reason — it runs this sequence once. A step that fails prints one `[diag vblk] … FAIL` line
naming the step, abandons the rest, and the driver goes on to its receive loop degraded, as `memory`
does. It does not reset the device or release anything on the way out.

1. `GET_VIRTIO_MMIO`. `ENOENT` → print `[diag vblk] no device` and go idle. This is H5's healthy
   no-disk path, not a failure.
2. `GET_DMA`; verify and clear the cookie (T7).
3. `MmioTransport::new`; check `DeviceID == 2`.
4. `negotiate` with **no optional features** — `VIRTIO_F_VERSION_1` alone. 6.4 adds `FLUSH`.
5. Read `capacity` (the first eight bytes of device configuration, in 512-byte sectors).
6. Lay out and enable queue 0; `DRIVER_OK`; read status back.
7. `IrqLine::new` on the reported INTID; then one refusal probe — a claim on `INTID + 1` must answer
   `EPERM`, which is what shows the allow-list is one entry long and not merely non-empty.
8. Print the success line (§6).

Then it enters its receive loop and **blocks**. It publishes nothing to DS (H5: a driver publishes
only when it carries a root, which 6.3 decides) and answers every request from its own
unknown-request arm with `ENOSYS`, the arm `tty` and `memory` each write for themselves (`server-rt`
hands application messages back; it refuses nothing). A `HARDWARE` notification in 6.2 is unexpected
— nothing was submitted — and is reported once as a `FAIL` line and left masked, TTY's `irq FAIL
extra` shape.

Step 6 is deliberately further than the tracker's proof text requires (features and capacity only).
Enabling the queue costs a few lines and makes the device accept the PAs from T7 and T11 in this
slice rather than discovering a layout mistake in the middle of 6.3's first request.

### T13 — `qemu-run.sh`

Three additions:

- `-global virtio-mmio.force-legacy=false` (H4);
- a raw image file under `target/`, created by the script if absent with `dd` (no `truncate`, which
  macOS lacks), of a fixed sector count held in one shell variable;
- `-drive file=…,format=raw,if=none,id=vblk0` and `-device
  virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.N`.

**Bus pinning for the phase:** blk on the first slot of the first page, and each later device eight
buses on — console, input, net at the first slots of pages 1, 2 and 3. The concrete `N` values are
written from T14's first measurement, not from this paragraph.

The image is blank and its size is deliberately **not** the ramdisk's 1 MiB, so a `capacity=` that
matches cannot be the ramdisk length leaking through some shared path. 6.3 replaces the blank file
with the root image. The sector count appears once in the script and once in the expected-marker
file, and each names the other in a comment.

The existing `fat:rw:` drive stays as it is — virtio-blk-**pci**, read by the firmware and by
nothing in minix.rs — until 6.4.

### T14 — measure first

Four observations, each made with temporary probes or command-line edits before the code that
depends on it, and each recorded in this document with its date as 6.1's were:

1. **Which address `bus=virtio-mmio-bus.N` denotes** (the tracker's unverified fact 1). Attach two
   devices on buses meant to be in different pages and read the probe's output. QEMU is known to
   expose the transports to a Linux guest in reverse address order, which is exactly the kind of
   fact that makes "bus 0 is slot 0" worth observing.
2. **That the HHDM reaches `0x0A00_0000`.** The GIC at `0x0800_0000` and the PL011 at `0x0900_0000`
   are read through it today, so this is expected; the probe's first read is the measurement, and a
   fault there means the probe needs its own mapping — a design change to bring back, not to absorb.
3. **That edk2 still boots Limine from the `fat:rw:` drive with a blank virtio-mmio disk attached**,
   and how long the detour costs against the boot budget. If the firmware stalls or picks the wrong
   disk, the one permitted fix is an explicit `bootindex` on the FAT drive. If that does not restore
   the boot inside the existing 600 s budget, the slice **stops** and the question comes back: the
   default boot must keep the disk attached, or `capacity=` proves nothing in CI.
4. **What an empty transport reads** under `force-legacy=false` — expected `Version == 2`,
   `DeviceID == 0` — and what the device's state is at kernel entry after the firmware has touched
   it. `negotiate` resets first either way.

**Measured (2026-11-21), QEMU 11.0.3, aarch64 `virt,gic-version=3`, stub-free boot, a probe reading
the three registers of all 32 slots through the HHDM before `device_teardown_selftest`:**

1. **Bus to slot.** `bus=virtio-mmio-bus.N` is slot `N`, address `0x0A00_0000 + N * 0x200`; there is
   no reversal here. `virtio-blk-device` on bus 0 read at slot 0 (`DeviceID 2`) and
   `virtio-rng-device` on bus 8 read at slot 8 (`DeviceID 4`), in different pages. So `BUS_VBLK =
   0`.
2. **The HHDM reaches the transports.** All 32 reads, `0x0A00_0000` to `0x0A00_3E00`, completed with
   no fault.
3. **Boot.** Limine booted from the `fat:rw:` drive and the kernel banner printed with the two
   devices attached, with no `bootindex` needed. Wall-clock from `cargo run` start to the banner
   (build already done): 8.1 s and 7.2 s with the two devices, 7.0 s and 7.2 s without; the detour
   is inside the noise.
4. **Empty and legacy transports.** With `force-legacy=false`, an empty slot reads magic
   `0x74726976`, `Version 2`, `DeviceID 0`, and the populated slots read `Version 2`. Without it,
   every slot, empty or populated, reads `Version 1`: the version-2-only driver sees the legacy
   refusal, as T12 expects. Devices arrive at kernel entry already reporting their ids; the probe
   did not read `Status`, so firmware-left state is not recorded and `negotiate` resets regardless.

---

## 6. Verification

Marker text is fixed by the plan; counts are recomputed from constants, never copied from here
([`testing-and-markers.md`](../../conventions/testing-and-markers.md#markers)).

**Default boot — expected** (`tests/qemu-boot.expected`):

| Marker (shape)                                              | Proves                                                          |
| ----------------------------------------------------------- | --------------------------------------------------------------- |
| `[virtio] slot=… id=2 …` for the disk                       | the probe saw the device on the pinned bus (T4)                 |
| `[devmap] vblk va=… pa=…`                                   | the register page is mapped (T5)                                |
| `[as] vblk nr=11`                                           | the slot is live (T1)                                           |
| `[diag vblk] virtio ok features=… capacity=…`               | T12 steps 1–8: mapping, cookie, negotiation, config, queue, IRQ |
| `[as] stub A nr=15` … `stub D nr=18`; `child_nr=19`         | T1's renumbering, once                                          |
| `[diag tty] irq.deny ok n=…`, `irq ok rounds=…` (unchanged) | TTY works on `IrqLine` (T9)                                     |
| `[diag tty] getinfo.deny ok n=2`                            | T8's gate: TTY asks for both selectors and gets `EPERM` twice   |

The `capacity=` value equals the image's sector count, and `features=` is exactly `VERSION_1`'s bit:
with nothing optional requested, any other bit means the negotiation accepted something it was not
asked for.

**Forbidden** (`tests/qemu-boot.forbidden`): `[virtio] refuse`, `[diag vblk]` followed by `FAIL`.
`[diag vblk] no device` is **not** forbidden here; it becomes meaningful when 6.3 has two boots.

**Stub-free boot** (`--no-default-features`): the same vblk markers; this is the configuration that
would have hidden T2's collision, so the default boot is the one that proves T2.

**Two refusal boots, run by hand and recorded** — not CI, in the manner of a mutation:

- the same command line without `force-legacy=false` → the transport's version refusal line, naming
  the flag; `virtio ok` absent;
- a second virtio device pinned into the disk's page → `[virtio] refuse` for both; `[diag vblk] no
  device`; no `[devmap] vblk`.

**Mutations**, each observed and reverted under the scratchpad-snapshot discipline, in the stub-free
configuration where the marker allows it. Predictions are predictions; what is recorded is what was
seen.

| Mutation                                                   | Predicted to move                                        |
| ---------------------------------------------------------- | -------------------------------------------------------- |
| drop the `Priv::irqs` push in the virtio arm               | step 7's claim answers `EPERM`; `virtio ok` absent       |
| push `INTID + 1` as well                                   | step 7's refusal probe; a `FAIL` line                    |
| map the page after the assigned one                        | step 3's magic check; a `FAIL` line                      |
| map the DMA run one frame off                              | step 2's cookie; a `FAIL` line                           |
| report a `pa` one frame off in `GET_DMA`                   | step 2's cookie                                          |
| drop rule 1 of the probe (shared page)                     | the second refusal boot maps the page; `refuse` vanishes |
| omit `VERSION_1` from the driver's feature write           | the status read-back; observed, not assumed              |
| widen T8's gate to any caller                              | `getinfo.deny` becomes a `FAIL` line                     |
| leave the stub priv ids at `16..=19` (T2), asserts removed | observed — which markers a clobbered driver priv moves   |

Host tests cover the probe's assignment rules, the negotiation decision, the queue layout and the
ring discipline; before recording a mutation as uncovered by the boot, run `cargo test` under it.

**The usual gates** from [`ci.md`](../../conventions/ci.md) — fmt, clippy, host tests, the
`c-headers` gate, `dprint`, the link checker, `cargo deny` — and both boots, before anything is
called done.

---

## 7. Documentation owed in the same PR

- **`book/`** — the drivers chapter (the transport, the queue, the `IrqLine` pattern, how a driver
  finds its device), the memory chapter (the DMA window, `reserve_contiguous`), the boot chapter
  (the probe step, the new roster and numbering), and the `SYS_GETINFO` reference. The whole-branch
  review is run against `book/`.
- **`docs/conventions/`** — `kernel.md` (the virtio arm beside "Device memory" and "The ramdisk
  window"; the probe; the DMA debt), `servers-and-drivers.md` (the `driver-rt` dependency bullet
  replaced; the virtio driver loop; priv-slot numbering in "Privilege wiring", which currently says
  stubs sit at 16+), `abi.md` (the selectors), `build-and-boot.md` (the QEMU command line).
- **`docs/plan.md` and `docs/plans/phase-6-virtio.md`** — the 6.2 box checked, a link to this
  document and to the plan, the measurements, and the items of §8 that amend phase-level text.
- **`CLAUDE.md`** — the Drivers line.
- **The sweep** from [`docs-and-workflow.md`](../../conventions/docs-and-workflow.md): every
  `11..=14`, `15`, "last slot", "placeholder" and "until Phase 6" claim in the touched crates and in
  `book/`, checked against what the branch added.

---

## 8. Rulings to check

Decisions made here that the phase file did not make, with what each costs if it is wrong.

| # | Ruling                                                                                     | If wrong                                                                                  |
| - | ------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------- |
| 1 | **T2:** stub priv ids move above `USER_PRIV_ID`; H1 missed the collision                   | none to undo — the alternative is the same renumbering done the other way round           |
| 2 | **T4:** a shared page is refused for *any* second device, not only another driver's        | a legitimately co-located driverless device blocks its neighbour; loosen one predicate    |
| 3 | **T4:** the kernel prints `Version` and leaves the refusal to the transport                | a legacy device gets a page mapped before it is refused; harmless, and one check to move  |
| 4 | **T7:** one uniform 64 KiB DMA run; reserved only for a driver with a device and an ELF    | a driver needing more forces a per-driver size table in the kernel                        |
| 5 | **T7:** the DMA cookie                                                                     | eight bytes and one compare; delete it once 6.3's first read proves the same thing better |
| 6 | **T8:** two selectors, values 65 and 66; `ENOENT` for "no device"                          | not emitted to C, so renumbering stays free for as long as that holds                     |
| 7 | **T9:** the battery's free-slot row becomes `NR_IRQ_HOOKS`                                 | fails loudly (`EPERM` for `EINVAL`) if a boot ever has 16 claimers                        |
| 8 | **T11:** queue size 8                                                                      | a driver constant; 6.9 may want deeper receive queues and can pass its own                |
| 9 | **T12:** virtio-blk enables its queue and sets `DRIVER_OK` in 6.2, beyond the stated proof | a device left `DRIVER_OK` with no server behind it; nothing submits, so nothing completes |

**One finding 6.2 does not resolve, for the phase file.** The 32 transport slots span exactly four
pages, and H2 gives every device its own page. Blk, console, input and net fill all four; 6.10's
`virtio-gpu` has no page left. The phase file's "four virtio devices (five with 6.10)" does not add
up under its own H2. It is a stretch slice and not this one's problem, but the tracker should say so
rather than let 6.10 find out: its options are a page shared under a narrower mapping granule (there
is none below 4 KiB), dropping a device from that boot, or the PCI transport.

---

## 9. Out of scope

- **Any request to the device, and completion interrupts.** 6.3. `deliver_irq`'s immediate path
  stays unexercised (6.1's debt) until then.
- **DS publication, `BDEV_*`, root selection.** 6.3.
- **The other three drivers' crates.** Their slots, privileges and `DeviceID` rows exist; their ELFs
  do not.
- **`VIRTIO_BLK_F_FLUSH`, GPT.** 6.4.
- **Clearing `Priv::irqs` and protecting the DMA run on driver exit.** Recorded debts; no driver
  exits.
- **x86_64 and the PCI transport.** Phase 8; T10's trait is the only provision.

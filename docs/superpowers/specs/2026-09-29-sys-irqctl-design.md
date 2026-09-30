# Pre-Phase-6 chunk 3 — `SYS_IRQCTL`: interrupt delivery to user-space drivers

**Date:** 2026-09-29 **Status:** design, pending review **Branch:** `feature/irqctl-design`
**Predecessor:** pre-Phase-6 chunk 2 (the user VA map, PR #60) **Tracker:**
[`docs/plans/phase-6-prep.md`](../../plans/phase-6-prep.md) chunk 3

Phase 6's first kernel work is delivering a hardware interrupt to an EL0 driver as a `NOTIFY` from
`HARDWARE`. This document is the design note chunk 3 asks for, written before any virtio code so
that virtio-blk is never tempted into a poll loop. **It designs; it implements nothing.** Slice 6.1
implements it, from its own plan document.

Decisions are labelled `I1…I13`: slice-local, distinct from the phase-level `D1…D13`.

---

## 1. What exists, and what 6.1 changes

| Component                            | Today                                                           | After 6.1                                                                    |
| ------------------------------------ | --------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `kernel/src/system/stubs.rs`         | `enosys_stub!(do_irqctl)`                                       | stub removed; `system/do_irqctl.rs` holds the handler                        |
| `kernel/src/system/mod.rs`           | `SYS_IRQCTL` in `dispatch_caller_local`                         | moves to the table-taking `match` — it needs `&mut Priv` (I5)                |
| `kernel/src/arch/aarch64/gic.rs`     | PPIs only; no mask/unmask after boot                            | `+ enable_spi`, `mask_spi`, `unmask_spi`, `nr_intids` (I9)                   |
| `kernel/src/arch/aarch64/irq.rs`     | one arm, PPI 27; anything else logs `unexpected INTID`          | `+` a hooked-SPI arm: mask, record, notify (I7)                              |
| `kernel/src/ipc/notify.rs`           | two kernel-originated notifies: `deliver_alarm`, `deliver_ksig` | `+ deliver_irq`, source `HARDWARE`; `build_notify_message` carries a bitmap  |
| `kernel/src/proc/priv_struct.rs`     | `irqs: ArrayVec<u32, NR_IRQ>`, never read                       | `irqs` is the allow-list (I4); `+ int_pending: u32` (I6)                     |
| `kernel/src/proc/sched.rs`           | empty run queue ⇒ "the previous current stays current"          | a real idle path (I11)                                                       |
| `kernel/src/system/do_exit.rs`, exec | no IRQ state to clear                                           | both release the target's hooks and mask their lines (I8)                    |
| `kernel-shared/src/callnr.rs`        | `SYS_IRQCTL = KERNEL_CALL + 7`, no payload constants            | `+ IRQ_*` subcodes, `IRQCTL_*_OFF`, `NOTIFY_IRQ_PENDING_OFF`                 |
| `drivers/tty`                        | TX-only, polled                                                 | claims the PL011 line for the round-trip proof (I12); RX itself is slice 6.5 |
| `tools/gen-c-headers`                | emits `SYS_IRQCTL` only                                         | `+` the subcode and offset rows                                              |

---

## 2. The call

### I1 — MINIX's four subcodes, MINIX's values

| Subcode         | Value | Meaning                                                                  |
| --------------- | ----- | ------------------------------------------------------------------------ |
| `IRQ_SETPOLICY` | 1     | claim a line: allocate a hook, configure the line, leave it **unmasked** |
| `IRQ_RMPOLICY`  | 2     | release a hook: mask the line, free the slot                             |
| `IRQ_ENABLE`    | 3     | unmask the hook's line                                                   |
| `IRQ_DISABLE`   | 4     | mask the hook's line                                                     |

These are `include/minix/com.h`'s values, unchanged. Register / unregister / enable / disable is the
whole surface; there is no fifth subcode for "acknowledge", because re-enabling *is* the
acknowledgement (I7).

### I2 — payload

Request, all `i32`, in MINIX's field order:

| Offset   | Field     | Used by                                                                  |
| -------- | --------- | ------------------------------------------------------------------------ |
| `0..4`   | `request` | all                                                                      |
| `4..8`   | `vector`  | `IRQ_SETPOLICY`                                                          |
| `8..12`  | `policy`  | `IRQ_SETPOLICY`                                                          |
| `12..16` | `hook_id` | `IRQ_SETPOLICY`: the caller's `notify_id`. The other three: the hook id. |

Reply: `m_type` is the result; on `IRQ_SETPOLICY` success `12..16` carries the kernel's hook id,
**1-based** as in MINIX, so 0 is never a valid hook — the `SAFECOPY_*`/`EXEC_SRC_OFF` "0 is invalid"
convention.

`hook_id` is overloaded exactly as MINIX overloads it: on the way in to `IRQ_SETPOLICY` it is the
driver's own small integer (`notify_id`, `0..=31`), the bit the driver will see set in the
notification; on the way out it is the kernel's handle. Two names in `callnr.rs`
(`IRQCTL_NOTIFY_ID_OFF` and `IRQCTL_HOOK_ID_OFF`, both 12) keep the call sites honest about which
one they mean.

**D8.** The call number has been allocated since Phase 2 and no C consumer reads these bytes, so the
new constants are additive rather than an ABI bump. 6.1 regenerates the C headers in the same PR.
This is a ruling, and it costs a follow-up fork bump if it is wrong.

### I3 — `vector` is the raw GIC INTID

The driver passes the number `do_irq` will read out of `ICC_IAR1_EL1`: SPI *n* is INTID `32 + n`.
The alternative — an SPI-relative number, as a device tree's `interrupts` cell gives it — would put
a `+ 32` at a boundary where forgetting it still yields a plausible-looking line number.

Only SPIs are claimable: `32 <= vector < gic::nr_intids()`. SGIs and PPIs answer `EINVAL` — PPI 27
is the kernel's timer and nothing at EL0 has business with it.

### I4 — privilege: two gates, and the allow-list has no opt-out

1. **`k_call_mask`** — the existing dispatch gate. Every boot server currently carries the full mask
   (`fill_bits(&mut pr.k_call_mask, NR_KERN_CALLS)`), so this gate is coarse today; narrowing it is
   not this slice's job, which is why it cannot be the only gate.
2. **`Priv::irqs`** — the per-line allow-list. `IRQ_SETPOLICY` on a vector not in the caller's list
   answers `EPERM`. It is **always checked**: an empty list denies every line. MINIX gates the check
   behind a `CHECK_IRQ` flag that a privileged server can simply lack; minix.rs has no such flag, so
   there is no "may claim any IRQ" state to fall into by omission.

The list is filled at boot, in `load_boot_server`'s per-driver arms beside the device pre-map — the
same place and the same reasoning as TTY's UART page: the kernel decides which hardware a boot
driver owns, the driver cannot widen it. Drivers started after boot get their list from RS through
`SYS_PRIVCTL`; that path is out of scope until a driver is started that way.

`NR_IRQ` is 8 per process. A virtio driver needs one.

### I5 — the shared user slot is refused

A caller whose privilege slot is shared (`proc_nr != Some(caller)`) answers `EPERM`, as
`SYS_SETGRANT` does: `int_pending` lives in the privilege slot (I6) and one bitmap cannot describe
several processes' interrupts. This is also why the handler needs `&mut Priv` and leaves
`dispatch_caller_local`.

### Result codes

| Condition                                                         | Result   |
| ----------------------------------------------------------------- | -------- |
| unknown `request`; `vector` not an SPI in range; `notify_id > 31` | `EINVAL` |
| `hook_id` out of range or naming a free hook                      | `EINVAL` |
| `policy` has `IRQ_REENABLE` set (I10)                             | `EINVAL` |
| vector not in `Priv::irqs`; shared slot; hook owned by another    | `EPERM`  |
| line already claimed by another process (I6)                      | `EBUSY`  |
| hook table full                                                   | `ENOSPC` |

---

## 3. The kernel side

### I6 — the hook table: one owner per line

```rust
struct IrqHook {
    owner: Endpoint,   // NONE = free
    intid: u32,
    notify_id: u8,     // bit index in the owner's Priv::int_pending
}
static IRQ_HOOKS: IrqHooks = …;   // [IrqHook; NR_IRQ_HOOKS], NR_IRQ_HOOKS = 16
```

The usual `UnsafeCell` newtype, per [`kernel.md`](../../conventions/kernel.md). `do_irq` finds the
hook by a linear scan over sixteen entries; a per-INTID index is not worth a second table.

MINIX chains several hooks on one line for shared PC interrupts. QEMU `virt` gives every device its
own SPI, so a second `IRQ_SETPOLICY` on a line owned by a different process answers `EBUSY` and
there is no chain to keep consistent. A repeat `IRQ_SETPOLICY` from the **same** owner with the same
`notify_id` replaces its hook, as in MINIX — that is what lets a restarted driver re-register
without first knowing its old hook id.

`Priv` gains `int_pending: u32`, MINIX's `s_int_pending`: bit `notify_id` is set when that hook's
line fires.

### I7 — delivery, and masking while a NOTIFY is in flight

`do_irq`, for an INTID that has a hook:

1. `gic::mask_spi(intid)` — and wait for `GICD_CTLR.RWP` to clear.
2. set bit `notify_id` in the owner's `int_pending`;
3. `ipc::notify::deliver_irq(owner)`;
4. `gic::eoi(intid)`, as today.

**Mask before EOI** is the storm avoidance. Every `virt` device line is level-triggered: it stays
asserted until the driver quiesces the device, which cannot happen until the driver runs, which
cannot happen until the kernel returns to EL0. EOI on an unmasked asserted line re-enters `do_irq`
immediately and forever. With the line masked the kernel takes exactly one interrupt per
`IRQ_ENABLE`.

The driver's half: service the device, acknowledge **at the device**, then `IRQ_ENABLE`. If the
device raised a new event in between, the line is still asserted and fires the moment it is unmasked
— a level-triggered line cannot lose an interrupt across this window, so no counter or replay logic
is needed.

`deliver_irq` is the `deliver_alarm` / `deliver_ksig` shape: source `HARDWARE`, **no `ipc_to`
check** (`HARDWARE`'s bitmap is empty, so `mini_notify` would deny it), immediate if the owner is
`RECEIVE`-blocked and would accept from `HARDWARE`, else deferred through `notify_pending` against
`HARDWARE`'s privilege slot.

The notification carries the bitmap. `build_notify_message`, when the source is `HARDWARE`, writes
the destination's `int_pending` into payload `0..4` (`NOTIFY_IRQ_PENDING_OFF`) **and zeroes it** —
at build time, which for a deferred notification is the owner's later `RECEIVE`, so bits from
several lines that fired while the driver was busy arrive coalesced in one message. Every other
notification keeps its all-zero payload.

An INTID with no hook keeps today's `do_irq: unexpected INTID` line and is additionally **masked**
if it is an SPI — a line nobody owns must not be able to storm either.

### I8 — a dead or exec'd owner releases its lines

`do_exit` and `do_exec` both walk `IRQ_HOOKS` for the target's endpoint, `mask_spi` each line, free
the hook, and zero `int_pending`. Exit, because a hook naming a stale endpoint would notify the
slot's next occupant — MINIX panics in `generic_handler` on exactly this. Exec, because the
privilege slot survives but the new image holds none of the old image's hook ids; this is the
`SYS_SETGRANT` precedent for state that describes an image being discarded.

### I9 — GIC: what `gic.rs` gains

- `enable_spi(intid, priority)` — `GICD_IGROUPR` (group 1 NS), `GICD_IPRIORITYR`, `GICD_ICFGR`
  (level), `GICD_IROUTER<n>` (affinity 0.0.0.0 — CPU 0, required because `ARE_NS` is on), then
  `GICD_ISENABLER`. Called by `IRQ_SETPOLICY`, which therefore leaves the line live.
- `mask_spi(intid)` / `unmask_spi(intid)` — `GICD_ICENABLER` / `GICD_ISENABLER`.
- `nr_intids()` — from `GICD_TYPER.ITLinesNumber`, read once in `init`. The range check in I3 uses
  it rather than a constant, so a claim on a line the distributor does not implement is `EINVAL`,
  not a write to a reserved register.

The lines on QEMU `-M virt,gic-version=3`:

| Device                         | PA                        | SPI      | INTID    |
| ------------------------------ | ------------------------- | -------- | -------- |
| PL011 UART0                    | `0x0900_0000`             | 1        | 33       |
| virtio-mmio transport *n* (32) | `0x0A00_0000 + n * 0x200` | `16 + n` | `48 + n` |

Which transport slot a `-device virtio-*-device` lands in is QEMU's choice, not the command line's
order. A driver finds its device by probing `MagicValue` / `DeviceID`, never by slot number; that is
slice 6.2's problem and is recorded here only so 6.1's allow-list is not written against a guessed
slot.

### I10 — `IRQ_REENABLE` is defined and refused

MINIX's `policy` bit `IRQ_REENABLE` asks the kernel to unmask the line itself straight after
notifying. On a level-triggered line that is the storm I7 exists to prevent, by construction.
`IRQ_SETPOLICY` with the bit set answers `EINVAL`. The constant is published so the refusal is
explicit — the "define and refuse" rule in
[`servers-and-drivers.md`](../../conventions/servers-and-drivers.md#define-and-refuse-never-fold-into-enosys)
— and so an edge-triggered line, if one ever appears, has a bit waiting for it.

### I11 — the kernel needs an idle path

Interrupt-driven I/O makes "no process is runnable" the ordinary state: every server blocked in
`RECEIVE`, the disk driver waiting on a completion. Two things in today's kernel do not survive it.

- `sched::schedule_next` returns without switching when `pick_proc` is `None` — its own comment says
  "the previous current stays current" — and the tail then `eret`s into that process, which is
  blocked. The default-on demo stubs spin forever, so a default boot always has something runnable
  and this has not been a live path.
- IRQs are taken only from EL0: vector slot 9 is wired, `el1h_irq` is still the fatal `EXC_ENTRY`.

The design, chosen to need **no** EL1 IRQ vector and no idle process: when `pick_proc` is `None`,
`schedule_next` records `IDLE` as current and loops — `wfi`, then the body of `do_irq` called
directly, then `pick_proc` again. `wfi` wakes on a pending interrupt even with `PSTATE.I` set, and
`do_irq` already ignores its frame argument, so the interrupt is serviced by polling the GIC on the
kernel stack with IRQs still masked. No nested exception frame exists at any point. The clock tick
reached this way must not bill or rotate a process: `sched::reschedule` returns early when current
is `IDLE`.

The alternatives: an EL0 idle process needs an image, an address space and `SCTLR_EL1.nTWI`; a wired
`el1h_irq` vector needs a second save/restore path and a decision about interrupting the kernel,
which the single-threaded-EL1 invariant every `// SAFETY:` comment cites currently rules out.

6.1 measures first: boot `--no-default-features` and establish whether the empty-queue state is
reached today, before changing what it does.

---

## 4. The driver side

### I12 — one pattern for TTY RX, virtio-console and virtio-blk

```text
init:    hook = irqctl(IRQ_SETPOLICY, vector, policy = 0, notify_id)
loop:    m = receive(ANY)
         if m.m_source == HARDWARE && m.m_type == NOTIFY_MESSAGE:
             for each set bit in payload[0..4]:
                 drain the device          (RX FIFO until empty / walk the used ring)
                 acknowledge at the device (PL011 UARTICR / virtio InterruptACK)
                 irqctl(IRQ_ENABLE, hook)
         else: ordinary request
```

The order inside the loop is load-bearing: acknowledging at the device **before** `IRQ_ENABLE` is
what makes the unmask safe, and draining before acknowledging is what makes the acknowledgement
true. The pattern lives once, as an `IrqLine` type in `drivers/driver-rt` (claim in `new`, `rearm()`
for the `IRQ_ENABLE`), so TTY and the virtio drivers cannot each get the order subtly different.

A driver identifies the notification by the **kernel-stamped `m_source`**, never by payload —
`HARDWARE` cannot be forged by a process, a payload bit can.

**TTY RX** then needs no VFS and no ABI change. 5.11 already defined `CDEV_READ` and TTY refuses it
from its unknown-request arm; RX is one new arm in TTY plus the loop above feeding a small input
buffer. **virtio-console** is the same loop with a used ring in place of a FIFO. What TTY does with
a `CDEV_READ` that arrives when no input is buffered — VFS is blocked in `SENDREC` for as long as
TTY withholds the reply — is a VFS-concurrency question that belongs to slice 6.5, not here.

**No polling.** A blk driver that spins on the used ring passes under TCG, where the device
completes inside the `QueueNotify` write, and proves nothing about a disk root under real completion
interrupts. 6.3's driver blocks in `receive` between requests, full stop.

---

## 5. Verification — what 6.1 must show

### I13 — a real interrupt, from a device the tree already drives

No virtio and no synthetic pend (`GICD_ISPENDR` on a level-configured line latches until explicitly
cleared, which would exercise a path no real device takes). TTY owns a real level-triggered source
already: it claims INTID 33, sets `UARTIMSC.TXIM`, and the PL011 raises its TX interrupt. TTY
receives the `HARDWARE` notification, clears `TXIM` — the device-side acknowledgement — calls
`IRQ_ENABLE`, and prints one line.

| Kind      | Marker                     | Proves                                            |
| --------- | -------------------------- | ------------------------------------------------- |
| expected  | `[diag tty] irq ok`        | register → fire → mask → NOTIFY → ack → re-enable |
| forbidden | `do_irq: unexpected INTID` | the SPI arm, not the fallback, handled it         |

The exact marker text, any counts in it, and the denial battery (`EPERM` for a line outside the
allow-list, `EBUSY`, `EINVAL` for a PPI and for `IRQ_REENABLE`) are 6.1's plan to fix — counts are
recomputed from constants, never copied from this document
([`testing-and-markers.md`](../../conventions/testing-and-markers.md#markers)).

Mutations 6.1 owes, each observed and reverted:

- drop the `mask_spi` in `do_irq` → the boot storms and the marker never prints;
- drop the `IRQ_ENABLE` in TTY → a second forced interrupt never arrives;
- drop the allow-list check → the denial probe's `EPERM` line changes.

Host-testable logic — the subcode decode, the range and allow-list predicates, the hook-table
find/replace/free — goes in `kernel-shared`, under the standing carve-out for pure predicates the
kernel crate cannot test.

---

## 6. Out of scope

- **Mapping virtio-mmio pages into a driver.** Slice 6.2; [`kernel.md`](../../conventions/kernel.md)
  already records that D1's rejection of `VMCTL_MAP_PHYS` is to be revisited there.
- **`CDEV_READ` blocking VFS.** Slice 6.5.
- **Narrowing the boot servers' `k_call_mask`.** A separate hardening change; I4's allow-list is
  what makes it non-urgent.
- **Shared lines, edge-triggered lines, SMP routing, x86_64.** No consumer.
- **Dynamic drivers' allow-lists via RS / `SYS_PRIVCTL`.** When RS first starts a driver.

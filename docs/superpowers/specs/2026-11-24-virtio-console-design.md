# Slice 6.6 — `virtio-console` as a second CDEV backend

**Status:** approved 2026-11-24. **Phase file:**
[`phase-6-virtio.md`](../../plans/phase-6-virtio.md#slice-66-virtio-console-as-a-second-cdev-backend).
Decisions here are labelled `C1…C14`; the phase's `H1…H12`, 6.2's `T1…T14` and 6.5's `R1…R12` stay
in force and are cited by their own labels.

**Goal:** a second character device, served by a virtio driver. A write to `/dev/hvc0` leaves the
machine on a second QEMU chardev, and bytes fed to that chardev come back from a `read()` of the
node — while a reader parked on the *other* console stays parked and is not in the way.

The phase file locks this much: the driver is on slot 12, one port, no multiport feature; VFS gains
a `/dev/hvc0` node; fd 0, 1 and 2 stay on TTY. 6.5's R12 adds that the driver implements the driver
side of R1 and that VFS's notifier check widens from one endpoint to the set of character drivers.
Everything else below is this slice's own design.

---

## 1. What exists, and what 6.6 changes

Verified against `main` at `ddd91b7`.

| Where                                   | Today                                                                                          | After 6.6                                                                                           |
| --------------------------------------- | ---------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `drivers/virtio-console/`               | a five-line host `fn main() {}` stub with a `Cargo.toml`; not packed, slot 12 has no ELF       | the driver: `main.rs`, pure `bringup.rs` / `port.rs`, `build.rs`, `user.ld`, branded (C3–C9)        |
| `kernel/build.rs`                       | 13 archive rows; `vblk` sits between `memory` and `mfs`                                        | `+` a `minixrs-virtio-console` row for proc 12, after `vblk` and before `mfs` (C4)                  |
| `drivers/driver-rt`                     | transport, queue, DMA, IRQ line, GPT                                                           | `+` `cdev.rs`: the pure half of R1 (`Waiter`, `read_plan`, the validator), moved out of TTY (C6)    |
| `drivers/tty`                           | owns `Waiter` / `read_plan`; answers `EIO` only for a refused claim                            | imports them from `driver-rt`; a stopped line answers `EIO` and wakes its waiter once (C9)          |
| `servers/vfs/src/dev.rs`, `fd.rs`       | three device nodes, `CharDriver::{Tty, Memory}`                                                | `+` `/dev/hvc0` → `CharDriver::Vcon`, minor 0 (C10)                                                 |
| `servers/vfs/src/main.rs`               | a notification is acted on only if `m_source == tty`; the retry walk stops at the first EAGAIN | the notifier is one of the character drivers, and the walk is **per driver** (C10)                  |
| `servers/vfs/src/park.rs`               | `oldest()` is the only way to walk                                                             | `+` an in-order walk that can pass over an entry (C10) — R12 predicted this file would not change   |
| `kernel-shared` `callnr.rs`             | `DEV_CONSOLE_PATH`, `DEV_NULL_PATH`, `DEV_ZERO_PATH`                                           | `+` `DEV_HVC0_PATH`, `CDEV_MINOR_HVC0`; not emitted into the C headers (C13)                        |
| `userland/init`                         | one proof child, blocked in `read(0)`                                                          | `+` a second proof child that writes, then reads, `/dev/hvc0` (C11)                                 |
| `tools/qemu-run.sh`                     | attaches the boot disk on bus 0 and nothing else                                               | `+` a `virtio-serial-device` on bus 8 with one `virtconsole` port on a host chardev, always (C1–C2) |
| `tools/feed-console.sh`, `ci.yml`       | feeds the serial console once `cons.beside ok` is in the log                                   | feeds `hvc0` first, then the serial console once `hvc0 ok` is in the log too (C12)                  |
| kernel, `Message`, call numbers, errnos | —                                                                                              | unchanged (C13)                                                                                     |

Facts the design rests on:

- **Slot 12 is already wired.** `VCON_PROC_NR`, its `BootEntry` row, its priv slot, its `ipc_to`
  bits, its `VIRTIO_DRIVERS` row (`VIRTIO_ID_CONSOLE = 3`, name `vcon`) and its DMA run all landed
  in 6.2 (H1). The kernel's boot probe already assigns a console device to it; nothing in
  `kernel/src` changes.
- **`bus=virtio-mmio-bus.8` is transport slot 8, in its own page** (measured, 6.2).
- **`DmaRegion::split_queue` can be called twice.** The remainder of a split is itself a region that
  has lent no buffer, and `dma.rs` already tests that a second split gives a second disjoint queue.
  Two queues need no `driver-rt` change.
- **`VirtQueue` handles more than one queue per driver and more than one chain in flight** — its
  bookkeeping is per instance — and its size cap of 8 is ample here (6.2's debt 3 is not touched).
- **The DMA run is 16 pages** (`DMA_RUN_PAGES`); this driver uses four.
- **Every boot server is `SRV_T` with `ipc_to` over every active slot**, so `vcon` may already
  `NOTIFY` VFS and reply to it. No privilege wiring changes.
- **The kernel never delivers a notification as the reply to a `SENDREC`** (6.5's R9). `vcon` sends
  its wake-up while VFS may be inside a `SENDREC` to it; this is the shape that rule exists for.
- **VFS's retry walk today cannot serve two drivers.** `retry_parked` takes the oldest parked read,
  asks its driver, and stops at the first `EAGAIN`. With a reader parked on TTY and a newer one on
  `vcon`, a notification from `vcon` asks TTY, hears `EAGAIN`, and returns — the `vcon` reader is
  never retried. Widening the notifier check alone, as R12 describes it, produces exactly that hang.

Not verified, because only running the slice can — each is a measure-first step (C14):

1. What edk2 and Limine do with a virtio console: whether they bind it, write boot text to it, read
   from it, and what `Status` it holds when the kernel starts.
2. How QEMU's `pipe` chardev behaves on macOS and on the runner: whether opening the FIFOs blocks
   with no peer, and what a guest write does when nobody reads the host side.
3. How QEMU's `virtconsole` behaves without `VIRTIO_CONSOLE_F_MULTIPORT`: that port 0 is queues 0
   (receive) and 1 (transmit); that a transmit completes with an interrupt whether or not the host
   side is read; that bytes fed with no receive buffer posted are held, not dropped; and how a host
   write is cut across guest buffers.

---

## 2. Decisions

### C1 — the device: one `virtconsole` port on bus 8, attached to every boot

`qemu-run.sh` gains, unconditionally:

```
-chardev <C2>,id=hvc0
-device virtio-serial-device,bus=virtio-mmio-bus.8
-device virtconsole,chardev=hvc0
```

`BUS_VCON=8` joins `BUS_VBLK=0`, per H2's one-device-per-page rule. `virtconsole`, not
`virtserialport`: QEMU reserves port 0 for a console, and port 0 is the only port a driver without
the multiport feature can see. The device is attached on every boot — disk, ramdisk, reboot, and a
hand-run one — so `[virtio] slot=8 … drv=vcon` is a common required marker and there is no
"console-less" configuration to keep green.

*Rejected:* attaching it only on fed boots (two machine shapes, and the reboot would prove a
different driver path from the boot before it).

### C2 — the host side: a `pipe` chardev over two FIFOs

`-chardev pipe,id=hvc0,path=$ROOT/target/hvc0`, with `qemu-run.sh` creating `target/hvc0.in` and
`target/hvc0.out` (`mkfifo`) when they are missing. The host writes guest input to `.in` and reads
guest output from `.out`. `MINIXRS_HVC0=<base>` overrides the path, so two boots run at once do not
share a pair.

A FIFO gives the feeder a **blocking read** of the guest's output: no second log to poll and no
stale-file race of the kind `feed-console.sh` guards against with two looks. An unfed boot must not
depend on a peer: nobody opens either FIFO, the guest's few bytes of output sit in the pipe, and its
reader parks for good — the same picture as an unfed `read(0)`.

This is the working choice, and C14 measures it before anything is built on it. It is acceptable
only if all four hold on macOS and on the runner: QEMU starts with no peer on either FIFO; a guest
write with nobody reading `.out` completes; bytes written to `.in` after the kernel is up reach the
guest; and what the firmware writes to the port (unverified fact 1) does not fill the pipe. If any
fails, the fallback is `-chardev socket,server=on,wait=off` on a Unix socket under `target/`, with
the feeder as its one client — same proof, one more tool on the host (`nc -U` or `python3`).

*Rejected:* `-chardev file` (output only — half the proof); `-chardev stdio` (taken by the serial
console); a `mux` of both consoles on stdio (the two input streams could not be fed apart, and the
serial log would carry the second console's bytes).

### C3 — features: none, and device configuration is never read

`negotiate(&mut t, 0)`: the accepted word is `VIRTIO_F_VERSION_1` alone. No
`VIRTIO_CONSOLE_F_MULTIPORT` (locked by the phase), no `F_SIZE` (nothing consumes a window size
without a line discipline), no `F_EMERG_WRITE`. Every field of the console's configuration space is
valid only under one of those three, so the driver reads none of it.

**6.2's debt 2 (sub-word config access) is therefore not paid here.** It named "6.6 or 6.8, the
first driver with 8- or 16-bit config fields"; this driver has no config field it may read. The debt
moves to 6.8, whose `virtio-input` selects with two `u8` fields.

*Rejected:* requesting `F_SIZE` to exercise typed config reads (a feature with no consumer, taken
only to pay a debt somewhere else).

### C4 — `vcon` always publishes; without a working device it answers `ENXIO`

`vcon_init` publishes the name `vcon` to DS unconditionally, as TTY does. Bring-up runs after
`sef_startup`, in straight-line code, and prints one verdict line. If the kernel assigned no device
(`ENOENT`, printed as `no device`) or a bring-up step failed (`virtio FAIL step=…`), the driver
stays in its receive loop and answers **`ENXIO`** to every CDEV request, well-formed or not — the
device does not exist, which is `virtio-blk`'s rootless answer applied to a character device.

`vblk` publishes only when it has a device because MFS reads a DS miss as "no disk" (6.3's B1).
Nothing selects between consoles, so a miss here would carry no meaning, and VFS's lookup can then
follow the ordinary rule: `vcon` in DS, falling back to `boot_endpoint(VCON_PROC_NR)` with a
distinguishable line (`vcon.ds ok ep=…` / `vcon.ds FAIL rc=… fallback=…`). The process is always
there to answer, so the fallback is always sound.

Bring-up never waits on the device (no completion is needed before the first request), so the row's
place in `kernel/build.rs` is constrained only by `vcon < vfs`. It goes directly after `vblk`.

`open("/dev/hvc0")` succeeds whatever the driver's state — a device open consults only VFS's table —
and the first `read` or `write` hears the `ENXIO`.

### C5 — two queues on two pages, one buffer each way

| DMA page | Holds                                                 |
| -------- | ----------------------------------------------------- |
| 0        | `receiveq` (queue 0)                                  |
| 1        | `transmitq` (queue 1)                                 |
| 2        | the receive buffer, `CDEV_MAX_IO` bytes, device-write |
| 3        | the transmit buffer, `CDEV_MAX_IO` bytes, device-read |

The two queue pages come off the front of the run with two `split_queue` calls, before any buffer
handle exists, so no `DmaBuf` can reach a ring. The layout is a pure function in `bringup.rs`,
host-tested like `vblk_layout`. The driver adds one `unsafe` block for the region and one for the
register window, as `virtio-blk` does, and no other.

**One receive buffer, not a pool.** It is posted at bring-up and is at any moment in exactly one of
two states: *posted* (the device may fill it) or *held* (the device returned it with `len` bytes,
`off` of which have been read). That is H3's "one request in flight" applied to receive, and it
makes the state a two-variant enum instead of a queue of buffers. Throughput is not a goal: a burst
longer than one buffer waits in the host's chardev until the buffer is posted again.

### C6 — receive: serve reads straight from the held buffer; nothing is ever dropped

`port.rs` is the pure state machine (`Rx::{Posted, Held { len, off }}`), host-tested with no device.

- **On a used element in `receiveq`:** clamp the device-supplied length to the buffer's, and become
  `Held { len, off: 0 }`. A zero-length element is re-posted at once and holds nothing.
- **`CDEV_READ`:** validate (minor 0, by `CDEV_WRITE`'s rules in `CDEV_WRITE`'s order); a
  zero-length read answers `0` before the buffer is looked at; a broken device answers `EIO` (C9);
  `Posted` answers `EAGAIN` and records the waiter first; `Held` copies `min(want, len - off)` bytes
  **directly from the DMA buffer's virtual address** to the caller's grant with `SYS_SAFECOPY` — one
  copy, no staging, the way `virtio-blk` serves a read from its bounce page.
- **Advance after the copy.** `off` moves only once the copy has returned `OK`; a refused copy
  (`EPERM`, `EFAULT`, relayed verbatim) leaves the bytes owed to whoever asks next. This is R1's
  peek-copy-consume, and C11 gives it a boot proof.
- **Re-post when drained.** When `off == len` the buffer is chained into `receiveq` again and the
  queue is notified, before the reply goes out.
- **The wake-up.** After an interrupt that moved the port to `Held`, and after the line is re-armed,
  the recorded waiter gets one `NOTIFY` and is forgotten; a failed `NOTIFY` keeps the waiter and
  prints `rx FAIL notify rc=…` once. Exactly TTY's lines.

There is **no input ring and no overflow**. TTY has both because a UART cannot make the sender wait;
a virtqueue can, and a held buffer is that wait. So there is no `rx overflow` line for this driver
and no byte it can lose. There is no storm guard either: `InterruptStatus` is cleared by the
acknowledgement, so the "line that keeps firing with nothing to receive" TTY counts has no virtio
form.

**The pure half of R1 moves to `driver-rt`.** `Waiter`, `ReadPlan` / `read_plan` and the shared
validator (parameterised by the driver's one minor) move from `drivers/tty/src/cdev.rs` to a new
`drivers/driver-rt/src/cdev.rs`, with their host tests; TTY imports them. Two drivers now implement
the contract and 6.8 adds a third; three copies of "record the waiter before answering `EAGAIN`"
would drift. `read_plan`'s third argument is renamed from `line_claimed` to what it has become —
whether input can still arrive (C9). The memory driver stays off `driver-rt`: it never says
`EAGAIN`.

*Rejected:* copying received bytes into a ring of TTY's shape (drops what the device would have
held, and adds a copy); a pool of receive buffers (a FIFO of held buffers with no consumer that
needs the throughput).

### C7 — transmit: one chain in flight, waited for

`CDEV_WRITE`: validate; a zero-length write answers `0` and touches nothing; otherwise copy
`min(len, CDEV_MAX_IO)` bytes from the caller's grant straight into the transmit buffer
(`SAFECOPY_FROM` to its virtual address — a refused grant never reaches the device), chain a
`DmaBuf` of exactly that length into `transmitq`, notify, and **wait for that chain's completion**
in a receive filtered to `HARDWARE`, never polling. The reply is the count moved, so a longer write
is a short write and VFS's loop re-sends, as it does for TTY.

The wait is `virtio-blk`'s `submit`, with one difference that matters: **a receive completion can
arrive during it.** The interrupt handler (C8) walks both queues every time, so input that lands
while a write is in flight is taken into the held buffer and its waiter notified from inside the
wait. VFS is then mid-`SENDREC` to `vcon`; the kernel latches the notification for VFS's next plain
`RECEIVE` (R9).

A device that never completes a transmit hangs the driver with VFS behind it. That is the stated
cost of every waited request in this tree (`virtio-blk`, the polled PL011), and unverified fact 3
checks that QEMU completes a console write whether or not anyone reads the host side.

*Rejected:* replying before the completion and reclaiming the buffer on a later interrupt (needs a
buffer pool and a "transmit busy" answer no client understands).

### C8 — one interrupt line, one handler, both queues

Every interrupt, wherever it is received: read `InterruptStatus`; acknowledge exactly those bits;
walk `receiveq` until `Ok(None)`; walk `transmitq` until `Ok(None)`; re-arm; **then** send the
wake-up if input arrived. That is `driver-rt`'s documented order with a second walk, and the
wake-after-re-arm rule of
[`servers-and-drivers.md`](../../conventions/servers-and-drivers.md#replying-relaying-and-register-layouts).
A configuration-change interrupt walks two empty rings and re-arms.

A used element nobody is waiting for is a fault, not something to skip: a `transmitq` completion
with no write in flight, a second `receiveq` completion while the buffer is already held, or any
`QueueFault` ends the device (C9).

### C9 — a device that stops says `EIO`, and wakes whoever is parked on it

When the device is broken — a queue fault, a used element out of turn, a refused re-arm, a failed
receive during the transmit wait — the driver writes `FAILED`, prints one `virtio FAIL step=io …`
line, leaves the line masked, answers **`EIO`** to every later read and write, and **sends its
recorded waiter one `NOTIFY`**. The woken reader's retry hears `EIO` instead of staying parked
behind an interrupt that can no longer come.

That last step is 6.5's debt 1, which named this slice: TTY answers `EIO` only for a refused claim,
and a failed TX proof, `rx FAIL storm` and `rx FAIL enable` leave a reader parked for good. `vcon`
is written without the hole, **and TTY is closed the same way in this slice**: `IrqService` reports
whether receive can still happen, `read_plan` takes that instead of `line_claimed`, and the
transition to `Stopped` wakes the waiter once. About five lines in TTY, as the debt estimated.

No healthy boot reaches any of this, and no mutation QEMU can be made to produce does (each trigger
is already behind a forbidden marker). It is held by host tests on `read_plan` and on the two
drivers' phase logic, and by reading.

### C10 — VFS: a fourth node, a third driver, and a retry walk per driver

- **The node.** `DEV_NODES` gains `(DEV_HVC0_PATH, CharDriver::Vcon, CDEV_MINOR_HVC0)`;
  `NR_DEV_NODES` goes 3 → 4. `CDEV_MINOR_HVC0` is 0 — minors are a per-driver namespace — and has
  its own name so no reader takes it for the console's.
- **The endpoint.** `vcon_endpoint()` resolves `vcon` once in the prologue beside the other three
  (C4). `cdev_endpoint` grows an arm. The three peers travel as one small `Copy` struct rather than
  as a fourth positional `Endpoint`.
- **The notifier.** A `NOTIFY_MESSAGE` is acted on if its kernel-stamped source is a driver that can
  say `EAGAIN` — TTY or `vcon` — and dropped otherwise. `cons.notify ok` stays keyed to TTY: it is
  the kernel rule's proof, and TTY's probe is what arranges it.
- **The walk is per driver, and this is the slice's one real change to VFS.** On driver *D*'s
  notification: take the parked reads oldest first; **pass over** any whose descriptor resolves to
  another driver; ask *D* for each of its own; a count or an error is that reader's reply and the
  entry is freed; **the first `EAGAIN` from *D* ends the walk**. An entry whose descriptor no longer
  resolves to a character device is answered `EBADF` and freed on any walk, as today. "Never spins
  on `EAGAIN`" and "a spurious notification costs one round" both survive, per driver.
- **`park.rs` gains a way to walk in order without removing the head** — `oldest_after(seq)`, or an
  equivalent — because `oldest()` cannot step past an entry that stays. R12 said this file would not
  change. It was written with one notifier in mind and is corrected, in the phase file and in the
  6.5 spec's R12, by rewriting the sentence.
- **The decision is a step function.** What to do with one parked entry given the notifier and the
  entry's driver — skip, ask, or answer `EBADF` — is lifted into a pure function with host tests,
  per the "lift a retry loop's body" rule. Its "skip" arm is otherwise reachable only when two
  drivers have readers parked at once.
- **A new VFS marker, `[diag vfs] cons.other ok`**, printed the first time a walk passes over a read
  parked on another driver and then answers one behind it. Only VFS can know that happened, and
  without the line the boot would pass whenever the two readers happened to park in the other order
  (C11 arranges the order; the marker proves the arrangement held).

*Rejected:* a park table per driver (the table is indexed by proc slot, which is what makes it
unable to fill; splitting it buys the walk nothing a filter does not); storing the driver in the
entry (the descriptor is already re-resolved at every retry, and a second copy of "which driver"
could disagree with it).

### C11 — the userland proof: a second child, on `/dev/hvc0`

init forks a second proof child after the `read(0)` one, under the same `cfg(not(feature =
"idle-probe"))`. The child:

1. opens `/dev/hvc0` read-write;
2. writes one fixed line **through that descriptor** (`HVC0_OUT`) — the host-visible half;
3. issues a `read` on it with a **buffer address that is in the user range and not mapped**. With
   nothing fed the driver answers `EAGAIN` and VFS parks the raw request. When the first bytes
   arrive the retry reaches the driver with bytes held, the copy is refused, and the child hears
   `EFAULT` — with the bytes still owed;
4. reads into a real buffer until it has `HVC0_IN.len()` bytes, and compares.

`minix.rs init: hvc0 ok n=<len>` is printed on fd 1 (the serial console) only if every step did what
it should: the bad read answered `EFAULT`, and the bytes then read are exactly `HVC0_IN` — all of
them, so a driver that consumed before its copy returned comes up short. Any other outcome is one
`hvc0 FAIL step=<step> …` line (forbidden). `n` is asserted against the constant at compile time, as
`stdin ok n=15` is.

Step 3 is 6.5's debt 11 paid for this driver: the consume-after-copy order gets a boot that fails
without it. **TTY's copy of the debt stays** — the same trick works on the `read(0)` child, and is
deliberately not applied here: it would re-shape 6.5's proof in the slice after it settled. The
recipe is recorded with the debt.

The child is forked **second**, so the `read(0)` child parks first and is the older entry when
`vcon`'s notification arrives; `cons.other ok` (C10) fails the boot if that did not happen.

Costs, both known from 6.5: the child holds a fork slot for the whole boot, so the rotation's first
`worker` moves one slot up and the fork-cycle markers in `tests/qemu-boot.expected` are recomputed;
and init reaps one more child that is not the rotation's own, so its wait-again logic is extended
and the boot budget is re-measured against the merge base.

*Rejected:* one child doing both reads in sequence (no new slot and no renumbering, but two readers
are never parked at once, so C10's walk would be proved by host tests alone); the proof inside VFS's
prologue (VFS cannot park on itself, and a write "through the descriptor under test" is the
convention).

### C12 — the feeder feeds `hvc0` first, then the serial console

`tools/feed-console.sh` stays the one script a fed boot runs as its stdin, and its job gains a first
stage:

1. read `target/hvc0.out` until `HVC0_OUT` appears — the guest's own write, arriving on the second
   chardev;
2. write `HVC0_IN` to `target/hvc0.in` in two chunks, a gap apart, so the second chunk needs a
   second interrupt **and** a re-posted receive buffer;
3. wait, as today, for `cons.beside ok` in the serial log, **and now also for `minix.rs init: hvc0
   ok`**;
4. write the serial probe to stdout in two chunks, as today.

Stage 1 is what makes one serial-log line prove both directions: `hvc0 ok` cannot print unless the
bytes were fed, and they are fed only after the guest's output was seen on the host. Stage 3's new
condition is what makes C10's proof deterministic: the `read(0)` reader is still parked when
`vcon`'s notification arrives.

The price is a coupling, stated rather than hidden: **a boot whose `vcon` is broken loses `stdin ok`
as well as `hvc0 ok`**. The first missing line names the cause, and `hvc0 ok` is listed first.

The host test in `userland/init` that parses the feeder's `PROBE` line gains the two new strings, so
the script and init cannot drift. CI's invocation does not change; the reboot stays unfed and its
seven-line set is untouched. The feeder's ceiling and the two-looks rule are unchanged.

### C13 — D8: additive, and the generated headers do not move

No `Message` layout, call number, endpoint or errno changes. `DEV_HVC0_PATH` and `CDEV_MINOR_HVC0`
are new names in `kernel-shared` and are **not** added to `tools/gen-c-headers`: no C program opens
the node (init is Rust), and a constant nobody consumes is better absent. The generated headers are
diffed against `origin/main` and must be byte-identical; the musl fork is not touched.

### C14 — measure first

Task 0, before any driver code, with throwaway probes that are not committed:

1. Unverified fact 1 — attach the device and record what the firmware and Limine do to it: `Status`
   at kernel entry, anything written to the host side before the kernel banner, whether boot time
   moves.
2. Unverified fact 2 — C2's four acceptance conditions, on this machine; the runner's answer is the
   branch's first CI run. If the pipe fails here, switch to the socket before Task 1 and record it.
3. Unverified fact 3 — queue numbering, transmit completion with no host reader, input fed with no
   buffer posted, and how one host write is cut across guest buffers.
4. The kernel's lines for the new device (`[virtio] slot=8 …`, `[dma] vcon …`), read from a boot,
   not derived.
5. The boot budget at the merge base, by [`ci.md`](../../conventions/ci.md)'s byte-position reading,
   for both backends.

The findings go in the phase file's *Measured* list. A finding that contradicts a decision above
rewrites that decision before code is written.

---

## 3. Verification

**Host tests** (each named in the plan with its task):

- `driver-rt::cdev` — the moved `Waiter` / `read_plan` / validator tests, plus: input-impossible
  answers `EIO` for a non-zero read and still `0` for a zero-length one.
- `virtio-console::bringup` — the four-page layout: each queue on its own page, neither buffer
  reaching a queue page, a short run refused.
- `virtio-console::port` — `Posted` → `Held` on a completion; length clamped; zero-length re-posts;
  a partial read leaves the rest held; `off` unmoved by a failed copy; drained → re-post; a second
  completion while held is a fault; the transmit plan clamps at `CDEV_MAX_IO`.
- `vfs::dev` — the fourth row; the duplicate-path and uses-every-driver tripwires grown, not left.
- `vfs` walk step — skip another driver's entry; ask the notifier's; `EBADF` for a non-device.
- `vfs::park` — the in-order walk: order, passing over a kept entry, an entry freed mid-walk.
- `tty::irq` — a stopped line reports that receive is over.
- `userland/init` — the feeder's strings equal init's constants.
- `cargo miri test -p minixrs-driver-rt` after the move.

**Boot markers** (text fixed in the plan; counts recomputed from constants):

| Marker                                            | Files            | Proves                                                           |
| ------------------------------------------------- | ---------------- | ---------------------------------------------------------------- |
| `[virtio] slot=8 … drv=vcon`, `[dma] vcon …`      | common expected  | the probe assigned the console and its run                       |
| `[diag vcon] virtio ok features=0x100000000 …`    | common expected  | bring-up, both queues, `VERSION_1` and nothing else              |
| `[diag vfs] vcon.ds ok`                           | common expected  | the packing order (`vcon < vfs`)                                 |
| `minix.rs init: hvc0 ok n=…`                      | common expected  | both directions, the refused copy consuming nothing, the re-post |
| `[diag vfs] cons.other ok`                        | common expected  | a walk passed over a reader parked on the other console          |
| `[diag vcon] virtio FAIL`, `no device`, `rx FAIL` | common forbidden | —                                                                |
| `hvc0 FAIL`, `[diag vfs] vcon.ds FAIL`            | common forbidden | —                                                                |
| every existing marker, `stdin ok n=15` among them | unchanged        | fd 0, 1 and 2 are still TTY's                                    |
| the fork-cycle slot numbers                       | common expected  | recomputed for the second long-lived child                       |

The new `[diag vfs]` lines do not depend on the root backend, so `diff-backends.sh` needs no
allowance. Definition of done is [the slice workflow](../../conventions/docs-and-workflow.md)'s:
both fed full boots pass `check-boot-log.sh`, `diff-backends.sh` is clean, the unfed reboot passes
its seven, and the budget reading is recorded against the merge base.

**Mutations** — each applied, observed and reverted; the prediction is what the plan checks against:

| #  | Mutation                                                     | Predicted                                                              |
| -- | ------------------------------------------------------------ | ---------------------------------------------------------------------- |
| 1  | `vcon` advances `off` before the copy returns                | `hvc0 ok` absent (the child comes up short); `stdin ok` absent with it |
| 2  | `vcon` does not re-post the drained buffer                   | `hvc0 ok` absent: the second chunk never arrives                       |
| 3  | `vcon` does not re-arm after an interrupt                    | `hvc0 ok` absent, or a hung first write                                |
| 4  | `vcon` sends no wake-up `NOTIFY`                             | `hvc0 ok` absent; everything before it unmoved                         |
| 5  | `vcon` skips the `transmitq` notify                          | the first write never completes: the boot stops behind VFS             |
| 6  | `vcon` answers `EAGAIN` without recording the waiter         | `hvc0 ok` absent                                                       |
| 7  | VFS drops `vcon`'s notification (notifier check not widened) | `hvc0 ok`, `cons.other ok` absent                                      |
| 8  | VFS's walk stops at the first `EAGAIN` from any driver       | `hvc0 ok`, `cons.other ok` absent — the hang §1 describes              |
| 9  | VFS's `/dev/hvc0` row names TTY                              | the out line appears on the serial log; `hvc0 ok` absent               |
| 10 | the archive row moved after `vfs`                            | `vcon.ds FAIL … fallback=…`; the rest still passes                     |
| 11 | acknowledge after the walks instead of before                | nothing moves (held by `driver-rt`'s argument, as for `virtio-blk`)    |
| 12 | a broken `vcon` does not wake its waiter; TTY likewise       | nothing moves on a boot; a host test fails                             |
| 13 | the feeder writes `HVC0_IN` in one piece                     | mutation 2 then passes — which is why it is two chunks                 |

A row that behaves differently is recorded as it was seen, and the proof it weakened is fixed or
entered as a debt.

---

## 4. Documentation owed in the same PR

- **`book/`** — `drivers/overview.md` and the console chapter: the second console, the driver's
  receive and transmit paths, the per-driver walk. Each touched chapter read end to end once.
- **[`servers-and-drivers.md`](../../conventions/servers-and-drivers.md)** — the R1 section's "this
  is the contract 6.6 and 6.8 reuse" paragraph (now a fact for 6.6, and where the shared code
  lives); *Parking a console read* (the notifier set, the per-driver walk, `cons.other ok`); the
  device-node table (four rows); the packing chain; the `EIO` bullet rewritten for C9; the CDEV
  literal-minor note under probes if a probe is added.
- **[`build-and-boot.md`](../../conventions/build-and-boot.md)** — *Fed boots*: the second stage,
  the FIFOs, `MINIXRS_HVC0`, the coupling of C12.
- **[`testing-and-markers.md`](../../conventions/testing-and-markers.md)** — the new markers and the
  recomputed counts.
- **[`phase-6-virtio.md`](../../plans/phase-6-virtio.md)** — the 6.6 section (design and plan links,
  *What it settled*, *Measured*, mutations, debts); R12's sentence about `park.rs` rewritten; 6.5's
  debts 1 and 11 and 6.2's debt 2 updated where they stand; both checkboxes, here and in
  [`plan.md`](../../plan.md).
- **The 6.5 spec's R12** — the `park.rs` sentence rewritten in place, not bannered.
- `tools/qemu-run.sh`'s header comment ("attaches nothing else") and `tests/*.expected` commentary.

---

## 5. Rulings to check

Decisions the phase file did not make. Each is undone by the change in its last column.

| #   | Ruling                                                                               | If wrong                                                                                                    |
| --- | ------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| C1  | The console device is attached to every boot                                         | One `if` in `qemu-run.sh` and the kernel markers move to a per-boot file                                    |
| C2  | Host side is a `pipe` chardev over FIFOs (socket if C14 says no)                     | Swap the chardev line and the feeder's two file descriptors; no guest change                                |
| C3  | No optional feature; 6.2's sub-word-config debt moves to 6.8                         | 6.8 pays it with no head start; nothing here to undo                                                        |
| C4  | `vcon` always publishes and answers `ENXIO` without a device                         | Make the publish conditional like `vblk`'s and give VFS's lookup a healthy-miss arm                         |
| C5  | One receive buffer, one transmit buffer, 256 bytes each                              | `port.rs` becomes a FIFO of held buffers; the layout already has 12 spare pages                             |
| C6  | No ring: reads are served from the held buffer, and the host is made to wait         | Add a ring and accept drops — a change inside one driver                                                    |
| C6  | R1's pure half moves from TTY to `driver-rt`                                         | Move it back, or copy it; a mechanical commit, but it touches TTY in a slice that otherwise leaves it alone |
| C7  | A transmit is waited for before the reply                                            | A buffer pool and a busy answer; contained in the driver                                                    |
| C9  | 6.5's debt 1 is paid in TTY as well as avoided in `vcon`                             | Revert five lines in TTY; the debt returns to the list                                                      |
| C10 | The walk is per driver, by a filter over one table; `park.rs` changes after all      | Two tables; more state for the same behaviour                                                               |
| C10 | A new required marker, `cons.other ok`                                               | Delete it and C10's walk is proved by host tests only                                                       |
| C11 | A second long-lived child: a fork slot, renumbered fork-cycle markers, one more reap | Fold it into the first child; the markers go back, and the two-reader proof is lost                         |
| C11 | Debt 11 is paid for `vcon` only; TTY's stays, with the recipe                        | One more read in the `read(0)` child                                                                        |
| C12 | One feeder, `hvc0` first; a broken `vcon` also costs `stdin ok`                      | Two independent feeders and a C10 proof that passes by timing                                               |
| C13 | The new names are not emitted into the C headers                                     | Add two `define`s and regenerate; additive, so still no ABI bump                                            |

---

## 6. Out of scope

- **Multiport, a control queue, more than one port, window size, emergency write.**
- **fd 0, 1 and 2 on anything but TTY**, and any way to choose a console at boot.
- **Echo and a line discipline** on either console (6.8 and Phase 7).
- **A deeper receive path** — buffer pools, zero-copy, `EVENT_IDX`.
- **Hardening "always reply" against a reply-shaped message** (6.5's debt 9, owner 6.8).
- **The kernel.** If Task 0 or a mutation shows it has to change, that is a finding to stop on, as
  6.5's was, not a change to make in passing.

---

## 7. Corrections found in implementation

Added 2026-11-24, after Tasks 0 to 5. The decisions above are left as approved; this section records
where building them showed the text to be wrong or incomplete. The plan's ledger has the detail, and
the phase file's
[6.6 section](../../plans/phase-6-virtio.md#slice-66-virtio-console-as-a-second-cdev-backend) is the
record of what was built.

- **§3's marker table has the two fed lines in the wrong order.** It lists `minix.rs init: hvc0 ok
  n=…` before `[diag vfs] cons.other ok`. In the log `cons.other ok` comes **first**: VFS prints it
  at the first chunk's walk, when its answer for the reader behind the passed-over serial one is the
  `EFAULT` of C11's step 3, and the child's verdict follows a feeder gap later. The order on a fed
  boot is `cons.other ok`, `hvc0 ok n=17`, `stdin ok n=15`.
- **§3's marker table omits four lines that were added.** Expected: `[as] vcon nr=12 ttbr0_pa=`,
  `[devmap] vcon va=0x40001000 pa=0xa001000 intid=56`, and a third `[ksys SYS_FORK] parent=i nr=10
  child_nr=21` (the table's "fork-cycle slot numbers" row covers the renumbering but not the new
  line). Forbidden: `[diag vfs] grant.test FAIL`, and also `[diag vcon] ds FAIL`. The kernel's
  `[dma] vcon` line reads `va=0xc0000000 pages=16`, and the driver's verdict `rxq=8 txq=8 slot=8`.
- **C11 lists four numbered steps — five actions, counting the compare — and there are six.** The
  child also **closes** the descriptor, on every path, before it prints its verdict: open, write,
  bad-read, read, compare, close, with `hvc0 FAIL step=close rc=…` for a close that fails. Without
  it the child exits with one descriptor open and VFS prints `[diag vfs] pm-exit reset closed=1` —
  `fileio`'s marker — far too early.
- **C12 says nothing about stale bytes in the FIFOs, and its stage list is short by a wait.** C2
  assumed an unfed boot's output "sits in the pipe" harmlessly. On macOS it is still there after
  QEMU has exited, so the next fed boot's feeder found the previous boot's line at once and fed a
  machine still in its firmware. The feeder now empties both FIFOs when it starts, and between
  seeing the guest's line and writing its answer it waits `SETTLE` seconds so that the child's
  unmapped read is parked, not refused directly. C2's "no stale-file race" claim for the FIFO is
  therefore wrong as written: the race exists and the drain is its guard.
- **C9 assumed a failed TX proof already left TTY's line stopped.** It did not: `proof_round`
  printed its `FAIL` and stayed in `Proof`. The slice made it enter `Stopped`, which C9 needs.
- **C10 says `cdev_endpoint` "grows an arm".** It left `main.rs` instead: `CharPeers::endpoint` in
  the new `retry.rs`, beside `CharPeers::notifier`, so both are host-tested. And the step function
  is in `retry.rs`, not `park.rs`.
- **C4's sentence on VFS's prologue is incomplete.** "`vcon_endpoint()` resolves `vcon` once in the
  prologue beside the other three" put a fourth blocking lookup ahead of VFS's `grant_test` send to
  PM, and with the boot stubs on that lost PM's four grant markers. `grant_test` now precedes every
  lookup. The rule is in
  [`servers-and-drivers.md`](../../conventions/servers-and-drivers.md#prologue-ordering).
- **§4 asks for the 6.5 spec's R12 sentence to be "rewritten in place, not bannered".** It was
  rewritten in place, with a dated note under it saying what it had said.
- **C14 items 3 and 5 were not done in Task 0.** Item 3 was measured on the real driver's first
  boot, and item 5's baseline was moved to Task 7 (ledger, Task 0 rulings).
- **§3 says `vcon.ds ok` proves the packing order, and mutation 10 predicts `vcon.ds FAIL` for a row
  moved after `vfs`.** Observed: nothing moves. The line proves the lookup, not the order.
- **§3's mutation 12 predicts "a host test fails".** True only for the shared helper; the call sites
  that send the wake-up are not host-testable. TTY's decision was moved into the same helper.
- **§3's mutations 1, 3, 4 and 6** behaved as recorded in the
  [phase file](../../plans/phase-6-virtio.md), not as their rows read.
- **§3's "budget reading against the merge base"** — see the phase file for what was booted.

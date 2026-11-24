# Slice 6.5 — TTY RX: PL011 receive interrupt + `CDEV_READ` + a VFS that does not block on it

**Status:** approved 2026-11-23. **Phase file:**
[`phase-6-virtio.md`](../../plans/phase-6-virtio.md#slice-65-tty-rx--pl011-receive-interrupt--cdev_read).
Decisions here are labelled `R1…R12`; the phase's `H1…H12` and 6.1's `I1…I14` stay in force and are
cited by their own labels.

**Goal:** the console can be read. A byte arriving on the PL011 reaches a `read(0)` in a user
process, and a process waiting for one does not stop VFS serving everybody else.

The phase file locks exactly two things for this slice: **VFS does not block on a console**, and
**6.6 and 6.8 reuse whatever 6.5 builds**. Everything else below is this slice's own design.

---

## 1. What exists, and what 6.5 changes

Verified against `main` at `15e13e5`.

| Where                       | Today                                                                                            | After 6.5                                                                                      |
| --------------------------- | ------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------- |
| `drivers/tty/src/main.rs`   | `CDEV_WRITE` only; `CDEV_READ` answers `ENOSYS` from the unknown-request arm                     | `+` a `CDEV_READ` arm that never withholds its reply (R1)                                      |
| `drivers/tty/src/irq.rs`    | one-shot TX proof; any later notification is `irq FAIL extra` and the line stays masked          | the proof hands the line to a steady-state RX loop (R4); the `extra` guard is retired (R5)     |
| `drivers/tty/src/pl011.rs`  | `DR` store, `FR.TXFF`, `IMSC.TXIM`                                                               | `+` `DR` load, `FR.RXFE`, `IMSC.RXIM`/`RTIM`, `ICR`                                            |
| `drivers/tty/src/input.rs`  | —                                                                                                | new: the input ring, host-tested (R3)                                                          |
| `servers/vfs/src/main.rs`   | `do_read` SENDRECs the driver and relays the answer; every request is replied to before the next | a console read that finds nothing is **parked**; a `NOTIFY` from TTY retries the parked (R2)   |
| `servers/vfs/src/park.rs`   | —                                                                                                | new: the park table, host-tested (R2)                                                          |
| `kernel-shared` `callnr.rs` | `CDEV_READ`: "`0` is EOF, a short read is legal"                                                 | `+` "`EAGAIN` means nothing is buffered and the driver will `NOTIFY` the caller" (R1, R10)     |
| `userland/init`             | `read-console` probe expects `ENOSYS` from `read()` on fd 1                                      | probe removed; a forked child reads fd 0 and reports what it got (R7)                          |
| `tools/`                    | boots take no input                                                                              | `+` `feed-console.sh`: writes bytes to QEMU's stdin once the log shows the read is parked (R8) |
| kernel                      | —                                                                                                | **no change expected** (R9)                                                                    |

Facts the design rests on:

- **QEMU's serial is `-serial stdio`**, so the host's stdin *is* the PL011's receive side. CI runs
  each boot as `timeout N cargo run … > log 2>&1`, with stdin inherited from the runner.
- **Every boot server and driver is `SRV_T`** (`trap_mask = !0`) with `ipc_to` filled for every
  active slot, so TTY may already `NOTIFY` VFS and no `ipc_to` edge has to be opened.
- **`ipc_notify` exists** in `minixrs-ipc` and is non-blocking; a notification to a process that is
  not receiving is latched in its `notify_pending` word and delivered at its next `RECEIVE`.
- **`SENDA` is dispatcher-denied** (`trap_gate`'s 16-bit mask cannot hold bit 16). An asynchronous
  reply from TTY to VFS is not available without kernel work.
- **A client's `SENDREC` to VFS is satisfied by any later `SEND` from VFS.** VFS "parking" a client
  costs the kernel nothing: VFS skips the reply and sends it later.
- **VFS's receive loop replies to everything**, including a message it does not understand (`_ =>
  ENOSYS`). A `NOTIFY` from TTY today would get an `ENOSYS` *sent back to TTY*.
- **`EAGAIN` (11) is already in `error.rs`** and the generated `errno.h`.
- **TTY's `irq FAIL extra` guard forbids every notification after the TX proof** — 6.1's carried
  debt names this slice as the one that retires it.

Not verified, because only running the slice can — each is a measure-first step (R11):

1. Whether the firmware leaves the PL011 FIFO enabled (`LCR_H.FEN`), and whether one received byte
   raises the RX interrupt under QEMU 11 in either state.
2. What QEMU does when the host's stdin reaches EOF under `-serial stdio`.
3. Whether edk2 or Limine consume bytes that are on stdin before the kernel starts.

---

## 2. Decisions

### R1 — `CDEV_READ` never blocks: empty answers `EAGAIN`, and the driver owes a `NOTIFY`

A character driver answers every `CDEV_READ` at once. With input buffered it copies `min(len,
buffered, CDEV_MAX_IO)` bytes into the grant and replies the count. With nothing buffered it replies
**`EAGAIN`**, records the caller's endpoint as its *waiter*, and — when input next arrives — sends
that endpoint one `NOTIFY` and forgets it. The caller then asks again with an ordinary `CDEV_READ`.

This is H8's notify-then-pull, turned to face the other way: H8 has TTY pulling from the keyboard
driver; here VFS pulls from TTY. One contract serves both hops, which is what "6.6 and 6.8 reuse
whatever 6.5 builds" asks for: `virtio-console` implements the same three lines, and in 6.8 TTY is
the *caller* of the same contract against `virtio-input`.

There is no lost wakeup. The `EAGAIN` reply reaches VFS before VFS can receive anything else, so the
read is parked before a notification can be seen; a notification sent while VFS is busy elsewhere is
latched by the kernel and delivered at VFS's next `RECEIVE`.

The driver keeps **one** waiter, not a queue of reads: every reader goes through VFS, and the
notification means "ask again", not "this read is ready", so one endpoint to notify is the whole
state. A second, different endpoint asking while a waiter is recorded is answered `EAGAIN` and
replaces it — the last asker wins. Nothing but VFS sends TTY a `CDEV_READ` today, so the case is
unreachable at boot; it is pinned by a host test and listed in §5.

- The request is validated by `CDEV_WRITE`'s rules in `CDEV_WRITE`'s order (minor, negative length,
  grant id), and `len == 0` then answers `0` **before the ring is looked at** — so a zero-length
  read of an empty console is `0`, not `EAGAIN`.
- A copy that fails (`EPERM`, `EFAULT` from `SYS_SAFECOPY`) is relayed verbatim and **consumes
  nothing**: the bytes are peeked, copied, and only then removed from the ring.
- `0` keeps its meaning — EOF — and TTY never sends it for a non-zero request. A console has no EOF
  until a line discipline defines one (R6).
- If TTY failed to claim its line at boot it can never receive, and answers **`EIO`** rather than
  `EAGAIN`: parking a reader behind an interrupt that cannot arrive would be a hang with no marker.

*Rejected:* **withholding the reply** (the status quo's implication — stalls every file operation in
the system, which the phase file forbids). **MINIX 3's `CDEV_REPLY` over `asynsend`** (needs
`SENDA`, which the kernel does not dispatch; widening `trap_mask` and finishing `mini_senda` is a
slice of its own). **Classic MINIX's `SUSPEND` + `DEV_REVIVE` + `DEV_STATUS`** (the driver keeps the
suspended request and its grant; it costs two new request numbers, a per-reader table in every
driver, and a grant held open for as long as nobody types — against a pool of eight). **A blocking
`SEND` from TTY to VFS on completion** (VFS's next `CDEV_WRITE` SENDREC would meet TTY blocked
sending to VFS: a deadlock the kernel reports as `ELOCKED`).

### R2 — VFS parks the read and retries it on TTY's notification

`do_read`'s character-device arm is unchanged up to the driver's answer. If the answer is `EAGAIN`,
VFS revokes the grant, records `{caller endpoint, fd, buf, len}` in a **park table**, and **does not
reply**. The caller stays blocked in its own `SENDREC`; VFS returns to `RECEIVE`.

A `NOTIFY_MESSAGE` whose kernel-stamped `m_source` is the console driver's endpoint is handled
before the request dispatch and gets **no reply**. VFS walks the parked reads oldest first. For
each: re-resolve the descriptor, issue a fresh magic grant, send `CDEV_READ`. Any answer other than
`EAGAIN` — a count or an error — is sent to the parked caller as the reply to its original
`VFS_READ` and the entry is freed. On `EAGAIN` the walk stops: that refusal re-armed the driver's
waiter, so the next byte brings the next notification.

- **No grant is held while parked.** The entry stores the raw request; the grant is issued per
  attempt, so a parked reader costs no slot of `GRANT_SLOTS`.
- **The table is indexed by proc slot**, one entry per process: a process blocked in `SENDREC` can
  have only one request outstanding, so the table cannot overflow and there is no "too many readers"
  errno. Each entry carries a sequence number for the oldest-first walk.
- **A request arriving from a slot evicts a parked read stored under a different endpoint for that
  slot** (`evict_stale`). VFS keeps no process table, so the kernel-stamped `m_source` of ordinary
  traffic is the only view it has of who occupies a slot; this is the second guard behind the next
  one, not the first.
- **`VFS_PM_EXIT` frees the exiting process's entry** before resetting its descriptors, so VFS never
  replies to a dead endpoint. The entry stores the full endpoint (generation included) and the retry
  compares it, so a recycled slot cannot inherit a parked read.
- **A descriptor that no longer resolves at retry** (it cannot be closed while its owner is blocked,
  but the check is free) answers `EBADF` and frees the entry.
- **A notification from anyone else** is dropped without a reply. A spurious one from TTY costs one
  round of `CDEV_READ`/`EAGAIN` per parked read and nothing more.
- **`rw::validate`'s rules still run first** — a bad length or buffer is refused before any driver
  is asked, so a malformed read is never parked.

The table and its walk order live in `servers/vfs/src/park.rs` as total functions over plain values,
in the `rw.rs`/`fd.rs` shape, with host tests; the IPC stays in `main.rs`.

Only a **console** descriptor can park: the memory driver never answers `EAGAIN`. VFS does not
special-case the driver, though — it parks on the answer, whoever gave it — so 6.6's `/dev/hvc0`
needs no VFS change beyond a second notifier endpoint. In 6.5 the source check compares against the
one DS-resolved `tty` endpoint VFS already holds; 6.6 widens it to the set of character drivers.

*Rejected:* a VFS worker-thread pool (MINIX 3's answer; minix.rs servers are single-threaded by
construction and this slice is not where that changes); keeping the grant across the park.

### R3 — the input ring

`drivers/tty/src/input.rs`: a fixed ring of `TTY_INPUT_MAX = 256` bytes with `push`, `peek` into a
slice, `consume(n)` and `len`. No `unsafe`, no IPC, host-tested. When full, `push` **drops the new
byte** and saturating-counts it; the first drop of a boot prints `[diag tty] rx overflow` once (a
forbidden marker). Dropping the newest keeps what the reader has not yet seen in order.

### R4 — the steady-state loop: acknowledge, drain to empty, re-arm

After the TX proof's last round TTY sets `UARTIMSC.RXIM | RTIM` (receive, and receive timeout — the
second covers a FIFO-enabled UART holding fewer bytes than its trigger level). On each `HARDWARE`
notification carrying its bit:

```text
write UARTICR = RXIC | RTIC          acknowledge both sources
while FR.RXFE == 0: push(DR & 0xFF)  drain until the FIFO reports empty
IRQ_ENABLE                           re-arm the line at the GIC
if anything was pushed and a waiter is recorded: NOTIFY it, forget it
```

**The acknowledgement precedes the drain**, which is the order the phase file's standing rules give
virtio and the reverse of I12's text. The reason is the same one: a byte landing between a
drain-to-empty and a later `UARTICR` write would have its interrupt acknowledged away with the byte
still in the FIFO, and the re-arm would find the line low — one keystroke stuck until the next.
Acknowledge-then-drain cannot lose one: a byte that lands after the final `RXFE` test raises a
fresh, unacknowledged interrupt, which the re-arm takes. The cost is at worst one notification that
finds the FIFO empty, which the loop handles by doing nothing.

`DR`'s error bits (framing, parity, break, overrun — bits 8–11) are discarded with the byte kept;
QEMU never sets them and a console has no use for a dropped character.

The waiter is notified **after** `IRQ_ENABLE`, so the line is live again before VFS can come back
with a read.

TTY does **not** write `LCR_H` or `IFLS`. The kernel's polled console shares this UART, and the loop
above is correct whether the FIFO is on or off.

### R5 — the TX proof stays, and hands over

`IrqProof` keeps its two rounds and its `irq ok rounds=2` marker unchanged. What changes is the end:
instead of leaving every mask bit clear and treating a third notification as failure, the last round
enables RX (R4) and the object becomes the RX service. `irq FAIL extra` and its `extra_reported`
field are deleted. `tests/qemu-boot.forbidden` holds the broader `[diag tty] irq FAIL`, which stays;
only its commentary changes.

What the guard caught — a line that keeps firing with nothing to service — is now caught
differently: a notification that drains zero bytes is counted, and a run of `RX_EMPTY_STORM = 64`
consecutive empty ones prints `[diag tty] rx FAIL storm` once (forbidden) and leaves the line
masked. One empty notification is legal (R4); sixty-four in a row is a device that is not dropping
its line.

### R6 — raw bytes, no echo, no line discipline

A `read()` returns whatever is buffered, at least one byte, exactly as received: no echo, no CR→LF,
no erase, no line buffering. The phase file's non-goal is the bound — "6.5 and 6.8 deliver input to
`read(0)` and stop" — and a line discipline without `termios` to turn it off is a policy with no way
out.

This is a ruling with a visible cost: typing into a hand-run boot shows nothing. Echo arrives with
6.8, whose proof requires it on the screen, and canonical mode with Phase 7's shell. Both land in
`input.rs`, which is why it is its own module now.

### R7 — the userland proof: a child that reads fd 0

`init`, early in its prologue, forks one child. The child calls `read(0, buf, 64)` and blocks. When
the read returns it compares the bytes against the fixed string the feeder sends (`CONSOLE_PROBE =
b"minix.rs stdin\n"`), writes one verdict line to fd 1 — `minix.rs init: stdin ok n=15` or `init:
stdin FAIL n=… first=…` — and exits with a positive sentinel status
([`testing-and-markers.md`](../../conventions/testing-and-markers.md#exit-status-probes)). init
reaps it in the normal course of its rotation, keyed on its pid.

The old `read-console` denial probe (`read()` on fd 1 expecting `ENOSYS`) is deleted, not re-aimed:
after this slice that read parks forever. `open.deny`'s count drops by one, recomputed from its
table.

The proof that VFS did not block is **VFS's own**, printed at the moment it is true: the first time
VFS completes a file-descriptor `VFS_READ` while at least one read is parked, it prints `[diag vfs]
cons.beside ok` (once per boot). When it first parks a read it prints `[diag vfs] cons.park`.
Neither is something init could know.

If the child reads a short count — the bytes arrive in two interrupts — it loops until it has the
probe's length or a non-positive answer, so the verdict does not depend on how QEMU batches stdin.

### R8 — the feeder, and CI

`tools/feed-console.sh <log>` polls the log for `cons.beside ok`, writes `CONSOLE_PROBE` to its
stdout once and exits (R11.2 measured that QEMU carries on past EOF on stdin); with no marker it
gives up, silent, at a ceiling. A boot is fed through a process substitution, so the shell does not
wait on the feeder and the boot's exit status is still `timeout`'s:

```bash
MINIXRS_ROOT=disk timeout 1200 cargo run -p minixrs-kernel … \
    < <(tools/feed-console.sh qemu-disk.log) > qemu-disk.log 2>&1
```

Gating on the marker is what makes the proof causal — the child's read cannot return until the
feeder writes, and the feeder does not write until VFS has served a file read beside the parked one
— and it also keeps the bytes away from the firmware and Limine (unverified fact 3).

- The **disk** and **ramdisk** full boots are fed; their new markers go in the common
  `tests/qemu-boot.expected`. `tools/diff-backends.sh` must stay clean across the pair.
- The **reboot** boot is not fed: its child parks for the whole 180 s, which is itself a second
  demonstration that a parked read costs the boot nothing. Its marker pair does not change.
- A hand-run `cargo run` with a terminal on stdin works as before; the first line typed completes
  the child's read (and almost certainly prints `stdin FAIL`, which is correct and harmless — the
  marker files apply to fed boots only).

New required markers: `[diag vfs] cons.park`, `[diag vfs] cons.beside ok`, `minix.rs init: stdin ok
n=15`. New forbidden: `rx overflow`, `rx FAIL`, `stdin FAIL`. The existing forbidden `[diag tty] irq
FAIL` is unchanged (R5).

### R9 — no kernel change

INTID 33 is already on TTY's allow-list, the line is already claimed, TTY may already notify VFS,
and VFS may already withhold a reply. If the measure-first step finds otherwise — a missing `ipc_to`
bit, a `mini_notify` path that mishandles a server-to-server notification — the finding is recorded
in the plan's ledger and fixed in this slice, with a QEMU marker, not a host test.

This is also the first boot in which a user process stays blocked indefinitely. It does not make the
default boot idle (init's rotation never stops), so 6.1's `idle-probe` debt is unaffected.

### R10 — D8: additive

No `Message` layout, call number, endpoint or errno **value** changes. `CDEV_READ` gains a defined
meaning for one reply value on a band whose only clients are VFS and (in 6.8) TTY — both in this
repository. musl never sees `EAGAIN` from a console `read()`: VFS parks instead of relaying it.
Regenerated headers are byte-identical, which the plan checks rather than assumes.

### R11 — measure first

Task 0 of the plan, before any design-dependent code:

1. Read `LCR_H` and `IFLS` from TTY at boot and print them; feed one byte and count interrupts. R4
   is written to be right either way; the measurement decides only what the documentation says.
2. Close stdin under `-serial stdio` and observe QEMU. If it exits or spins, the feeder's hold-open
   behaviour (R8) is load-bearing and is documented as such; if it carries on, the feeder may simply
   exit.
3. Pipe bytes at QEMU from time zero and see whether they survive to the kernel. R8 does not depend
   on the answer, but `build-and-boot.md` should state it.
4. Boot-budget: the last required marker's position against the merge base, as
   [`ci.md`](../../conventions/ci.md) prescribes. One extra fork and one parked process are not
   expected to move it.

### R12 — what 6.6 and 6.8 inherit

Stated so the next two specs can cite it instead of re-deciding:

- **6.6** implements R1 in `virtio-console`, adds `/dev/hvc0` to VFS's device table, and VFS's
  notification check gains that driver's endpoint. No change to `park.rs`.
- **6.8** makes TTY a *client* of R1 against `virtio-input`: on that driver's `NOTIFY`, TTY sends
  `CDEV_READ` until it hears `EAGAIN`, pushes through the keymap into the same ring, and notifies
  its own waiter exactly as R4's last line does. The ring gains a second producer and nothing else.

---

## 3. Verification

| Behaviour                                                                  | Proved by                                                                                  |
| -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| ring: push/peek/consume, wrap, full-drops-newest                           | host tests, `drivers/tty` `input.rs`                                                       |
| `CDEV_READ` validation; `len == 0`; clamp to `CDEV_MAX_IO`                 | host tests, `drivers/tty` `cdev.rs`                                                        |
| park table: one per slot, oldest-first, exit frees, stale endpoint refused | host tests, `servers/vfs` `park.rs`                                                        |
| a byte on the wire reaches `read(0)`                                       | `minix.rs init: stdin ok n=15` (fed boots)                                                 |
| VFS serves a file read while a console read is parked                      | `[diag vfs] cons.park` then `[diag vfs] cons.beside ok`; the feeder writes only after both |
| the parked read is woken by TTY's notification                             | `stdin ok` appearing at all — nothing else can complete that read                          |
| a parked reader does not hold the boot                                     | every existing marker still present in all three boots, the unfed reboot included          |
| the TX proof still runs                                                    | `[diag tty] irq ok rounds=2`, unchanged                                                    |
| no interrupt storm, no dropped input                                       | forbidden `rx FAIL`, `rx overflow`                                                         |
| headers unchanged                                                          | `gen-c-headers` output diffed against `main`                                               |

Mutations, each applied, observed and reverted
([discipline](../../conventions/testing-and-markers.md#mutation-testing)):

| # | Mutation                                                        | Expected to move                                                        |
| - | --------------------------------------------------------------- | ----------------------------------------------------------------------- |
| 1 | TTY withholds the reply on empty instead of `EAGAIN`            | VFS wedges: `cons.beside ok` and everything after it vanish             |
| 2 | VFS relays `EAGAIN` instead of parking                          | `stdin FAIL` (the child sees `-11`); `cons.park` absent                 |
| 3 | TTY does not `NOTIFY` its waiter                                | `stdin ok` absent; the rest of the boot unmoved                         |
| 4 | VFS replies `ENOSYS` to TTY's notification (the status quo arm) | observe and record — may be absorbed; an honest "nothing moves" if so   |
| 5 | TTY drops the `IRQ_ENABLE` after the drain                      | first chunk only: `stdin` verdict depends on batching — observe, record |
| 6 | TTY skips the drain                                             | `rx FAIL storm`                                                         |
| 7 | `consume` before the copy succeeds                              | host test for peek-then-consume fails                                   |
| 8 | `VFS_PM_EXIT` does not free the park entry                      | host test fails; no boot marker (nothing kills a parked reader at boot) |

Rows 4, 5 and 8 are stated as predictions, not results; the plan records what was seen.

---

## 4. Documentation owed in the same PR

- [`servers-and-drivers.md`](../../conventions/servers-and-drivers.md): the "Don't answer for a
  driver" console paragraph is **replaced** (it says Phase 6 touches VFS not at all); a new rule for
  R1 — a character driver never withholds a `CDEV_READ` reply — with R2 as VFS's half; the 6.1 `irq
  FAIL extra` lines corrected; I12's order note gains the PL011-RX case (R4).
- `book/src/drivers/overview.md`: the `CDEV_READ` paragraph ending "Phase 6 changes TTY and nothing
  else" is **replaced**; the TTY and VFS chapters gain the RX loop and the park table. Each touched
  chapter is read end to end once.
- [`abi.md`](../../conventions/abi.md) and `callnr.rs`'s `CDEV_READ` doc comment: the `EAGAIN`
  contract (R1, R10).
- [`testing-and-markers.md`](../../conventions/testing-and-markers.md),
  [`build-and-boot.md`](../../conventions/build-and-boot.md), [`ci.md`](../../conventions/ci.md):
  fed boots, the feeder, the unfed reboot, R11's measurements.
- `drivers/tty` module docs ("TX only", "`CDEV_READ` lands in the unknown-request arm, on purpose"),
  `pl011.rs` ("TX only, polled"), `irq.rs` ("deliberately not the steady-state pattern"), VFS
  `do_read`'s "Phase 6 changes TTY and nothing here", and the marker files' commentary — the
  stale-claim sweep, by phrase, across `tests/ book/ docs/ CLAUDE.md`.
- The 6.1 spec's I12 "TTY RX needs no VFS … change" paragraph: its operative text is rewritten, not
  bannered.
- `docs/plan.md` and `phase-6-virtio.md`: 6.5's box, the link to this spec and its plan, and 6.1's
  carried `irq FAIL extra` debt struck as paid.

---

## 5. Rulings to check

| #   | Ruling                                                                    | If wrong                                                                                                                               |
| --- | ------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| R1  | `EAGAIN` + `NOTIFY` + retry, not an asynchronous reply                    | Moving to `SENDA`/`CDEV_REPLY` later rewrites the read arm of each character driver and VFS's retry; `park.rs` and the ring survive.   |
| R1  | One waiter endpoint, last asker wins                                      | A second concurrent CDEV client of TTY could starve the first of its `NOTIFY`. Fix is a two-slot set in `main.rs`; no protocol change. |
| R1  | `EIO` when TTY has no interrupt line                                      | One constant; no marker depends on it.                                                                                                 |
| R2  | No grant held across a park; a fresh one per retry                        | None foreseen — it is strictly cheaper. Listed because classic MINIX does the opposite.                                                |
| R2  | VFS parks on the *answer*, not on the device                              | If a driver ever answers `EAGAIN` without owing a `NOTIFY`, its reader hangs. The convention rule added in §4 is the guard.            |
| R4  | Acknowledge before draining, against I12's wording                        | Swap two statements; mutation 5/6 show which order loses a byte, if either does under QEMU.                                            |
| R5  | A 64-empty-notification storm guard replaces `irq FAIL extra`             | The threshold is a guess. Too low: a false `rx FAIL` on a noisy line — raise it. Deleting the guard costs one marker.                  |
| R6  | Raw, no echo, no line discipline in 6.5                                   | Echo and CR→LF are a few lines in `input.rs`; adding them here would move `stdin ok`'s expected bytes and the feeder string.           |
| R7  | The proof child is forked by init and lives in init's source              | A separate `userland/` binary is a new crate and rootfs entry; move it if init's prologue is judged too crowded.                       |
| R7  | The `read-console` denial probe is deleted, not replaced by another errno | `open.deny`'s count drops by one. Nothing replaces "read on a write-only console" because the console is not write-only any more.      |
| R8  | Input is fed by a script gated on a log marker                            | If log polling proves flaky in CI, the fallback is a fixed delay — weaker (not causal) but one line.                                   |
| R8  | The reboot boot is not fed                                                | Feeding it is one process substitution and three markers in the reboot pair.                                                           |
| R9  | No kernel change                                                          | A finding becomes a kernel task in this slice; the spec's table in §1 is corrected in the same branch.                                 |
| R10 | `EAGAIN` on `CDEV_READ` is not an ABI bump                                | If C in the musl fork ever speaks CDEV directly, a coordinated note in both repos; no value moves either way.                          |

---

## 6. Out of scope

- Echo, canonical mode, `termios`, `VEOF`, signals from the keyboard (`^C`) — 6.8 and Phase 7.
- `O_NONBLOCK`, `select`/`poll`. VFS relaying `EAGAIN` to a non-blocking descriptor is the obvious
  later use of R1 and nothing here precludes it.
- `/dev/tty`, a second console minor, `virtio-console` (6.6).
- Interrupt-driven **TX**. Transmission stays polled.
- `SENDA`. Unblocking it is its own slice if R1 is ever judged wrong.
- Killing a parked reader with a signal: `VFS_PM_EXIT` frees the entry (host-tested), but no boot
  path sends a signal to a process blocked in VFS, and this slice adds none.

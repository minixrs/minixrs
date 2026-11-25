# Slice 6.6 — `virtio-console` as a second CDEV backend — Implementation Plan

**Goal:** a write to `/dev/hvc0` leaves the machine on a second QEMU chardev, bytes fed to that
chardev come back from a `read()` of the node, and a reader parked on the other console is not in
the way.

**Spec:**
[`../specs/2026-11-24-virtio-console-design.md`](../specs/2026-11-24-virtio-console-design.md),
decisions `C1…C14`, under the phase decisions `H1…H12` of
[`../../plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md) and 6.5's `R1…R12`. Where this plan
and the spec disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **Branch `feature/6.6-virtio-console`.** Every commit `--signoff`ed and GPG-signed; never
  `--no-gpg-sign`, never `--no-verify`. **No push, no PR, no CI trigger.**
- **SPDX header** first in every new `.rs` file.
- **No ABI value moves** (D8, C13). `cargo gen-c-headers` output is diffed against `main` and must
  be byte-identical.
- **No kernel change.** If Task 0 or a later task finds one is needed, that is a finding: it goes in
  the ledger and to the user before any kernel code.
- **No polling** (I12): `vcon` blocks in `receive` between requests and while a transmit is in
  flight; VFS never spins on `EAGAIN`.
- **`unsafe` in `virtio-console`:** the two `driver-rt` constructors in `main.rs` and nothing else.
  `bringup.rs` and `port.rs` have none.
- **Overflow-safe arithmetic** for every offset and length derived from a request or from the
  device.
- **Boots.** `$S` is the session scratch directory. Build first (`MINIXRS_SDK=/nonexistent cargo
  build -p minixrs-kernel --target aarch64-unknown-none --release`), then `MINIXRS_SDK=/nonexistent
  MINIXRS_ROOT=<disk|ramdisk> timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none
  --release < <(tools/feed-console.sh $S/<name>.log) > $S/<name>.log 2>&1`. One boot at a time.
  Iterate and mutation-test stub-free (`--no-default-features`), judging by the specific marker.
  Read `check-boot-log.sh`'s whole output; apply the starved-boot check before believing a missing
  marker.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing).
- **Counts in markers come from constants**, never typed into a driver.
- **Markdown** wraps at 100; `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.

## Marker text (fixed here; the kernel's two lines are read from Task 0's boot, not derived)

```text
[virtio] slot=8 pa=<…> id=3 ver=2 drv=vcon        kernel probe (exact text from Task 0)
[dma] vcon va=<…> pages=16                        kernel (exact text from Task 0)
[diag vcon] virtio ok features=0x100000000 rxq=<n> txq=<n> slot=8
[diag vcon] no device                             forbidden
[diag vcon] virtio FAIL step=<step> …             forbidden (substring `[diag vcon] virtio FAIL`)
[diag vcon] rx FAIL notify rc=<rc>                forbidden (substring `[diag vcon] rx FAIL`)
[diag vfs] vcon.ds ok ep=<ep>
[diag vfs] vcon.ds FAIL rc=<rc> fallback=<ep>     forbidden (substring `vcon.ds FAIL`)
[diag vfs] cons.other ok                          a walk passed over another driver's reader, then answered one
minix.rs init: hvc0 ok n=<HVC0_IN.len()>
minix.rs init: hvc0 FAIL step=<step> …            forbidden (substring `hvc0 FAIL`)
```

`HVC0_OUT` is `minix.rs hvc0 out\n` and `HVC0_IN` is `minix.rs hvc0 in\n`; each lives once in
`userland/init`, and a host test there compares them with the strings `tools/feed-console.sh` holds.
The fork-cycle markers move one slot up (the second long-lived child) and are recomputed, not
edited.

**Corrections found in implementation (2026-11-24).** The block above is the text as planned. As
built, and as the marker files hold it:

```text
[as] vcon nr=12 ttbr0_pa=<…>                       expected; not in the list above
[virtio] slot=8 pa=0xa001000 id=3 ver=2 drv=vcon   the exact text Task 4's boot printed
[devmap] vcon va=0x40001000 pa=0xa001000 intid=56  expected; not in the list above
[dma] vcon va=0xc0000000 pages=16                  the exact text (pa= follows and is not asserted)
[diag vcon] virtio ok features=0x100000000 rxq=8 txq=8 slot=8
[diag vcon] ds FAIL rc=<rc>                        forbidden; not in the list above
[ksys SYS_FORK] parent=i nr=10 child_nr=21         expected: a third fork line, the first worker's
[diag vfs] grant.test FAIL send rc=<rc>            forbidden (substring `[diag vfs] grant.test FAIL`)
minix.rs init: hvc0 ok n=17
```

`HVC0_IN` is 17 bytes and `HVC0_OUT` 18. A third shared string, `HVC0_OK` (`minix.rs init: hvc0
ok`), is what the feeder waits for, and the host test pins it too. **Task 5's "in that order" is
wrong**: a fed boot prints `cons.other ok`, then `hvc0 ok`, then `stdin ok n=15`. The fork-cycle
markers name slot 21 for the first worker (19 and 20 are the two proof children). A full boot is 190
checks.

## Review Focus

1. **Advance after the copy** — `off` moves only once `SYS_SAFECOPY` returned `OK`. Pinned by
   `port.rs` host tests and, on a boot, by the child's bad-buffer read (`hvc0 ok`; mutation 1).
2. **Re-post when drained, and only then** — a partial read leaves the buffer held; the re-post and
   the queue notify precede the reply. `port.rs` tests; mutation 2 through the feeder's second
   chunk.
3. **Device-supplied length** — clamped to the buffer; zero re-posts and holds nothing. `port.rs`
   tests with a length above the buffer and with zero.
4. **`CDEV_READ` answer order** — validate, zero-length `0`, broken `EIO`, posted `EAGAIN` with the
   waiter recorded first, held copy. `driver-rt::cdev` tests; the order in `main.rs` by reading.
5. **Both walks on every interrupt, ack first, wake after re-arm** — including the interrupt taken
   inside the transmit wait. By reading against `driver-rt`'s crate docs; mutations 3, 4 and 11.
6. **A used element out of turn ends the device** — a transmit completion with nothing in flight, a
   receive completion while held. `port.rs` tests; the `break` path by reading.
7. **A stopped device wakes its waiter, in both drivers** (C9) — host tests on `read_plan` and on
   TTY's `IrqService`; mutation 12 moves nothing on a boot, which is expected.
8. **The moved code is a move** — `Waiter` / `read_plan` / the validator in `driver-rt::cdev` behave
   as they did in TTY; TTY's tests moved with them and none was dropped. Compare test names before
   and after.
9. **The per-driver walk** — skip another driver's entry; the first `EAGAIN` from the notifier ends
   it; a count or an error frees; `EBADF` for a non-device on any walk; it terminates. The step
   function's host tests, `park.rs`'s in-order walk tests, `cons.other ok`, mutations 7 and 8.
10. **Never reply to a notification** — the widened arm still sits before the dispatch and still
    `continue`s for every source.
11. **Tripwires grown, not left** — `NR_DEV_NODES`, the "uses every driver" test in `dev.rs`, any
    `match` on `CharDriver`, the fork-cycle slot numbers, init's child count and wait-again logic.
12. **The feeder** — `hvc0` first, gated on the guest's own output; two chunks; the serial probe
    gated on `cons.beside ok` **and** `hvc0 ok`; an unfed boot leaves nothing blocked.
13. **Layout arithmetic** — four pages, each queue alone on its page, both buffers in the remainder;
    verify the offsets by hand against `USER_PAGE_SIZE`.

## Tasks

### Task 0: Measure first (C14)

Throwaway changes to `qemu-run.sh` and a throwaway probe driver, never committed; restored from a
scratchpad snapshot. The five items of C14. Results in the ledger; a finding that contradicts the
spec rewrites the spec before Task 1.

### Task 1: R1's pure half moves to `driver-rt`; a stopped TTY line says `EIO` (C6, C9)

**Files:** `drivers/driver-rt/src/cdev.rs` (new), `drivers/driver-rt/src/lib.rs`,
`drivers/driver-rt/Cargo.toml` if a dependency is needed, `drivers/tty/src/{cdev,irq,main}.rs`.

- Move `Waiter`, `ReadPlan` / `read_plan`, `read_len` and the validator (parameterised by minor)
  with their tests. `read_plan`'s third argument becomes "input can still arrive".
- TTY: `IrqService` reports whether receive can still happen; the transition to `Stopped` wakes the
  waiter once.

**Proves it:** `cargo test -p minixrs-driver-rt -p minixrs-tty`; `cargo miri test -p
minixrs-driver-rt`; a fed stub-free boot with every marker unmoved.

### Task 2: VFS — the node, the third driver, the per-driver walk (C10)

**Files:** `kernel-shared/src/callnr.rs`, `servers/vfs/src/{dev,fd,park,main}.rs`.

- `DEV_HVC0_PATH`, `CDEV_MINOR_HVC0`; the fourth row; `CharDriver::Vcon`.
- `park.rs`: the in-order walk. The walk's step function, pure, with tests written first.
- `main.rs`: `vcon_endpoint()` with fallback; the peers struct; the notifier set; the walk;
  `cons.other ok`.
- Inert at boot until Task 4: slot 12 has no ELF yet, so the lookup takes its fallback. `vcon.ds ok`
  is not required until Task 4.

**Proves it:** `cargo test -p minixrs-vfs`; a fed stub-free boot with every prior marker present.

### Task 3: the driver's pure half (C5, C6, C7)

**Files:** `drivers/virtio-console/{Cargo.toml,build.rs,user.ld}`,
`drivers/virtio-console/src/{bringup,port}.rs`, `src/main.rs` reduced to the module lines and the
brand.

**Proves it:** `cargo test -p minixrs-virtio-console`, tests first (Review Focus 1–3, 6, 13).

### Task 4: the driver, packed and attached (C1–C9)

**Files:** `drivers/virtio-console/src/main.rs`, `kernel/build.rs`, `tools/qemu-run.sh`, the common
marker files, `sonar-project.properties` if `main.rs` needs its exclusion.

**Proves it:** an unfed stub-free boot prints the kernel's two lines, `[diag vcon] virtio ok …` and
`[diag vfs] vcon.ds ok`, with every prior marker present.

### Task 5: the proof child and the feeder (C11, C12)

**Files:** `userland/init/src/main.rs`, `tools/feed-console.sh`, marker files.

**Proves it:** a fed stub-free boot prints `hvc0 ok`, `cons.other ok` and `stdin ok n=15`, in that
order; an unfed one prints none of the three and no `FAIL`.

### Task 6: CI

**Files:** `.github/workflows/ci.yml` only if the invocation has to change (C12 says it does not);
`actionlint` / `shellcheck` on what was touched.

### Task 7: recorded boots (spec §3)

Both full musl-flavour boots fed, `check-boot-log.sh` for each, `diff-backends.sh`, the unfed
reboot, the budget reading against Task 0's baseline, the header diff, every gate
[`ci.md`](../../conventions/ci.md) lists as run-locally.

### Task 8: mutations (spec §3)

The spec's thirteen rows, each applied, observed and reverted from a scratchpad snapshot, on fed
stub-free boots. A row that behaves differently is recorded as seen.

### Task 9: documentation, trackers, the whole-branch sweep (spec §4)

Book, conventions, the 6.5 spec's R12 sentence, the phase file, `docs/plan.md`, the stale-claim
grep, then the fresh whole-branch review of the diff.

## Ledger

One entry per task: its commit, what was verified, and every ruling made in it.

- **Task 0 (2026-11-24; no commit of its own — the device was attached through `qemu-run.sh`'s
  pass-through arguments, so nothing was edited).** Two unfed stub-free boots, QEMU 11.0.3,
  Homebrew's edk2, macOS on Apple Silicon. **C14.2 (the pipe):** with `-chardev pipe` over two FIFOs
  and **nobody on either**, QEMU starts and the boot is unchanged — the first of C2's four
  conditions holds here; the other three need a guest driver and are Task 4's first boot. **C14.4
  (the kernel's line):** `[virtio] slot=8 pa=0xa001000 id=3 ver=2 skip=no-module for=vcon` while
  slot 12 has no ELF, and no `[dma] vcon` line; the lines with a module are read at Task 4. **C14.1
  (the firmware):** edk2 **binds the console and writes to it** — 808 bytes reached the host side
  before the kernel banner, `BdsDxe: loading Boot0001 …` and Limine's menu text, one cursor escape
  per character. So the feeder must match `HVC0_OUT` as a line among other output, never "the first
  bytes", and an unfed boot leaves under a kilobyte in the pipe. `Status` at kernel entry is read by
  the driver at Task 4. **Rulings:** (a) **C14.3 is measured on the real driver's first boot, not
  with a throwaway probe** — a probe that enables two queues, transmits and receives is most of the
  driver. *If wrong:* a QEMU behaviour that contradicts C5–C7 is found at Task 4 instead of before
  Task 1, and costs a rewrite of `port.rs`. (b) **C14.5's baseline is taken at Task 7**, from a
  worktree of the merge base, back to back with the slice's own boots, instead of now: the two
  readings are then taken under the same machine load. 6.5 recorded 63.20% (disk) and 41.41%
  (ramdisk) for this merge base. *If wrong:* nothing is lost; the baseline is one boot pair either
  way.

- **Task 1 (`85bf7b8`).** `driver-rt::cdev` holds `Waiter`, `ReadPlan` / `read_plan`, `read_len` and
  `validate_read` / `validate_write(req, minor)`; `drivers/tty/src/cdev.rs` is deleted. All 28 of
  TTY's `cdev::tests` names are present in `driver-rt` unrenamed, plus 3 new; TTY gained 8 `irq`
  tests. Verified: `cargo test` (144 in `driver-rt`, 29 in TTY), clippy `-D warnings`, fmt, `cargo
  miri test -p minixrs-driver-rt` (144 passed), the bare-metal build. **Rulings:** (a) **a stopped
  device answers `EIO` before its buffer is looked at**, so after TTY's line stops, bytes still in
  the ring are unreadable — the order the spec's Review Focus 4 and the moved 6.5 test already
  fixed, now reachable in TTY. *If wrong:* swap two arms of `read_plan`, and `vcon` then has to
  serve a held buffer from a broken device. (b) **a failed proof round now moves TTY to `Stopped`**
  — the spec assumed it already did; it printed its `FAIL` and stayed in `Proof`, and could even
  print `irq ok` after an `irq FAIL bits`. Without this C9 does not cover the failed proof. *If
  wrong:* one line in `proof_round`. (c) the phase transition is a pure `Phase::after(rearm)`,
  host-tested per cause; its four call sites in `irq.rs` are checked by reading. (d) during the
  proof `can_receive` is true: a read parks and is woken by the first byte or by the proof failing.
  (e) `ReadPlan::NoLine` is `Stopped`; the validators take `server_rt::cdev::Request`.

- **Task 2 (`fcd6c98`).** `DEV_HVC0_PATH`, `CDEV_MINOR_HVC0`; `CharDriver::Vcon` and the fourth row;
  `park.rs`'s `oldest_after(Option<seq>)`; a new pure `retry.rs` (`CharPeers`, `step`, `Walk`);
  `main.rs`'s `vcon_endpoint`, notifier arm and per-driver `retry_parked`; `cons.other ok`. 22 new
  host tests, 113 in VFS, 16 seen failing against stubs first. Verified: tests, clippy, fmt, the
  generated headers `diff -r`-identical before and after, the bare-metal build. **Rulings:** (a) the
  step function lives in a new `retry.rs`, not `park.rs`, which stays "the record and the order".
  (b) **`cdev_endpoint` left `main.rs`**: it is `CharPeers::endpoint`, beside `CharPeers::notifier`,
  so the driver-to-endpoint match and the notifier check are host-tested — the spec said the
  function "grows an arm". (c) `CharDriver::ALL`, with an exhaustive-match tripwire, is what the
  "uses every driver" test and the notifier check iterate. (d) **`cons.other ok` needs a real
  `CDEV_READ` answer** behind the pass-over; an `EBADF` step or a grant errno does not count. (e)
  `ParkTable::oldest` is test-only now. *If wrong:* each is local to VFS; (d) is one line.

- **Task 3 (`35cb157`).** The crate in `virtio-blk`'s shape with `bringup.rs` (`vcon_layout`, the
  queue and page constants) and `port.rs` (`Rx`, `Tx`, `Fault`, `tx_len`); `main.rs` is a branded
  skeleton that spins and is not packed. 34 host tests, 17 seen failing against stubs. Verified:
  tests, clippy, fmt, host `cargo check --workspace`, the nested bare-metal build by hand with the
  brand note present. **Rulings:** (a) the layout refuses a run shorter than four whole pages. (b)
  `Rx::advance` by more than is available is a fault that changes nothing, not "drained". (c) `Rx`'s
  fields are public and its methods total for any values. **Carried to Task 4:** the two
  `cfg_attr(not(test), allow(dead_code))` lines (`bringup.rs`, `port.rs`) to delete.

- **Task 4.** The driver (`main.rs`: bring-up, one interrupt handler for both queues, `CDEV_READ`
  from the held buffer, `CDEV_WRITE` waited for), the archive row for proc 12 after `vblk`,
  `qemu-run.sh`'s device on bus 8 over a FIFO pair, six expected and five forbidden lines; the
  common pair now holds 185 checks for a full boot. `port.rs` gained `Irq`, `absent_reply` and
  `wake_due` with 5 host tests (39 in the crate); `driver-rt`'s `DmaBuf` gained `prefix` with 2 (146
  in the crate, Miri clean). Nothing under `kernel/src` changed. Verified, and re-run by the
  coordinator: tests, workspace clippy `-D warnings`, fmt, an unfed stub-free boot with `[diag vcon]
  virtio ok features=0x100000000 rxq=8 txq=8 slot=8` and `[diag vfs] vcon.ds ok ep=12`, the eleven
  stub lines and the unfed `stdin ok` its only misses, nothing forbidden. **The rest of Task 0,
  measured with a self-test since removed** (QEMU 11.0.3): the firmware leaves the device **reset**
  (`Status` 0 at kernel entry); `QueueNumMax` is 1024 for both queues, clamped to 8; queue 0
  receives and queue 1 transmits; a transmit completes with one interrupt **with or without** a
  reader on the host side; the guest's line lands at offset 808 of the host capture, straight after
  the firmware's text; host bytes reach the guest, a 600-byte write arriving as used elements of
  256, 256 and 88 — one per re-post; and **bytes written while the buffer is held are kept**, and
  delivered at the re-post (4.6 s later in the measurement). Nothing contradicts C5–C7, and C2's
  four conditions hold on this machine. Not measured: whether the firmware consumes bytes written
  before the kernel starts (the feeder never writes that early). **Rulings:** (a) `DmaBuf::prefix` —
  a safe, narrowing-only handle — is how a write chains exactly its bytes; the alternative was
  carrying the remainder region in the layout. (b) every break wakes the waiter, wherever it is
  found, through one pure `wake_due`. (c) **a read whose copy succeeded and whose re-post then
  failed replies the count**, not `EIO`: the caller has the bytes; the next request hears `EIO`. (d)
  the `ENXIO` gate (`absent_reply`) is consulted before the request is parsed. (e) a zero-length
  write to a broken device is `0`. (f) two expected lines beyond the spec's table — `[as] vcon
  nr=12` and `[devmap] vcon … intid=56` — and `[diag vcon] ds FAIL` forbidden. (g) `[diag vcon] no
  device` is forbidden in the common file: unlike `vblk`'s, no boot attaches no console. *If wrong:*
  each is a few lines in the driver or a marker line.
- **Task 5.** init's second proof child on `/dev/hvc0` (open, write `HVC0_OUT`, a read into unmapped
  memory expecting `EFAULT`, read `HVC0_IN`, compare, close, `minix.rs init: hvc0 ok n=17`), both
  proof children through one `fork_proof_child` / `report_proof_reap`; `tools/feed-console.sh` in
  two stages (hvc0 first, the serial probe gated on `hvc0 ok` as well); three expected lines
  (`cons.other ok`, `hvc0 ok n=17`, a third `SYS_FORK … child_nr=21`), the worker's slot 20 → 21,
  `hvc0 FAIL` and `[diag vfs] grant.test FAIL` forbidden; 190 checks for a full boot. Verified by
  the coordinator: init/vfs/pm tests, clippy, fmt, shellcheck, and a **fed full disk boot passing
  all 190** with the feeder reporting the hvc0 line at 26 s, hvc0 fed at 32 s, the probe at 36 s.
  Fed stub-free ×3 and unfed stub-free by the agent: order always `cons.other ok`, `hvc0 ok`, `stdin
  ok`; unfed shows none of the three and no `FAIL`. **A defect this slice introduced and this task
  found:** with boot stubs on, PM's four grant lines (`grant.test ok`, `copy ok`, `magic ok`,
  `grant.deny ok`) were missing, 3 boots of 3. VFS's `grant_test` is an `ipc_send` to PM; Task 2's
  fourth DS lookup ahead of it let stub D's fault land first, PM blocked in `VFS_PM_EXIT`, and the
  kernel refused the send, whose result was discarded. Tasks 2–4 were verified stub-free and never
  saw it. Fixed: `grant_test` runs before the lookups, its send result is printed (`grant.test FAIL
  send rc=`), and PM's comment claiming the prologue precedes every exit is corrected. **Spec and
  plan errors:** `cons.other ok` comes *before* `hvc0 ok` (VFS prints it when the answer behind the
  passed-over serial reader is the `EFAULT`), not after as §3's table and the plan's "in that order"
  had it; C12 says nothing of stale FIFO bytes; C11 lists five steps and there are six.
  **Measured:** bytes written to `.in` while the firmware runs are lost; on macOS an unread boot's
  ~826 bytes stay in `.out` after QEMU exits with no holder (`qemu-run.sh`'s comment corrected).
  **Rulings:** (a) the grant-test fix is an ordering fix, not a retry — a stub that faulted before
  VFS's first instruction after `sef_startup` would still win; the forbidden line now names it. (b)
  the feeder discards whatever is in the FIFOs when it starts (`DRAIN=0.5`), so a feeder started
  against an already-running boot can lose the guest's line. (c) the feeder reads `.out` with a
  background `cat` into a temp file and treats its EOF as "QEMU gone", exiting 1. (d) `SETTLE=3`
  before the first hvc0 chunk, so the unmapped read is parked, not refused directly, when the bytes
  arrive — a guess like `GAP`, 3 s per fed boot. (e) the child closes `/dev/hvc0` on every path
  before its verdict, a sixth step `step=close`: exiting with it open printed `pm-exit reset
  closed=1` early and left fileio's marker vacuous. (f) a third shared string `HVC0_OK`, pinned by
  the host test. (g) unfed boots accumulating unread bytes in `.out` is left as a debt, not fixed in
  the runner. *If wrong:* (a) a boot fails loudly with the named line; (b)–(d) are feeder knobs; (e)
  one call; (g) an untested full pipe after ~80 unfed boots with no fed one between.

- **Task 9 (documentation; no boots, no code).** The phase file's 6.6 section, both checkboxes, the
  conventions (`servers-and-drivers.md`, `testing-and-markers.md`, `build-and-boot.md`, `ci.md`,
  `abi.md`), the book (`drivers`, `servers`, `boot`, `build`, `ipc`, `architecture`, `roadmap`), the
  6.5 spec's R12 and a "Corrections found in implementation" section in this slice's spec and in
  this plan's marker list. Tasks 7 and 8 are not recorded by this task: the phase file carries
  `TASK7` / `TASK8` placeholder comments and `build-and-boot.md` a `TASK7: budget` one. **Rulings:**
  (a) **the book's virtio-console text is a section of `drivers/overview.md`, not a new page**: the
  book has one page per chapter and `virtio-blk` is a section of that page, so `SUMMARY.md` is
  unchanged. *If wrong:* move the section to its own file and add one `SUMMARY.md` line; every link
  to `#virtio-console` then needs its path changed. (b) **R12 in the 6.5 spec is rewritten in place
  and carries a dated note of what it said**, which is this spec's §4 and `docs-and-workflow.md`'s
  rule plus an annotation, rather than a note alone. *If wrong:* restore the original sentence above
  the note. (c) H2's "publishes nothing to DS" sentence in the phase file is narrowed to block
  drivers, since C4 contradicts it for `vcon`. (d) Task 5's one fed full disk boot is recorded in
  the phase file as Task 5's, not as a Task 7 result.

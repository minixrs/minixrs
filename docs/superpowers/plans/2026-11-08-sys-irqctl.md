# Slice 6.1 — `SYS_IRQCTL`, the Kernel Idle Path, `HARDWARE` NOTIFY — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a real level-triggered hardware interrupt to an EL0 driver as a `NOTIFY` from
`HARDWARE`, and give the kernel an idle path so "nothing runnable" is a state it survives. TTY's
PL011 TX interrupt is the proof (`[diag tty] irq ok`). The `idle-probe` boot is the idle proof
(`[idle] wake ok`).

**Architecture:** The pure logic lives in `kernel-shared`, where it is host-tested: subcode decode,
range predicates, and the one-owner-per-line hook table. The kernel wraps that table in a static,
implements `SYS_IRQCTL` against it, and delivers interrupts in four steps: mask the SPI, set a
`Priv::int_pending` bit, notify from `HARDWARE`, EOI. When `pick_proc` is empty, `schedule_next`
loops `wfi` → `irq_dispatch()` inside itself. No EL1 IRQ vector and no idle process are involved.

**Tech Stack:** Rust (`no_std` kernel, EL0 driver, `kernel-shared` host tests), GICv3, PL011,
aarch64 QEMU `virt`, `dprint`.

**Spec:** [`../specs/2026-09-29-sys-irqctl-design.md`](../specs/2026-09-29-sys-irqctl-design.md),
decisions `I1…I14`. Read it before Task 1; every task argues from it. Where this plan and the spec
disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **SPDX header** on every new `.rs` file before any other content — see
  [`rust-style.md`](../../conventions/rust-style.md).
- **Every commit `--signoff`ed and GPG-signed.** Never `--no-gpg-sign`, never `--no-verify`.
- **Branch `feature/6.1-irqctl`** (exists; the I14 amendment is its first commit). Never commit to
  `main`. **Do not push, open a PR, or trigger CI.**
- **D8 (I2):** the new `callnr` constants are additive. No C consumer reads them, and the C headers
  are regenerated from Rust (never committed), so there is no fork bump.
- **The kernel is never host-built.** There is no `#[cfg(test)]` under `kernel/src/`, and QEMU is
  the kernel's verification path. Iterate with `MINIXRS_SDK=/nonexistent timeout 60 cargo run -p
  minixrs-kernel --target aarch64-unknown-none --release --no-default-features > $SCRATCH/log 2>&1`.
  For an `idle-probe` boot, add `--features idle-probe`.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing): snapshot to
  the scratchpad first (including files this plan **adds**), restore from the snapshot, `diff -q`
  back, and finish with `grep -rn MUTATION` clean. Run `grep -a 'error\[E' log` before recording any
  observation. Record each mutation and what moved in the task's commit message.
- **Counts in markers are computed from constants or `array.len()`**, never typed in.
- **Markdown wraps at 100 columns**. Run `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.
- **`unsafe` discipline** per [`kernel.md`](../../conventions/kernel.md#unsafe-and-static-tables):
  every `unsafe` block gets a `// SAFETY:` comment, statics use the `UnsafeCell` newtype, and no
  `&mut` into a static is live across a call that re-borrows it.
- **Values pinned by the spec:** subcodes `IRQ_SETPOLICY=1`, `IRQ_RMPOLICY=2`, `IRQ_ENABLE=3`,
  `IRQ_DISABLE=4`. `IRQ_REENABLE=0x001`. Hook ids are 1-based. `NR_IRQ_HOOKS=16`. `notify_id` is in
  `0..=31`. The SPI priority is `0x80`. PL011 is INTID 33. Only SPIs are claimable: `32 <= vector <
  nr_intids()`.

## Review Focus

1. **A deferred `HARDWARE` notification collected after exec.** I8 says `do_exec` must clear
   `HARDWARE`'s bit in the target's `notify_pending`, because `do_exit` zeroes the whole map but
   exec does not. Expected: the new image never sees an empty-bitmap interrupt notify. No boot
   exercises exec of a hook owner. Task 1 pins the logic with the `release_owner` host test, and
   Task 3's review checks the `notify_pending` clear by reading the code.
2. **A replacement `IRQ_SETPOLICY` that fails.** It must leave the old hook untouched: allow-list
   `EPERM` before the table, and `EBUSY` in the table before mutating. Task 1's
   `replace_that_hits_busy_line_leaves_old_hook` test pins it.
3. **The idle loop re-entering itself through `clock::tick` → `reschedule` → `schedule_next`.**
   Expected: no stack growth and no per-tick byte while idling. Task 2's second mutation shows what
   happens without the guard, and the guarded run is the control.
4. **A spurious INTID (1023) in the idle poll.** `wfi` can wake with nothing to acknowledge.
   Expected: `irq_dispatch` returns without an EOI and the loop polls again. Task 2 keeps
   `irq_dispatch`'s existing early return for `INTID_SPURIOUS`, and its review checks that the loop
   does not treat a spurious wake as progress.
5. **`HARDWARE` notifications reaching SEF's classifier.** A non-zero payload from `HARDWARE` must
   stay application traffic, not a Ping or a signal. Task 4 corrects the `classify.rs` comment and
   adds `hardware_notify_is_application_traffic` to its tests.

---

### Task 1: ABI constants and the host-tested IRQ logic in `kernel-shared`

**Files:**

- Modify: `kernel-shared/src/callnr.rs` (beside `SYS_IRQCTL`, line 59)
- Create: `kernel-shared/src/irq.rs`, and register it in `kernel-shared/src/lib.rs`
- Modify: `tools/gen-c-headers/src/callnr_h.rs` (add `define_dec` rows beside the other `*_OFF`
  rows)

**Interfaces:**

- Produces (`callnr`): `IRQ_SETPOLICY: i32 = 1`, `IRQ_RMPOLICY = 2`, `IRQ_ENABLE = 3`,
  `IRQ_DISABLE = 4`, `IRQ_REENABLE: i32 = 0x001`, `IRQCTL_REQUEST_OFF: usize = 0`,
  `IRQCTL_VECTOR_OFF = 4`, `IRQCTL_POLICY_OFF = 8`, `IRQCTL_NOTIFY_ID_OFF = 12`,
  `IRQCTL_HOOK_ID_OFF = 12`, `NOTIFY_IRQ_PENDING_OFF: usize = 0`. Doc each one from I1/I2/I7/I10.
- Produces (`minixrs_kernel_shared::irq`):
  - `pub const NR_IRQ_HOOKS: usize = 16;` `pub const FIRST_SPI: u32 = 32;` `pub const NOTIFY_ID_MAX:
    i32 = 31;`
  - `pub enum IrqRequest { SetPolicy { vector: u32, notify_id: u8 }, RmPolicy { hook_id: i32 },
    Enable { hook_id: i32 }, Disable { hook_id: i32 } }`, deriving `Clone, Copy, Debug, PartialEq,
    Eq`
  - `pub fn decode(request: i32, vector: i32, policy: i32, id: i32, nr_intids: u32) ->
    Result<IrqRequest, i32>`
  - `pub struct IrqHook { pub owner: Endpoint, pub intid: u32, pub notify_id: u8 }` (`owner == NONE`
    means free) and `pub struct Claim { pub hook_id: i32, pub moved_from: Option<u32> }`
  - `pub struct HookTable` with `pub const fn new() -> Self`, `pub fn set_policy(&mut self, owner:
    Endpoint, intid: u32, notify_id: u8) -> Result<Claim, i32>`, `pub fn owned(&self, owner:
    Endpoint, hook_id: i32) -> Result<IrqHook, i32>`, `pub fn remove(&mut self, owner: Endpoint,
    hook_id: i32) -> Result<IrqHook, i32>`, `pub fn by_intid(&self, intid: u32) -> Option<IrqHook>`,
    and `pub fn release_owner(&mut self, owner: Endpoint, mask: impl FnMut(u32)) -> usize`

Semantics: `decode` returns `EINVAL` for an unknown request. For `SetPolicy` it also returns
`EINVAL` for `vector < 32`, `vector >= nr_intids` (compare as `i64`; a negative `vector` is
`EINVAL`), `policy != 0`, or `id` outside `0..=31`. The other three pass `id` through as `hook_id`
unchecked, because the table owns that range check. `set_policy` follows I6 exactly:

- A hook with the same `(owner, notify_id)` is the replace candidate.
- Any *other* hook on `intid` is `EBUSY`. This covers the same owner under a different `notify_id`.
- With no candidate, the first free slot is used, or `ENOSPC` if there is none.
- `hook_id = index + 1`. `moved_from = Some(old)` only when a replacement changes the INTID.

`owned` / `remove` return `EINVAL` for `hook_id` outside `1..=NR_IRQ_HOOKS` or naming a free slot,
and `EPERM` for a slot owned by someone else. `release_owner` frees every hook of `owner`, calls
`mask(intid)` for each, and returns the count.

- [ ] **Step 1: Write the failing tests** in `irq.rs`'s `#[cfg(test)] mod tests`:
  - `decode_rejects_unknown_request` — `decode(5, 33, 0, 0, 288)` and `decode(0, …)` → `Err(EINVAL)`
  - `decode_rejects_sgi_ppi_and_out_of_range` — vectors `0`, `27`, `31`, `288` (with `nr_intids =
    288`), and `-1` → `Err(EINVAL)`. `32` and `287` → `Ok`
  - `decode_rejects_any_policy_bit` — `policy = IRQ_REENABLE`, `0x100`, `0x400` → `Err(EINVAL)`
  - `decode_notify_id_range_is_signed` — `id = -1`, `32` → `Err(EINVAL)`. `0`, `31` → `Ok`
  - `decode_passes_hook_id_through` — `decode(IRQ_ENABLE, 0, 0, 0, 288)` → `Ok(Enable { hook_id: 0
    })`
  - `claim_is_one_based` — first claim on an empty table → `hook_id == 1`, `moved_from == None`
  - `second_owner_on_claimed_line_is_ebusy`, and `same_owner_second_notify_id_is_ebusy`
  - `same_owner_same_notify_id_replaces` — same hook id comes back. Re-claiming on a new INTID gives
    `moved_from == Some(old)`, and `by_intid(old) == None`
  - `replace_that_hits_busy_line_leaves_old_hook` — A holds 33 as id 0 and B holds 34. A re-claims
    id 0 on 34 → `EBUSY`, and `by_intid(33)` is still A's
  - `full_table_is_enospc` — 16 distinct claims succeed and the 17th is `Err(ENOSPC)`
  - `owned_checks_range_free_and_owner` — hook ids `0`, `17`, a free slot → `EINVAL`. Another
    owner's hook → `EPERM`
  - `release_owner_masks_and_frees_only_that_owner` — collect the `mask` calls into a `Vec`. Assert
    exactly the owner's INTIDs are masked, the other owner's hook survives, and the return value
    equals the count

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p minixrs-kernel-shared irq::` Expected: compile failure (`irq` items undefined).

- [ ] **Step 3: Implement the `callnr` constants and `kernel-shared/src/irq.rs`** to the semantics
      above. The table is a plain `[IrqHook; NR_IRQ_HOOKS]` with a linear scan, as I6 says. Add the
      eleven `define_dec` rows to `callnr_h.rs`.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p minixrs-kernel-shared && cargo test -p minixrs-gen-c-headers && cargo
gen-c-headers` Expected: all pass, and the generated `callnr.h` contains `IRQCTL_HOOK_ID_OFF`.

- [ ] **Step 5: Commit**: `feat(kernel-shared): SYS_IRQCTL payload constants and hook-table logic`

---

### Task 2: The kernel idle path (I11) and the opt-in `idle-probe` proof (I14)

Lands first because it is independent of the call: the timer alone wakes it, and RS's existing alarm
is the reason to leave idle.

**Files:**

- Modify: `kernel/src/arch/aarch64/irq.rs` (split `do_irq`)
- Modify: `kernel/src/proc/sched.rs` (`schedule_next` at line 296, `reschedule` at line 358, a new
  `IDLING` flag)
- Modify: `kernel/src/clock.rs` (`tick`, line 76)
- Modify: `kernel/src/arch/aarch64/gic.rs` (`ack`'s `// SAFETY:` contract, line 198)
- Modify: `kernel/Cargo.toml` (`[features]`), `kernel/build.rs` (beside `CARGO_FEATURE_BOOT_STUBS`
  at line 49, and the `extra` match at line 245)
- Modify: `userland/init/Cargo.toml` (a new `[features] idle-probe = []`),
  `userland/init/src/main.rs` (the fork loop at line 279)

**Interfaces:**

- Produces: `pub(crate) unsafe fn irq_dispatch()` in `arch::aarch64::irq`. It holds `do_irq`'s whole
  body, and `do_irq(_frame)` becomes a one-line call to it. Task 4 adds the SPI arm here.
- Produces: `pub fn idling() -> bool` in `proc::sched`, backed by `static IDLING: AtomicBool`.

- [ ] **Step 1: Implement the loop.** In `schedule_next`, replace `None => return` with a loop:
      while `pick_proc()` is `None`, set `IDLING`, execute `wfi`, and call `irq_dispatch()`. On
      exit, if `IDLING` was set, clear it. The first such exit in the boot prints `[idle] wake ok`
      through `Uart`, once, from a one-shot `AtomicBool`. Leave `CURRENT_PROC_NR` untouched while
      idling. Kernel tasks are never enqueued (only `userland.rs` calls `enqueue`), so `pick_proc`
      yields only processes with an address space. Keep it that way.
- [ ] **Step 2: Gate `clock::tick` and `reschedule`.** In `tick`, after `fire_expired_alarms`,
      `return` if `sched::idling()`: no `putc`, no quantum accounting, no `reschedule`. Make the
      first line of `reschedule` `if idling() { return; }`. Rewrite `gic::ack`'s `// SAFETY:` to
      name both callers: IRQ context, and the idle poll at EL1 with `DAIF.I` set.
- [ ] **Step 3: Wire `idle-probe`.** `kernel/Cargo.toml` gets `idle-probe = []` with a comment
      matching `boot-stubs`'s. In `build.rs`, if `CARGO_FEATURE_IDLE_PROBE` and `stubs` are both
      set, `panic!` with a message naming `--no-default-features` and saying why: the stubs spin, so
      the queue never empties. Add the arm `"minixrs-init" if idle_probe => &["--features",
      "idle-probe"]`. In init, under `#[cfg(feature = "idle-probe")]`:
  - count successful forks, and once that reaches `EXEC_TARGETS.len()`, leave the loop
  - reap with `PM_WAIT` until it answers an error (`ECHILD`)
  - print `minix.rs init: idle.park` through the existing console path
  - then `loop { let _ = ipc_receive(ANY, &mut m); }`
- [ ] **Step 4: Verify the default boot is unchanged.** Boot with `--no-default-features` (no
      probe). Expected: `grep -ac '\[idle\] wake ok'` = 0, and the `minix.rs hello: errno ok` and
      `fs.selfcheck ok` markers present.
- [ ] **Step 5: Verify the refusal.** Run `cargo build -p minixrs-kernel --target
      aarch64-unknown-none --release --features idle-probe`. Expected: a build failure carrying the
      `--no-default-features` message.
- [ ] **Step 6: Verify the proof.** Boot with `--no-default-features --features idle-probe`, 60 s.
      Expected: `minix.rs init: idle.park`, then `[idle] wake ok`. Nothing from
      `tests/qemu-boot.forbidden` appears (`grep -aFf` with the comment and blank lines stripped).
      Tick bytes stop after the park, apart from the RS heartbeat's brief bursts.
- [ ] **Step 7: Mutations** under `idle-probe`, each snapshotted, observed, and restored:
  - restore `None => return` → `[idle] wake ok` is absent (record what the log shows instead)
  - delete the `idling()` return in `tick` → record what happens; I14 says observe, not predict
- [ ] **Step 8: Commit**: `feat(kernel): idle path — wfi + polled dispatch when the run queue is
      empty`, with the mutation record in the body.

---

### Task 3: `SYS_IRQCTL` — GIC SPI control, the hook table, the handler, release on exit/exec

**Files:**

- Modify: `kernel/src/arch/aarch64/gic.rs` (new SPI functions, `GICD_TYPER` read in `init`)
- Create: `kernel/src/irq_hooks.rs` (declare it in `main.rs`)
- Create: `kernel/src/system/do_irqctl.rs`, and delete `enosys_stub!(do_irqctl)` from `stubs.rs:25`
- Modify: `kernel/src/system/mod.rs` (move `SYS_IRQCTL` from `dispatch_caller_local`, line 262, to
  the table-taking `match`, beside `SYS_SETGRANT`)
- Modify: `kernel/src/proc/priv_struct.rs` (`pub int_pending: u32`, and `0` in `EMPTY`)
- Modify: `kernel/src/system/do_exit.rs` (around line 196), `kernel/src/system/do_exec.rs` (around
  line 236)
- Modify: `kernel/src/arch/aarch64/userland.rs` (the TTY arm at line 640: push the allow-list
  entry), `kernel/src/arch/aarch64/uart.rs` (`pub const PL011_INTID: u32 = 33;`)
- Modify: `server-rt/src/kcall.rs` (`sys_irqctl`)
- Create: `drivers/tty/src/irq.rs`. Modify `drivers/tty/src/main.rs` and `drivers/tty/src/pl011.rs`
  (`pub const PL011_INTID: u32 = 33;`, duplicated with the same cross-reference comment the file
  already uses for its other constants)
- Modify: `tests/qemu-boot.expected`, `tests/qemu-boot.forbidden`

**Interfaces:**

- Consumes: Task 1's `decode`, `HookTable`, `Claim`, and the `IRQCTL_*` offsets.
- Produces (`gic`): `pub unsafe fn enable_spi(intid: u32, priority: u8)`, which programs
  `GICD_IGROUPR` (group 1 NS), `GICD_IPRIORITYR`, `GICD_ICFGR` (level, `0b00`) and `GICD_IROUTER`
  (`0x6000 + 8 * intid`, value 0), then sets `GICD_ISENABLER`. Also `pub unsafe fn mask_spi(intid:
  u32)` (`GICD_ICENABLER`, then wait for `GICD_CTLR.RWP` to clear), `pub unsafe fn unmask_spi(intid:
  u32)`, and `pub fn nr_intids() -> u32`. The last returns `min(32 * (GICD_TYPER[4:0] + 1), 1020)`
  and is stored in `init`.
- Produces (`irq_hooks`): `static IRQ_HOOKS` (an `UnsafeCell<HookTable>` newtype), plus `pub(crate)
  fn lookup(intid: u32) -> Option<IrqHook>` and `pub(crate) fn release_owner(owner: Endpoint) ->
  usize`. The latter masks each line through `gic::mask_spi`. `do_irqctl` reaches the table through
  `pub(crate) unsafe fn table_mut() -> &'static mut HookTable`.
- Produces: `pub(super) fn do_irqctl(caller_priv: &mut Priv, caller_nr: ProcNr, caller_e: Endpoint,
  msg: &mut Message) -> i32`.
- Produces (`server-rt`): `pub fn sys_irqctl(request: i32, vector: i32, policy: i32, id: i32) ->
  (i32, i32)`, returning `(result, reply word at IRQCTL_HOOK_ID_OFF)`.

Handler order:

1. `decode(…, gic::nr_intids())`.
2. A shared slot (`caller_priv.proc_nr != Some(caller_nr)`) → `EPERM` (I5).
3. `SetPolicy`:
   - a vector not in `caller_priv.irqs` → `EPERM`
   - `set_policy`
   - if `moved_from`, `mask_spi(old)`
   - `enable_spi(intid, 0x80)`
   - write `hook_id` at `IRQCTL_HOOK_ID_OFF`
4. `RmPolicy`: `remove`, then `mask_spi`.
5. `Enable` / `Disable`: `owned`, then `unmask_spi` / `mask_spi`.

`do_exit` and `do_exec` call `irq_hooks::release_owner(target endpoint)` and zero `int_pending`.
Exec additionally clears `HARDWARE`'s priv bit in the target's `notify_pending` (I8). Doc comments
cite I8, in the same style as the grant-table resets beside them.

**TTY's half (this task):** at startup, after `sef_startup`, claim `PL011_INTID` with `notify_id
TTY_IRQ_NOTIFY_ID = 0`. A failed claim prints `[diag tty] irq FAIL claim rc=<rc>`. Then run the
denial battery: a `const` table of `(request, vector, policy, id, expected_rc)` cases, each run
through `sys_irqctl`. Print `[diag tty] irq.deny ok n=<cases.len()>`, or `[diag tty] irq.deny FAIL
case=<i> rc=<rc>` on the first mismatch. Cases, in order:

| Case                                    | Expected |
| --------------------------------------- | -------- |
| SETPOLICY vector 34                     | `EPERM`  |
| SETPOLICY vector 27                     | `EINVAL` |
| SETPOLICY vector 1020                   | `EINVAL` |
| SETPOLICY 33, policy `IRQ_REENABLE`     | `EINVAL` |
| SETPOLICY 33, notify_id 1               | `EBUSY`  |
| SETPOLICY 33, notify_id 32              | `EINVAL` |
| SETPOLICY 33, notify_id -1              | `EINVAL` |
| ENABLE hook 0                           | `EINVAL` |
| ENABLE hook `NR_IRQ_HOOKS + 1`          | `EINVAL` |
| ENABLE hook `claimed + 1` (a free slot) | `EINVAL` |
| request 99                              | `EINVAL` |

Then one positive case: SETPOLICY 33 with notify_id 0 again must return `OK` (the I6 replacement),
and TTY adopts the returned hook id. Count it in `n`. TTY does **not** set `TXIM` in this task.

- [ ] **Step 1: Add the markers first.** Add `[diag tty] irq.deny ok n=` to
      `tests/qemu-boot.expected` (the substring stops before the count, per the file's rule). Add
      `[diag tty] irq.deny FAIL` and `[diag tty] irq FAIL` to `tests/qemu-boot.forbidden`. Boot and
      confirm `check-boot-log.sh` reports `irq.deny ok` MISSING.
- [ ] **Step 2: Implement** the GIC functions, `irq_hooks.rs`, `do_irqctl.rs` and the dispatch move.
      Keep the `NR_KERN_CALLS == 18` const assert and update its comment list. Add `int_pending`,
      the exit/exec release, the TTY allow-list push (via `priv_slot_mut` on TTY's priv id), and
      `sys_irqctl`. Add `drivers/tty/src/irq.rs` with `pub fn claim_and_probe() -> i32`, which
      returns the adopted hook id or a negative errno.
- [ ] **Step 3: Verify the battery.** Run a stub-free boot. Expected: `[diag tty] irq.deny ok n=12`
      (the number printed must equal the table length plus the positive case), no `irq FAIL`, and no
      `do_irq: unexpected INTID`.
- [ ] **Step 4: Run the lint gate:** `cargo clippy -p minixrs-kernel --target aarch64-unknown-none
      --release -- -D warnings` and `cargo test -p minixrs-server-rt`. Expected: clean.
- [ ] **Step 5: Mutation.** Drop the allow-list check → `irq.deny FAIL case=0 rc=` appears (vector
      34 is now claimed). Restore per the discipline.
- [ ] **Step 6: Commit**: `feat(kernel): SYS_IRQCTL — hook table, GIC SPI control, allow-list,
      release on exit/exec`, with the mutation record.

---

### Task 4: Delivery — the hooked-SPI arm, `deliver_irq`, the bitmap payload, TTY's two-round proof

**Files:**

- Modify: `kernel/src/arch/aarch64/irq.rs` (`irq_dispatch`'s `other` arm)
- Modify: `kernel/src/ipc/notify.rs` (`build_notify_message` at line 41, new `deliver_irq`),
  `kernel/src/ipc/mod.rs` (new `fire_irq` beside `fire_expired_alarms`, line 401),
  `kernel/src/ipc/receive.rs` (line 77)
- Modify: `server-rt/src/classify.rs` (the "NOTIFY carries no payload" doc, line 36; plus a test)
- Modify: `drivers/tty/src/irq.rs`, `drivers/tty/src/main.rs` (the receive loop, line 114),
  `drivers/tty/src/pl011.rs` (`IMSC_OFFSET = 0x038`, `IMSC_TXIM = 1 << 5`, `pub fn set_txim(on:
  bool)` as a read-modify-write)
- Modify: `tests/qemu-boot.expected`

**Interfaces:**

- Consumes: Task 3's `irq_hooks::lookup`, `gic::mask_spi`, `Priv::int_pending`, `sys_irqctl`, and
  TTY's adopted hook id. Task 2's `irq_dispatch`.
- Produces: `build_notify_message(dst: &mut Proc, source_e: Endpoint, irq_pending: u32)`. This
  writes `irq_pending` little-endian at `NOTIFY_IRQ_PENDING_OFF`. The three existing callers pass
  `0`.
- Produces: `pub(crate) fn deliver_irq(proc_table, priv_table, owner_idx: usize)`, in the
  `deliver_alarm` shape: source `boot_endpoint(HARDWARE)` and no `ipc_to` check. The immediate path
  passes `core::mem::take(&mut priv.int_pending)`. Otherwise it sets `HARDWARE`'s priv bit in the
  owner's `notify_pending`.
- Produces: `pub fn fire_irq(owner_e: Endpoint, notify_id: u8)` in `ipc`. It materializes the tables
  like `fire_expired_alarms`, resolves `owner_e` with `okendpt`, and silently drops a stale owner
  (unreachable given I8). It sets bit `notify_id` in the owner's `int_pending`, then calls
  `deliver_irq`.

`receive.rs`: when `take_pending_notification` yields `boot_endpoint(HARDWARE)`, pass the caller's
`int_pending` (taken, so it is zeroed); otherwise pass `0`. `irq_dispatch`'s arm, for any non-timer
INTID:

- With a hook: `gic::mask_spi(intid)`, then `crate::ipc::fire_irq(h.owner, h.notify_id)`.
- Without one: keep the line starting `do_irq: unexpected INTID` (the forbidden substring) but drop
  "(only PPI 27 is configured)". If `intid >= FIRST_SPI`, also `mask_spi`.

The existing EOI stays last.

**TTY's proof (I13):** after `claim_and_probe`, `pl011::set_txim(true)`. In `main`'s loop, before
the request `match`, a message with `m_source == boot_endpoint(HARDWARE) && m_type ==
NOTIFY_MESSAGE` goes to `irq::on_notify(bits)` and then `continue`, with no reply. `on_notify`:

- a missing `TTY_IRQ_NOTIFY_ID` bit → `[diag tty] irq FAIL bits=<bits:#x>`
- otherwise, `set_txim(false)`, then `sys_irqctl(IRQ_ENABLE, 0, 0, hook)`, then `rounds += 1`
- `rounds < IRQ_PROOF_ROUNDS` (`const = 2`) → `set_txim(true)`
- `rounds == IRQ_PROOF_ROUNDS` → `[diag tty] irq ok rounds=<IRQ_PROOF_ROUNDS>`, with `TXIM` left
  clear (I13)

- [ ] **Step 1: Write the failing host test** `hardware_notify_is_application_traffic` in
      `classify.rs`. It builds a `NOTIFY_MESSAGE` from `boot_endpoint(HARDWARE)` with payload `[1,
      0, 0, 0, …]` and asserts that `classify` returns the application variant, not `Ping` or a
      signal. Run `cargo test -p minixrs-server-rt classify`. Expected: PASS already; it pins
      today's behaviour, which this task relies on. Fix the doc comment.
- [ ] **Step 2: Add the marker** `[diag tty] irq ok rounds=` to `tests/qemu-boot.expected`. Boot and
      confirm it is MISSING.
- [ ] **Step 3: Implement** the kernel delivery path and TTY's proof as above.
- [ ] **Step 4: Verify the stub-free boot.** Expected: `[diag tty] irq ok rounds=2`, `irq.deny ok`,
      no `do_irq: unexpected INTID`, and all `fs.*` / `hello` markers present.
- [ ] **Step 5: Verify the idle-probe boot.** Expected: `[diag tty] irq ok`, `minix.rs init:
      idle.park` and `[idle] wake ok`, all in one log. That is I14's combined proof.
- [ ] **Step 6: Mutations (I13)**, stub-free, each snapshotted, observed, restored:
  - drop `mask_spi` in the hooked arm → the boot storms and `irq ok` never prints
  - drop TTY's `IRQ_ENABLE` → round 2 never arrives, so `irq ok` is absent
  - replace `set_txim(false)` in `on_notify` with a write of `UARTICR.TXIC` (`ICR_OFFSET = 0x044`,
    bit 5) → the boot storms
  - make the immediate path pass `0` instead of taking `int_pending` → `irq FAIL bits=0x0`

  The fourth mutation is an extra beyond I13's four. It proves the bitmap is what TTY reads.
- [ ] **Step 7: Commit**: `feat(kernel): deliver hooked SPIs as HARDWARE notifications; TTY proves
      two rounds`, with the mutation record.

---

### Task 5: Documentation, trackers, and full verification

**Files:**

- Modify: `book/src/drivers/overview.md` (the sentence at line 194, plus a new section, "Interrupts
  reach a driver as a `HARDWARE` notification": the I12 driver pattern, mask-before-EOI, and
  identifying the source by `m_source`)
- Modify: `book/src/reference/syscalls.md` line 47 (`SYS_IRQCTL` → live, with the four subcodes)
- Modify: `book/src/ipc/overview.md` (a `NOTIFY` from `HARDWARE` carries the `int_pending` bitmap at
  payload `0..4`; every other notification is all-zero)
- Modify: `docs/conventions/kernel.md`. A new `## Interrupts and the idle loop` section, a few
  bullets each citing the spec:
  - one owner per line
  - mask before EOI
  - the idle loop never returns towards EL0 without a picked process
  - `tick` does only timekeeping while idling
  - release on exit/exec
- Modify: `docs/conventions/servers-and-drivers.md` (one bullet: a driver identifies an interrupt by
  `m_source == boot_endpoint(HARDWARE)`, never by payload, and re-arms with `IRQ_ENABLE` only after
  acknowledging at the device)
- Modify: `docs/conventions/build-and-boot.md` (the `idle-probe` boot command and its refusal with
  `boot-stubs`)
- Modify: `docs/plan.md` and `docs/plans/phase-6-virtio.md` (check **6.1**, in both its places in
  the phase file, linking this plan)

- [ ] **Step 1: Write the docs**, derived from the code as it now stands (book-first rule). Run
      `~/.dprint/bin/dprint fmt && ~/.dprint/bin/dprint check && python3 tools/check-md-links.py`.
      Expected: clean, 0 broken.
- [ ] **Step 2: Run the local CI gate list** from [`ci.md`](../../conventions/ci.md): `cargo fmt
      --all --check`, the workspace clippy, the kernel clippy, `cargo test` for `kernel-shared`,
      `server-rt` and `gen-c-headers`, and `tools/check-dco.sh main..HEAD`. Expected: all clean.
- [ ] **Step 3: Run the full default boot:** `MINIXRS_SDK=/nonexistent timeout 300 cargo run -p
      minixrs-kernel --target aarch64-unknown-none --release > $SCRATCH/full.log 2>&1;
      tools/check-boot-log.sh $SCRATCH/full.log`. Expected: PASS, exit 124 from the timeout.
- [ ] **Step 4: Check the boot budget.** For `minix.rs hello: errno ok`, compute `grep -abo … | head
      -1` ÷ `wc -c`, against the same number on `main`. Expected: no meaningful rise, since the TTY
      proof runs at startup. Record both numbers in the commit body. If the fraction has climbed,
      apply `ci.md`'s rule before raising any timeout.
- [ ] **Step 5: Sweep.** `grep -rn MUTATION` and `git status` must show nothing untracked or leaked.
- [ ] **Step 6: Commit**: `docs(6.1): interrupts and the idle loop in book/ and conventions; check
      6.1`. Then stop. The push and the PR need the user's approval and the pre-PR checklist.

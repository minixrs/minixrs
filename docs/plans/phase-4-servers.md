# Phase 4: Core Servers (PM, VFS, RS, DS, SCHED) + init — slice history

Full per-slice record for Phase 4, moved verbatim from `docs/plan.md` when it was
restructured into a lean tracker (2026-07-23). Status summary and milestone live in
[`../plan.md`](../plan.md).

---

Phase 4 is split into 8 PR-sized slices (the decomposition was originally
tracked in a local planning file, since retired — this file is the durable
record). Each slice independently
builds, boots, and prints observable progress — same cadence as Phases 2–3. The
Phase 4 milestone ("Full server boot sequence completes; init process starts")
is satisfied at the end of slice 4.8.

Two scope decisions shape the phase: **exec is real but boot-embedded** (no
filesystem/musl until Phase 5, so `SYS_EXEC` loads ELF binaries packed into the
boot-image archive — the same archive that loads the servers — and Phase 5 later
swaps the source to a VFS file with no PM/kernel rework); and **scheduling moves
to a real user-space SCHED** by making the kernel scheduler *delegatable* rather
than replacing it (a per-proc `scheduler` endpoint defaults to kernel-scheduled,
and on quantum exhaustion the kernel either requeues or notifies the proc's
user-space scheduler). The boot `IMAGE` in `kernel/src/proc/table.rs` already
lists pm/vfs/rs/ds/sched/init with correct priv flags, and `init_boot_image`
already fills their `ipc_to` / `k_call_mask`, so loading a server needs only an
ELF + the generalized `load_boot_server` path — no new boot priv wiring.

- **Slice 4.1** ✓ shipped (PR #23, merged 2026-06-14) —
  `server-rt` SEF framework + migrate VM onto it + finish
  `minix-ipc`. Add `ipc_notify` / `ipc_sendnb` (new SVC `primitive` values).
  Build `server-rt`: `sef_startup()` (learn own endpoint/name via
  `SYS_GETINFO(GET_WHOAMI)`, run `init_fresh` callback) and `sef_receive()`
  (wrap `ipc_receive(ANY, …)`, intercept SEF control messages — ping/signal/init
  — and return only application messages), with static function-pointer callback
  registration (no heap; minimal subset vs MINIX `lib/libsys/sef.c`). Port
  `servers/vm` to the SEF loop (handlers unchanged) so VM is live regression
  coverage for the framework before any new server depends on it.
- **Slice 4.2** ✓ shipped (PR #24, merged 2026-06-21) — Multi-module boot image
  + DS server + VFS skeletal boot.
  `kernel/build.rs`'s single-VM embed is generalized into `build_server(name,
  dir, …)` + `pack_mxbi(...)`: it builds each server (VM/DS/VFS) into its own
  isolated `CARGO_TARGET_DIR`, packs the ELFs into one MXBI archive in `OUT_DIR`
  (16-byte header `magic "MXBI"/ver/count/total_size` + 32-byte records
  `{proc_nr:i32, offset:u32, len:u32, name:[u8;20]}` + back-to-back payloads, all
  LE), and emits `BOOT_IMAGE_PATH` (replacing `VM_ELF_PATH`). `boot_image/mod.rs`
  becomes a zero-copy `BootImage` view (`include_bytes!` of the archive) exposing
  `iter()` → `(ProcNr, &[u8])` for the load loop plus `module_by_proc_nr` /
  `module_by_name` (the latter `#[allow(dead_code)]` until exec in 4.7); the whole
  module stays `cfg(target_os="none")` so host `cargo check`/`test` (where
  `BOOT_IMAGE_PATH` is unset) never evaluate the `env!`. `userland.rs::vm_bootstrap`
  is refactored into `load_boot_server(nr, elf, stack_va)` looped over
  `BootImage::iter()`; all servers share one `SERVER_STACK_VA` (each has its own
  TTBR0). No new boot priv wiring — `init_boot_image` already grants DS(5)/VFS(1)/
  VM(7) `SRV_T` `ipc_to` over `[0, n_active)`. New `kernel-shared/callnr.rs`
  `DS_RQ_BASE = 0xE00` + `DS_PUBLISH`/`DS_RETRIEVE`/`DS_CHECK` + `NR_DS_REQUESTS`
  (const-asserted distinct from VM/SEF, below `NOTIFY_MESSAGE`) + host tests. DS
  (`servers/ds`): a static `[Entry; 16]` name→endpoint registry
  (`servers/ds/src/registry.rs`, `UnsafeCell` newtype like `vm/region.rs`, pure
  `publish/retrieve/check` host-tested) driven by a SEF loop; key = 16-byte
  NUL-padded server name in payload `0..16`, endpoint i32 in `16..20`. DS seeds
  its *own* entry in-process in `ds_init` (a self-SENDREC would deadlock). VFS
  (`servers/vfs`): skeletal SEF boot that drops application traffic. A shared
  `server-rt::sef_publish_to_ds(endpoint, name)` helper (`init_fresh` body for
  VM + VFS, coverage-excluded with `sef.rs`) marshals `DS_PUBLISH`. Verified in
  QEMU over 10 s: seven `[as]` lines (vm/ds/vfs asid 1–3, stubs A–D asid 4–7);
  head `[ipc]` shows VM→SYSTEM + VM→DS publish (DS replies VM), DS→SYSTEM +
  RECEIVE(ANY) (no DS self-publish), VFS→SYSTEM + VFS→DS publish, then A↔B
  ping-pong; stub C `SYS_GETINFO` (`[ksys] call=0`) and stub D's three `[pf]`
  (brk×2 + mmap) resolved by VM (`VMCTL_PT_MAP`×3 + one `VMCTL_PT_UNMAP`) all
  intact; 3319 `[ipc]` + 3310 `[ksys]` + 3 `[pf]`, every line `result=0`; zero
  panic / `el0_sync_unexpected`. Host: `cargo test -p minixrs-kernel-shared`
  (DS callnr) + `-p minixrs-ds` (registry) green; `cargo check --workspace` +
  clippy `-D warnings` + fmt clean.
- **Slice 4.3** ✓ shipped (PR #25, merged 2026-06-27) — Real
  user-space scheduling: kernel delegatable scheduler + SCHED server (single PR).
  `Proc` gains `scheduler: Endpoint` (`NONE` = kernel-scheduled, the boot
  default; `populate_proc` sets it and `Proc::EMPTY` zeroes it to `NONE`).
  `sched::reschedule` branches on it: `NONE` keeps the slice-2.4 refill+rotate;
  otherwise it dequeues the preempted proc (which `clock::tick`'s bare `fetch_or`
  left enqueued), leaves `RTS_NO_QUANTUM` set, and sends `SCHEDULING_NO_QUANTUM`
  to the scheduler via new `ipc::send::mini_sched_no_quantum_send` (a near-clone
  of 3.4's `mini_pf_send`, wrapped by `ipc::send_no_quantum` which materializes
  the proc-table slice like `send_pagefault_to_vm`). `SYS_SCHEDULE` becomes real
  and a new `SYS_SCHEDCTL` lands (`kernel/src/system/do_schedule.rs`), both
  target-taking and routed beside `SYS_VMCTL` in `kernel_call_dispatch`:
  `do_schedule` sets a target's priority/quantum and re-admits it
  (`rts_unset(RTS_NO_QUANTUM)` if off-queue, else dequeue+enqueue for the band
  move); `do_schedctl` claims (`scheduler = caller`) or releases
  (`SCHEDCTL_FLAG_KERNEL` → `NONE`) a target. `kernel-shared/callnr.rs` gains
  `SYS_SCHEDCTL`, `SCHEDCTL_FLAG_KERNEL`, `NR_KERN_CALLS_PHASE4 = 15` (one-slice
  `_PHASE3` alias, dropped in 4.4), and a `SCHED_RQ_BASE = 0xF00` range
  (`SCHEDULING_NO_QUANTUM`/`START`/`STOP`/`SET_NICE`, `NR_SCHED_MSGS = 4`,
  const-asserted distinct from VM/SEF/DS and below `NOTIFY_MESSAGE`) + host
  tests; `init_boot_image`'s `k_call_mask` fill widens to `NR_KERN_CALLS_PHASE4`
  so SCHED may issue the two calls. SCHED (`servers/sched`, server-rt based):
  SEF loop publishing to DS, handlers for all four `SCHEDULING_*`; the policy is
  a round-robin quantum refresh at a fixed managed band (`USER_Q = 8`, the
  boot-server band, so a CPU-bound managed proc round-robins instead of sinking
  behind the kernel-scheduled stubs) held in a static `[SchedProc; 16]`
  `UnsafeCell` table (`servers/sched/src/policy.rs`, pure helpers host-tested
  like `ds/registry.rs`). MINIX-style priority aging (drop a band + periodic
  `balance_queues` boost) is deferred to 4.4's `SYS_SETALARM` — without the boost,
  dropping a band would starve the managed proc behind the band-8 stubs.
  `SCHEDULING_START`/`STOP`/`SET_NICE` are implemented for PM/RS to drive from
  4.5+; in 4.3 the kernel pre-delegates stub C (`userland.rs` sets its
  `scheduler = boot_endpoint(SCHED_PROC_NR)`), the live exercise. Added to the
  MXBI `servers` array in `build.rs` (proc_nr 9). Verified in QEMU over 10 s:
  eight `[as]` lines (vm/ds/vfs/sched asid 1–4, stubs A–D asid 5–8); SCHED boots
  through SEF (`[ipc 10/11] caller=9` GET_WHOAMI + DS publish); head traces show
  the round-trip — `[noq N] proc=C nr=13 -> scheduler=0x9` (kernel delegates) and
  `[ksys SYS_SCHEDULE] target=C nr=13 prio=8 quantum=5 result=0` (SCHED
  re-admits); stub C sustains (`[ksys]` `caller=13` to sample 250 800), A↔B
  ping-pong + D's three `[pf]` (brk×2 + mmap) resolved by VM all intact; every
  line `result=0`; zero panic / `el0_sync_unexpected`. Host: `cargo test
  -p minixrs-kernel-shared -p minixrs-sched -p minixrs-server-rt` green; `cargo
  check --workspace` + clippy `-D warnings` + fmt clean.
- **Slice 4.4** ✓ shipped (PR #26, merged 2026-06-27) —
  RS (reincarnation server) + real `SYS_SETALARM`. `SYS_SETALARM` replaces its
  slice-2.6 `ENOSYS` stub with a per-proc one-shot timer: `Proc` gains
  `alarm_at: u64` (absolute uptime tick, 0 = disarmed; `Proc::EMPTY` zeroes it),
  and `kernel/src/system/do_setalarm.rs` (caller-local, like the other
  non-target `SYS_*`) reads a relative `delta` (u64, payload `0..8`), stores
  `alarm_at = uptime()+delta` (disarms on `delta==0`), and replies the previous
  timer's remaining ticks. `clock.rs` gains an `EARLIEST_ALARM` fast-path gate
  (`arm_alarm`/`set_earliest_alarm`) so `tick()` stays O(1) and only pays the
  O(N) scan when an alarm is actually due; the scan + delivery live in
  `ipc::fire_expired_alarms` (a kernel-originated-delivery wrapper beside
  `send_pagefault_to_vm`/`send_no_quantum`), which clears each expired
  `alarm_at`, calls `ipc::notify::deliver_alarm`, traces `[alarm N]` (head
  carve-out + modulo, like `TRACE_HEAD`), and recomputes the next-earliest.
  `deliver_alarm` is a kernel-originated `NOTIFY` from `CLOCK` with **no `ipc_to`
  check** (CLOCK's bitmap is empty, so routing through `mini_notify` would deny
  it) — immediate when the owner is `RECEIVE`-blocked, else deferred via
  `notify_pending` against CLOCK's priv slot (drained by `mini_receive`). No new
  kernel-shared constants — the alarm reuses `NOTIFY_MESSAGE` + `CLOCK`, and
  `NR_KERN_CALLS_PHASE4` stays 15. RS (`servers/rs`, server-rt based, already
  `ROOT_SYS_PROC` + `sig_mgr` with full priv wiring from `init_boot_image`) boots
  through SEF, publishes to DS, arms a periodic alarm (`ALARM_PERIOD = 100`
  ticks), and on each fire pings a static peer set (DS/VM/SCHED/VFS via
  `boot_endpoint`) with `ipc_notify`, tallying acks in a host-tested
  `servers/rs/src/monitor.rs` (`UnsafeCell` newtype like `sched/policy.rs`);
  restart-on-crash is detect-only (the `monitor::sweep` dead count — EL0 can't
  log and exec is a later slice). The RS heartbeat reuses the existing SEF ping
  (peers ack via `server-rt`'s `sef.receive`); the alarm `NOTIFY` from CLOCK is
  classified `Application` (source ≠ RS), so RS's loop keys on
  `m_source == boot_endpoint(CLOCK)`. Added to the MXBI `servers` array in
  `build.rs` (proc_nr 2); no new boot priv wiring. Verified in QEMU over 25 s:
  nine `[as]` lines (vm/ds/vfs/sched/rs asid 1–5, stubs A–D asid 6–9); head
  `[ipc]` shows RS GET_WHOAMI + DS publish (`[ipc 3/4]`), DS reply to RS
  (`[ipc 10]`), and RS's first heartbeat pings to DS/VM (`[ipc 11/12]`); six
  periodic `[alarm N] owner=r nr=2 at=100..600` fires; stub D's three `[pf]` →
  VM, SCHED `[noq]` delegation, and A↔B ping-pong + C `SYS_GETINFO` all intact;
  every line `result=0`; zero panic / `el0_sync_unexpected`. Host: `cargo test
  -p minixrs-rs -p minixrs-kernel-shared -p minixrs-server-rt` green; `cargo
  check --workspace` + clippy `-D warnings` + fmt clean.
- **Slice 4.5** ✓ shipped (PR #27, merged 2026-07-17) —
  PM part A: mproc + getpid + real `SYS_PRIVCTL` + minimal signals
  (kernel-mediated, MINIX-faithful). The kernel gains the signal trio
  `SYS_KILL`/`SYS_GETKSIG`/`SYS_ENDKSIG` (`0x60F..0x611`;
  `NR_KERN_CALLS_PHASE4` 15 → 18, overdue `_PHASE3` alias dropped) in
  `kernel/src/system/do_sig.rs`: `do_kill` validates and calls `cause_sig`,
  which records the signal in a new per-proc bitmap (`Proc::sig_pending`),
  sets `RTS_SIGNALED | RTS_SIG_PENDING` (the reserved flags, first real use),
  and wakes PM with a kernel-originated `NOTIFY` from `SYSTEM`
  (`ipc::notify::deliver_ksig`, a `deliver_alarm` clone — SYSTEM's `ipc_to` is
  empty so `mini_notify` would deny it; `do_kill`'s deferred-notify write is
  why `kernel_call_dispatch`'s priv param went `&mut`). PM drains with
  `SYS_GETKSIG` (hands off the bitmap — the scan requires `sig_pending != 0`,
  not just the RTS bit, so a handed-off proc isn't re-returned — and keeps RTS
  state) and acknowledges with `SYS_ENDKSIG` (clears it; PM must ENDKSIG
  *every* returned endpoint, after any terminate). Terminate itself is
  `SYS_EXIT`-lite (`do_exit.rs`, target-taking): dequeue via
  `rts_set(RTS_PROC_STOP)`, `alarm_at = 0`, and a `caller_q` unlink of the one
  chain named by `sendto_e` when the target died SENDING — AS teardown / slot
  free / generation bump stay with 4.6. `SYS_PRIVCTL` becomes real
  (`do_privctl.rs`): sole subcode `PRIVCTL_SET_USER` points a
  `RTS_NO_PRIV`-frozen target at the new shared USER priv slot
  (`table::USER_PRIV_ID` = 20: `USR_T`, `ipc_to` = {PM}, empty `k_call_mask`,
  `sig_mgr` = PM; `populate_user_priv` also opens the reverse PM → 20 edge,
  `install_stub_d_priv` pattern) and releases it (`EPERM` on a live target —
  the freeze gate doubles as authorization). PM (`servers/pm`, MXBI row 6,
  proc 0) boots through SEF, publishes to DS, seeds a host-tested
  `mproc.rs` table (pids: PM 0, INIT 1, servers 2..10 slot-order parented to
  RS, stubs 11..15 parented to INIT; stub proc nrs now shared in `com.rs` as
  `STUB_A..E_PROC_NR`), releases the new frozen stub E, serves `PM_GETPID`
  (new `PM_RQ_BASE = 0x700` — the last free block below `VM_RQ_BASE`; reply
  `m_type` *is* the pid, ppid in payload `0..4`), and on `NOTIFY` from
  `SYSTEM` runs the drain with default-terminate for user procs
  (`MF_PRIV_PROC` servers are skipped — sig2mess waits for RS restarts).
  Signal numbers live in new `kernel-shared/src/signal.rs` (POSIX values).
  Live demo: stub E is built *frozen* (`build_stub` gains `Option<PrivId>` +
  `frozen`; no priv slot, not enqueued) and PM's init unfreezes it into a
  `SENDREC PM_GETPID` loop; stub D, after 32 steady-loop iterations, touches
  its munmapped mmap page — VM's out-of-region arm now raises
  `SYS_KILL(faulter, SIGSEGV)` instead of the slice-3.5 silent return. RS
  heartbeats PM as a fifth peer; `ipc::TRACE_HEAD` widened 12 → 24 (six
  servers' boot chatter). Verified in QEMU over 25 s: eleven `[as]` lines
  (vm/ds/vfs/sched/rs/pm asid 1–6, stubs A–E asid 7–11);
  `[ksys SYS_PRIVCTL] target=E nr=15 subcode=1 result=0` exactly once; E's
  getpid SENDRECs recur in sampled `[ipc]` (`caller=15 … target=0x0`); stub
  D's three resolved `[pf]` + one fatal at the munmapped `far=0x2000000`,
  then the full chain in order — `[ksys SYS_KILL] target=D sig=11` →
  `[ksys SYS_GETKSIG] target=D map=0x800` → `[ksys SYS_EXIT] target=D` →
  `[ksys SYS_ENDKSIG] target=D` — and D goes silent; A↔B ping-pong, C
  `[noq]`/`SYS_SCHEDULE` delegation, six periodic RS `[alarm]` fires all
  intact; every traced `result=0`; zero panic / `el0_sync_unexpected`. Host:
  `cargo test --workspace` green (new mproc + callnr/signal/com + classify
  tests); `cargo check --workspace` + clippy `-D warnings` + fmt clean; miri
  job gains `-p minixrs-pm` (advisory).
- **Slice 4.6** — PM part B: fork + exit + wait (two PRs like 3.4). **4.6a**
  ✓ shipped (PR #28, merged 2026-07-18): the kernel half — `SYS_FORK` (free
  slot, copy register frame, bump generation, alloc ASID, eager AS copy by
  walking the parent's TTBR0 + copying each user page via HHDM; CoW deferred;
  zeroes `sig_pending` on slot reuse), completion of 4.5's `SYS_EXIT`-lite into a
  full teardown (`AddrSpace::destroy`, free slot, bump generation,
  `unblock_dependents`), `okendpt` stale-generation rejection, and ASID
  free-list recycling. **4.6b** ✓ shipped (PR #29, merged 2026-07-18): the
  PM/VM/stub-E half.
  New PM requests `PM_FORK`/`PM_EXIT`/`PM_WAIT` (`PM_RQ_BASE + 1..3`) let a user
  proc drive the lifecycle entirely through PM (POSIX shape: user → PM, never
  user → kernel). `handle_fork` owns the tree — allocate a child `mproc` slot
  (fork pool `[16, NR_MPROCS)`, slot index = child kernel proc-nr), `SYS_FORK`
  (kernel clones the *frozen* child), `VM_FORK` (new `0xC04` request; VM's
  `region::fork` clones the parent's `ClientRegions`, `MAX_CLIENTS` widened
  16→32 to address the fork pool), `SCHEDULING_START` (SCHED schedules the
  still-frozen child — verified safe: `rts_unset` only enqueues on the
  last-block-bit clear, so `SYS_SCHEDULE`/`SYS_PRIVCTL` leave it a blocked
  `RTS_RECEIVING` receiver), `SYS_PRIVCTL(PRIVCTL_SET_USER)` to release the
  freeze, then a reply to **both** halves of the shared SENDREC (child gets
  `m_type = 0`, parent gets the child pid — MINIX fork-returns-twice). `mproc`
  gains a generation-aware `endpoint` field, `exit_status`, `MF_WAITING`, and a
  free-slot allocator / zombie-and-reap helpers (all pure `*_in`, host-tested).
  `handle_exit` = `SCHEDULING_STOP` (before teardown, while the endpoint is
  valid) + `SYS_EXIT` + zombie; `handle_wait` reaps a zombie or suspends the
  parent (no reply) until `handle_exit` wakes it. **Scope decision:** parent
  notification is the zombie + wait-reap handshake only — the kernel signal path
  default-*terminates* user procs, so a real `SIGCHLD` to the handler-less
  parent would kill it; async `SIGCHLD` waits for Phase 5 handlers. No new priv
  wiring (PM↔VM, PM↔SCHED, child↔`USER_PRIV_ID` edges all pre-exist). Stub E
  rewritten from the 4.5 `PM_GETPID` loop into a fork/exit/wait loop: fork, and
  on the reply `m_type` branch child→`PM_EXIT(0)` vs parent→`PM_WAIT`→loop.
  Verified in QEMU over 25 s: eleven `[as]` lines; `[ksys SYS_PRIVCTL] target=E`
  releasing E then each child; a sustained fork loop (69 cycles, still forking at
  t≈25 s) all reusing child slot 16 with a monotonically advancing endpoint
  generation (`0x10 → 0x220010` — proof of `okendpt` slot/ASID recycling and
  full reap between cycles), `[ksys SYS_FORK]`/`SYS_EXIT] target=E nr=16 freed=2`
  round-trips; A↔B ping-pong, C `SYS_GETINFO`, D's 3 resolved `[pf]` + 1 fatal
  SIGSEGV, six RS `[alarm]` fires, SCHED `[noq]` delegation all intact; every
  traced `result=0` (bar D's designed kill), zero panic / `el0_sync_unexpected`.
  Host: `cargo test --workspace` green (new `mproc` fork/wait, `region::fork`,
  `callnr` contiguity tests); `cargo check --workspace` + clippy `-D warnings` +
  fmt clean.
- **Slice 4.7** ✓ shipped (PR #30, merged 2026-07-18) — exec:
  `SYS_EXEC` + PM exec of a boot-embedded binary. `SYS_EXEC` (already numbered
  `0x603`, ENOSYS since 2.6) becomes real in `kernel/src/system/do_exec.rs` and
  moves from the caller-local arm to the **target-taking** match beside
  `SYS_FORK`: PM names the exec-ing user proc as the target. The handler resolves
  the boot-embedded binary by name (`BootImage::module_by_name`, its `dead_code`
  allow now dropped), builds a fresh address space via a new arch helper
  `userland::load_exec_image` (factored out of `load_boot_server` — `AddrSpace::new`
  + `elf::load_into` + one RW stack page at `SERVER_STACK_VA` + `alloc_asid`,
  `mem::forget`-ing the tree, `None` + partial-tree cleanup on OOM), resets the
  target's register frame to a clean EL0 start (`ArchRegisterFrame::EMPTY` +
  `elr_el1`/`sp_el0`/`spsr_el1`), renames the proc to the new binary, swaps in the
  new `(ttbr0_pa, asid)`, reclaims the old image via `do_exit::teardown_addrspace`
  (now `pub(super)`), and `rts_unset(RTS_RECEIVING)` to resume it at `_start`. The
  target is gated exactly like `do_fork`'s parent (a clean `RTS_RECEIVING`
  receiver — in the live flow mid-`SENDREC` to PM); `SELF`/self-target rejected
  (the active-TTBR0 hazard). exec preserves pid/priv/scheduler; **no reply on
  success** (the kernel resumes the target), errno reply on failure. New
  `PM_EXEC = PM_RQ_BASE + 4` (`0x704`, `NR_PM_MSGS` 4→5) + `EXEC_NAME_LEN = 16`
  (the name field in the `SYS_EXEC` payload) in `kernel-shared`; PM's `handle_exec`
  issues `sys_exec(caller, EXEC_TARGET="worker")` (a Phase-5 filesystem/musl path
  will thread a user-supplied name). The exec target is a new freestanding
  `userland/worker` ELF (getpid loop + `PM_EXIT`, no SEF — a plain user program),
  packed into the MXBI archive by `build.rs` with sentinel proc_nr
  `com::EXEC_ONLY_PROC_NR = -1` so the boot loader skips it (resolvable by name,
  never boot-loaded). Stub E's child branch rewritten from `PM_EXIT` to `PM_EXEC`:
  fork → child execs `worker` → worker exits → parent reaps → loop. PM's `main.rs`
  stays coverage-excluded (glob widened to `userland/**/src/main.rs`). Verified in
  QEMU over 25 s: eleven `[as]` lines (worker **absent** — exec-only); six
  `[ksys SYS_EXEC] target=16 name=worker entry=0x100000 freed=2` matched by six
  `SYS_FORK` (child_nr 16) and six `SYS_EXIT] target=w nr=16 freed=2`, all reusing
  child slot 16 with a monotonically advancing endpoint generation
  (`0x10 → 0x18010`) and recycled ASIDs (12↔10 — proof of exec teardown + reap +
  slot/ASID recycle); the worker's `PM_GETPID` SENDRECs surface (`caller=16
  target=0x0`); A↔B ping-pong, C `SYS_GETINFO`, D's 3 `[pf]` + 1 fatal SIGSEGV,
  SCHED `[noq]`, six RS `[alarm]` fires all intact; every traced `result=0` (bar
  D's designed kill), zero panic / `el0_sync_unexpected`. Host: `cargo test
  --workspace` green (extended `PM_EXEC` contiguity tests); `cargo check
  --workspace` + clippy `-D warnings` + fmt clean; `cargo kernel-aarch64` builds +
  packs the worker.
- **Slice 4.8** ✓ shipped (PR #31, merged 2026-07-18) — init
  (PID 1) + Phase 4 wrap-up + docs. **Phase 4 complete.** `init` becomes a real
  boot process: a freestanding `userland/init` ELF (fork/exec/wait respawn loop,
  `minix-ipc` only — no SEF; `_start` + panic handler `not(test)`-gated; `user.ld`
  copied from `worker`) packed into the MXBI archive by adding
  `("minixrs-init", …/init, 10)` to `build.rs`'s `servers` array. The existing
  boot loader loads it into `INIT_PROC_NR=10`, clears `RTS_NO_PRIV`, and enqueues
  it — no PM hand-release (contrast stub E's `PRIVCTL_SET_USER`). Its loop:
  `PM_FORK` → child (`m_type==0`) `PM_EXEC`s the boot-embedded `worker`, parent
  (`>0`) `PM_WAIT`s to reap, then loops. **User-grade privilege:** init's `IMAGE`
  `BootEntry.trap_mask` goes `SRV_T`→`USR_T`, and `init_boot_image` special-cases
  `INIT_PROC_NR` to point its proc slot at the shared `USER_PRIV_ID` (slot 20,
  filled by `populate_user_priv`, which already opens the PM↔USER edge) rather than
  a dedicated server-grade slot — so init SENDRECs PM only and makes no kernel
  calls, exactly the forked-child profile; its would-be dedicated priv slot 15
  stays free. `MF_PRIV_PROC` stays on init's `mproc` seed (unkillable PID 1; that
  flag gates only the kill path, not fork/wait/getpid). **Stub E retired (only E;
  A–D kept** as the live regression battery — A↔B IPC primitives, C's SCHED
  quantum-delegation round-trip, D's page-fault→VM + out-of-region SIGSEGV — which
  init+worker don't exercise): removed E's `user_stub.S` blob, its `userland.rs`
  `build_stub`/VAs/externs/summary line, `com.rs` `STUB_E_PROC_NR` + `NR_STUB_PROCS`
  5→4 (which shifts `FORK_POOL_BASE` 16→15, so forked-child kernel proc-nrs now
  start at 15), and PM `pm_init`'s stub-E release; `mproc` host tests rebase from
  stub E (slot 15) onto stub D (slot 14). Docs: new mdBook *Servers* chapter
  (`book/src/servers/overview.md`) + `SUMMARY.md` entry + CLAUDE.md 4.8 bullet.
  Verified in QEMU over 30 s: 11 `[as]` lines (vm/ds/vfs/sched/rs/pm/init asid 1–7,
  stubs A–D asid 8–11; stub E + `worker` **absent**), init (`parent=i nr=10`)
  driving `SYS_FORK child_nr=15` → `SYS_EXEC target=15 name=worker` →
  `SYS_EXIT target=w nr=15 freed=2` with a monotonically advancing child endpoint
  generation (`0xf → 0x800f → 0x1000f`) + recycled ASIDs, worker `PM_GETPID`
  SENDRECs (`caller=15/16 target=0x0`) surfacing; A↔B ping-pong, C `[noq]`, D's
  three `[pf]` + `SYS_KILL sig=11` chain, six RS `[alarm]` fires all intact; every
  `result=0` (bar D's designed kill); zero panic / `el0_sync_unexpected`. Host:
  `cargo test --workspace` green; `cargo check --workspace` + clippy `-D warnings`
  + fmt clean; `cargo kernel-aarch64` builds + packs the init ELF.

Aggregate scope (Phase 4 as a whole):

- `minix-ipc`: NOTIFY + SENDNB primitives
- `server-rt`: SEF startup + receive loop + init/signal callbacks (minimal subset)
- Multi-module boot-image archive (MXBI) + generalized server loader
- Kernel calls made real: `SYS_FORK`, `SYS_EXEC`, `SYS_EXIT`, `SYS_PRIVCTL`,
  `SYS_SCHEDULE`, `SYS_SETALARM`, plus new `SYS_SCHEDCTL` and a minimal signal
  mechanism; delegatable scheduler (`Proc::scheduler`)
- Servers (all static-allocation, no heap): DS (endpoint registry), SCHED (real
  user-space policy), RS (heartbeat monitor), VFS (skeletal boot), PM (process
  table, fork, exec, exit, wait, getpid, minimal signals)
- init (PID 1) forking/exec/waiting; embedded "worker" binary as the exec target
- **Milestone:** Full server boot sequence completes; init process starts —
  **reached; Phase 4 complete (slice 4.8, 2026-07-18).**

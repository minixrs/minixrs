# Phase 2: Kernel IPC + Scheduling — slice history

Full per-slice record for Phase 2, moved verbatim from `docs/plan.md` when it was
restructured into a lean tracker (2026-07-23). Status summary and milestone live in
[`../plan.md`](../plan.md).

---

Phase 2 is split into 6 PR-sized slices. Each slice is independently
buildable, boots, and produces observable output. The Phase 2 milestone
("two processes exchange IPC messages") is satisfied at the end of Slice
2.5; Slice 2.6 finishes the kernel-call surface needed by Phase 3.

- **Slice 2.1** ✓ shipped (PR #3, merged 2026-05-20) — `kernel-shared`
  foundation: `Message` (104-byte, 8-aligned `repr(C)`), `Endpoint`
  (generation-aware, 15-bit signed proc field, sentinels derived from
  `ENDPOINT_SLOT_TOP`), IPC primitive numbers, kernel-call numbers,
  task and server endpoint constants (renumbered contiguously; no
  static `LOG` slot), MINIX errno values. `NR_PROCS = 1024`. 21
  host-side unit tests; no kernel changes.
- **Slice 2.2** ✓ shipped (PR #4, merged 2026-05-22) — `Proc` and `Priv` structs with kernel-internal
  `RTS_*` / `MF_*` / priv-flag / trap-mask constants in
  `kernel/src/proc/flags.rs`; static `PROC_TABLE` (1029 slots) and
  `PRIV_TABLE` (64 slots) under `UnsafeCell` + `unsafe impl Sync` with
  documented single-threaded boot invariants; a 16-entry boot `IMAGE`
  drives `proc::init()`, and `proc::dump_tables()` writes a tabular
  UART view (5 kernel tasks runnable, 11 boot servers blocked on
  `RTS_NO_PRIV`, contiguous priv-ids 0–15). `kernel-shared` migrated
  `ProcNr` / `PrivId` / `SysId` from `i32` aliases to newtypes,
  added `NR_SYS_CALLS = 32` and a new `sys_limits` module
  (`NR_IO_RANGE`, `NR_IRQ`, `NR_MEM_RANGE`). `ArchRegisterFrame` stub
  in `arch/aarch64/context.rs` ready for slice 2.3 to populate. 25
  host-side tests pass (21 existing + 4 new newtype round-trips);
  clean release build with no warnings.
- **Slice 2.3** ✓ shipped (PR #5, merged 2026-05-22) — aarch64 SVC entry
  cooperative context switch. `entry.S`
  promotes the kernel from Limine's EL1t to EL1h on a primed SP_EL1
  (`mov x9, sp; msr SPSel, #1; mov sp, x9` — the only sequence that
  worked on QEMU virt / Cortex-A72; a bare `msr sp_el1, xN` from EL1t
  silently locks the core). New `arch/aarch64/{mmu,userland,trap,
  user_stub}.S/rs` build a minimal TTBR0_EL1 walk that maps one EL0
  code page (RO+X) and one EL0 stack page (RW+NX) at low VAs, copies a
  hand-coded `svc #0; b .` stub blob into the code page, and erets into
  EL0. Vector slot 8 (`el0_64_sync`) is specialized to `b
  el0_64_sync_entry` in `trap.S`; the SVC handler parks `&mut p.regs`
  in `TPIDR_EL1` so it can save state directly into the proc table and
  tail-call back to EL0 via `el1_return_to_user`. `do_ipc(frame)` in
  the new `kernel/src/ipc/` module prints each call and stores `OK` in
  `frame.x[0]`. `proc::sched::switch_to_user` wraps the eret. Verified
  in QEMU: `do_ipc[N]: call_nr=3 src_dst=32766 msg=0x0...` repeats
  ~16K times per second.
- **Slice 2.4** ✓ shipped (PR #6, merged 2026-05-23) — GICv3 + ARM generic timer + run queues. New
  `arch/aarch64/{gic,timer,irq}.rs` + `interrupt.S` bring up GICD/GICR
  (QEMU virt cortex-a72, `-machine virt,gic-version=3`), enable PPI 27
  (EL1 virtual timer) at 100 Hz, and route the IRQ through vector slot
  9 → `el0_64_irq_entry` (mirrors slice 2.3's slot-8 treatment) →
  `do_irq` → ICC_IAR1_EL1 ack / `clock::tick` / ICC_EOIR1_EL1. New
  `proc::sched` adds priority-banded FIFO run queues (`enqueue`,
  `dequeue`, `pick_proc`, `reschedule`, `run`) chained through
  `Proc::next_ready: Option<ProcNr>`. New `kernel/src/clock.rs` owns
  the per-tick handler — prints `current_proc.name[0]` to PL011, then
  decrements `quantum_left` and triggers `reschedule` on
  `RTS_NO_QUANTUM`. `userland_bootstrap` now stages two EL0 stubs (A
  and B at `0x40_0000`/`0x41_0000` sharing one code page, distinct
  stacks at `0x80_0000`/`0x81_0000`); SPSR drops to `0x340` so IRQs
  are unmasked at EL0. Verified in QEMU: 171 A ticks vs 170 B ticks
  over ~3.4 s, clean `AAAAA BBBBB AAAAA …` 5-per-quantum bursts with
  `do_ipc[N]` SVC traces interleaved (proves SVC + IRQ paths coexist).
- **Slice 2.5** ✓ shipped (PR #7, merged 2026-05-23) — IPC primitives in
  Rust. New `kernel/src/ipc/{mod,
  message, send, receive, notify, senda, deadlock}.rs`: `do_ipc`
  dispatcher with trap-mask gating, `mini_send` / `mini_receive` /
  `mini_notify` / `mini_sendnb` faithful to MINIX 3 `kernel/proc.c`,
  `deadlock_check` (size-2 SEND↔RECV legalization via the `function<<2`
  trick), `mini_senda` stub returning `ENOSYS` (deferred to a later
  slice; no Phase-2 consumer). New `sched::rts_set` / `rts_unset`
  capture `nr` and end the borrow before calling `enqueue`/`dequeue`
  so RTS transitions stay run-queue-coherent. New `sched::schedule_next`
  picks the highest-priority runnable proc, parks its frame in
  `TPIDR_EL1`, and flushes any pending `MF_DELIVERMSG` into the user
  buffer at `Proc::deliver_msg_vir` — invoked from the SVC tail via the
  new `el1_svc_tail` shim (`trap.S` now reads `bl do_ipc; bl
  el1_svc_tail; b el1_return_to_user`) and from `reschedule` + `run`.
  `Proc` gains `deliver_msg_vir: u64`. User-memory IPC reads via
  `core::ptr::read_volatile` with a coarse `USER_VA_TOP = 1 << 48`
  bounds check; fault recovery is Phase 3. `user_stub.S` rewritten:
  two PC-relative-free blobs in separate sections — stub A
  (`SENDREC` to endpoint B, persistent counter in `x19`), stub B
  (`RECEIVE` from `ANY` then `SEND`-reply to `m_source`). `userland.rs`
  installs two physical code pages, four `map_4k` calls, plus
  `install_stub_privs` filling priv slots 16/17 with
  `trap_mask = SRV_T` and `ipc_to` cross-targeting A↔B. Verified in
  QEMU over 8 s: ~897K IPC ops; ~2990 each of `call=1`/`2`/`3` with
  every line `result=0`; no panic / `el0_sync_unexpected`; `A`/`B`
  clock-tick interleaving from slice 2.4 still visible. **Phase 2
  milestone reached.**
- **Slice 2.6** ✓ shipped (PR #8, merged 2026-05-25) — Kernel-call
  dispatch + minimum `SYS_*` set. New
  `kernel/src/system/{mod,do_getinfo,stubs}.rs` implement the MINIX 3
  fast-path shape: `ipc::dispatch`'s SENDREC arm detects
  `src_dst_e == boot_endpoint(SYSTEM)` and diverts to
  `system::kernel_call_sendrec`, which runs synchronously in the
  caller's EL1 context (mirrors `kernel/system.c::kernel_call`). The
  dispatcher applies `Priv::k_call_mask` gating and the same `ipc_to`
  permission check that `mini_send` does (re-exported `get_sys_bit`).
  `SYS_GETINFO` is real (`GET_WHOAMI` writes caller's endpoint,
  priv flags, and `name` into the payload in-place and returns `OK`);
  the other 13 Phase-2 `SYS_*` calls land as `ENOSYS` stubs with their
  canonical MINIX 3 `do_*` names so Phase 3+ can swap real handlers in
  without touching dispatch. A `const _: () = assert!(NR_KERN_CALLS_
  PHASE2 == 14)` next to the dispatch match locks arm coverage — adding
  a new `SYS_*` without a new arm is a compile error.
  `kernel-shared/callnr.rs` gains `GET_WHOAMI = 12` (matches MINIX 3
  `include/minix/sysinfo.h`) and `SYS_GETINFO_NAME_LEN = 16` (kernel's
  `PROC_NAME_LEN`; deviates from MINIX 3's 44 B because minix.rs never
  stores more than 16 B per slot). `user_stub.S` gains a third
  `.rodata.user_stub_c` blob — SENDREC to `ENDPOINT_SYS` (`0x7FFE` =
  `boot_endpoint(SYSTEM)`) with `m_type = SYS_GETINFO` and
  `payload[0..4] = GET_WHOAMI`, persistent counter in `x19`.
  `userland.rs` adds stub C's code/stack pages plus `STUB_C_PROC_NR`
  (13) / `STUB_C_PRIV_ID` (18). `install_stub_c_priv` differs from the
  slice-2.5 helper: `trap_mask = USR_T` (only SENDREC), `ipc_to`
  opened only to SYSTEM's priv slot (resolved at boot via
  `proc_table_ref()` so the IMAGE order isn't hard-coded), and
  `k_call_mask` opened only to `SYS_GETINFO`. `ipc/mod.rs`'s trace
  gains a `TRACE_HEAD = 12` head carve-out — C's fast-path rate
  (~125 K ops/sec) would otherwise drown A↔B's ~10 SVCs/sec in the
  modulo-100 sampling and the slice-2.5 ping-pong would look like it
  regressed; the head trace shows each stub's first SVC explicitly.
  Verified in QEMU over 8 s: SVC #1 = stub A SENDREC → B (result=0),
  SVCs #2–4 = stub B RECEIVE/SEND/RECEIVE (all result=0), SVCs #5+ =
  stub C SENDREC → SYSTEM with 6536 `[ksys N]` dispatches all
  `result=0`; clean — no `el0_sync_unexpected`, no panic.

Aggregate scope:

- `kernel-shared`: Message struct, Endpoint, all call numbers, error codes
- Process table (`Proc`), privilege table (`Priv`)
- IPC: `mini_send`, `mini_receive`, `mini_notify`, `deadlock_check`
- aarch64 arch: exception vectors, GICv3, ARM generic timer, SVC entry, context switch
- Run queues: `enqueue`, `dequeue`, `pick_proc`, `switch_to_user`
- Kernel calls: minimum set (SYS_GETINFO, SYS_PRIVCTL, SYS_FORK, SYS_EXEC, SYS_EXIT, SYS_COPY, SYS_SAFECOPY, SYS_IRQCTL, SYS_VMCTL, SYS_SCHEDULE, SYS_SETALARM, SYS_TIMES, SYS_DIAGCTL, SYS_SETGRANT)
- Boot image unpacking, load two test processes
- **Milestone:** Two processes exchange IPC messages ("ping-pong test")

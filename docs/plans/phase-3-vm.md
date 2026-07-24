# Phase 3: VM Server + Memory Management — slice history

Full per-slice record for Phase 3, moved verbatim from `docs/plan.md` when it was
restructured into a lean tracker (2026-07-23). Status summary and milestone live in
[`../plan.md`](../plan.md).

---

Phase 3 is split into 7 PR-sized slices (the decomposition was originally
tracked in a local planning file, since retired — this file is the durable
record). Each slice
independently builds, boots, and prints observable progress. The Phase 3
milestone ("Boot processes each have isolated address spaces; VM handles
page faults") is satisfied at the end of slice 3.4; slices 3.5/3.6 then
add brk + mmap on top. POSIX fork and exec are deferred to Phase 4
(PM-driven).

Architecture choices (locked in by plan): per-process TTBR0 + 8-bit
ARMv8 ASIDs, kernel writes all user PTEs (VM passes decisions in via
SYS_VMCTL subcalls), kernel reads cross-AS user memory via HHDM after
walking the target proc's page table, VM uses static `[Region; N]`
per-proc tables (no allocator), stubs A/B/C from Phase 2.5/2.6 migrated
to per-proc TTBR0 in 3.1b and kept as regression coverage.

- **Slice 3.1a** ✓ shipped (PR #9, merged 2026-05-27) — Physical frame allocator + addrspace API, kernel-only,
  no EL0 changes. New `kernel/src/mm/{mod,frame}.rs`: intrusive free-list +
  per-region bump pointers seeded from Limine `MEMMAP_USABLE` entries
  (capacity `MAX_REGIONS = 16`; QEMU virt + Apple Silicon QEMU both fit
  comfortably). Frames inside the kernel image, embedded boot image, and
  Phase-2.5/2.6 static stub pages live in `EXECUTABLE_AND_MODULES` and
  are never visible to the allocator — no explicit reservation logic
  needed. `alloc_frame` zeros on hand-out so the caller never sees
  residual state; `free_frame` pushes via HHDM. `kernel-shared` /
  `Limine` integration: extended `arch/aarch64/limine.rs` with a
  `MemmapEntry` repr-C struct and a `memmap_entries()` iterator that
  walks the `**entry` indirection Limine uses. New
  `kernel/src/arch/aarch64/addrspace.rs`: `AddrSpace::new` allocates
  one L0 frame; `map_page(va, pa, Prot)` walks/allocates L1/L2/L3 on
  demand via the frame allocator, writes the leaf PTE through HHDM
  using the same PTE bit constants as `mmu.rs`; `walk_pt(va)` returns
  `Option<u64>`; `destroy()` recursively frees intermediate tables and
  the L0 root (leaf frames are caller-owned, not freed here). One-shot
  `mm_smoke_test` in `kmain` builds a throwaway AddrSpace, installs four
  mappings across distinct L2 slots, walks them all (plus one negative
  check), tears down, then verifies the free-list is LIFO by asserting
  the next `alloc_frame` returns the just-freed L0 PA. The smoke test is
  removed in 3.1b once real per-proc AddrSpaces replace `userland.rs`'s
  static `L0/L1/L2/L3_*` tables. Verified in QEMU over 8 s:
  `[mm] frame_alloc OK ttbr0_pa=0x40000000 / map OK / walk OK / free OK`
  prints in order; A↔B ping-pong head trace (`[ipc 1..4]`) and stub C
  SYS_GETINFO carve-out (~726 K SVCs, every line `result=0`) both
  unchanged from slice 2.6; no panic, no `el0_sync_unexpected`.
- **Slice 3.1b** ✓ shipped (PR #10, merged 2026-05-27) — Per-process
  TTBR0s + 8-bit ASIDs + minimal
  page-fault-diagnostic handler. `Proc` gains `ttbr0_pa: u64` and
  `asid: u8` (placed in a new "MMU state" block between `deliver_msg_vir`
  and `next_ready`; `Proc::EMPTY` zeroes both — kernel tasks and
  RTS_NO_PRIV boot servers keep the sentinel). New
  `kernel/src/arch/aarch64/asid.rs` carries an `UnsafeCell<u8>` counter
  starting at `FIRST_ASID = 1` (0 reserved for "uninitialized"), with
  `alloc_asid()` panicking on 8-bit wrap — real rollover deferred to
  Phase 4 since slice 3.1b only hands out three. `mmu.rs` loses the
  slice-2.3 monolithic `activate_user_ttbr0` (plus the slice-2.5 static
  `PageTable` newtype, `map_4k`, `pte_index`, `make_*_desc` const fns —
  all unused since 3.1a's `AddrSpace` took over) and gains three new
  helpers: `assert_tcr_el1_ttbr0_ready` now also asserts `TCR_EL1.AS == 0`;
  `enable_ttbr0_walks_once()` clears `TCR_EL1.EPD0` once at boot
  without binding any TTBR0; `switch_ttbr0_with_asid(ttbr0_pa, asid)`
  writes `TTBR0_EL1 = ttbr0_pa | ((asid as u64) << 48)` then issues
  `isb / tlbi aside1, Xt / dsb ish / isb` — TLBI is ASID-tagged and
  unconditional (the simpler control flow beats micro-optimizing three
  ASIDs). `kernel/src/arch/aarch64/userland.rs` is rewritten end-to-end:
  every static `L0_TABLE` / `L1_TABLE` / `L2_TABLE` / `L3_CODE_TABLE` /
  `L3_STACK_TABLE` / `USER_CODE_PAGE_*` / `USER_STACK_PAGE_*` and the
  `kernel_pa_of` helper are gone. Each stub's `build_stub` allocates an
  `AddrSpace::new()` L0 root, a code frame (stub blob copied in via
  `mm::phys_to_hhdm` + `mmu::flush_icache_range`), and a stack frame
  (zeroed by `alloc_frame`), then installs them with `Prot::RO_CODE` /
  `Prot::RW_DATA`. The resulting `(ttbr0_pa, asid)` is written into the
  proc slot by an 8-arg `populate_stub_slot`. The `AddrSpace` value is
  `core::mem::forget`-ed since the page-table tree is now durably owned
  via `Proc::ttbr0_pa`; only exit/exec paths in later slices will
  `destroy`. `proc::sched::schedule_next` adds two lines between
  `set_tpidr_to` and `flush_deliver_msg`: a `debug_assert!(ttbr0_pa != 0
  && asid != 0)` (kernel tasks would silently inherit the previous
  TTBR0 otherwise) and a `switch_ttbr0_with_asid` call. The order
  matters — the message flush writes via the active TTBR0, so the new
  proc's AS must be live first; cross-AS IPC delivery is still slice
  3.4's job. `el0_sync_unexpected` in `arch/aarch64/exception.rs`
  trades its single "EC = …" panic line for a per-EC decoder: EC=0x20
  prints IFSC + the `fsc_name` mnemonic; EC=0x24 prints DFSC + WnR +
  ISV. Real recovery (`RTS_PAGEFAULT` + scheduler unblock) still
  lives in slice 3.2; this slice keeps the `panic!` tail. The
  slice-3.1a `mm_smoke_test` is removed from `kmain` — three real
  per-proc AddrSpaces driving the EL0 stubs are the live exercise now.
  `kernel-shared` is untouched; host-side tests stay at 26 passing.
  Verified in QEMU over 8 s: boot prints three distinct
  `[as] stub X nr=N ttbr0_pa=0x... asid=N` lines (A=`0x40000000`/1,
  B=`0x40007000`/2, C=`0x4000e000`/3 — distinct L0 PAs courtesy of
  intermediate-table allocations between roots), then 2723 sampled
  `[ipc N]` traces from A↔B ping-pong, 2710 sampled `[ksys N]` traces
  from stub C's SYS_GETINFO, all `result=0`. Zero panic lines, zero
  `el0_sync_unexpected` lines, zero non-zero result codes.
- **Slice 3.2** ✓ shipped (PR #11, merged 2026-05-28) — Real EL0 page-fault handler + `RTS_PAGEFAULT` +
  kernel-resolved heap-window faults + 4th stub D. New
  `kernel/src/proc/page_fault.rs` carries arch-neutral `PageFaultState`
  (`addr`/`flags`/`ip`; flag bits `PFF_WRITE`/`PFF_INSTR`/`PFF_PERMISSION`)
  and `HeapWindow { start, end }` with a `contains` helper; `Proc` gains a
  `page_fault_state` + `heap_window` block between `asid` and `next_ready`
  (`Proc::EMPTY` zeroes both). `arch/aarch64/exception.rs` adds
  `do_page_fault(esr, elr, far)`: it classifies the abort (EC 0x20/0x24,
  FSC, WnR), records `page_fault_state`, blocks the faulting proc on the
  3.1b `RTS_PAGEFAULT` bit via `sched::rts_set`, and — since no VM exists
  yet — resolves heap-window faults inline (kernel-as-VM): `alloc_frame`,
  new `addrspace::map_page_in(ttbr0_pa, …)` (the extracted `map_page` body,
  reused so the kernel can map into a live tree by root PA), new
  `mmu::flush_tlb_asid(asid)` (ASID-tagged TLBI without a TTBR0 write),
  then `sched::rts_unset` requeues the proc. Faults outside the window
  still halt via the verbatim 3.1b `el0_sync_unexpected` decoder. `trap.S`'s
  non-SVC sync arm now mirrors the SVC tail (`bl do_page_fault; bl
  el1_svc_tail; b el1_return_to_user`) so the unblocked proc is rescheduled
  and retries the aborting instruction (aarch64 leaves `ELR_EL1` on it).
  `user_stub.S` gains a `.rodata.user_stub_d` blob (store to `0x0100_0000`
  in a loop, no SVC); `userland.rs` wires stub D (ProcNr 14, PrivId 19,
  code `0x43_0000` / stack `0x83_0000`, heap window `[0x0100_0000,
  0x0100_4000)`, `trap_mask = TSK_T` — D does no IPC) and threads a
  `heap_window` arg through `build_stub` / `populate_stub_slot` (A/B/C pass
  `HeapWindow::EMPTY`). `kernel-shared` untouched; 26 host tests stay
  green. Verified in QEMU over 8 s: four `[as]` lines (D = ttbr0_pa
  `0x40015000` / asid 4), exactly one `[pf] proc=D far=0x1000000 → alloc
  frame=0x4001c000, map RW, retry`, then D round-robins; A↔B ping-pong
  (1732 `[ipc]`) and stub C SYS_GETINFO (1720 `[ksys]`) all `result=0`;
  zero panic / `el0_sync_unexpected` lines.
- **Slice 3.3** ✓ shipped (PR #12, merged 2026-06-01) — Real `SYS_VMCTL`
  subcalls + stub D self-managing its heap. New
  `kernel/src/system/do_vmctl.rs` replaces the slice-2.6 `ENOSYS` placeholder
  with six subcalls: `VMCTL_PT_MAP` (kernel allocates a fresh frame — the
  frame allocator is kernel-side, unlike MINIX 3's VM-owned pool — maps it
  into the target's AS via the 3.1a `addrspace::map_page_in`, and returns the
  PA in the reply), `VMCTL_PT_UNMAP` (clears the PTE via the newly-extracted
  `addrspace::unmap_page_in` free fn and `free_frame`s the leaf),
  `VMCTL_CLEAR_PAGEFAULT` / `VMCTL_GET_PAGEFAULT` (clear / read the slice-3.2
  `RTS_PAGEFAULT` state — exercised cross-process by VM in 3.4),
  `VMCTL_VMINHIBIT_SET/_CLEAR`. Each subcall names a target by endpoint
  (`SELF` allowed), resolved `endpoint_proc → proc_index` like `ipc/send.rs`;
  run-queue transitions use the `sched::rts_set`/`rts_unset` capture-then-
  borrow-end pattern, and every PTE change is followed by an ASID-tagged
  `mmu::flush_tlb_asid`. To give `do_vmctl` the whole proc table (it acts on a
  target, not the caller), `system::kernel_call_dispatch` was refactored to
  take `(proc_table, priv_table, caller_nr, msg)`, route `SYS_VMCTL` to the
  table-taking handler, and dispatch the other 13 caller-only calls through a
  `dispatch_caller_local` helper. `kernel-shared/callnr.rs` gains the six
  `VMCTL_*` subcall numbers (`1..=6`, 0 reserved), `NR_VMCTL_SUBCALLS = 6`
  (locked by a const-assert next to the `do_vmctl` match), `VMCTL_PROT_WRITE`
  / `_EXEC`, and `NR_KERN_CALLS_PHASE3 = 14` (with `NR_KERN_CALLS_PHASE2` kept
  as a one-slice alias, dropped in 3.4). `user_stub.S`'s stub D is rewritten
  from the 3.2 fault-on-touch blob into a `VMCTL_PT_MAP` → store → `PT_UNMAP`
  loop against its own endpoint (heap VA `0x0100_0000`); `userland.rs`'s
  `install_stub_d_priv` widens D from `trap_mask = TSK_T` to `USR_T` with
  `ipc_to` opened to SYSTEM and `k_call_mask` granting `SYS_VMCTL`, and D's
  `heap_window` is set `EMPTY` (D self-manages memory, so the 3.2 kernel-as-VM
  fast path — kept in `do_page_fault` with no live consumer until 3.4 — is
  bypassed; a stray D fault now halts loudly). 28 host tests pass (26 + new
  VMCTL-subcall + phase-alias tests). Verified in QEMU over 8 s: four `[as]`
  lines (A/B/C/D ttbr0_pa distinct, asid 1–4), head traces
  `[ksys VMCTL_PT_MAP] proc=D va=0x1000000 pa=0x4001c000 result=0` +
  matching `PT_UNMAP` (PA stable across the map/free cycle → LIFO free-list
  reuse, no exhaustion), ~985 sampled `caller=14 call=8` VMCTL dispatches and
  ~1631 `caller=13 call=0` stub-C `SYS_GETINFO` dispatches all `result=0`,
  A↔B ping-pong handshake visible at boot (`[ipc 1..4]`); zero panic, zero
  `el0_sync_unexpected`, zero `[pf]` lines (D never faults).
- **Slice 3.4** ✓ shipped (two PRs; 3.4a PR #13, merged 2026-06-04; 3.4b
  PR #14, merged 2026-06-04)
  — Real VM server + kernel-originated `VM_PAGEFAULT` send. **3.4a** stood up
  the user-space build toolchain and ELF loader: `minix-ipc` SVC stubs, a
  freestanding `servers/vm` ELF (`user.ld`, base `0x10_0000`) built by
  `kernel/build.rs` (separate `CARGO_TARGET_DIR`, `CARGO_ENCODED_RUSTFLAGS`
  linker-script override) and embedded via `include_bytes!`, a minimal
  ET_EXEC/AArch64 loader in `kernel/src/boot_image/elf.rs`, and
  `userland::vm_bootstrap` loading VM into its real `VM_PROC_NR=7` slot (no
  priv install — `init_boot_image` already grants `SRV_T` + `SYS_VMCTL`). VM
  ran a `RECEIVE(ANY)` stub. **3.4b** made it functional: new
  `kernel-shared` `VM_PAGEFAULT` request number (`0xC00`) and dropped the
  `NR_KERN_CALLS_PHASE2` alias; new `ipc::send::mini_pf_send` — a
  kernel-originated SEND that models the faulting proc as a blocked sender on
  VM's `caller_q` (so the lingering `RTS_PAGEFAULT` keeps it blocked through
  the `RTS_SENDING` clear until `VMCTL_CLEAR_PAGEFAULT`); `do_page_fault`
  rewritten to record + block + `send_pagefault_to_vm` instead of the
  slice-3.2 inline heap-window resolve (permission faults still halt); the VM
  server's real loop resolves each `VM_PAGEFAULT` via
  `SYS_VMCTL(VMCTL_PT_MAP)` + `VMCTL_CLEAR_PAGEFAULT`; stub D reverted to a
  pure fault-on-touch demo (`trap_mask = TSK_T`, no IPC). The cross-AS
  user-copy rewrite the original plan listed was **deferred** to Phase 4:
  `schedule_next` flushes `MF_DELIVERMSG` *after* the TTBR0 switch, so every
  3.4 user-buffer copy already runs under the correct live TTBR0. Verified in
  QEMU: `[as] vm nr=7 asid=1`; one `[pf] proc=D far=0x1000000 flags=0x1 → VM`
  → `[ksys VMCTL_PT_MAP] proc=D pa=0x40023000 result=0` round-trip → D runs
  91 ticks with no re-fault; A/B ping-pong + C `SYS_GETINFO` intact, zero
  nonzero results, no panic. **Phase 3 milestone reached.**
- **Slice 3.5** ✓ shipped (PR #15, merged 2026-06-08) — VM
  region tracking + `VM_BRK`. New `servers/vm/src/region.rs`: a static
  `[ClientRegions; 16]` table (`UnsafeCell` newtype, keyed by proc number;
  `MAX_REGIONS = 4`) with `HEAP_BASE = 0x0100_0000`, `set_brk` (find-or-create
  the Heap region, page-align the new break) and `contains` (region lookup for
  the fault path). The VM server's receive loop dispatches `VM_BRK` to a
  `handle_brk` that replies to the SENDREC caller with the resulting break, and
  `handle_pagefault` now gates `VMCTL_PT_MAP` on `region::contains` — faults
  outside every region take a silent SIGSEGV path (faulter left blocked on
  `RTS_PAGEFAULT`; real signals are Phase 4). `kernel-shared/callnr.rs` gains
  `VM_BRK = VM_RQ_BASE + 1` (`0xC01`) + a host test. Stub D rewritten from a
  fault-on-touch blob into a brk client: `VM_BRK(0x0100_4000)` → touch page 0 →
  `VM_BRK(0x0100_8000)` → touch page 1 (only in range after the grow) → loop;
  `install_stub_d_priv` widened from `trap_mask = TSK_T` to `USR_T` with
  `ipc_to` opened to VM, and VM's `ipc_to` opened back to D (its priv slot 19 is
  past the `[0, n_active)` boot fill, so VM couldn't otherwise reply). Verified
  in QEMU: `[pf] proc=D far=0x1000000` + `far=0x1004000`, two
  `[ksys VMCTL_PT_MAP] proc=D va=0x1000000`/`va=0x1004000` `result=0`, then D
  round-robins with no re-fault; A↔B + C intact, no panic / `el0_sync_unexpected`.
- **Slice 3.6** ✓ shipped (PR #21, merged 2026-06-13) —
  `VM_MMAP` / `VM_MUNMAP` + Phase 3 doc/CLAUDE.md cleanup. **Phase 3 complete.**
  `kernel-shared/callnr.rs` gains `VM_MMAP = VM_RQ_BASE + 2` (`0xC02`) and
  `VM_MUNMAP = VM_RQ_BASE + 3` (`0xC03`) with two host contiguity tests. New in
  `servers/vm/src/region.rs`: a `Kind::Mmap` region variant and a per-client
  bump arena (`mmap_next`, seeded to `MMAP_BASE = 0x0200_0000` — a clean 16 MiB
  above `HEAP_BASE`). `mmap(len)` page-aligns, claims a free `MAX_REGIONS` slot
  as `Kind::Mmap [mmap_next, +size)`, bumps the pointer, and returns the
  VM-chosen base (`EINVAL` on zero/overflow len, `ENOMEM` on a full table —
  like `mmap(NULL, …)`); `munmap(addr, len)` matches the `Mmap` region by base,
  marks it `Unused`, and returns the page-aligned `[start, min(end, region.end))`
  sweep range — the `min` cap stops an overstated `len` from freeing heap
  frames. Ten new host tests cover both. The VM server (`main.rs`) dispatches
  `VM_MMAP → handle_mmap` (reply chosen base in payload `0..8`) and
  `VM_MUNMAP → handle_munmap`, which loops `SYS_VMCTL(VMCTL_PT_UNMAP)` over the
  returned range, ignoring the harmless `EINVAL` a never-faulted page returns.
  No kernel dispatch, `do_vmctl`, or priv-wiring changes — mmap/munmap ride the
  same D→VM SENDREC edge `install_stub_d_priv` already opened for brk.
  `user_stub.S` extends stub D after its brk sequence: `VM_MMAP(0x2000)` →
  stash the returned base in callee-saved `x22` → touch it (faults once, VM
  maps it) → `VM_MUNMAP(x22, 0x2000)` → steady loop over the two heap pages only
  (the mmap page is now unmapped). Docs: a real mdBook *Memory Management*
  chapter (`book/src/memory/overview.md`) written from source — frame
  allocator, per-proc `AddrSpace`/TTBR0/ASID, the page-fault → VM flow,
  `SYS_VMCTL`, and brk/mmap/munmap — plus CLAUDE.md notes on VM region kinds and
  the `VMCTL_PT_UNMAP`-on-hole behavior. Verified in QEMU over 8 s: five `[as]`
  lines (A/B/C/D + vm); three `[pf] proc=D` faults (`far=0x1000000`,
  `0x1004000`, `0x2000000`); three `[ksys VMCTL_PT_MAP] proc=D` (the two heap
  pages + `va=0x2000000`) and exactly one `[ksys VMCTL_PT_UNMAP] proc=D
  va=0x2000000` (the second, never-touched mmap page is a silent kernel
  `EINVAL`); D then round-robins with no re-fault; A↔B ping-pong + C
  `SYS_GETINFO` intact; zero nonzero results, no panic / `el0_sync_unexpected`.

Aggregate scope (Phase 3 as a whole):

- `kernel/src/mm/`: physical frame allocator
- `kernel/src/arch/aarch64/addrspace.rs`: per-process page-table API
- Per-proc TTBR0 + ASID allocator; context switch updates TTBR0_EL1
- EL1 page-fault handler routes to VM via kernel-originated SEND
- `kernel/src/system/do_vmctl.rs`: real SYS_VMCTL subcalls
- `kernel/src/boot_image/elf.rs`: minimal ELF loader for VM bootstrap
- `servers/vm/`: receive loop, region tracking, page-fault resolution,
  brk, mmap (all static-allocation; no heap allocator in VM)
- **Milestone:** Boot processes each have isolated address spaces; VM
  handles page faults

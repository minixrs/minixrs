# Kernel conventions

Rules that bind code under `kernel/src/` — crate shape, `unsafe` and static-table discipline,
assembly placement, IPC and scheduling invariants, and the memory-access, grant, device-mapping and
`exec` contracts established across Phase 5.

Neighbouring areas, linked rather than restated: [`rust-style.md`](./rust-style.md) for lint traps
and source hygiene, [`servers-and-drivers.md`](./servers-and-drivers.md) for the user-space crates
that sit on the other side of these contracts, and [`abi.md`](./abi.md) for request-band numbers,
the errno bands and the D8 ABI freeze.

## Crate shape

- The kernel ships `--release` only, so `debug_assert!` is compiled out. Use a hard `assert!` for
  invariants whose violation would silently corrupt (e.g. a null TTBR0/ASID reaching the scheduler);
  reserve `debug_assert!` for cheap "can't happen" documentation that's fine to drop in release
- The kernel crate is **bare-metal only and never host-built**. `kernel/Cargo.toml` sets
  `forced-target = "aarch64-unknown-none"` under `cargo-features = ["per-package-target"]`, so
  *every* cargo invocation cross-compiles it — `cargo check`/`clippy`/`test`, bare or `--workspace`,
  and any IDE that runs them. This wins even over an explicit `--target x86_64-apple-darwin`, so a
  host build is unreachable rather than merely discouraged. Consequences to respect: (a)
  `per-package-target` is **unstable** (rust-lang/cargo#9406) — it needs the pinned nightly, and a
  bump that drops it fails loudly on the manifest, with workspace `default-members` omitting
  `"kernel"` as the fallback; (b) the kernel bin must keep `test = false`/`bench = false`, or `cargo
  check --all-targets` breaks with `E0463: can't find crate for test` on a phantom `no_std` test
  harness; (c) `forced-target` also overrides the `--target` in an alias, which is why there is **no
  `kernel-x86_64` alias** — it would silently build aarch64. Phase 8 must relax `forced-target`
  first. Two guards (`build.rs`'s `cargo::error=` and `main.rs`'s `compile_error!`) are unreachable
  defense-in-depth. **No `#[cfg(target_os = "none")]` gates remain in `kernel/src/`** — do not
  reintroduce them; a new kernel module is just `mod foo;`. (The `cfg_attr(target_os = "minixrs",
  unsafe(link_section = …))` attributes in `servers/*`/`userland/*` are unrelated — those crates
  *are* host-built and host-tested; the attribute only hides an ELF section specifier from a Mach-O
  host.)
- `cargo test -p minixrs-kernel` does not run — the crate is bare-metal only and in-QEMU test infra
  is not yet built, so **there is no `#[cfg(test)]` code under `kernel/src/`**. Host-runnable logic
  belongs in `kernel-shared`: `user_va_ok` + `USER_VA_TOP` live in `kernel-shared/src/message.rs`
  precisely because their tests had never executed while the module was cfg-gated. Put new pure
  predicates over shared ABI types there — the crate doc carries a narrow carve-out for exactly that
  — and keep raw-pointer/hardware behaviour (e.g. `copy_msg_from_user`) in the kernel. QEMU is the
  primary verification for kernel code; see [`build-and-boot.md`](./build-and-boot.md)

## `unsafe` and static tables

- Kernel `unsafe` blocks require `// SAFETY:` comments documenting the invariant
- Static mutable tables use `UnsafeCell<[T; N]>` inside a `#[repr(transparent)]` newtype with
  `unsafe impl Sync`; document the single-threaded-boot invariant in the `// SAFETY:` comment
- To end a `&mut` borrow before an `unsafe` call that re-borrows the same static, capture state into
  locals (bool / scalar) and rely on NLL — `drop(&mut x)` is a no-op and triggers a
  `dropping_references` warning. The same hazard bites *refactors*: every static table here is an
  `UnsafeCell` newtype, so hoisting a read-only loop into a shared-borrow helper is never a pure
  extraction — a live `&mut` at the call site plus the helper's `&` is aliasing UB (slice 5.3
  shipped this in `free_frame`/`is_usable_pa`; fix is to run the check *before* taking the `&mut`)

## Assembly

- Assembly is confined to `.S` files (assembled via `cc` crate in `build.rs`); use
  `core::arch::asm!` only for single-instruction operations
- New `.S` files must be added to `kernel/build.rs`'s `sources` array; offset blocks (`.equ
  REGS_*_OFFSET …`) are duplicated per-file since there is no cross-`.S` include

## IPC and scheduling

- IPC linked lists use `Option<ProcNr>` indices into static arrays, not raw pointers
- Run-queue admission is decoupled from boot: `IMAGE.runnable` marks IPC reachability; only
  `proc::sched::enqueue` puts a proc in the scheduler's run queue
- IPC primitives take an explicit `&mut [Proc; N_PROC_SLOTS]` (and `&mut [Priv; NR_SYS_PROCS]`)
  slice; only `ipc::do_ipc` materializes those from `PROC_TABLE` / `PRIV_TABLE` via
  `proc_table_mut_slice` / `priv_table_mut_slice`. Keeps each primitive testable in isolation and
  dodges the two-`&mut`-from-one-`UnsafeCell` UB hazard
- Every EL1 → EL0 transition (SVC tail via `el1_svc_tail`, `sched::reschedule`, `sched::run`) calls
  `sched::schedule_next`, which flushes `Proc::deliver_msg` to the user buffer at
  `Proc::deliver_msg_vir` and clears `MF_DELIVERMSG` before resuming
- IPC blocking pairs with the `sched::rts_set` / `rts_unset` helpers — they capture `nr`, end the
  `&mut Proc` borrow, then call `enqueue` / `dequeue` so RTS state and the run queue stay in sync.
  Same NLL-capture pattern slice 2.4 used in `clock::tick`
- Kernel-call handlers that act on a *target* proc named in the message (e.g. `system::do_vmctl`,
  `system::do_schedule`'s `do_schedule`/`do_schedctl`) take the whole `&mut [Proc; N_PROC_SLOTS]`
  slice + `caller_nr`; caller-only handlers (e.g. `do_getinfo`) get a single `&mut Proc` / `&Priv`.
  `system::kernel_call_dispatch` routes `SYS_VMCTL` / `SYS_SCHEDULE` / `SYS_SCHEDCTL` to the
  table-taking form (a small `match` before `dispatch_caller_local`) and the rest through
  `dispatch_caller_local`. Run-queue transitions on a target use the same `sched::rts_set` /
  `rts_unset` capture-then-borrow-end pattern the IPC primitives use

## Fault-safe user access

(slice 5.1 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

- The kernel **never dereferences a user VA**, not even the active TTBR0's. Every byte in or out of
  a user address space goes through `kernel/src/mm/uaccess.rs` — `copy_from_user_as(ttbr0_pa, va,
  dst)` / `copy_to_user_as(ttbr0_pa, va, src)` / `probe_user_range(ttbr0_pa, va, len, write)` —
  which page-chunks the range, resolves each page with `addrspace::walk_pt_in(ttbr0_pa, va) ->
  Option<(u64, Prot)>` (the fourth member of the `map_page_in`/`unmap_page_in`/`walk_leaves`
  free-function family; `AddrSpace::walk_pt` delegates to it), and `memcpy`s through the frame's
  HHDM alias. Build on these primitives rather than re-deriving them
- An unmapped page is a walk miss returning **`EFAULT`, not an EL1 abort** — so **no exception-fixup
  table is needed or wanted** (D5 rejected the Linux `extable` approach)
- The copy is **address-space-independent**, so `flush_deliver_msg` may write a receiver's buffer
  before its TTBR0 is installed; the TTBR0-before-flush ordering in `sched::schedule_next` is not
  load-bearing, only `eret` needs it. Do not reintroduce a dependency on it
- `copy_to_user_as` must **check `Prot::writable`**, because the HHDM alias is a kernel mapping that
  the MMU's EL0 permission bits do not police
- Writes are **all-or-nothing** (probe first), since a 104-byte `Message` is only 8-aligned and
  really can straddle two pages
- `user_va_ok` stays the cheap range/alignment pre-gate; the page arithmetic (`page_chunks` /
  `PageChunk` / `USER_PAGE_SIZE`) lives in `kernel-shared/src/message.rs` beside it because the
  kernel crate has no `#[cfg(test)]`
- `ipc/message.rs` carries **zero `unsafe`** — messages stage through a `[u8; 104]` — so all
  raw-pointer work stays in `mm::uaccess` alone
- Error routing for a faulted copy: `EFAULT` to the caller's `x0` via `do_ipc` for the immediate
  sites, and to a blocked receiver's **parked `x0`** (`p.regs.x[0] = e as i64 as u64` — the MINIX
  `retreg` idiom `do_exit::unblock_dependents` uses) for the deferred flush, which also sets/clears
  `MF_MSGFAILED`
- **`SYS_DIAGCTL` is the servers' debug channel** (D2) — servers run at EL0 with no console, and it
  must keep working while other subsystems are under construction, so the text rides **inline in the
  payload** (subcode `0..4`, len `4..8`, up to `DIAG_TEXT_MAX = 88` bytes from `DIAG_TEXT_OFF = 8`)
  and needs no user-copy machinery. `kernel/src/system/do_diagctl.rs` prints one line per call as
  `[diag <name>] <text>`, where `<name>` is the caller's kernel-known `Proc::name` (never payload
  data, so a server can only identify itself) and the text is sanitized to printable ASCII, so one
  call is always exactly one line — the `grep -aF` marker contract depends on it. The client side is
  `server-rt::diag_print`; see [`servers-and-drivers.md`](./servers-and-drivers.md)
- Adding a demo stub is discouraged: `NR_STUB_PROCS` feeds `FORK_POOL_BASE`, so a fifth stub shifts
  init's forked children 15 → 16 and breaks checked-in markers — put new probe behavior in an
  existing stub's prologue

## Grants — the governed cross-address-space copy

(slice 5.2, D4 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

- A granting process keeps its `GrantEntry` table **in its own address space** and registers `(addr,
  entries)` with `SYS_SETGRANT`; `SYS_SAFECOPY` reads the entry back out of the *granter's* address
  space on every call, so a granter revokes by writing its own memory and **the kernel caches
  nothing that could go stale**
- The ABI is `kernel-shared/src/grant.rs` — a flat `#[repr(C)]` `GrantEntry`
  (flags/seq/who_to/who_from/addr/len, 32 bytes, layout pinned by `offset_of!` asserts because the
  kernel decodes it from raw bytes), MINIX's CPF flag values, and `GRANT_SHIFT = 20` id packing
  (`grant_id`/`grant_idx`/`grant_seq`)
- The engine is `mm::uaccess::copy_between_as(src_ttbr0, src_va, dst_ttbr0, dst_va, len)`: probe
  both ranges, then walk-and-`memcpy` per `dual_page_chunks` — **all-or-nothing on the
  destination**, and its `Prot::writable` check is load-bearing, because a granter may *lie* about
  writability and the HHDM alias is a kernel mapping the MMU's EL0 bits do not police
- `verify_grant` (`system/do_safecopy.rs`) checks, **in MINIX's order**: `okendpt` the granter;
  table registered + idx in range; the entry reads back (a walk miss is hidden as **`EPERM`, not
  `EFAULT`**, so a grantee cannot probe the granter's address space); `CPF_USED|CPF_VALID`; `seq`
  matches the id; requested access ⊆ flags; `who_to` == caller's *stored endpoint*; `offset + bytes
  <= len`. Keep that order
- `CPF_DIRECT` resolves the memory to the granter; `CPF_MAGIC` resolves it to `who_from` and
  **additionally requires the granter's `Priv::flags & SYS_PROC`** (minix.rs's stand-in for MINIX's
  hardcoded VFS/MIB list); `CPF_INDIRECT` is a documented `EINVAL`
- `SYS_COPY` is the same engine with no grant — `resolve_target` on both endpoints (`SELF` works),
  `k_call_mask` is the only gate, the `do_vmctl` trust stance
- All three route from the **target-taking `match`** in `kernel_call_dispatch`, `SYS_SETGRANT`
  included: it is caller-local in effect but needs `&mut Priv`, which `dispatch_caller_local` cannot
  hand out. It also **rejects a shared priv slot** (`proc_nr != Some(caller)` → `EPERM`) — one table
  address cannot describe several processes' memory
- **Both `do_exit` and `do_exec`** clear a dedicated slot's registration: exit so a recycled slot
  cannot inherit a stale table address, exec because the registered VA describes the image being
  discarded while the privilege slot survives
- A server that holds `SYS_COPY`/`SYS_SAFECOPY` and serves clients that do not must take the
  **granter from the kernel-stamped `m_source`, never from the payload** — a caller-supplied granter
  endpoint turns that server into a confused deputy, aiming a privileged cross-AS copy wherever the
  caller points. Apply it to **every** grant-id-carrying request
- Byte buffers use `kernel-shared::message::user_range_ok` (no alignment requirement), unlike
  `user_va_ok`, which stays right for the 8-aligned grant table. The client-side `GrantPool` lives
  in `server-rt`; see [`servers-and-drivers.md`](./servers-and-drivers.md)

## Device memory

(slice 5.3, D1 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

- **Device memory comes from reading `MAIR_EL1`, never writing it:** writing byte *i* retroactively
  retypes every live mapping using `AttrIndx=i`, including Limine's TTBR1 kernel/HHDM mappings whose
  indices this repo cannot enumerate. `mmu::init_device_attr_idx` (called once from
  `userland_bootstrap`, before any device mapping) scans bytes **1..8** for `0x04` (Device-nGnRE,
  preferred) else `0x00` (nGnRnE) — sufficient because an *unprogrammed* MAIR byte reads `0x00`,
  which is itself a valid MMIO encoding, so "already Device" and "unused" coincide. Only those two
  qualify: nGRE would gather two `DR` stores into one lost character, GRE would let the `DR` store
  pass the `FR` poll. `[mair] device attr_idx=… byte=…` is **forensic, not a boot marker**
  (firmware-dependent)
- `Prot` has a third field, `device` (+ `Prot::DEVICE_RW`), which is a compile error at every struct
  literal — the point. `do_vmctl`'s `pt_map` answers `device: false` **permanently**, because D1
  rejected a VM-mediated `VMCTL_MAP_PHYS` for Phase 5 (revisit for Phase-6 virtio-mmio, gated on a
  per-driver PA whitelist)
- `pte_prot` decodes `device` **statelessly** (`pte_attr_idx_of(pte) != ATTR_IDX_NORMAL`), sound
  because the kernel emits only two indices and byte 0 is pinned Normal-WB (`const _: () =
  assert!(ATTR_IDX_NORMAL == 0)`)
- The **total invariant** is `map_page_in`'s pair of asserts against `mm::is_usable_pa`:
  `prot.device ⇒ ¬RAM` and `¬prot.device ⇒ RAM`, so every leaf is provably `(RAM ∧ ¬device)` or
  `(device ∧ ¬RAM)` — the lemma that makes `if !prot.device { free_frame(…) }` sound in the **five**
  leaf sweeps: `do_exit::teardown_addrspace`, `do_fork::copy_addrspace`'s copy loop *and* its OOM
  unwind, `do_vmctl::pt_unmap`, and `userland::destroy_addrspace_with_leaves`. `free_frame` keeps
  its loud out-of-range assert deliberately — a silent skip would demote a forged-PA/double-free
  into an untraceable leak
- Fork **re-maps** a device leaf rather than copying it (MMIO is shared, and a `memcpy` through the
  cacheable HHDM alias would read side-effecting registers into RAM), and
  `mm::uaccess::resolve_copyable` rejects a device leaf as copy source *or* destination (`EFAULT`)
- `teardown_addrspace` is `pub(crate)` (and `system::do_exit` a `pub(crate) mod`) returning `(freed,
  devs)` — hence `devs=` on `[ksys SYS_EXIT]` — for `userland::device_teardown_selftest`, which is
  **load-bearing, not decoration**: TTY never exits, so the device arm would otherwise be dead code
  on a live landmine whose failure is a *kernel panic*. It runs unconditionally at boot (so
  `--no-default-features` covers it) and asserts **both** counts, `[devmap] selftest ok freed=0
  devs=1` — a guard skipping every leaf gives `devs=0`. Keep it unconditional
- The device VA map is an *address* ABI, not a message one, and lives in
  `kernel-shared/src/uspace.rs`: `USER_DEVICE_WINDOW_BASE = 0x4000_0000` (one whole L1 slot),
  `USER_DEVICE_WINDOW_SIZE = 16 MiB`, `TTY_UART_VA` = page 0; **not** emitted in the generated C
  headers
- The pre-map lives in `load_boot_server` gated on `nr == TTY_PROC_NR`, **deliberately not in
  `load_exec_image`** (shared with `do_exec`, so every exec'd binary would inherit a device window —
  and conversely a proc that exec'd would *lose* its window, which is why `do_exec` drops the device
  count). **No TLB maintenance**, because the AS was built moments ago and never installed in TTBR0
  (a recycled ASID is clean — `teardown_addrspace` flushes before `free_asid`) and
  `switch_ttbr0_with_asid` already issues `isb; tlbi aside1; dsb ish; isb` on TTY's first schedule —
  the same reasoning `SERVER_STACK_VA` relies on. Do not diverge
- PL011 register offsets are **duplicated** in `kernel/src/arch/aarch64/uart.rs` and
  `drivers/tty/src/pl011.rs` and cannot be shared: the kernel crate is bare-metal-only and pinned by
  `forced-target`, so it can never be a user-space dep, and a register layout is a hardware fact,
  not a shared ABI

## `exec` — the initial stack

(slice 5.5, D13 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

- `SYS_EXEC` does not merely point `sp_el0` at the stack page top — it builds the **Linux/SysV
  initial frame** there first (`[argc][argv…][NULL][envp…][NULL][auxv pairs][AT_NULL]` then the
  NUL-terminated name, padded so `sp` is 16-aligned; `argc = 1`, empty envp) so musl's
  crt/`__libc_start_main`/`__init_tls` run **unpatched**. The standing keep-the-musl-diff-minimal
  principle only holds if the *kernel* supplies the frame
- The byte layout lives in `kernel-shared/src/execstack.rs` (`build_initial_stack`, pure, zero
  `unsafe`, host-tested — the `user_va_ok`/`page_chunks` carve-out, since the kernel crate has no
  `#[cfg(test)]`); `do_exec` stages it in a `[u8; INITIAL_STACK_MAX]` **kernel-stack** buffer and
  installs it with `copy_to_user_as`, which is address-space-independent by design, so no new copy
  machinery and the TTBR0 need not be installed
- Both frame failures (`None` → `E2BIG`, copy error → `ENOMEM`) run `do_exit::teardown_addrspace` on
  the **fresh** image and return **before the point of no return**, extending `load_exec_image`'s
  leave-the-target-on-its-old-image invariant rather than weakening it
- `elf::load_into` returns `LoadedElf { entry, phdr_va: Option<u64>, phnum, phentsize }`, `phdr_va`
  computed in the segment loop from the first `PT_LOAD` whose *file* range covers `[e_phoff,
  e_phoff + phnum*phentsize)`; `ExecImage` carries the three through
- Auxv order is **fixed by the caller** (`AT_PHDR`, `AT_PHNUM`, `AT_PHENT`, `AT_PAGESZ`), not
  Linux's incidental order, so the trace and the tests are deterministic. The `AT_*` values are the
  Linux/SysV ones and are deliberately **not** emitted by `gen-c-headers` — musl defines them itself
- **`AT_PHDR` needs a linker-script opt-in.** lld's default aarch64 `max-page-size` (64 KiB) puts
  PT_LOAD #0 at file offset `0x10000`, leaving `e_phoff = 64` in an unmapped prefix, so a reported
  VA would fault `__init_tls`. `userland/worker/user.ld` therefore takes the `FILEHDR PHDRS` idiom
  (`text PT_LOAD FILEHDR PHDRS FLAGS(5)` + `. = <base> + SIZEOF_HEADERS`), giving PT_LOAD #0 at
  offset `0x0` / vaddr `0x100000` — both still page-aligned, so the loader's strict checks are
  untouched; only the entry moves one page up. **Server `user.ld`s stay unchanged** (boot-loaded
  images keep `sp = SERVER_STACK_VA + PAGE_SIZE` and never read an auxv). Any new exec'able binary
  that wants `AT_PHDR` must copy that idiom and be re-checked with `llvm-readobj --program-headers`

## `exec` from a filesystem

(slice 5.9, D6 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

- `SYS_EXEC` carries a **source selector** (`EXEC_SRC_OFF`, 0 invalid — the `SAFECOPY_*` convention)
  plus a grant triple, so one call number covers "a boot-archive module" and "the file VFS staged".
  `4..20` stays `argv[0]` *and* the proc name in **both** forms; only where the bytes come from
  changes
- The loader reads through `boot_image::elf::ElfSource` — an **enum, not a `dyn` trait** (no vtable
  per header field) — with `Bytes(&[u8])` and `UserGrant { ttbr0_pa, va, len }`, the granted arm
  delegating to `copy_from_user_as`, so there is **no new copy machinery**; the per-page segment
  copy stays a single copy in both forms
- The grant is validated by `do_safecopy::verify_grant` (widened to `pub(super)`), **not a second
  copy of its eleven checks**, and the read completes **before the point of no return**, so a
  granted buffer that is not an ELF leaves the target on its old image
- `load_exec_image` takes an `ElfSource` and returns `Result<_, i32>`, ending the
  fold-everything-into-`ENOMEM` stance: `Map(OutOfMemory)` → `ENOMEM`, `Source` → `EFAULT`, else
  **`ENOEXEC`**. Keep the errnos distinct — a probe that stages a non-ELF and expects a refusal
  proves nothing without them
- **`kernel/build.rs`'s pack-time `scan_brand` cannot reach a file**, so the runtime scan is the
  only gate: `brand::scan_note_segment` is split out (pure, host-tested; `scan_brand` delegates) and
  the loader stages each `PT_NOTE` through a bounded stack buffer — a brand past `MAX_NOTE_BYTES` is
  not found, which is impossible for anything this repo's `user.ld` rule produces
- `p_memsz`/`e_phnum` are *input* rather than build output, so `kernel-shared::execimage` carries
  `MAX_PHNUM`, a **cumulative** `PageBudget` (per-segment checking misses a hundred
  reasonable-looking segments), and `segment_end` — all pure and host-tested, the standing rule for
  predicates `kernel/src/` cannot test
- **A leading `/` is `PM_EXEC`'s only discriminator** between a path and a module name (one field,
  because two can disagree; and it is already the FS band's rule). Module names and paths are
  therefore disjoint namespaces, so nothing resolving a *path* can name the `rootfs` blob
- **`argv[0]` is the path's basename, never the path** — that single choice is what leaves
  `EXEC_NAME_LEN`, `PROC_NAME_LEN`, `execstack::INITIAL_STACK_MAX` and `worker`'s `ARGV0_NAMES` set
  untouched
- **`server-rt::rd_name` cannot express a NUL-padded field's rules**: it reports "no NUL anywhere"
  and "a full-width name" identically, and those are `ENAMETOOLONG` and `EINVAL`. Take the payload's
  whole fixed-width field as **raw bytes** instead (PM's `path::parse` is the precedent), for every
  fixed-width inline field

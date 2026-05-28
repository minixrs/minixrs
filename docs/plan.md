# MINIX 4: Implementation Plan

## Context

Build MINIX 4 as a learning OS that preserves MINIX 3's microkernel architecture -- message-passing IPC, user-space servers (PM, VFS, VM, RS, DS, SCHED), user-space drivers, and fine-grained privilege control -- but with a greenfield Rust kernel targeting modern 64-bit platforms (x86_64, aarch64) under QEMU/VirtIO. Someone familiar with MINIX 3 or the Tanenbaum book should recognize the concepts immediately.

**Key constraints:**
- BSD/MIT licensing only (no GPL)
- musl-libc fork for MINIX syscall wrappers
- aarch64 first (native Apple Silicon dev platform), then x86_64
- QEMU as primary platform, VirtIO drivers
- Custom basic userland (not NetBSD)

**Reference material:**
- MINIX 3 source (architectural reference only): https://github.com/Stichting-MINIX-Research-Foundation/minix
- musl-libc upstream: https://musl.libc.org/ (fork v1.2.5 for MINIX adaptation)
- Limine bootloader: https://github.com/limine-bootloader/limine

---

## Architecture Overview

```
+------------------------------------------------------------------+
|                        User Programs                             |
|  (linked against musl-minix, POSIX-compatible)                   |
+------------------------------------------------------------------+
         |  _syscall(endpoint, callnr, &msg)  via SYSCALL/SVC
         v
+--------+--------+---------+--------+--------+--------+-----------+
|   PM   |  VFS   |   VM    |   RS   |   DS   | SCHED  | Drivers   |
| fork   | open   | mmap    | monitor| pub/   | policy | virtio-blk|
| exec   | read   | pageflt | restart| sub    |        | virtio-net|
| exit   | write  | brk     | live-  | store  |        | virtio-con|
| signal | close  | CoW     | update |        |        | memory    |
+--------+--------+---------+--------+--------+--------+-----------+
         |  IPC messages (SEND/RECEIVE/SENDREC/NOTIFY)
         v
+------------------------------------------------------------------+
|                     MINIX 4 Microkernel (Rust)                   |
|  IPC | Scheduling | Interrupt dispatch | Memory protection       |
|  Kernel calls (SYS_*) for privileged servers                     |
+------------------------------------------------------------------+
|  aarch64 HAL (primary)  |        x86_64 HAL                      |
|  SVC/ERET               |        SYSCALL/SYSRET                  |
|  GIC, translation tables|        APIC, 4-level PT                |
+------------------------------------------------------------------+
|                     Limine Bootloader                            |
+------------------------------------------------------------------+
```

---

## Repository Structure

```
minix-4/
  Cargo.toml                    # Workspace root
  rust-toolchain.toml           # Pinned nightly
  .cargo/config.toml            # Per-target flags, linker scripts
  LICENSE                       # BSD-2-Clause

  kernel/                       # Rust microkernel (no_std, no_main)
    Cargo.toml
    build.rs                    # Assembles .S files via cc crate
    src/
      main.rs                   # kmain() entry
      panic.rs                  # Kernel panic handler
      ipc/
        mod.rs                  # IPC subsystem
        message.rs              # Message copy/validate
        send.rs                 # mini_send (blocking send)
        receive.rs              # mini_receive (blocking receive)
        notify.rs               # mini_notify (async notification)
        senda.rs                # Async send table
        deadlock.rs             # Cyclic dependency detection
      proc/
        mod.rs                  # Process table
        table.rs                # Static NR_TASKS+NR_PROCS array
        endpoint.rs             # Generation-aware endpoint math
        privilege.rs            # Priv structure (IPC masks, k_call masks, I/O grants)
        schedule.rs             # Run queues, enqueue/dequeue/pick_proc
      system/
        mod.rs                  # Kernel call dispatch table
        do_fork.rs              # SYS_FORK .. SYS_SAFECOPY etc (~40 handlers)
        ...
      arch/
        mod.rs                  # Arch trait + cfg selection
        aarch64/                # PRIMARY -- native dev target
          mod.rs
          boot.rs               # Limine UEFI handshake, early init
          exception.rs          # EL1 exception vectors, SVC dispatch
          context.rs            # Register frame (x0-x30, SP_EL0, ELR_EL1, SPSR_EL1)
          syscall.rs            # SVC entry/exit
          mmu.rs                # Translation tables (4KB granule, 4-level)
          gic.rs                # GICv3 interrupt controller
          timer.rs              # ARM generic timer (CNTV)
          uart.rs               # PL011 UART (QEMU virt)
          entry.S               # Vector table, _start, context save/restore
          linker.ld
        x86_64/                 # SECONDARY -- added after aarch64 works
          mod.rs
          boot.rs               # Limine handshake
          gdt.rs                # GDT/TSS
          idt.rs                # IDT, exception handlers
          context.rs            # Register frame, context switch
          syscall.rs            # SYSCALL/SYSRET MSR setup
          interrupt.rs          # APIC, IRQ dispatch
          paging.rs             # 4-level page tables
          serial.rs             # Early debug output
          entry.S               # _start, SYSCALL entry, IRQ stubs
          linker.ld
      clock.rs                  # Timer interrupt, quantum management
      interrupt.rs              # Generic IRQ hook framework
      memory.rs                 # Kernel heap, phys frame allocator
      boot_image.rs             # Unpack embedded boot modules

  kernel-shared/                # Types shared between kernel + userspace (no_std)
    Cargo.toml
    src/
      lib.rs
      message.rs                # Message struct (96-byte payload union)
      endpoint.rs               # Endpoint constants + math
      ipc_const.rs              # SEND/RECEIVE/SENDREC/NOTIFY/SENDNB/SENDA
      com.rs                    # Server endpoints (PM_PROC_NR, VFS_PROC_NR, ...)
      callnr.rs                 # PM_*, VFS_*, VM_* call numbers
      error.rs                  # MINIX error codes

  minix-ipc/                    # Rust IPC library for userspace
    Cargo.toml
    src/
      lib.rs                    # ipc_send, ipc_receive, ipc_sendrec, ipc_notify
      x86_64.rs                 # SYSCALL asm stub
      aarch64.rs                # SVC asm stub

  server-rt/                    # Server runtime (SEF equivalent)
    Cargo.toml
    src/
      lib.rs
      sef.rs                    # sef_startup(), sef_receive()
      init.rs                   # Fresh/restart init callbacks
      signal.rs                 # Signal handling
      ping.rs                   # RS heartbeat

  servers/                      # User-space system servers (Rust)
    pm/                         # Process Manager
    vfs/                        # Virtual File System
    vm/                         # Virtual Memory
    rs/                         # Reincarnation Server
    ds/                         # Data Store
    sched/                      # Scheduler

  drivers/                      # User-space drivers (Rust)
    driver-rt/                  # Driver runtime (BDEV/CDEV protocol + VirtIO)
    virtio-blk/
    virtio-net/
    virtio-console/
    memory/                     # /dev/null, /dev/zero, ramdisk

  fs/                           # File system servers
    mfs/                        # MINIX File System (Rust)
    pfs/                        # Pipe File System

  musl/                         # musl-libc fork (v1.2.5)
                                # Add src/minix/ with MINIX IPC syscall wrappers

  userland/                     # Custom basic userland
    init/                       # /sbin/init (PID 1)
    sh/                         # Simple shell
    coreutils/                  # Multi-call binary: ls, cat, cp, mv, rm, mkdir, ...

  tools/
    mkimage.sh                  # Create bootable QEMU disk image
    mkbootimage.rs              # Pack server ELFs into boot archive
    qemu-run.sh                 # Launch QEMU with correct flags
    targets/
      x86_64-minix-kernel.json  # Rust custom target (kernel)
      x86_64-minix-user.json    # Rust custom target (userspace)
      aarch64-minix-kernel.json
      aarch64-minix-user.json

  external/
    limine/                     # Limine bootloader (BSD-licensed)
      limine.h                  # Vendored protocol header
      Makefile                  # Download + extract binaries

  docs/
    architecture.md
    ipc.md
    syscalls.md
    servers.md
    boot.md
    drivers.md
    musl.md
    memory-layout.md
    build.md
    minix3-mapping.md
```

---

## Core Data Structures (Rust)

### Message (kernel-shared)

```rust
/// 104-byte fixed-size IPC message matching MINIX 3 x86_64 layout.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Message {
    pub m_source: Endpoint,    // Who sent this (set by kernel)
    pub m_type: i32,           // Call number or result code
    pub payload: [u8; 96],     // Typed access via accessor methods
}
```

Typed message accessors (e.g., `msg.as_vfs_read()`) replace MINIX 3's opaque `m1i1`/`m2l1` field names -- a major educational improvement. The `kernel-shared` crate provides both the raw `Message` and typed accessor structs for each call.

### Process Table (kernel)

```rust
pub const NR_TASKS: usize = 5;     // ASYNCM, IDLE, CLOCK, SYSTEM, HARDWARE
pub const NR_PROCS: usize = 1024;

pub struct Proc {
    pub regs: ArchRegisterFrame,
    pub nr: ProcNr,
    pub endpoint: Endpoint,
    pub priv_ptr: Option<PrivId>,   // Index into privilege table
    pub rts_flags: AtomicU32,       // RTS_SENDING | RTS_RECEIVING | ...
    pub misc_flags: u32,            // MF_DELIVERMSG | ...
    pub priority: u8,
    pub quantum_left: u64,

    // IPC state -- linked lists via indices, not raw pointers
    pub caller_q: Option<ProcNr>,   // Head of callers wanting to send to us
    pub q_link: Option<ProcNr>,     // Next in caller queue
    pub getfrom_e: Endpoint,        // Whom we're waiting to receive from
    pub sendto_e: Endpoint,         // Whom we're waiting to send to
    pub send_msg: Message,          // Buffered outgoing message
    pub deliver_msg: Message,       // Message to deliver on unblock

    pub name: [u8; 16],
}
```

Using `Option<ProcNr>` (table indices) instead of raw pointers for linked lists eliminates aliasing hazards and makes the code safer and easier to reason about.

### Privilege Structure (kernel)

```rust
pub struct Priv {
    pub id: SysId,
    pub flags: u16,                              // PREEMPTIBLE, BILLABLE
    pub trap_mask: u16,                           // Allowed IPC traps bitmap
    pub ipc_to: [u32; NR_SYS_PROCS / 32],       // Allowed IPC destinations
    pub k_call_mask: [u32; NR_SYS_CALLS / 32],  // Allowed kernel calls
    pub notify_pending: [u32; NR_SYS_PROCS / 32],
    pub io_ranges: ArrayVec<IoRange, 16>,
    pub irqs: ArrayVec<u32, 8>,
    pub grant_table: VirAddr,
    pub grant_entries: usize,
}
```

---

## IPC Semantics (preserved from MINIX 3)

Six primitives, same semantics as MINIX 3's `proc.c`:

| Primitive | Behavior |
|-----------|----------|
| `SEND`    | Block until receiver accepts message |
| `RECEIVE` | Block until a message arrives (checks notify pending, async pending, caller queue) |
| `SENDREC` | Atomic SEND + RECEIVE (most common -- used by `_syscall()`) |
| `NOTIFY`  | Set bit in receiver's `notify_pending` bitmap; non-blocking |
| `SENDNB`  | Non-blocking send; returns error if receiver not waiting |
| `SENDA`   | Process table of async messages |

**Entry path (aarch64):** User executes `SVC #0`. Exception vector saves registers, calls `do_ipc()` in Rust.

**Entry path (x86_64):** User executes `SYSCALL` instruction. Kernel entry in `entry.S` saves registers, calls same `do_ipc()`.

---

## System Call Path (same as MINIX 3)

```
User: read(fd, buf, count)
  |
  v
musl: read.c fills Message { m_type: VFS_READ, fd, buf_ptr, count }
  |   calls _syscall(VFS_PROC_NR, VFS_READ, &msg)
  |     calls ipc_sendrec(VFS_PROC_NR, &msg)
  |       executes SVC (aarch64) or SYSCALL (x86_64)
  v
Kernel: do_ipc() -> mini_send() copies msg to VFS deliver_msg, unblocks VFS
         -> mini_receive() blocks caller until VFS replies
  |
  v
VFS: sef_receive() returns the message
  |   dispatches to do_read()
  |   routes to MFS via REQ_READ message
  |   MFS reads from disk (via BDEV_READ to virtio-blk)
  |   MFS replies to VFS
  |   VFS replies to user process
  v
Kernel: mini_send() delivers reply, unblocks user
  |
  v
musl: _syscall() returns result, sets errno if negative
  |
  v
User: read() returns bytes read
```

---

## musl-libc Integration

The musl fork (v1.2.5, MIT license) needs the following changes:

**New files to add (`src/minix/`):**
- `_syscall.c` -- `int _syscall(endpoint_t who, int callnr, message *m)` using `ipc_sendrec()`
- `_ipc_aarch64.S` -- `SVC`-based IPC trap
- `_ipc_x86_64.S` -- `SYSCALL`-based IPC trap (matches kernel's `entry.S` ABI)
- ~100 POSIX wrapper files (open.c, read.c, write.c, fork.c, exit.c, etc.) that construct MINIX messages and call `_syscall()`. Structurally identical to MINIX 3's `lib/libc/sys/` wrappers.

**Files to modify:**
- `arch/aarch64/syscall_arch.h` -- Gut Linux `__syscall*` macros; redirect to MINIX path
- `arch/x86_64/syscall_arch.h` -- Same
- `Makefile` -- Add `src/minix/` sources

**Include bridge:** A small header set (`minix/ipc.h`, `minix/com.h`, `minix/callnr.h`) generated from `kernel-shared` Rust crate via `cbindgen`, so the C wrappers use the same message types and call numbers as the Rust kernel and servers.

---

## Boot Sequence

**Bootloader:** Limine (BSD-licensed, supports x86_64 BIOS+UEFI and aarch64 UEFI).

```
Limine loads kernel ELF64 from FAT32 boot partition
  -> _start (entry.S): set kernel stack, call kmain()
  -> kmain():
     1. Parse Limine responses (memmap, HHDM offset)
     2. Init kernel heap, serial console
     3. Unpack embedded boot image -> module_list[]
     4. arch_init() -- exception vectors, GIC, timer (aarch64) or GDT/IDT/APIC (x86_64)
     5. proc_init() -- clear process table
     6. Load each boot module ELF, create page tables, set privileges
     7. system_init() -- register kernel call handlers
     8. Enable timer interrupt
     9. Unblock boot processes
    10. switch_to_user() -- schedule first process (never returns)

Boot processes start in order:
  DS -> RS -> PM -> SCHED -> VFS -> memory -> tty -> VM -> PFS -> MFS -> init
```

Boot modules are packed into the kernel ELF as a `.boot_image` section (MXBI header + ELF entries).

---

## Build System

- **Cargo workspace** at root for all Rust crates (kernel, servers, drivers, userland)
- **Custom Rust targets** (`aarch64-minix-kernel.json`, `aarch64-minix-user.json`, plus x86_64 variants) for `no_std` kernel and freestanding userspace
- **musl** built separately via its own Makefile with cross-compilation flags; produces `libc.a` + `crt*.o` installed to a sysroot
- **`tools/mkbootimage.rs`** packs server/driver ELF binaries into the boot archive linked into the kernel
- **`tools/mkimage.sh`** creates GPT disk: FAT32 boot partition (Limine + kernel) + MinixFS root
- **`tools/qemu-run.sh`** launches QEMU with VirtIO devices and serial console

---

## Phased Implementation Plan

**Platform order:** aarch64 first, x86_64 second.

### Phase 0: Architecture Documentation (complete)

Full documentation and project scaffolding so future contributors have complete context.

**Deliverables:** `docs/` directory with architecture, IPC, syscall catalog, servers, boot, drivers, musl, memory layout, build, and MINIX 3 mapping documentation. Cargo workspace with all crate placeholders. CLAUDE.md, LICENSE (BSD-2-Clause).

**Milestone:** `docs/` is comprehensive enough to start coding Phase 1 without re-exploring MINIX 3.

### Phase 1: Kernel Scaffolding + Boot (aarch64, QEMU virt) (complete)

- Cargo workspace with `kernel`, `kernel-shared` crates
- Builds via `aarch64-unknown-none` + linker script (`kernel/src/arch/aarch64/linker.ld`); the bespoke `aarch64-minix-kernel.json` target spec is deferred to Phase 2 (the stock target is sufficient for boot)
- Vendor Limine v9.x binary + header (`external/limine/Makefile`); Rust-side request block in `kernel/src/arch/aarch64/limine.rs` (base revision, HHDM, memmap, paging mode, stack size)
- PL011 UART driver at `kernel/src/arch/aarch64/uart.rs` with `core::fmt::Write` adapter; MMIO base is HHDM-relative (Limine base revision 2 keeps the [0, 4 GiB) blanket map covering PL011)
- aarch64 exception vector table (`vectors.S` + `exception.rs`); any unexpected trap routes through `exception_entry` and panics with a decoded ESR_EL1/ELR_EL1/FAR_EL1 dump
- `tools/qemu-run.sh` is the cargo runner: stages an ESP under `target/esp/`, auto-detects edk2 firmware, boots with `qemu-system-aarch64 -M virt` via `-drive file=fat:rw:...`
- **Milestone:** `cargo run -p minix4-kernel --target aarch64-unknown-none --release` boots through UEFI + Limine, prints `MINIX 4 booting on aarch64` + HHDM offset, then halts in `wfe`. Exception path verified by injecting a deliberate fault and observing a clean panic dump.

### Phase 2: Kernel IPC + Scheduling (the heart of MINIX)

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
  `PROC_NAME_LEN`; deviates from MINIX 3's 44 B because MINIX 4 never
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

### Phase 3: VM Server + Memory Management

Phase 3 is split into 7 PR-sized slices (decomposition tracked in
`~/.claude/plans/work-on-phase-3-optimized-petal.md`). Each slice
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
- **Slice 3.2** ◀ ready (branch `feature/phase-3-2-page-fault-handler`,
  pending merge) — Real EL0 page-fault handler + `RTS_PAGEFAULT` +
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
- **Slice 3.3** ◀ next — Real `SYS_VMCTL` subcalls (`PT_MAP`, `PT_UNMAP`,
  `CLEAR_PAGEFAULT`, `GET_PAGEFAULT`, `VMINHIBIT_SET/_CLEAR`) exercised
  from stub D directly (no fake-VM bridge).
- **Slice 3.4** — Real VM server boots, kernel ELF loader, cross-AS IPC
  delivery (HHDM-after-walk), kernel-originated `VM_PAGEFAULT` send.
  **Phase 3 milestone reached here.**
- **Slice 3.5** — VM region tracking (static `[Region; N]` per proc) +
  `VM_BRK`.
- **Slice 3.6** — `VM_MMAP` / `VM_MUNMAP` + Phase 3 doc/CLAUDE.md cleanup.

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

### Phase 4: Core Servers (PM, VFS, RS, DS, SCHED)

- `server-rt`: SEF implementation (startup, receive loop, ping/signal handling)
- `servers/pm/`: fork, exec, exit, wait, signals, UIDs
- `servers/vfs/`: File operations routed to FS servers, worker threads
- `servers/rs/`: Service monitoring, heartbeat, restart-on-crash
- `servers/ds/`: Key-value publish/subscribe
- `servers/sched/`: User-space scheduling policy
- **Milestone:** Full server boot sequence completes; init process starts

### Phase 5: musl Fork + File Systems

- Add `src/minix/` to musl fork with IPC wrappers
- Generate C headers from `kernel-shared` via cbindgen
- `fs/mfs/`: MINIX File System server (Rust, MinixFS v3 on-disk format)
- `fs/pfs/`: Pipe File System
- `drivers/memory/`: /dev/null, /dev/zero, ramdisk
- initramfs for early boot before disk driver
- **Milestone:** C "Hello World" compiled against musl runs on MINIX 4

### Phase 6: VirtIO Drivers

- `drivers/driver-rt/`: VirtIO MMIO transport (aarch64), virtqueue management, BDEV/CDEV protocol
- `drivers/virtio-blk/`: Block device
- `drivers/virtio-console/`: TTY
- `drivers/virtio-net/`: Network (packet I/O only; TCP/IP stack is later)
- Root filesystem on VirtIO disk
- **Milestone:** Boots from VirtIO disk, mounts MinixFS root

### Phase 7: Userland

- `userland/init/`: Run /etc/rc, respawn gettys
- `userland/sh/`: Simple shell (pipes, redirects, background jobs, builtins)
- `userland/coreutils/`: Multi-call binary -- ls, cat, cp, mv, rm, mkdir, rmdir, echo, wc, head, tail, grep, chmod, pwd, env, sleep, date, uname, kill, ps, true, false, test
- **Milestone:** Login, navigate filesystem, run scripts

### Phase 8: x86_64 Port

- `kernel/src/arch/x86_64/`: GDT, IDT, APIC, SYSCALL/SYSRET, 4-level page tables, context switch
- `musl/src/minix/_ipc_x86_64.S`
- VirtIO PCI transport
- `tools/qemu-run-x86_64.sh`
- **Milestone:** Same MINIX 4 boots on `qemu-system-x86_64`

### Phase 9: Documentation + Polish

- Comprehensive rustdoc across all crates
- Architecture guide for students
- End-to-end syscall trace walkthrough ("How read() works")
- IPC latency + context switch benchmarks
- Test suite (kernel unit tests, IPC stress, POSIX conformance)
- **Milestone:** Teaching-ready OS with documentation

---

## Key Design Decisions

**Rust kernel, not incremental port:** MINIX 3's C kernel uses raw pointers, global mutable state, and macro-heavy abstractions that resist incremental Rustification. A greenfield kernel uses proper Rust patterns from the start (enums for flags, Result for errors, indices not pointers for linked lists).

**`unsafe` boundaries in kernel:** (a) Process table access (`UnsafeCell` static array), (b) user-space memory copies, (c) hardware register access (inline asm), (d) assembly entry/exit paths. Each `unsafe` block gets a `// SAFETY:` comment.

**musl over Rust libc:** musl is well-tested, BSD-compatible, and complete. The MINIX-specific work is confined to ~100 syscall wrappers + IPC assembly stub. The rest of musl (stdio, string, math, locale) works unchanged.

**Servers in Rust:** Shared `kernel-shared` message types provide compile-time IPC protocol verification. C servers (linked against musl) also work since the IPC mechanism is language-agnostic.

**Multi-call coreutils:** Single `coreutils` binary dispatching on argv[0] (like BusyBox) minimizes disk space and simplifies building.

---

## Verification Strategy

Each phase has a concrete milestone testable in QEMU (aarch64 primary):

- Phase 0: `docs/` complete, directory tree navigable, CLAUDE.md in place
- Phase 1: `qemu-system-aarch64 -M virt` -> serial output visible
- Phase 2: Ping-pong test via IPC verified by serial trace
- Phase 3: Boot processes run in separate address spaces
- Phase 4: Server initialization sequence completes to init
- Phase 5: `printf("Hello\n")` from C program reaches serial
- Phase 6: Boots from VirtIO disk, mounts MinixFS root
- Phase 7: Interactive shell session
- Phase 8: Same tests pass on `qemu-system-x86_64`
- Phase 9: `cargo doc`, test suite green

QEMU flags for aarch64 dev:
```
qemu-system-aarch64 -M virt -cpu cortex-a72 -m 256M \
  -serial stdio -no-reboot -d int,cpu_reset -D qemu.log \
  -drive file=minix4.img,format=raw,if=virtio \
  -device virtio-net-device
```
GDB: add `-s -S` then `rust-gdb -ex "target remote :1234"`

---

## Critical Reference Files (MINIX 3)

Paths are relative to the MINIX 3 source tree root (under the `minix/` subdirectory
for MINIX-specific code). See https://github.com/Stichting-MINIX-Research-Foundation/minix

| Purpose | Path in MINIX 3 tree |
|---------|---------------------|
| IPC implementation (translate to Rust) | `kernel/proc.c` |
| Process/privilege structures | `kernel/proc.h`, `kernel/priv.h` |
| Kernel call dispatch | `kernel/system.c` |
| x86_64 entry points | `kernel/arch/x86_64/mpx.S` |
| Message definitions | `include/minix/ipc.h` |
| IPC constants | `include/minix/ipcconst.h` |
| Server endpoints + call numbers | `include/minix/com.h`, `include/minix/callnr.h` |
| User-space IPC stubs | `lib/libc/arch/x86_64/sys/_ipc.S` |
| _syscall() wrapper | `lib/libc/sys/syscall.c` |
| POSIX wrappers (template for musl) | `lib/libc/sys/*.c` |
| SEF framework | `lib/libsys/sef.c` |
| Boot process table | `kernel/table.c` |
| Limine integration design | `BOOT.md` (at repo root) |

# minix.rs: Implementation Plan

## Context

Build minix.rs as a learning OS that preserves MINIX 3's microkernel architecture -- message-passing IPC, user-space servers (PM, VFS, VM, RS, DS, SCHED), user-space drivers, and fine-grained privilege control -- but with a greenfield Rust kernel targeting modern 64-bit platforms (x86_64, aarch64) under QEMU/VirtIO. Someone familiar with MINIX 3 or the Tanenbaum book should recognize the concepts immediately.

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
|                     minix.rs Microkernel (Rust)                  |
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
minixrs/
  Cargo.toml                    # Workspace root
  rust-toolchain.toml           # Pinned nightly
  .cargo/config.toml            # Per-target flags, linker scripts
  LICENSE                       # BSD-3-Clause

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

**Deliverables:** `docs/` directory with architecture, IPC, syscall catalog, servers, boot, drivers, musl, memory layout, build, and MINIX 3 mapping documentation. Cargo workspace with all crate placeholders. CLAUDE.md, LICENSE (BSD-3-Clause).

**Milestone:** `docs/` is comprehensive enough to start coding Phase 1 without re-exploring MINIX 3.

### Phase 1: Kernel Scaffolding + Boot (aarch64, QEMU virt) (complete)

- Cargo workspace with `kernel`, `kernel-shared` crates
- Builds via `aarch64-unknown-none` + linker script (`kernel/src/arch/aarch64/linker.ld`); the bespoke `aarch64-minix-kernel.json` target spec is deferred to Phase 2 (the stock target is sufficient for boot)
- Vendor Limine v9.x binary + header (`external/limine/Makefile`); Rust-side request block in `kernel/src/arch/aarch64/limine.rs` (base revision, HHDM, memmap, paging mode, stack size)
- PL011 UART driver at `kernel/src/arch/aarch64/uart.rs` with `core::fmt::Write` adapter; MMIO base is HHDM-relative (Limine base revision 2 keeps the [0, 4 GiB) blanket map covering PL011)
- aarch64 exception vector table (`vectors.S` + `exception.rs`); any unexpected trap routes through `exception_entry` and panics with a decoded ESR_EL1/ELR_EL1/FAR_EL1 dump
- `tools/qemu-run.sh` is the cargo runner: stages an ESP under `target/esp/`, auto-detects edk2 firmware, boots with `qemu-system-aarch64 -M virt` via `-drive file=fat:rw:...`
- **Milestone:** `cargo run -p minixrs-kernel --target aarch64-unknown-none --release` boots through UEFI + Limine, prints `minix.rs booting on aarch64` + HHDM offset, then halts in `wfe`. Exception path verified by injecting a deliberate fault and observing a clean panic dump.

### Phase 2: Kernel IPC + Scheduling (the heart of MINIX)

Complete — 6 PR-sized slices, PRs #3–#8, merged 2026-05-20 → 2026-05-25. Each
slice was independently buildable, booted, and produced observable output; the
Phase 2 milestone ("two processes exchange IPC messages") was satisfied at the
end of slice 2.5, with 2.6 finishing the kernel-call surface needed by Phase 3.
Full slice history: [`docs/plans/phase-2-ipc.md`](plans/phase-2-ipc.md).

- **2.1** ✓ `kernel-shared` foundation — Message, Endpoint, call numbers, errnos (PR #3, 2026-05-20)
- **2.2** ✓ `Proc`/`Priv` structs, static tables, boot `IMAGE` (PR #4, 2026-05-22)
- **2.3** ✓ aarch64 SVC entry + cooperative context switch + first EL0 stub (PR #5, 2026-05-22)
- **2.4** ✓ GICv3 + ARM generic timer + priority-banded run queues (PR #6, 2026-05-23)
- **2.5** ✓ IPC primitives (`mini_send`/`receive`/`notify`/`sendnb`, deadlock check) (PR #7, 2026-05-23) — **milestone**
- **2.6** ✓ kernel-call dispatch + real `SYS_GETINFO` + `ENOSYS` stubs (PR #8, 2026-05-25)

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

Complete — 7 PR-sized slices, PRs #9–#15 and #21, merged 2026-05-27 →
2026-06-13. The milestone ("boot processes each have isolated address spaces;
VM handles page faults") was satisfied at the end of slice 3.4; 3.5/3.6 added
brk + mmap on top. Architecture choices locked by the phase plan: per-process
TTBR0 + 8-bit ARMv8 ASIDs, kernel writes all user PTEs (VM passes decisions in
via `SYS_VMCTL` subcalls), kernel reads cross-AS user memory via HHDM, VM uses
static `[Region; N]` per-proc tables (no allocator), stubs A/B/C kept as
regression coverage. POSIX fork/exec were deferred to Phase 4 (PM-driven).
Full slice history: [`docs/plans/phase-3-vm.md`](plans/phase-3-vm.md).

- **3.1a** ✓ physical frame allocator + `AddrSpace` API (PR #9, 2026-05-27)
- **3.1b** ✓ per-process TTBR0s + ASIDs + fault diagnostics (PR #10, 2026-05-27)
- **3.2** ✓ EL0 page-fault handler + `RTS_PAGEFAULT` + stub D (PR #11, 2026-05-28)
- **3.3** ✓ real `SYS_VMCTL` subcalls, stub D self-managing its heap (PR #12, 2026-06-01)
- **3.4** ✓ VM server + kernel-originated `VM_PAGEFAULT` send (PRs #13/#14, 2026-06-04) — **milestone**
- **3.5** ✓ VM region tracking + `VM_BRK` (PR #15, 2026-06-08)
- **3.6** ✓ `VM_MMAP`/`VM_MUNMAP` + Phase 3 doc cleanup (PR #21, 2026-06-13)

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

### Phase 4: Core Servers (PM, VFS, RS, DS, SCHED) + init

Complete — 8 PR-sized slices (9 PRs), PRs #23–#31, merged 2026-06-14 →
2026-07-18. Two scope decisions shaped the phase: **exec is real but
boot-embedded** (no filesystem/musl until Phase 5, so `SYS_EXEC` loads ELF
binaries packed into the boot-image archive; Phase 5 swaps the source to a VFS
file with no PM/kernel rework), and **scheduling moved to a real user-space
SCHED** by making the kernel scheduler *delegatable* rather than replacing it.
Full slice history: [`docs/plans/phase-4-servers.md`](plans/phase-4-servers.md).

- **4.1** ✓ `server-rt` SEF framework, VM migrated onto it, `minix-ipc` finished (PR #23, 2026-06-14)
- **4.2** ✓ MXBI multi-module boot image + DS server + skeletal VFS (PR #24, 2026-06-21)
- **4.3** ✓ delegatable kernel scheduler + SCHED server (PR #25, 2026-06-27)
- **4.4** ✓ RS heartbeat monitor + real `SYS_SETALARM` (PR #26, 2026-06-27)
- **4.5** ✓ PM part A — mproc, getpid, real `SYS_PRIVCTL`, minimal signals (PR #27, 2026-07-17)
- **4.6** ✓ PM part B — fork/exit/wait: kernel half (PR #28), PM/VM/stub-E half (PR #29), 2026-07-18
- **4.7** ✓ exec — `SYS_EXEC` + PM exec of the boot-embedded worker (PR #30, 2026-07-18)
- **4.8** ✓ init (PID 1) + Phase 4 wrap-up + docs (PR #31, merged 2026-07-18) — **milestone; Phase 4 complete**

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

### Pre-Phase-5 cleanup ◀ next

Phase 4's close-out review identified PR-sized cleanup/prep chunks to land
before Phase 5 starts: a CI QEMU smoke job, the mdBook content port + legacy
`docs/` retirement, a stub A–D disable flag, capacity-ceiling unification, a
toolchain bump, a kernel-crate de-hosting investigation, and — gating
Phase 5 — a dedicated Phase 5 design + slicing session. Tracked with the
usual markers in [`docs/plans/phase-5-prep.md`](plans/phase-5-prep.md);
chunks land one per session/PR, in any order except the design session,
which must come last. Chunk 1 (the CI QEMU smoke job) is `◀ ready`
(branch `feature/ci-qemu-smoke`, pending merge).

### Phase 5: musl Fork + File Systems

On deck after the pre-Phase-5 cleanup. The slice decomposition and design
decisions (console/stdio sink, root-image strategy, grant model, ELF-loading
authority, cbindgen/ABI-freeze timing) come out of the design session tracked
in [`docs/plans/phase-5-prep.md`](plans/phase-5-prep.md); this section is
rewritten as a slice list when that lands. Grants/safecopy and a fault-safe
user copy are the expected opening slices — they are Phase 5 feature work.

- Add `src/minix/` to musl fork with IPC wrappers
- Generate C headers from `kernel-shared` via cbindgen
- `fs/mfs/`: MINIX File System server (Rust, MinixFS v3 on-disk format)
- `fs/pfs/`: Pipe File System
- `drivers/memory/`: /dev/null, /dev/zero, ramdisk
- initramfs for early boot before disk driver
- **Milestone:** C "Hello World" compiled against musl runs on minix.rs

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
- **Milestone:** Same minix.rs boots on `qemu-system-x86_64`

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
  -drive file=minixrs.img,format=raw,if=virtio \
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

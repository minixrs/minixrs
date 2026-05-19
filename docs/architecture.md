# MINIX 4 System Architecture

## Overview

MINIX 4 is a microkernel operating system written in Rust, designed as a learning OS for
people familiar with MINIX 3 and Andrew Tanenbaum's *Operating Systems: Design and
Implementation*. It preserves MINIX 3's core architectural principles -- message-passing IPC,
user-space servers, user-space drivers, and fine-grained privilege control -- while targeting
modern 64-bit hardware (aarch64, x86_64) running under QEMU.

### What Makes MINIX a Microkernel

In a monolithic kernel (Linux, FreeBSD), the entire OS -- file systems, device drivers,
networking, memory management -- runs in a single privileged address space. A bug in any
driver can crash the whole system.

MINIX takes the opposite approach. The kernel handles only four things:

1. **IPC (Inter-Process Communication)** -- Message passing between processes
2. **Scheduling** -- Deciding which process runs next
3. **Interrupt dispatch** -- Routing hardware interrupts to driver processes
4. **Memory protection** -- Setting up page tables so processes can't access each other's memory

Everything else -- the file system, process management, memory management, device drivers --
runs as separate user-space processes that communicate by sending messages through the kernel.

```
+------------------------------------------------------------------+
|                        User Programs                              |
|  (linked against musl-libc, POSIX-compatible)                    |
+------------------------------------------------------------------+
         |  _syscall(endpoint, callnr, &msg)  via SVC/SYSCALL
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
|  aarch64 HAL             |        x86_64 HAL                     |
|  SVC/ERET, GIC, TT      |        SYSCALL/SYSRET, APIC, PT       |
+------------------------------------------------------------------+
|                     Limine Bootloader                             |
+------------------------------------------------------------------+
```

### Relation to MINIX 3

MINIX 4 preserves MINIX 3's system call interface and server model. A developer familiar
with MINIX 3 will recognize:

- The same IPC primitives (SEND, RECEIVE, SENDREC, NOTIFY, SENDNB, SENDA)
- The same server roles (PM, VFS, VM, RS, DS, SCHED)
- The same system call routing (user -> libc -> IPC message -> server -> reply)
- The same kernel call mechanism for privileged servers
- The same driver protocols (BDEV_*, CDEV_*)
- The same SEF (System Events Framework) for server lifecycle

What's different:

| Aspect | MINIX 3 | MINIX 4 |
|--------|---------|---------|
| Kernel language | C | Rust |
| Target architectures | i386, ARM (earm) | aarch64, x86_64 |
| Bootloader | Custom 3-stage / Multiboot | Limine (UEFI) |
| C library | NetBSD libc (modified) | musl-libc fork |
| Userland | NetBSD commands | Custom minimal (Rust) |
| Build system | NetBSD make | Cargo workspace + make (musl) |
| License | Mix (BSD + GPL in userland) | BSD-2-Clause only |
| IPC linked lists | Raw C pointers | Table indices (`Option<ProcNr>`) |
| Message types | Opaque unions (m1i1, m2l1) | Named Rust structs |

## System Components

### The Microkernel

The kernel is the only code that runs in privileged mode (EL1 on aarch64, ring 0 on x86_64).
It is roughly 5,000-10,000 lines of Rust plus ~500 lines of assembly.

**Responsibilities:**

- **IPC dispatch** -- Copy messages between processes, manage send/receive queues, detect
  deadlocks. See [docs/ipc.md](ipc.md).
- **Scheduling** -- Maintain priority-based run queues. Pick the highest-priority runnable
  process. Handle timer-based preemption (quantum expiry).
- **Interrupt dispatch** -- Route hardware interrupts to the driver process that registered
  for them. Interrupts become NOTIFY messages.
- **Kernel calls** -- Handle privileged operations requested by system servers (SYS_FORK,
  SYS_EXEC, SYS_VMCTL, etc.). These are NOT available to regular user programs.

**What the kernel does NOT do:**

- File system operations (VFS + MFS handle these)
- Process lifecycle management (PM handles fork/exec/exit)
- Virtual memory management (VM handles page faults, mmap)
- Device I/O (drivers handle this)
- Networking (future TCP/IP server)

### System Servers

Each server is a separate user-space process with its own address space. Servers communicate
exclusively through IPC messages. See [docs/servers.md](servers.md) for details.

| Server | Role | MINIX 3 equivalent |
|--------|------|--------------------|
| **PM** (Process Manager) | fork, exec, exit, wait, signals, UIDs/GIDs | `minix/servers/pm/` |
| **VFS** (Virtual File System) | File operations, routes to FS drivers | `minix/servers/vfs/` |
| **VM** (Virtual Memory) | Page faults, mmap, process memory | `minix/servers/vm/` |
| **RS** (Reincarnation Server) | Monitor/restart services, manage lifecycle | `minix/servers/rs/` |
| **DS** (Data Store) | Key-value publish/subscribe for crash recovery | `minix/servers/ds/` |
| **SCHED** (Scheduler) | User-space scheduling policy | `minix/servers/sched/` |

### Device Drivers

Drivers are user-space processes, just like servers. They communicate with the kernel
(for interrupt notifications and I/O port access) and with VFS (for block/character device
protocols) via IPC.

| Driver | Role | Protocol |
|--------|------|----------|
| **virtio-blk** | Block storage | BDEV_OPEN/CLOSE/READ/WRITE/IOCTL |
| **virtio-net** | Network interface | Packet send/receive |
| **virtio-console** | Terminal (TTY) | CDEV_OPEN/CLOSE/READ/WRITE/SELECT |
| **memory** | /dev/null, /dev/zero, ramdisk | CDEV + BDEV |

See [docs/drivers.md](drivers.md) for the driver model.

### File Systems

File system servers implement the REQ_* protocol to handle VFS requests:

| FS | Role |
|----|------|
| **MFS** | MINIX File System (MinixFS v3 on-disk format) |
| **PFS** | Pipe File System (in-memory, for pipes and FIFOs) |

### C Library (musl fork)

User programs link against a fork of musl-libc. The fork replaces musl's Linux syscall
layer with MINIX IPC message passing:

```
User calls read(fd, buf, n)
  -> musl read.c constructs Message { m_type: VFS_READ, ... }
  -> _syscall(VFS_PROC_NR, VFS_READ, &msg)
  -> ipc_sendrec() -> SVC instruction (aarch64) or SYSCALL (x86_64)
  -> kernel delivers message to VFS, blocks caller
  -> VFS processes, replies
  -> kernel unblocks caller, delivers reply
  -> _syscall() returns, musl extracts result/errno
  -> read() returns to user
```

See [docs/musl.md](musl.md) for the integration plan.

## The System Call Path

MINIX has two kinds of calls:

### POSIX System Calls (for user programs)

User programs make POSIX calls (open, read, write, fork, etc.) which the C library
translates into IPC messages sent to the appropriate server:

```
open()  -> message to VFS  (VFS_OPEN)
fork()  -> message to PM   (PM_FORK)
mmap()  -> message to VM   (VM_MMAP)
kill()  -> message to PM   (PM_KILL)
```

The user program never talks to the kernel directly for these. The `_syscall()` function
in musl uses `ipc_sendrec()` (SENDREC IPC primitive) to send a message and wait for the
reply, which traps into the kernel's IPC dispatcher.

### Kernel Calls (for privileged servers only)

Servers need to ask the kernel to perform privileged operations (manipulate page tables,
set up process contexts, configure interrupt routing). These use a separate mechanism:

```
PM calls sys_fork()   -> SYS_FORK kernel call
VM calls sys_vmctl()  -> SYS_VMCTL kernel call
RS calls sys_privctl() -> SYS_PRIVCTL kernel call
```

Kernel calls are messages sent to the SYSTEM task (a kernel-internal pseudo-process).
Only processes with the appropriate bit set in their `k_call_mask` can make each call.

See [docs/syscalls.md](syscalls.md) for the complete catalog.

## Privilege Model

Every system process has a `Priv` structure that controls:

- **`trap_mask`** -- Which IPC operations it can use (SEND, RECEIVE, etc.)
- **`ipc_to`** -- Bitmap of which other processes it can send messages to
- **`k_call_mask`** -- Bitmap of which kernel calls it can make
- **`io_ranges`** -- Which hardware I/O ports it can access (x86_64)
- **`irqs`** -- Which interrupt lines it can register for
- **`grant_table`** -- Memory grants for controlled cross-process memory access

Regular user programs have no privilege structure -- they can only use SENDREC to
communicate with servers listed in their IPC permissions.

This fine-grained model means a compromised driver cannot access kernel calls it
doesn't need, cannot talk to servers it has no business with, and cannot access
hardware resources outside its domain.

## Memory Layout

See [docs/memory-layout.md](memory-layout.md) for the detailed virtual/physical memory
layout on each architecture.

Key points:
- Each process has its own page tables (isolated address space)
- The kernel is mapped into the high virtual addresses of every process
- Limine provides a Higher Half Direct Map (HHDM) for the kernel to access physical memory
- User space occupies the lower half of the virtual address space

## Boot Sequence

See [docs/boot.md](boot.md) for the full boot sequence.

Summary:
1. Limine bootloader loads kernel ELF from FAT32 boot partition
2. Kernel initializes arch-specific hardware (exception vectors, interrupt controller, timer)
3. Kernel unpacks embedded boot image (12 server/driver ELF binaries)
4. Kernel loads each boot module into its own address space with appropriate privileges
5. Boot processes start: DS -> RS -> PM -> SCHED -> VFS -> memory -> tty -> VM -> PFS -> MFS -> init
6. init opens /dev/console and spawns a shell

## Crate Structure

The project is organized as a Cargo workspace:

```
kernel/          -- Microkernel (no_std, no_main)
kernel-shared/   -- Message types, endpoints, call numbers (no_std, used by all)
minix-ipc/       -- User-space IPC library (SVC/SYSCALL stubs)
server-rt/       -- Server runtime / SEF framework
servers/         -- PM, VFS, VM, RS, DS, SCHED
drivers/         -- VirtIO block/net/console, memory, driver-rt library
fs/              -- MFS, PFS
userland/        -- init, sh, coreutils
```

The `kernel-shared` crate is the glue -- it defines the `Message` struct, endpoint
constants, system call numbers, and error codes used by both the kernel and all
user-space components. Having these as Rust types provides compile-time verification
of the IPC protocol.

## Further Reading

- [IPC Design](ipc.md) -- Message format, primitives, deadlock detection
- [System Calls](syscalls.md) -- Complete call catalog
- [Servers](servers.md) -- Server responsibilities and IPC flows
- [Drivers](drivers.md) -- Driver model and VirtIO
- [Boot Sequence](boot.md) -- From Limine to shell prompt
- [Memory Layout](memory-layout.md) -- Virtual/physical address spaces
- [musl Integration](musl.md) -- C library adaptation
- [Build System](build.md) -- How to build and run
- [MINIX 3 Mapping](minix3-mapping.md) -- Where things moved from MINIX 3

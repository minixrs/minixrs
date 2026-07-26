# Architecture

minix.rs is a microkernel operating system written in Rust. It preserves MINIX
3's core architectural principles — message-passing IPC, user-space servers, and
a fine-grained privilege model — while dropping 32-bit legacy and targeting
modern 64-bit platforms under QEMU. **aarch64** (Apple-Silicon / QEMU `virt`) is
the primary target; an x86_64 port is planned (see [Roadmap](../roadmap.md)).

This chapter is the map of the system as it stands at the end of Phase 4. It
describes the pieces that boot today and cross-references the chapters that cover
each in depth. Where a MINIX-3 concept exists only as a plan, it is called out as
such rather than described in the present tense.

## What makes it a microkernel

In a monolithic kernel (Linux, the BSDs), the whole OS — file systems, device
drivers, networking, memory management — runs in one privileged address space, so
a bug in any driver can take down the system. MINIX takes the opposite approach,
and minix.rs follows it: the kernel handles only four things.

1. **IPC** — copying fixed-size messages between processes, managing the
   send/receive queues, and detecting deadlocks.
2. **Scheduling** — priority run queues and timer-driven preemption.
3. **Interrupt dispatch** — routing hardware interrupts to the process that
   registered for them, delivered as `NOTIFY` messages.
4. **Memory protection** — owning the physical frame allocator and performing
   every page-table write, so processes cannot reach into each other's memory.

Everything else runs as an ordinary user-space process that communicates only
through messages:

```text
+------------------------------------------------------------------+
| User programs   (init, worker; musl-linked C programs — planned) |
+------------------------------------------------------------------+
        |  SENDREC(server, &msg)  via the SVC IPC trap
        v
+--------+--------+--------+--------+--------+---------------------+
|  PM    |  VFS   |  VM    |  RS    |  DS    |  SCHED              |
| fork   | (skel- | page-  | moni-  | name → | quantum             |
| exec   |  etal) | fault  | tor    | endpt  | delegation          |
| exit   |        | brk    |        | regis- |                     |
| wait   |        | mmap   |        | try    |                     |
| signal |        |        |        |        |                     |
+--------+--------+--------+--------+--------+---------------------+
|  Device drivers (VirtIO) and file systems (MFS/PFS) — planned    |
+------------------------------------------------------------------+
        |  IPC messages (SEND / RECEIVE / SENDREC / NOTIFY / SENDNB)
        v
+------------------------------------------------------------------+
|                    minix.rs microkernel (Rust)                   |
|  IPC | scheduling | interrupt dispatch | memory protection       |
|  Kernel calls (SYS_*) for privileged servers                     |
+------------------------------------------------------------------+
|  aarch64 HAL: SVC/ERET, GICv3, translation tables                |
|  (x86_64 — planned)                                              |
+------------------------------------------------------------------+
|                    Limine boot protocol (UEFI)                   |
+------------------------------------------------------------------+
```

## Relation to MINIX 3

A developer who knows MINIX 3 will recognize the IPC primitives, the server
roles, and the two-tier call model (user → server → reply for POSIX calls, server
→ kernel for privileged operations). What differs:

| Aspect | MINIX 3 | minix.rs |
|--------|---------|----------|
| Kernel language | C | Rust (`no_std`, `no_main`) |
| Target architectures | i386, 32-bit ARM | aarch64 (x86_64 planned) |
| Bootloader | custom / multiboot | Limine (UEFI) |
| C library | NetBSD libc | musl fork (planned, Phase 5) |
| Userland | NetBSD commands | minimal Rust (`init`, `worker`) |
| License | mixed (BSD + GPL) | BSD-3-Clause only |
| IPC linked lists | raw C pointers | table indices (`Option<ProcNr>`) |
| Message payload | opaque unions (`m1i1`, …) | typed structs per call |

minix.rs preserves MINIX 3's *ABI* — the 104-byte message layout, endpoint
encoding, and call-numbering conventions — as a reference point, not by shipping
MINIX 3 code. See [System Calls & ABI](../reference/syscalls.md) and
[MINIX 3 Source Mapping](../reference/minix3-mapping.md).

## The microkernel

The kernel is the only code that runs privileged (EL1 on aarch64). Its
responsibilities are the four listed above; concretely it provides:

- **IPC dispatch** — see [IPC](../ipc/overview.md).
- **Scheduling** — priority run queues, quantum expiry, and a *delegatable*
  scheduler: a process can be scheduled by the kernel or handed to the user-space
  SCHED server. See [Servers](../servers/overview.md).
- **Interrupt and timer handling** — the ARM generic timer drives a 100 Hz tick;
  hardware IRQ routing to driver processes is groundwork for the driver era.
- **Kernel calls (`SYS_*`)** — privileged operations requested by system servers
  (`SYS_FORK`, `SYS_EXEC`, `SYS_VMCTL`, `SYS_KILL`, …), gated per process. These
  are *not* available to ordinary user programs.

What the kernel does **not** do: file-system operations, process-lifecycle policy
(PM), page-fault policy (VM), device I/O, or networking.

## System servers

Each server is a separate user-space process with its own address space,
communicating only through IPC. The full treatment is in
[Servers](../servers/overview.md); in brief:

| Server | Role | State at Phase 4 |
|--------|------|------------------|
| **PM** | fork, exec, exit, wait, minimal signals | real |
| **VM** | page-fault resolution, `brk`, `mmap`, `munmap` | real |
| **RS** | monitor / heartbeat system processes | real (detect-only) |
| **DS** | name → endpoint registry | real |
| **SCHED** | user-space scheduling policy | real |
| **VFS** | file-operation switch | skeletal (no file ops yet) |

Device drivers and file-system servers (MFS/PFS) are user-space processes in the
same model, but exist only as stubs today — they belong to later phases (see
[Roadmap](../roadmap.md)).

## The two call paths

MINIX has two kinds of calls, and minix.rs keeps both.

**POSIX system calls** — a user program's `open`/`read`/`fork` becomes an IPC
message to the responsible server (`fork` → PM, file ops → VFS, `mmap` → VM). The
program never traps into the kernel for these; it uses `SENDREC` to send a request
and block for the reply. Today the user side of this path is exercised directly
through the `minixrs-ipc` crate (by `init` and `worker`); the musl wrappers that
will make it transparent to C programs arrive in Phase 5.

**Kernel calls (`SYS_*`)** — servers ask the kernel for privileged operations
(edit a page table, fork a process slot, raise a signal) by sending a `SENDREC` to
the `SYSTEM` task. Each call is gated by the caller's `k_call_mask`; a process
without the bit gets an error. The full catalog is in
[System Calls & ABI](../reference/syscalls.md).

## Privilege model

Every *system* process has a privilege-table entry
(`kernel/src/proc/priv_struct.rs`) that controls what it may do. User processes
share a single user-class slot. The live fields are:

- **`trap_mask`** (`u16`) — which IPC primitives the process may use. Bit `i`
  allows primitive `i`; ordinary user processes typically get only `SENDREC`.
- **`ipc_to`** — bitmap of which other privileged processes it may send to.
- **`k_call_mask`** — bitmap of which kernel calls it may make (empty for user
  processes).
- **`sig_mgr`** — the endpoint that manages signals raised against it (PM, for
  user processes).

The struct also carries `io_ranges`, `irqs`, `mem_ranges`, and a `grant_table`
pointer. These are forward-declared for the driver and grant eras — device I/O,
IRQ handler registration, and grant-validated safe-copy are Phase 5+/driver work,
not yet exercised. This fine-grained model is what lets a future compromised
driver be denied kernel calls it doesn't need and servers it has no business
talking to.

## Memory and boot

- Memory management — the kernel-owned frame allocator, per-process address
  spaces, the page-fault path, and VM's region tracking — is covered in
  [Memory Management](../memory/overview.md).
- The boot path — Limine, the aarch64 entry sequence, and the embedded MXBI boot
  image — is covered in [Boot](../boot/overview.md).

## Crate structure

The project is a single Cargo workspace:

```text
kernel/          microkernel (no_std, no_main)
kernel-shared/   message types, endpoints, call numbers (shared by all crates)
minixrs-ipc/       user-space IPC library (the SVC trap stubs)
server-rt/       server runtime / SEF framework
servers/         pm, vfs, vm, rs, ds, sched
drivers/         virtio-blk/net/console, memory, driver-rt   (stubs — planned)
fs/              mfs, pfs                                     (stubs — planned)
userland/        init, worker (real); sh, coreutils          (stubs — planned)
```

`kernel-shared` is the glue: it defines the `Message` struct, endpoint encoding,
call numbers, and error codes used by both the kernel and every user-space
component, so the IPC protocol is checked at compile time.

## Further reading

- [Boot](../boot/overview.md) — Limine to first user process
- [IPC](../ipc/overview.md) — message format, primitives, deadlock detection
- [Memory Management](../memory/overview.md) — frames, address spaces, VM
- [Servers](../servers/overview.md) — server responsibilities and IPC flows
- [Build & Toolchain](../build.md) — building, running, and trace forensics
- [System Calls & ABI](../reference/syscalls.md) — the call catalog
- [MINIX 3 Source Mapping](../reference/minix3-mapping.md) — where things moved
- [Roadmap](../roadmap.md) — drivers, musl, file systems, x86_64

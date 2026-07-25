# Roadmap

> **Mostly planned — parts of Phase 5 now boot.** This chapter is design intent for
> phases beyond Phase 4, but Phase 5 has begun landing and the sections below are no
> longer uniformly future tense: grants and the fault-safe copy engine are live
> (slices 5.1–5.2), and so is the first user-space driver — TTY, which owns the
> PL011 and serves `CDEV_WRITE` (slice 5.3, see [Drivers](drivers/overview.md)).
> Still absent: the musl fork, the VirtIO drivers, the file-system servers, and any
> x86_64 port. The rest of this book describes what *does* run; this chapter
> describes where the project is headed. For live phase status and slice tracking,
> see the repository's `docs/plan.md`.

## Grants and safe copy (Phase 5, first)

Every interesting data path beyond Phase 4 — VFS read/write, FS ↔ VFS, and later
block I/O — moves bytes across address spaces, and a 96-byte message payload can't
carry them. MINIX solves this with **grants**: a process publishes a grant table
(via `SYS_SETGRANT`) describing memory regions and permitted operations
(read / write); it passes a grant ID in a message; the peer copies through
`SYS_SAFECOPY`, which the kernel authorizes against the grant table before moving
any bytes. This keeps the kernel from having to trust a raw pointer from one
process on behalf of another.

The call numbers exist as stubs today (see
[System Calls & ABI](reference/syscalls.md)). A real grant table plus a
fault-safe user copy (returning `EFAULT` on a bad pointer rather than panicking
the kernel) is the expected opening work of Phase 5, because the musl and
file-system slices depend on it.

## musl C library

User C programs will link against a fork of [musl](https://musl.libc.org/)
(MIT-licensed) whose Linux syscall layer is replaced with MINIX IPC. A POSIX call
becomes a message to a server rather than a Linux `syscall`:

```text
read(fd, buf, n)
  → construct Message { m_type: VFS_READ, fd, buf_ptr, count }
  → _syscall(VFS_PROC_NR, VFS_READ, &msg)   // SENDREC via the IPC trap
  → VFS handles it, replies
  → _syscall() extracts the result / errno
```

The fork's shape:

- **`_syscall()`** — the central routing function: set `m_type`, `ipc_sendrec`,
  and translate a negative reply into `errno` + `-1`. Mirrors MINIX 3's
  `lib/libc/sys/syscall.c`.
- **IPC trap stubs** — a tiny `.S` per architecture issuing the trap. On aarch64
  that is `svc #0` with the register convention from [IPC](ipc/overview.md)
  (`x0` = endpoint, `x1` = primitive, `x2` = message).
- **~100 POSIX wrappers** — one per call (`open`, `read`, `write`, `fork`,
  `execve`, `mmap`, `brk`, …), each constructing a message and calling `_syscall`.
- **A cbindgen bridge** — the C headers (`Message`, endpoint constants, call
  numbers) generated from the `kernel-shared` Rust crate, so the wire protocol has
  a single source of truth. This also means the `kernel-shared` ABI needs to be
  frozen deliberately before the header bridge is stood up.
- **A cross-compiled sysroot** — `libc.a` + the `crt*.o` startup files, linked into
  user programs.

The `printf` "Hello World" milestone that closes the early Phase-5 work needs a
console/stdio sink; whether that is a kernel diagnostic call, a minimal TTY, or
deferred is a design decision to be locked in the Phase-5 plan.

## File systems

Two file-system servers are planned, each speaking the `REQ_*` protocol to VFS
(which is skeletal today — see [Servers](servers/overview.md)):

- **MFS** — the MINIX File System (MinixFS v3 on-disk format).
- **PFS** — the Pipe File System (in-memory, for pipes and FIFOs).

A root image is also needed before block drivers exist — likely an initramfs or an
MXBI-embedded FS image, another Phase-5 design decision. FS-backed `exec` will
reuse the kernel's ELF loader (`kernel/src/boot_image/elf.rs`).

## Device drivers

Drivers are user-space processes, like servers — a buggy driver can crash only
itself, and RS restarts it. They talk to the kernel (interrupts, I/O) and to VFS
(device protocols) over IPC.

- **Block drivers** answer the `BDEV_*` protocol (`OPEN`/`CLOSE`/`READ`/`WRITE`/
  `IOCTL`, plus scatter-gather); **character drivers** answer `CDEV_*`
  (`OPEN`/`CLOSE`/`READ`/`WRITE`/`IOCTL`/`SELECT`). Bulk data rides grants +
  `SYS_SAFECOPY`.
- **Interrupts** arrive as `NOTIFY` messages from the `HARDWARE` endpoint after a
  driver registers a handler with `SYS_IRQCTL`. The kernel masks the line, notifies
  the driver, and the driver re-enables it — no interrupt storms.
- **VirtIO transport** — MMIO on aarch64 (devices memory-mapped in the QEMU `virt`
  device tree), PCI on x86_64. Devices exchange data through **virtqueues**: a
  descriptor table plus an available ring (driver → device) and a used ring
  (device → driver).
- The planned set is `virtio-blk`, `virtio-net`, `virtio-console` (a VirtIO TTY),
  and a hardware-free `memory` driver (`/dev/null`, `/dev/zero`, ramdisk).
- A `driver-rt` crate will provide the reusable `BlockDriver` / `CharDriver`
  traits and the VirtIO transport types. Only a console story is needed for the
  Phase-5 `printf` milestone; the rest is Phase 6.

## Copy-on-write fork

`fork` today eagerly gives the child its own frames (the kernel copies page tables
via `SYS_FORK`; VM clones the region set via `VM_FORK` — see
[Memory Management](memory/overview.md)). Copy-on-write fork — sharing pages
read-only and duplicating only on the first write fault — is a later optimization,
not a Phase-4/5 requirement.

## x86_64 port

aarch64 is the primary target. The kernel is structured for a second architecture
(the HAL split, the `x86_64-unknown-none` target scaffolding, and the ABI-neutral
`kernel-shared` types), but the x86_64 path — `SYSCALL`/`SYSRET`, GDT/IDT, APIC,
the x86_64 IPC register ABI — is not implemented. It is a late phase.

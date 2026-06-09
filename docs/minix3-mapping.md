# MINIX 3 to minix.rs Mapping

This table maps MINIX 3 source files and concepts to their minix.rs equivalents.
Use this if you're coming from the MINIX 3 codebase or the Tanenbaum book and want
to find where things live in minix.rs.

## Kernel

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `kernel/proc.c` (mini_send) | `kernel/src/ipc/send.rs` | Same algorithm, Rust |
| `kernel/proc.c` (mini_receive) | `kernel/src/ipc/receive.rs` | Same algorithm |
| `kernel/proc.c` (mini_notify) | `kernel/src/ipc/notify.rs` | Same algorithm |
| `kernel/proc.c` (deadlock) | `kernel/src/ipc/deadlock.rs` | Same algorithm |
| `kernel/proc.c` (do_ipc) | `kernel/src/ipc/mod.rs` | IPC dispatch |
| `kernel/proc.h` (struct proc) | `kernel/src/proc/table.rs` (Proc) | `Option<ProcNr>` replaces raw pointers |
| `kernel/priv.h` (struct priv) | `kernel/src/proc/privilege.rs` (Priv) | Same fields |
| `kernel/system.c` | `kernel/src/system/mod.rs` | Kernel call dispatch table |
| `kernel/system/do_fork.c` | `kernel/src/system/do_fork.rs` | Per-call handler files |
| `kernel/system/do_exec.c` | `kernel/src/system/do_exec.rs` | |
| `kernel/system/do_copy.c` | `kernel/src/system/do_copy.rs` | |
| `kernel/system/do_irqctl.c` | `kernel/src/system/do_irqctl.rs` | |
| `kernel/system/do_vmctl.c` | `kernel/src/system/do_vmctl.rs` | |
| `kernel/clock.c` | `kernel/src/clock.rs` | Timer interrupt handler |
| `kernel/interrupt.c` | `kernel/src/interrupt.rs` | IRQ hook framework |
| `kernel/main.c` (kmain) | `kernel/src/main.rs` (kmain) | Entry point |
| `kernel/table.c` | `kernel/src/boot_image.rs` | Boot process table |
| `kernel/arch/i386/head.S` | `kernel/src/arch/aarch64/entry.S` | Arch-specific entry |
| `kernel/arch/i386/mpx.S` | `kernel/src/arch/aarch64/entry.S` | Context switch asm |
| `kernel/arch/i386/protect.c` | `kernel/src/arch/aarch64/exception.rs` | Vectors/GIC |
| `kernel/arch/i386/memory.c` | `kernel/src/arch/aarch64/mmu.rs` | Page tables |

## Shared Headers

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `include/minix/ipc.h` | `kernel-shared/src/message.rs` | Message struct |
| `include/minix/ipcconst.h` | `kernel-shared/src/ipc_const.rs` | SEND, RECEIVE, etc. |
| `include/minix/com.h` | `kernel-shared/src/com.rs` | Server endpoints, SYS_* numbers |
| `include/minix/callnr.h` | `kernel-shared/src/callnr.rs` | PM_*, VFS_* call numbers |
| `include/minix/type.h` | `kernel-shared/src/lib.rs` | Type aliases |
| `include/minix/safecopies.h` | `kernel-shared/src/grant.rs` | Grant types |

## Libraries

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `lib/libsys/sef.c` | `server-rt/src/sef.rs` | SEF framework |
| `lib/libsys/sef_init.c` | `server-rt/src/init.rs` | Init callbacks |
| `lib/libsys/sef_ping.c` | `server-rt/src/ping.rs` | Heartbeat |
| `lib/libsys/sef_signal.c` | `server-rt/src/signal.rs` | Signal handling |
| `lib/libc/sys/syscall.c` | `musl/src/minix/_syscall.c` | _syscall() wrapper |
| `lib/libc/arch/x86_64/sys/_ipc.S` | `musl/src/minix/_ipc_aarch64.S` | IPC trap |
| `lib/libc/sys/open.c` | `musl/src/minix/open.c` | POSIX wrappers |
| `lib/libc/sys/read.c` | `musl/src/minix/read.c` | |
| `lib/libc/sys/fork.c` | `musl/src/minix/fork.c` | |
| `lib/libblockdriver/` | `drivers/driver-rt/src/block.rs` | Block driver framework |
| `lib/libchardriver/` | `drivers/driver-rt/src/char.rs` | Char driver framework |
| `lib/libsys/kernel_call.c` | `minix-ipc/src/lib.rs` | Kernel call wrapper |

## Servers

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `servers/pm/main.c` | `servers/pm/src/main.rs` | PM main loop |
| `servers/pm/forkexit.c` | `servers/pm/src/fork.rs` | fork/exit handlers |
| `servers/pm/exec.c` | `servers/pm/src/exec.rs` | exec handler |
| `servers/pm/signal.c` | `servers/pm/src/signal.rs` | Signal handling |
| `servers/pm/table.c` | `servers/pm/src/table.rs` | Dispatch table |
| `servers/vfs/main.c` | `servers/vfs/src/main.rs` | VFS main loop |
| `servers/vfs/open.c` | `servers/vfs/src/open.rs` | Open handler |
| `servers/vfs/read.c` | `servers/vfs/src/read.rs` | Read/write |
| `servers/vfs/path.c` | `servers/vfs/src/path.rs` | Path resolution |
| `servers/vfs/mount.c` | `servers/vfs/src/mount.rs` | Mount handling |
| `servers/vfs/worker.c` | `servers/vfs/src/worker.rs` | Thread pool |
| `servers/vm/main.c` | `servers/vm/src/main.rs` | VM main loop |
| `servers/vm/pagefaults.c` | `servers/vm/src/fault.rs` | Page fault handler |
| `servers/vm/region.c` | `servers/vm/src/region.rs` | Memory regions |
| `servers/vm/mmap.c` | `servers/vm/src/mmap.rs` | mmap handler |
| `servers/rs/main.c` | `servers/rs/src/main.rs` | RS main loop |
| `servers/rs/manager.c` | `servers/rs/src/manager.rs` | Service lifecycle |
| `servers/ds/main.c` | `servers/ds/src/main.rs` | DS main loop |
| `servers/ds/store.c` | `servers/ds/src/store.rs` | Key-value store |
| `servers/sched/main.c` | `servers/sched/src/main.rs` | Scheduler |

## Drivers

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `drivers/storage/virtio_blk/` | `drivers/virtio-blk/` | VirtIO block |
| `drivers/storage/memory/` | `drivers/memory/` | /dev/null, ramdisk |
| `drivers/tty/` | `drivers/virtio-console/` | Terminal (VirtIO) |
| `drivers/net/virtio_net/` | `drivers/virtio-net/` | VirtIO network |

## File Systems

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `servers/mfs/` or `minix/fs/mfs/` | `fs/mfs/` | MINIX File System |
| `servers/pfs/` | `fs/pfs/` | Pipe File System |

## Build System

| MINIX 3 | minix.rs | Notes |
|---------|---------|-------|
| `build.sh` | `Cargo.toml` (workspace) | Root build config |
| NetBSD Makefiles | Cargo + tools/ scripts | Build orchestration |
| `releasetools/` | `tools/mkimage.sh` | Image creation |

## Concepts Unchanged

These MINIX 3 concepts carry over directly to minix.rs:

- Message-passing IPC with fixed-size messages
- Six IPC primitives (SEND, RECEIVE, SENDREC, NOTIFY, SENDNB, SENDA)
- Endpoint-based process identification with generation numbers
- Privilege bitmaps (trap_mask, ipc_to, k_call_mask)
- Grant-based safe memory access
- SEF server lifecycle (init, ping, signal)
- BDEV/CDEV driver protocols
- REQ_* filesystem protocol between VFS and FS servers
- Boot image with ordered server startup
- RS heartbeat monitoring and crash recovery
- DS publish/subscribe for state recovery

## Concepts Changed

| MINIX 3 | minix.rs | Why |
|---------|---------|-----|
| Raw C pointers for IPC queues | `Option<ProcNr>` indices | Memory safety |
| `m1i1`/`m2l1` message fields | Named typed structs | Readability |
| `EXTERN` macro globals | Rust module-scoped statics | Language idiom |
| `RTS_SET()`/`RTS_UNSET()` macros | `rts_set()`/`rts_unset()` functions | Type safety |
| `volatile` flags | `AtomicU32` | Rust atomics |
| Integer error returns | `Result<T, E>` | Rust error handling |
| `#ifdef` arch selection | `cfg(target_arch)` | Rust conditional compilation |
| NetBSD libc | musl-libc fork | BSD license, simpler |
| NetBSD userland | Custom Rust coreutils | BSD license, minimal |

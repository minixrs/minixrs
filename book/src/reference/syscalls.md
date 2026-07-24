# System Calls & ABI

This is a reference chapter. It documents two things and keeps them distinct:

1. **What dispatches today** — the kernel calls the microkernel actually handles
   at the end of Phase 4, and the server request ranges minix.rs uses on the
   wire.
2. **The MINIX ABI minix.rs targets** — the POSIX call catalog (PM and VFS) that
   the musl wrappers and file-system servers will wire up in Phase 5 and beyond.
   These numbers are the MINIX 3 reference ABI; they are *not* the request
   numbers minix.rs currently sends.

## The two call paths

MINIX has two kinds of "system call," and minix.rs keeps both.

**POSIX calls (user → server).** A user program's `open` / `read` / `fork`
becomes an IPC message to the responsible server, sent with `SENDREC`:

```text
user program → SENDREC(server_endpoint, &msg) → kernel IPC → server → reply
```

The kernel routes the message but does not interpret the call. Today this path is
driven directly through the `minix-ipc` crate (by `init` and `worker`, using the
live PM request numbers below). The musl C wrappers that will make it transparent
to C programs — `read(fd, buf, n)` constructing the message for you — arrive in
Phase 5.

**Kernel calls (server → kernel).** A privileged server asks the kernel for a
low-level operation by sending a `SENDREC` to the `SYSTEM` task
(`m_type` = the call number). Each call is gated by the caller's `k_call_mask`; a
process without the bit gets an error.

## Kernel calls (`SYS_*`) — live today

Kernel-call numbers are contiguous from `KERNEL_CALL = 0x600`
(`kernel-shared/src/callnr.rs`). Phase 4 defines 18. Twelve have real handlers;
six are placeholder stubs whose consumers arrive later (grants in Phase 5, IRQ
control in the driver era). The dispatch table is `kernel/src/system/mod.rs`.

| Number | Call | Status | Purpose |
|-------:|------|--------|---------|
| `0x600` | `SYS_GETINFO` | live | Kernel introspection (e.g. `GET_WHOAMI`). |
| `0x601` | `SYS_PRIVCTL` | live | Set up a target's privilege slot (`PRIVCTL_SET_USER`). |
| `0x602` | `SYS_FORK` | live | Clone a process slot as a frozen child. |
| `0x603` | `SYS_EXEC` | live | Replace a target's image with a boot-embedded binary. |
| `0x604` | `SYS_EXIT` | live | Full process teardown (address space, endpoint, slot). |
| `0x605` | `SYS_COPY` | stub | Inter-space copy — placeholder. |
| `0x606` | `SYS_SAFECOPY` | stub | Grant-validated copy — real grants are Phase 5. |
| `0x607` | `SYS_IRQCTL` | stub | IRQ handler registration — driver era. |
| `0x608` | `SYS_VMCTL` | live | VM's paging-mechanism call (see subcalls below). |
| `0x609` | `SYS_SCHEDULE` | live | Set a process's priority and quantum. |
| `0x60A` | `SYS_SETALARM` | live | Arm/cancel a per-process one-shot alarm. |
| `0x60B` | `SYS_TIMES` | stub | Process accounting times — placeholder. |
| `0x60C` | `SYS_DIAGCTL` | stub | Diagnostic output control — placeholder. |
| `0x60D` | `SYS_SETGRANT` | stub | Register the caller's grant table — Phase 5. |
| `0x60E` | `SYS_SCHEDCTL` | live | Claim/release a process for a user-space scheduler. |
| `0x60F` | `SYS_KILL` | live | Raise a signal on a target (queues toward PM). |
| `0x610` | `SYS_GETKSIG` | live | PM: fetch the next process with pending signals. |
| `0x611` | `SYS_ENDKSIG` | live | PM: acknowledge signal processing for a target. |

### `SYS_VMCTL` subcalls

`SYS_VMCTL` is VM's single privileged lever over the kernel's paging mechanism.
The subcall selector is in the first payload word; the target process (endpoint,
`SELF` allowed) in the next. Numbers start at 1 so a zeroed payload is invalid.

| Subcall | Effect |
|---------|--------|
| `VMCTL_PT_MAP` (1) | Allocate a fresh zeroed frame, map it at `vaddr`, reply with the PA. |
| `VMCTL_PT_UNMAP` (2) | Unmap `vaddr` and free the frame (`EINVAL` if nothing mapped). |
| `VMCTL_CLEAR_PAGEFAULT` (3) | Clear a recorded fault and make the target runnable. |
| `VMCTL_GET_PAGEFAULT` (4) | Read the target's recorded fault coordinates. |
| `VMCTL_VMINHIBIT_SET` / `_CLEAR` (5/6) | Gate scheduling while VM mutates the target's AS. |

See [Memory Management](../memory/overview.md) for how VM uses these.

## Server request ranges — live today

Server requests are ordinary IPC `m_type` values, not kernel calls. Each server
occupies a distinct band below `NOTIFY_MESSAGE` (`0x1000`), const-asserted
disjoint in `kernel-shared/src/callnr.rs`:

| Base | Value | Server / requests |
|------|-------|-------------------|
| `PM_RQ_BASE`    | `0x700` | PM: `GETPID` / `FORK` / `EXIT` / `WAIT` / `EXEC` |
| `VM_RQ_BASE`    | `0xC00` | VM: `PAGEFAULT` / `BRK` / `MMAP` / `MUNMAP` / `FORK` |
| `SEF_RQ_BASE`   | `0xD00` | SEF control: `INIT` / `SIGNAL` |
| `DS_RQ_BASE`    | `0xE00` | DS: `PUBLISH` / `RETRIEVE` / `CHECK` |
| `SCHED_RQ_BASE` | `0xF00` | SCHED: `NO_QUANTUM` / `START` / `STOP` / `SET_NICE` |

The [Servers](../servers/overview.md) chapter documents each request. These
minix.rs-specific numbers are what actually travels on the wire today — distinct
from the MINIX 3 POSIX ABI numbers catalogued below.

## The MINIX ABI catalog (target)

> **Reference, not current behavior.** The tables below are the MINIX 3 POSIX
> call ABI that minix.rs is heading toward — the numbering musl and the
> file-system servers will adopt. They describe the *interface*, not what is
> dispatched today: VFS is skeletal (it handles none of these yet), and PM's live
> request numbers are the `0x700` band above, not these. Treat this as the map of
> Phase 5+ work.

### PM calls (base `0x000`, target)

The Process Manager handles process lifecycle, signals, credentials, and timing.

| Call | # | POSIX | Description |
|------|--:|-------|-------------|
| `PM_EXIT` | 1 | `_exit` | Terminate the caller |
| `PM_FORK` | 2 | `fork` | Create a child process |
| `PM_WAIT4` | 3 | `wait4` | Wait for a child to change state |
| `PM_GETPID` | 4 | `getpid` | Get the caller's PID |
| `PM_SETUID`/`GETUID` | 5/6 | `setuid`/`getuid` | Real user ID |
| `PM_KILL` | 11 | `kill` | Send a signal |
| `PM_SETGID`/`GETGID` | 12/13 | `setgid`/`getgid` | Real group ID |
| `PM_EXEC` | 14 | `execve` | Execute a new program image |
| `PM_SETSID`/`GETPGRP` | 15/16 | `setsid`/`getpgrp` | Session / process group |
| `PM_ITIMER` | 17 | `setitimer` | Interval timer |
| `PM_SIGACTION` … `PM_SIGRETURN` | 20–24 | `sigaction`, `sigsuspend`, `sigpending`, `sigprocmask` | Signal machinery |
| `PM_GETPRIORITY`/`SETPRIORITY` | 26/27 | `getpriority`/`setpriority` | Scheduling priority |
| `PM_GETTIMEOFDAY` | 28 | `gettimeofday` | Time of day |
| `PM_CLOCK_GETRES`/`GETTIME`/`SETTIME` | 33–35 | `clock_*` | POSIX clocks |
| `PM_GETRUSAGE` | 36 | `getrusage` | Resource usage |
| `PM_REBOOT` | 37 | `reboot` | Reboot / halt |
| `PM_SRV_FORK` … `PM_GETSYSINFO` | 41–47 | (MINIX) | Service management for RS |

Slots 25, 40 and a few others are unused; numbers ≥ 40 are MINIX-specific service
infrastructure. (Full detail: `minix/include/minix/callnr.h`.)

### VFS calls (base `0x100`, target)

The Virtual File System server handles file I/O, directories, mounts, and — in
MINIX 3.4 — BSD sockets.

| Call | # | POSIX | Call | # | POSIX |
|------|--:|-------|------|--:|-------|
| `VFS_READ` | 0 | `read` | `VFS_FCNTL` | 25 | `fcntl` |
| `VFS_WRITE` | 1 | `write` | `VFS_PIPE2` | 26 | `pipe2` |
| `VFS_LSEEK` | 2 | `lseek` | `VFS_UMASK` | 27 | `umask` |
| `VFS_OPEN` | 3 | `open` | `VFS_CHROOT` | 28 | `chroot` |
| `VFS_CREAT` | 4 | `creat` | `VFS_GETDENTS` | 29 | `getdents` |
| `VFS_CLOSE` | 5 | `close` | `VFS_SELECT` | 30 | `select` |
| `VFS_LINK`/`UNLINK` | 6/7 | `link`/`unlink` | `VFS_FCHDIR` | 31 | `fchdir` |
| `VFS_CHDIR` | 8 | `chdir` | `VFS_FSYNC` | 32 | `fsync` |
| `VFS_MKDIR` | 9 | `mkdir` | `VFS_TRUNCATE`/`FTRUNCATE` | 33/34 | `truncate`/`ftruncate` |
| `VFS_MKNOD` | 10 | `mknod` | `VFS_FCHMOD`/`FCHOWN` | 35/36 | `fchmod`/`fchown` |
| `VFS_CHMOD`/`CHOWN` | 11/12 | `chmod`/`chown` | `VFS_UTIMENS` | 37 | `utimensat` |
| `VFS_MOUNT`/`UMOUNT` | 13/14 | `mount`/`umount` | `VFS_STATVFS1`/`FSTATVFS1` | 40/41 | `statvfs`/`fstatvfs` |
| `VFS_ACCESS` | 15 | `access` | `VFS_SOCKET` … `VFS_SHUTDOWN` | 49–63 | BSD sockets |
| `VFS_SYNC` | 16 | `sync` | | | |
| `VFS_RENAME` | 17 | `rename` | | | |
| `VFS_RMDIR` | 18 | `rmdir` | | | |
| `VFS_SYMLINK`/`READLINK` | 19/20 | `symlink`/`readlink` | | | |
| `VFS_STAT`/`FSTAT`/`LSTAT` | 21–23 | `stat`/`fstat`/`lstat` | | | |
| `VFS_IOCTL` | 24 | `ioctl` | | | |

The socket calls (49–63) route through VFS to a socket driver or network stack.
(Full detail: `minix/include/minix/callnr.h`.)

## Safe copy and grants (planned)

Because a message payload is only 96 bytes, larger transfers use MINIX's grant
mechanism rather than passing raw pointers the kernel would have to trust: a
process publishes a grant describing a region and the permitted operation (read /
write), passes the grant ID in a message, and the peer copies through
`SYS_SAFECOPY`, which the kernel authorizes against the grant table. The call
numbers (`SYS_SETGRANT`, `SYS_SAFECOPY`) exist as stubs today; a real grant table
and validated copy are an opening Phase-5 slice — every interesting Phase-5 data
path (VFS read/write, FS ↔ VFS) moves bytes across address spaces and needs them.

## MINIX 3 source references

| File | Purpose |
|------|---------|
| `include/minix/callnr.h` | PM and VFS call numbers |
| `include/minix/com.h` | Kernel-call numbers and IPC constants |
| `lib/libc/sys/syscall.c` | User-space `_syscall()` |
| `lib/libsys/kernel_call.c` | Server-side `_kernel_call()` |

# MINIX 4 System Call Catalog

This document provides a complete catalog of all system calls in MINIX 4, organized by
the server or kernel task that handles them. MINIX uses a microkernel architecture where
system calls are implemented as IPC messages to user-space servers, not as direct kernel
traps (with the exception of kernel calls used by privileged servers).

---

## The System Call Path

### User-Space System Calls (PM and VFS)

When a user program invokes a system call, the request follows this path:

```
User program
  -> musl libc wrapper (e.g., fork(), read(), open())
    -> _syscall(server_endpoint, callnr, &msg)
      -> ipc_sendrec()
        -> SVC / SYSCALL trap (architecture-dependent)
          -> kernel IPC mechanism (sendrec)
            -> target server (PM or VFS)
              -> server processes request
                -> reply message back to caller
```

The `_syscall()` function constructs an IPC message containing the call number and
arguments, then performs a synchronous send-receive (`ipc_sendrec`) to the appropriate
server endpoint. The kernel handles the IPC routing but does not interpret the system
call itself -- it simply delivers the message to the destination server process.

### Kernel Calls (SYS_*)

Privileged servers (PM, VFS, drivers, etc.) use kernel calls for operations that require
direct kernel involvement, such as memory management, process table manipulation, and
device I/O. These follow a different path:

```
Server process
  -> sys_*() wrapper (e.g., sys_fork(), sys_vircopy())
    -> _kernel_call(callnr, &msg)
      -> kernel call dispatch table
        -> handler function in kernel SYSTEM task
          -> reply message back to calling server
```

Kernel calls are restricted by a per-process privilege bitmask. Only servers explicitly
granted access to a given kernel call may invoke it. Unprivileged processes that attempt
kernel calls will receive an error.

---

## 1. PM (Process Manager) Calls

**Base offset:** `0x000`
**Dispatch target:** PM server
**Total call slots:** NR_PM_CALLS = 48

The Process Manager handles process lifecycle, signals, credentials, and timing.

| Call               | Number | POSIX Equivalent     | Description                                                  |
|--------------------|--------|----------------------|--------------------------------------------------------------|
| PM_EXIT            | 1      | `_exit()`            | Terminate the calling process                                |
| PM_FORK            | 2      | `fork()`             | Create a child process                                       |
| PM_WAIT4           | 3      | `wait4()`            | Wait for a child process to change state                     |
| PM_GETPID          | 4      | `getpid()`           | Get the process ID of the calling process                    |
| PM_SETUID          | 5      | `setuid()`           | Set the real user ID of the calling process                  |
| PM_GETUID          | 6      | `getuid()`           | Get the real user ID of the calling process                  |
| PM_STIME           | 7      | `stime()`            | Set the system time (deprecated)                             |
| PM_PTRACE          | 8      | `ptrace()`           | Process tracing and debugging                                |
| PM_SETGROUPS       | 9      | `setgroups()`        | Set supplementary group IDs                                  |
| PM_GETGROUPS       | 10     | `getgroups()`        | Get supplementary group IDs                                  |
| PM_KILL            | 11     | `kill()`             | Send a signal to a process or process group                  |
| PM_SETGID          | 12     | `setgid()`           | Set the real group ID of the calling process                 |
| PM_GETGID          | 13     | `getgid()`           | Get the real group ID of the calling process                 |
| PM_EXEC            | 14     | `execve()`           | Execute a new program image                                  |
| PM_SETSID          | 15     | `setsid()`           | Create a new session and set the process group ID            |
| PM_GETPGRP         | 16     | `getpgrp()`          | Get the process group ID of the calling process              |
| PM_ITIMER          | 17     | `setitimer()`        | Set or get an interval timer                                 |
| PM_GETMCONTEXT     | 18     | `getcontext()`       | Get the machine context of the calling thread                |
| PM_SETMCONTEXT     | 19     | `setcontext()`       | Set the machine context of the calling thread                |
| PM_SIGACTION       | 20     | `sigaction()`        | Examine or change a signal action                            |
| PM_SIGSUSPEND      | 21     | `sigsuspend()`       | Atomically set signal mask and suspend until signal          |
| PM_SIGPENDING      | 22     | `sigpending()`       | Examine pending signals                                      |
| PM_SIGPROCMASK     | 23     | `sigprocmask()`      | Examine or change blocked signals                            |
| PM_SIGRETURN       | 24     | (none)               | Return from signal handler (internal)                        |
| PM_GETPRIORITY     | 26     | `getpriority()`      | Get the scheduling priority of a process                     |
| PM_SETPRIORITY     | 27     | `setpriority()`      | Set the scheduling priority of a process                     |
| PM_GETTIMEOFDAY    | 28     | `gettimeofday()`     | Get the current time of day                                  |
| PM_SETEUID         | 29     | `seteuid()`          | Set the effective user ID                                    |
| PM_SETEGID         | 30     | `setegid()`          | Set the effective group ID                                   |
| PM_ISSETUGID       | 31     | `issetugid()`        | Check if process was started setuid/setgid                   |
| PM_GETSID          | 32     | `getsid()`           | Get the session ID of a process                              |
| PM_CLOCK_GETRES    | 33     | `clock_getres()`     | Get the resolution of a clock                                |
| PM_CLOCK_GETTIME   | 34     | `clock_gettime()`    | Get the current value of a clock                             |
| PM_CLOCK_SETTIME   | 35     | `clock_settime()`    | Set the value of a clock                                     |
| PM_GETRUSAGE       | 36     | `getrusage()`        | Get resource usage statistics                                |
| PM_REBOOT          | 37     | `reboot()`           | Reboot or halt the system                                    |
| PM_SVRCTL          | 38     | (none)               | Server control operations (MINIX-specific)                   |
| PM_SPROF           | 39     | (none)               | Statistical profiling control (MINIX-specific)               |
| PM_SRV_FORK        | 41     | (none)               | Fork a new system service process (MINIX-specific)           |
| PM_SRV_KILL        | 42     | (none)               | Send a signal to a system service (MINIX-specific)           |
| PM_EXEC_NEW        | 43     | (none)               | Execute a new program for a service (MINIX-specific)         |
| PM_EXEC_RESTART    | 44     | (none)               | Restart a service after exec (MINIX-specific)                |
| PM_GETEPINFO       | 45     | (none)               | Get endpoint info for a process (MINIX-specific)             |
| PM_GETPROCNR       | 46     | (none)               | Get the process slot number (MINIX-specific)                 |
| PM_GETSYSINFO      | 47     | (none)               | Get system information tables (MINIX-specific)               |

**Note:** Call number 25 is unused. Call numbers 40 and beyond include MINIX-specific
extensions for the Reincarnation Server (RS) and service management infrastructure.

---

## 2. VFS (Virtual File System) Calls

**Base offset:** `0x100`
**Dispatch target:** VFS server
**Total call slots:** NR_VFS_CALLS = 64

The Virtual File System server handles file I/O, directory operations, mount management,
and (in MINIX 4) BSD-style socket operations.

| Call               | Number | POSIX Equivalent     | Description                                                  |
|--------------------|--------|----------------------|--------------------------------------------------------------|
| VFS_READ           | 0      | `read()`             | Read from a file descriptor                                  |
| VFS_WRITE          | 1      | `write()`            | Write to a file descriptor                                   |
| VFS_LSEEK          | 2      | `lseek()`            | Reposition the file offset                                   |
| VFS_OPEN           | 3      | `open()`             | Open or create a file                                        |
| VFS_CREAT          | 4      | `creat()`            | Create a new file (equivalent to open with O_CREAT\|O_TRUNC) |
| VFS_CLOSE          | 5      | `close()`            | Close a file descriptor                                      |
| VFS_LINK           | 6      | `link()`             | Create a hard link                                           |
| VFS_UNLINK         | 7      | `unlink()`           | Remove a directory entry                                     |
| VFS_CHDIR          | 8      | `chdir()`            | Change the current working directory                         |
| VFS_MKDIR          | 9      | `mkdir()`            | Create a directory                                           |
| VFS_MKNOD          | 10     | `mknod()`            | Create a special or ordinary file                            |
| VFS_CHMOD          | 11     | `chmod()`            | Change file mode bits                                        |
| VFS_CHOWN          | 12     | `chown()`            | Change file owner and group                                  |
| VFS_MOUNT          | 13     | `mount()`            | Mount a filesystem                                           |
| VFS_UMOUNT         | 14     | `umount()`           | Unmount a filesystem                                         |
| VFS_ACCESS         | 15     | `access()`           | Check file accessibility                                     |
| VFS_SYNC           | 16     | `sync()`             | Flush all filesystem caches to disk                          |
| VFS_RENAME         | 17     | `rename()`           | Rename a file or directory                                   |
| VFS_RMDIR          | 18     | `rmdir()`            | Remove a directory                                           |
| VFS_SYMLINK        | 19     | `symlink()`          | Create a symbolic link                                       |
| VFS_READLINK       | 20     | `readlink()`         | Read the value of a symbolic link                            |
| VFS_STAT           | 21     | `stat()`             | Get file status by path                                      |
| VFS_FSTAT          | 22     | `fstat()`            | Get file status by file descriptor                           |
| VFS_LSTAT          | 23     | `lstat()`            | Get file status (do not follow symlinks)                     |
| VFS_IOCTL          | 24     | `ioctl()`            | Device-specific control operations                           |
| VFS_FCNTL          | 25     | `fcntl()`            | File descriptor control operations                           |
| VFS_PIPE2          | 26     | `pipe2()`            | Create a pipe with flags                                     |
| VFS_UMASK          | 27     | `umask()`            | Set the file mode creation mask                              |
| VFS_CHROOT         | 28     | `chroot()`           | Change the root directory                                    |
| VFS_GETDENTS       | 29     | `getdents()`         | Read directory entries                                       |
| VFS_SELECT         | 30     | `select()`           | Synchronous I/O multiplexing                                 |
| VFS_FCHDIR         | 31     | `fchdir()`           | Change working directory by file descriptor                  |
| VFS_FSYNC          | 32     | `fsync()`            | Synchronize a file's state with storage                      |
| VFS_TRUNCATE       | 33     | `truncate()`         | Truncate a file to a specified length                        |
| VFS_FTRUNCATE      | 34     | `ftruncate()`        | Truncate an open file to a specified length                  |
| VFS_FCHMOD         | 35     | `fchmod()`           | Change mode of an open file                                  |
| VFS_FCHOWN         | 36     | `fchown()`           | Change owner of an open file                                 |
| VFS_UTIMENS        | 37     | `utimensat()`        | Set file access and modification times with nanoseconds      |
| VFS_GETVFSSTAT     | 39     | `getvfsstat()`       | Get list of all mounted filesystems                          |
| VFS_STATVFS1       | 40     | `statvfs()`          | Get filesystem statistics by path                            |
| VFS_FSTATVFS1      | 41     | `fstatvfs()`         | Get filesystem statistics by file descriptor                 |
| VFS_SVRCTL         | 43     | (none)               | VFS server control operations (MINIX-specific)               |
| VFS_MAPDRIVER      | 45     | (none)               | Map a device driver to a major device number (MINIX-specific)|
| VFS_COPYFD         | 46     | (none)               | Copy a file descriptor to another endpoint (MINIX-specific)  |
| VFS_SOCKETPATH     | 47     | (none)               | Resolve a Unix domain socket path (MINIX-specific)           |
| VFS_GETSYSINFO     | 48     | (none)               | Get VFS system information tables (MINIX-specific)           |
| VFS_SOCKET         | 49     | `socket()`           | Create a socket endpoint                                     |
| VFS_SOCKETPAIR     | 50     | `socketpair()`       | Create a pair of connected sockets                           |
| VFS_BIND           | 51     | `bind()`             | Bind a name to a socket                                      |
| VFS_CONNECT        | 52     | `connect()`          | Initiate a connection on a socket                            |
| VFS_LISTEN         | 53     | `listen()`           | Listen for connections on a socket                           |
| VFS_ACCEPT         | 54     | `accept()`           | Accept a connection on a socket                              |
| VFS_SENDTO         | 55     | `sendto()`           | Send a message on a socket to a specified address            |
| VFS_SENDMSG        | 56     | `sendmsg()`          | Send a message on a socket with ancillary data               |
| VFS_RECVFROM       | 57     | `recvfrom()`         | Receive a message from a socket                              |
| VFS_RECVMSG        | 58     | `recvmsg()`          | Receive a message from a socket with ancillary data          |
| VFS_SETSOCKOPT     | 59     | `setsockopt()`       | Set a socket option                                          |
| VFS_GETSOCKOPT     | 60     | `getsockopt()`       | Get a socket option                                          |
| VFS_GETSOCKNAME    | 61     | `getsockname()`      | Get the local address of a socket                            |
| VFS_GETPEERNAME    | 62     | `getpeername()`      | Get the remote address of a connected socket                 |
| VFS_SHUTDOWN       | 63     | `shutdown()`         | Shut down part of a full-duplex connection                   |

**Note:** Call numbers 38, 42, and 44 are unused. The socket calls (49-63) were added
in MINIX 3.4 / NetBSD integration and are routed through VFS to the appropriate socket
driver or network stack.

---

## 3. Kernel Calls (SYS_*)

**Base offset:** `0x600`
**Dispatch target:** SYSTEM kernel task
**Total call slots:** NR_SYS_CALLS = 58
**Access:** Privileged servers only (controlled by privilege bitmask)

Kernel calls provide low-level services that require direct kernel involvement: process
table manipulation, memory operations, device I/O, and inter-server coordination.

| Call               | Number | Category         | Description                                                  |
|--------------------|--------|------------------|--------------------------------------------------------------|
| SYS_FORK           | 0      | Process          | Notify kernel of a process fork                              |
| SYS_EXEC           | 1      | Process          | Notify kernel of a process exec (update stack pointer)       |
| SYS_CLEAR          | 2      | Process          | Clean up kernel state for an exiting process                 |
| SYS_SCHEDULE       | 3      | Scheduling       | Set scheduling parameters for a process                      |
| SYS_PRIVCTL        | 4      | Security         | Set or update privilege structure for a process              |
| SYS_TRACE          | 5      | Debugging        | Kernel-level ptrace support operations                       |
| SYS_KILL           | 6      | Signals          | Cause the kernel to send a signal notification               |
| SYS_MEMSET         | 13     | Memory           | Fill a physical memory region with a pattern                 |
| SYS_UMAP           | 14     | Memory           | Map virtual address to physical address                      |
| SYS_VIRCOPY        | 15     | Memory           | Copy data between virtual address spaces                     |
| SYS_PHYSCOPY       | 16     | Memory           | Copy data using physical addresses                           |
| SYS_UMAP_REMOTE    | 17     | Memory           | Map virtual address of a remote process to physical          |
| SYS_VUMAP          | 18     | Memory           | Vectored virtual-to-physical address mapping                 |
| SYS_IRQCTL         | 19     | Interrupts       | Register, enable, or disable an IRQ handler                  |
| SYS_DEVIO          | 21     | Device I/O       | Perform a single I/O port read or write                      |
| SYS_SDEVIO         | 22     | Device I/O       | Perform a string (block) I/O port operation                  |
| SYS_VDEVIO         | 23     | Device I/O       | Perform a vector of I/O port operations                      |
| SYS_SETALARM       | 24     | Timers           | Set or cancel a synchronous alarm for the calling process    |
| SYS_TIMES          | 25     | Timers           | Get process accounting times (user, system, children)        |
| SYS_GETINFO        | 26     | System Info      | Retrieve kernel information (proc table, kinfo, etc.)        |
| SYS_ABORT          | 27     | System Control   | Abort the system (panic / shutdown)                          |
| SYS_IOPENABLE      | 28     | Device I/O       | Enable direct I/O port access for a process                  |
| SYS_SAFECOPYFROM   | 31     | Safe Copy        | Copy data from a grant in another process                    |
| SYS_SAFECOPYTO     | 32     | Safe Copy        | Copy data to a grant in another process                      |
| SYS_VSAFECOPY      | 33     | Safe Copy        | Perform a vector of safe copy operations                     |
| SYS_SETGRANT       | 34     | Safe Copy        | Register the grant table for the calling process             |
| SYS_SETTIME        | 40     | Timers           | Set the system clock (real-time and/or boot time)            |
| SYS_VMCTL          | 43     | Virtual Memory   | VM control operations (page faults, memory maps, TLB)       |
| SYS_DIAGCTL        | 44     | Diagnostics      | Diagnostic output control (kernel log, console output)       |
| SYS_RUNCTL         | 46     | Process          | Control process execution (stop/resume for update)           |
| SYS_GETMCONTEXT    | 50     | Context          | Get the machine context of a process                         |
| SYS_SETMCONTEXT    | 51     | Context          | Set the machine context of a process                         |
| SYS_UPDATE         | 52     | Live Update      | Swap two processes for live update                           |
| SYS_EXIT           | 53     | Process          | Notify kernel that a system process is exiting               |
| SYS_SCHEDCTL       | 54     | Scheduling       | Configure the kernel scheduler for a process                 |
| SYS_STATECTL       | 55     | System Control   | Control system-wide state (e.g., hibernation)                |
| SYS_SAFEMEMSET      | 56     | Safe Copy        | Fill a memory region in another process via grant            |

**Note:** Call numbers 7-12, 20, 29-30, 35-39, 41-42, 45, and 47-49 are unused or
reserved. Gaps in the numbering reflect calls that were removed or consolidated during
the MINIX 3 evolution.

### Kernel Call Categories

- **Process** (0-2, 46, 53): Process lifecycle notifications from PM to the kernel.
- **Scheduling** (3, 54): Scheduler parameter management.
- **Security** (4): Privilege assignment and restriction.
- **Debugging** (5): Kernel support for ptrace.
- **Signals** (6): Kernel-mediated signal delivery.
- **Memory** (13-18): Address translation and inter-process memory operations.
- **Interrupts** (19): Hardware interrupt management for device drivers.
- **Device I/O** (21-23, 28): Port-level I/O for device drivers.
- **Timers** (24-25, 40): Alarm and time management.
- **System Info** (26): Kernel data structure queries.
- **System Control** (27, 55): Shutdown and system state management.
- **Safe Copy** (31-34, 56): Grant-based secure inter-process data transfer.
- **Virtual Memory** (43): Page table and TLB management via VM server.
- **Diagnostics** (44): Kernel log and console output control.
- **Context** (50-51): Machine context get/set for signal handling and context switching.
- **Live Update** (52): Process state migration for runtime service replacement.

---

## Safe Copy and Grants

The safe copy mechanism (SYS_SAFECOPYFROM, SYS_SAFECOPYTO, SYS_VSAFECOPY, SYS_SETGRANT,
SYS_SAFEMEMSET) deserves special mention as it is central to MINIX IPC security.

Rather than passing raw virtual addresses between processes (which would require the
kernel to trust the sender), MINIX uses a grant table system:

1. A process creates a grant entry describing a memory region and the permitted
   operations (read, write, or both).
2. The grant ID is passed in an IPC message to the other process.
3. The receiving process uses `sys_safecopyfrom()` or `sys_safecopyto()` with the
   grant ID. The kernel validates that the grant exists, is owned by the claimed
   process, and permits the requested operation before performing the copy.

This prevents a malicious or buggy driver from reading or writing arbitrary memory in
other processes.

---

## MINIX 3 Source References

The call numbers and definitions in this catalog are derived from the following MINIX 3
source files, which serve as the authoritative reference for the MINIX 4 system call
interface:

| File                                        | Purpose                                            |
|---------------------------------------------|----------------------------------------------------|
| `minix/include/minix/callnr.h`             | PM and VFS call number definitions                 |
| `minix/include/minix/com.h`                | Kernel call number definitions and IPC constants   |
| `minix/lib/libc/sys/syscall.c`             | User-space `_syscall()` implementation             |
| `minix/lib/libsys/kernel_call.c`           | Server-side `_kernel_call()` implementation        |

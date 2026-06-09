# musl-libc Integration

## Overview

minix.rs uses a fork of [musl-libc](https://musl.libc.org/) (v1.2.5, MIT license) as its C library.
The fork replaces musl's Linux syscall layer with MINIX message-passing IPC, so that standard
POSIX functions like `open()`, `read()`, `fork()` route through MINIX servers instead of Linux
system calls.

The musl fork should be a clean checkout of musl v1.2.5 with a working branch for MINIX-specific changes.

## What Changes

### The Key Difference: Linux vs MINIX

**Linux musl (original):**
```
read(fd, buf, n)
  -> __syscall3(SYS_read, fd, buf, n)
  -> syscall instruction with Linux ABI
  -> kernel read() handler
  -> return
```

**minix.rs musl (fork):**
```
read(fd, buf, n)
  -> construct Message { m_type: VFS_READ, fd, buf_ptr, count }
  -> _syscall(VFS_PROC_NR, VFS_READ, &msg)
  -> ipc_sendrec(VFS_PROC_NR, &msg)
  -> SVC/SYSCALL instruction (IPC trap, not Linux syscall)
  -> kernel delivers message to VFS server
  -> VFS processes, replies
  -> _syscall() extracts result/errno
  -> return
```

### Files to Add

All new files go in `musl/src/minix/`:

**Core IPC mechanism:**

| File | Purpose |
|------|---------|
| `_syscall.c` | `int _syscall(endpoint_t who, int callnr, message *m)` -- send SENDREC and extract result |
| `_ipc_aarch64.S` | IPC trap via `SVC #0` for aarch64 |
| `_ipc_x86_64.S` | IPC trap via `SYSCALL` for x86_64 |

**POSIX wrappers (~100 files):**

Each POSIX function that was previously a Linux syscall becomes a wrapper that constructs
a MINIX message and calls `_syscall()`. Examples:

| File | POSIX function | Server | Message type |
|------|---------------|--------|-------------|
| `open.c` | `open()` | VFS | VFS_OPEN / VFS_CREAT |
| `read.c` | `read()` | VFS | VFS_READ |
| `write.c` | `write()` | VFS | VFS_WRITE |
| `close.c` | `close()` | VFS | VFS_CLOSE |
| `stat.c` | `stat()`, `fstat()`, `lstat()` | VFS | VFS_STAT / VFS_FSTAT / VFS_LSTAT |
| `lseek.c` | `lseek()` | VFS | VFS_LSEEK |
| `mkdir.c` | `mkdir()` | VFS | VFS_MKDIR |
| `unlink.c` | `unlink()` | VFS | VFS_UNLINK |
| `rename.c` | `rename()` | VFS | VFS_RENAME |
| `ioctl.c` | `ioctl()` | VFS | VFS_IOCTL |
| `fork.c` | `fork()` | PM | PM_FORK |
| `exit.c` | `_exit()` | PM | PM_EXIT |
| `exec.c` | `execve()` | PM | PM_EXEC |
| `wait.c` | `waitpid()`, `wait4()` | PM | PM_WAIT4 |
| `kill.c` | `kill()` | PM | PM_KILL |
| `getpid.c` | `getpid()` | PM | PM_GETPID |
| `sigaction.c` | `sigaction()` | PM | PM_SIGACTION |
| `mmap.c` | `mmap()` | VM | VM_MMAP |
| `brk.c` | `brk()`, `sbrk()` | VM | VM_BRK |

### Files to Modify

| File | Change |
|------|--------|
| `arch/aarch64/syscall_arch.h` | Replace Linux `__syscall*` macros with stubs that redirect to MINIX IPC |
| `arch/x86_64/syscall_arch.h` | Same for x86_64 |
| `Makefile` | Add `src/minix/` to source list |
| `src/internal/syscall.h` | Adjust `SYS_*` definitions to MINIX message types |

### Include Bridge (cbindgen)

The musl fork needs C headers for the MINIX message types, endpoint constants, and call
numbers. These are generated from the `kernel-shared` Rust crate using `cbindgen`:

```sh
cbindgen --config cbindgen.toml --crate minixrs-kernel-shared \
    --output musl/include/minix/kernel_shared.h
```

This produces:
- `minix/ipc.h` -- `Message` struct, typed message variants
- `minix/com.h` -- Server endpoint constants (PM_PROC_NR, VFS_PROC_NR, etc.)
- `minix/callnr.h` -- System call numbers (PM_FORK, VFS_READ, etc.)
- `minix/type.h` -- `endpoint_t`, `vir_bytes`, etc.

## The _syscall() Function

This is the central routing function, equivalent to MINIX 3's `_syscall()` in
`lib/libc/sys/syscall.c`:

```c
#include <minix/ipc.h>
#include <minix/com.h>
#include <errno.h>

int _syscall(endpoint_t who, int syscallnr, message *msgptr)
{
    int status;

    msgptr->m_type = syscallnr;
    status = ipc_sendrec(who, msgptr);  /* SVC/SYSCALL trap */

    if (status != 0) {
        /* IPC failure (should not happen in normal operation) */
        msgptr->m_type = status;
    }

    if (msgptr->m_type < 0) {
        errno = -msgptr->m_type;
        return -1;
    }

    return msgptr->m_type;
}
```

## IPC Assembly Stubs

### aarch64 (`_ipc_aarch64.S`)

```asm
// int ipc_sendrec(endpoint_t dst, message *msg)
.global ipc_sendrec
ipc_sendrec:
    // x0 = destination endpoint (passed through)
    // x1 = message pointer (passed through)
    mov x2, x1          // x2 = message pointer
    mov x1, x0          // x1 = destination endpoint
    mov x0, #3          // x0 = SENDREC (3)
    svc #0              // Trap to kernel
    ret                 // x0 = return value (0 or error)
```

### x86_64 (`_ipc_x86_64.S`)

```asm
// int ipc_sendrec(endpoint_t dst, message *msg)
.global ipc_sendrec
ipc_sendrec:
    // rdi = destination, rsi = message pointer
    mov %rsi, %rdx      // rdx = message pointer
    mov %rdi, %rbx      // rbx = destination endpoint (callee-saved)
    mov $3, %eax         // rax = SENDREC (3)
    syscall              // Trap to kernel
    ret                  // rax = return value
```

## Example: How open() Works

```c
// musl/src/minix/open.c

#include <minix/ipc.h>
#include <minix/callnr.h>
#include <minix/com.h>
#include <fcntl.h>
#include <stdarg.h>
#include <string.h>

int open(const char *path, int flags, ...)
{
    message m;
    int call;
    va_list ap;

    memset(&m, 0, sizeof(m));

    if (flags & O_CREAT) {
        va_start(ap, flags);
        mode_t mode = va_arg(ap, mode_t);
        va_end(ap);

        m.m_lc_vfs_creat.name = (vir_bytes)path;
        m.m_lc_vfs_creat.len = strlen(path) + 1;
        m.m_lc_vfs_creat.flags = flags;
        m.m_lc_vfs_creat.mode = mode;
        call = VFS_CREAT;
    } else {
        m.m_lc_vfs_path.name = (vir_bytes)path;
        m.m_lc_vfs_path.len = strlen(path) + 1;
        m.m_lc_vfs_path.flags = flags;
        call = VFS_OPEN;
    }

    return _syscall(VFS_PROC_NR, call, &m);
}
```

## Cross-Compilation

### Building musl for minix.rs aarch64

```sh
cd musl/
CC=clang --target=aarch64-unknown-none \
  CFLAGS="-nostdinc -I../kernel-shared/include/generated" \
  ./configure --prefix=/opt/minixrs-sysroot/usr \
              --target=aarch64-minix
make -j$(nproc)
make install
```

This produces:
- `lib/libc.a` -- Static C library
- `lib/crt1.o`, `lib/crti.o`, `lib/crtn.o` -- C runtime startup files

### Using the sysroot with Rust

The `libc` Rust crate can link against musl by setting:

```toml
# .cargo/config.toml
[target.aarch64-minix-user]
linker = "clang"
rustflags = [
    "-C", "link-arg=--sysroot=/opt/minixrs-sysroot",
    "-C", "link-arg=-nostdlib",
]
```

## MINIX 3 Reference

| Aspect | MINIX 3 File |
|--------|-------------|
| _syscall() | `lib/libc/sys/syscall.c` |
| IPC stubs (x86_64) | `lib/libc/arch/x86_64/sys/_ipc.S` |
| POSIX wrappers | `lib/libc/sys/*.c` (open.c, read.c, fork.c, etc.) |
| Message types | `include/minix/ipc.h` |
| Call numbers | `include/minix/callnr.h` |

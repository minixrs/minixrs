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
driven directly through the `minixrs-ipc` crate (by `init` and `worker`, using the
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
| `0x603` | `SYS_EXEC` | live | Replace a target's image, from the boot archive or a grant (see below). |
| `0x604` | `SYS_EXIT` | live | Full process teardown (address space, endpoint, slot). |
| `0x605` | `SYS_COPY` | stub | Inter-space copy — placeholder. |
| `0x606` | `SYS_SAFECOPY` | stub | Grant-validated copy — real grants are Phase 5. |
| `0x607` | `SYS_IRQCTL` | stub | IRQ handler registration — driver era. |
| `0x608` | `SYS_VMCTL` | live | VM's paging-mechanism call (see subcalls below). |
| `0x609` | `SYS_SCHEDULE` | live | Set a process's priority and quantum. |
| `0x60A` | `SYS_SETALARM` | live | Arm/cancel a per-process one-shot alarm. |
| `0x60B` | `SYS_TIMES` | stub | Process accounting times — placeholder. |
| `0x60C` | `SYS_DIAGCTL` | live | Print inline text to the console (see subcodes below). |
| `0x60D` | `SYS_SETGRANT` | stub | Register the caller's grant table — Phase 5. |
| `0x60E` | `SYS_SCHEDCTL` | live | Claim/release a process for a user-space scheduler. |
| `0x60F` | `SYS_KILL` | live | Raise a signal on a target (queues toward PM). |
| `0x610` | `SYS_GETKSIG` | live | PM: fetch the next process with pending signals. |
| `0x611` | `SYS_ENDKSIG` | live | PM: acknowledge signal processing for a target. |

### The `SYS_EXEC` initial stack

`SYS_EXEC` does not merely set `sp` to the top of the new image's stack page — it
builds the **SysV/Linux initial process stack** there first and points `SP_EL0`
at it. That is what lets a C runtime start unpatched: musl's `crt1` →
`__libc_start_main` → `__init_libc` reads `argc`/`argv`/`envp` and the auxiliary
vector straight off the stack, takes `libc.page_size` from `AT_PAGESZ`, and lets
`__init_tls` walk the program headers from `AT_PHDR`. Keeping the musl fork's
diff confined to `arch/aarch64/syscall_arch.h` + `src/minix/` depends on the
kernel supplying this frame rather than crt being taught a new shape.

The layout, upward from `sp` (16-byte aligned, as the AAPCS64 requires at entry —
`SCTLR_EL1.SA0` turns a violation into an EL0 alignment abort):

| Offset | Contents |
|-------:|----------|
| `+0`   | `argc` (`u64`) |
| `+8`   | `argv[0]` — a VA pointing at the name string below |
| `+16`  | `argv` NULL terminator |
| `+24`  | `envp` NULL terminator (the environment is empty) |
| `+32`  | auxiliary vector: `(a_type, a_val)` pairs, 16 bytes each |
| …      | `(AT_NULL, 0)` — auxv terminator |
| …      | the NUL-terminated name string |
| …      | zero padding up to the stack-page top |

There is exactly one argument (`argc == 1`, `argv[0]` the exec name) and no
environment. Slice 5.9 added exec-from-FS and deliberately **did not** add
user-supplied `argv`/`envp`: the kernel keeps synthesising `argc = 1` with
`argv[0]` set to the path's *basename*, which is what leaves `EXEC_NAME_LEN`,
`PROC_NAME_LEN`, and this frame's whole geometry untouched by that slice. Real
`argv`/`envp` need a place to carry an unbounded vector, which is a separate
design rather than a field. The auxiliary vector is emitted in a fixed order
rather than Linux's incidental one, so traces and tests are deterministic:

| Entry | Value | Emitted |
|-------|-------|---------|
| `AT_PHDR` (3) | VA the program headers are readable at | only when a `PT_LOAD` maps them |
| `AT_PHNUM` (5) | `e_phnum` | with `AT_PHDR` |
| `AT_PHENT` (4) | `e_phentsize` (56) | with `AT_PHDR` |
| `AT_PAGESZ` (6) | 4096 | always |
| `AT_NULL` (0) | 0 | always, last |

### The `SYS_EXEC` payload and its two source forms

Slice 5.9 gave `SYS_EXEC` a **source selector**, so the same call number covers
loading a boot-archive module and loading a file the filesystem staged.

| Offset | Field |
|-------:|-------|
| `0..4` | target endpoint (`i32`) |
| `4..20` | `argv[0]` / the new proc name, NUL-padded (`EXEC_NAME_LEN` = 16) |
| `20..24` | source selector — `EXEC_SRC_NAME` (1) or `EXEC_SRC_GRANT` (2); 0 is invalid |
| `24..28` | granter endpoint (`i32`, grant form only) |
| `28..32` | grant id (`i32`, grant form only) |
| `32..40` | image length (`u64`, grant form only) |

`4..20` is `argv[0]` and the proc's new name in **both** forms; only where the
image's bytes come from changes. In the name form that field doubles as the MXBI
module name.

The grant form is decision D6: the kernel keeps ELF authority, and PM/VFS do the
staging. VFS reads the whole file into its own buffer and direct-grants it to PM;
PM names that grant here; the kernel reads the ELF **through the grant** using the
same page-walking copy engine every `SYS_SAFECOPY` uses, a header at a time onto
its own stack. There is no kernel filesystem, no kernel heap, and no kernel
staging buffer. The grant is validated by the ordinary `verify_grant` — `who_to`
must be PM's own stored endpoint — and the read completes **before the point of no
return**, so a granted buffer that turns out not to be an ELF leaves the target
untouched on its old image and PM relays `ENOEXEC` to it.

There is deliberately no grant-*offset* field: the granted buffer holds the image
from its start, the rule the BDEV and FS bands already state.

`AT_PHDR` is conditional because it must not be invented: a linker script that
does not pull the ELF header into the first `PT_LOAD` leaves `e_phoff` in an
unmapped file prefix, and reporting a VA there would fault `__init_tls`.
`userland/worker/user.ld` uses the `FILEHDR PHDRS` idiom (plus
`. = <base> + SIZEOF_HEADERS`) so its first `PT_LOAD` starts at file offset 0 and
does cover the headers. Boot servers are loaded by a different path
(`load_boot_server`), get no frame at all, and start with `sp` at the page top —
their `_start` reads nothing.

The byte layout lives in `kernel-shared/src/execstack.rs`
(`build_initial_stack`), which is pure and host-tested; `do_exec` stages the
frame in a kernel-stack buffer and installs it with `mm::uaccess::copy_to_user_as`
against the new address space's `ttbr0_pa` — that primitive is
address-space-independent, so the frame lands before the AS is ever installed. A
frame that cannot be built (`E2BIG`) or copied (`ENOMEM`) tears the fresh image
back down and leaves the target on its old one. The `AT_*` values are the
Linux/SysV ones and are deliberately **not** emitted by `gen-c-headers`: musl
defines them itself.

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

### `SYS_DIAGCTL` subcodes

`SYS_DIAGCTL` is the servers' debug channel. Servers run at EL0 with no console
of their own, so without it their behavior is only observable indirectly, through
kernel-side traces. `server-rt::diag_print` is the client side.

Unlike MINIX 3, which passes a `(buf, len)` user pointer and copies the text in,
minix.rs carries the text **inline in the message payload**: the subcode in
payload `0..4`, the length in `4..8`, and up to `DIAG_TEXT_MAX` (88) text bytes
from `DIAG_TEXT_OFF` (8). The channel therefore needs no user-copy machinery and
cannot fault — it has to keep working while the copy engine and grants are
themselves under construction. `diag_print` splits longer strings across
successive calls, one console line each.

| Subcode | Effect |
|---------|--------|
| `DIAGCTL_CODE_DIAG` (1) | Print the inline text as one `[diag <name>] …` line. |
| `DIAGCTL_CODE_STACKTRACE` (2) | Reserved (MINIX 3) — `EINVAL`. |
| `DIAGCTL_CODE_REGISTER` / `_UNREGISTER` (3/4) | Reserved (MINIX 3 kernel-message subscription) — `EINVAL`. |

The `<name>` prefix is the caller's name as the *kernel* knows it, never
anything from the payload, so a server can only ever identify itself. Text is
restricted to printable ASCII, which keeps one call to exactly one line — the
boot-marker checks in `tests/qemu-boot.expected` depend on that framing. No extra
privilege gate is needed: `Priv::k_call_mask` already limits kernel calls to
server-grade privileges, and the shared USER privilege has an empty mask, so
ordinary user processes cannot reach this call.

## Server request ranges — live today

Server requests are ordinary IPC `m_type` values, not kernel calls. Each server
occupies a distinct band below `NOTIFY_MESSAGE` (`0x1000`), const-asserted
disjoint in `kernel-shared/src/callnr.rs`:

| Base | Value | Server / requests |
|------|-------|-------------------|
| `PM_RQ_BASE`    | `0x700` | PM: `GETPID` / `FORK` / `EXIT` / `WAIT` / `EXEC` |
| `VFS_RQ_BASE`   | `0x800` | VFS: `WRITE` / `OPEN` / `READ` / `CLOSE` / `EXEC_STAGE` |
| `FS_RQ_BASE`    | `0x900` | MFS: `READSUPER` / `LOOKUP` / `READ` / `WRITE` / `CREATE` / `TRUNC` |
| `BDEV_RQ_BASE`  | `0xA00` | block drivers: `READ` / `WRITE` (slice 5.7) |
| `CDEV_RQ_BASE`  | `0xB00` | character drivers: `WRITE` (slice 5.3) / `READ` (5.11) |
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

## Generated C headers

The ABI above is described to C by headers **generated from the live Rust
constants** and never committed (phase-5 decision D8):

| Header | Contents |
|--------|----------|
| `minixrs/ipc.h` | the `message` struct, endpoint packing macros, IPC primitives |
| `minixrs/com.h` | process numbers and the boot endpoints derived from them |
| `minixrs/callnr.h` | kernel-call numbers and the server request bands |
| `minixrs/errno.h` | the MINIX 200-band errnos; an opt-in check on the C library's POSIX ones |

```sh
cargo gen-c-headers          # -> target/gen-c-headers/
```

Every value comes from `kernel-shared`, and the headers carry `_Static_assert`s
pinning the `message` layout and the endpoint encode/decode against it — so an
ABI change on the Rust side fails to compile on the C side rather than silently
corrupting IPC. See [Build & CI](../build.md#generated-c-headers).

## MINIX 3 source references

| File | Purpose |
|------|---------|
| `include/minix/callnr.h` | PM and VFS call numbers |
| `include/minix/com.h` | Kernel-call numbers and IPC constants |
| `lib/libc/sys/syscall.c` | User-space `_syscall()` |
| `lib/libsys/kernel_call.c` | Server-side `_kernel_call()` |

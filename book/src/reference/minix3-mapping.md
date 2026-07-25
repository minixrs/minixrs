# MINIX 3 Source Mapping

This chapter maps MINIX 3 source files and concepts to their minix.rs
equivalents — a navigation aid if you are coming from the MINIX 3 codebase or
Tanenbaum's *Operating Systems: Design and Implementation*. Only paths that exist
in the tree today are listed as real; everything else is grouped under
[Planned](#planned) so the table never points at a file that isn't there.

> Some rows cite 64-bit paths in the MINIX 3 tree (e.g.
> `lib/libc/arch/x86_64/`). MINIX 3 shipped only as a 32-bit OS; a working 64-bit
> MINIX 3 was a personal prototype by this project's author, not an upstream
> release. These are cited as an ABI/source reference only — see the note in
> [IPC](../ipc/overview.md#message-structure).

## Kernel

| MINIX 3 | minix.rs |
|---------|----------|
| `kernel/proc.c` — `mini_send` | `kernel/src/ipc/send.rs` |
| `kernel/proc.c` — `mini_receive` | `kernel/src/ipc/receive.rs` |
| `kernel/proc.c` — `mini_notify` | `kernel/src/ipc/notify.rs` |
| `kernel/proc.c` — `mini_senda` | `kernel/src/ipc/senda.rs` (stub) |
| `kernel/proc.c` — `deadlock` | `kernel/src/ipc/deadlock.rs` |
| `kernel/proc.c` — `do_ipc` | `kernel/src/ipc/mod.rs` |
| `kernel/proc.h` — `struct proc` | `kernel/src/proc/proc_struct.rs` (static tables in `table.rs`) |
| `kernel/priv.h` — `struct priv` | `kernel/src/proc/priv_struct.rs` |
| `kernel/system.c` | `kernel/src/system/mod.rs` |
| `kernel/system/do_fork.c` | `kernel/src/system/do_fork.rs` |
| `kernel/system/do_exec.c` | `kernel/src/system/do_exec.rs` |
| `kernel/system/do_vmctl.c` | `kernel/src/system/do_vmctl.rs` |
| `kernel/system/do_copy.c`, `do_irqctl.c` | `kernel/src/system/stubs.rs` (stub handlers) |
| `kernel/clock.c` | `kernel/src/clock.rs` |
| `kernel/interrupt.c` | `kernel/src/arch/aarch64/irq.rs`, `gic.rs` |
| `kernel/main.c` — `kmain` | `kernel/src/main.rs` |
| `kernel/table.c` — boot table | `kernel/src/boot_image/mod.rs` |
| `kernel/arch/i386/head.S` | `kernel/src/arch/aarch64/entry.S` |
| `kernel/arch/i386/mpx.S` — context switch | `kernel/src/arch/aarch64/context.rs` |
| `kernel/arch/i386/protect.c` — vectors | `kernel/src/arch/aarch64/exception.rs` |
| `kernel/arch/i386/memory.c` — page tables | `kernel/src/arch/aarch64/mmu.rs`, `addrspace.rs` |

## Shared headers

| MINIX 3 | minix.rs |
|---------|----------|
| `include/minix/ipc.h` | `kernel-shared/src/message.rs` |
| `include/minix/ipcconst.h` | `kernel-shared/src/ipc_const.rs` |
| `include/minix/com.h` | `kernel-shared/src/com.rs` |
| `include/minix/callnr.h` | `kernel-shared/src/callnr.rs` |
| `include/minix/type.h` — `endpoint_t` | `kernel-shared/src/endpoint.rs` |
| error codes | `kernel-shared/src/error.rs` |
| signal numbers | `kernel-shared/src/signal.rs` |

The arrow runs both ways for the first four rows: `tools/gen-c-headers` generates
`minix/{ipc,com,callnr,errno}.h` back out of those Rust modules for the musl fork
to include, so C never gets a hand-maintained copy of the ABI.

Errno numbering is minix.rs's own policy rather than a port of either reference
tree (phase-5 decision D7): the POSIX block `1..=40` uses classic book-era MINIX
values — identical to Linux/musl, which is what lets musl's stock
`bits/errno.h` work unmodified — while the MINIX-specific IPC errnos take modern
MINIX 3's 200-band values, clear of Linux's entire range. Both are stored negated
in Rust. `EBADSRCDST` is the one name modern MINIX lacks; it takes the value of
that tree's `EBADEPT` (216).

## Runtime and libraries

| MINIX 3 | minix.rs |
|---------|----------|
| `lib/libsys/sef.c` | `server-rt/src/sef.rs` |
| `lib/libsys/sef_init.c` | `server-rt/src/init.rs` |
| `lib/libsys/sef_signal.c` | `server-rt/src/signal.rs` |
| SEF message classifier | `server-rt/src/classify.rs` |
| DS publish glue | `server-rt/src/ds.rs` |
| `lib/libsys/kernel_call.c` | `minix-ipc/src/lib.rs` |

## Servers

| MINIX 3 | minix.rs |
|---------|----------|
| `servers/pm/main.c`, `forkexit.c`, `exec.c`, `signal.c` | `servers/pm/src/main.rs` |
| `servers/pm/` — process table | `servers/pm/src/mproc.rs` |
| `servers/vfs/main.c` | `servers/vfs/src/main.rs` (skeletal) |
| `servers/vm/main.c` | `servers/vm/src/main.rs` |
| `servers/vm/region.c` | `servers/vm/src/region.rs` |
| `servers/rs/main.c` | `servers/rs/src/main.rs` |
| `servers/rs/manager.c` — monitoring | `servers/rs/src/monitor.rs` |
| `servers/ds/main.c` | `servers/ds/src/main.rs` |
| `servers/ds/store.c` | `servers/ds/src/registry.rs` |
| `servers/sched/main.c` | `servers/sched/src/main.rs` |
| `servers/sched/schedule.c` — policy | `servers/sched/src/policy.rs` |

## Build system

| MINIX 3 | minix.rs |
|---------|----------|
| `build.sh`, NetBSD Makefiles | root `Cargo.toml` workspace + `.cargo/config.toml` |
| `releasetools/` | `tools/qemu-run.sh`, `tools/check-boot-log.sh` |

## Planned

These MINIX 3 areas have no minix.rs counterpart in the tree yet; the paths below
are the *intended* destinations (see [Roadmap](../roadmap.md)):

| MINIX 3 | minix.rs (planned) |
|---------|--------------------|
| `include/minix/safecopies.h` — grants | grant table + `SYS_SAFECOPY` (Phase 5) |
| `lib/libc/sys/syscall.c`, `_ipc.S`, `open.c`, … | the musl fork (`musl/src/minix/*`) |
| `lib/libblockdriver/`, `lib/libchardriver/` | `drivers/driver-rt` traits |
| `servers/vfs/{open,read,path,mount}.c` | VFS I/O (VFS is one `main.rs` today) |
| `servers/mfs/`, `servers/pfs/` | `fs/mfs`, `fs/pfs` (stubs) |
| `drivers/storage/*`, `drivers/net/*`, `drivers/tty/` | `drivers/virtio-*`, `drivers/memory` (stubs) |
| disk-image tooling | (no root disk yet — see [Boot](../boot/overview.md)) |

## Concepts unchanged

These MINIX 3 concepts carry over directly and are live today:

- Message-passing IPC with fixed-size (104-byte) messages.
- **Five** live IPC primitives — SEND, RECEIVE, SENDREC, NOTIFY, SENDNB (SENDA is
  stubbed).
- Endpoint-based process identification with generation numbers.
- Privilege bitmaps (`trap_mask`, `ipc_to`, `k_call_mask`).
- SEF server lifecycle (init, ping, signal).
- An embedded boot image (MXBI). Startup is *not* hand-ordered, though — servers
  rendezvous at run time through DS.
- RS heartbeat monitoring (restart-on-crash is detect-only so far).
- DS as the name → endpoint registry.

Carried over in design but not yet realized: grant-based safe copy, the BDEV/CDEV
driver protocols, and the `REQ_*` VFS ↔ FS protocol.

## Concepts changed

| MINIX 3 | minix.rs | Why |
|---------|----------|-----|
| raw C pointers in IPC queues | `Option<ProcNr>` indices | memory safety |
| `m1i1` / `m2l1` message fields | named typed structs | readability |
| `EXTERN` macro globals | module-scoped statics | Rust idiom |
| `RTS_SET` / `RTS_UNSET` macros | `rts_set` / `rts_unset` fns | type safety |
| `volatile` flags | `AtomicU32` | Rust atomics |
| integer error returns | `Result<T, E>` | Rust error handling |
| `#ifdef` arch selection | `cfg(target_arch)` | Rust conditional compilation |
| NetBSD libc / userland | musl fork / Rust userland (planned) | BSD license, minimal |

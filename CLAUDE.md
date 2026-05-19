# MINIX 4

MINIX 4 is a learning OS with a Rust microkernel, based on MINIX 3's architecture.

## Project Overview

- **Kernel:** Rust (no_std, no_main), greenfield microkernel
- **Architecture:** aarch64 primary (Apple Silicon / QEMU virt), x86_64 secondary
- **Servers:** PM, VFS, VM, RS, DS, SCHED as user-space Rust processes
- **Drivers:** VirtIO (MMIO for aarch64, PCI for x86_64) as user-space processes
- **C library:** musl-libc fork with MINIX IPC syscall wrappers
- **License:** BSD-2-Clause only (no GPL code)
- **Platform:** QEMU as primary target

## Reference Codebase

MINIX 4's architecture is based on MINIX 3. When the docs reference "MINIX 3 source",
they mean paths within the MINIX 3 source tree (e.g., `kernel/proc.c` means the
`kernel/proc.c` file in a MINIX 3 checkout). Key reference files:

- `kernel/proc.c` -- IPC implementation (mini_send, mini_receive, deadlock detection)
- `kernel/proc.h`, `priv.h` -- Process and privilege structures
- `kernel/system.c` -- Kernel call dispatch
- `include/minix/ipc.h` -- Message structure definitions
- `include/minix/com.h`, `callnr.h` -- Server endpoints, call numbers
- `lib/libc/sys/*.c` -- POSIX syscall wrappers (template for musl adaptation)
- `lib/libsys/sef.c` -- SEF framework (template for server-rt)

The MINIX 3 source is available at https://github.com/Stichting-MINIX-Research-Foundation/minix

## Build

```sh
# Build kernel for aarch64 (primary target)
cargo kernel-aarch64

# Run in QEMU
tools/qemu-run.sh

# Build kernel for x86_64
cargo kernel-x86_64
```

## Architecture

See `docs/architecture.md` for the full system design. Key concepts:

- **Microkernel:** Only IPC, scheduling, interrupt dispatch, and memory protection in kernel
- **Message passing:** 6 IPC primitives (SEND, RECEIVE, SENDREC, NOTIFY, SENDNB, SENDA)
- **User-space servers:** All OS services (file system, process management, memory management) run as separate processes communicating via IPC
- **Privilege model:** Fine-grained bitmaps control which processes can communicate and what kernel calls they can make

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `kernel` | Microkernel (no_std) |
| `kernel-shared` | Message types, endpoints, call numbers shared between kernel and userspace |
| `minix-ipc` | User-space IPC library (SVC/SYSCALL asm stubs) |
| `server-rt` | Server runtime / SEF framework |
| `servers/*` | System servers (PM, VFS, VM, RS, DS, SCHED) |
| `drivers/*` | Device drivers (VirtIO block/net/console, memory) |
| `fs/*` | File system servers (MFS, PFS) |
| `userland/*` | User programs (init, sh, coreutils) |

## Code Conventions

- Kernel `unsafe` blocks require `// SAFETY:` comments documenting the invariant
- IPC linked lists use `Option<ProcNr>` indices into static arrays, not raw pointers
- Message types are defined in `kernel-shared` and shared across all crates
- Assembly is confined to `.S` files (assembled via `cc` crate in `build.rs`); use `core::arch::asm!` only for single-instruction operations

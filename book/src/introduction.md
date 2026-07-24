# minix.rs

> **MINIX 3, in Rust, for the 64-bit era**

A 64-bit-only reimplementation of MINIX 3 in Rust, preserving the original ABI.

minix.rs is a learning operating system built around a greenfield Rust
microkernel. It keeps MINIX 3's architecture — message-passing IPC, user-space
servers, user-space drivers, and a fine-grained privilege model — while dropping
32-bit legacy and targeting modern 64-bit platforms under QEMU.

## Highlights

- **Microkernel in Rust** (`no_std`, `no_main`) — only IPC, scheduling,
  interrupt dispatch, and memory protection live in the kernel.
- **Message passing** — MINIX 3's six IPC primitives; five are live (SEND,
  RECEIVE, SENDREC, NOTIFY, SENDNB), SENDA is still a stub.
- **User-space servers** — PM, VFS, VM, RS, DS, SCHED run as separate processes.
- **User-space drivers** — VirtIO drivers as separate processes (planned; see the
  [Roadmap](roadmap.md)).
- **aarch64 first** (Apple Silicon / QEMU virt); x86_64 planned.
- **ABI-preserving** — message layout, endpoints, and call numbers track MINIX 3.

## Status

Phases 0–4 are complete (as of 2026-07-18): the aarch64 kernel boots through
UEFI/Limine, the system servers (PM, VFS, VM, RS, DS, SCHED) start from a
multi-module boot image and discover each other via DS, and init (PID 1)
drives a fork/exec/wait loop over a boot-embedded worker binary — all verified
in QEMU. Phase 5 — a musl libc fork and the first file systems, ending in a C
"Hello World" — is next; the working plan lives in the repository's
`docs/plan.md`.

## About this book

This book is the canonical, source-derived documentation for minix.rs, written
from the actual kernel and server code. Chapters describe the system in the
present tense only for what boots today; forward-looking design is collected in
the [Roadmap](roadmap.md).

> **Note:** The repository's `docs/` directory once held hand-written bootstrap
> notes used to plan the project; those have been ported into this book and
> retired. Only the planning tree remains there — `docs/plan.md` (the live phase
> tracker) and `docs/plans/` (per-phase slice histories).

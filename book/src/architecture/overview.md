# Architecture

> _This page is a stub. The architecture documentation will be written from the
> kernel source (`kernel/`, `kernel-shared/`) as the corresponding code
> stabilizes._

minix.rs is a microkernel: only IPC, scheduling, interrupt dispatch, and memory
protection live in the kernel. Every other OS service — process management, the
file system, memory management — runs as a separate user-space server that
communicates over message-passing IPC.

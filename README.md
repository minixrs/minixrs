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
- **Message passing** — six IPC primitives (SEND, RECEIVE, SENDREC, NOTIFY,
  SENDNB, SENDA).
- **User-space servers** — PM, VFS, VM, RS, DS, SCHED run as separate processes.
- **User-space drivers** — VirtIO (MMIO on aarch64, PCI on x86_64).
- **aarch64 first** (Apple Silicon / QEMU virt), x86_64 secondary.
- **ABI-preserving** — message layout, endpoints, and call numbers track MINIX 3.

## Build

```sh
# Build the kernel for aarch64 (primary target)
cargo kernel-aarch64

# Boot in QEMU (the kernel runs indefinitely once EL0 starts, so timeout is required)
timeout 8 cargo run -p minixrs-kernel --target aarch64-unknown-none --release

# Build the kernel for x86_64
cargo kernel-x86_64

# Run host-side unit tests
cargo test -p minixrs-kernel-shared
```

See [`docs/`](docs/) for the full architecture, IPC model, syscall catalog, boot
flow, and implementation plan.

## License

BSD-3-Clause. See [LICENSE](LICENSE).

## Disclaimer

minix.rs is an independent project. It is **not** affiliated with, endorsed by,
or backed by the Vrije Universiteit Amsterdam or Andrew S. Tanenbaum. "MINIX" is
used here only to describe the architectural lineage this project reimplements;
the original MINIX 3 source serves as an architectural reference only.

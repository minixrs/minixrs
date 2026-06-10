# minixrs

[![crates.io](https://img.shields.io/crates/v/minixrs.svg)](https://crates.io/crates/minixrs)
[![docs.rs](https://docs.rs/minixrs/badge.svg)](https://docs.rs/minixrs)

> **MINIX 3, in Rust, for the 64-bit era**

Umbrella crate for [minix.rs](https://github.com/minixrs/minixrs), a 64-bit-only
reimplementation of MINIX 3 in Rust. It re-exports the project's reusable,
host-buildable libraries under one name:

| Re-export | Crate | Purpose |
|-----------|-------|---------|
| `minixrs::kernel_shared` | `minixrs-kernel-shared` | MINIX ABI: message types, endpoints, call numbers |
| `minixrs::ipc` | `minixrs-ipc` | User-space IPC wrappers over the SVC trap (aarch64) |
| `minixrs::server_rt` | `minixrs-server-rt` | Server runtime / SEF framework |
| `minixrs::driver_rt` | `minixrs-driver-rt` | Driver runtime |

The kernel, system servers, drivers, and userland programs are freestanding
binaries built for `aarch64-unknown-none` and are **not** published to crates.io.
To build and run the OS, clone the repository and see its
[README](https://github.com/minixrs/minixrs#build).

## License

BSD-3-Clause. See [LICENSE](https://github.com/minixrs/minixrs/blob/main/LICENSE).

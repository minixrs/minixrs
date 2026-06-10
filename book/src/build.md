# Build & Toolchain

> _This page is a stub. The build documentation will be written from the actual
> build system (`Cargo.toml` aliases, `kernel/build.rs`, `rust-toolchain.toml`,
> `tools/`) as it stabilizes._

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

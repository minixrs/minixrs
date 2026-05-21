# Build System

## Overview

MINIX 4 uses a hybrid build system:
- **Cargo workspace** for all Rust crates (kernel, servers, drivers, userland)
- **Make** for the musl-libc fork (C cross-compilation)
- **Shell scripts** in `tools/` for disk image creation and QEMU launch

## Prerequisites

- Rust nightly (pinned via `rust-toolchain.toml`)
- QEMU (`qemu-system-aarch64`, `qemu-system-x86_64`)
- OVMF UEFI firmware for aarch64 (`OVMF_AARCH64.fd` or `QEMU_EFI.fd`)
- Clang/LLVM (for musl cross-compilation and assembly)
- `mtools` or similar for FAT32 image creation

## Quick Start

```sh
# One-time: download the pinned Limine binary release into external/limine/dist/
make -C external/limine

# Build + launch the kernel under QEMU. The cargo runner (tools/qemu-run.sh)
# stages an ESP directory at target/esp/, drops Limine + the kernel ELF in,
# and boots qemu-system-aarch64 with QEMU's directory-as-FAT helper -- no
# disk-image scripting needed for Phase 1.
cargo run -p minix4-kernel --target aarch64-unknown-none --release
```

Expected serial output:

```
MINIX 4 booting on aarch64
HHDM offset: 0xffff000000000000
```

The kernel then halts in a `wfe` loop. Exit QEMU with `Ctrl-A x`.

### UEFI firmware

`tools/qemu-run.sh` auto-detects the aarch64 UEFI firmware in a few common
locations (homebrew QEMU, `/usr/share/edk2-aarch64`, AAVMF). Override with:

```sh
QEMU_EFI_AARCH64=/path/to/QEMU_EFI.fd cargo run -p minix4-kernel ...
```

## Cargo Workspace

The root `Cargo.toml` declares all crates as workspace members:

```toml
[workspace]
members = [
    "kernel",           # Microkernel (no_std, no_main)
    "kernel-shared",    # Shared types (no_std)
    "minix-ipc",        # User-space IPC stubs
    "server-rt",        # Server runtime (SEF)
    "servers/pm", "servers/vfs", "servers/vm",
    "servers/rs", "servers/ds", "servers/sched",
    "drivers/driver-rt", "drivers/virtio-blk",
    "drivers/virtio-net", "drivers/virtio-console",
    "drivers/memory",
    "fs/mfs", "fs/pfs",
    "userland/init", "userland/sh", "userland/coreutils",
]
```

### Custom Targets

The kernel requires a custom Rust target (`no_std`, `no_main`, kernel code model).
Target specs live in `tools/targets/`:

- `aarch64-minix-kernel.json` -- Kernel target (aarch64)
- `aarch64-minix-user.json` -- User-space target (aarch64)
- `x86_64-minix-kernel.json` -- Kernel target (x86_64)
- `x86_64-minix-user.json` -- User-space target (x86_64)

Example kernel target spec:

```json
{
    "llvm-target": "aarch64-unknown-none",
    "data-layout": "e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128",
    "arch": "aarch64",
    "target-endian": "little",
    "target-pointer-width": "64",
    "os": "none",
    "executables": true,
    "linker-flavor": "ld.lld",
    "linker": "rust-lld",
    "panic-strategy": "abort",
    "disable-redzone": true,
    "features": "+strict-align"
}
```

### Build Commands

```sh
# Build kernel
cargo build -p minix4-kernel --target aarch64-unknown-none --release

# Build a specific server
cargo build -p minix4-pm --target aarch64-unknown-none --release

# Build all servers
cargo build --workspace --exclude minix4-kernel --target aarch64-unknown-none --release

# Alias (defined in .cargo/config.toml)
cargo kernel-aarch64
```

### Assembly in build.rs

The kernel crate's `build.rs` uses the `cc` crate to assemble `.S` files:

```rust
// kernel/build.rs
fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let asm_files = match arch.as_str() {
        "aarch64" => vec!["src/arch/aarch64/entry.S"],
        "x86_64" => vec![
            "src/arch/x86_64/entry.S",
            "src/arch/x86_64/vectors.S",
        ],
        _ => panic!("Unsupported architecture: {}", arch),
    };

    let mut build = cc::Build::new();
    build.compiler("clang");
    for file in &asm_files {
        build.file(file);
        println!("cargo:rerun-if-changed={}", file);
    }
    build.compile("asm");
}
```

## musl-libc Build

The musl fork is built separately with its own Makefile:

```sh
cd musl/
./configure \
    --prefix=$SYSROOT/usr \
    --target=aarch64-minix \
    CC="clang --target=aarch64-unknown-none" \
    CFLAGS="-nostdinc -I../kernel-shared/include/generated"
make -j$(nproc)
make install
```

Output: `libc.a`, `crt1.o`, `crti.o`, `crtn.o` installed to the sysroot.

## Boot Image Packing

`tools/mkbootimage` packs server/driver ELF binaries into the MXBI archive format:

```sh
# Build all boot modules, then pack
cargo build --workspace --exclude kernel --release
tools/mkbootimage \
    target/aarch64-unknown-none/release/minix4-ds \
    target/aarch64-unknown-none/release/minix4-rs \
    target/aarch64-unknown-none/release/minix4-pm \
    target/aarch64-unknown-none/release/minix4-sched \
    target/aarch64-unknown-none/release/minix4-vfs \
    target/aarch64-unknown-none/release/minix4-memory \
    target/aarch64-unknown-none/release/minix4-virtio-console \
    target/aarch64-unknown-none/release/minix4-vm \
    target/aarch64-unknown-none/release/minix4-pfs \
    target/aarch64-unknown-none/release/minix4-mfs \
    target/aarch64-unknown-none/release/minix4-init \
    -o boot_image.bin
```

The boot image is then linked into the kernel:

```
# In kernel linker script
.boot_image : {
    _boot_image_start = .;
    KEEP(*(.boot_image))
    _boot_image_end = .;
}
```

## Disk Image Creation

`tools/mkimage.sh` creates a bootable disk image:

1. Create a raw disk file (e.g., 256 MB)
2. Partition with GPT: ESP (FAT32, 32 MB) + root (MinixFS, rest)
3. Format ESP as FAT32
4. Copy Limine files (`BOOTAA64.EFI`, `limine.conf`) and kernel to ESP
5. Format root partition as MinixFS
6. Copy userland binaries, /etc, /dev to root
7. For BIOS boot (x86_64 only): run `limine bios-install`

## QEMU Launch Scripts

### tools/qemu-run.sh (aarch64)

```sh
#!/bin/sh
qemu-system-aarch64 \
    -M virt -cpu cortex-a72 -m 256M \
    -bios "${OVMF:?Set OVMF to path to UEFI firmware (e.g. edk2-aarch64-code.fd)}" \
    -drive file=minix4.img,format=raw,if=virtio \
    -device virtio-net-device \
    -serial stdio \
    -no-reboot \
    "$@"
```

### tools/qemu-run-x86_64.sh

```sh
#!/bin/sh
qemu-system-x86_64 \
    -m 256M \
    -drive file=minix4-x86_64.img,format=raw,if=virtio \
    -device virtio-net-pci \
    -serial stdio \
    -no-reboot \
    "$@"
```

### Debug with GDB

```sh
# Terminal 1: QEMU with GDB stub
tools/qemu-run.sh -s -S

# Terminal 2: GDB
rust-gdb target/aarch64-unknown-none/release/kernel \
    -ex "target remote :1234" \
    -ex "break kmain" \
    -ex "continue"
```

## CI

Future CI pipeline (GitHub Actions):
1. Install Rust nightly + targets
2. `cargo build --workspace --release`
3. Build musl cross
4. Pack boot image
5. Create disk image
6. QEMU smoke test (boot, check serial output, shutdown)

## Directory Structure Reference

```
Cargo.toml              # Workspace root
rust-toolchain.toml     # Pinned nightly
.cargo/config.toml      # Target flags, aliases

kernel/
  Cargo.toml
  build.rs              # Assembles .S files
  src/

kernel-shared/
  Cargo.toml
  src/

tools/
  mkimage.sh            # Create disk image
  mkbootimage.rs        # Pack boot archive
  qemu-run.sh           # Launch QEMU (aarch64)
  qemu-run-x86_64.sh
  targets/
    aarch64-minix-*.json
    x86_64-minix-*.json

external/
  limine/
    limine.h            # Vendored header
    Makefile             # Download Limine binaries
```

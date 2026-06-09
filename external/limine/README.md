# Limine (vendored)

minix.rs uses the [Limine](https://github.com/limine-bootloader/limine) bootloader
(BSD Zero Clause License).

Run `make` here to download a pinned upstream commit and extract:

- `dist/BOOTAA64.EFI` -- aarch64 UEFI bootloader, copied onto the EFI System
  Partition at `/EFI/BOOT/BOOTAA64.EFI` by `tools/qemu-run.sh`.
- `dist/limine.h` -- C header documenting the Limine boot protocol. The Rust
  request structs in `kernel/src/arch/aarch64/limine.rs` are derived from this
  header; check here when adding a new request type.

Pinned commit: see `LIMINE_COMMIT` in the Makefile.

The `dist/` directory is gitignored; no Limine artifacts are checked in.

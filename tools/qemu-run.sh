#!/usr/bin/env bash
#
# tools/qemu-run.sh -- cargo runner for the aarch64 kernel target.
#
# Cargo invokes this with the kernel ELF path as $1. We build a tiny ESP
# directory tree on the fly and hand it to QEMU's `-drive file=fat:rw:DIR`
# helper, which exposes the directory as a FAT32 filesystem. That avoids
# needing parted/mtools/hdiutil to land a "MINIX 4 booting" banner.
#
# Override the UEFI firmware location with QEMU_EFI_AARCH64=/path/to/fw.fd.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <kernel-elf> [extra qemu args...]" >&2
    exit 64
fi

KERNEL="$1"
shift

ROOT="$(git rev-parse --show-toplevel)"
ESP="$ROOT/target/esp"
LIMINE_BIN="$ROOT/external/limine/dist/BOOTAA64.EFI"

# Materialize the Limine binary if `make` hasn't been run yet.
if [[ ! -f "$LIMINE_BIN" ]]; then
    echo "==> Fetching Limine binaries (one-time)..." >&2
    make -C "$ROOT/external/limine" >&2
fi

# Locate aarch64 UEFI firmware. Honor explicit override first.
FIRMWARE_CANDIDATES=(
    "${QEMU_EFI_AARCH64:-}"
    "$(brew --prefix qemu 2>/dev/null || true)/share/qemu/edk2-aarch64-code.fd"
    "/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
    "/usr/local/share/qemu/edk2-aarch64-code.fd"
    "/usr/share/edk2-aarch64/QEMU_EFI.fd"
    "/usr/share/AAVMF/AAVMF_CODE.fd"
)
FIRMWARE=""
for cand in "${FIRMWARE_CANDIDATES[@]}"; do
    if [[ -n "$cand" && -f "$cand" ]]; then
        FIRMWARE="$cand"
        break
    fi
done
if [[ -z "$FIRMWARE" ]]; then
    cat >&2 <<'EOF'
error: no aarch64 UEFI firmware found.
       Install QEMU (brew install qemu) or point QEMU_EFI_AARCH64 at an
       OVMF/edk2 firmware blob, e.g.:
           export QEMU_EFI_AARCH64=/path/to/QEMU_EFI.fd
EOF
    exit 69
fi

# Stage the ESP directory tree.
rm -rf "$ESP"
mkdir -p "$ESP/EFI/BOOT" "$ESP/boot"
cp "$LIMINE_BIN" "$ESP/EFI/BOOT/BOOTAA64.EFI"
cp "$ROOT/tools/limine.conf" "$ESP/limine.conf"
cp "$KERNEL" "$ESP/boot/kernel"

exec qemu-system-aarch64 \
    -M virt -cpu cortex-a72 -m 256M \
    -bios "$FIRMWARE" \
    -drive "file=fat:rw:fat-type=32:$ESP,format=raw,if=virtio" \
    -display none \
    -serial stdio \
    -no-reboot \
    "$@"

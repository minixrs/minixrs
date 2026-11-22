#!/usr/bin/env bash
#
# tools/qemu-run.sh -- cargo runner for the aarch64 kernel target.
#
# Cargo invokes this with the kernel ELF path as $1. We build a tiny ESP
# directory tree on the fly and hand it to QEMU's `-drive file=fat:rw:DIR`
# helper, which exposes the directory as a FAT32 filesystem. That avoids
# needing parted/mtools/hdiutil to land a "minix.rs booting" banner.
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

# Bus pinning for the phase: one device per transport page (a page holds 8
# 0x200-byte slots), eight buses apart, so each driver's registers can be mapped
# on their own page. `virtio-mmio-bus.N` is transport slot N (measured for buses
# 0 and 8, slice 6.2 Task 0). blk is bus 0; the later devices are planned for
# buses 8 (console), 16 (input) and 24 (net), inferred from that and not yet
# measured.
BUS_VBLK=0

# The root backend (slice 6.3, phase decision H5: what is attached selects it).
#
#   MINIXRS_ROOT=disk     (default) attach the root image as a virtio-blk disk;
#                         MFS mounts its root through the `vblk` driver.
#   MINIXRS_ROOT=ramdisk  attach no virtio-blk device at all; `vblk` reports
#                         `no device` and MFS mounts the ramdisk.
#
# The image is `rootfs.img` beside the kernel ELF -- kernel/build.rs writes the
# very bytes it packs into the boot archive there, so the two backends serve one
# image. It is COPIED on every run: the boot writes to its root, and a reused
# file would make the second run a different test from the first. Nothing
# persists across boots; that is slice 6.4's proof.
#
# MINIXRS_VBLK_IMAGE=<file> attaches that file as-is instead (no copy), for the
# refusal boots -- a blank disk, or the root image with extra drive options in
# MINIXRS_VBLK_DRIVE_OPTS (e.g. `readonly=on`).
ROOT_BACKEND="${MINIXRS_ROOT:-disk}"
VBLK_ARGS=()
case "$ROOT_BACKEND" in
    disk)
        if [[ -n "${MINIXRS_VBLK_IMAGE:-}" ]]; then
            VBLK_IMAGE="$MINIXRS_VBLK_IMAGE"
            if [[ ! -f "$VBLK_IMAGE" ]]; then
                echo "error: MINIXRS_VBLK_IMAGE not found: $VBLK_IMAGE" >&2
                exit 66
            fi
        else
            ROOTFS_IMAGE="$(dirname "$KERNEL")/rootfs.img"
            if [[ ! -f "$ROOTFS_IMAGE" ]]; then
                echo "error: no root image at $ROOTFS_IMAGE (kernel/build.rs writes it)" >&2
                exit 66
            fi
            VBLK_IMAGE="$ROOT/target/vblk-root.img"
            mkdir -p "$ROOT/target"
            cp "$ROOTFS_IMAGE" "$VBLK_IMAGE"
        fi
        VBLK_ARGS=(
            -drive "file=$VBLK_IMAGE,format=raw,if=none,id=vblk0${MINIXRS_VBLK_DRIVE_OPTS:+,$MINIXRS_VBLK_DRIVE_OPTS}"
            -device "virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.$BUS_VBLK"
        )
        ;;
    ramdisk)
        # The disk knobs mean nothing here, and a refusal boot started with the
        # wrong backend would otherwise run as a plain, passing ramdisk boot.
        if [[ -n "${MINIXRS_VBLK_IMAGE:-}" || -n "${MINIXRS_VBLK_DRIVE_OPTS:-}" ]]; then
            echo "error: MINIXRS_VBLK_IMAGE / MINIXRS_VBLK_DRIVE_OPTS need MINIXRS_ROOT=disk" >&2
            exit 64
        fi
        ;;
    *)
        echo "error: MINIXRS_ROOT must be 'disk' or 'ramdisk', got '$ROOT_BACKEND'" >&2
        exit 64
        ;;
esac

# `force-legacy=false` makes the mmio transports modern (version 2); the
# drivers refuse version 1. "$@" stays last so a later -global can override it
# for the refusal boots.
exec qemu-system-aarch64 \
    -M "virt,gic-version=3" -cpu cortex-a72 -m 256M \
    -bios "$FIRMWARE" \
    -drive "file=fat:rw:fat-type=32:$ESP,format=raw,if=virtio" \
    -global virtio-mmio.force-legacy=false \
    ${VBLK_ARGS[@]+"${VBLK_ARGS[@]}"} \
    -display none \
    -serial stdio \
    -no-reboot \
    "$@"

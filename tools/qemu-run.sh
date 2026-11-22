#!/usr/bin/env bash
#
# tools/qemu-run.sh -- cargo runner for the aarch64 kernel target.
#
# Cargo invokes this with the kernel ELF path as $1. The machine boots from ONE
# disk: a GPT image written by the host tool tools/mkimage, holding a FAT32 ESP
# (Limine, its config, the kernel) and -- on a disk boot -- the MinixFS root
# partition. It is attached as a single modern virtio-blk device on
# virtio-mmio-bus.0; the firmware reads the kernel from it and minix.rs's
# `vblk` driver then brings the same device up. Nothing else is attached, so a
# kernel banner in the log is itself the proof that edk2 and Limine read the
# image (slice 6.4, spec P3/P4).
#
# stdout is the guest's serial log, which callers redirect and grep. Everything
# the runner itself says -- the nested cargo's output, mkimage's summary, and
# one `qemu-run: ...` line holding the exact QEMU command line -- goes to stderr.
#
# Environment (all optional; see the root-backend block below for the last three):
#   QEMU_EFI_AARCH64=<fw.fd>        the UEFI firmware, instead of the search below
#   MINIXRS_ROOT=disk|ramdisk|nodisk
#   MINIXRS_VBLK_IMAGE=<file>
#   MINIXRS_VBLK_DRIVE_OPTS=<opts>

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <kernel-elf> [extra qemu args...]" >&2
    exit 64
fi

KERNEL="$1"
shift

ROOT="$(git rev-parse --show-toplevel)"
LIMINE_BIN="$ROOT/external/limine/dist/BOOTAA64.EFI"

# Cargo hands over the ELF relative to the workspace root when it is run from
# there and absolute when run from a member directory. mkimage is run from
# $ROOT (below), so pin the path down against the directory we were started in
# first.
case "$KERNEL" in
    /*) ;;
    *) KERNEL="$PWD/$KERNEL" ;;
esac

# Bus pinning for the phase: one device per transport page (a page holds 8
# 0x200-byte slots), eight buses apart, so each driver's registers can be mapped
# on their own page. `virtio-mmio-bus.N` is transport slot N (measured for buses
# 0 and 8, slice 6.2 Task 0). blk is bus 0; the later devices are planned for
# buses 8 (console), 16 (input) and 24 (net), inferred from that and not yet
# measured.
BUS_VBLK=0

# The root backend (phase decision H5: what is attached selects it; slice 6.4,
# spec P4: the machine always boots from the disk image, so what differs is
# whether the image carries a root partition and which transport it sits on).
#
#   MINIXRS_ROOT=disk     (default) the image has the ESP and the root partition.
#                         `vblk` finds the root by its GPT type GUID and MFS
#                         mounts through it.
#   MINIXRS_ROOT=ramdisk  the SAME image without the root partition (same size,
#                         the root's range left unused). `vblk` brings the
#                         device up, finds no partition of the root type and
#                         prints `root FAIL no root partition`; MFS prints
#                         `bdev.root vblk rc=-6 fallback=memory` and mounts the
#                         ramdisk. This is the no-root FALLBACK boot.
#   MINIXRS_ROOT=nodisk   the rootless image attached as virtio-blk-PCI instead:
#                         the firmware reads it, minix.rs has no PCI and sees no
#                         virtio-mmio device at all, so `vblk` prints `no
#                         device` and MFS finds no `vblk` in DS. A machine with
#                         no disk has nothing to boot from, so this is the only
#                         way to reach the no-device state. Hand-run only: CI
#                         does not boot it and no marker pair exists for it.
#
# The image is BUILT ON EVERY RUN, from the kernel ELF we were handed, Limine,
# tools/limine.conf and -- for `disk` -- `rootfs.img` beside the kernel ELF
# (kernel/build.rs writes there the very bytes it packs into the boot archive,
# so the disk and the ramdisk serve one filesystem image). Rebuilt rather than
# reused because the boot WRITES to its root, and the driver flushes every
# write: a reused image would make the second run a different test from the
# first. The two flavours go to different files so a ramdisk boot does not
# destroy the image a disk boot left behind.
#
# MINIXRS_VBLK_IMAGE=<file> (disk only) attaches that file AS-IS: no build, no
# copy, and the boot's writes land in it. It is how a refusal boot attaches a
# prepared image, and how a "reboot" attaches the image the previous disk boot
# wrote (MINIXRS_VBLK_IMAGE=target/disk.img). The machine boots entirely from
# that file, so it needs its own ESP with a kernel on it -- the kernel ELF in $1
# is NOT used -- and a file without one (the raw rootfs.img, say) gives no
# kernel at all, only the firmware's boot-failure output.
#
# MINIXRS_VBLK_DRIVE_OPTS=<opts> (disk only) is appended to the -drive argument,
# e.g. `readonly=on`. Without MINIXRS_VBLK_IMAGE the fresh image is still built
# and the options apply to it.
ROOT_BACKEND="${MINIXRS_ROOT:-disk}"
DISK_IMAGE="$ROOT/target/disk.img"
ROOTLESS_IMAGE="$ROOT/target/disk-rootless.img"

BUILD_IMAGE=""  # the file mkimage writes; empty when nothing is built
BUILD_ROOTFS="" # its --root input; empty for a rootless image
case "$ROOT_BACKEND" in
    disk)
        if [[ -n "${MINIXRS_VBLK_IMAGE:-}" ]]; then
            ATTACH_IMAGE="$MINIXRS_VBLK_IMAGE"
            if [[ ! -f "$ATTACH_IMAGE" ]]; then
                echo "error: MINIXRS_VBLK_IMAGE not found: $ATTACH_IMAGE" >&2
                exit 66
            fi
        else
            BUILD_ROOTFS="$(dirname "$KERNEL")/rootfs.img"
            if [[ ! -f "$BUILD_ROOTFS" ]]; then
                echo "error: no root image at $BUILD_ROOTFS (kernel/build.rs writes it)" >&2
                exit 66
            fi
            BUILD_IMAGE="$DISK_IMAGE"
            ATTACH_IMAGE="$DISK_IMAGE"
        fi
        ;;
    ramdisk | nodisk)
        # The disk knobs mean nothing here, and a refusal boot started with the
        # wrong backend would otherwise run as a plain, passing fallback boot.
        if [[ -n "${MINIXRS_VBLK_IMAGE:-}" || -n "${MINIXRS_VBLK_DRIVE_OPTS:-}" ]]; then
            echo "error: MINIXRS_VBLK_IMAGE / MINIXRS_VBLK_DRIVE_OPTS need MINIXRS_ROOT=disk" >&2
            exit 64
        fi
        BUILD_IMAGE="$ROOTLESS_IMAGE"
        ATTACH_IMAGE="$ROOTLESS_IMAGE"
        ;;
    *)
        echo "error: MINIXRS_ROOT must be 'disk', 'ramdisk' or 'nodisk', got '$ROOT_BACKEND'" >&2
        exit 64
        ;;
esac

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

if [[ -n "$BUILD_IMAGE" ]]; then
    # Materialize the Limine binary if `make` hasn't been run yet.
    if [[ ! -f "$LIMINE_BIN" ]]; then
        echo "==> Fetching Limine binaries (one-time)..." >&2
        make -C "$ROOT/external/limine" >&2
    fi

    MKIMAGE_ARGS=("$BUILD_IMAGE")
    if [[ -n "$BUILD_ROOTFS" ]]; then
        MKIMAGE_ARGS+=(--root "$BUILD_ROOTFS")
    fi
    MKIMAGE_ARGS+=(
        "EFI/BOOT/BOOTAA64.EFI=$LIMINE_BIN"
        "limine.conf=$ROOT/tools/limine.conf"
        "boot/kernel=$KERNEL"
    )

    # mkimage is a HOST tool, built by a cargo nested inside the `cargo run`
    # that started us (measured, 6.4 Task 0: it does not wait on the build
    # lock). It runs from $ROOT so cargo finds the workspace whatever directory
    # we were started in, and with all of its output on stderr.
    #
    # The environment cargo gives a runner (measured: CARGO, CARGO_MANIFEST_*,
    # CARGO_PKG_*, the rustup toolchain variables) carries nothing that
    # retargets a build, and the kernel's linker-script rustflags live under
    # `[target.aarch64-unknown-none]` in .cargo/config.toml, which a host build
    # never reads. What WOULD misdirect it is a caller's own environment: flags
    # meant for the kernel, or a default target. Scrub those, the same reflex as
    # kernel/build.rs's nested server builds.
    mkdir -p "$ROOT/target"
    (
        cd "$ROOT"
        env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_BUILD_RUSTFLAGS \
            -u CARGO_BUILD_TARGET \
            cargo run -q --release -p minixrs-mkimage -- "${MKIMAGE_ARGS[@]}"
    ) >&2
else
    echo "qemu-run: attaching $ATTACH_IMAGE as-is (not built, not copied; \$1 is unused)" >&2
fi

case "$ROOT_BACKEND" in
    nodisk)
        # virtio-blk-pci: edk2 has a driver for it, minix.rs has no PCI at all.
        DISK_ARGS=(-drive "file=$ATTACH_IMAGE,format=raw,if=virtio")
        ;;
    *)
        DISK_ARGS=(
            -drive "file=$ATTACH_IMAGE,format=raw,if=none,id=vblk0${MINIXRS_VBLK_DRIVE_OPTS:+,$MINIXRS_VBLK_DRIVE_OPTS}"
            -device "virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.$BUS_VBLK"
        )
        ;;
esac

# `force-legacy=false` makes the mmio transports modern (version 2); the
# drivers refuse version 1, and edk2 boots from a modern one (measured, 6.4
# P11.1). "$@" stays last so a later -global can override it for the refusal
# boots.
QEMU=(
    qemu-system-aarch64
    -M "virt,gic-version=3" -cpu cortex-a72 -m 256M
    -bios "$FIRMWARE"
    -global virtio-mmio.force-legacy=false
    "${DISK_ARGS[@]}"
    -display none
    -serial stdio
    -no-reboot
    "$@"
)

# The command line, on one line, for the log: what was attached is the first
# thing to check when a boot surprises.
echo "qemu-run: ${QEMU[*]}" >&2

exec "${QEMU[@]}"

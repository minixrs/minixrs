#!/usr/bin/env bash
#
# tools/qemu-run.sh -- cargo runner for the aarch64 kernel target.
#
# Cargo invokes this with the kernel ELF path as $1. The machine boots from ONE
# disk: a GPT image written by the host tool tools/mkimage, holding a FAT32 ESP
# (Limine, its config, the kernel) and -- on a disk boot -- the MinixFS root
# partition. It is attached as a single modern virtio-blk device on
# virtio-mmio-bus.0; the firmware reads the kernel from it and minix.rs's
# `vblk` driver then brings the same device up. Nothing else on the machine
# holds a kernel, so a kernel banner in the log is itself the proof that edk2
# and Limine read the image (slice 6.4, spec P3/P4).
#
# The script attaches TWO other devices, on every boot whatever the root
# backend. One is a virtio console (slice 6.6, spec C1/C2) -- a
# `virtio-serial-device` on virtio-mmio-bus.8 with a single `virtconsole` port
# -- which minix.rs's `vcon` driver serves as /dev/hvc0. Its host side is a pair
# of FIFOs, described at the console block below. The other is a display
# (slice 6.7, spec F1): `ramfb`, the framebuffer the firmware sets a mode on,
# Limine reports to the kernel and minix.rs's TTY driver draws the console on.
# It is not on a virtio-mmio bus. Nothing shows it unless MINIXRS_DISPLAY=window
# is set; a QMP socket, also there on every boot, is how a script takes a
# `screendump` of it. Both are described at the display block below. (`-M
# virt`'s own default devices are still there: no -nodefaults.)
#
# stdout is the guest's serial log, which callers redirect and grep. Everything
# the runner itself says -- the nested cargo's output, mkimage's summary, and
# one `qemu-run: ...` line holding the exact QEMU command line -- goes to stderr.
# The second console's bytes are on neither: they go to and come from the FIFOs.
#
# Environment (all optional; see the root-backend block below for the three
# after the firmware, the console block for the fifth, and the display block
# for the last two):
#   QEMU_EFI_AARCH64=<fw.fd>        the UEFI firmware, instead of the search below
#   MINIXRS_ROOT=disk|ramdisk|nodisk
#   MINIXRS_VBLK_IMAGE=<file>
#   MINIXRS_VBLK_DRIVE_OPTS=<opts>
#   MINIXRS_HVC0=<base>             the console's FIFO pair is <base>.in and
#                                   <base>.out, instead of target/hvc0.{in,out}
#   MINIXRS_QMP=<socket>            QEMU's QMP socket, instead of target/qmp.sock;
#                                   its path must be shorter than 104 bytes
#   MINIXRS_DISPLAY=none|window     `none` (the default) shows nothing; `window`
#                                   opens QEMU's own display window, scaled
#                                   to the window's size

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
# 0 and 8, slice 6.2 Task 0). blk is bus 0 and the console is bus 8 -- slot 8,
# the first slot of the second page (slice 6.6); the later devices are planned
# for buses 16 (input) and 24 (net), inferred from that and not yet measured.
BUS_VBLK=0
BUS_VCON=8

# The display (slice 6.7, spec F1): `-device ramfb` on EVERY boot, so that CI
# and a boot somebody watches run one code path -- a TTY with a framebuffer.
# No resolution is asked for anywhere; the kernel takes the mode the firmware
# set (800x600, 32 bpp with Homebrew's edk2 under QEMU 11.0.3: measured, 6.7
# Task 0). Dropping the device is how to boot the no-framebuffer path by hand:
# the kernel prints `[fb] none` and TTY is serial-only.
#
# MINIXRS_DISPLAY says whether anything SHOWS the display:
#
#   none    (default) `-display none`: no window. The framebuffer is still
#           there and still drawn on; QMP's `screendump` reads it.
#   window  QEMU opens a window, and the window SCALES: the guest's screen is
#           the firmware's 800x600, which a high-density display shows very
#           small, so the UI is asked to zoom the picture to whatever size the
#           window is dragged or maximised to (`zoom-to-fit=on`). The guest
#           still draws 800x600 -- nothing it prints and no screendump
#           changes. The UI is the first of cocoa (macOS) and gtk that this
#           QEMU was built with; with neither, `-display none` is simply left
#           off and QEMU picks its default, unscaled. The serial console stays
#           on stdio.
#
# Anything else is refused: a misspelt `window` would otherwise run as a
# perfectly good headless boot and look like a display that does not work.
#
# QMP is on EVERY boot too: a unix socket QEMU listens on (`server=on`) without
# waiting for a client (`wait=off`), so a boot nobody connects to runs exactly
# as before. It is the only way in to a headless display -- `stop`,
# `screendump`, `cont` -- and what the screen check is built on.
#
# MINIXRS_QMP=<socket> moves the socket, so that two boots run at once do not
# share one -- the MINIXRS_HVC0 pattern. A path of 104 bytes or more is refused
# here rather than left to QEMU: a unix socket's address holds 104 bytes on
# macOS including its terminator, QEMU's own message for it does not name the
# variable to change, and the default lives under the checkout, whose depth is
# not this script's to choose (measured, 6.7 Task 0). The limit is applied on
# Linux as well, where the address is 4 bytes longer, so that a path which works
# in CI works on a laptop.
#
# A socket left behind by an earlier boot is removed just before QEMU starts:
# QEMU unlinks it on a clean exit but not when it is killed, which is how every
# `timeout` boot ends, and it will not bind over one. Anything else in the
# socket's place is refused rather than replaced, as for the FIFOs below.
#
# All three checks run here, ahead of the root backend's and before the image
# is built, so a refused boot costs nothing and starts nothing.
DISPLAY_MODE="${MINIXRS_DISPLAY:-none}"
case "$DISPLAY_MODE" in
    none) DISPLAY_ARGS=(-display none) ;;
    window)
        DISPLAY_ARGS=()
        backends="$(qemu-system-aarch64 -display help 2>/dev/null || true)"
        for ui in cocoa gtk; do
            if grep -qx "$ui" <<<"$backends"; then
                DISPLAY_ARGS=(-display "$ui,zoom-to-fit=on")
                break
            fi
        done
        ;;
    *)
        echo "error: MINIXRS_DISPLAY must be 'none' or 'window', got '$DISPLAY_MODE'" >&2
        exit 64
        ;;
esac

QMP_SOCK="${MINIXRS_QMP:-$ROOT/target/qmp.sock}"
# Bytes, not characters: the limit is on the encoded path.
QMP_SOCK_BYTES=$(($(printf '%s' "$QMP_SOCK" | wc -c)))
if ((QMP_SOCK_BYTES >= 104)); then
    echo "error: the QMP socket path is $QMP_SOCK_BYTES bytes; a unix socket path must be shorter than 104: $QMP_SOCK" >&2
    echo "       set MINIXRS_QMP to a shorter path" >&2
    exit 64
fi
if [[ ! -S "$QMP_SOCK" && (-e "$QMP_SOCK" || -L "$QMP_SOCK") ]]; then
    echo "error: $QMP_SOCK exists and is not a socket; remove it, or set MINIXRS_QMP" >&2
    exit 73
fi

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

# The second console (slice 6.6, spec C1/C2): one `virtconsole` port -- QEMU's
# port 0, the only port a driver without the multiport feature can see -- on a
# `pipe` chardev. QEMU appends `.in` and `.out` to the chardev's path itself:
# the host writes guest input to <base>.in and reads guest output from
# <base>.out. Both are FIFOs, made here when missing.
#
# Nobody has to be on either end. QEMU opens both read-write, so it starts with
# no peer (measured, 6.6 Task 0) and a guest write with no reader completes
# (measured, 6.6 Task 4): the bytes sit in the pipe. They are NOT reliably
# emptied when QEMU exits -- on macOS about 800 bytes of an unread boot were
# still in <base>.out with no process holding it (measured, 6.6 Task 5) -- so
# tools/feed-console.sh discards what it finds there before it starts looking,
# and unread boots with no fed boot between them accumulate. The firmware uses
# the port too -- edk2 and Limine put about 800 bytes of boot text on it before
# the kernel starts -- so whatever reads <base>.out must look for its line
# among other output, never at "the first bytes".
#
# MINIXRS_HVC0=<base> moves the pair, so that two boots run at once do not
# share one. A regular file in a FIFO's place is refused rather than replaced:
# QEMU would open it, the guest's output would land in it, and a feeder
# blocked on the FIFO it expected would wait for good.
HVC0_BASE="${MINIXRS_HVC0:-$ROOT/target/hvc0}"
for fifo in "$HVC0_BASE.in" "$HVC0_BASE.out"; do
    if [[ -p "$fifo" ]]; then
        continue
    fi
    if [[ -e "$fifo" || -L "$fifo" ]]; then
        echo "error: $fifo exists and is not a FIFO; remove it, or set MINIXRS_HVC0" >&2
        exit 73
    fi
    mkdir -p "$(dirname "$fifo")"
    mkfifo "$fifo"
done
# A comma in a QEMU option value is written doubled.
VCON_ARGS=(
    -chardev "pipe,id=hvc0,path=${HVC0_BASE//,/,,}"
    -device "virtio-serial-device,bus=virtio-mmio-bus.$BUS_VCON"
    -device "virtconsole,chardev=hvc0"
)

# The display and the QMP socket (the display block above has the why). The
# stale socket goes now, as late as possible, so that a run refused further up
# leaves an earlier boot's socket alone.
if [[ -S "$QMP_SOCK" ]]; then
    rm -f "$QMP_SOCK"
fi
mkdir -p "$(dirname "$QMP_SOCK")"
FB_ARGS=(
    -device ramfb
    -qmp "unix:${QMP_SOCK//,/,,},server=on,wait=off"
)

# `force-legacy=false` makes the mmio transports modern (version 2); the
# drivers refuse version 1, and edk2 boots from a modern one (measured, 6.4
# P11.1). "$@" stays last so a later -global can override it for the refusal
# boots. `DISPLAY_ARGS` can be empty for a window boot, hence the `+` expansion:
# bash before 4.4 treats an empty array as unset under `set -u`.
QEMU=(
    qemu-system-aarch64
    -M "virt,gic-version=3" -cpu cortex-a72 -m 256M
    -bios "$FIRMWARE"
    -global virtio-mmio.force-legacy=false
    "${DISK_ARGS[@]}"
    "${VCON_ARGS[@]}"
    "${FB_ARGS[@]}"
    ${DISPLAY_ARGS[@]+"${DISPLAY_ARGS[@]}"}
    -serial stdio
    -no-reboot
    "$@"
)

# The command line, on one line, for the log: what was attached is the first
# thing to check when a boot surprises.
echo "qemu-run: ${QEMU[*]}" >&2

exec "${QEMU[@]}"

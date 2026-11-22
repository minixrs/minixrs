#!/usr/bin/env bash
#
# tools/check-boot-log.sh -- verify a QEMU serial boot log against the
# checked-in marker lists.
#
#   usage: tools/check-boot-log.sh <disk|ramdisk|reboot> <qemu-log>
#
# The first argument names the boot the log came from, and so which marker
# files it is checked against. It is REQUIRED.
#
# `disk` and `ramdisk` are the root backend a FULL boot ran with (MINIXRS_ROOT
# in tools/qemu-run.sh; `disk` is that script's default). Both boots attach the
# boot disk; `disk` means it carries the root partition and MFS mounted through
# it, `ramdisk` means it carries none and MFS fell back to the ramdisk. Every
# substring in tests/qemu-boot.expected AND tests/qemu-boot.<backend>.expected
# must appear in the log; no substring in tests/qemu-boot.forbidden or
# tests/qemu-boot.<backend>.forbidden may appear. The common lists alone cannot
# tell a disk boot that silently fell back to the ramdisk from a healthy one, so
# there is no way to check them alone. (MINIXRS_ROOT=nodisk, the hand-run
# no-device boot, has no pair here.)
#
# `reboot` is the second boot of the disk image a disk boot left behind
# (MINIXRS_VBLK_IMAGE=target/disk.img, attached as-is). It is checked against
# tests/qemu-boot.reboot.expected and tests/qemu-boot.reboot.forbidden ONLY --
# the common lists are NOT applied, because the write battery was designed
# against a fresh image and what it does on its own leftovers is not defined
# (tests/qemu-boot.reboot.expected says more). The pair requires what a fresh
# boot cannot print and forbids what a fresh boot must, so a `disk` log fails
# `reboot` and a reboot's log fails `disk`.
#
# Blank lines and '#' comment lines are ignored. Every violation is reported
# (not just the first) before the non-zero exit, so one run shows the whole
# story.
#
# A boot kind whose expected files hold NO marker at all fails (exit 1), before
# the log is looked at: with nothing required, every log would pass. The
# `reboot` kind is why -- it leans on one three-line file and nothing else, so
# that file emptied, or reduced to comments, would otherwise read as PASS.
#
# grep needs -a (the log interleaves raw single-char tick bytes, so it reads
# as binary) and -F (the marker files hold literal substrings, not regexes).

set -euo pipefail

USAGE="usage: $0 <disk|ramdisk|reboot> <qemu-log>"

if [[ $# -ne 2 ]]; then
    echo "$USAGE" >&2
    exit 64
fi

BACKEND="$1"
LOG="$2"
ROOT="$(git rev-parse --show-toplevel)"

# Which lists apply. A full boot reads the common pair and its backend's; the
# reboot reads its own pair and nothing else.
case "$BACKEND" in
    disk | ramdisk)
        EXPECTED=("$ROOT/tests/qemu-boot.expected" "$ROOT/tests/qemu-boot.$BACKEND.expected")
        FORBIDDEN=("$ROOT/tests/qemu-boot.forbidden" "$ROOT/tests/qemu-boot.$BACKEND.forbidden")
        ;;
    reboot)
        EXPECTED=("$ROOT/tests/qemu-boot.reboot.expected")
        FORBIDDEN=("$ROOT/tests/qemu-boot.reboot.forbidden")
        ;;
    *)
        echo "error: the first argument must be 'disk', 'ramdisk' or 'reboot', got '$BACKEND'" >&2
        echo "$USAGE" >&2
        exit 64
        ;;
esac

if [[ ! -f "$LOG" ]]; then
    echo "error: log file not found: $LOG" >&2
    exit 66
fi
for f in "${EXPECTED[@]}" "${FORBIDDEN[@]}"; do
    if [[ ! -f "$f" ]]; then
        echo "error: marker file not found: $f" >&2
        exit 66
    fi
done

fail=0
checked=0
required=0

# How many markers this boot kind requires. Zero is a broken marker file, not a
# boot that passed: refuse it here rather than report `PASS` on nothing.
while IFS= read -r line; do
    if [[ -z "$line" || "$line" == \#* ]]; then continue; fi
    required=$((required + 1))
done < <(cat "${EXPECTED[@]}")
if [[ "$required" -eq 0 ]]; then
    echo "FAIL: no required marker for a $BACKEND boot -- ${EXPECTED[*]} hold(s) none, so any log would pass" >&2
    exit 1
fi

# Required markers: report every one that is missing.
while IFS= read -r line; do
    if [[ -z "$line" || "$line" == \#* ]]; then continue; fi
    checked=$((checked + 1))
    if ! grep -aqF -- "$line" "$LOG"; then
        echo "MISSING   $line"
        fail=1
    fi
done < <(cat "${EXPECTED[@]}")

# Forbidden markers: report every one that is present, with its first hit.
while IFS= read -r line; do
    if [[ -z "$line" || "$line" == \#* ]]; then continue; fi
    checked=$((checked + 1))
    if hit="$(grep -am1 -F -- "$line" "$LOG")"; then
        echo "FORBIDDEN $line"
        echo "    first hit: $hit"
        fail=1
    fi
done < <(cat "${FORBIDDEN[@]}")

if [[ "$fail" -ne 0 ]]; then
    echo "FAIL: $LOG failed the $BACKEND checks above ($checked markers checked)"
    exit 1
fi
echo "PASS: $LOG passed all $checked marker checks ($BACKEND)"

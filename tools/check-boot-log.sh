#!/usr/bin/env bash
#
# tools/check-boot-log.sh -- verify a QEMU serial boot log against the
# checked-in marker lists.
#
#   usage: tools/check-boot-log.sh <disk|ramdisk> <qemu-log>
#
# The first argument is the root backend the boot ran with (MINIXRS_ROOT in
# tools/qemu-run.sh; `disk` is that script's default). Both boots attach the
# boot disk; `disk` means it carries the root partition and MFS mounted through
# it, `ramdisk` means it carries none and MFS fell back to the ramdisk. It is
# REQUIRED: the common lists alone cannot tell a disk boot that silently fell
# back to the ramdisk from a healthy one, so there is no way to check them
# alone. (MINIXRS_ROOT=nodisk, the hand-run no-device boot, has no pair here.)
#
# Every substring in tests/qemu-boot.expected AND tests/qemu-boot.<backend>.expected
# must appear in the log; no substring in tests/qemu-boot.forbidden or
# tests/qemu-boot.<backend>.forbidden may appear. Blank lines and '#' comment
# lines are ignored. Every violation is reported (not just the first) before
# the non-zero exit, so one run shows the whole story.
#
# grep needs -a (the log interleaves raw single-char tick bytes, so it reads
# as binary) and -F (the marker files hold literal substrings, not regexes).

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <disk|ramdisk> <qemu-log>" >&2
    exit 64
fi

BACKEND="$1"
LOG="$2"
case "$BACKEND" in
    disk | ramdisk) ;;
    *)
        echo "error: backend must be 'disk' or 'ramdisk', got '$BACKEND'" >&2
        echo "usage: $0 <disk|ramdisk> <qemu-log>" >&2
        exit 64
        ;;
esac

ROOT="$(git rev-parse --show-toplevel)"
EXPECTED=("$ROOT/tests/qemu-boot.expected" "$ROOT/tests/qemu-boot.$BACKEND.expected")
FORBIDDEN=("$ROOT/tests/qemu-boot.forbidden" "$ROOT/tests/qemu-boot.$BACKEND.forbidden")

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

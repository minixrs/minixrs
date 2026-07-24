#!/usr/bin/env bash
#
# tools/check-boot-log.sh -- verify a QEMU serial boot log against the
# checked-in marker lists.
#
#   usage: tools/check-boot-log.sh <qemu-log>
#
# Every substring in tests/qemu-boot.expected must appear in the log; no
# substring in tests/qemu-boot.forbidden may appear. Blank lines and '#'
# comment lines in either file are ignored. Every violation is reported (not
# just the first) before the non-zero exit, so one run shows the whole story.
#
# grep needs -a (the log interleaves raw single-char tick bytes, so it reads
# as binary) and -F (the marker files hold literal substrings, not regexes).

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <qemu-log>" >&2
    exit 64
fi

LOG="$1"
ROOT="$(git rev-parse --show-toplevel)"
EXPECTED="$ROOT/tests/qemu-boot.expected"
FORBIDDEN="$ROOT/tests/qemu-boot.forbidden"

if [[ ! -f "$LOG" ]]; then
    echo "error: log file not found: $LOG" >&2
    exit 66
fi

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
done < "$EXPECTED"

# Forbidden markers: report every one that is present, with its first hit.
while IFS= read -r line; do
    if [[ -z "$line" || "$line" == \#* ]]; then continue; fi
    checked=$((checked + 1))
    if hit="$(grep -am1 -F -- "$line" "$LOG")"; then
        echo "FORBIDDEN $line"
        echo "    first hit: $hit"
        fail=1
    fi
done < "$FORBIDDEN"

if [[ "$fail" -ne 0 ]]; then
    echo "FAIL: $LOG failed the checks above ($checked markers checked)"
    exit 1
fi
echo "PASS: $LOG passed all $checked marker checks"

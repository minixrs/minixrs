#!/usr/bin/env bash
#
# tools/diff-backends.sh -- prove the filesystem behaves identically on the two
# root backends (slice 6.3, spec B10; slice 6.4, spec P4).
#
#   usage: tools/diff-backends.sh <disk-log> <ramdisk-log>
#          tools/diff-backends.sh --self-test
#
# D3's promise is virtio-blk under an UNCHANGED MFS, so everything MFS and VFS
# report about the filesystem must be the same whichever driver is underneath.
# The marker files require each line; this checks that nothing ELSE differs.
#
# It takes the `[diag mfs]` lines and the `[diag vfs]` lines of each log as two
# separate streams and keeps the first occurrence of each line, in order. Then:
#
#   * the two VFS streams must be identical, and
#   * the two MFS streams must differ in exactly one line -- `bdev.ds ok name=`,
#     the line that names the backend -- except that
#   * the RAMDISK side may carry one more line, exactly
#     `[diag mfs] bdev.root vblk rc=-6 fallback=memory`.
#
# That last line is how the ramdisk boot reaches its root since slice 6.4: the
# boot disk is attached but carries no root partition, so MFS finds `vblk`,
# is told ENXIO (-6), and falls back. It is allowed on the ramdisk side only (on
# the disk side it is the silent fallback), as that exact line only (another rc
# is another failure and still fails here), and at most once -- counted in the
# raw log, before repeats are dropped, because a second one would mean MFS
# probed twice. It is allowed, not required: requiring it is
# tests/qemu-boot.ramdisk.expected's job.
#
# Why this shape, all three measured on two boots of the SAME backend (the
# control, 6.3 Task 0): the servers interleave differently from boot to boot,
# so the streams are compared separately; VFS prints one `pm-exit reset` line
# per exit for as long as the boot runs, so repeats are dropped; and a tick
# character can precede a line, so a line is matched wherever it starts.
#
# What it cannot see: a backend-dependent change in how often a line repeats,
# or in the order of lines between the two servers.

set -euo pipefail

NAME_LINE='[diag mfs] bdev.ds ok name='
FALLBACK_LINE='[diag mfs] bdev.root vblk rc=-6 fallback=memory'

# Print one server's first-occurrence stream from a log.
stream() { # <server> <log>
    grep -ao "\[diag $1\] .*" "$2" | tr -d '\r' | awk '!seen[$0]++' || true
}

compare() { # <disk-log> <ramdisk-log>; prints findings, returns 0 if clean
    local disk="$1" ram="$2" bad=0 out
    local d_vfs r_vfs d_mfs r_mfs
    d_vfs="$(stream vfs "$disk")"
    r_vfs="$(stream vfs "$ram")"
    d_mfs="$(stream mfs "$disk")"
    r_mfs="$(stream mfs "$ram")"

    if [[ -z "$d_mfs" || -z "$r_mfs" || -z "$d_vfs" || -z "$r_vfs" ]]; then
        echo "DIFF      a log has no [diag mfs] or no [diag vfs] lines at all"
        return 1
    fi
    if ! out="$(diff <(printf '%s\n' "$d_vfs") <(printf '%s\n' "$r_vfs"))"; then
        echo "DIFF      [diag vfs] differs between the backends:"
        printf '%s\n' "$out" | sed 's/^/    /'
        bad=1
    fi
    # The MFS streams with the naming line removed -- and, on the RAMDISK side
    # only, the fallback line, matched as a whole line (-x) so that neither a
    # different rc nor a longer line is excused -- must be identical...
    if ! out="$(diff <(printf '%s\n' "$d_mfs" | grep -vF -- "$NAME_LINE") \
        <(printf '%s\n' "$r_mfs" | grep -vF -- "$NAME_LINE" | grep -vxF -- "$FALLBACK_LINE"))"; then
        echo "DIFF      [diag mfs] differs beyond the line naming the backend and the ramdisk's one fallback line:"
        printf '%s\n' "$out" | sed 's/^/    /'
        bad=1
    fi
    # ...the fallback line may be there at most once. The stream cannot show a
    # repeat (it keeps first occurrences), so this counts the raw log.
    local r_fallbacks
    r_fallbacks="$(grep -ac -F -- "$FALLBACK_LINE" "$ram" || true)"
    if [[ "$r_fallbacks" -gt 1 ]]; then
        echo "DIFF      ramdisk log carries the fallback line $r_fallbacks times, not at most once"
        bad=1
    fi
    # ...and each must carry exactly one naming line, naming its own backend.
    local d_name r_name
    d_name="$(printf '%s\n' "$d_mfs" | grep -F -- "$NAME_LINE" || true)"
    r_name="$(printf '%s\n' "$r_mfs" | grep -F -- "$NAME_LINE" || true)"
    if [[ "$d_name" != "${NAME_LINE}vblk ep="* || "$d_name" == *$'\n'* ]]; then
        echo "DIFF      disk log does not name vblk exactly once: '${d_name}'"
        bad=1
    fi
    if [[ "$r_name" != "${NAME_LINE}memory ep="* || "$r_name" == *$'\n'* ]]; then
        echo "DIFF      ramdisk log does not name memory exactly once: '${r_name}'"
        bad=1
    fi
    return "$bad"
}

self_test() {
    local dir disk ram
    dir="$(mktemp -d)"
    trap 'rm -rf "$dir"' RETURN
    disk="$dir/disk.log"
    ram="$dir/ram.log"
    # A tick before a line, CRLF endings, interleaving and repeats that differ.
    printf '[diag mfs] sef ready\r\nC[diag vfs] sef ready\r\n[diag mfs] bdev.ds ok name=vblk ep=11\r\n[diag mfs] mount ok\r\n[diag vfs] fs.mount ok\r\n[diag vfs] pm-exit reset closed=1\r\n' >"$disk"
    printf '[diag vfs] sef ready\r\n[diag mfs] sef ready\r\n[diag mfs] bdev.ds ok name=memory ep=3\r\n[diag vfs] fs.mount ok\r\nC[diag mfs] mount ok\r\n[diag vfs] pm-exit reset closed=1\r\n[diag vfs] pm-exit reset closed=1\r\n' >"$ram"
    local fails=0
    expect() { # <want: pass|fail> <description>
        local got=pass
        compare "$disk" "$ram" >/dev/null || got=fail
        if [[ "$got" != "$1" ]]; then
            echo "self-test FAILED: $2 (wanted $1, got $got)"
            fails=1
        fi
    }
    expect pass "a clean pair passes"
    cp "$ram" "$dir/ram.keep"
    cp "$disk" "$dir/disk.keep"
    # The one allowance (slice 6.4): the fallback line, on the ramdisk side.
    printf '[diag mfs] bdev.root vblk rc=-6 fallback=memory\r\n' >>"$ram"
    expect pass "the fallback line once on the ramdisk side"
    # A literal repeat is invisible to the first-occurrence stream, so this case
    # passes or fails on the raw-log count alone.
    printf 'C[diag mfs] bdev.root vblk rc=-6 fallback=memory\r\n' >>"$ram"
    expect fail "the fallback line twice on the ramdisk side"
    cp "$dir/ram.keep" "$ram"
    printf '[diag mfs] bdev.root vblk rc=-5 fallback=memory\r\n' >>"$ram"
    expect fail "a fallback line with a different rc"
    cp "$dir/ram.keep" "$ram"
    printf '[diag mfs] bdev.root vblk rc=-6 fallback=memory again\r\n' >>"$ram"
    expect fail "a longer line that merely starts with the fallback line"
    cp "$dir/ram.keep" "$ram"
    printf '[diag mfs] bdev.root vblk rc=-6 fallback=memory\r\n' >>"$disk"
    expect fail "the fallback line on the disk side"
    printf '[diag mfs] bdev.root vblk rc=-6 fallback=memory\r\n' >>"$ram"
    expect fail "the fallback line on both sides"
    cp "$dir/disk.keep" "$disk"
    cp "$dir/ram.keep" "$ram"
    printf '[diag mfs] some other line\r\n' >>"$ram"
    expect fail "any other extra [diag mfs] line on the ramdisk side"
    cp "$dir/ram.keep" "$ram"
    sed -i.bak 's/fs.mount ok/fs.mount FAIL/' "$ram"
    expect fail "a [diag vfs] line that differs"
    cp "$dir/ram.keep" "$ram"
    sed -i.bak 's/name=memory/name=vblk/' "$ram"
    expect fail "the ramdisk log naming vblk"
    cp "$dir/ram.keep" "$ram"
    sed -i.bak '/bdev.ds ok/d' "$ram"
    expect fail "a missing naming line"
    cp "$dir/ram.keep" "$ram"
    : >"$disk"
    expect fail "an empty log"
    if [[ "$fails" -ne 0 ]]; then
        return 1
    fi
    echo "diff-backends: self-test passed (the check fails on each breakage it exists to catch)"
}

if [[ $# -eq 1 && "$1" == "--self-test" ]]; then
    self_test
    exit $?
fi
if [[ $# -ne 2 ]]; then
    echo "usage: $0 <disk-log> <ramdisk-log> | --self-test" >&2
    exit 64
fi
for f in "$1" "$2"; do
    if [[ ! -f "$f" ]]; then
        echo "error: log file not found: $f" >&2
        exit 66
    fi
done
if compare "$1" "$2"; then
    echo "PASS: [diag mfs] and [diag vfs] are identical on both backends apart from the line naming the backend and the ramdisk's one allowed fallback line"
else
    echo "FAIL: the two backends' filesystem output differs (see above)"
    exit 1
fi

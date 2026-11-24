#!/usr/bin/env bash
#
# tools/feed-console.sh -- type one fixed line at a booting minix.rs, once the
# boot log says somebody is waiting for it (slice 6.5, spec R7/R8).
#
#   usage: tools/feed-console.sh <qemu-log>
#
# QEMU's serial is `-serial stdio` (tools/qemu-run.sh), so the boot's stdin IS
# the PL011's receive side. A fed boot runs this script as its stdin, through a
# process substitution, and points it at the log the same boot is writing:
#
#     timeout 1200 cargo run -p minixrs-kernel --target aarch64-unknown-none \
#         --release < <(tools/feed-console.sh qemu.log) > qemu.log 2>&1
#
# The shell does not wait on a process substitution, so the boot's exit status
# is still timeout's.
#
# What it does: polls <qemu-log> (which need not exist yet) for the line
#
#     [diag vfs] cons.beside ok
#
# and, once it is there, writes the probe -- `minix.rs stdin` and a newline, 15
# bytes -- to stdout EXACTLY ONCE, and exits. init's proof child is blocked in
# `read(0)` for those bytes and prints `minix.rs init: stdin ok n=15` when they
# arrive (userland/init/src/main.rs, CONSOLE_PROBE; a host test there reads the
# PROBE line below and fails if the two differ).
#
# Why it waits for that line, and not for a delay:
#
#   * It is what makes the proof causal. VFS prints `cons.beside ok` the first
#     time it completes a file read while a console read is parked, so the
#     child's read cannot return before VFS has been seen serving somebody
#     else beside it.
#   * Bytes that are on stdin before the kernel starts never arrive: the
#     firmware or Limine consumes them (measured, slice 6.5 Task 0). A feeder
#     that wrote at time zero would feed the boot loader.
#
# The line must be seen on TWO consecutive polls. The log path is usually one
# an earlier boot wrote, and in the command above the shell starts this script
# an instant before `> qemu.log` truncates it; one look could find the OLD
# boot's line and write into the firmware. A stale line is gone by the next
# poll, a real one is not.
#
# Exiting after the write closes QEMU's stdin. That is harmless: with stdin at
# EOF QEMU carries on and the boot is unchanged (measured, same task). So there
# is nothing to hold open, and this script never outlives its one job.
#
# It also gives up: if the line has not appeared after FEED_CONSOLE_CEILING
# seconds it exits 1 having written NOTHING, so a boot that never parks a read
# (or never starts) cannot leave a poller behind for good. If stdout is closed
# when the write is made -- QEMU already gone -- the script dies of SIGPIPE or
# exits 1 on the failed write; either way it stops.
#
# stdout carries the probe and nothing else. Every diagnostic goes to stderr.
#
# Environment (optional):
#   FEED_CONSOLE_CEILING=<seconds>  give up after this long (default 1800: a
#                                   1200 s boot plus the build in front of it)
#
# Exit status: 0 the probe was written; 1 the ceiling passed, or the write
# failed; 64 usage.

set -euo pipefail

# What is written, as a printf %b string: `\n` is the only escape in it.
# userland/init's host test parses THIS LINE -- keep it one line, single-quoted.
PROBE='minix.rs stdin\n'

# What is waited for. VFS's own text (servers/vfs/src/main.rs); if it moves and
# this does not, the feeder never writes and `stdin ok` goes missing -- loudly.
MARKER='[diag vfs] cons.beside ok'

# Seconds between looks at the log. Short enough to be invisible in a boot's
# budget, long enough that a stale log is truncated between two of them.
POLL=0.2

# Where the probe is cut, and how long the feeder waits between the halves.
# The cut is inside the text, well clear of the `\n` escape at the end.
SPLIT=9
GAP=3

if [[ $# -ne 1 || -z "$1" || "$1" == -* ]]; then
    echo "usage: $0 <qemu-log>" >&2
    exit 64
fi
LOG="$1"

CEILING="${FEED_CONSOLE_CEILING:-1800}"
if [[ ! "$CEILING" =~ ^[0-9]+$ ]]; then
    echo "feed-console: FEED_CONSOLE_CEILING must be a whole number of seconds, got '$CEILING'" >&2
    exit 64
fi
# Base 10 whatever the digits: `08` passes the test above and is invalid octal
# in an arithmetic context.
CEILING=$((10#$CEILING))

# grep needs -a (the log interleaves raw tick bytes and reads as binary) and -F
# (the marker is a literal, brackets and all). A log that does not exist yet is
# simply a log without the line.
marker_present() {
    grep -aqF -- "$MARKER" "$LOG" 2>/dev/null
}

seen=0
SECONDS=0
while ((SECONDS < CEILING)); do
    if marker_present; then
        seen=$((seen + 1))
    else
        seen=0
    fi
    if ((seen >= 2)); then
        # The probe, once, in TWO chunks a few seconds apart, so the second
        # chunk needs a second interrupt: it arrives only if TTY re-armed its
        # line after draining the first. (Written in one piece the whole probe
        # is one interrupt, and a dropped IRQ_ENABLE passes -- measured, slice
        # 6.5 mutation 5.) A failure here (stdout closed) must end the script,
        # not send it round the loop again.
        if ! { printf '%b' "${PROBE:0:SPLIT}" && sleep "$GAP" && printf '%b' "${PROBE:SPLIT}"; }; then
            echo "feed-console: the write failed (stdout closed?); nothing more to do" >&2
            exit 1
        fi
        echo "feed-console: fed the probe after ${SECONDS}s ($LOG shows '$MARKER')" >&2
        exit 0
    fi
    sleep "$POLL"
done

echo "feed-console: gave up after ${CEILING}s: '$MARKER' never appeared in $LOG; nothing was written" >&2
exit 1

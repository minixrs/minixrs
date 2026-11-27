#!/usr/bin/env bash
#
# tools/screendump.sh -- take pictures of a booting minix.rs's screen until
# one shows a console that has SCROLLED, and prove that picture is the boot's
# own console (slice 6.7, spec F10/F11).
#
#   usage: tools/screendump.sh <qemu-log> <out.ppm>
#
# TTY writes every console byte to the UART and, since slice 6.7, draws it on
# the framebuffer Limine handed the kernel. The serial markers prove the first
# half. This script proves the second, from the host: it asks the running QEMU
# for a dump of the screen and hands it to tools/fbcheck, which decodes every
# character cell with the renderer that painted it and then places the whole
# screen in the serial log -- which the renderer never wrote.
#
# "Places" is not grep. The serial line has three writers and the console is
# one of them (measured, slice 6.7: 54 MB of log for some 60 KB of console):
#
#   * the kernel writes whole lines -- `[ipc 1414500] caller=17 ...`,
#     `[ksys ...`, `[diag vfs] ...`, `[pf ...` -- which can begin in the
#     MIDDLE of a console line, the console line carrying on after them;
#   * the kernel also writes one byte per 10 ms of guest time, the first
#     letter of the running process's name (kernel/src/clock.rs), singly or in
#     runs, anywhere: the log held `minix.rs hello: iov CCCCCCCCCC......` for
#     the screen row `minix.rs hello: iov ......`.
#
# So the checker cuts the log at TTY's own `[diag tty] fb ok` line, takes
# every kernel line out of the rest -- from a `[`, a lower-case name and a
# space or `]`, through the line end -- and requires the screen, read top to
# bottom, to be ONE UNBROKEN STRETCH of what is left: each row where the row
# above leaves off, a line ending where a row stops short of the last column,
# a line wrapping where it does not. Between the bytes of a row only tick
# bytes are forgiven -- ASCII letters or `?`, at most 120 per row. A row cut
# short, shifted, repeated, out of order or left over from before a scroll
# breaks the stretch. The verdict's `stray=<n>` counts the rows that needed
# tick bytes allowed and `worst=<n>` is the most one row needed -- watch it
# against the 120. The rule in full, and what it still does not see (letters
# lost from a row, above all), is in tools/fbcheck/src/check.rs.
#
# WHY SCROLLED. Until the screen has scrolled, nothing has exercised the
# scroll path: the grid shifting up and the screen being repainted from it.
# In the default boot the line this script waits for (stage 2) appears with
# 22 of the screen's 37 rows written, and a check that passed there would
# pass a console whose scroll paints nothing (observed: slice 6.7, mutation
# 7). So only a scrolled screen counts as a pass. The checker tells the two
# apart from the log and the dump alone: a screen that has not scrolled
# begins with the first thing the console wrote; one that has begins later
# and is written down to its last rows. A screen that is right but has not
# scrolled yet is reported as `early`, and the script looks again. A scrolled
# screen is not long in coming: in the default disk boot the 38th console row
# is written 5% of the way through the log, and the boot's last required
# marker at 65%.
#
# It runs BESIDE a boot, as a background job pointed at the log the same boot
# is writing, and is waited for after the boot's timeout:
#
#     tools/screendump.sh qemu.log screen.ppm &
#     dumper=$!
#     timeout 1200 cargo run -p minixrs-kernel --target aarch64-unknown-none \
#         --release < <(tools/feed-console.sh qemu.log) > qemu.log 2>&1 || true
#     wait "$dumper"
#
# What it does, in three stages, in this order:
#
#   1. BUILDS the checker, once: `cargo build --release -p minixrs-fbcheck`,
#      and from then on runs target/release/minixrs-fbcheck directly. Never
#      `cargo run` in the loop below -- that would put a cargo invocation,
#      with its lock and its freshness walk, between a guest that is
#      scrolling and each look at it. MINIXRS_FBCHECK=<binary> skips the
#      build and uses that binary.
#
#      This cargo and the boot's own `cargo run` start at the same moment and
#      share target/, so one of them waits on the other's build-directory
#      lock. That is accepted, and bounded: the boot's cargo lets the lock go
#      before it starts its runner (measured, slice 6.4 Task 0, for the
#      runner's nested mkimage build -- tools/qemu-run.sh), so the wait is one
#      host-tool build at the longest, minutes before stage 2's line. When it
#      is the BOOT that waits, those seconds come out of its timeout: build
#      the checker beforehand where that matters (CI already does so for
#      mkimage), and this build is then only a freshness check.
#   2. POLLS <qemu-log> (which need not exist yet) for
#
#          minix.rs init: stdin ok
#
#      init prints it when the serial probe tools/feed-console.sh writes has
#      arrived -- the last event the feeder drives. The feeder gates on the
#      same family of lines in the same log (`[diag vfs] cons.beside ok`,
#      `minix.rs init: hvc0 ok`); this one comes after all of them, so the
#      dumper's pauses of the guest (stage 3) cannot disturb the feeder's
#      timed exchanges. An UNFED boot never prints it: this script is for fed
#      boots, and against any other it waits out its ceiling and fails.
#
#      Like the feeder's lines, it must be seen on TWO consecutive polls. The
#      log path is usually one an earlier boot wrote, and this script starts
#      an instant before the boot's `> qemu.log` truncates it; one look could
#      find the OLD boot's line. A stale line is gone by the next poll.
#   3. LOOKS, every SCREENDUMP_GAP seconds:
#
#          minixrs-fbcheck dump-and-check --scrolled <qmp-sock> <out.ppm> <qemu-log>
#
#      which over QMP stops the guest, has QEMU write the screen to <out.ppm>
#      as a PPM, lets the guest run again -- always, whatever the dump did --
#      and then checks the dump against the log. Every look therefore PAUSES
#      THE GUEST for as long as QEMU takes to write the dump (milliseconds
#      for 800x600; the checker gives each QMP step 10 s before giving up).
#      Three things can come back:
#
#        * `fbcheck: ok ... scrolled=yes` (exit 0): a scrolled screen that is
#          the boot's. THE FIRST ONE WINS: it is printed and the script exits
#          0.
#        * `fbcheck: early ... (not scrolled yet)` (exit 2): right so far.
#          Not a pass and not a failure, and it does NOT use up a try: the
#          script looks again. One line is printed per change in the number
#          of rows on the screen, not per look.
#        * `fbcheck: FAIL ...` (exit 1; any other status counts the same): a
#          dump that is not the boot's console, or could not be taken. It
#          uses up one of SCREENDUMP_TRIES, and each says why on stderr.
#          When they are used up the script exits 1.
#
#      The tries are not politeness. The screen is never quiet, so a dump can
#      land with a glyph half-painted or a scroll half-repainted (the guest
#      is stopped between two pixel stores); the check refuses such a dump
#      and the next one is of a different instant. A screen that is WRONG
#      fails every try.
#
# The QMP socket is ${MINIXRS_QMP:-<repo root>/target/qmp.sock} -- the default
# tools/qemu-run.sh uses, computed the same way. Set MINIXRS_QMP for the boot
# and this script sees it too if it inherits the boot's environment.
#
# <out.ppm> is overwritten by every look, so when the script ends it holds the
# LAST dump taken -- the one the last message is about. It is never deleted:
# on failure it is the evidence (CI uploads it). If no dump was ever taken
# (QEMU gone, no socket) the file is absent, or is whatever was there before.
#
# It also gives up: SCREENDUMP_CEILING seconds after it started, whichever
# stage it is in, it exits 1 -- in stage 2 because the line never came, in
# stage 3 because the screen never scrolled (or never stopped failing) -- so a
# boot that never gets that far, or never starts, cannot leave a poller
# behind for good. The ceiling is looked at between looks, so the script can
# outlast it by one look and one gap.
#
# stdout carries nothing. Every line -- the build's, each look's, the verdict
# -- goes to stderr.
#
# Environment (optional):
#   SCREENDUMP_TRIES=<n>          FAILed looks allowed in stage 3 (default 10)
#   SCREENDUMP_GAP=<seconds>      between looks (default 10; may be
#                                 fractional)
#   SCREENDUMP_CEILING=<seconds>  give up this long after starting (default
#                                 1300: a 1200 s boot plus the build in front
#                                 of it)
#   MINIXRS_QMP=<socket>          the QMP socket, as for tools/qemu-run.sh
#   MINIXRS_FBCHECK=<binary>      a built minixrs-fbcheck; skips stage 1
#
# Exit status: 0 a dump of a scrolled screen passed; 1 the tries were used
# up, the ceiling passed, or the checker could not be built or run; 64 usage.

set -euo pipefail

# The line stage 2 waits for, without its count: init prints
# `minix.rs init: stdin ok n=15` (userland/init/src/main.rs). If it moves and
# this does not, the script waits out its ceiling -- loudly.
GATE='minix.rs init: stdin ok'

# Seconds between looks at the log. As tools/feed-console.sh: short enough to
# be invisible in a boot's budget, long enough that a stale log is truncated
# between two of them.
POLL=0.2

# The checker's exit status for a screen that is right but has not scrolled.
EARLY=2

if [[ $# -ne 2 || -z "$1" || "$1" == -* || -z "$2" || "$2" == -* ]]; then
    echo "usage: $0 <qemu-log> <out.ppm>" >&2
    exit 64
fi
LOG="$1"
OUT="$2"

# A whole number of <what> from the environment, or exit 64. Base 10 whatever
# the digits: `08` is invalid octal in an arithmetic context.
whole() {
    if [[ ! "$2" =~ ^[0-9]+$ ]]; then
        echo "screendump: $1 must be a whole number, got '$2'" >&2
        exit 64
    fi
    echo $((10#$2))
}

TRIES="$(whole SCREENDUMP_TRIES "${SCREENDUMP_TRIES:-10}")"
CEILING="$(whole SCREENDUMP_CEILING "${SCREENDUMP_CEILING:-1300}")"
if ((TRIES < 1)); then
    echo "screendump: SCREENDUMP_TRIES must be at least 1" >&2
    exit 64
fi
GAP="${SCREENDUMP_GAP:-10}"
if [[ ! "$GAP" =~ ^[0-9]+(\.[0-9]+)?$ ]]; then
    echo "screendump: SCREENDUMP_GAP must be a number of seconds, got '$GAP'" >&2
    exit 64
fi

# The ceiling counts from here, the build included.
SECONDS=0

# The checkout this script is in, wherever it was started from: the runner's
# `git rev-parse --show-toplevel`, asked of the script's own directory.
ROOT="$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
SOCK="${MINIXRS_QMP:-$ROOT/target/qmp.sock}"

# ---- Stage 1: the checker, built once. --------------------------------------

if [[ -n "${MINIXRS_FBCHECK:-}" ]]; then
    FBCHECK="$MINIXRS_FBCHECK"
else
    # A HOST tool, built from $ROOT so cargo finds the workspace whatever
    # directory we were started in, with all of its output on stderr. The
    # caller's environment may hold flags meant for the kernel, or a default
    # target; scrub those -- the same reflex, and the same list, as
    # tools/qemu-run.sh's nested mkimage build.
    if ! (
        cd "$ROOT"
        env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_BUILD_RUSTFLAGS \
            -u CARGO_BUILD_TARGET \
            cargo build -q --release -p minixrs-fbcheck
    ) >&2; then
        echo "screendump: building minixrs-fbcheck failed; no dump was taken" >&2
        exit 1
    fi
    FBCHECK="$ROOT/target/release/minixrs-fbcheck"
fi
if [[ ! -x "$FBCHECK" ]]; then
    echo "screendump: $FBCHECK is not an executable; no dump was taken" >&2
    exit 1
fi

# ---- Stage 2: the gate line in the serial log, on two consecutive polls. ----

# grep needs -a (the log reads as binary: raw tick bytes) and -F (the pattern
# is a literal). A file that does not exist yet is a file without the line.
has_line() {
    grep -aqF -- "$1" "$2" 2>/dev/null
}

seen=0
while ((seen < 2)); do
    if has_line "$GATE" "$LOG"; then
        seen=$((seen + 1))
        if ((seen >= 2)); then
            break
        fi
    else
        seen=0
    fi
    if ((SECONDS >= CEILING)); then
        echo "screendump: gave up after ${CEILING}s: $LOG never showed '$GATE' (an unfed boot, or none?); no dump was taken" >&2
        exit 1
    fi
    sleep "$POLL"
done
echo "screendump: saw '$GATE' after ${SECONDS}s; dumping through $SOCK" >&2

# ---- Stage 3: dump and check, until a scrolled screen passes. ---------------

# Say where the last dump is, if there is one.
kept() {
    if [[ -f "$OUT" ]]; then
        echo "$OUT is as the last look left it"
    else
        echo "no dump was ever written to $OUT"
    fi
}

failed=0     # FAILed looks so far, of TRIES
looks=0      # looks so far, of any outcome
last_rows="" # the `rows=<n>` of the last early line printed
last_early="(none)"
while :; do
    looks=$((looks + 1))
    # One line either way: `ok` and `early` on stdout, `FAIL` on stderr.
    status=0
    verdict="$("$FBCHECK" dump-and-check --scrolled "$SOCK" "$OUT" "$LOG" 2>&1)" || status=$?
    if ((status == 0)); then
        echo "screendump: look $looks passed after ${SECONDS}s: $verdict ($OUT)" >&2
        exit 0
    elif ((status == EARLY)); then
        last_early="$verdict"
        rows=""
        if [[ "$verdict" =~ rows=([0-9]+) ]]; then
            rows="${BASH_REMATCH[1]}"
        fi
        # One line per change, not per look: a boot can sit unscrolled for
        # minutes.
        if [[ "$rows" != "$last_rows" ]]; then
            echo "screendump: look $looks after ${SECONDS}s: $verdict" >&2
            last_rows="$rows"
        fi
    else
        failed=$((failed + 1))
        echo "screendump: look $looks after ${SECONDS}s, failure $failed/$TRIES: $verdict" >&2
        if ((failed >= TRIES)); then
            echo "screendump: FAIL $TRIES looks failed; $(kept)" >&2
            exit 1
        fi
    fi
    if ((SECONDS >= CEILING)); then
        echo "screendump: FAIL gave up after ${CEILING}s and $looks looks ($failed failed) without a scrolled screen that passed; last early result: $last_early; $(kept)" >&2
        exit 1
    fi
    sleep "$GAP"
done

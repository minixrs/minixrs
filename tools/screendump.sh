#!/usr/bin/env bash
#
# tools/screendump.sh -- take a picture of a booting minix.rs's screen once
# the boot has something on it, and prove the picture is the boot's own
# console (slice 6.7, spec F10/F11).
#
#   usage: tools/screendump.sh <qemu-log> <out.ppm>
#
# TTY writes every console byte to the UART and, since slice 6.7, draws it on
# the framebuffer Limine handed the kernel. The serial markers prove the first
# half. This script proves the second, from the host: it asks the running QEMU
# for a dump of the screen and hands it to tools/fbcheck, which decodes every
# character cell with the renderer that painted it and looks each row of text
# up in the serial log -- which the renderer never wrote.
#
# "Looks up" allows for the serial line's second writer. The kernel puts one
# identifying byte on the UART per 10 ms timer tick (kernel/src/clock.rs) and
# it can land INSIDE a console line: a boot's log held `minix.rs initt:
# dev.null ok n=35` for the screen row `minix.rs init: dev.null ok n=35`
# (measured, slice 6.7). So a row is found if the log holds its bytes in order
# with at most 2 extra bytes among them, each alone (never two together) and
# neither a line end; a missing or a different byte is never forgiven. The
# verdict's `stray=<n>` counts the rows that needed that. The rule in full,
# and everything else about what passes, is in tools/fbcheck/src/check.rs.
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
#      `cargo run` in the retry loop below -- that would put a cargo
#      invocation, with its lock and its freshness walk, between a guest that
#      is scrolling and each look at it. MINIXRS_FBCHECK=<binary> skips the
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
#      `minix.rs init: hvc0 ok`); this one comes after all of them, so by now
#      every proof child has printed and init's rotation is what keeps the
#      screen moving. An UNFED boot never prints it: this script is for fed
#      boots, and against any other it waits out its ceiling and fails.
#
#      Like the feeder's lines, it must be seen on TWO consecutive polls. The
#      log path is usually one an earlier boot wrote, and this script starts
#      an instant before the boot's `> qemu.log` truncates it; one look could
#      find the OLD boot's line. A stale line is gone by the next poll.
#   3. TRIES, up to SCREENDUMP_TRIES times, SCREENDUMP_GAP seconds apart:
#
#          minixrs-fbcheck dump-and-check <qmp-sock> <out.ppm> <qemu-log>
#
#      which over QMP stops the guest, has QEMU write the screen to <out.ppm>
#      as a PPM, lets the guest run again -- always, whatever the dump did --
#      and then checks the dump against the log. THE FIRST TRY THAT PASSES
#      WINS: its verdict is printed and the script exits 0.
#
#      The retries are not politeness. The screen is never quiet, so a dump
#      can land with a glyph half-painted (the guest is stopped between two
#      pixel stores) or before QEMU's log write has caught up; the check
#      refuses such a dump and the next one is of a different instant. A
#      screen that is WRONG fails every try the same way, and each try says
#      how on stderr.
#
# The QMP socket is ${MINIXRS_QMP:-<repo root>/target/qmp.sock} -- the default
# tools/qemu-run.sh uses, computed the same way. Set MINIXRS_QMP for the boot
# and this script sees it too if it inherits the boot's environment.
#
# <out.ppm> is overwritten by every try, so after a failure it holds the LAST
# dump taken -- the one the last message is about. It is never deleted: on
# failure it is the evidence (CI uploads it). If no dump was ever taken (QEMU
# gone, no socket) the file is absent, or is whatever was there before.
#
# It also gives up: if the line of stage 2 has not been seen
# SCREENDUMP_CEILING seconds after the script started, it exits 1 saying so,
# so a boot that never gets that far (or never starts) cannot leave a poller
# behind for good. Stage 3 needs no ceiling of its own: it is a fixed number
# of tries, and the checker bounds every step of a QMP exchange (10 s), so a
# dead socket costs a try, not a hang.
#
# stdout carries nothing. Every line -- the build's, each try's, the verdict
# -- goes to stderr.
#
# Environment (optional):
#   SCREENDUMP_TRIES=<n>          tries in stage 3 (default 10)
#   SCREENDUMP_GAP=<seconds>      between tries (default 3; may be fractional)
#   SCREENDUMP_CEILING=<seconds>  give up waiting for stage 2's line after
#                                 this long (default 1300: a 1200 s boot plus
#                                 the build in front of it)
#   MINIXRS_QMP=<socket>          the QMP socket, as for tools/qemu-run.sh
#   MINIXRS_FBCHECK=<binary>      a built minixrs-fbcheck; skips stage 1
#
# Exit status: 0 a dump passed; 1 no try passed, the ceiling passed, or the
# checker could not be built or run; 64 usage.

set -euo pipefail

# The line stage 2 waits for, without its count: init prints
# `minix.rs init: stdin ok n=15` (userland/init/src/main.rs). If it moves and
# this does not, the script waits out its ceiling -- loudly.
GATE='minix.rs init: stdin ok'

# Seconds between looks at the log. As tools/feed-console.sh: short enough to
# be invisible in a boot's budget, long enough that a stale log is truncated
# between two of them.
POLL=0.2

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
GAP="${SCREENDUMP_GAP:-3}"
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

# ---- Stage 3: dump and check, until one passes. ------------------------------

for ((try = 1; try <= TRIES; try++)); do
    # The checker prints one line either way: `fbcheck: ok ...` on stdout,
    # `fbcheck: FAIL ...` on stderr.
    if verdict="$("$FBCHECK" dump-and-check "$SOCK" "$OUT" "$LOG" 2>&1)"; then
        echo "screendump: try $try/$TRIES passed after ${SECONDS}s: $verdict ($OUT)" >&2
        exit 0
    fi
    echo "screendump: try $try/$TRIES: $verdict" >&2
    if ((try < TRIES)); then
        sleep "$GAP"
    fi
done

if [[ -f "$OUT" ]]; then
    echo "screendump: FAIL no try of $TRIES passed; $OUT is as the last try left it" >&2
else
    echo "screendump: FAIL no try of $TRIES passed, and no dump was ever written to $OUT" >&2
fi
exit 1

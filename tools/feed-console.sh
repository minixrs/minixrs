#!/usr/bin/env bash
#
# tools/feed-console.sh -- give a booting minix.rs its input on BOTH consoles,
# each once the boot itself says somebody is waiting for it (slice 6.5, spec
# R7/R8; slice 6.6, spec C12).
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
# The second console, /dev/hvc0, is a virtio console on a QEMU `pipe` chardev:
# two FIFOs, <base>.out (what the guest writes) and <base>.in (what it reads),
# where <base> is ${MINIXRS_HVC0:-<repo root>/target/hvc0} -- the default
# tools/qemu-run.sh uses, computed the same way. Set MINIXRS_HVC0 for the boot
# and this script sees it too: it inherits the boot's environment.
#
# What it does, in four stages, in this order:
#
#   1. READS <base>.out until the guest's own line
#
#          minix.rs hvc0 out
#
#      has come out of it. init's second proof child writes that line through
#      /dev/hvc0 (userland/init/src/main.rs, HVC0_OUT), so seeing it here is
#      the guest-to-host direction, observed on the host. The firmware and
#      Limine put about 800 bytes of escape-laden boot text on the same port
#      first, so the line is looked for as a substring of everything read so
#      far, never as "the first bytes".
#   2. WAITS SETTLE seconds, then WRITES `minix.rs hvc0 in` and a newline --
#      17 bytes -- to <base>.in, one time, in TWO CHUNKS (the first SPLIT
#      bytes, GAP seconds, the rest). The child has a read parked on the node;
#      it must come back with exactly these bytes (HVC0_IN). Two chunks so
#      that the second needs a second interrupt AND a re-posted receive
#      buffer: it arrives only if the driver gave its one buffer back to the
#      device and re-armed after the first.
#
#      The wait is for the child's read to BE parked. The host sees the
#      guest's line the moment the driver hands it to the device, which is
#      before the child's write has even returned, and the read is several
#      IPC hops after that. Written at once, the first chunk beat the read on
#      a boot with the stubs on (measured, slice 6.6 Task 5): the read found
#      bytes already held and was refused directly, never having parked. The
#      boot passes either way, but only a parked read is retried by VFS, and
#      that retry -- a raw request with an unmapped buffer, re-granted and
#      refused -- is what the child's first read is there to exercise.
#   3. POLLS <qemu-log> (which need not exist yet) for BOTH of
#
#          [diag vfs] cons.beside ok
#          minix.rs init: hvc0 ok
#
#      The first is slice 6.5's gate, unchanged, and why it is one is below.
#      The second is the child's verdict on stage 2, and waiting for it is what
#      keeps the read(0) reader parked for the whole of the hvc0 exchange: VFS
#      has to pass over it to answer the hvc0 reader (`[diag vfs] cons.other
#      ok`), which it could not be seen doing if the serial console were fed
#      at the same time.
#   4. WRITES the serial probe -- `minix.rs stdin` and a newline, 15 bytes --
#      to stdout one time, in TWO CHUNKS like stage 2's, and exits. Two chunks
#      so that the second needs a second interrupt: it arrives only if TTY
#      re-armed its line after draining the first (written in one piece, a
#      dropped IRQ_ENABLE passed the boot -- slice 6.5, mutation 5). init's
#      first proof child is blocked in `read(0)` for those bytes and prints
#      `minix.rs init: stdin ok n=15` when they arrive (CONSOLE_PROBE).
#
# GAP and SETTLE are guesses, not measurements of anything the boot reports.
# If both chunks of either probe land in one interrupt the boot still passes
# and that re-arm (or re-post) goes unproved; if the child's read is not
# parked SETTLE seconds after its line, the boot still passes and the retry of
# a parked read into an unmapped buffer goes unexercised. Nothing in the log
# says so in either case.
#
# A host test in userland/init reads the PROBE, HVC0_OUT, HVC0_IN and HVC0_OK
# lines below and fails if any differs from what init holds.
#
# THE COUPLING, stated rather than hidden: stage 4 comes after stage 3, so a
# boot whose virtio console is broken never gets its serial probe either. It
# loses `stdin ok` as well as `hvc0 ok`. Read the first missing line: `hvc0
# ok` is listed first, and it names the cause.
#
# Why the serial probe waits for `cons.beside ok`, and not for a delay:
#
#   * It is what makes the proof causal. VFS prints `cons.beside ok` the first
#     time it completes a file read while a console read is parked, so the
#     child's read cannot return before VFS has been seen serving somebody
#     else beside it.
#   * Bytes that are on stdin before the kernel starts never arrive: the
#     firmware or Limine consumes them (measured, slice 6.5 Task 0). A feeder
#     that wrote at time zero would feed the boot loader.
#
# Stage 3's lines must be seen, together, on TWO consecutive polls. The log
# path is usually one an earlier boot wrote, and in the command above the shell
# starts this script an instant before `> qemu.log` truncates it; one look
# could find the OLD boot's lines. A stale line is gone by the next poll, a
# real one is not. (Stage 1 needs no second look. It has the same problem --
# an earlier boot's line, still in the FIFO -- and deals with it differently:
# the FIFO is emptied before the boot can have written to it. See below.)
#
# How the FIFOs are handled, and why this way:
#
#   * The script never creates them. tools/qemu-run.sh does, just before QEMU
#     starts -- possibly minutes after this script, with a build in between --
#     so stage 1 begins by waiting for <base>.out to exist. Anything there that
#     is not a FIFO is an error at once (exit 1): the runner refuses the same
#     thing, and no boot is coming.
#   * A FIFO that is already there when the script starts is EMPTIED first,
#     both of the pair. An earlier boot's bytes can still be in it: on macOS
#     what QEMU wrote to <base>.out with nobody reading is still there after
#     QEMU has died and every descriptor is closed (measured, slice 6.6 Task
#     5: 826 bytes after an unfed boot -- the firmware's text and the guest's
#     line). Unemptied, the next fed boot's feeder finds that OLD line at
#     once, writes its answer into a machine that is still in its firmware,
#     and the answer is lost: no `hvc0 ok`, and by the coupling no `stdin ok`
#     (observed, exactly so). This is the two-looks rule's twin. It is safe
#     because the script starts with the boot command: QEMU is at least a
#     freshness check and an image build away, and the guest's line seconds
#     of boot beyond that. A FIFO the runner creates LATER is new and is not
#     touched. The price: started against a boot that is already running,
#     this script may throw the guest's line away and then never feed. Start
#     it with the boot, as above.
#   * <base>.out is then read by a background `cat` into a private temporary
#     file, which is polled with grep exactly like the serial log. That keeps
#     the binary firmware text out of the shell (bash's `read` drops NULs,
#     splits on newlines the firmware does not send, and in bash 3.2 loses a
#     partial line on a timeout), and it keeps the port drained for the rest
#     of the boot. The `cat` opens the FIFO read-only, which blocks until QEMU
#     has it open -- in the background, where it costs nothing -- and reads
#     end-of-file when QEMU lets go.
#   * That end-of-file is used: once the `cat` has exited having read
#     something, QEMU is gone and no line this script waits for can still
#     arrive, so it gives up there and then instead of polling a dead boot's
#     log until the ceiling -- and, in stage 1, instead of sitting on the FIFO
#     where it would take the NEXT boot's line from that boot's own feeder.
#     (A `cat` that exits having read nothing is started again: something
#     other than a boot opened the FIFO and closed it.)
#   * <base>.in is opened read-write, which never blocks on a FIFO -- a plain
#     `> <base>.in` would wait for a reader for ever if QEMU had died.
#
# Exiting after the last write closes QEMU's stdin. That is harmless: with
# stdin at EOF QEMU carries on and the boot is unchanged (measured, slice 6.5
# Task 0). So there is nothing to hold open, and this script never outlives its
# job. The `cat` and the temporary file go with it, on every exit path.
#
# It also gives up: FEED_CONSOLE_CEILING seconds after it started, whichever
# stage it is in, it exits 1, so a boot that never gets that far (or never
# starts) cannot leave a poller behind for good. In stage 1 that is with
# NOTHING written anywhere; in stage 3 with nothing written to stdout. If
# stdout is closed when the serial probe is written -- QEMU already gone -- the
# write fails and the script exits 1.
#
# stdout carries the serial probe and nothing else. Every diagnostic goes to
# stderr, one line per stage.
#
# An unfed boot (stdin from /dev/null or a terminal, no feeder) is not
# affected by any of this: both FIFOs work with nobody on them, and both proof
# children simply stay parked.
#
# Environment (optional):
#   FEED_CONSOLE_CEILING=<seconds>  give up after this long (default 1800: a
#                                   1200 s boot plus the build in front of it)
#   MINIXRS_HVC0=<base>             the second console's FIFO pair, as for
#                                   tools/qemu-run.sh
#
# Exit status: 0 both consoles were fed; 1 the ceiling passed, QEMU went away,
# a FIFO path held something else, or a write failed; 64 usage.

set -euo pipefail

# What is written to the serial console, as a printf %b string: `\n` is the
# only escape in it. userland/init's host test parses THIS LINE -- keep it one
# line, single-quoted. The same goes for the three HVC0_ lines below.
PROBE='minix.rs stdin\n'

# What the guest writes to the second console, in the same %b spelling as
# init's HVC0_OUT -- the line stage 1 waits for.
HVC0_OUT='minix.rs hvc0 out\n'

# What is written back to it (init's HVC0_IN).
HVC0_IN='minix.rs hvc0 in\n'

# The child's verdict in the serial log, without its count.
HVC0_OK='minix.rs init: hvc0 ok'

# Stage 1's grep pattern: the guest's line less the `\n` escape that ends it
# (two characters here, a backslash and an `n`). grep -F wants no newline.
HVC0_LINE=${HVC0_OUT%\\n}

# What else stage 3 waits for. VFS's own text (servers/vfs/src/main.rs); if it
# moves and this does not, the feeder never writes the serial probe and
# `stdin ok` goes missing -- loudly.
MARKER='[diag vfs] cons.beside ok'

# Seconds between looks. Short enough to be invisible in a boot's budget, long
# enough that a stale log is truncated between two of them.
POLL=0.2

# How long a FIFO that predates the script is read for, to empty it. What is
# in a pipe is read at once; this only has to outlast `cat` starting up, and
# stay far shorter than the time a boot takes to reach init.
DRAIN=0.5

# Where each probe is cut, and how long the feeder waits between the halves.
# The cut is inside both texts, well clear of the `\n` escape at their ends.
SPLIT=9
GAP=3

# How long after the guest's line the second console's answer is held back,
# so that the read it answers is parked by then. See stage 2 in the header.
SETTLE=3

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

# The FIFO pair's base: tools/qemu-run.sh's default, computed as it computes
# it. git is asked only when the default is needed.
if [[ -n "${MINIXRS_HVC0:-}" ]]; then
    HVC0_BASE="$MINIXRS_HVC0"
else
    HVC0_BASE="$(git rev-parse --show-toplevel)/target/hvc0"
fi
FIFO_OUT="$HVC0_BASE.out"
FIFO_IN="$HVC0_BASE.in"

# Everything read from <base>.out so far, and the `cat` that is reading it.
CAPTURE=""
CAT_PID=""

# shellcheck disable=SC2329  # invoked by the EXIT trap below
cleanup() {
    if [[ -n "$CAT_PID" ]]; then
        kill "$CAT_PID" 2>/dev/null || true
        wait "$CAT_PID" 2>/dev/null || true
    fi
    if [[ -n "$CAPTURE" ]]; then
        rm -f -- "$CAPTURE"
    fi
}
trap cleanup EXIT
# bash runs the EXIT trap for a signal only if the signal is itself trapped.
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
# A write to a closed stdout must come back as a failed printf, not as a
# death that skips the cleanup above.
trap '' PIPE

# Start the `cat` that copies <base>.out into the capture, appending. It opens
# the FIFO read-only: blocked until a writer (QEMU) has it open, end-of-file
# when the last writer lets go.
start_capture() {
    cat -- "$FIFO_OUT" >>"$CAPTURE" 2>/dev/null &
    CAT_PID=$!
}

# True once that `cat` has exited having read something: QEMU had the port
# open, wrote to it, and has closed it -- the boot is over. (bash reaps the
# child as it exits, so the pid stops answering.) A `cat` that exited with
# nothing read was not ended by a boot; it is started again and this is false.
qemu_gone() {
    if kill -0 "$CAT_PID" 2>/dev/null; then
        return 1
    fi
    if [[ -s "$CAPTURE" ]]; then
        return 0
    fi
    start_capture
    return 1
}

# Empty a FIFO that was there before this boot: read it, read-write so the
# open cannot block, for DRAIN seconds, and throw the bytes away.
drain_stale() {
    local n
    cat <>"$1" >"$CAPTURE" 2>/dev/null &
    CAT_PID=$!
    sleep "$DRAIN"
    kill "$CAT_PID" 2>/dev/null || true
    wait "$CAT_PID" 2>/dev/null || true
    CAT_PID=""
    n=$(($(wc -c <"$CAPTURE")))
    if ((n > 0)); then
        echo "feed-console: discarded $n stale bytes an earlier boot left in $1" >&2
    fi
    : >"$CAPTURE"
}

# Is `$1` a FIFO? Something else in its place ends the script: the runner
# refuses to boot over it, so there is nothing to wait for.
fifo_ready() {
    if [[ -p "$1" ]]; then
        return 0
    fi
    if [[ -e "$1" || -L "$1" ]]; then
        echo "feed-console: $1 exists and is not a FIFO; nothing was written" >&2
        exit 1
    fi
    return 1
}

# grep needs -a (both the capture and the log read as binary: firmware escapes
# in one, raw tick bytes in the other) and -F (every pattern is a literal,
# brackets and all). A file that does not exist yet is a file without the line.
has_line() {
    grep -aqF -- "$1" "$2" 2>/dev/null
}

SECONDS=0
CAPTURE="$(mktemp "${TMPDIR:-/tmp}/feed-console.XXXXXX")"

# ---- Before anything: empty the FIFOs an earlier boot left behind. ---------

for fifo in "$FIFO_OUT" "$FIFO_IN"; do
    if fifo_ready "$fifo"; then
        drain_stale "$fifo"
    fi
done

# ---- Stage 1: the guest's line, on the host side of the second console. ----

while ! fifo_ready "$FIFO_OUT"; do
    if ((SECONDS >= CEILING)); then
        echo "feed-console: gave up after ${CEILING}s: $FIFO_OUT never appeared (no boot?); nothing was written" >&2
        exit 1
    fi
    sleep "$POLL"
done

start_capture

until has_line "$HVC0_LINE" "$CAPTURE"; do
    if qemu_gone; then
        # One last look: the line and the end-of-file can arrive together.
        if has_line "$HVC0_LINE" "$CAPTURE"; then
            break
        fi
        echo "feed-console: QEMU closed $FIFO_OUT after ${SECONDS}s without '$HVC0_LINE' on it; nothing was written" >&2
        exit 1
    fi
    if ((SECONDS >= CEILING)); then
        echo "feed-console: gave up after ${CEILING}s: '$HVC0_LINE' never came out of $FIFO_OUT; nothing was written" >&2
        exit 1
    fi
    sleep "$POLL"
done
echo "feed-console: saw the hvc0 line after ${SECONDS}s ($FIFO_OUT carried '$HVC0_LINE')" >&2

# ---- Stage 2: the answer, into the second console, in two chunks. ----------

# The runner made both FIFOs before QEMU started, so this one is there.
if ! fifo_ready "$FIFO_IN"; then
    echo "feed-console: $FIFO_IN is missing; nothing was written" >&2
    exit 1
fi
# Give the child's read time to park (the header says why this is a wait).
sleep "$SETTLE"
# Read-write: never blocks, reader or no reader. Descriptor 4, so that stdout
# stays the serial console's and nothing but its probe can reach it.
exec 4<>"$FIFO_IN"
# Two chunks a few seconds apart, so the second needs a second interrupt and a
# receive buffer the driver has posted again. Do not merge them.
if ! { printf '%b' "${HVC0_IN:0:SPLIT}" >&4 && sleep "$GAP" && printf '%b' "${HVC0_IN:SPLIT}" >&4; }; then
    echo "feed-console: the write to $FIFO_IN failed; the serial console was not fed" >&2
    exit 1
fi
exec 4>&-
echo "feed-console: fed hvc0 after ${SECONDS}s ($FIFO_IN)" >&2

# ---- Stage 3: both lines in the serial log, on two consecutive polls. ------

seen=0
while ((seen < 2)); do
    if has_line "$MARKER" "$LOG" && has_line "$HVC0_OK" "$LOG"; then
        seen=$((seen + 1))
        if ((seen >= 2)); then
            break
        fi
    else
        seen=0
    fi
    if qemu_gone; then
        echo "feed-console: QEMU went away after ${SECONDS}s before $LOG showed both '$MARKER' and '$HVC0_OK'; the serial console was not fed" >&2
        exit 1
    fi
    if ((SECONDS >= CEILING)); then
        echo "feed-console: gave up after ${CEILING}s: $LOG never showed both '$MARKER' and '$HVC0_OK'; the serial console was not fed" >&2
        exit 1
    fi
    sleep "$POLL"
done

# ---- Stage 4: the serial probe, on stdout, in two chunks. ------------------

# Once, in TWO chunks a few seconds apart, so the second chunk needs a second
# interrupt: it arrives only if TTY re-armed its line after draining the
# first. (Written in one piece the whole probe is one interrupt, and a dropped
# IRQ_ENABLE passes -- measured, slice 6.5 mutation 5.)
if ! { printf '%b' "${PROBE:0:SPLIT}" && sleep "$GAP" && printf '%b' "${PROBE:SPLIT}"; }; then
    echo "feed-console: the write failed (stdout closed?); nothing more to do" >&2
    exit 1
fi
echo "feed-console: fed the probe after ${SECONDS}s ($LOG shows '$MARKER' and '$HVC0_OK')" >&2
exit 0

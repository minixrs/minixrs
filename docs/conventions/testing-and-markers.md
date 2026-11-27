# Testing and markers

How to verify a server that cannot print, what the two kernel trace samplers do and do not catch,
the boot-marker-file contract, the screen check, and the mutation-testing discipline. See
[`build-and-boot.md`](./build-and-boot.md) for the boot commands themselves (how to produce and time
a log) — this file covers how to read one.

## Verifying server behaviour

User-space servers run at EL0 with no console access — they cannot print. Verify server behaviour
through kernel-side traces (`[pf]` from `do_page_fault`, `[ksys …]` from `do_vmctl`/`system`, `[ipc
N]` from `ipc::dispatch`), never server-side logging.

Trace sampling is asymmetric between the two: `[ipc N]` head-traces the first ~12 calls *plus* every
100th, but `[ksys N]` samples only every 100th (no head carve-out) — a server's first/rare kernel
call (e.g. a startup `SYS_GETINFO`) shows on `[ipc]`, not `[ksys]`.

Since slice 5.1 a server can also print through `SYS_DIAGCTL` via `server-rt::diag_print`; see
[`kernel.md`](./kernel.md) for the debug channel's kernel half. That is a channel for deliberate
diagnostics, not a replacement for the traces above — nothing routes a server's ordinary execution
through it.

The two `EFAULT` traces — `[efault] proc=… nr=… call=… va=…` and `[efault deliver] proc=… nr=… va=…`
— are **uncounted**, unlike the sampled `[ipc {n}]` form, which is what makes them stable boot
markers instead of ones that can fall out of a fixed-size sample window. The `do_ipc` one keys on
`result == EFAULT`, which is unambiguous because nothing else in the kernel produces that errno.
Keep that invariant in mind before changing `do_ipc`'s tracing — it's the thing that makes the
`[efault]` marker unambiguous.

## Trace forensics

The `[ipc]` modulo sampler almost never catches low-rate callers — a blocking SENDREC client (e.g. a
fork/wait loop) round-trips once per rotation, thousands of times rarer than a synchronous
kernel-call flood from a busier caller — and a server packed late in the MXBI archive boots after
that flood starts, so its SEF handshake never lands in `TRACE_HEAD` either.

**Zero sampled lines for such a caller is NOT evidence it's stuck.** Verify via its downstream
head-carved `[ksys …]` traces instead (e.g. `[ksys SYS_FORK]`/`SYS_EXIT` are head-carved at 6, so
raise the head const temporarily to count real cycles), or add a temporary unconditional `[DBG]`
trace in `ipc::do_ipc` keyed on the caller nr — remove it before committing.

## Exit-status probes

A process that cannot print at all — not even through a driver, e.g. `worker`'s exec-frame validator
running before a console exists — reports its verdict through its own exit status. **Encode the
pass, never the absence of a failure.** `execstack::EXEC_STACK_PROBE_PASS` (`0x5A`) is the positive
sentinel a passing probe returns; status `0` prints `FAIL no-verdict` rather than being read as
success, because two other paths can produce a `0` that means nothing:

- `mproc::handle_kill_in` zombifies a signalled process without touching `exit_status`, so a probe
  that died before it could report is reaped with status `0`.
- A frame bad enough to fault the probe lands it in VM's SIGSEGV arm, which prints no `!!! EL0 data
  abort` for the forbidden-marker list to catch — a silent, verdict-free death.

Companion rule: key any reap-derived marker on the child's **pid**, not on "the first reap" —
`alloc_pid` never returns `0`, so `0` is the correct "not yet" sentinel, and pid-keying is what
stops an unrelated zombie (reaped first, by the same parent) from being mistaken for the probe's own
child.

## Markers

`tools/check-boot-log.sh <disk|ramdisk|reboot> <log>` greps a captured boot log against the marker
files (see [`build-and-boot.md`](./build-and-boot.md) for how to produce the log, which backend a
boot ran with, and how long to budget). Update those marker files in the same PR when trace formats
or the boot roster change, or the `qemu-smoke` CI job goes red.

There are four pairs of files — eight files since slice 6.4 — and the checker reads two pairs for a
full boot and one for the reboot:

| Pair                                      | Checked for      | Holds                                                                                    |
| ----------------------------------------- | ---------------- | ---------------------------------------------------------------------------------------- |
| `tests/qemu-boot.expected` / `.forbidden` | every full boot  | everything that must hold whichever driver serves the root                               |
| `tests/qemu-boot.disk.*`                  | a `disk` boot    | the device's bring-up, `root ok`, the flush, `name=vblk`; forbids the fallback           |
| `tests/qemu-boot.ramdisk.*`               | a `ramdisk` boot | the same bring-up, `root FAIL no root partition`, the fallback line, `name=memory`       |
| `tests/qemu-boot.reboot.*`                | the reboot only  | `persist gen=1` and the two lines saying the root is the disk's; forbids `persist gen=0` |

- **Both full boots attach the boot disk.** Since slice 6.4 the machine boots from its disk, so the
  `ramdisk` boot is the **no-root fallback** boot: the same image without the root partition, which
  `vblk` brings up and declines and MFS falls back from. The device's four bring-up lines are
  therefore required by *both* per-backend files, and they are not in the common file only because
  the hand-run `MINIXRS_ROOT=nodisk` boot has none of them and must still pass the common set.
- **The no-device state has no checked boot.** `[diag vblk] no device` and a DS miss in MFS are
  reached only with `MINIXRS_ROOT=nodisk`, by hand; both per-backend files forbid `no device`, so
  that log passes the common pair and fails both backends' by design.
- **The backend argument is required, and there is no way to check the common pair alone.** The
  common files cannot tell a disk boot that silently fell back to the ramdisk from a healthy one:
  every filesystem marker passes either way. What closes that is the **forbidden** half of the
  per-backend pair — a `disk` boot forbids `bdev.ds ok name=memory`, `bdev.root vblk`, `[diag vblk]
  no device`, `[diag vblk] root FAIL` and `bdev.minor absent`; a `ramdisk` boot forbids `bdev.ds ok
  name=vblk`, `[diag vblk] root ok`, `[diag vblk] no device`, `[diag vblk] flush ok` and `bdev.minor
  ok`. Each requires a line the other forbids, so each boot fails the other backend's set.
- **The reboot is checked against its own pair and nothing else** — not the common pair, not the
  disk pair. It is the second boot of the image a disk boot wrote, and init's write battery was
  designed against a fresh image: it creates files that must not exist, fills a file that ships
  empty, grows a directory that ships exactly full. What it does on its own leftovers is not
  defined, so a full check of a used image would judge behaviour nobody specified. The persistence
  probe therefore runs *before* the battery and the reboot is stopped soon after its line. Two
  consequences: `persist gen=0` is required of every full boot and forbidden on a reboot, so neither
  log can pass as the other; and the common forbidden list is not applied to a reboot, so a panic
  there is caught only by `persist gen=1` never appearing.
- **The full boots are fed boots, and exactly three required lines need the feeding.** Slice 6.5 put
  five console lines in the common expected file — `[diag tty] rx on`, `[diag vfs] cons.notify ok`,
  `[diag vfs] cons.park`, `[diag vfs] cons.beside ok` and `minix.rs init: stdin ok n=15` — and slice
  6.6 added two for the second console, `[diag vfs] cons.other ok` and `minix.rs init: hvc0 ok
  n=17`. The last of 6.5's and both of 6.6's print only when `tools/feed-console.sh` fed the boot
  (the command form, and what the feeder waits for, are under
  [Fed boots](./build-and-boot.md#fed-boots)). The other four depend on no input and every boot
  prints them, fed or not: the receive masks were set, TTY's notify probe arrived at VFS, a read was
  parked, a file read was served beside it. An **unfed** full boot therefore fails on exactly
  `cons.other ok`, `hvc0 ok` and `stdin ok`, by design; that is a missing feeder, not a regression,
  and it is the first thing to rule out when those three are the only lines missing. The same goes
  for the forbidden `stdin FAIL` and `hvc0 FAIL`: they describe a fed boot, and a boot somebody
  typed at prints `stdin FAIL` correctly. The **reboot is not fed** and loses nothing by it — it is
  checked against its own pair only, which holds none of the seven.
- **The three fed lines come in a fixed order: `cons.other ok`, then `hvc0 ok n=17`, then `stdin ok
  n=15`** — and the first missing one names the cause. The feeder feeds the second console first.
  Its first chunk makes `vcon` notify VFS, whose walk passes over the parked serial reader and gets
  an answer for the `/dev/hvc0` reader behind it — the `EFAULT` for that child's read into an
  unmapped buffer — and that is when VFS prints `cons.other ok`, a whole feeder gap ahead of the
  child's verdict. (The 6.6 spec and plan had `hvc0 ok` first; three fed stub-free boots and a fed
  full one all showed this order.) The serial console is fed only after `hvc0 ok` is in the log, so
  **a boot whose `vcon` is broken loses `stdin ok` as well** — a coupling the feeder states rather
  than hides. A missing `stdin ok` with `hvc0 ok` present is the serial path; all three missing is
  an unfed boot or the second console, and `cons.other ok` present with `hvc0 ok` missing points
  past the first chunk's walk — at the bytes the child then has to collect.
- **`hvc0 ok` proves the second console in both directions, and the order of a copy and a consume.**
  It cannot print unless the feeder's bytes arrived, and the feeder sends them only after reading
  the guest's own line off the host side of the chardev. The child's first read is into an unmapped
  address and must answer `EFAULT` having consumed nothing — it then has to read all 17 bytes. And
  the probe arrives in two chunks, so the second needs `vcon` to have re-posted its one receive
  buffer and re-armed its line after the first was drained.
- **`stdin ok` proves two interrupts, not one, and only because of how the boot is fed.** The feeder
  sends the probe in two chunks a few seconds apart ([Fed boots](./build-and-boot.md#fed-boots) has
  the why), so the line needs the receive interrupt, the drain, the notification, VFS's retry — and
  then TTY's **re-arm** of its line, without which the second chunk never interrupts. Fed in one
  write, the same line proves everything but the re-arm.
- **`cdev.short ok n=256` has carried a second proof since slice 6.5**, and `cons.notify ok` exists
  for the same one: a kernel rule about notifications and `SENDREC`, stated with its failure line in
  [`kernel.md`](./kernel.md#a-pending-notification-is-never-the-reply-to-a-sendrec). A `cdev.short
  FAIL rc=4096` is that rule broken, not a console-write bug.
- **The framebuffer console's seven lines are common, and their numbers are a measurement** (slice
  6.7). Three are required of every full boot — `[fb] ok w=800 h=600 pitch=3200 r=16 g=8 b=0
  usable=0`, `[devmap] tty fb va=0x100000000 pages=469` and `[diag tty] fb ok w=800 h=600 cols=100
  rows=37` — and four are forbidden: `[fb] none`, `[fb] FAIL`, `[diag tty] fb none` and `[diag tty]
  fb FAIL`. The runner attaches `ramfb` to every boot, so a boot without a screen is a failure
  whichever side reports it; they are listed as forbidden lines, rather than left to `fb ok` going
  missing, because after any of the four the console is serial-only and *every other marker still
  passes*. The geometry is the mode the firmware set, not a constant of this repository
  ([`build-and-boot.md`](./build-and-boot.md#the-display-and-the-qmp-socket)): a firmware with
  another default moves **all three expected lines together** — `w`/`h`/`pitch` in the first,
  `pages=` (`ceil(pitch * h / 4096)`) in the second, `w`/`h`/`cols`/`rows` in the third. Three
  moving at once is that; one moving alone is a bug. All three are serial evidence that TTY believed
  it had a screen and that its stores did not fault. That the pixels are right is not theirs to say
  — it is [the screen check](#the-screen-check)'s. The reboot's pair holds none of the seven.
- **A new marker goes in the common pair unless it exists because of one backend.** Everything the
  filesystem proves — mount, read, write, create/truncate, the hole probe, exec from the root — is
  common and therefore required of both drivers. A marker moved into a per-backend file stops being
  required of the other backend, so that is a decision, not a tidy-up.
- **`tools/diff-backends.sh <disk-log> <ramdisk-log>` checks what the marker files cannot: that
  nothing *else* differs.** It takes each log's `[diag mfs]` lines and `[diag vfs]` lines as two
  separate streams, keeps the first occurrence of each line in order, and fails unless the VFS
  streams are identical and the MFS streams differ in exactly the `bdev.ds ok name=` line — with two
  further allowances since slice 6.4, each for one exact line: the ramdisk side may carry `bdev.root
  vblk rc=-6 fallback=memory` (at most once; allowed, not required — requiring it is the marker
  file's job), and the `bdev.minor` verdict is each backend's own (`ok minor=1 mbr=1` on the disk
  side, `absent minor=1 rc=-6` on the ramdisk side, at the same place in the two streams, at most
  once each). Another `rc`, a longer line, a `FAIL`, or either line on the wrong side still fails.
  The shape was measured, not chosen: two boots of the *same* backend differ in the raw interleaved
  stream (the servers interleave differently from boot to boot, and VFS prints one `pm-exit reset`
  line per exit for as long as the boot runs), and agree per server by first occurrence. What it
  therefore **cannot** see is a backend-dependent change in how often a line repeats, or in the
  order of lines between the two servers. `--self-test` proves it still fails on a difference; CI
  runs that before the comparison.

A few properties of the checker matter when you're reading its output rather than just running it:

- **The marker files test first occurrences only**, never counts — keep expectations timing-robust,
  since CI's TCG is slower than local and a marker that only ever appears once is the only kind that
  survives that difference.
- **It works on a partial log.** Once it reports PASS on a still-growing log, the verdict is final
  and the boot can be stopped — the 5.10b review round got all 97 markers well inside the 600 s
  budget. Only `qemu-smoke`'s exit-124 assertion needs the timeout to actually elapse. Copy the log
  aside before checking; the live file grows underneath the script.
- **Its PASS total counts expected AND forbidden lines** (5.11: 86 + 20 = 106), so a slice that adds
  N expected and M forbidden lines moves the total by N + M — don't be surprised when the total
  moves by more than the expected-marker count alone. The total is per boot kind, because each adds
  its own pair to the common one (6.4: 165 for each full boot, 7 for the reboot; 6.5 added six
  expected and three forbidden lines to the common pair, so 174 and still 7; 6.6 added nine expected
  and seven forbidden, so 190 and still 7; 6.7 added three expected and four forbidden, so **197**
  and still 7 — counted from the files as committed: 125 + 59 common, 8 + 5 per backend, 3 + 4 for
  the reboot); count the marker files rather than quoting a number from a plan.
- **The fork-cycle markers name slots, and a long-lived child of init moves them.** PM's fork pool
  starts at 19 and init's first forks are its two proof children, so the `read(0)` child holds slot
  19, the `/dev/hvc0` child slot 20, and the rotation's first `worker` runs in **slot 21** — three
  `SYS_FORK … child_nr=` lines (19, 20, 21), then `SYS_EXEC target=21 name=worker` and `SYS_EXIT …
  nr=21`. That holds with the stubs and without them (their slots, 15–18, lie below the pool), and
  fed or unfed: an unfed child stays parked in its slot, and a fed one that has already exited is
  still a zombie holding it when the rotation first forks, because init's first `wait()` comes after
  that fork. Later turns recycle whichever slots are free, so only the *first* worker's slot is
  fixed — which is all a first-occurrence check needs — and no marker names the slot of the first
  `hello` (22 with stubs, 21 without). **Recompute these lines from the fork order when a slice adds
  or removes a child that outlives the first rotation turn; do not edit the digit.** 6.5 moved the
  worker 19 → 20 and 6.6 moved it 20 → 21.
- **A proof child closes every descriptor it opened before it exits.** VFS prints `[diag vfs]
  pm-exit reset closed=1` for any process that exits with one descriptor open, and that line is
  `fileio`'s to earn, late in the boot, by leaking one on purpose. 6.6's `/dev/hvc0` child first
  exited with its descriptor open: the line printed at 3.6% of a fed disk log instead of about 63%,
  every check still passed, and `fileio`'s proof had become vacuous — satisfied minutes early by a
  different process. The child now closes the node on every path, pass or fail, before it prints its
  verdict, and a failed close is its own `hvc0 FAIL step=close`. The general form: **a
  first-occurrence marker is owned by the first process that can print it**, so a new process that
  can print an existing marker's text has to be kept from doing so.
- **Never copy a count into a marker from a plan** — recompute it from the constant. 5.8's plan said
  `n=30` for what was actually a 31-byte `ROOTFS_MOTD`.
- **A marker must be able to fail on the thing it proves.** The script matches with `grep -F`, so a
  prefix is a substring: 6.2's planned `[devmap] vblk va=0x4000` also matched TTY's page at
  `0x40000000`, the exact off-by-one-page mapping it existed to catch. Assert the whole line when
  every field is deterministic. When one field varies by run, print the stable fields first — 6.2
  moved `pages=` ahead of the firmware-dependent `pa=` so that `pages=16` could be asserted. 6.7's
  two kernel lines were drafted with `pa=` in the middle and were reordered for the same reason:
  `[fb] ok … usable=0 pa=…` and `[devmap] tty fb va=… pages=469 pa=…` both end in `pa=`, which
  follows the firmware's memory map, and everything before it is asserted as one prefix. The second
  line's text is `tty fb va=` and not `tty va=` so that TTY's UART line cannot satisfy it.

## The screen check

The marker files read the serial log, and the serial log cannot say what is on the screen. Since
slice 6.7 that is checked separately, from the host: `tools/screendump.sh <qemu-log> <out.ppm>` runs
**beside** a fed full boot, takes QMP `screendump`s of the running guest until one shows a console
that has **scrolled**, and has `minixrs-fbcheck` (`tools/fbcheck`) decide whether that picture is
the boot's console. CI runs it beside both full boots and it blocks ([`ci.md`](./ci.md)); the reboot
is not screen-checked. Run it locally beside both full boots for any slice that touches what TTY
draws, the kernel's framebuffer path, or the runner's display arguments. The command form is in the
script's header, and the rule in full is the module documentation of `tools/fbcheck/src/check.rs`,
which this section follows and does not replace.

### What a pass proves

`fbcheck` applies three steps to a dump and the log of the boot it came from; the first that fails
is the verdict.

1. **Geometry.** The dump is exactly the `w=` × `h=` of the log's *last* `[diag tty] fb ok` line,
   and the grid the renderer library derives from that size is exactly that line's `cols=` ×
   `rows=`. The line must be complete and is read **exactly**, with no allowance for stray bytes:
   the kernel writes a `[diag …]` line whole, inside one kernel call, so nothing can land in it.
2. **Every cell is a glyph.** Each 8×16 cell decodes — all 128 pixels one of the two console
   colours, forming one printable character — and every pixel outside the grid is background. The
   decoded text, painted again by the same library, must reproduce the dump pixel for pixel.
3. **The text is the boot's, and it is where the boot put it.** At least 8 rows (`MIN_ROWS`) are
   non-blank, and **the screen, read top to bottom, is one unbroken stretch of the console stream
   after the last `[diag tty] fb ok` line**: every row where the row above it leaves off, a line
   ending where a row stops short of the last column, a line wrapping where it does not.

Step 3 is what makes this more than the renderer agreeing with itself: the log was written by TTY's
UART sink, which shares nothing with the renderer but the bytes it was handed. A golden glyph drawn
by hand in `font`'s host tests covers what is left — a font read wrongly on both sides. Because the
rows are anchored to each other, **row order, a repeated row, a row cut short, a row shifted left
and a row left over from before a scroll are all seen**: each breaks the stretch.

### How the screen is placed in the log

Not with `grep`. The serial line has three writers and the console is one of them:

- **The console stream.** The log is cut at the `fb ok` line step 1 used — nothing before it was
  drawn by this TTY — and every kernel line is taken out of the rest: from a `[` followed by one or
  more of `a`–`z`, `_`, `.`, `-` and then a space or a `]` (`[ipc `, `[ksys `, `[diag tty]`,
  `[efault `), through the next `\n`. What is left is the console's bytes with the kernel's tick
  bytes still among them.
- **A stray byte** is what a tick prints: an ASCII letter, or `?`. Never a digit, a space,
  punctuation or a line end.
- **The walk.** From the top row down to the last non-blank one, the stream must hold, in order and
  with nothing else between: the row's text (trailing blanks removed), byte for byte, with stray
  bytes allowed between two of its bytes — **at most 120 per row** (`MAX_STRAY`); then up to the
  row's width in blanks; then a line end — or, only if the row reached the last column, nothing,
  because the line wrapped and the next row carries straight on. A full row may be followed by
  either, since a line of exactly the screen's width followed by a newline takes one row.
- **Stray bytes between the pieces are not counted.** Before a row's first byte, after its last and
  inside a line end they may come in any number — the runs between lines reach thousands — and a run
  ends at the first byte that is not a letter or `?`. The one place none is allowed is after a
  trailing blank.
- **Two rows are special.** The last non-blank row needs only its text to be there: it may be the
  line still being written, and the log is read after the dump. The top row decides whether the
  screen has scrolled.

### Scrolled, or not yet

A screen whose top row begins at the very start of the console stream has **not scrolled**: row 0 is
TTY's banner and everything since is on the screen in order. A screen that begins later in the
stream has **scrolled** — and must then be *full*, its last non-blank row being the last row or the
one above it; one that begins later and is not full is refused, because that is what a scroll that
cleared the screen instead of moving it looks like. The not-scrolled reading is tried first.

**Only a scrolled screen proves the scroll path**, and a pass that does not require one proves less
than it appears to. In the default boot the script's gate line appears with 22 of the screen's 37
rows written. Slice 6.7's first script took the first dump that passed, so CI never looked at a
scrolled screen, and a TTY whose scroll repainted *nothing* passed the check
([mutation 7](../plans/phase-6-virtio.md#slice-67-framebuffer-console)). `fbcheck --scrolled`
therefore reports a right-but-unscrolled screen as `early` — neither pass nor failure — and
`screendump.sh` always asks for it.

### What a pass does not prove

These are the checker's own list ("What step 3 does not see"). Know them before citing a passing
screen check as evidence.

- **Lost letters.** This is the price of allowing tick bytes at all. Letters missing from *inside* a
  row pass, up to the cap of 120: `minix.rs it: stdin ok` is accepted for the log's `minix.rs init:
  stdin ok`, with `ni` taken for tick bytes. So do any number of letters missing from the very
  **start** of a line (`x.rs init: …`, with `mini` taken for a run before the line) or from its very
  **end** (`… stdio o` for `… stdio ok`) — so a line made of nothing but letters can be shown as a
  blank row. A lost digit, space or punctuation mark is seen; so is any wrong, extra or moved
  character.
- **The end of the last non-blank row.** It is matched as a prefix, so a last row cut short passes.
- **The head of a scrolled screen's top row.** It may begin anywhere in a line, so a top row that
  lost its first characters passes — and so does a **wrapped line that begins there** and lost
  characters anywhere along it, since the whole of it can be read as starting later.
- **A frozen screen.** A screen that stopped being drawn some time ago is still an unbroken stretch
  of the stream; nothing compares the screen's last row with the stream's end.
- **Console text that looks like a kernel line is falsely refused.** A console line holding `[word]`
  or `[word ` is taken out from there to its end, and the screen showing it is refused: a false
  *failure*, never a false pass. Nothing the boot prints to the console does. A program that does —
  a shell saying `[done]`, a harness printing `[ok]` — will fail this check, and the fix belongs in
  the checker's rule or the program's text, not in more tries.
- **Bytes the screen draws differently are falsely refused too.** A tab (a cursor move), an
  unprintable byte (`?`) and a `\r` not followed by `\n` (back to the start of the row) each make a
  screen the log does not hold as drawn. Nothing in the boot writes one to the console.
- **A line that ends in a blank followed by tick bytes** is a third false-failure risk the rule
  accepts: no stray may follow a trailing blank. No console line of the measured boots ends in one.
- **Nothing about colour beyond the two it draws.** Grey has three equal channels, so swapped
  channel shifts put the same pixels on the screen (mutation 5 moved nothing).
- **One instant, one screenful.**

### The script: early, tries, and the ceiling

`screendump.sh` waits for `minix.rs init: stdin ok` in the log — the last line the feeder causes, so
that its pauses of the guest cannot disturb the feeder's timed exchanges, and seen on two
consecutive polls for the feeder's stale-log reason. It then **looks every 10 seconds**
(`SCREENDUMP_GAP`) with `minixrs-fbcheck dump-and-check --scrolled`. **Every look pauses the
guest**: `stop`, `screendump`, `cont` — `cont` always — and the log is read only *after* the dump,
since TTY writes the UART before it draws. Three things can come back:

| Line                                         | Exit | What the script does                                               |
| -------------------------------------------- | ---: | ------------------------------------------------------------------ |
| `fbcheck: ok <fields> scrolled=yes`          |    0 | the first one wins: prints it and exits 0                          |
| `fbcheck: early <fields> (not scrolled yet)` |    2 | looks again; **does not use up a try**                             |
| `fbcheck: FAIL <reason>`                     |    1 | uses up one of `SCREENDUMP_TRIES` (10); exits 1 when they are gone |

An `early` line is printed once per change in the number of rows on the screen, not once per look,
so a verdict file normally holds a few `early` lines and then the `ok`. The tries exist because a
dump can catch a glyph half-painted or a scroll half-repainted — the guest is stopped between two
pixel stores — and the next look is of a different instant. The consequence to keep in mind: **a
defect that shows on only some dumps is forgiven by design**, up to nine times. A screen that is
wrong fails every try, each with its reason on stderr, so read all of them.

`SCREENDUMP_CEILING` (1300 s) covers **the whole script**, from its start: it exits 1 in stage 2
because the line never came, or in stage 3 because the screen never scrolled or never stopped
failing. It is looked at between looks, so the script can outlast it by one look and one gap.
Because the gate is a fed line, the script is for fed boots only. Used by hand, `fbcheck`'s own exit
statuses are 0 (passed, or dumped), 1 (failed), 2 (`early`, with `--scrolled` only) and 64 (usage);
without `--scrolled` an unscrolled screen that is right is `ok … scrolled=no`, exit 0.

### Reading the verdict

`<fields>` is `<w>x<h> <cols>x<rows> rows=<n> wraps=<n> stray=<n> worst=<n>`: the geometry, the
non-blank rows, the rows that reached the last column and whose line carried straight on below, the
rows that matched only by allowing stray bytes inside them, and the most any one row needed. **Watch
`worst` against the 120.** The longest run measured inside a console line was 30, on a fast host
under hardware acceleration; ticks are counted in guest time, so a slower host writes proportionally
more of them into the same pause, and the cap was set with room for CI — where `worst` is unmeasured
until the branch's first run. The `FAIL` reasons of step 3 say which way the stretch broke:

- `screen row N is not in the serial log's console text, even allowing 120 tick bytes inside it` —
  the row's text is nowhere in the stream;
- `screen row N is in the serial log's console text, but not where the screen has it` — it does not
  follow the row above (shifted, repeated, out of order, or left over from before a scroll);
- `screen row N stops where the serial log's console text goes on` — cut short, or the row below is
  not its continuation;
- `the screen does not begin with the console's first line, yet it is not full` — a later stretch on
  a screen not written down to its last rows;
- `only N non-blank rows on the screen; at least 8 are needed`.

After the script ends `<out.ppm>` is the last dump taken.

### What the serial log really is

Measured on fed boots in slice 6.7 — 54 MB of log for a default disk boot, of which some 60 KB is
the console's — and worth knowing for anything that reads console lines out of a serial log, not
only for this check:

- **The kernel's trace lines are written whole, but they begin in the middle of console lines**,
  which carry on after them: `…-----[ksys 1412800] caller=17 call=0 result=0\r\n-----…`. About a
  million kernel lines in that boot.
- **The kernel's tick bytes arrive in runs as well as singly.** One byte per 10 ms of guest time
  while a process runs — the first byte of its name, or `?` (`kernel/src/clock.rs`) — so a console
  line can read `minix.rs initt: dev.null ok`, and, when nothing else prints between ticks,
  `minix.rs hello: iov CCCCCCCCCC....`. Between two lines they come thousands at a time.
- **A tick byte lands inside roughly one console line in a thousand**: 0.13% with no painting and
  0.09% with the changed-cells repaint, on 300 s stub-free boots (the figures are in
  [the 6.7 spec](../superpowers/specs/2026-11-26-framebuffer-console-design.md#7-corrections-found-in-implementation)).

`check-boot-log.sh` matches with `grep -F` and allows for neither, so a marker that is a *console*
line — the `minix.rs …` family — is exposed to both in a way a kernel-printed `[diag …]` line is
not. That exposure predates the screen; slice 6.7 measured the rate above and not how often a marker
is hit. If one console-line marker is missing with its neighbours present, grep the log for its two
halves before believing it.

## A starved boot is not a regression

A boot on a loaded host fails markers that are not broken, and the failure mimics a real regression
in an unrelated subsystem. In the user-VA-map slice alone that produced three wrong conclusions —
"the budget must rise to 1800 s", "the filesystem write path hangs", and "the line I just added
broke the boot". Every one of them was host contention.

The discriminator costs one command. The IPC counter is the throughput proxy: QEMU under TCG
advances guest time at whatever share of the host it gets, so a starved run simply does less work in
the same wall-clock budget.

```bash
grep -ao '\[ipc [0-9]*' <log> | tail -1     # final IPC counter, the throughput proxy
uptime; ps -eo pcpu,comm -r | head -5       # what was competing for the machine
```

A clean 300 s run on this branch reaches **~18.8 M**; a passing 1200 s run reached **114 M**. (Slice
6.3's baseline, two 300 s boots of its merge base, reached 32.3 M and 31.9 M — the floor below is
deliberately far under any clean run.) **Under ~10 M at the timeout means the run was starved, not
broken.** Re-run before believing any marker failure, and never change code on the strength of a
starved boot — a "fix" that passes on the re-run has proved nothing about the code.

**The shape of the missing set is the better tell, and the threshold alone will fool you.** One run
reached 10.59 M — over the floor — and was plainly starved. Read the whole `MISSING` list:

- **Contiguous from some point to the end of the boot** → the run stopped early. Starvation.
- **A specific subsystem's markers gone while later ones still land** → a real regression.

The first shape is unmistakable once seen: a starved run loses `SYS_FORK`, then every exec, then
everything downstream, because it never got there. A regression in the exec path would let fork
through and take out exec alone.

**Never pipe `check-boot-log.sh` through `tail`.** It prints one `MISSING` line per marker and the
verdict last, so `tail -2` shows the final miss and hides the rest — which is exactly how a
contiguous tail-of-boot failure gets mistaken for a single broken marker. Since the last missing
marker is usually the newest feature's, a truncated verdict points straight at whatever you just
changed. Use `grep -c '^MISSING'` and read the full list.

The control that settles it in one step: restore the merge base's copy of the file you changed,
rebuild, and boot again. If HEAD fails identically, the host is the cause and the file is innocent.
Restore via the scratchpad-snapshot discipline below, not `git checkout`.

## Mutation testing

The standard (established slices 5.1–5.3): apply a mutation, observe the named marker move, revert.
This runs against an **uncommitted** working tree, so the restore step has real hazards:

- **Never revert a mutation with `git checkout <file>`.** Copy the files to the scratchpad first and
  restore from there, then `diff -q` each back against the snapshot and `grep -rn MUTATION` to prove
  nothing leaked into the PR.
- **The scratchpad snapshot must cover files a slice ADDS, not just the ones it edits.** `git
  checkout -- <untracked file>` does not restore — it *errors* — and behind a `|| true` it silently
  leaves the mutation in the tree (slice 5.9 did exactly this to a new file). Snapshot every file
  you will mutate before the first run, and let the final `grep -rn MUTATION` sweep — never the
  restore command's exit status — be what proves the tree clean.
- **A mutation that fails to compile is indistinguishable from one that worked.** `kernel/build.rs`
  panics on the nested server build, the log holds no kernel output at all, and `check-boot-log.sh`
  reports every marker MISSING. Before recording any observation, run `grep -a 'error\[E' <log>` or
  confirm unrelated markers still PASS — slice 5.4 nearly recorded a false result this way.
- **Iterate and mutation-test in the stub-free config** (`--no-default-features`): 5.10b measured
  the `fs.*` markers landing at ~0.14% of a 60 s stub-free log, so `timeout 30` sufficed for nine
  boots that would each have cost 240 s otherwise. Judge such a run by grepping the specific marker,
  since `check-boot-log.sh` FAILs the stub A–D markers there by design. A mutation whose marker
  depends on console input needs the boot [fed](./build-and-boot.md#fed-boots) like any other; an
  unfed one never prints `cons.other ok`, `hvc0 ok` or `stdin ok` whatever the code does. **And a
  stub-free pass is not a verification of anything the stubs can disturb** — a change to a server's
  prologue or to when it first blocks needs a default boot through `check-boot-log.sh` (the rule and
  the defect that paid for it are under
  [Prologue ordering](./servers-and-drivers.md#prologue-ordering)).
- **A mutation of what TTY draws is judged by the screen check, on a fed boot, with every look's
  message read.** No serial marker moves when a pixel is wrong; run `tools/screendump.sh` beside the
  mutated boot and record the checker's reason (the cell that did not decode, the row and how it
  broke the stretch) rather than "the check failed". 6.7's two examples: `console_write` skipping
  the framebuffer moved **only** the screen check (`only 0 non-blank rows`), and a store at `x + 1`
  in the binary's `Surface` moved only the screen check too (a cell that is not a glyph) — the same
  edit made in the library's `paint_cell` instead fails 21 `paint` host tests, the call-site and
  helper forms being two rows as always.
- **A check that accepts the first passing sample has to be shown a sample of the state the mutation
  breaks.** 6.7's mutation 7 — a scroll that repaints nothing — **passed** the screen check as first
  built, on the default boot: the script's gate line appears with 22 of 37 rows drawn, the first
  passing dump won, and so no dump the check ever judged had scrolled. Nothing in the verdict said
  so. The check now requires `scrolled=yes` and anchors each row to the one above, and the same
  mutation fails on a scrolled screen (`screen row 36 is not in the serial log's console text…`).
  The general form is the "a probe proves as many events as it causes" rule below, applied to a
  sampler: ask what state the sample was taken in before believing what it cleared.
- **A device-mapping fault in a boot driver reads like a starved boot.** 6.7's mutation 3 mapped the
  framebuffer one page short. TTY faulted in its start-up clear — `[pf] proc=t far=0x1001d4000
  flags=0x1 -> VM` — and since VM has nothing to map there and TTY never returns, `[diag tty] fb ok`
  and the banner never print and **the whole boot stalls near `[ipc 1700]`, with no forbidden marker
  fired**. The missing set is contiguous to the end of the boot, exactly the
  [starved-boot shape](#a-starved-boot-is-not-a-regression), and the IPC counter is tiny for the
  same reason. The discriminator is the `[pf] proc=t far=0x1001…` line: a fault address in the
  framebuffer window. Grep for `[pf] proc=t` before calling such a log starved.
- **Feed a receive-path mutation with the standard feeder, never with a `printf` of your own.**
  `tools/feed-console.sh` sends the probe in two chunks; a hand-rolled feed that writes it at once
  delivers one interrupt, and every mutation of what TTY does *after* its first drain then passes.
  6.5's feeder itself first wrote the probe in one piece, and "drop the `IRQ_ENABLE` after the
  drain" passed under it, which is why it now has two chunks. If a mutation needs different input,
  keep it in at least two writes with a gap between them.
- **A probe proves as many events as it causes.** One write was one interrupt, so it proved one
  interrupt's worth of the receive path and nothing about the steady state. Before trusting a probe
  for a loop — re-arm, re-queue, retry — count how many times the probe makes the loop go round; if
  the answer is once, the code that sets up the *second* turn is unproved, and the mutation that
  deletes it is the way to find out.
- **Not every mutation moves its predicted marker.** Reordering the MXBI packing does not reliably
  break a DS lookup, because publish-before-retrieve is scheduler-dependent and often still resolves
  — to exercise a DS *fallback* branch, remove the peer entirely or break the key (slice 5.7 got
  that observation from the drop-the-server mutation instead, and slice 5.8 reconfirmed both halves:
  a driver swap moved nothing, while breaking a DS key gave a clean fallback FAIL).
- **Some correct invariants have no mutation that moves anything, and the honest record says so
  rather than omitting the row.** 5.10b found two orderings whose violation needs a failure
  *between* two steps, where the only available failure is `EIO` from a fixed RAM ramdisk whose
  block numbers are bounds-checked before they leave the filesystem server — no probe can induce
  one. A third case, an off-by-one in a bitmap-clear helper, moved no marker either but **is**
  caught by host unit tests — a different situation worth distinguishing. Before recording a row as
  uncovered, run `cargo test` under the mutation too, not just a boot.
- **A mutation predicted to be caught by "a host test" must name the test.** 6.3's spec predicted
  exactly that for skipping the read in `virtio-blk`'s partial write. The host tests cover the
  *plan* (`write_plan` returns `Partial`), not the step in `main.rs` that acts on it, which no host
  test calls; the mutation moved nothing at all until a boot probe was written for it
  (`bdev.partial`, see
  [`servers-and-drivers.md`](./servers-and-drivers.md#writing-probes-and-denial-batteries)). A
  prediction that cannot name its test is a guess that the coverage exists.
- **A refusal path needs a boot that provokes the refusal.** CI's boots prove a disk with a root and
  a disk without one (the fallback, since 6.4). Every other refusal — a read-only disk, a damaged
  partition table, a disk with no table at all — and the no-device state are boots run by hand with
  `MINIXRS_VBLK_IMAGE` / `MINIXRS_VBLK_DRIVE_OPTS` / `MINIXRS_ROOT=nodisk` and recorded in the phase
  file, because CI runs none of them.
- **The firmware writes to a writable boot disk, so boot a damaged-table image read-only.** edk2
  repairs a damaged primary GPT header or entry array from the backup copy *before the kernel runs*
  (6.4: one bit flipped in either, and the driver printed `root ok` with the full disk set passing).
  A mutation or refusal boot that depends on the damage reaching the driver needs `readonly=on`, and
  where the firmware then refuses to boot the disk at all (a damaged header), the second-drive
  recipe in [`build-and-boot.md`](./build-and-boot.md). The same fact limits "the image is unchanged
  afterwards" as evidence: a byte comparison of a boot disk can differ because of a write no
  minix.rs code made.
- **A mutation booted off the standard path needs a control boot beside it.** 6.4's deleted
  header-CRC check had to be booted on a read-only second drive; there the mutated driver printed
  `root FAIL readonly` — the next gate — and only the unmutated control (`root FAIL gpt crc`) shows
  that the line moved.
- **A mutation at a call site and the same mutation inside the pure helper are two rows.** Skipping
  the flush where `do_write` calls it failed no host test and moved one marker; the same omission
  inside `blk::flush_after` fails a host test. Record where the mutation was applied, and when a
  decision is host-tested but its caller is not, run the call-site form.
- **A guard's mutation moves the marker of the probe that exists for that guard, not a marker on a
  healthy path** — a healthy path never trips it. Slice 5.8's plan predicted that deleting a
  not-a-directory guard would break the filesystem self-check; resolving a real file only ever walks
  real directories, so what actually moved was the deny-probe's FAIL line instead.
- **Mutate a guard only once something exercises it.** 6.2's stub/driver priv-id collision moved no
  marker when its task landed, because no driver ran in the colliding slots yet; with virtio-blk
  live the same mutation hung the default boot with 47 markers missing. A "nothing moves" row
  recorded before the consumer exists is not a result — re-run the mutation when the consumer lands.
- **QEMU is more permissive than the specification, so observe which check fires rather than
  predicting it.** 6.2 predicted that a wrong register page would fail the virtio magic check; an
  empty virtio-mmio slot still reads the magic, and the DeviceID check caught it instead. It also
  predicted that the status read-back would refuse a driver omitting `VIRTIO_F_VERSION_1`; QEMU
  keeps `FEATURES_OK`, and only the exact `features=` marker text and the host tests catch that
  (re-observed in 6.4 with optional features requested: `virtio ok features=0x200`, and the device
  kept working fully).

Two mechanical notes from 5.11: rust-analyzer diagnostics (`dead_code`, unused imports) that appear
mid-run are usually residue of a mutated or half-edited file — trust `cargo clippy … -D warnings` on
the committed tree, not the IDE. And the boot-ratio measurement's `git checkout --detach
<merge-base>` needs a clean tree: commit marker edits first, detach, boot, then check out the branch
again. Stage by explicit path (`git add $(git diff --name-only)`) — a bare `git add -A` sweeps in
unrelated local changes.

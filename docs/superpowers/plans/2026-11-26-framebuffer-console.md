# Slice 6.7 — framebuffer console — Implementation Plan

**Goal:** every byte user space writes to the console reaches a screen as well as the serial line,
and a QMP `screendump` of the running guest proves it.

**Spec:**
[`../specs/2026-11-26-framebuffer-console-design.md`](../specs/2026-11-26-framebuffer-console-design.md),
decisions `F1…F13`, under the phase decisions `H1…H12` of
[`../../plans/phase-6-virtio.md`](../../plans/phase-6-virtio.md). Where this plan and the spec
disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **Branch `feature/6.7-framebuffer-console`.** Every commit `--signoff`ed and GPG-signed; never
  `--no-gpg-sign`, never `--no-verify`. **No push, no PR, no CI trigger.**
- **SPDX header** first in every new `.rs` file.
- **No ABI value moves** (D8, F4). `GET_FRAMEBUFFER` is additive and not emitted to C: `cargo
  gen-c-headers` output is diffed against `main` and must be byte-identical.
- **No `#[cfg(test)]` under `kernel/src/`.** Kernel arithmetic that needs a test lives in
  `kernel-shared` (F5).
- **Device-memory discipline (H9, F6):** the framebuffer is touched only by one aligned volatile
  `u32` store in `drivers/tty/src/main.rs`. No load, no `copy_nonoverlapping`, no slice over the
  mapping. The lib half has no `unsafe`.
- **Overflow-safe arithmetic** for every value derived from Limine's response or the `GETINFO`
  reply.
- **No userland, server, CDEV-band or `ipc_to` change** (F12). Finding one is needed is a ledger
  entry and goes to the user first.
- **Boots.** `$S` is the session scratch directory. Build first (`MINIXRS_SDK=/nonexistent cargo
  build -p minixrs-kernel --target aarch64-unknown-none --release`), then `MINIXRS_SDK=/nonexistent
  MINIXRS_ROOT=<disk|ramdisk> timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none
  --release < <(tools/feed-console.sh $S/<name>.log) > $S/<name>.log 2>&1`. One boot at a time.
  Iterate and mutation-test stub-free (`--no-default-features`), judging by the specific marker.
  Read `check-boot-log.sh`'s whole output; apply the starved-boot check before believing a missing
  marker.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing).
- **Markdown** wraps at 100; `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.

## Marker text (the kernel's values are read from Task 0's boot, not derived)

```text
[fb] ok w=<w> h=<h> pitch=<p> r=<n> g=<n> b=<n> usable=0 pa=<…>     kernel
[fb] none                                                          kernel, no response; forbidden
[fb] FAIL <condition>                                              kernel, forbidden
[devmap] tty fb va=0x100000000 pages=<n> pa=<…>                    kernel
[diag tty] fb ok w=<w> h=<h> cols=<c> rows=<r>
[diag tty] fb none rc=<rc>                                         forbidden (ramfb is on every boot)
[diag tty] fb FAIL <reason>                                        forbidden
```

## Review Focus

1. **`subtract`** at every edge — hole touching a region's start or end by one byte, hole equal to
   the region, hole outside on each side (`kernel-shared` host tests).
2. **Boot order:** the framebuffer is validated before the allocator is seeded, and nothing
   allocates between (read `main.rs`; mutation 6 is an honest row).
3. **Pages mapped** = `ceil(pitch * height / 4096)`, checked, and within the window (mutation 3).
4. **The two `TTY_PROC_NR` gates agree** — loader arm and `GET_FRAMEBUFFER` (mutation 4).
5. **TTY re-validates the reply** rather than trusting the kernel: the VA is `FB_VA`, `w * 4 <=
   pitch`, `pitch * h` within the window, shifts sane, at least one cell (`tty::reply` tests, over
   `paint::Format::new`'s).
6. **No store outside `w × h`**, including a width or height that is not a multiple of the cell
   (`tty::paint` test with an 13×21 surface).
7. **Deferred wrap** and scroll at the last row (`tty::grid` tests).
8. **`fbcheck` refuses**: one flipped pixel, wrong geometry, a row absent from the log, too few rows
   (`fbcheck` tests), and a row cut short, repeated, out of order or left over from before a scroll
   — and passes a dump with a wrapped row, and a row with a kernel line or a run of tick bytes
   inside it in the log. A screen that has not scrolled is `early`, not a pass, under `--scrolled`.
   Know what it accepts by design: letters lost from a row (up to 120 inside it, any number at a
   line's start or end), a last row cut short, and a frozen screen (spec F10).
9. **Tripwires grown, not left:** `callnr.rs`'s selector asserts; `GETINFO_DENIED` users.
10. **`book/`** read end to end for every chapter touched.

## Tasks

### Task 0: Measure first (F13)

Throwaway kernel edit (framebuffer request, print the response and the memmap entry holding it,
paint a block through the HHDM) and `-device ramfb -qmp …` through `qemu-run.sh`'s pass-through
arguments. Record geometry, format, memmap kind, and whether the block is in a `screendump`. The
kernel edit is discarded, not committed. F13.2 and F13.3 are measured on the real code (Task 6).

### Task 1: `kernel-shared` — the window, the selector, `subtract` (F3, F4, F5)

Files: `kernel-shared/src/{uspace,callnr,lib}.rs`, a new `kernel-shared/src/memrange.rs`. Tests
first: `subtract`'s cases — sixteen tests: the hole below, above and adjacent on each side, covering
the region, at the head, at the tail, in the middle, a byte inside each edge, the empty and inverted
arguments, the extremes of `u64`, and the inside-the-region/outside-the-hole property; the offsets
test and the pairwise selector sweep; the four-window disjointness test. Commit.

### Task 2: kernel — request, validation, carve, map, `GET_FRAMEBUFFER`; the runner (F1–F5)

Files: a new `kernel-shared/src/framebuffer.rs` (the pure `validate`, host-tested, with
`kernel-shared/src/lib.rs`), a new `kernel/src/arch/aarch64/framebuffer.rs` (the accepted record and
the `[fb]` line) with `kernel/src/arch/aarch64/{limine,mod,userland}.rs`, `kernel/src/mm/frame.rs`,
`kernel/src/main.rs`, `kernel/src/system/do_getinfo.rs`, `kernel/src/proc/table.rs` (6.6's debt 14,
the stale boot-module comment), `tools/gen-c-headers/src/callnr_h.rs` (the not-emitted list grown),
and `tools/qemu-run.sh` — all of F1 here: `-device ramfb`, the QMP socket with its path-length
check, `MINIXRS_QMP` and `MINIXRS_DISPLAY`. Proof: `[fb] ok` and `[devmap] tty fb` in a stub-free
boot. Commit.

### Task 3: TTY lib — `font`, `grid`, `paint` (F6–F8)

Files: `drivers/tty/Cargo.toml`, `drivers/tty/src/{lib,font,grid,paint}.rs`,
`drivers/tty/font/{spleen-8x16.psfu,LICENSE,README.md}`, and `kernel/build.rs` (the watch on
`font/`, which is outside the `src` that `build_server` watches). Tests first, as the spec's section
3 lists. Commit.

### Task 4: TTY bin — `console_write`, the reply check and the verdict line (F9)

Files: `drivers/tty/src/main.rs`, a new `drivers/tty/src/reply.rs` (the fourth library module:
decode and accept-or-refuse, tests first) with `drivers/tty/src/lib.rs`, the marker files under
`tests/` (three expected, four forbidden), and the two kernel lines reordered to end in `pa=`
(`kernel/src/arch/aarch64/{framebuffer,userland}.rs`). Proof: `[diag tty] fb ok` in a boot and a
hand `screendump` that shows text. Commit.

### Task 5: `fbcheck` and `screendump.sh` (F10, F11)

Files: `tools/fbcheck/` (new workspace member: `Cargo.toml`, `src/{lib,main,check,ppm,qmp}.rs`), the
root `Cargo.toml` and `Cargo.lock`, `tools/screendump.sh`. `tools/qemu-run.sh` is **not** edited
here — Task 2 did all of F1. Tests first for decode and the refusals. Proof: the check passes on a
fed disk boot. Commit. Two fix rounds followed, each its own commit over
`tools/fbcheck/src/{check,lib,main}.rs` and `tools/screendump.sh`: the first when a real log refused
an honest screen (kernel lines stripped, tick-byte runs tolerated); the second when Task 8's
mutation 7 passed the check (rows anchored into one unbroken stretch, a scrolled screen required,
`--scrolled`, `early`).

### Task 6: CI and the budget (F11, F13.2, F13.3)

Files: `.github/workflows/ci.yml` (the pre-build step, the screen check beside both full boots, the
artifact). Measure the last marker's position against the merge base; adopt changed-cells repaint
first if over budget — it was, and the repaint is its own commit
(`drivers/tty/src/{paint,grid,lib,main}.rs`: `paint::Shown`, `paint_cell_diff`, `paint_diff`).
Commit.

### Task 7: recorded boots (spec §3)

Both full boots fed and screen-checked, the reboot, `diff-backends.sh`, every local gate in
[`ci.md`](../../conventions/ci.md).

### Task 8: mutations (spec §3)

The eight rows, each applied, observed, reverted.

### Task 9: documentation, trackers, the whole-branch sweep (spec §4)

Then the fresh review of `git diff origin/main...HEAD`.

## Ledger

One entry per task: its commit, what was verified, and every ruling made in it.

- **Task 0 (2026-11-26; no commit — the probe was a throwaway kernel edit, discarded, and the
  devices went through `qemu-run.sh`'s pass-through arguments).** One stub-free boot, QEMU 11.0.3,
  Homebrew's edk2, macOS on Apple Silicon. **F13.1 holds:** Limine answers the request with one
  framebuffer, `pa=0x4c7a0000 w=800 h=600 pitch=3200 bpp=32`, memory model 1 (RGB), masks 8 bits at
  shifts `r=16 g=8 b=0`; a 200×100 block stored through the HHDM after `ExitBootServices` is in a
  `stop` / `screendump` / `cont` PPM (800×600, `ff0000` inside the block, `000000` outside). The
  memmap reports the range as kind 7 (`FRAMEBUFFER`), `base=0x4c7a0000 len=0x1d4c00` — exactly
  `pitch * height`, **not a page multiple** — followed at `0x4c974c00` by a kind-1 (reserved) entry.
  No usable region overlaps it, so F5's carve does not run here, as the spec expected. The screen is
  100 × 37 cells with an 8-pixel bottom margin. **Findings:** (a) the last mapped page (469 of 469)
  extends `0x400` bytes past the framebuffer into firmware-reserved memory; F3's `ceil` maps it
  anyway, device, and TTY never stores past `pitch * height`. *If wrong:* map `floor` pages and
  shorten the drawable height by a row. (b) **A QMP socket path over 104 bytes is refused by QEMU**
  on macOS; `target/qmp.sock` under a deep checkout could hit it, so `qemu-run.sh` checks the length
  and says so (Task 2). (c) F13.2 and F13.3 are measured on the real code at Task 6, not with the
  probe.
- **Task 1 (`8301ac9`).** `kernel-shared`: the framebuffer window in `uspace`, `GET_FRAMEBUFFER` and
  its offsets in `callnr`, and the new `memrange::subtract`. Verified by host tests only, as the
  task is pure: `subtract`'s sixteen, the selector's layout test and the pairwise selector sweep,
  the four-window disjointness test. **Rulings:** (a) `subtract` returns two fixed slots, below and
  above, rather than a compacted list, and treats an empty or inverted hole as "remove nothing"
  instead of panicking. *If wrong:* a caller that passes a malformed hole loses the carve silently —
  which is why Task 2's seed asserts the hole first. (b) The six window pairs are asserted pairwise,
  not neighbour to neighbour. *If wrong:* nothing; it is strictly more checking. (c) The selector
  tripwire became a table swept pairwise. *If wrong:* nothing moves on the wire.
- **Task 3 (`3de0058`; landed before Task 2).** TTY's library half: `font`, `grid`, `paint`, the
  vendored Spleen file with its `LICENSE` and `README.md`, and the `kernel/build.rs` watch on
  `font/`. Verified by the library's host tests. **Rulings:** (a) **the upstream 8×16 file is PSF
  version 1, not the PSF2 the spec named**; it is vendored unconverted and `font.rs` validates a
  PSF1 header (spec F7 rewritten, section 7). *If wrong:* a release that ships PSF2 fails the build
  at the `const` check, loudly. (b) The 95 printable glyphs are copied into a table at build time
  rather than sliced out of the file at run time. *If wrong:* a few hundred bytes of image either
  way. (c) `forbid(unsafe_code)` on the library. *If wrong:* delete one line. (d) The font directory
  gets a watch of its own in `kernel/build.rs`, for TTY alone. *If wrong:* a swapped font is not
  re-packed until something else changes.
- **Task 2 (`8244abb`).** The kernel half and the runner: `kernel-shared::framebuffer::validate`,
  `arch/aarch64/framebuffer.rs`, the Limine request, the seed around the hole, the TTY arm, the
  selector's handler, `qemu-run.sh`'s `ramfb`, QMP socket, `MINIXRS_QMP` and `MINIXRS_DISPLAY`; also
  6.6's debt 14 (`proc/table.rs`) and the C-header not-emitted list. Verified: `validate`'s host
  tests, and `[fb] ok` and `[devmap] tty fb` in a stub-free boot. **Rulings:** (a) the decision is a
  pure function in `kernel-shared` with the accepted record in one kernel static read by all three
  consumers, where the spec had a "validated accessor" in `limine.rs`. *If wrong:* move a function;
  the single record is what keeps the carve, the mapping and the reply from disagreeing. (b) The
  `[fb]` line is printed after the allocator seed, because it carries `usable=`. *If wrong:* the
  line moves a few lines up the log. (c) Two refusal tokens exist outside `validate` — `pointer`,
  and `address` when there is no HHDM response. *If wrong:* a token's text. (d) **The runner refuses
  a QMP socket path of 104 bytes or more itself** (Task 0's finding b), on Linux too. *If wrong:* a
  path that Linux would have accepted is refused; set `MINIXRS_QMP`. (e) A stale socket is removed
  just before QEMU starts; anything else at the path is refused. *If wrong:* a boot after a killed
  boot fails to bind. (f) The seed asserts the hole is non-empty and page-aligned. *If wrong:* a
  panic at boot where a silent no-carve would have been.
- **Task 4 (`cff8851`).** TTY's binary: `console_write`, `fb_bring_up`, `Screen::store`, and the
  fourth library module `reply`; the marker files (three expected, four forbidden); the two kernel
  lines reordered. Verified: `reply`'s host tests; `[diag tty] fb ok w=800 h=600 cols=100 rows=37`
  in a boot and a hand `screendump` showing text. **Rulings:** (a) **the banner is written by
  `main`, not `tty_init`**, because the console's state is a `main`-frame local. *If wrong:* a
  static and a second `unsafe` to put it back; the serial order is unchanged either way. (b) **The
  reply check is its own module, `reply`**, with the tokens `va`, `window` and `grid` beside
  `Format`'s five. *If wrong:* fold it into `paint`. (c) **`pa=` is last on both kernel lines**, so
  the stable fields are asserted as one prefix. *If wrong:* two `writeln!`s and two marker lines.
  (d) `[fb] none` and `fb none` are forbidden as well as the two `FAIL`s, since `ramfb` is on every
  boot. *If wrong:* a hand-run boot without the device fails the checker — which it does, by design.
  (e) `Screen::store` re-checks its bound on every store, in pixels. *If wrong:* a compare per pixel
  store. (f) TTY refuses a screen smaller than one cell (`grid`) that the kernel accepts. *If
  wrong:* such a machine boots serial-only with a forbidden `fb FAIL grid` line; the kernel's
  validator would have to refuse it too.
- **Task 5 (`f61718c`, then fix rounds `2e96e21` and `d923ea8`).** `tools/fbcheck` and
  `tools/screendump.sh`. Verified by `fbcheck`'s host tests and by the check passing beside a fed
  disk boot. The first cut looked each row up in the raw log allowing two stray single bytes; a real
  log (54 MB, kernel lines mid-console-line, tick bytes in runs) refused an honest screen, and
  `2e96e21` stripped kernel lines and allowed 32 tick bytes a row. Task 8's mutation 7 then passed
  that check, and `d923ea8` rebuilt it: the screen as one unbroken stretch of the console stream
  after the last `fb ok` line, a scrolled screen required, `MAX_STRAY` 120, the `fb ok` line read
  exactly. **Rulings:** (a) **tick bytes are forgiven inside a row, so a row that lost letters
  passes** — up to 120 inside it and any number at a line's start or end. *If wrong:* the check
  cannot be both tolerant of the kernel's bytes and strict about letters; the alternative is to stop
  the kernel printing tick bytes. (b) **`MAX_STRAY` is 120, four times the longest run measured
  (30)**, for a slower host. *If wrong:* too low refuses an honest screen on CI; too high forgives
  more lost letters. (c) **A pass requires a scrolled screen**; an unscrolled one is `early`, exit
  2. *If wrong:* a boot whose console never fills 37 rows can never pass — today's fills them 5% of
  the way through. (d) A later stretch on a screen that is not full is refused. *If wrong:* a
  console that legitimately clears would be refused; nothing clears today. (e) The last non-blank
  row is matched as a prefix. *If wrong:* a last row cut short passes (stated). (f) The script looks
  every 10 s, `early` costs no try, a `FAIL` costs one of ten, and the ceiling covers the whole
  script. *If wrong:* three constants; ten tries also forgive an intermittent defect. (g) The gate
  is `minix.rs init: stdin ok`, so the check is for fed boots only and its pauses come after the
  feeder's exchanges. *If wrong:* an earlier gate risks pausing the guest inside the feeder's timed
  gaps. (h) Console text holding `[word]` is falsely refused. *If wrong:* the first program to print
  such text fails the check and the rule has to learn the kernel's real prefixes.
- **Task 6 (`42e3b69`, CI in `297b544`).** The budget and CI. **F13.2:** three 300 s stub-free unfed
  boots ended at `[ipc]` 11.29 M with no painting, 1.79 M with a full repaint on scroll and 7.69 M
  with the changed-cells repaint — a steady-state slowdown against no painting of 6.3× falling to
  1.47×. On the final code the last required marker is at 65.02% of a 600 s local disk log and
  41.53% of the ramdisk one, against 6.5's recorded 63.20% / 41.41% for the previous merge base (not
  re-measured back to back). **No timeout moved.** **F13.3:** `stop`/`screendump`/`cont` beside a
  fed boot disturbed nothing — 197 of 197 on both backends. **Rulings:** (a) **the changed-cells
  repaint replaced the full repaint**, with a 10 240-byte shadow in `main`'s frame (21 376 bytes of
  the 64 KiB stack, from 11 136). *If wrong:* `paint_all` back in `console_write` and the shadow
  dropped, at about a quarter of the throughput. (b) Every paint after the clear must go through the
  two `_diff` functions or the shadow goes stale. *If wrong:* stale glyphs after a scroll — which
  the rebuilt screen check now sees. (c) CI pre-builds `fbcheck` and hands it over by
  `MINIXRS_FBCHECK`. *If wrong:* a cargo beside the boot, charged to its timeout. (d) **CI kills a
  script still running when the boot ends and counts it failed**, deciding by `kill -0` on its pid.
  *If wrong:* a recycled pid could be signalled or a finished script miscounted — a debt in the
  phase file. (e) The dump and verdict are uploaded on every run, not only on failure. *If wrong:* a
  few megabytes of artifact per run. (f) Both boots use the default QMP socket, safe only because
  they run in sequence. *If wrong:* a second concurrent boot takes the socket. (g) The budget
  comparison is against a figure recorded by an earlier slice, not a same-day baseline. *If wrong:*
  the two-point rise (63.20% → 65.02%) is partly host drift; no timeout depends on it.
- **Task 7 (2026-11-26; no commit of its own — boots only).** The three boots as CI runs them, 600 s
  per full boot locally, musl flavour (`MINIXRS_SDK=/nonexistent`), fed, with `tools/screendump.sh`
  beside each full boot, run **twice**. *First run*, at `297b544`, quiet host: disk 197/197, reboot
  7/7, ramdisk 197/197, `diff-backends.sh` clean; the screen check (as it then was) passed on its
  first try on both, `rows=22` — an unscrolled screen, which Task 8 showed proves nothing about a
  scroll. The budget figures recorded under Task 6 (65.02% / 41.53%) are this run's. *Second run*,
  at `d923ea8` (the 2^48 bound and the rebuilt check): disk 197/197, reboot 7/7, ramdisk 197/197,
  `diff-backends.sh` clean; the screen check reported `early` while the screen filled and passed at
  255 s (disk) and 188 s (ramdisk) with `fbcheck: ok 800x600 100x37 rows=36 wraps=12 stray=5
  worst=10 scrolled=yes`. **The second run's host was slow**: its logs are an eighth the size (final
  `[ipc]` 6.6 M against 54 M) and the last required marker sits at 74.39% (disk) and 46.65%
  (ramdisk) of the log. A control pair of 200 s boots straight after says it was the host and not
  the dumper: 2.13 M IPC with no dump, 2.06 M with a `stop` / `screendump` / `cont` every 10 s (3.5%
  less). **Rulings:** (a) **no timeout moves** on either set of figures. *If wrong:* the first CI
  run shows the disk boot's markers or the scrolled screen arriving too close to 1200 s, and the
  three timeouts and `timeout-minutes` rise in one edit. (b) **The merge base was not re-booted back
  to back**; the comparison is with 6.5's recorded 63.20% / 41.41%. *If wrong:* a regression of a
  few points hides in host drift. (c) **The screen check now needs the boot to live until its screen
  scrolls** — 255 s of 600 on a slow host here. *If wrong:* on a much slower runner the script is
  still `early` when `timeout` kills QEMU and CI counts it failed; the remedy is a later kill or a
  gate on fewer rows, not a weaker check.
- **Task 8 (mutations; no commit — each applied to the working tree, booted stub-free and fed for
  180 s with the screen check beside it, and reverted).** Observed: 1, `console_write` skipping the
  framebuffer — the screen check only (`only 0 non-blank rows`); 2, a store at `x + 1` in the bin's
  `Surface` — the screen check only (a cell that is not a glyph), and the same edit in the lib's
  `paint_cell` fails 21 `paint::tests`; 3, the window one page short — `[pf] proc=t far=0x1001d4000
  flags=0x1 -> VM` during `clear`, `fb ok` and the banner missing, the boot stalled near `[ipc
  1700]`, no forbidden marker, and `pages=468` in the honest form; 4, the gate naming another proc —
  `[diag tty] fb none rc=-1`, expected missing, forbidden hit, screen check fails; 5, red and blue
  swapped — nothing moves; 6, the carve skipped — nothing moves; 7, a scroll that repaints nothing —
  **passed the original script**, caught by the rebuilt check on a scrolled screen (`screen row 36
  is not in the serial log's console text…`); 8, `ramfb` dropped — `[fb] none`, `fb none rc=-2`,
  three expected missing, two forbidden, screen check fails cleanly. **Rulings:** (a) **mutation 7's
  pass was treated as a defect in the check, not as an honest row**, and the check was rebuilt (Task
  5, `d923ea8`). *If wrong:* nothing is lost but the stricter rule's new false-failure risks. (b)
  Mutations 5 and 6 are recorded as honest "nothing moves" rows and as debts (colour; a carve no
  boot has run). *If wrong:* a red/blue swap or a missing carve ships unnoticed on a machine where
  it matters. (c) Mutation 3's silent stall is recorded as a debt rather than fixed. *If wrong:* a
  framebuffer mapping fault keeps reading like a starved boot.
- **Task 9 (`104ffe3`, and the fix round that carries this ledger).** The book, the conventions, the
  spec and the plan. Verified: `dprint check` and `tools/check-md-links.py` clean; every statement
  taken from the code at the commit it describes. **Rulings:** (a) the kernel's rules for the
  framebuffer got a section of their own in `kernel.md`, which the spec's section 4 had not listed.
  *If wrong:* a section to fold into "Device memory". (b) The screen check's limits live in
  `testing-and-markers.md` and its mechanism in the book's build chapter. *If wrong:* the two drift;
  both point at `check.rs`'s module docs as the rule. (c) The definition of done in
  `docs-and-workflow.md` was **not** changed to name the screen check. *If wrong:* a slice can call
  itself done locally without running it; CI still blocks. (d) Stale comments in `.rs`, `.yml` and
  `tests/` found by the sweep were reported, not edited.
- **Review round (`6d1cb26`, with `297b544`'s comment corrections).** The whole-branch review found
  that a framebuffer whose page-rounded end passes 2^48 would have been mapped at another address —
  the mapper masks a physical address to 48 bits — with no assert firing; `validate` now refuses it
  as `address` (`PA_LIMIT`), with a host test at the limit and one page past it. It also found the
  comments a later task had falsified (where TTY's `unsafe` lives, where the grid is kept, PSF1, who
  compares the framebuffer with its window, `kernel-shared`'s carve-out), three documentation errors
  (each of the three marker lines "ends in `pa=`"; "a mutation proves the wiring"; "`fb FAIL` means
  the two validators disagree"), and that the framebuffer's range is not checked against the memory
  map's non-usable kinds. **Rulings:** (a) the 2^48 refusal reuses the `address` token. *If wrong:*
  one more token and one marker comment. (b) The range-versus-memmap-kinds check was **not** added;
  it is a debt in the phase file. *If wrong:* a firmware that reports a framebuffer overlapping the
  kernel image or a module would have it mapped into TTY as device memory.

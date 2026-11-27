# Slice 6.7 — framebuffer console: Limine framebuffer → TTY

**Status:** approved 2026-11-26. **Phase file:**
[`phase-6-virtio.md`](../../plans/phase-6-virtio.md#slice-67-framebuffer-console). Decisions here
are labelled `F1…F13`; the phase's `H1…H12` stay in force and are cited by their own labels.

**Goal:** every byte user space writes to the console reaches a screen as well as the serial line.
The kernel asks Limine for a framebuffer, maps it into TTY, and TTY draws text on it with a
host-tested renderer; a QMP `screendump` of the running guest proves the pixels arrived.

H9 locks this much: the framebuffer comes from a Limine request against `-device ramfb`; the kernel
pre-maps it into TTY in its own window, always as a **device** mapping, removing it from the frame
allocator first if it overlaps usable RAM; base, width, height, pitch and pixel format travel by
`SYS_GETINFO`; TTY writes each console byte to the UART **and** the framebuffer; the renderer is a
host-tested library half of `drivers/tty`; the font is Spleen or another BSD/MIT bitmap font,
vendored with attribution; kernel and `[diag]` output stays serial-only. Everything else below is
this slice's own design.

---

## 1. What exists, and what 6.7 changes

Verified against `main` at `b05f2e8`.

| Where                                    | Today                                                                                  | After 6.7                                                                                                                                  |
| ---------------------------------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `kernel/src/arch/aarch64/limine.rs`      | base revision, HHDM, memmap, paging mode, stack size, kernel address                   | plus a framebuffer request and `framebuffer()`, which reads the first one and hands it to the validator (F2)                               |
| `kernel-shared::framebuffer`             | —                                                                                      | new: the pure, host-tested `validate` and the accepted `Framebuffer` record with its page-rounded `range()` (F2)                           |
| `kernel/src/arch/aarch64/framebuffer.rs` | —                                                                                      | new: the one static holding the accepted framebuffer (`init`, `accepted`) and the `[fb]` line (`report`)                                   |
| `kernel-shared::uspace`                  | three kernel-owned windows: device (1 GiB), ramdisk (2 GiB), DMA (3 GiB)               | a fourth, the framebuffer window at 4 GiB (F3)                                                                                             |
| `kernel-shared::callnr`                  | `GET_RAMDISK` 64, `GET_VIRTIO_MMIO` 65, `GET_DMA` 66                                   | `GET_FRAMEBUFFER` 67, gated to TTY (F4)                                                                                                    |
| `kernel-shared::memrange`                | no range arithmetic anywhere                                                           | new: a pure, host-tested `subtract(region, hole)` the allocator uses (F5)                                                                  |
| `kernel/src/mm/frame.rs`                 | regions seeded from every `MEMMAP_USABLE` entry                                        | `init_from_limine_memmap(hole)`: the same, minus any accepted framebuffer's range; returns the pages carved (F5)                           |
| `kernel/src/main.rs`                     | seeds the allocator straight after `proc::init`                                        | reads and validates the framebuffer first, seeds around it, then prints the `[fb]` line (F5)                                               |
| `userland.rs::load_boot_server`          | TTY gets one device page, the PL011                                                    | plus the framebuffer's pages, `Prot::DEVICE_RW`, at `FB_VA` (F3)                                                                           |
| `kernel/src/system/do_getinfo.rs`        | four selectors                                                                         | a fifth, `fill_framebuffer` (F4)                                                                                                           |
| `drivers/tty`                            | a bin only (`main`, `input`, `irq`, `pl011`); `pl011::write` is the one console output | a lib half (`font`, `grid`, `paint`, `reply`) and one `console_write` in `main.rs` that feeds both sinks (F6–F9)                           |
| `kernel/build.rs`                        | watches each boot crate's `src`                                                        | also watches `drivers/tty/font`, which the renderer embeds from outside `src` (F7)                                                         |
| `tools/qemu-run.sh`                      | `-display none`, no QMP, no display device                                             | `-device ramfb` and a QMP socket always (`MINIXRS_QMP` moves it); `MINIXRS_DISPLAY=window` drops `-display none` (F1)                      |
| `tools/`                                 | no screen check                                                                        | `tools/fbcheck` (host Rust: `check`, `ppm`, `qmp`) and `tools/screendump.sh` (F10–F11)                                                     |
| `tools/gen-c-headers`                    | a not-emitted test naming the two virtio selectors and their offsets                   | the same test also names `GET_FRAMEBUFFER` and its eight offsets (F4)                                                                      |
| CI `qemu-smoke`                          | three boots, serial markers only                                                       | `fbcheck` pre-built; both full boots also run the screen check, which blocks; each dump and verdict is in the artifact, pass or fail (F11) |

`userland/`, `servers/`, the CDEV band and every existing serial marker are untouched.

---

## 2. Decisions

### F1 — the machine: `ramfb` on every boot, QMP on every boot, a window on request

`qemu-run.sh` adds `-device ramfb` and `-qmp unix:<sock>,server=on,wait=off` unconditionally, so CI
and a window boot run one code path (H9). The socket is `${MINIXRS_QMP:-$ROOT/target/qmp.sock}` —
the `MINIXRS_HVC0` pattern, so two boots at once can be kept apart. **The socket's path must be
shorter than 104 bytes**, and the runner refuses a longer one itself (exit 64) instead of leaving it
to QEMU: that is all a unix socket's address holds on macOS, the default lives under the checkout,
and QEMU's message does not name the variable to change. The limit is applied on Linux too. A stale
socket from a killed boot is removed just before QEMU starts; anything else at the path is refused
(exit 73). `MINIXRS_DISPLAY=window` omits `-display none` and lets QEMU pick its default UI; any
other value but `none` (the default) is refused. No resolution is requested from Limine: the kernel
takes the mode the firmware set (F13 measures which).

*Rejected:* `ramfb` only on a windowed boot (CI would then test a TTY with no framebuffer, the path
nobody looks at); `-vnc`/`-spice` (a second way to get the same picture `screendump` already gives).

### F2 — the kernel accepts one pixel format, and says so when it gets another

The request is Limine's framebuffer request at revision 0; the kernel reads the **first**
framebuffer only. It is accepted when all of these hold, and otherwise the kernel prints one `[fb]`
line naming the failed condition and carries on with no framebuffer:

- `bpp == 32`, `memory_model == RGB`, each of the three mask sizes is 8, and each shift is one of 0,
  8, 16 or 24, pairwise distinct;
- `width`, `height` non-zero; `pitch >= width * 4` and `pitch % 4 == 0`;
- `address` is an HHDM address and its physical address is page-aligned;
- `pitch * height`, rounded up to pages, fits `FB_WINDOW_SIZE` (all products `checked_`);
- the page-rounded physical end, `pa + pages * 4096`, does not pass `PA_LIMIT`, 2^48. A page
  descriptor holds 48 bits of physical address and the kernel's mapper masks the rest away, so a
  page above that would be mapped at another address with no assert firing. Refused as `address`.

The decision is one pure function, `kernel-shared::framebuffer::validate`, host-tested because
`kernel/src` cannot be; `limine.rs` only copies the fields out. The accepted line is `[fb] ok w=…
h=… pitch=… r=… g=… b=… usable=<pages> pa=…` where `usable` counts the pages F5 had to carve
(expected 0) and the firmware-dependent `pa=` comes last, so that the stable fields before it can be
asserted as one prefix. A missing response, or one listing no framebuffer, prints `[fb] none`. A
refusal prints `[fb] FAIL <token>`, the token being the first condition that failed, in the order
tested: `bpp`, `model`, `mask-size`, `shift`, `size`, `pitch`, `address`, `align`, `window` — plus
`pointer` for a null array or first entry in a non-empty response, and `address` when there is no
HHDM response to judge an address by. The line is printed after the allocator has been seeded (F5),
since it reports what the seed carved.

*Rejected:* supporting 24 bpp or 16 bpp (unaligned or merged stores, which H9 forbids on a device
mapping, or a second paint path with no machine to test it on).

### F3 — a fourth window, 4 GiB, 16 MiB

`FB_WINDOW_BASE = 0x1_0000_0000`, `FB_WINDOW_SIZE = 0x0100_0000`, `FB_VA = FB_WINDOW_BASE`: one L1
slot above the DMA window, so `USER_REGION_LIMIT` does not move and VM needs no edit (the property
`uspace` already documents). 16 MiB holds 1920×1080×32 (8.3 MiB). `load_boot_server`'s TTY arm maps
`ceil(pitch * height / 4096)` pages `Prot::DEVICE_RW` and prints one `[devmap] tty fb va=… pages=…
pa=…` line (`pa=` last, as in F2). Compile-time asserts pin the window against the other three and
against `USER_VA_TOP`.

### F4 — `GET_FRAMEBUFFER` (67): one reply, gated to TTY

| offset | type | meaning                 |
| ------ | ---- | ----------------------- |
| 0..8   | u64  | `FB_VA`                 |
| 8..12  | u32  | width in pixels         |
| 12..16 | u32  | height in pixels        |
| 16..20 | u32  | pitch in bytes          |
| 20..21 | u8   | red shift (bit offset)  |
| 21..22 | u8   | green shift             |
| 22..23 | u8   | blue shift              |
| 23..24 | u8   | bits per pixel (32, F2) |

`EPERM` for any caller but `TTY_PROC_NR` — the same two-places-must-agree rule as `GET_RAMDISK` —
and `ENOENT` when no framebuffer was accepted, which is how TTY learns it is serial-only. Like its
three neighbours the selector is not emitted into the C headers. **D8:** additive (H12); no layout,
call number, endpoint or errno changes. `callnr.rs`'s selector tripwires (the pairwise `!=` asserts
and the offset-disjointness test) grow by this selector in the task that adds it.

### F5 — the overlap rule is a pure function in `kernel-shared`

H9 requires removing a framebuffer that overlaps a usable region from the allocator. The arithmetic
— one region minus one hole giving zero, one or two regions — goes in `kernel-shared` as a pure
function with host tests, because `kernel/src` has none. `init_from_limine_memmap` applies it to
each usable entry before any frame is handed out, so no allocated frame can lie in the range; a
split that would exceed `MAX_REGIONS` panics with the existing message. The kernel therefore reads
and validates the framebuffer response **before** it seeds the allocator.

On QEMU the range is expected to be reported as `LIMINE_MEMMAP_FRAMEBUFFER`, not usable, so the
carving arm does not run at boot. That is stated as an honest "nothing moves" row (section 3,
mutation 6): the host tests prove the arithmetic, and **nothing proves the wiring** — with the carve
skipped altogether no marker moved, `usable=0` either way. The call in `init_from_limine_memmap` has
never been executed by a boot with something to carve; it is held by reading.

*Rejected:* refusing an overlapping framebuffer (simpler, and contradicts H9); carving after
allocation has begun (frames already handed out could be the screen).

### F6 — TTY grows a library half, with no feature gate

`drivers/tty/src/lib.rs` holds four pure modules under `#![forbid(unsafe_code)]`; `main.rs` stays
the bin and is the only file that touches a device.

- `font` — glyph lookup over the vendored font (F7).
- `grid` — the text model: a fixed `MAX_COLS × MAX_ROWS` (160 × 64) byte array, a cursor, and `put`.
  A screen larger than that is used up to that many cells from the top-left; the rest stays
  background.
- `paint` — turns grid cells into pixel stores through a `Surface` trait whose one write is
  `store(x, y, pixel: u32)`. The bin implements it with an aligned volatile `u32` store at `FB_VA +
  y * pitch + x * 4`; the host implements it over a `Vec<u32>`. It also holds `Format`, the
  validated description of a framebuffer, and `decode_cell`, the inverse of painting a cell, for
  F10.
- `reply` — the `GET_FRAMEBUFFER` payload decoded (`Reply::decode`) and the accept-or-refuse
  decision (`accept`), F9's re-validation.

Unlike `fs/mfs` there is no `server` feature: nothing reaches the TTY lib from a build script, and
the crate's dependencies already build on the host for `cargo test`.

**Device-memory discipline (H9):** every framebuffer access is one aligned 32-bit volatile store.
The renderer never reads the framebuffer — scrolling repaints from the grid, and what is already on
the screen is known from a shadow array, not from the pixels (F8) — so there is no load, no
`copy_nonoverlapping` and no `memset` the compiler could merge or misalign.

### F7 — the font: Spleen 8×16, vendored as the upstream PSF file

`drivers/tty/font/spleen-8x16.psfu` is the upstream release file, byte for byte, embedded with
`include_bytes!`; beside it sit the upstream `LICENSE` (BSD-2-Clause) and a `README.md` recording
the upstream URL, release tag and the file's SHA-256. In release 2.3.0 the 8×16 face is **PSF
version 1** (magic `36 04`, 512 glyphs with a Unicode table) — the other faces are version 2 — so
`font` validates the PSF1 header in a `const` context (magic, a 16-byte glyph, the Unicode-table
bit, the glyph data all present) and a wrong file fails the build. A host test walks the PSF1
Unicode table and pins that glyph `n` is U+00`n` for `0x20..=0x7E`, which is what lets the renderer
index by byte.

Bytes outside `0x20..=0x7E` that are not controls (F8) draw glyph `0x3F` (`?`). The repository's
licence stays BSD-3-Clause; BSD-2-Clause is compatible and the note says so.

*Rejected:* a generated Rust table (a generator to maintain and a second copy to drift); the BDF
source (a parser in a build script for no gain over the binary the same release ships).

### F8 — what a byte does

| byte          | effect                                                           |
| ------------- | ---------------------------------------------------------------- |
| `0x20..=0x7E` | draw at the cursor, advance; at the last column, wrap first      |
| `\n`          | column 0 of the next row; past the last row, scroll one row      |
| `\r`          | column 0                                                         |
| `\t`          | advance to the next multiple of 8, clamped to the last column    |
| `\b`          | back one column if not at column 0; erases nothing               |
| other         | `?` (F7) — including `ESC`: there is no escape-sequence terminal |

Wrap is deferred: writing in the last column leaves the cursor pending, and the wrap happens when
the next printable byte arrives, so a line of exactly `cols` bytes followed by `\n` takes one row,
not two. Light grey (`0xAAAAAA`) on black; no cursor is drawn, because nothing echoes input until
6.8.

A scroll shifts the grid and repaints **only the cells whose byte changed**. `paint` keeps a shadow,
`Shown` — the byte last painted in each cell, a second `MAX_COLS × MAX_ROWS` array beside the grid —
and `paint_diff` stores a cell only where the grid disagrees with it; a drawn byte goes through the
same comparison (`paint_cell_diff`), so rewriting a cell with the byte it already shows costs
nothing. The framebuffer is still never read. This was the named fallback and F13's second
measurement adopted it: repainting every cell on every scroll cut console throughput about sixfold.

### F9 — one `console_write`, UART first

TTY's two output sites — the banner and `do_write` — call one `console_write(console, bytes)`, which
writes the UART and then, when a framebuffer was accepted, the grid and the screen. UART first so
that no serial marker's timing depends on painting. The console's state — the screen, the grid and
the shadow — is a local of `main`'s frame, so **the banner is written by `main`**, straight after
`sef_startup` returns, and not by the `tty_init` callback, which cannot reach that frame without a
static; `tty_init` only publishes to DS. Nothing runs between the two, so the serial log's order is
unchanged. At start-up TTY asks `GET_FRAMEBUFFER` once, before the banner, and prints exactly one
of:

- `[diag tty] fb ok w=… h=… cols=… rows=…` — after clearing the screen to background, so the line
  also says a full screen of stores did not fault;
- `[diag tty] fb none rc=…` (`ENOENT`: no framebuffer; anything else is a defect);
- `[diag tty] fb FAIL <reason>` — a reply TTY will not draw on, after which it is serial-only. The
  decision is `reply::accept`, which does not trust the kernel: the VA must be `FB_VA` (`va`), F2's
  conditions are re-checked by `paint::Format::new` (`bpp`, `shift`, `shifts-overlap`, `zero-size`,
  `pitch`), `pitch * height` must fit the window (`window`), and at least one cell must fit
  (`grid`).

Three lines join the common expected-marker file — `[fb] ok w=800 h=600 pitch=3200 r=16 g=8 b=0
usable=0`, `[devmap] tty fb va=0x100000000 pages=469` and `[diag tty] fb ok w=800 h=600 cols=100
rows=37` — and four the forbidden one: `[fb] none`, `[fb] FAIL`, `[diag tty] fb none` and `[diag
tty] fb FAIL`. `ramfb` is on every boot, so a boot with no screen is a failure whichever side says
so. The reboot boot's seven-line set is unchanged.

### F10 — the proof: decode the screen, and place it in the serial log

The screen is never quiet — init's rotation prints for ever — so there is no fixed image to compare
against. `tools/fbcheck` (a host crate depending on the TTY lib) instead checks a dump against the
renderer and against the boot's own serial log:

1. **Geometry.** The dump's size equals the `w=`/`h=` of the log's last `[diag tty] fb ok` line, and
   the grid the library derives from that size equals its `cols=`/`rows=`. The line must be complete
   and is read exactly, with no allowance for stray bytes: the kernel writes a `[diag …]` line
   whole, inside one kernel call.
2. **Every cell is a glyph.** Each 8×16 cell inside the grid decodes, pixel-exactly, to one byte
   drawn by `paint` in the two console colours; every pixel outside the grid is background.
   Re-rendering the decoded grid with the same library reproduces the dump — the phase file's
   "matches an image the host renders from the same text".
3. **The text is the boot's, and it is where the boot put it.** At least `MIN_ROWS` (8) rows are
   non-blank, and **the screen, read top to bottom, is one unbroken stretch of the console stream**.

   The *console stream* is the log from that `fb ok` line on — nothing before it was drawn by this
   TTY — with every kernel line taken out: from a `[` followed by one or more of `a`–`z`, `_`, `.`,
   `-` and then a space or `]`, through the next `\n`. (The log has three writers — measured, 54 MB
   of log for some 60 KB of console — and a kernel line, though written whole, can begin in the
   middle of a console line.) What is left still holds the kernel's per-tick bytes, the *strays*:
   ASCII letters or `?`.

   Walking the rows from the top to the last non-blank one, the stream must hold, in order and with
   nothing else between: the row's text (trailing blanks removed), byte for byte, with strays
   allowed between two of its bytes, at most `MAX_STRAY` (120) per row; then up to the row's width
   in blanks; then a line end — or, only if the row reached the last column, nothing, the line
   having wrapped into the next row. A full row may be followed by either, since a line of exactly
   `cols` bytes followed by a newline takes one row (F8). Strays *between* those pieces are not
   counted and may come in any number, except after a trailing blank. The last non-blank row needs
   only its text to be there: it may be the line still being written.

   So every row is anchored to the one above it. **Row order, a repeated row, a row cut short, a row
   shifted left and a row left over from before a scroll are all refused**, each with its own reason
   (`… not in the serial log's console text`, `… but not where the screen has it`, `… stops where
   the serial log's console text goes on`).

   **Scrolled, or not yet.** A screen whose top row begins at the very start of the console stream
   has not scrolled. One that begins later has — and must then be full, its last non-blank row the
   last row or the one above it; a later stretch on a screen that is not full is refused (a scroll
   that cleared instead of moving). The not-scrolled reading is tried first. Only a scrolled screen
   has exercised the scroll path, so `--scrolled` asks for one, and a screen that is right but has
   not scrolled is then `early`: neither a pass nor a failure.

   What step 3 knowingly does not see: **lost letters** — up to 120 inside a row (`minix.rs it:
   stdin ok` for `minix.rs init: stdin ok`), and any number at the very start or very end of a line,
   so a line of nothing but letters can be shown as a blank row; **the end of the last non-blank
   row**, matched as a prefix; **the head of a scrolled screen's top row**, and a wrapped line that
   begins there and lost characters anywhere along it; **a frozen screen**, which is still an
   unbroken stretch; and, as false failures rather than false passes, console text that looks like a
   kernel line (`[word]` or `[word `) and bytes the screen draws differently (a tab, an unprintable
   byte, a bare `\r`).

One line says what happened, and the exit status with it: `fbcheck: ok <fields> scrolled=yes|no` on
stdout, exit 0; `fbcheck: early <fields> (not scrolled yet)` on stdout, exit 2, with `--scrolled`
only; `fbcheck: FAIL <reason>` on stderr, exit 1; usage, 64. `<fields>` is `<W>x<H> <C>x<R> rows=<N>
wraps=<N> stray=<N> worst=<N>` — the size in pixels and in cells, the non-blank rows, the rows whose
line wrapped into the next, the rows that needed strays allowed, and the most one row needed.

Step 3 is what makes the check more than the renderer agreeing with itself: the serial log is
written by `pl011`, not by the renderer. One hand-drawn golden glyph in the host tests covers the
remaining blind spot, a font read wrongly on both sides.

*Rejected:* a fixed "test card" image (nothing in the default boot is ever the last writer);
reconstructing the console stream from the serial log and rendering it (kernel lines and tick bytes
interleave with it unpredictably, which
[`testing-and-markers.md`](../../conventions/testing-and-markers.md) already records).

### F11 — taking the dump: stop, dump, continue, until the screen has scrolled

`tools/screendump.sh <qemu-log> <out.ppm>` works in three stages. It first makes sure it has the
checker: it builds `minixrs-fbcheck` once (`cargo build --release`) and from then on runs the binary
directly, or takes one already built from `MINIXRS_FBCHECK` and builds nothing. It then polls the
log for `minix.rs init: stdin ok` (the last event the feeder drives, so that the pauses below cannot
disturb the feeder's timed exchanges), which must be **seen on two consecutive polls**, because the
log path is usually one an earlier boot wrote and the script starts an instant before the boot
truncates it. Then it **looks every `SCREENDUMP_GAP` (10) seconds** with `fbcheck dump-and-check
--scrolled`: over QMP it sends `stop`, `screendump` (PPM), `cont` — `cont` always, whatever the dump
did — and then reads the log and applies F10. Every look therefore pauses the guest. The first `ok …
scrolled=yes` wins. An `early` verdict is not a pass and **does not use up a try**: the script looks
again. A `FAIL` uses up one of `SCREENDUMP_TRIES` (10) — a dump caught with a glyph half-painted or
a scroll half-repainted is simply looked at again — and when they are gone the script exits 1.
`SCREENDUMP_CEILING` (1300) seconds after it started the script exits 1 **whichever stage it is
in**: because the gate line never came, or because the screen never scrolled. The last dump is kept
either way. `fbcheck`'s QMP client is a few lines over `UnixStream`, each step bounded at 10 s; no
new host dependency.

**Why a scrolled screen.** In the default boot the gate line appears with 22 of the screen's 37 rows
written. As first built the script ran up to ten tries three seconds apart and the first passing
dump won — so no dump it judged had ever scrolled, and a TTY whose scroll repainted nothing passed
(section 3, mutation 7). A scrolled screen is not long in coming: the 38th console row is written
about 5% of the way through a default disk boot's log.

CI builds `fbcheck` in a step of its own and hands it over through `MINIXRS_FBCHECK`, so that no
cargo runs beside a boot. It starts the script as a background job beside each of the two full boots
and settles it when the boot's `timeout` has fired: a script that has exited leaves its status in
`fb-<boot>.rc`, and **one still running is killed and counted as failed** — the script cannot see
QEMU die, and one still looking never saw its gate line, never saw the screen scroll, or never got a
scrolled dump to pass. The check step fails the job on a non-zero status. `fb-<boot>.ppm` and
`fb-<boot>.verdict` are uploaded with the logs on every run, pass or fail. The reboot boot is not
checked.

### F12 — what does not change

No request band, no new process, no `ipc_to` edge, no IRQ. `CDEV_WRITE`'s contract and reply are
unchanged, so VFS and musl are not touched. The framebuffer is never unmapped: TTY is a boot driver
and its teardown is unreachable, as for the UART page.

### F13 — measure first

Task 0, before any design-dependent code, on this machine (QEMU 11.0.3, Homebrew edk2):

1. **Unverified fact 3.** With `-device ramfb` and a throwaway framebuffer request, does Limine
   answer, with what geometry, format and memmap kind, and does a store through the HHDM show up in
   a `screendump` taken after `ExitBootServices`? If it does not, the slice stops and comes back to
   H9.
2. **What a repaint costs.** The last required marker's position in a fixed-timeout log, against the
   merge base, with full repaint on scroll. The slice moves no timeout without that figure; over
   budget, F8's changed-cells-only repaint is tried before any timeout is. **Measured, and the
   changed-cells repaint was adopted** (section 7 has the figures): a full repaint is 3700 cells,
   473,600 device stores, on every newline once the screen is full. **The budget, measured on the
   final code:** the last required marker sits at 65.02% of a 600 s local disk log and 41.53% of the
   ramdisk one, against the 63.20% / 41.41% slice 6.5 recorded for the previous merge base. The two
   were not re-measured back to back, so this is a comparison with a recorded figure, not a same-day
   baseline. No timeout moved.
3. **Whether `stop`/`screendump`/`cont` disturbs the boot** — the markers and the feeder's exchange
   still pass with the dumper running. **Measured: nothing was disturbed** — beside a fed boot with
   the dumper looking, both backends passed 197 of 197 marker checks.

CI's QEMU and firmware (`qemu-efi-aarch64`) are unmeasured until the branch's first run, as in 6.4
and 6.6.

---

## 3. Verification

**Host tests** (each named in the plan with its task):

- `kernel-shared::memrange` — `subtract`: a hole below, above and adjacent on each side; covering
  the region exactly and more than exactly; at the head, at the tail, in the middle (two ranges);
  one byte inside each edge; an empty hole, an empty region, an inverted argument; the extremes of
  `u64`; and that every result lies inside the region and outside the hole.
- `kernel-shared::framebuffer` — `validate`: the measured framebuffer accepted; each of F2's
  conditions refused under its own token, the first failing one reported; every distinct byte order
  of the shifts accepted; the window filled exactly, and one row and four bytes past it; each
  overflow; `range()` rounded outward to pages.
- `kernel-shared::callnr` — `GET_FRAMEBUFFER`'s offsets ordered, disjoint, aligned and unpadded; all
  five selectors pairwise distinct.
- `kernel-shared::uspace` — the four windows pairwise disjoint and below `USER_VA_TOP`.
- `tty::font` — header accepted and wrong files refused; the ASCII identity through the Unicode
  table; one golden glyph by hand; the printable glyphs pairwise distinct.
- `tty::grid` — each row of F8's table; deferred wrap (`cols` bytes then `\n` is one row); scroll;
  clamping to `MAX_COLS × MAX_ROWS`; a zero-sized screen draws nothing.
- `tty::paint` — `Format::new`'s refusals; a cell's 128 stores land at the right coordinates with
  the right shifts; nothing is stored outside `w × h` (a 13×21 surface); `decode_cell` inverts
  `paint_cell`; painting by diff through `Shown` is pixel-identical to painting from scratch, for
  generated byte streams on several screen sizes.
- `tty::reply` — the payload decoded at `callnr`'s offsets; each refusal (`va`, the five format
  tokens, `window`, `grid`) and the measured reply accepted.
- `fbcheck` — a rendered image decodes back to its text; one flipped pixel, a wrong size, a row
  absent from the log, and too few rows are each refused; kernel lines stripped and tick-byte runs
  tolerated up to the limit and no further; a row cut short, a repeated row, rows out of order and
  rows left over from before a scroll each refused; a full-width line and a wrapped one told apart;
  a screen that begins with the console's first line is not scrolled, one whose first line has
  scrolled away is, one that reads both ways is reported as not scrolled, and a later stretch on a
  screen that is not full is refused; a row that lost letters still passes (the stated cost); the
  QMP exchange against a mock server, `cont` always sent.

**Boot markers:** `[fb] ok`, `[devmap] tty fb`, `[diag tty] fb ok` in the common expected file;
`[fb] none`, `[fb] FAIL`, `[diag tty] fb none` and `[diag tty] fb FAIL` forbidden (F9 has the full
text of each). Both full boots, the reboot and `diff-backends.sh` as the definition of done
requires.

**The screen check** (F10–F11) passes on both full boots, on a screen that has scrolled.

**Mutations.** All eight were applied, booted stub-free and fed for 180 s with the screen check
beside the boot, and reverted. The column is what was **observed**, not what was predicted.

| # | Mutation                                              | Observed                                                                                                                                                                                                                                              |
| - | ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1 | `console_write` skips the framebuffer                 | **the screen check only**: `only 0 non-blank rows`. No serial marker moves                                                                                                                                                                            |
| 2 | the store lands at `x + 1`                            | in the bin's `Surface`: **the screen check only** (a cell that is not a glyph). The same edit in the lib's `paint_cell` fails 21 `paint::tests`                                                                                                       |
| 3 | the kernel maps the window one page short             | `[pf] proc=t far=0x1001d4000 flags=0x1 -> VM` during TTY's `clear`; `[diag tty] fb ok` and the banner missing; **the whole boot stalls near `[ipc 1700]` and no forbidden marker fires**. The honest form of the edit also moves `pages=469` to `468` |
| 4 | `GET_FRAMEBUFFER`'s gate names another proc           | `[diag tty] fb none rc=-1`: an expected marker missing, a forbidden one hit, and the screen check fails                                                                                                                                               |
| 5 | red and blue shifts swapped in the reply              | **nothing moves** — grey is channel-symmetric                                                                                                                                                                                                         |
| 6 | the loader treats the range as usable (carve skipped) | **nothing moves** on QEMU — `usable=0` either way (F5)                                                                                                                                                                                                |
| 7 | a scroll shifts the grid and repaints nothing         | caught by the screen check **on a scrolled screen**: `screen row 36 is not in the serial log's console text…`. With the script as first built it **passed**                                                                                           |
| 8 | `-device ramfb` dropped                               | `[fb] none` and `[diag tty] fb none rc=-2`: three expected markers missing, two forbidden hit, and the screen check fails cleanly                                                                                                                     |

Four of the rows say something the predictions did not:

- **Mutations 1 and 2 are caught by the screen check and by nothing else.** Without it a TTY that
  draws nothing, or draws every glyph one pixel to the right, passes all 197 marker checks.
- **Mutation 3 looks like a starved boot.** TTY faults inside its start-up clear, VM has nothing to
  map at a framebuffer address, and TTY never returns: everything after stalls and the missing
  markers are contiguous to the end of the log, with a tiny IPC count. No forbidden marker fires.
  The `[pf] proc=t far=0x1001…` line — a fault address inside the framebuffer window — is the
  discriminator, and
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing) now says to
  look for it.
- **Mutations 5 and 6 are honest "nothing moves" rows, and debts.** 5 has no proof while the console
  draws grey, and is owned by whoever adds colour. 6 means the carve's *wiring* has never been
  executed by a boot: it is held by the host tests of the arithmetic and by reading (F5).
- **Mutation 7 is why the screen check was rebuilt.** In the default boot the script's gate line
  appears with 22 of 37 rows drawn; the first passing try won; so CI never judged a scrolled screen,
  and a scroll that painted nothing passed. A pass now requires `scrolled=yes`, and each row is
  anchored to the one above it (F10, F11).

---

## 4. Documentation owed in the same PR

- `book/src/drivers/overview.md` — TTY's two sinks, the lib/bin split, the renderer and font;
  `book/src/boot/overview.md` — the framebuffer request and its validation;
  `book/src/memory/overview.md` — the fourth window and the carve rule;
  `book/src/reference/syscalls.md` — `GET_FRAMEBUFFER`; `book/src/build.md` — the window mode.
- `docs/conventions/` — `build-and-boot.md` (ramfb, QMP, `MINIXRS_DISPLAY`, `MINIXRS_QMP`),
  `testing-and-markers.md` (the screen check and its retry), `ci.md` (the extra step and artifact),
  `servers-and-drivers.md` (TTY's device-store rule), `kernel.md` (the boot order, the carve, the
  window), `abi.md` (selector 67).
- `docs/plan.md` and `phase-6-virtio.md` — the box, unverified fact 3's outcome, the debts.
- `uspace.rs` and `limine.rs` module docs; `proc/table.rs`'s stale "only `vblk` has a boot module"
  line (6.6's debt 14, owned by the next slice to touch the kernel — this one).

---

## 5. Rulings to check

Decisions the phase file did not make. Each is undone by the change in its last column.

| #   | Ruling                                                                                                   | If wrong                                                                                         |
| --- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| F1  | `ramfb` and a QMP socket on every boot; window by `MINIXRS_DISPLAY=window`                               | Two lines in `qemu-run.sh`                                                                       |
| F2  | Only 32 bpp RGB with 8-bit channels is accepted; anything else = no screen                               | A second `paint` path; the reply already carries shifts and bpp                                  |
| F3  | The window is at 4 GiB and 16 MiB                                                                        | Two constants; nothing else names the address                                                    |
| F4  | Selector 67, shifts as bytes, no mask sizes on the wire                                                  | Not emitted to C, so a repack is one Rust edit on both sides                                     |
| F5  | Overlap arithmetic lives in `kernel-shared`, applied at allocator seed time                              | Move the function; the boot order change (fb before allocator) is the real cost                  |
| F6  | No `server` feature on the TTY crate                                                                     | Add the gate as `fs/mfs` has it, plus its extra clippy step in CI                                |
| F7  | Spleen 8×16 as the upstream `.psfu` (PSF1), indexed by byte                                              | Swap the file; if the ASCII identity fails, a 95-entry map in `font`                             |
| F8  | Deferred wrap; `?` for unknown bytes; grey on black; no cursor                                           | `grid` and its tests only; 6.8 will want the cursor anyway                                       |
| F8  | Scroll repaints only changed cells, found from a shadow array; no fb reads                               | `paint_all` back in `console_write`, drop `Shown` — at a quarter the throughput                  |
| F9  | UART first, then screen; one `fb` verdict line                                                           | Reorder two calls                                                                                |
| F10 | The proof decodes the dump and places the screen in the serial log as one unbroken stretch; no test card | A test card needs a quiet console, i.e. a change to init's rotation                              |
| F10 | Tick bytes are forgiven inside a row, up to 120; lost letters therefore pass                             | Lower `MAX_STRAY`; too low refuses an honest screen on a slow host (CI's `worst=` is unmeasured) |
| F10 | Only a scrolled screen passes in CI (`--scrolled`); an unscrolled one is `early`                         | Drop the flag in `screendump.sh` — and mutation 7 passes again                                   |
| F11 | The dumper pauses the guest (`stop`/`cont`) on every look, every 10 s, until a scrolled screen passes    | Drop the pause; half-repainted dumps then cost tries. A shorter gap pauses the guest more often  |
| F11 | `early` costs no try; a `FAIL` costs one of ten; the ceiling covers the whole script                     | Three constants in `screendump.sh`. Ten forgiven failures also forgive an intermittent defect    |
| F11 | The screen check runs on both full boots in CI and blocks                                                | Make it one boot, or non-blocking, in `ci.yml`                                                   |
| F13 | No Limine `resolution:` is set; the firmware's mode is taken as found                                    | One line in `tools/limine.conf`                                                                  |

---

## 6. Out of scope

- Keyboard input, echo, a cursor and a keymap — 6.8.
- `virtio-gpu` — 6.10; `paint`'s `Surface` is the seam it will use.
- Escape sequences, colour, cursor addressing, more than one font size.
- Kernel or `[diag]` output on the screen (H9).
- Unmapping or resizing the framebuffer; a second framebuffer.
- A framebuffer for the reboot boot's check, or any x86_64 path.

---

## 7. Corrections found in implementation

- **F7 said PSF2; the upstream 8×16 file is PSF1** (Task 3). The decision text above was rewritten;
  the file is still vendored unconverted.
- **F2 and F3 put `pa=` in the middle of their lines; it is last** (Task 4).
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#markers) requires the stable
  fields first so that they can be asserted as one prefix, and `pa=` follows the firmware's memory
  map. The two decisions above were reworded.
- **F9 names "the banner in `tty_init`"; the banner is written by `main`** (Task 4), immediately
  after `sef_startup` returns and after the framebuffer's verdict. `console_write`'s state — the
  grid and the accepted format — is a local of `main`'s frame, which a SEF callback (a plain `fn`
  pointer) cannot reach without a static and the `unsafe` that goes with one. Nothing runs between
  the callback returning and the banner, so the serial log's order is unchanged.
- **F9's re-validation is a fourth library module, `reply`**, not part of `paint` (Task 4): decoding
  the `GET_FRAMEBUFFER` payload and the accept/refuse decision, with the refusal tokens `va`,
  `window` and `grid` beside `FormatError`'s five.
- **F8's full repaint on scroll was replaced by its named fallback, the changed-cells repaint**
  (Task 6, F13.2). Once the screen is full every newline scrolls, and repainting all 100 × 37 cells
  is 473,600 device stores — about 37 ms under QEMU TCG. Three 300-second stub-free unfed boots on
  the same machine, judged by where the log had got to when the timeout fired:

  | Boot                               | final `[ipc N]` | `hello: errno ok` lines | console lines | tick byte inside a line |
  | ---------------------------------- | --------------- | ----------------------- | ------------- | ----------------------- |
  | no painting (Task 2's kernel)      | 11,293,900      | 2,794                   | 44,146        | 57 (0.13%)              |
  | full repaint on scroll (Task 4)    | 1,788,900       | 440                     | 6,836         | 17 (0.25%)              |
  | changed-cells repaint (this entry) | 7,688,800       | 1,901                   | 30,054        | 26 (0.09%)              |

  The full repaint cost a factor of 6.3 in steady-state console throughput; the changed-cells
  repaint costs 1.47, and the share of console lines with a kernel tick byte spliced inside them is
  back at or below the unpainted figure. The mechanism is `paint::Shown`, a second 10,240-byte array
  in `main`'s frame (now 21,376 bytes of the 64 KiB stack, from 11,136) recording the byte painted
  in each cell; the framebuffer is still never read. What remains is the 128 stores a drawn
  character costs and the cells a scroll really does change. A screendump taken 200 seconds into the
  third boot decoded in every cell, so the diff leaves no stale glyph behind.
- **F10's step 3 was rewritten twice, and now states the rule that is built.** The first text said
  every non-blank row "occurs in the serial log", with a full-width row joined to the one below
  before the search. A real log defeated that (Task 5's first fix round): kernel lines begin
  mid-console-line and tick bytes arrive in runs, so rows were looked up one at a time in the log
  with kernel lines removed and up to 32 tick bytes allowed inside a row. **The mutation run then
  defeated that rule** (Task 8): rows looked up one at a time say nothing about where they are, and
  mutation 7 — a scroll that repaints nothing — passed. The rule as built (Task 5's second fix
  round, `d923ea8`) is F10's text above: one unbroken stretch of the console stream after the last
  `fb ok` line, every row anchored to the one above, `MAX_STRAY` 120 with room for a slow host, the
  `fb ok` line read exactly, and a screen classed as scrolled or not.
- **F11 now says what the script does** (Tasks 5, 6 and 8): the gate line must be seen on two
  consecutive polls; the script builds the checker or takes `MINIXRS_FBCHECK`; it looks every 10 s
  for a **scrolled** screen, an `early` verdict costing no try and a `FAIL` one of ten; the ceiling
  covers the whole script; and CI kills a script still running when the boot ends and counts it
  failed. The dump and verdict are uploaded on every run, not only on failure. The first script —
  ten tries three seconds apart, first pass wins — is what let mutation 7 through: its gate line
  appears with 22 of 37 rows drawn.
- **F1 gained the socket path limit** (Task 0's finding b, implemented in Task 2): under 104 bytes,
  refused by the runner.
- **Section 3's mutation table now records what was observed** (Task 8), in place of its "Expected
  to move" column. Row 7's first prediction ("rows not in log order") could not be reported by the
  checker of the time, and the mutation passed it; with the rebuilt checker it fails on a scrolled
  screen. Rows 1 and 2 turned out to be caught by the screen check alone, and row 3 to stall the
  boot with no forbidden marker.
- **F5 said "a mutation proves the wiring"; none does** (Task 8, mutation 6). Skipping the carve
  moves nothing on QEMU. F5 was reworded.
- **F2 gained a condition: the page-rounded end must not pass 2^48** (`PA_LIMIT`; review,
  `6d1cb26`), refused as `address`. The mapper masks a physical address to 48 bits, so a range
  crossing the limit would have been mapped elsewhere with no assert firing.
- **F13.2 and F13.3 have their outcomes** (Task 6): 65.02% / 41.53% of a 600 s log for the last
  required marker, no timeout moved; and the dumper beside a fed boot disturbed nothing.
- **`fb FAIL` does not always mean the two validators disagree** (review). For `grid` it does not:
  the kernel accepts a screen smaller than one 8×16 cell and TTY refuses it.
- **Section 3's host-test list names the modules as built**: `kernel-shared::memrange` and
  `kernel-shared::framebuffer` (the validator, which the first list did not have at all), and
  `tty::reply` beside `tty::paint`.
- **Section 1's table names what the code names**: `kernel-shared::memrange` and
  `kernel-shared::framebuffer`, the kernel's `arch/aarch64/framebuffer.rs`, `reply` in the TTY
  library, `MINIXRS_QMP`, and the `kernel/build.rs` font watch. F6 says four modules, F9 says where
  the banner is written and lists the three expected and four forbidden lines, and F2 lists the
  refusal tokens.

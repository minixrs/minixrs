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

| Where                               | Today                                                                                  | After 6.7                                                                                    |
| ----------------------------------- | -------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `kernel/src/arch/aarch64/limine.rs` | base revision, HHDM, memmap, paging mode, stack size, kernel address                   | plus a framebuffer request and a validated `framebuffer()` accessor (F2)                     |
| `kernel-shared::uspace`             | three kernel-owned windows: device (1 GiB), ramdisk (2 GiB), DMA (3 GiB)               | a fourth, the framebuffer window at 4 GiB (F3)                                               |
| `kernel-shared::callnr`             | `GET_RAMDISK` 64, `GET_VIRTIO_MMIO` 65, `GET_DMA` 66                                   | `GET_FRAMEBUFFER` 67, gated to TTY (F4)                                                      |
| `kernel-shared`                     | no range arithmetic                                                                    | a pure, host-tested `subtract(region, hole)` the allocator uses (F5)                         |
| `kernel/src/mm/frame.rs`            | regions seeded from every `MEMMAP_USABLE` entry                                        | the same, minus any accepted framebuffer's range (F5)                                        |
| `userland.rs::load_boot_server`     | TTY gets one device page, the PL011                                                    | plus the framebuffer's pages, `Prot::DEVICE_RW`, at `FB_VA` (F3)                             |
| `drivers/tty`                       | a bin only (`main`, `input`, `irq`, `pl011`); `pl011::write` is the one console output | a lib half (`font`, `grid`, `paint`) and one `console_write` that feeds both sinks (F6–F9)   |
| `tools/qemu-run.sh`                 | `-display none`, no QMP, no display device                                             | `-device ramfb` and a QMP socket always; `MINIXRS_DISPLAY=window` drops `-display none` (F1) |
| `tools/`                            | no screen check                                                                        | `tools/fbcheck` (host Rust) and `tools/screendump.sh` (F10–F11)                              |
| CI `qemu-smoke`                     | three boots, serial markers only                                                       | both full boots also run the screen check; the dump is an artifact on failure (F11)          |

`userland/`, `servers/`, the CDEV band and every existing serial marker are untouched.

---

## 2. Decisions

### F1 — the machine: `ramfb` on every boot, QMP on every boot, a window on request

`qemu-run.sh` adds `-device ramfb` and `-qmp unix:<sock>,server=on,wait=off` unconditionally, so CI
and a window boot run one code path (H9). The socket is `${MINIXRS_QMP:-$ROOT/target/qmp.sock}` —
the `MINIXRS_HVC0` pattern, so two boots at once can be kept apart. `MINIXRS_DISPLAY=window` omits
`-display none` and lets QEMU pick its default UI; any other value but `none` (the default) is
refused. No resolution is requested from Limine: the kernel takes the mode the firmware set (F13
measures which).

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
- `pitch * height`, rounded up to pages, fits `FB_WINDOW_SIZE` (all products `checked_`).

The accepted line is `[fb] ok w=… h=… pitch=… r=… g=… b=… usable=<pages> pa=…` where `usable` counts
the pages F5 had to carve (expected 0) and the firmware-dependent `pa=` comes last, so that the
stable fields before it can be asserted as one prefix. A missing response prints `[fb] none`.

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
carving arm does not run at boot. That is stated as an honest "nothing moves" row (section 3): the
host tests prove the arithmetic and a mutation proves the wiring.

*Rejected:* refusing an overlapping framebuffer (simpler, and contradicts H9); carving after
allocation has begun (frames already handed out could be the screen).

### F6 — TTY grows a library half, with no feature gate

`drivers/tty/src/lib.rs` holds three pure modules; `main.rs` stays the bin and is the only file that
touches a device.

- `font` — glyph lookup over the vendored font (F7).
- `grid` — the text model: a fixed `MAX_COLS × MAX_ROWS` (160 × 64) byte array, a cursor, and `put`.
  A screen larger than that is used up to that many cells from the top-left; the rest stays
  background.
- `paint` — turns grid cells into pixel stores through a `Surface` trait whose one write is
  `store(x, y, pixel: u32)`. The bin implements it with an aligned volatile `u32` store at `FB_VA +
  y * pitch + x * 4`; the host implements it over a `Vec<u32>`.

Unlike `fs/mfs` there is no `server` feature: nothing reaches the TTY lib from a build script, and
the crate's dependencies already build on the host for `cargo test`.

**Device-memory discipline (H9):** every framebuffer access is one aligned 32-bit volatile store.
The renderer never reads the framebuffer — scrolling repaints from the grid — so there is no load,
no `copy_nonoverlapping` and no `memset` the compiler could merge or misalign.

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

A scroll shifts the grid and repaints every cell. Whether that fits the boot budget is F13's second
measurement; the fallback, recorded there, is to repaint only cells whose byte changed.

### F9 — one `console_write`, UART first

TTY's two output sites — the banner in `tty_init` and `do_write` — call one `console_write(bytes)`,
which writes the UART and then, when a framebuffer was reported, the grid and the screen. UART first
so that no serial marker's timing depends on painting. At start-up TTY asks `GET_FRAMEBUFFER` once,
before the banner, clears the screen to background, and prints exactly one of:

- `[diag tty] fb ok w=… h=… cols=… rows=…`
- `[diag tty] fb none rc=…` (`ENOENT`: no framebuffer; anything else is a defect)
- `[diag tty] fb FAIL …` — a reply TTY will not draw on (it re-checks F2's conditions rather than
  trusting them, and checks `FB_VA`), after which it is serial-only.

`fb ok` joins the common expected-marker file and `fb FAIL` the forbidden one. The reboot boot's
seven-line set is unchanged.

### F10 — the proof: decode the screen, and find its text in the serial log

The screen is never quiet — init's rotation prints for ever — so there is no fixed image to compare
against. `tools/fbcheck` (a host crate depending on the TTY lib) instead checks a dump against the
renderer and against the boot's own serial log:

1. **Geometry.** The dump's size equals the `w=`/`h=` of `[diag tty] fb ok` in the log.
2. **Every cell is a glyph.** Each 8×16 cell inside the grid decodes, pixel-exactly, to one byte
   drawn by `paint` in the two console colours; every pixel outside the grid is background.
   Re-rendering the decoded grid with the same library reproduces the dump — the phase file's
   "matches an image the host renders from the same text".
3. **The text is the boot's.** At least `MIN_ROWS` (8) rows are non-blank, and every non-blank row
   occurs in the serial log (a row exactly `cols` wide is joined with the row below it before the
   search, undoing the wrap).

Step 3 is what makes the check more than the renderer agreeing with itself: the serial log is
written by `pl011`, not by the renderer. One hand-drawn golden glyph in the host tests covers the
remaining blind spot, a font read wrongly on both sides.

*Rejected:* a fixed "test card" image (nothing in the default boot is ever the last writer);
reconstructing the console stream from the serial log and rendering it (kernel lines and tick bytes
interleave with it unpredictably, which
[`testing-and-markers.md`](../../conventions/testing-and-markers.md) already records).

### F11 — taking the dump: stop, dump, continue, retry

`tools/screendump.sh <qemu-log> <out.ppm>` polls the log for `minix.rs init: stdin ok` (the last
event the feeder drives), then runs `fbcheck dump-and-check` up to `TRIES` times, `GAP` seconds
apart: over QMP it sends `stop`, `screendump` (PPM), `cont`, then applies F10. The first pass wins;
a dump caught with a glyph half-painted fails step 2 and is simply retried. It exits non-zero, with
the last dump kept, if no try passes or a ceiling expires. `fbcheck`'s QMP client is a few lines
over `UnixStream`; no new host dependency.

CI runs it as a background job beside each of the two full boots and `wait`s on it after the boot's
`timeout`; a failure fails the job and uploads the `.ppm` with the logs. The reboot boot is not
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
   budget, F8's changed-cells-only repaint is tried before any timeout is.
3. **Whether `stop`/`screendump`/`cont` disturbs the boot** — the markers and the feeder's exchange
   still pass with the dumper running.

CI's QEMU and firmware (`qemu-efi-aarch64`) are unmeasured until the branch's first run, as in 6.4
and 6.6.

---

## 3. Verification

**Host tests** (each named in the plan with its task):

- `kernel-shared` `subtract` — hole outside, covering, at the head, at the tail, in the middle (two
  regions), empty hole, touching edges.
- `kernel-shared::callnr` — `GET_FRAMEBUFFER`'s offsets ordered, disjoint, aligned; the selector
  tripwires grown.
- `kernel-shared::uspace` — the four windows pairwise disjoint and below `USER_VA_TOP`.
- `tty::font` — header accepted; the ASCII identity through the Unicode table; one golden glyph by
  hand.
- `tty::grid` — each row of F8's table; deferred wrap (`cols` bytes then `\n` is one row); scroll;
  clamping to `MAX_COLS × MAX_ROWS`; a zero-sized screen draws nothing.
- `tty::paint` — a cell's 128 stores land at the right coordinates with the right shifts; nothing is
  stored outside `w × h`; pitch wider than `w * 4` is honoured.
- `fbcheck` — a rendered image decodes back to its text; one flipped pixel, a wrong size, a row
  absent from the log, and too few rows are each refused; the wrap join.

**Boot markers:** `[fb] ok`, `[devmap] tty fb`, `[diag tty] fb ok` in the common expected file;
`[diag tty] fb FAIL` forbidden. Both full boots, the reboot and `diff-backends.sh` as the definition
of done requires.

**The screen check** (F10–F11) passes on both full boots.

**Mutations** (each observed, then reverted):

| # | Mutation                                             | Expected to move                    |
| - | ---------------------------------------------------- | ----------------------------------- |
| 1 | `console_write` skips the framebuffer                | screen check: too few rows          |
| 2 | `paint` stores at `x + 1`                            | screen check: cells do not decode   |
| 3 | the kernel maps the window one page short            | TTY faults / `fb ok` missing        |
| 4 | `GET_FRAMEBUFFER`'s gate names another proc          | `fb none rc=-1`, `fb ok` missing    |
| 5 | swap red and blue shifts in the reply                | none on grey text — honest row      |
| 6 | the loader treats the range as usable (skip carving) | none on QEMU — honest row (F5)      |
| 7 | scroll shifts the grid without repainting            | screen check: rows not in log order |
| 8 | drop `-device ramfb`                                 | `[fb] none`, `fb ok` missing        |

Mutation 5 has no proof while the console draws grey; it is recorded as a debt owned by whoever adds
colour. Mutation 7's exact symptom is to be observed, not assumed.

---

## 4. Documentation owed in the same PR

- `book/src/drivers/overview.md` — TTY's two sinks, the lib/bin split, the renderer and font;
  `book/src/boot/overview.md` — the framebuffer request and its validation;
  `book/src/memory/overview.md` — the fourth window and the carve rule;
  `book/src/reference/syscalls.md` — `GET_FRAMEBUFFER`; `book/src/build.md` — the window mode.
- `docs/conventions/` — `build-and-boot.md` (ramfb, QMP, `MINIXRS_DISPLAY`, `MINIXRS_QMP`),
  `testing-and-markers.md` (the screen check and its retry), `ci.md` (the extra step and artifact),
  `servers-and-drivers.md` (TTY's device-store rule), `abi.md` (selector 67).
- `docs/plan.md` and `phase-6-virtio.md` — the box, unverified fact 3's outcome, the debts.
- `uspace.rs` and `limine.rs` module docs; `proc/table.rs`'s stale "only `vblk` has a boot module"
  line (6.6's debt 14, owned by the next slice to touch the kernel — this one).

---

## 5. Rulings to check

Decisions the phase file did not make. Each is undone by the change in its last column.

| #   | Ruling                                                                      | If wrong                                                                        |
| --- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| F1  | `ramfb` and a QMP socket on every boot; window by `MINIXRS_DISPLAY=window`  | Two lines in `qemu-run.sh`                                                      |
| F2  | Only 32 bpp RGB with 8-bit channels is accepted; anything else = no screen  | A second `paint` path; the reply already carries shifts and bpp                 |
| F3  | The window is at 4 GiB and 16 MiB                                           | Two constants; nothing else names the address                                   |
| F4  | Selector 67, shifts as bytes, no mask sizes on the wire                     | Not emitted to C, so a repack is one Rust edit on both sides                    |
| F5  | Overlap arithmetic lives in `kernel-shared`, applied at allocator seed time | Move the function; the boot order change (fb before allocator) is the real cost |
| F6  | No `server` feature on the TTY crate                                        | Add the gate as `fs/mfs` has it, plus its extra clippy step in CI               |
| F7  | Spleen 8×16 as the upstream `.psfu` (PSF1), indexed by byte                 | Swap the file; if the ASCII identity fails, a 95-entry map in `font`            |
| F8  | Deferred wrap; `?` for unknown bytes; grey on black; no cursor              | `grid` and its tests only; 6.8 will want the cursor anyway                      |
| F8  | Scroll repaints everything, no framebuffer reads                            | Changed-cells repaint (already the named fallback)                              |
| F9  | UART first, then screen; one `fb` verdict line                              | Reorder two calls                                                               |
| F10 | The proof decodes the dump and matches rows to the serial log; no test card | A test card needs a quiet console, i.e. a change to init's rotation             |
| F11 | The dumper pauses the guest (`stop`/`cont`) and retries                     | Drop the pause; retries alone still converge, more slowly                       |
| F11 | The screen check runs on both full boots in CI and blocks                   | Make it one boot, or non-blocking, in `ci.yml`                                  |
| F13 | No Limine `resolution:` is set; the firmware's mode is taken as found       | One line in `tools/limine.conf`                                                 |

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

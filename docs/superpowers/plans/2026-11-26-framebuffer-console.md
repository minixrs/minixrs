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
[fb] none                                                          kernel, no response
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
5. **TTY re-validates the reply** rather than trusting the kernel: `w * 4 <= pitch`, `pitch * h`
   within the window, shifts sane (`tty::paint` tests).
6. **No store outside `w × h`**, including a width or height that is not a multiple of the cell
   (`tty::paint` test with an 13×21 surface).
7. **Deferred wrap** and scroll at the last row (`tty::grid` tests).
8. **`fbcheck` refuses**: one flipped pixel, wrong geometry, a row absent from the log, too few rows
   (`fbcheck` tests) — and passes a dump with a wrapped row.
9. **Tripwires grown, not left:** `callnr.rs`'s selector asserts; `GETINFO_DENIED` users.
10. **`book/`** read end to end for every chapter touched.

## Tasks

### Task 0: Measure first (F13)

Throwaway kernel edit (framebuffer request, print the response and the memmap entry holding it,
paint a block through the HHDM) and `-device ramfb -qmp …` through `qemu-run.sh`'s pass-through
arguments. Record geometry, format, memmap kind, and whether the block is in a `screendump`. The
kernel edit is discarded, not committed. F13.2 and F13.3 are measured on the real code (Task 6).

### Task 1: `kernel-shared` — the window, the selector, `subtract` (F3, F4, F5)

Files: `kernel-shared/src/{uspace,callnr}.rs`, a new `kernel-shared/src/memrange.rs`. Tests first:
`subtract`'s seven cases; the offsets test; the four-window disjointness test. Commit.

### Task 2: kernel — request, validation, carve, map, `GET_FRAMEBUFFER` (F2–F5)

Files: `kernel/src/arch/aarch64/{limine,userland}.rs`, `kernel/src/mm/frame.rs`,
`kernel/src/main.rs`, `kernel/src/system/do_getinfo.rs`, `tools/qemu-run.sh` (`-device ramfb`).
Proof: `[fb] ok` and `[devmap] tty fb` in a stub-free boot. Commit.

### Task 3: TTY lib — `font`, `grid`, `paint` (F6–F8)

Files: `drivers/tty/Cargo.toml`, `drivers/tty/src/{lib,font,grid,paint}.rs`,
`drivers/tty/font/{spleen-8x16.psfu,LICENSE,README.md}`. Tests first, as the spec's section 3 lists.
Commit.

### Task 4: TTY bin — `console_write` and the verdict line (F9)

Files: `drivers/tty/src/main.rs`, the marker files under `tests/`. Proof: `[diag tty] fb ok` in a
boot and a hand `screendump` that shows text. Commit.

### Task 5: `fbcheck`, `screendump.sh`, QMP and window mode in the runner (F1, F10, F11)

Files: `tools/fbcheck/` (new workspace member), `tools/screendump.sh`, `tools/qemu-run.sh`. Tests
first for decode and the refusals. Proof: the check passes on a fed disk boot. Commit.

### Task 6: CI and the budget (F11, F13.2, F13.3)

Files: `.github/workflows/ci.yml`. Measure the last marker's position against the merge base; adopt
changed-cells repaint first if over budget. Commit.

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
  and says so (Task 5). (c) F13.2 and F13.3 are measured on the real code at Task 6, not with the
  probe.

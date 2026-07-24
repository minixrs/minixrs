# Pre-Phase-5 Cleanup + Prep

Phase 4 shipped in full (slice 4.8, PR #31, merged 2026-07-18). Before Phase 5
(musl + file systems) starts, the close-out review identified a set of
PR-sized cleanup/prep chunks so Phase 5 does not build on soft ground.

**How to use this file:** each chunk is one session / one PR. Chunks 1–5 are
independent and can land in any order; chunk 6 (the Phase 5 design + slicing
session) must come last — it gates starting Phase 5 proper. Markers follow the
`docs/plan.md` convention: `◀ next` (unstarted), `◀ ready (branch …, pending
merge)`, `✓ shipped (PR #N, merged YYYY-MM-DD)`. Flip a chunk's marker as part
of its own PR, and move `◀ next` to whichever chunk you intend to take next.

---

## Chunk 1: CI QEMU smoke job ◀ next

**Goal:** the kernel's `target_os = "none"` modules currently have zero CI
coverage — every Phase 2–4 regression was caught by hand-running QEMU. One
smoke job closes that gap cheaply.

**Scope:**

- New job in `.github/workflows/ci.yml` (ubuntu): install
  `qemu-system-aarch64` + edk2 firmware, build the kernel
  (`cargo kernel-aarch64`), boot via the cargo runner under `timeout 25`,
  capture the serial log to a file.
- Grep the log (`grep -a` — raw tick bytes make it binary-ish) against a
  checked-in expected-substrings file (e.g. `tests/qemu-boot.expected`):
  the 11 `[as]` lines, init's `SYS_FORK`/`SYS_EXEC name=worker`/`SYS_EXIT`
  chain, stub D's designed `sig=11` kill chain, RS `[alarm]` fires; and
  assert the absence of `panic` / `el0_sync_unexpected`.
- Start `continue-on-error: true` like geiger/miri; flip to blocking once it
  proves stable across a few PRs.
- Remember QEMU-under-TCG guest time runs slower than wall clock — pick
  expectations that land well inside the timeout (boot chatter + first alarm
  fires, not tick counts).
- Pin any third-party actions to full commit SHAs (repo convention).

**Proof:** a PR that deliberately breaks boot (or the expected file) fails the
job; a green run greps every expected marker.

## Chunk 2: mdBook content port + legacy docs retirement ◀ next

**Goal:** the mdBook in `book/` is the canonical documentation, but its
architecture/IPC pages are stubs while the real content sits in legacy
`docs/*.md` written in Phase-0 future tense (CoW, drivers, musl described as
present). Port with an accuracy pass; retire the legacy files.

**Scope:**

- Port `docs/{architecture,ipc,servers,boot,build,memory-layout,syscalls,
  drivers,musl,minix3-mapping}.md` into `book/src/` chapters, rewritten
  against source: present tense only for what exists as of Phase 4;
  aspirational content either dropped or clearly marked as roadmap.
- Delete each legacy `docs/*.md` as it is ported (`docs/plan.md` and
  `docs/plans/` stay — they are the planning tree, not the book).
- Document the QEMU trace-forensics rule in the book's build/debugging
  chapter (zero `[ipc]` samples ≠ stuck caller; head-carve vs modulo
  sampling; `grep -a`; TCG time skew) — currently tribal knowledge in
  CLAUDE.md only.
- May split into two PRs (kernel chapters first, servers/build second).

**Proof:** `mdbook build book` green; legacy files gone; book pages describe
only what boots today.

## Chunk 3: Stub A–D disable flag ◀ next

**Goal:** stubs A–D are the live regression battery (IPC ping-pong, SCHED
delegation, VM fault paths) but they consume ASIDs/priv slots and flood traces
(stub C especially). Debugging init/musl wants a clean boot without deleting
the battery.

**Scope:**

- Cargo feature on the kernel crate (e.g. `boot-stubs`, **default-on**)
  gating the stub blob assembly (`user_stub.S` in `build.rs`), the
  `build_stub` calls + VAs + priv installs in `userland.rs`, and PM's stub
  `mproc` seeds (PM must not seed procs that don't exist — check
  `com.rs`/`mproc.rs` coupling; `NR_STUB_PROCS` and `FORK_POOL_BASE` must
  stay consistent between the kernel and PM for a given feature setting).
- Default stays on so CI smoke (chunk 1) and normal dev keep the battery.
- Document the flag + the clean-boot invocation in CLAUDE.md and the book's
  build chapter.

**Proof:** `--no-default-features`-style boot shows only servers + init/worker
`[as]` lines and no stub traffic; default boot unchanged.

## Chunk 4: Capacity ceilings ◀ next

**Goal:** the effective user-process capacity is spread across three silent
constants — PM `NR_MPROCS` (32), VM `MAX_CLIENTS` (32), and the SCHED table
(16) — plus VM `MAX_REGIONS = 4` per client, which one loader + heap + stack
+ a few mmaps will blow immediately in Phase 5.

**Scope:**

- Raise VM `MAX_REGIONS` 4 → 16.
- Move the shared capacity story into `kernel-shared` (one constant for the
  user-process ceiling; PM/VM/SCHED tables sized from it) instead of three
  independent numbers that happen to agree.
- Const-assert the relationships (`MAX_CLIENTS >= NR_MPROCS`, fork pool fits,
  SCHED table covers delegated procs) so a future bump can't silently skew.

**Proof:** host tests green; QEMU boot + fork/exec loop unchanged; a
deliberate mismatched-constant build fails at compile time.

## Chunk 5: Toolchain bump + kernel clippy debt ◀ next

**Goal:** the pinned nightly (`rust-toolchain.toml`) is ~2 months old; bump it
deliberately *before* Phase 5 churn, not mid-slice. Same session: deal with
the known kernel-target clippy lints that CI never sees.

**Scope:**

- Bump the dated nightly pin; fix fmt/lint fallout across the workspace
  (blocking gates: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, plus
  `cargo kernel-aarch64` and the QEMU boot).
- Fix or explicitly quarantine (`#[allow]` + comment) the pre-existing
  `cargo clippy -p minixrs-kernel --target aarch64-unknown-none` lints
  (nomem-asm pointers, `manual_is_multiple_of`, interior-mutable-const).
- Optional: add a non-blocking CI job running kernel-target clippy so the
  surface stays visible.

**Proof:** all blocking gates green on the new nightly; kernel-target clippy
clean or every remaining lint carries an explicit allow + rationale.

## Chunk 6: Phase 5 design + slicing session (gates Phase 5) ◀ next

**Goal:** Phase 5 in `docs/plan.md` is six bullets and a milestone. Phases 2–4
succeeded because each slice was PR-sized, independently bootable, and left a
QEMU-visible trace. Phase 5 needs the same treatment — and several design
decisions locked *before* wrappers are written.

**Scope:** a dedicated brainstorm/design session (plan mode, not
implementation) producing `docs/plans/phase-5-musl-fs.md`:

- **Design decisions to lock:** console/stdio sink for the `printf` milestone
  (kernel diag call vs minimal TTY vs deferred); root-image strategy
  (initramfs vs MXBI-embedded FS image before block drivers); grant model
  (real MINIX-style safecopy grants vs interim kernel copy API — prefer real
  grants); ELF-loading authority for FS-backed exec (reuse kernel
  `boot_image/elf.rs` vs VM/PM); cbindgen/ABI-freeze timing for the
  `kernel-shared` → C header bridge; musl vendoring policy (tree vs
  submodule, CI scan exclusions).
- **Slice decomposition:** PR-sized slices with observable QEMU proof each.
  Expected opening slices (feature work, deliberately *not* in this cleanup
  file): fault-safe user copy for messages (`EFAULT`, not a kernel panic, on
  a bad user pointer) and a real grant table + `SYS_SAFECOPY`/`SYS_SETGRANT`
  — every interesting Phase 5 data path (VFS read/write, MFS↔VFS, later
  BDEV) moves bytes cross-address-space.
- Rewrite `docs/plan.md`'s Phase 5 section as the slice list and move
  `◀ next` onto slice 5.0.

**Proof:** the design doc exists with every decision above resolved (not
"TBD"), and plan.md's Phase 5 section is a slice table.

---

## Drive-by notes (not chunks)

- Stale era comments ("until phase 3", "slice 2.6+") in
  `kernel/src/ipc/{message.rs,senda.rs,mod.rs}` hot paths — clean up in the
  first Phase 5 PR that touches those files; not worth a standalone PR.
- README / book "six IPC primitives" claims were softened to "five live +
  SENDA stubbed" in the close-out PR; if SENDA ever becomes real (it is
  currently `ENOSYS` *and* denied by the `trap_mask: u16` gate — bit 16
  doesn't fit), restore the claim then.

## Non-goals before Phase 5

Do **not** block Phase 5 on: the x86_64 port (Phase 8), VirtIO blk/net
(Phase 6 — only a console story is needed for the printf milestone), CoW
fork, full RS live-update/restart, implementing SENDA, an interactive
shell/coreutils (Phase 7), replacing stubs A–D entirely (the disable flag is
enough), or full POSIX signal semantics (DFL/IGN + kill + wait first;
handlers follow).

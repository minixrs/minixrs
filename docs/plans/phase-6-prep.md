# Pre-Phase-6 Cleanup + Prep

Phase 5 shipped in full — milestone B at slice 5.9, plus the three post-milestone stretch slices
5.10a, 5.10b and 5.11. Before Phase 6 (VirtIO drivers + root on disk) starts, a close-out review of
Phases 0–5 identified the PR-sized chunks below, so virtio-blk, IRQ delivery and a disk root do not
land on scaffolding Phase 5 deliberately left temporary.

**How to use this file:** each chunk is one session / one PR. Chunk 2 (the user VA map) should land
**before** any virtio code — every new fixed VA a driver premaps makes that migration harder — and
chunk 4 (the Phase 6 design + slicing session) gates starting Phase 6 proper. The rest are
independent and can land in any order. The per-slice design and plan documents go in
`docs/superpowers/{specs,plans}/` as usual; this file tracks status and links to them rather than
restating them.

> **Status convention.** A slice's status is a GFM checkbox — `- [ ]` not started, `- [x]` done —
> checked by the PR that does the work, in that same PR. The first unchecked box in plan order is
> the next slice. Lines in the older form, `✓ shipped (PR #N, merged YYYY-MM-DD)`, are retired-form
> history; never write a new one. Full rule:
> [`docs/conventions/git-and-prs.md`](../conventions/git-and-prs.md).

**Provenance.** This file replaces a loose `PRE6-RECOMMEND.md` review draft (2026-08-04, revalidated
2026-09-17) that was never tracked. Every constant it cited was re-checked against `main` before
being carried here; items that had shipped in the meantime were dropped rather than copied, and the
one item that is simply *done* is recorded as chunk 1 so the reasoning is not lost.

## Status

- [x] **Chunk 1** — plan-marker freshness (superseded by the checkbox convention; PR #58)
- [x] **Chunk 2** — user VA map: high stack, larger stack, image-relative brk
- [ ] **Chunk 3** — `SYS_IRQCTL` design note
- [ ] **Chunk 4** — Phase 6 tracker + slicing session
- [ ] **Chunk 5** — musl syscall surface
- [ ] **Chunk 6** — SDK flavor CI coverage

---

## Chunk 1: Plan-marker freshness

**Goal (as written 2026-08-04):** the tracker still labelled 5.11 `◀ ready (pending merge)` after PR
#57 had merged, and no `◀ next` marker existed anywhere under `docs/`, so the tracker did not say
what came next.

**Outcome:** resolved by removing the class of defect rather than the instance. PR #58 replaced the
`◀ next` / `◀ ready` / `✓ shipped (PR #N, merged …)` marker vocabulary with a **GFM checkbox checked
by the PR that does the work**, so a slice can no longer be described as complete by a later
documentation pass, and "what is next" is read off the first unchecked box instead of a marker
somebody has to remember to move. The retired-form `✓ shipped` lines on the Phase 2–5 slices stay as
history; no new one is ever written.

Nothing further is owed here. Matching checkboxes live under Pre-Phase-6 cleanup in
[`../plan.md`](../plan.md); chunk 1 is recorded as done there too, so the first unchecked box is
chunk 2 rather than virtio transport.

## Chunk 2: User VA map — high stack, larger stack, image-relative brk

**Design:**
[`2026-09-19-user-va-map-design.md`](../superpowers/specs/2026-09-19-user-va-map-design.md) —
decisions `V1…V14`, including two things this file did not anticipate. VM had never been told that
an exec happened, so an exec'd process kept the regions of an image whose address space the kernel
had already torn down; V6 closes that with `VM_EXEC`, and `dropped=1` on every `[diag vm] exec` line
is the gap being closed once per exec. And the clang `--image-base` pin is dropped rather than kept
— it only ever compensated for the stack sitting at lld's default base.

**Plan:** [`2026-09-19-user-va-map.md`](../superpowers/plans/2026-09-19-user-va-map.md) — thirteen
tasks, all landed on `feature/user-va-map`.

**Tooling hand-off — outstanding.** The minixrs half is done and checked above; the tooling half is
not. [`2026-09-19-user-va-map-tooling.md`](../superpowers/plans/2026-09-19-user-va-map-tooling.md)
carries the exact edits for a separate session in `~/src/tooling`: `verify/check-image.sh`'s VA
constants and overlap rule, `verify/check-driver.sh`'s `--image-base` assertion, LLVM patch 0006's
dropped pin, and `verify/selftest.sh`'s inverted `image-base-1m` fixture. Per `V12` the ordering is
load-bearing — **this repo's PR lands first**, because dropping the pin before the stack moves would
link SDK images straight onto the stack page — and the three-boot matrix is re-run against the
rebuilt SDK afterwards. Chunk 2's box above is checked for the OS-side work it names; the tooling
edits are tracked by that plan, not by this box.

**Do this before any virtio code.** Phase 5 kept a greenfield *low* map that is not borrowed from
32-bit MINIX 3 (which puts `USR_STACKTOP` near `0xF0000000`). Verified on `main`:

| VA            | Role                                                                      | Defined in                                |
| ------------- | ------------------------------------------------------------------------- | ----------------------------------------- |
| `0x0010_0000` | load base (`user.ld` / clang `--image-base`)                              | `user.ld`, LLVM patch 0006                |
| `0x0020_0000` | **one** stack page (`SERVER_STACK_VA`) — a 1 MiB image ceiling            | `kernel/src/arch/aarch64/userland.rs:149` |
| `0x0100_0000` | fixed `HEAP_BASE` (brk)                                                   | `servers/vm/src/region.rs:36`             |
| `0x0200_0000` | mmap bump arena (`MMAP_BASE`)                                             | `servers/vm/src/region.rs:50`             |
| `0x4000_0000` | device window (TTY UART; Phase 6 virtio-mmio pages) — also `REGION_LIMIT` | `kernel-shared/src/uspace.rs`             |
| `0x8000_0000` | ramdisk window                                                            | `kernel-shared/src/uspace.rs`             |

Four consequences, all of which Phase 6 makes worse:

1. **Image ceiling.** Any `PT_LOAD` reaching `0x200000` collides with the stack (`AlreadyMapped` /
   exec `ENOMEM`). Multi-MiB disk-resident programs are exactly what Phase 6 invites, and tooling's
   risk register already tracks this.
2. **One-page stack.** `SERVER_STACK_BYTES == USER_PAGE_SIZE == 4096`, unchanged by the stretch
   slices — which instead added pressure: MFS carries **two** 4 KiB `.bss` buffers (`BLOCK` and
   5.10b's `STAGE`) and VFS a 256 KiB `EXEC_STAGE`, all `.bss` precisely because a one-page stack
   cannot hold them. VirtIO descriptor rings and bounce buffers hit the same wall.
3. **Fixed heap origin.** `HEAP_BASE` is unrelated to the last loaded segment. musl malloc
   eventually needs a real `brk`/`mmap`; opening USER→VM without fixing the map cements the wrong
   layout.
4. **The clang pin is compensating for the OS.** LLVM patch 0006 forces `--image-base=0x100000`
   because lld's aarch64 default *is* `0x200000` — the stack. Relocating the stack is the OS-side
   fix the tooling docs deferred to "a later minixrs slice".

**Scope:**

- **High stack.** Map the initial stack near the top of user VA, clear of the device (`0x4000_0000`)
  and ramdisk (`0x8000_0000`) windows. Prefer a MINIX-shaped `USR_STACKTOP`-style constant in
  `kernel-shared::uspace` so tooling's `check-image.sh` can share it. Note `userland.rs`'s `const _`
  assert that the device window sits above `SERVER_STACK_VA + PAGE_SIZE`: a high stack **inverts**
  that guard, so rewrite it rather than delete it.
- **Grow the stack.** Raise `SERVER_STACK_BYTES` to several pages, for servers and for exec'd
  images. Revisit MFS's two `.bss` buffers and VFS's stage — a local becomes viable again; keep
  `fs/mfs`'s `const _` tripwire honest. musl's `%Lf` VLAs (~7.4 KiB) were already a landmine at 4
  KiB.
- **brk after the last `PT_LOAD`.** Drop the fixed `HEAP_BASE` for exec'd (and eventually boot)
  images: the loader reports image-end, VM seeds the heap region at `[page_align_up(image_end), …)`
  or defers creating it until the first `VM_BRK`. Stub D / boot-stubs need a migration story or stay
  on a legacy origin behind the `boot-stubs` feature.
- **mmap arena.** Keep it bumping above the heap, or place it in a high hole below the stack; still
  capped at `REGION_LIMIT` (the lowest kernel-owned window, `USER_DEVICE_WINDOW_BASE`).
- **`Kind::Stack` (optional).** Track the stack as a VM region so fork clones it and fault
  accounting stays consistent — today the kernel maps the page and VM never hears about it.
- **Tooling sync.** Update tooling's `verify/check-image.sh` VA constants in the **same** change
  window, and decide whether clang may drop the unconditional `--image-base` pin (keep it if a
  shared `0x100000` load base independent of the stack is still wanted).

**Proof:** a multi-MiB image loads and runs (`userland/bigprog`); `sp` is high in the `[exec] …
sp=0x3fff` marker, which this chunk tightened from its prefix-only `sp=0x` form; brk lands past the
last `PT_LOAD`, witnessed by the `[diag vm] exec nr=` marker; tooling's image checker is green after
the hand-off above. Re-mutation-test the image-base and oversized-image fixtures. This is a
**mandatory** three-boot matrix change — see
[`ci.md`](../conventions/ci.md#the-sdk-flavor-has-zero-ci-coverage).

**Out of scope:** CoW, ASLR, guard-page stack growth, and USER→VM for malloc — the last can follow
immediately once the map is sane.

## Chunk 3: `SYS_IRQCTL` design note

**Goal:** Phase 6's first real kernel work is IRQ delivery to EL0 (a `NOTIFY` from `HARDWARE` after
registration). `stubs::do_irqctl` is still `ENOSYS`, dispatched from `kernel/src/system/mod.rs`.
Write the design note **before** coding virtio-blk.

**Scope** — a section in the Phase 6 tracker (chunk 4) covering:

- register / unregister / enable / disable subcodes;
- privilege (`k_call_mask`): which drivers may claim which IRQs;
- masking while a NOTIFY is in flight (MINIX's storm avoidance);
- GIC routing on QEMU `virt` for virtio-mmio IRQs;
- how TTY RX and virtio-console share the pattern. 5.11 already shaped this: `CDEV_READ` exists and
  TTY refuses it from its unknown-request arm, so RX is **one new arm in TTY** with no VFS or ABI
  change.

**Do not** invent a one-off "poll forever in the driver" for blk. TCG may hide the latency, but the
milestone is a disk root under real completion interrupts.

**Proof:** the note exists and 6.1 implements it; a single forced-IRQ round trip is visible as a
`[diag]` line or a boot marker before any virtio code is written.

## Chunk 4: Phase 6 tracker + slicing session

**Goal:** [`../plan.md`](../plan.md)'s Phase 6 is five checkboxes and one milestone, and there is no
`docs/plans/phase-6-virtio.md`. That is exactly how Phase 5 looked before `phase-5-prep.md` chunk 6
produced [`phase-5-musl-fs.md`](phase-5-musl-fs.md). **This chunk gates starting Phase 6 proper.**

**Scope:** add `docs/plans/phase-6-virtio.md` with locked decisions and a per-slice scope/proof
table, linking to the `docs/superpowers/{specs,plans}/` documents rather than restating them (see
[`docs-and-workflow.md`](../conventions/docs-and-workflow.md)).

**Recommended decomposition** (adjust freely):

| Slice   | Goal                                                | Observable proof                                                                     |
| ------- | --------------------------------------------------- | ------------------------------------------------------------------------------------ |
| **6.0** | User VA map — chunk 2, if it has not already landed | Multi-MiB image loads; `sp` high; brk past the last `PT_LOAD`; tooling checker green |
| **6.1** | `SYS_IRQCTL` + HARDWARE NOTIFY                      | Driver registers a line; a forced IRQ reaches it; storm-safe masking                 |
| **6.2** | `driver-rt` VirtIO MMIO transport + virtqueues      | Enumerate a virtio device; feature bits; read back a known config field              |
| **6.3** | `virtio-blk` behind BDEV                            | MFS root works with MEM *or* virtio selected; read **and write** markers identical   |
| **6.4** | GPT / `mkimage.sh` + Limine + a MinixFS partition   | QEMU `-drive if=virtio`; boots without the MXBI `rootfs` (or with it as fallback)    |
| **6.5** | TTY RX and/or virtio-console                        | TTY serves `CDEV_READ`; optional shell prep for Phase 7                              |
| **6.6** | virtio-net, packet I/O only                         | TX/RX frames in diag; no TCP                                                         |

Landing IRQCTL + virtqueues + blk + disk image + console RX in one PR would recreate the pain Phases
4–5 avoided. `tools/` has no `mkimage.sh` today.

## Chunk 5: musl syscall surface

**This is the widest gap between what the kernel can do and what C can reach**, and the stretch
slices widened it. `external/musl`'s `src/minixrs/_syscall.c` still dispatches exactly six Linux
call numbers: `writev`, `write`, `exit`, `exit_group`, `set_tid_address`, `ioctl`.

- **A C program cannot open, read, or write a file.** Everything 5.8–5.10b built (`VFS_OPEN` /
  `VFS_READ` / `VFS_WRITE` / `VFS_CLOSE`, `FS_CREATE`, `FS_TRUNC`) is exercised only by the Rust
  `init` probes, and `/dev/null` and `/dev/zero` are likewise unreachable from C.
- `hello` needs no malloc — `mmap` and `brk` are link stubs. The first non-`hello` C program on disk
  needs both malloc *and* file I/O.

**Scope**, after chunk 2 and before any C beyond `hello`:

- `openat` / `read` / `close` / `lseek`-shaped wrappers onto VFS. The fd numbering already matches:
  VFS `NR_FDS = 8`, with 0/1/2 pre-opened to the console.
- Real `brk` / `mmap` / `munmap` onto VM — widening USER's `ipc_to` to include VM, **with the
  reverse reply edge**; the 5.4 lesson is that each entry costs a *pair* of bits.
- Re-verify musl's `__init_tls` and arena assumptions against the new heap origin.
- **D8 applies.** Any new request number or payload field is an ABI bump touching both repos, and
  the fork branch is force-pushed — so the fork bump and the `external/musl` submodule bump land in
  the **same** PR. See [`abi.md`](../conventions/abi.md).

**Proof:** a C program that opens a file on the MinixFS root, reads it, and prints its contents —
through musl, not through an `init` probe.

## Chunk 6: SDK flavor CI coverage

`MINIXRS_SDK` appears **zero** times in `.github/workflows/ci.yml`, so the SDK `hello` flavor is
never exercised on a runner and a patched-clang-driver regression ships green. Markers are
byte-identical across the SDK and musl flavors, which is what makes this invisible.

**Scope:** an SDK-cached CI job, or an explicit decision to keep the gap and rely on the local
three-boot matrix. The rule and the full local mitigation live in
[`ci.md`](../conventions/ci.md#the-sdk-flavor-has-zero-ci-coverage) — this file does not restate
them.

---

## Sequencing: keep the ramdisk until virtio-blk is green

D3's contract is that **Phase 6 swaps virtio-blk under an unchanged MFS.** Until blk passes the same
BDEV self-check battery MFS already runs — now including the **write** path (`fs.write ok n=32768
v=32768`, create/truncate, the `/etc/holey` hole probe) — keep packing `rootfs` and booting the MEM
backend. Dual-backend selection (boot arg, DS name, or a compile-time feature) beats a flag day that
blacks out every FS marker at once. The bar rose with 5.10: a read-only virtio backend is **no
longer** marker-equivalent to MEM.

## Capacity: what is already raised, and what is not

| Limit                                    | Value             | Note                                                                                                              |
| ---------------------------------------- | ----------------- | ----------------------------------------------------------------------------------------------------------------- |
| `NR_SERVED_PROCS`                        | 32                | The shared PM/VM/SCHED ceiling — keep it shared                                                                   |
| VM `MAX_REGIONS`                         | 16                | Already raised from the Phase-4 "4" scare                                                                         |
| VFS `NR_FDS`                             | 8                 | Raised 4 → 8 in 5.8; VFS-local, not ABI                                                                           |
| `USER_STACK_BYTES`                       | 64 KiB            | Raised 4 KiB → 64 KiB in chunk 2; MFS's two block buffers became `main` locals, VFS's 256 KiB stage stayed `.bss` |
| `ROOTFS_IMAGE_BLOCKS` / `ROOTFS_NINODES` | 256 (1 MiB) / 128 | Fine for the ramdisk; a disk root must not inherit either as a format limit                                       |
| qemu-smoke budget                        | 600 s             | Raised 120 → 240 → 600 across 5.10a/5.10b; disk I/O under TCG will push again                                     |

Before raising the boot budget again, measure the way [`ci.md`](../conventions/ci.md) prescribes —
the last required marker's byte position as a fraction of a fixed-timeout log, compared against the
same number at the merge base — and think in the *ratio*, not the wall-clock seconds.

## Carried from Phase 5: two unproven write-back orderings

5.10a and 5.10b left two write-back orderings **correct but unproven by any boot marker**. Probe
them in whatever slice adds `lseek` or a second truncate consumer; the detail is in
[`phase-5-musl-fs.md`](phase-5-musl-fs.md).

## Explicit non-goals for the pre-6 window

- Do **not** remove the MXBI `rootfs` ramdisk on day one of Phase 6.
- Do **not** couple "first virtio boot" to "full Unix VA map" in one PR — land chunk 2 first, so a
  disk bring-up failure is not also a layout failure.
- Do **not** write inside `$MINIXRS_SDK` when adjusting image-base or stack assumptions: tooling's
  `build-musl.sh` still does `rm -rf $SDK/sysroot`.
- Do **not** reopen the D8 ABI freeze for convenience. `0x700..0xC00` has been **fully allocated**
  (PM / VFS / FS / BDEV / CDEV) since 5.8, so a Phase 6 band needs a home outside that span.

## Deferred: do not block Phase 6 on these

| Item                               | Why it can wait                                           |
| ---------------------------------- | --------------------------------------------------------- |
| `SENDA` + `trap_mask: u32`         | musl uses SENDREC; the README already qualifies the claim |
| `SYS_TIMES`                        | Nice for `time(1)`; not needed for a disk boot            |
| CoW fork                           | Performance, not correctness                              |
| PFS / pipes                        | Phase 7 shell                                             |
| Indirect grants                    | No re-granting consumer yet                               |
| Signal dispositions / `SIGCHLD`    | Shell / job control                                       |
| Orphan reparent to init            | Multi-process userland                                    |
| `GETDENTS` / `stat` on the FS band | No consumer until a shell lists a directory               |
| Dynamic linking                    | Far future                                                |
| x86_64                             | Phase 8                                                   |

---

## One-line summary

Phase 5 and its three stretch slices delivered a read-write POSIX data path and device nodes on a
ramdisk — but only to Rust callers. Before Phase 6 grows device windows and multi-MiB disk programs,
replace the temporary low-stack / fixed-heap map with a high stack, a larger stack, and an
image-relative brk; then slice IRQCTL → virtio transport → blk-under-BDEV → real disk image, keeping
the ramdisk until the new backend is marker-green, and widen the musl syscall surface so C can reach
the filesystem that already works.

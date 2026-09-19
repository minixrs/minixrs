# Pre-Phase-6 chunk 2 — the user VA map: high stack, larger stack, image-relative brk

**Date:** 2026-09-19 **Status:** design, pending review **Branch:** `feature/user-va-map`
**Predecessor:** pre-Phase-6 chunk 1 (checkbox status convention, PR #58, merged 2026-09-17)
**Tracker:** [`docs/plans/phase-6-prep.md`](../../plans/phase-6-prep.md) chunk 2

Phase 5 left a greenfield *low* user VA map: a one-page stack at 2 MiB that doubles as a 1 MiB
ceiling on every image, a heap origin fixed at 16 MiB regardless of where the image ended, and an
mmap arena at a constant 32 MiB. Phase 6 makes all three worse — virtio drivers want descriptor
rings and bounce buffers that do not fit in one page, and a disk root invites multi-MiB programs
that the stack's VA blocks outright. This slice replaces the map before any virtio code is written,
which is why the tracker orders it first.

Decisions are labelled `V1…V13`: slice-local, distinct from the phase-level `D1…D13`, which stay
locked.

---

## 1. What exists, and what the slice changes

| Component                             | Today                                                         | After this slice                                                                                                              |
| ------------------------------------- | ------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `kernel-shared/src/uspace.rs`         | `SERVER_STACK_BYTES = 4096`; stack VA unpublished             | `USER_STACK_TOP`, `USER_STACK_BYTES = 64 KiB`, `USER_STACK_BASE`, `USER_STACK_GUARD_BYTES`, `USER_REGION_LIMIT` all published |
| `kernel/src/arch/aarch64/userland.rs` | private `SERVER_STACK_VA = 0x0020_0000`; one mapped page      | derives the range from `uspace`; maps 16 pages; the device-window asserts invert                                              |
| `kernel/src/boot_image/elf.rs`        | `LoadedElf { entry, phdr_va, phnum, phentsize }`              | `+ image_end: u64`                                                                                                            |
| `kernel/src/system/do_exec.rs`        | reply carries no payload                                      | reply carries `image_end` at `EXEC_IMAGE_END_OFF` (`40..48`)                                                                  |
| `kernel-shared/src/callnr.rs`         | VM band is `VM_RQ_BASE + 0..=4`                               | `+ VM_EXEC = VM_RQ_BASE + 5`; `+ EXEC_IMAGE_END_OFF = 40`                                                                     |
| `servers/pm/src/main.rs`              | exec ends at `SYS_EXEC`                                       | `+ VM_EXEC` leg after a successful `SYS_EXEC`                                                                                 |
| `servers/vm/src/region.rs`            | fixed `HEAP_BASE` / `MMAP_BASE`; `Kind::{Unused, Heap, Mmap}` | per-proc origins seeded by `VM_EXEC`; `+ Kind::Stack`; `REGION_LIMIT` drops below the stack                                   |
| `fs/mfs/src/lib.rs`                   | `const _` tripwire: block buffer cannot be a local            | tripwire re-aimed; `BLOCK` and `STAGE` become locals                                                                          |
| `userland/`                           | `coreutils`, `hello`, `init`, `sh`, `worker`                  | `+ bigprog` — the VA-ceiling regression fixture                                                                               |
| `tools/gen-c-headers`                 | emits `VM_RQ_BASE + 0..=4`                                    | `+ VM_EXEC` row                                                                                                               |
| tooling repo (`~/src/tooling`)        | `check-image.sh` hardcodes a one-page stack at `0x200000`     | constants re-synced; the clang `--image-base` pin dropped                                                                     |

---

## 2. The new map

### V1 — the stack goes to the top of usable process VA

```
0x0010_0000   image base (user.ld; SDK images move to lld's default 0x200000 — see V11)
    ...       PT_LOAD segments, growing up
0x????_????   heap, origin = page_align_up(image_end)                        (V4)
    ...       mmap arena, origin = heap_origin + MMAP_GAP                    (V5)
------------------------------------------------ USER_REGION_LIMIT = 0x3FFE_F000
0x3FFE_F000   guard page, never mapped                                       (V3)
0x3FFF_0000   USER_STACK_BASE — 16 stack pages, growing down
0x4000_0000   USER_STACK_TOP == USER_DEVICE_WINDOW_BASE
0x4000_0000   device window (TTY UART; Phase 6 virtio-mmio pages)
0x8000_0000   ramdisk window
```

`USER_STACK_TOP` is defined as `USER_DEVICE_WINDOW_BASE` rather than as an independent number. That
is the whole argument for this placement: the lowest kernel-owned window already *is* the ceiling on
everything a process may touch, so putting the stack immediately beneath it makes "top of the stack"
and "top of the process" the same address by construction, and no new ordering invariant is
introduced. It is also cheap — the stack shares L1 slot 0 with the image, so an address space costs
exactly one extra L3 frame, not the L1+L2+L3 chain a stack near `USER_VA_TOP` would need.

The alternatives were weighed and rejected: a stack near `USER_VA_TOP` (0x1_0000_0000_0000) buys
maximum separation for ~12 KiB of page tables per address space and leaves the kernel windows
stranded in the middle of the map; a stack above the ramdisk window would make a *process* VA sit
among kernel-owned windows, which is exactly the distinction `uspace.rs`'s
`the_window_clears_every_occupied_user_va` test exists to keep clean.

### V2 — 64 KiB of stack, and the constant is renamed

`SERVER_STACK_BYTES` becomes `USER_STACK_BYTES = 0x1_0000` (16 pages). The rename is not cosmetic:
the constant now governs exec'd user images as well as boot servers, and the old name would keep
implying the Phase-4 world where only servers had a stack at all. `uspace` constants are
deliberately not emitted into the generated C headers, so the rename costs nothing at the ABI.

64 KiB was chosen against two neighbours. 16 KiB is the minimum that clears musl's `%Lf` VLA (~7.4
KiB), but leaves no room to revisit MFS's buffers. 256 KiB would let VFS's `EXEC_STAGE` become a
local too, at 8 MiB of eagerly-mapped RAM across the 32-proc ceiling, and would effectively retire
the stack budget as something a server author thinks about — which is the discipline the constant
exists to enforce. 64 KiB clears the VLA with wide margin, makes MFS's two 4 KiB buffers viable
locals again, and costs at most 2 MiB across `NR_SERVED_PROCS`.

**The stack is eagerly mapped**, as today: all 16 pages are allocated and mapped at image load.
Guard-page *growth* is out of scope (§7), so there is no lazy stack fault path to write.

### V3 — one guard page below the stack

`USER_STACK_GUARD_BYTES = USER_PAGE_SIZE`, never mapped, and `REGION_LIMIT` is placed below it. It
is free — it consumes a page of VA in a 1 GiB span, no frame, and no code beyond the constant — and
it converts stack overflow from "silently walk into the mmap arena" into a fault VM reports as
out-of-region. Today a 4 KiB local in a server with a 4 KiB stack produces exactly that silent walk;
`fs/mfs`'s tripwire comment names the failure mode and notes it "prints nothing
`tests/qemu-boot.forbidden` catches".

### V4 — brk starts after the last `PT_LOAD`

`LoadedElf` gains `image_end`: `page_align_up(max(p_vaddr + p_memsz))` over the `PT_LOAD` segments,
computed in the load loop that already walks them. `ExecImage` carries it out of
`userland::load_exec_image`, and `do_exec` writes it into the `SYS_EXEC` reply at
`EXEC_IMAGE_END_OFF = 40` (`40..48`, u64).

That offset is **reply-only** and deliberately past `EXEC_LEN_OFF` (`32..40`), the last request
field, so it aliases nothing a caller wrote: a handler that reads a request field after the reply
has been composed reads its own value, not a repurposed one. It is 8-byte aligned, like every other
u64 payload field in the band.

`region::HEAP_BASE` stops being the origin for exec'd images and becomes the **legacy** origin (V6).

### V5 — the mmap arena becomes relative too

`MMAP_BASE` is `HEAP_BASE + 16 MiB` today (`0x0100_0000` → `0x0200_0000`). The gap is preserved as a
named `MMAP_GAP = 0x0100_0000` and applied to the per-proc heap origin, so the heap keeps the same
16 MiB of room to grow before it could reach the arena. Both remain capped at `REGION_LIMIT`, which
now excludes the stack and its guard page — so the runtime `ENOMEM` checks `set_brk` and `mmap`
already carry need no new logic, only the new bound.

---

## 3. Teaching VM about exec

### V6 — `VM_EXEC`, and the gap it closes

**VM has never been told that an exec happened.** There is no `VM_EXEC`; PM's exec path ends at
`SYS_EXEC`. So an exec'd process keeps whatever heap and mmap regions its *previous* image
accumulated, pointing into an address space the kernel has already torn down. Nothing exploits this
today — init's exec battery execs fresh procs — but an image-relative heap origin cannot be recorded
anywhere without a message that says "this proc has a new image", and leaving the stale regions in
place while adding a second, relative origin would make the inconsistency load-bearing.

```
EL0  --PM_EXEC-->            PM
PM   --VFS_EXEC_STAGE-->     VFS        (path → staged bytes + grant)
PM   --SYS_EXEC-->           kernel
                             reply: OK, payload[40..48] = image_end
PM   --VM_EXEC(proc, image_end)-->  VM
                             VM: drop this proc's regions;
                                 heap_origin = image_end
                                 mmap_origin = image_end + MMAP_GAP
                                 record Kind::Stack                  (V8)
kernel resumes the target at _start
```

`VM_EXEC = VM_RQ_BASE + 5 = 0xC05`. Payload: target endpoint (`0..4`, i32), `image_end` (`8..16`,
u64 — `4..8` is left as padding so the u64 lands 8-byte aligned, as `EXEC_LEN_OFF` and the BDEV/CDEV
offset fields already do). Reply `m_type = OK`, or `EINVAL` for an endpoint outside the served proc
range — mirroring `VM_FORK`, which is the closest existing shape.

### V7 — where `VM_EXEC` sits in PM's exec, and what happens if it fails

`SYS_EXEC` is the point of no return: on success the kernel has already installed the new address
space and resumed the target. `VM_EXEC` therefore runs **after** it, and its failure cannot roll
anything back. PM treats a failed `VM_EXEC` as a diagnostic, not an error: it emits a `[pm]` trace
line and continues, leaving the proc running with VM's stale view — strictly no worse than today's
unconditional behaviour. This is deliberate and is the reason `VM_EXEC` does not move earlier: doing
the VM bookkeeping *before* `SYS_EXEC` would mean unwinding it on every one of the eight failures
init's `exec_denials` battery fires.

### V8 — `Kind::Stack`

`region::Kind` gains `Stack`, recorded at the same moment as the heap origin — on `VM_EXEC` for an
exec'd image, and at boot for the servers (§4). **The kernel keeps installing the mapping**; VM only
records the range. Because every stack page is already mapped when the record is made, no fault ever
resolves against a `Kind::Stack` region, so the fault path's behaviour is unchanged and the region
is pure bookkeeping.

What it buys: `VM_FORK` clones it, so a forked child's region set describes its whole address space
rather than omitting the one range the kernel installed behind VM's back; and a fault just below the
stack hits the guard page (V3) and is reported against a known neighbour instead of as an anonymous
out-of-region SIGSEGV.

`set_brk` and `mmap` must skip `Kind::Stack` when scanning for a free slot or a matching region, and
`munmap` must refuse it. `MAX_REGIONS` is 16 and a proc uses at most heap + stack + mmaps, so no
resizing is needed.

### V9 — boot servers and stub D keep a legacy origin

Boot servers are loaded by `load_boot_server`, not by exec, and never call `brk`. Stub D (under the
`boot-stubs` feature) does `VM_BRK` and touches `HEAP_BASE` by convention baked into `user_stub.S`'s
blob.

So `HEAP_BASE` and `MMAP_BASE` survive in `region.rs` as the legacy origins: `set_brk` and `mmap`
use a proc's **recorded** origin when `VM_EXEC` has supplied one, and the legacy constant otherwise.
Stub D is untouched, and nothing in `user_stub.S` needs rewriting. The `const _` asserts pinning
both legacy constants below `REGION_LIMIT` stay — and now assert against a *lower* bound, so they
are doing more work than before, not less.

Boot servers do get their `Kind::Stack` region: the kernel cannot send `VM_EXEC` (it is a server
request, and the kernel does not originate those outside `VM_PAGEFAULT`), so VM seeds the stack
region for the boot procs from its own `sef_init`, where it already knows the boot proc set and the
stack range is a compile-time constant from `uspace`.

---

## 4. The invariants, and rewriting the guards that encode them

`userland.rs:172`'s `USER_DEVICE_WINDOW_BASE > SERVER_STACK_VA + PAGE_SIZE` is the guard the prep
doc warns about: a high stack **inverts** it, and deleting it would drop the only compile-time check
that the map is self-consistent. It is rewritten, not removed. The full set after this slice:

| Invariant                                                          | Where                       |
| ------------------------------------------------------------------ | --------------------------- |
| `USER_STACK_TOP == USER_DEVICE_WINDOW_BASE`                        | `uspace.rs`                 |
| `USER_STACK_BASE == USER_STACK_TOP - USER_STACK_BYTES`             | `uspace.rs`                 |
| `USER_STACK_BYTES` is a non-zero multiple of `USER_PAGE_SIZE`      | `uspace.rs`                 |
| `USER_STACK_BASE` is page-aligned and 16-byte-aligned for `sp`     | `uspace.rs`                 |
| `USER_REGION_LIMIT == USER_STACK_BASE - USER_STACK_GUARD_BYTES`    | `uspace.rs`                 |
| `REGION_LIMIT` is `USER_REGION_LIMIT` (a re-export, asserted)      | `region.rs`                 |
| `HEAP_BASE < REGION_LIMIT`, `MMAP_BASE < REGION_LIMIT` (legacy)    | `region.rs`                 |
| `MMAP_GAP > 0` and the legacy pair still differ by it              | `region.rs`                 |
| every stub VA `+ PAGE_SIZE <= USER_REGION_LIMIT` (was: `< window`) | `userland.rs`, `boot-stubs` |
| `USER_DEVICE_WINDOW_BASE + SIZE <= RAMDISK_WINDOW_BASE`            | `uspace.rs` (unchanged)     |

`uspace.rs`'s `the_window_clears_every_occupied_user_va` test enumerates the occupied process VAs
and asserts each is below the device window. Its `SERVER_STACK_VA` row (`0x0020_0000`) goes away —
the stack is no longer below the window, it is flush against it — and `HEAP_BASE` / `MMAP_BASE`
become the legacy stub rows they now are. The test's bound changes from `USER_DEVICE_WINDOW_BASE` to
`USER_REGION_LIMIT`, which is the address the enumeration was always really about.

**`REGION_LIMIT` moves to `uspace`** to make that possible: `region.rs` cannot be referenced from
`kernel-shared`, so the bound the map is really about was unavailable to the very module that
documents the map. `uspace::USER_REGION_LIMIT` becomes the definition and `region::REGION_LIMIT` a
re-export with a `const _` pinning them equal — which also gives the tooling repo a single named
constant to mirror instead of re-deriving the arithmetic.

`a_server_stack_is_exactly_one_page` is replaced by a test asserting the 16-page geometry and that
the range is flush beneath the window. `boot_image/elf.rs:141`'s comment referencing
`SERVER_STACK_VA` is updated to the new name.

### V10 — `fs/mfs`'s tripwire fires, deliberately

`fs/mfs/src/lib.rs:110` asserts `MFS_BLOCK_SIZE >= SERVER_STACK_BYTES`, written "the way round that
fires when the stack **grows**: at that point a local becomes plausible again, and the reasoning
above deserves to be re-read rather than silently outlived." This slice is that moment. Acting on
it: MFS's `BLOCK` and 5.10b's `STAGE` — two 4 KiB `.bss` statics behind a capability token — become
locals in the frames that use them, the `Blocks` token machinery goes with them, and the tripwire is
re-aimed at the condition that would make that wrong again (a block buffer that no longer fits
comfortably in a frame). VFS's 256 KiB `EXEC_STAGE` stays in `.bss`; 64 KiB of stack does not change
the answer for a buffer that size, and `main.rs:167`'s comment is updated to say so against the new
number rather than the old one.

---

## 5. Tooling (separate session; exact plan produced here)

Per the cross-repo rule these are **not** edited from the minixrs session. This design carries the
exact change list; a separate session in `~/src/tooling` implements it with its own signing.

| File                        | Change                                                                                                 |
| --------------------------- | ------------------------------------------------------------------------------------------------------ |
| `verify/check-image.sh:59`  | `STACK_VA`/`STACK_PAGES` → `USER_STACK_BASE=$((0x3FFF0000))`, `STACK_PAGES=16`                         |
| `verify/check-image.sh:179` | the overlap rule becomes `vend > USER_STACK_BASE`; it subsumes the separate `DEVICE_WINDOW_BASE` check |
| `verify/check-driver.sh:94` | the `--image-base=0x100000` assertion is removed                                                       |
| LLVM patch 0006             | the unconditional `--image-base=0x100000` pin is dropped (V11)                                         |
| `verify/selftest.sh:142`    | the `image-base-1m` fixture inverts — it stops being the pass sentinel                                 |

### V11 — the clang `--image-base` pin is dropped

Patch 0006 forces `--image-base=0x100000` for one reason: lld's aarch64 default is `0x200000`, which
was the stack. The prep doc calls this "the clang pin compensating for the OS", and once the stack
moves the compensation has no subject. SDK-built images link at lld's default; repo-built images
keep `user.ld`'s explicit `0x100000`. Two different load bases are fine — each image has its own
TTBR0, which is the same reasoning that already lets every server share one base.

`check-image.sh` continues to assert what actually matters (segments clear of the stack, headers
covered by a `PT_LOAD`, the brand present) and stops asserting a particular base.

### V12 — ordering is load-bearing

```
1. minixrs PR lands           — the stack leaves 0x200000; both bases become safe
2. tooling PR lands + SDK rebuild — the pin is dropped; SDK images move to 0x200000
3. minixrs three-boot matrix re-run against the rebuilt SDK
```

Dropping the pin before step 1 would link SDK images straight onto the stack page. The minixrs
change is safe in isolation because it makes `0x200000` an ordinary image address — the SDK flavor
keeps building at `0x100000` until step 2, and passes either way. `$MINIXRS_SDK` is never written to
(`build-musl.sh` still does `rm -rf $SDK/sysroot`).

---

## 6. Proof

Every claim below is a command whose output is read before the claim is made.

| Claim                             | Proof                                                                                                    |
| --------------------------------- | -------------------------------------------------------------------------------------------------------- |
| the image ceiling is gone         | `userland/bigprog` loads and runs — see below                                                            |
| the stack is high                 | `[exec] … sp=0x3fff` in the boot log, with the expectation **tightened** past its current `sp=0x` prefix |
| brk is image-relative             | `[diag vm] exec` marker (below) + `region.rs` host unit tests over a recorded origin                     |
| regions survive exec correctly    | the same marker reports how many stale regions it dropped; host tests cover the drop                     |
| MFS still works with stack locals | the existing `fs.*` marker battery, unchanged                                                            |
| the tooling checker is green      | `check-image.sh` over every built image, after step 2 of V12                                             |
| no flavour regressed              | the **mandatory** three-boot matrix (SDK, forced musl, moved-aside sysroot + `MINIXRS_SDK=/nonexistent`) |

### V13 — the brk proof is VM-side, because USER cannot reach VM

`kernel/src/proc/table.rs:394` defines `USER_IPC_TO = [PM_PROC_NR, VFS_PROC_NR]`. The shared USER
privilege has **no `ipc_to` bit for VM**, so neither init nor any exec'd program can send `VM_BRK` —
an end-to-end "a program called brk and got an image-relative answer" proof is not available in this
slice, and opening that edge is chunk 5's work (it costs a *pair* of bits, the 5.4 lesson).

Stating that plainly rather than shipping a proof that cannot run. What is proved instead:

- **A `[diag vm]` marker on every `VM_EXEC`**, carrying the origin VM derived and how many stale
  regions it dropped:

  ```
  [diag vm] exec nr=18 image_end=0x302000 heap=0x302000 mmap=0x1302000 dropped=2
  ```

  `heap` equal to `image_end` and *unequal* to the legacy `0x0100_0000` is the assertable claim.
  `dropped` is non-zero on a re-exec of a proc that had regions, which is the stale-region gap (V6)
  becoming observable for the first time.
- **Host unit tests in `servers/vm/src/region.rs`**, which already carries a `#[cfg(test)]` module
  and runs on the host: `set_brk` below a recorded origin is `EINVAL`, a first `set_brk` creates the
  heap at the recorded origin rather than `HEAP_BASE`, a proc with no recorded origin still gets
  `HEAP_BASE`, `exec` drops prior regions, and the mmap arena bumps from `origin + MMAP_GAP`.

The end-to-end proof is chunk 5's first deliverable and should be written there, against this
marker.

### `userland/bigprog`

A new userland crate whose `.bss` is ~2 MiB, so `p_vaddr + p_memsz` crosses `0x200000` and ends near
`0x300000`. It touches its first and last page and prints a marker.

The buffer is `.bss` rather than `.rodata` on purpose: `p_filesz` stays kilobytes, so the 1 MiB
rootfs (`ROOTFS_IMAGE_BLOCKS = 256`) and the 600 s boot budget are both untouched, while the loader
maps `ceil(p_memsz / PAGE_SIZE)` pages either way — so the VA collision the fixture exists to prove
is byte-identical to a multi-MiB `.rodata` image. Under today's map it fails `AlreadyMapped` →
`ENOEXEC`. **The tradeoff, stated rather than discovered:** this proves the *VA* ceiling, not
multi-MiB file I/O through MFS. The file path is already proven by 5.10a's 32 KiB write, and sizing
the rootfs for a multi-MiB file is the disk-root question Phase 6 slice 6.4 owns.

Re-mutation-test the image-base and oversized-image fixtures, per
[`testing-and-markers.md`](../../conventions/testing-and-markers.md).

### Boot-timing

A stack that is 16 pages instead of 1 costs 15 extra frame allocations and mappings per address
space, and `bigprog` adds ~512 more at its exec. Measure the way [`ci.md`](../../conventions/ci.md)
prescribes — the last required marker's byte position as a fraction of a fixed-timeout log, against
the same number at the merge base — and think in the ratio. The 600 s budget is not expected to
move; if it does, the raise needs the evidence `ci.md` demands, not a wall-clock anecdote.

---

## 7. Out of scope

- **CoW fork** — performance, not correctness; deferred in the prep doc's table.
- **ASLR** — nothing depends on a fixed base today, and a randomised one would defeat the
  marker-based proofs this project runs on.
- __Guard-page stack *growth*__ — V3 reserves the page; growing into it needs a lazy stack fault
  path, which is a slice of its own.
- **USER→VM for malloc** — the prep doc's chunk 5. It "can follow immediately once the map is sane",
  and widening USER's `ipc_to` costs a *pair* of bits (the 5.4 lesson), which deserves its own PR.
- **Raising `ROOTFS_IMAGE_BLOCKS`** — see the `bigprog` note; it belongs with the disk root.

---

## 8. ABI

`VM_EXEC` and `EXEC_IMAGE_END_OFF` are **additive**. `VM_RQ_BASE = 0xC00` is outside the fully
allocated `0x700..0xC00` span, and `tools/gen-c-headers/src/callnr_h.rs:576` already anticipates "a
`VM_RQ_BASE + 5` request" in its guard-name logic. No existing number, layout, endpoint or errno
changes, so no consumer is broken.

It is still a generated-header change, so [`abi.md`](../../conventions/abi.md) applies in full: the
`gen-c-headers` regen and the `external/musl` submodule bump land in the **same** PR. musl itself
calls nothing in the VM band, so the fork's own sources are untouched — only the vendored header
moves.

`uspace.rs`'s constants, including the renamed `USER_STACK_BYTES`, are not emitted to C and are
therefore not ABI.

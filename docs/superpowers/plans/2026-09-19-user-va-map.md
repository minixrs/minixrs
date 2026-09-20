# User VA Map Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Phase 5's temporary low user VA map with a high 64 KiB stack, a guard page, and an
image-relative heap and mmap arena, before any virtio code depends on the old layout.

**Architecture:** `kernel-shared::uspace` becomes the single definition of the whole process VA map
— stack range, guard page, and the region ceiling — and every other crate derives from it. The ELF
loader starts reporting where an image ends; `SYS_EXEC` returns that to PM; PM forwards it to VM in
a new `VM_EXEC` message, which also closes a gap nobody had noticed: VM has never been told that an
exec happened, so exec'd processes keep the regions of an image whose address space is already gone.

**Tech Stack:** Rust (`no_std` kernel + EL0 servers), aarch64, QEMU, `dprint` for markdown, bash for
the tooling repo's verifiers.

**Spec:** [`../specs/2026-09-19-user-va-map-design.md`](../specs/2026-09-19-user-va-map-design.md) —
decisions `V1…V13`. Read it before Task 1; every task below argues from it.

## Global Constraints

- **SPDX header** on every new `.rs`/`.S` file, before any other content — see
  [`rust-style.md`](../../conventions/rust-style.md) for the exact two forms.
- **Every commit `--signoff`ed and GPG-signed.** Never `--no-gpg-sign`, never `--no-verify`.
- **Never commit to `main`.** This plan's branch is `feature/user-va-map`, already created.
- **Do not push, open a PR, or trigger CI.** Commit freely; stop at the push and surface the branch.
- **Markdown wraps at 100 columns**; run `~/.dprint/bin/dprint fmt` and `python3
  tools/check-md-links.py` on anything you edit. Both block in CI.
- **The kernel is never host-built.** `cargo check`/`clippy`/`test` cross-compile it; there is no
  `#[cfg(test)]` under `kernel/src/`. QEMU is the verification path for kernel behaviour.
- **Verify before claiming.** Run the command, read its output, *then* say it passes.
- **Do not write inside `$MINIXRS_SDK`.** Task 12 produces a change plan for the tooling repo; it
  does not edit it.
- **A `match` that could be `?` trips clippy too** (`question_mark`), once a function returns
  `Result`. Task 8's prescribed `exec_from_fs` body hit this. When a task changes a return type from
  a bare code to a `Result`, re-check every `match` in that function.
- **All-constant `assert!` trips clippy.** CI runs `cargo clippy --workspace --all-targets -- -D
  warnings` (`.github/workflows/ci.yml:91`), and `assertions_on_constants` fires on a runtime
  `assert!` whose operands are all constants. **Several tasks below prescribe exactly that pattern
  in their test code — that is a defect in the plan, not in the repo.** Use the idiom the repo
  already uses (`kernel-shared/src/callnr.rs:1553`, `uspace.rs`'s window tests): bind a local, then
  `assert_eq!(a.min(b), a, "msg")`. A `const _: () = assert!(...)` at item scope is unaffected —
  only runtime `assert!` inside a `#[test]` fn.
- **`cargo gen-c-headers --stdout`**, with no `--` separator: the cargo alias already ends in `--`.
- **The generator emits absolute hex** — `#define VM_EXEC 0xC05`, not `(VM_RQ_BASE + 5)`.
- **Run `cargo fmt -p <crate> -- --check` before reporting done.** It blocks in CI, and rustfmt
  collapses multi-line `assert_eq!` calls that fit in 100 columns — several appear expanded below.
- Exact values from the spec, copied verbatim:
  - `USER_STACK_TOP = USER_DEVICE_WINDOW_BASE = 0x4000_0000`
  - `USER_STACK_BYTES = 0x1_0000` (64 KiB, 16 pages)
  - `USER_STACK_BASE = 0x3FFF_0000`
  - `USER_STACK_GUARD_BYTES = USER_PAGE_SIZE = 0x1000`
  - `USER_REGION_LIMIT = 0x3FFE_F000`
  - `MMAP_GAP = 0x0100_0000` (16 MiB)
  - `VM_EXEC = VM_RQ_BASE + 5 = 0xC05`
  - `EXEC_IMAGE_END_OFF = 40` (reply-only, `40..48`, u64)

---

## Execution Waves

Tasks are grouped by the **crate** they build, not merely by the files they touch. File-level
disjointness is not enough: `cargo test -p <crate>` builds the whole lib-test binary, so two agents
in one crate compile each other's half-finished work. Wave A learned this the hard way — Tasks 1 and
4 edit different files of `minixrs-kernel-shared`, and one task's TDD red phase made the other's
green phase fail to compile at all.

**Grouping by `-p` target is also not enough**, which Wave B then learned. `kernel/Cargo.toml`
build-depends on `minixrs-mkfs-mfs` → `minixrs-mfs`, and `kernel/build.rs` shells out to build every
server ELF (`servers/vm`, `servers/vfs`, `servers/pm`, …). So `cargo kernel-aarch64` is **not** a
kernel-only build: it transitively builds most of the workspace. Worse, `kernel/build.rs:815` scrubs
`RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS` but **not** `RUSTC_WORKSPACE_WRAPPER`/`CLIPPY_ARGS`, so `cargo
clippy -p minixrs-kernel -- -D warnings` runs clippy with `-D warnings` over those nested server
builds too — and fails on a neighbouring task's not-yet-called code.

The practical rule: **a task that touches `minixrs-kernel` cannot run beside a task that touches any
crate the kernel's build script builds.** In this plan that means Wave C is sequential.

| Wave | Tasks                | Files, and why they are disjoint                                                                                                                   |
| ---- | -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| A    | ~~1 ∥ 4~~ **serial** | Both are `minixrs-kernel-shared`. **Not parallelizable** — see the note above.                                                                     |
| B    | **2+3 ∥ 6 ∥ 9**      | `minixrs-kernel` ∥ `minixrs-vm` ∥ `minixrs-mfs` + `minixrs-vfs` — three separate `-p` targets                                                      |
| C    | **5 → 7 → 8 → 10**   | **Sequential.** Task 5 is `minixrs-kernel`, whose build script builds `minixrs-vm`, `minixrs-pm` and `minixrs-mfs` — the other three tasks' crates |
| D    | 11 → 12 → 13         | serial: 11 and 12 need a booting system; 13 describes what shipped                                                                                 |

**Agents in a shared worktree must not commit concurrently** — `git index.lock` races. Either the
dispatcher commits each agent's work as it returns, or each agent gets its own worktree.

Tasks 1-8 deliberately leave `cargo check --workspace` **failing**: the `SERVER_STACK_BYTES` rename
is not finished until Task 9. Each task verifies its own package instead. The workspace goes green
at Task 9, and that is a checkpoint, not a regression.

---

## File Structure

| File                                         | Responsibility after this plan                                               |
| -------------------------------------------- | ---------------------------------------------------------------------------- |
| `kernel-shared/src/uspace.rs`                | **The** definition of the process VA map: stack range, guard, region ceiling |
| `kernel-shared/src/callnr.rs`                | `VM_EXEC`, its payload offsets, `EXEC_IMAGE_END_OFF`                         |
| `kernel/src/boot_image/elf.rs`               | reports `image_end` alongside entry / phdr                                   |
| `kernel/src/arch/aarch64/userland.rs`        | maps the 16-page stack at the shared VA; holds the collision asserts         |
| `kernel/src/system/do_exec.rs`               | returns `image_end` in the `SYS_EXEC` reply                                  |
| `servers/vm/src/region.rs`                   | per-proc heap/mmap origins, `Kind::Stack`, `exec()`; re-exports the ceiling  |
| `servers/vm/src/main.rs`                     | `VM_EXEC` handler, the `[diag vm] exec` marker, boot-proc stack seeding      |
| `servers/pm/src/main.rs`                     | threads `image_end` out of `SYS_EXEC` and into `VM_EXEC`                     |
| `fs/mfs/src/lib.rs`, `fs/mfs/src/main.rs`    | block/stage buffers become locals; tripwire re-aimed                         |
| `userland/bigprog/`                          | the VA-ceiling regression fixture (~2 MiB `.bss`)                            |
| `tools/gen-c-headers/src/callnr_h.rs`        | emits the `VM_EXEC` row                                                      |
| `docs/plans/phase-6-prep.md`, `docs/plan.md` | chunk 2 checked; tooling change plan recorded                                |

---

## Task 1: The VA map constants

**Files:**

- Modify: `kernel-shared/src/uspace.rs` (constants, module doc table, `const _` asserts, tests)

**Interfaces:**

- Consumes: `crate::message::{USER_PAGE_SIZE, USER_VA_TOP}`, `USER_DEVICE_WINDOW_BASE` (all already
  in this module)
- Produces: `pub const USER_STACK_TOP, USER_STACK_BYTES, USER_STACK_BASE, USER_STACK_GUARD_BYTES,
  USER_REGION_LIMIT: u64`. **Removes** `SERVER_STACK_BYTES` (renamed to `USER_STACK_BYTES`) — Tasks
  2, 6 and 9 consume the new name.

- [ ] **Step 1: Write the failing tests**

Replace the `a_server_stack_is_exactly_one_page` test in `kernel-shared/src/uspace.rs`'s `tests`
module with these, and add them to that module:

```rust
    #[test]
    fn the_stack_is_sixteen_pages_flush_beneath_the_device_window() {
        // The stack top *is* the lowest kernel-owned window, so "top of stack"
        // and "top of process" are the same address by construction (V1).
        assert_eq!(USER_STACK_TOP, USER_DEVICE_WINDOW_BASE);
        assert_eq!(USER_STACK_TOP, 0x4000_0000);
        assert_eq!(USER_STACK_BYTES, 64 * 1024);
        assert_eq!(USER_STACK_BYTES / USER_PAGE_SIZE, 16);
        assert_eq!(USER_STACK_BASE, 0x3FFF_0000);
        assert_eq!(USER_STACK_BASE, USER_STACK_TOP - USER_STACK_BYTES);
        // `sp` starts at the top, and AArch64 requires a 16-byte-aligned SP.
        assert_eq!(USER_STACK_TOP % 16, 0);
        assert_eq!(USER_STACK_BASE % USER_PAGE_SIZE, 0);
    }

    #[test]
    fn the_guard_page_sits_below_the_stack_and_bounds_the_regions() {
        // The guard page is never mapped; `USER_REGION_LIMIT` is below it, so a
        // heap or mmap that reached its cap still cannot touch the stack (V3).
        assert_eq!(USER_STACK_GUARD_BYTES, USER_PAGE_SIZE);
        assert_eq!(USER_REGION_LIMIT, 0x3FFE_F000);
        assert_eq!(
            USER_REGION_LIMIT,
            USER_STACK_BASE - USER_STACK_GUARD_BYTES
        );
        assert_eq!(USER_REGION_LIMIT % USER_PAGE_SIZE, 0);
    }

    #[test]
    fn the_stack_clears_both_kernel_windows() {
        // The stack is a *process* VA, so it must stay wholly below every
        // kernel-owned window. (`min` rather than `<`: an all-constant
        // `assert!` trips clippy's `assertions_on_constants`.)
        assert_eq!(
            USER_STACK_TOP.min(USER_DEVICE_WINDOW_BASE),
            USER_STACK_TOP,
            "the stack runs into the device window"
        );
        assert_eq!(
            USER_STACK_TOP.min(RAMDISK_WINDOW_BASE),
            USER_STACK_TOP,
            "the stack runs into the ramdisk window"
        );
    }
```

And rewrite the existing `the_window_clears_every_occupied_user_va` test body — its
`SERVER_STACK_VA` row is gone (the stack is no longer *below* the window, it is flush against it),
and its bound moves to `USER_REGION_LIMIT`, which is the address the enumeration was always about:

```rust
#[test]
fn the_region_limit_clears_every_occupied_user_va() {
    // Mirrors the table in the module docs. These are the VAs a *process*
    // occupies below its stack; all must sit under `USER_REGION_LIMIT`, the
    // ceiling `servers/vm/src/region.rs` enforces on every growing region.
    //
    // The stack itself gets no entry: it is not below the limit, it is what
    // the limit is derived *from*. The kernel windows get none either —
    // they are kernel-owned, and their separation is transitive through
    // `the_kernel_windows_are_disjoint_and_ascending`.
    for occupied in [
        0x0010_0000_u64, // server / init / worker ELF base
        0x0020_0000,     // SDK image base once the clang pin is dropped (V11)
        0x0043_0000,     // stub D code
        0x0083_0000,     // stub D stack
        0x0100_0000,     // region::HEAP_BASE (legacy origin, stub D)
        0x0200_0000,     // region::MMAP_BASE (legacy origin, stub D)
    ] {
        assert!(
            occupied < USER_REGION_LIMIT,
            "{occupied:#x} collides with the region limit"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p minixrs-kernel-shared uspace` Expected: FAIL — `cannot find value USER_STACK_TOP
in this scope` and siblings.

- [ ] **Step 3: Add the constants and their asserts**

In `kernel-shared/src/uspace.rs`, **delete** the `SERVER_STACK_BYTES` constant and its doc comment,
and add in its place:

```rust
/// Top of the initial user stack: the highest VA a process may name, and the
/// value the kernel points `SP_EL0` at before `eret`.
///
/// Defined *as* [`USER_DEVICE_WINDOW_BASE`] rather than as an independent
/// number, and that is the whole argument for the placement: the lowest
/// kernel-owned window already is the ceiling on everything a process may
/// touch, so putting the stack immediately beneath it makes "top of the stack"
/// and "top of the process" the same address by construction. No new ordering
/// invariant is introduced, and none can drift.
///
/// It is also the cheap placement. The stack shares L1 slot 0 with the image,
/// so an address space costs exactly one extra L3 frame — not the L1+L2+L3
/// chain a stack near [`USER_VA_TOP`] would need in every address space.
pub const USER_STACK_TOP: u64 = USER_DEVICE_WINDOW_BASE;

/// Bytes of stack the kernel gives a process: 16 pages.
///
/// Every page is **eagerly mapped** at image load — there is no lazy stack
/// fault path, and guard-page *growth* is deliberately out of scope — so this
/// is real RAM per process, at most 2 MiB across the `NR_SERVED_PROCS` ceiling.
///
/// Why 16 and not 1: musl's `%Lf` VLAs are ~7.4 KiB, which was a landmine under
/// the old single page, and `fs/mfs` carried two 4 KiB `.bss` buffers purely
/// because a one-page stack could not hold them. Why not 64: at 256 KiB a
/// server author stops thinking about the stack budget at all, which is the
/// discipline this constant exists to enforce.
///
/// Published — rather than kept private like the VA used to be — because a
/// server has to know how much frame it can spend: it is what decides whether a
/// buffer may be a local at all. `fs/mfs` carries the `const _` tripwire that
/// fires when this grows enough to make a local plausible again.
pub const USER_STACK_BYTES: u64 = 0x1_0000;

/// Lowest mapped stack VA. The kernel maps
/// `[USER_STACK_BASE, USER_STACK_TOP)` and points `SP_EL0` at the top.
///
/// Published so the tooling repo's `verify/check-image.sh` can assert that no
/// `PT_LOAD` reaches it, instead of re-deriving the arithmetic from a comment.
pub const USER_STACK_BASE: u64 = USER_STACK_TOP - USER_STACK_BYTES;

/// One page below the stack that is **never mapped**.
///
/// Free — a page of VA in a 1 GiB span, no frame, no code beyond this constant
/// and [`USER_REGION_LIMIT`] — and it converts stack overflow from "silently
/// walk into the mmap arena" into a fault VM reports as out-of-region. Under
/// the old 4 KiB stack that silent walk was real: `fs/mfs`'s tripwire comment
/// names it, and notes that it "prints nothing `tests/qemu-boot.forbidden`
/// catches".
///
/// Reserved, not grown into: a lazily-growing stack needs a fault path this
/// slice does not write.
pub const USER_STACK_GUARD_BYTES: u64 = USER_PAGE_SIZE;

/// Exclusive upper bound of every VM-tracked region — the heap and the mmap
/// arena — placed below the guard page.
///
/// This lives here, rather than in `servers/vm/src/region.rs` where it used to,
/// for two reasons. `kernel-shared` cannot reference a server crate, so the
/// bound the map is really about was unavailable to the very module that
/// documents the map. And the tooling repo needs one named constant to mirror
/// rather than the arithmetic that produces it.
///
/// `region::REGION_LIMIT` is a re-export of this, pinned by a `const _` there.
pub const USER_REGION_LIMIT: u64 = USER_STACK_BASE - USER_STACK_GUARD_BYTES;
```

Then add these `const _` asserts next to the existing window asserts:

```rust
// The stack's geometry. A whole number of pages, page-aligned at both ends, and
// 16-byte-aligned at the top because that is where `SP_EL0` starts and AArch64
// faults on a misaligned SP.
const _: () = assert!(USER_STACK_BYTES > 0);
const _: () = assert!(USER_STACK_BYTES.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(USER_STACK_BASE.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(USER_STACK_TOP.is_multiple_of(16));
const _: () = assert!(USER_STACK_BASE == USER_STACK_TOP - USER_STACK_BYTES);

// The guard page and the region ceiling. `USER_REGION_LIMIT` must sit strictly
// below the stack with the guard page between, or a heap grown to its cap would
// be adjacent to the stack and an overflow would land in it silently.
const _: () = assert!(USER_STACK_GUARD_BYTES.is_multiple_of(USER_PAGE_SIZE));
const _: () = assert!(USER_REGION_LIMIT == USER_STACK_BASE - USER_STACK_GUARD_BYTES);
const _: () = assert!(USER_REGION_LIMIT < USER_STACK_BASE);

// The stack is a *process* VA: it must stay wholly below every kernel-owned
// window. Flush against the device window is allowed (the ranges are half-open);
// overlapping it is not.
const _: () = assert!(USER_STACK_TOP <= USER_DEVICE_WINDOW_BASE);
const _: () = assert!(USER_STACK_TOP <= RAMDISK_WINDOW_BASE);
```

- [ ] **Step 4: Update the module doc's VA table**

In the `## The device window` section of the module doc, replace the table rows for the stack, heap
and mmap arena so the doc matches the code:

```rust
//! | VA | What |
//! |---|---|
//! | `0x0010_0000` | server / init / worker ELF images (`user.ld` base) |
//! | `0x0020_0000` | SDK-built image base (lld's aarch64 default, once the clang pin is dropped) |
//! | `0x0040_0000` / `0x0080_0000` | demo stub code / stack pages |
//! | *image end* | VM's per-process heap origin (`VM_EXEC` records it) |
//! | *+16 MiB* | VM's per-process anonymous-mmap arena |
//! | `0x3FFE_F000` | [the region ceiling](USER_REGION_LIMIT) — heap and mmap stop here |
//! | `0x3FFF_0000` | [the initial stack](USER_STACK_BASE), 16 pages, growing down |
//! | `0x4000_0000` | **the device window**, and [the stack's top](USER_STACK_TOP) |
//! | `0x8000_0000` | [the ramdisk window](RAMDISK_WINDOW_BASE) (slice 5.7) |
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p minixrs-kernel-shared uspace` Expected: PASS, including the three new tests and
the renamed `the_region_limit_clears_every_occupied_user_va`.

- [ ] **Step 6: Commit**

```bash
git add kernel-shared/src/uspace.rs
git commit --signoff -m "kernel-shared: publish the new user VA map

The stack moves to the top of usable process VA, flush beneath the device
window, and grows from one page to sixteen. A guard page sits below it and
USER_REGION_LIMIT below that, so a heap or mmap at its cap still cannot
reach the stack.

USER_REGION_LIMIT moves here from servers/vm/src/region.rs: kernel-shared
cannot reference a server crate, so the bound the map is really about was
unavailable to the module that documents the map -- and the tooling repo
needs one named constant to mirror rather than the arithmetic behind it.

SERVER_STACK_BYTES is renamed USER_STACK_BYTES: it now governs exec'd user
images too, not just boot servers. uspace constants are not emitted to the
generated C headers, so the rename is not an ABI change.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

Expect this commit to leave the workspace **not building** — Tasks 2, 6 and 9 still name
`SERVER_STACK_BYTES`. That is intended; the rename is finished in Task 9.

---

## Task 2: Map the 16-page stack, and invert the collision asserts

> **Execution note — Tasks 2 and 3 run as ONE agent, in this order.** Both edit
> `kernel/src/arch/aarch64/userland.rs` *and* `kernel/src/boot_image/elf.rs`, so they cannot be
> dispatched concurrently. They stay two tasks, and two commits, because the changes are logically
> separate and the history is worth keeping — but one agent does both, committing between them.

**Files:**

- Modify: `kernel/src/arch/aarch64/userland.rs:149` (the `SERVER_STACK_VA` constant and its
  asserts), `:511` (the stack mapping), `:536` (`sp_top`)
- Modify: `kernel/src/boot_image/elf.rs:141` (a comment naming `SERVER_STACK_VA`)

**Interfaces:**

- Consumes: `uspace::{USER_STACK_BASE, USER_STACK_BYTES, USER_STACK_TOP, USER_REGION_LIMIT}` from
  Task 1
- Produces: nothing new to other tasks; `ExecImage::sp_top` keeps its type and meaning

- [ ] **Step 1: Replace the private stack VA with the shared range**

In `kernel/src/arch/aarch64/userland.rs`, delete the `SERVER_STACK_VA` constant (line 149) and the
`SERVER_STACK_BYTES == PAGE_SIZE` assert below it, and put in their place:

```rust
/// Pages of stack the kernel maps for every image. Derived from the shared
/// [`USER_STACK_BYTES`] so the count and the size cannot drift.
const USER_STACK_PAGES: usize = (USER_STACK_BYTES / PAGE_SIZE as u64) as usize;

// The stack VA is no longer this file's to choose. It used to be private —
// each server reaches its stack through `SP_EL0`, never through a constant — but
// the tooling repo's image checker has to know where it is in order to refuse an
// image that would collide with it, and it was duplicating the number from a
// comment. `uspace` owns the whole range now; this file only maps it.
const _: () = assert!(USER_STACK_BYTES.is_multiple_of(PAGE_SIZE as u64));
const _: () = assert!(USER_STACK_PAGES > 0);
```

Add `USER_STACK_BASE`, `USER_STACK_BYTES`, `USER_STACK_TOP` and `USER_REGION_LIMIT` to the `uspace`
import list at line 46.

- [ ] **Step 2: Rewrite the collision asserts**

Replace the whole `----- The device window must clear every VA declared above -----` assert block
(lines ~157-183) with this. The old guard compared each VA against the device window; a high stack
**inverts** that relation, so the comparison moves to `USER_REGION_LIMIT` — the address that is now
genuinely above everything a process places by hand.

```rust
// ----- Every VA declared here must clear the region ceiling ------------------
//
// `kernel-shared::uspace` documents the whole user VA map and const-asserts the
// stack's and the windows' geometry, but the *stub* VAs are declared here. So
// the collision checks live here, where a slice that adds a stub trips them.
//
// These used to read `USER_DEVICE_WINDOW_BASE > <va>`. The stack moving to the
// top of process VA inverts that: the device window is no longer the first thing
// above these addresses — the region ceiling is, and it is strictly lower. So the
// bound is `USER_REGION_LIMIT`, and each assert is strictly stronger than the one
// it replaces. Do not weaken them back to the window: a stub placed between
// `USER_REGION_LIMIT` and `USER_DEVICE_WINDOW_BASE` would land in the guard page
// or the stack and pass a window-based check.

#[cfg(feature = "boot-stubs")]
const _: () = assert!(USER_STACK_VA_A + PAGE_SIZE as u64 <= USER_REGION_LIMIT);
#[cfg(feature = "boot-stubs")]
const _: () = assert!(USER_STACK_VA_B + PAGE_SIZE as u64 <= USER_REGION_LIMIT);
#[cfg(feature = "boot-stubs")]
const _: () = assert!(USER_STACK_VA_C + PAGE_SIZE as u64 <= USER_REGION_LIMIT);
#[cfg(feature = "boot-stubs")]
const _: () = assert!(USER_STACK_VA_D + PAGE_SIZE as u64 <= USER_REGION_LIMIT);
#[cfg(feature = "boot-stubs")]
const _: () = assert!(USER_CODE_VA_D + PAGE_SIZE as u64 <= USER_REGION_LIMIT);
```

- [ ] **Step 3: Map all 16 pages**

In `load_exec_image`, replace the single-frame stack block (around line 505-520) with a loop. Note
the error handling: a partially-mapped stack must free only the frame that did not get linked, since
the leaf sweep will find the ones that did.

```rust
// Stack: 16 zeroed RW pages, mapped eagerly; SP starts at the top.
// `alloc_frame` zeroes, so the stack arrives clean without an explicit memset.
for page in 0..USER_STACK_PAGES {
    let va = USER_STACK_BASE + (page * PAGE_SIZE) as u64;
    let stack_frame = match alloc_frame() {
        Some(f) => f,
        None => {
            destroy_addrspace_with_leaves(aspace);
            return Err(ENOMEM);
        }
    };
    if let Err(e) = aspace.map_page(va, stack_frame.addr(), Prot::RW_DATA) {
        // `map_page` failed before linking this leaf, so free the orphan
        // frame explicitly; the sweep below sees only the ones that linked.
        free_frame(stack_frame);
        destroy_addrspace_with_leaves(aspace);
        // Since exec-from-FS the image is an *input*, so `AlreadyMapped`
        // here means the file's own segments cover a stack VA — that is
        // `ENOEXEC` ("not an image this kernel will run"), not `ENOMEM`
        // ("out of frames"). Only a genuine allocator failure is the latter.
        return Err(elf_errno(ElfError::Map(e)));
    }
}
```

And set `sp_top` from the shared constant:

```rust
sp_top: USER_STACK_TOP,
```

- [ ] **Step 4: Update the two stale doc references**

In `load_exec_image`'s doc comment, replace "map one zeroed RW stack page at [`SERVER_STACK_VA`]"
with "map [`USER_STACK_PAGES`] zeroed RW stack pages at [`USER_STACK_BASE`]", and replace "The stack
VA is shared with every boot server because each image gets its own TTBR0, so the same low VA
resolves to a distinct frame per proc." with:

```rust
/// The stack VA is shared by every image because each gets its own TTBR0, so the
/// same VA resolves to a distinct set of frames per proc — the same reasoning
/// that lets every `user.ld` share one load base.
```

In `load_boot_server`'s TLB-maintenance comment (around line 629), change "the same reasoning the
`SERVER_STACK_VA` mapping above already relies on" to "the same reasoning the stack mapping above
already relies on".

In `kernel/src/boot_image/elf.rs:141`, change "running four times past the segment's end into the
next one (or into `SERVER_STACK_VA`)" to "…(or into the stack range)".

- [ ] **Step 5: Verify it cross-compiles and lints clean**

Run: `cargo kernel-aarch64` Expected: the kernel builds. `servers/`, `fs/` and `userland/` still
fail on `SERVER_STACK_BYTES` until Task 9 — build the kernel package alone here, not `--workspace`.

Run: `cargo clippy -p minixrs-kernel --target aarch64-unknown-none --release` Expected: no warnings.

Run: `cargo clippy -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features`
Expected: no warnings. This is the arm that proves the `boot-stubs`-gated asserts still compile when
the feature is *off*, which the default build cannot show.

- [ ] **Step 6: Commit**

```bash
git add kernel/src/arch/aarch64/userland.rs kernel/src/boot_image/elf.rs
git commit --signoff -m "kernel: map the 16-page stack at the shared high VA

The stack VA stops being private to this file and comes from uspace: the
tooling repo's image checker has to know where it is to refuse a colliding
image, and it was duplicating the number out of a comment.

The device-window collision asserts are rewritten, not deleted. They read
USER_DEVICE_WINDOW_BASE > <va>, and a high stack inverts that relation --
the window is no longer the first thing above a stub VA, the region ceiling
is, and it is strictly lower. Each assert is now strictly stronger.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: The loader reports where an image ends

> **Execution note — same agent as Task 2** (shared files; see Task 2's note).

**Files:**

- Modify: `kernel/src/boot_image/elf.rs:228-243` (`LoadedElf`), `:261-313` (`load_into`)
- Modify: `kernel/src/arch/aarch64/userland.rs:430-444` (`ExecImage`), `load_exec_image`'s
  constructor

**Interfaces:**

- Consumes: nothing from earlier tasks
- Produces: `LoadedElf::image_end: u64` and `ExecImage::image_end: u64` — page-aligned exclusive end
  of the highest `PT_LOAD`. Task 5 reads `ExecImage::image_end`.

- [ ] **Step 1: Add the field to `LoadedElf`**

In `kernel/src/boot_image/elf.rs`, add to `pub struct LoadedElf`:

```rust
/// Page-aligned exclusive end of the highest `PT_LOAD` segment — the first
/// VA above the image that no segment claims.
///
/// This is where a process's heap begins (`VM_EXEC` carries it to VM, which
/// seeds the heap region there). Before this slice VM used a fixed
/// `HEAP_BASE` unrelated to the image, which worked only because nothing
/// had an image big enough to reach it.
///
/// Zero is impossible for a loadable image: `load_into` rejects an ELF with
/// no `PT_LOAD`, and a segment is only counted after it has mapped.
pub image_end: u64,
```

- [ ] **Step 2: Compute it in the load loop**

In `load_into`, add the accumulator before the phdr loop:

```rust
let mut image_end: u64 = 0;
```

and, **after** the `load_segment(…)?` call inside the loop (order matters — see the comment):

```rust
// Tracked *after* a successful `load_segment`, which is what makes the
// `saturating_add` honest rather than lazy: the segment has already been
// mapped, so its whole span passed `check_va` and cannot exceed
// `USER_VA_TOP`. An overflow here is unreachable, and saturating is the
// right answer for an unreachable case that must not panic in a kernel.
let seg_end = p_vaddr.saturating_add(p_memsz as u64);
if seg_end > image_end {
    image_end = seg_end;
}
```

and page-align at the return:

```rust
Ok(LoadedElf {
    entry: eh.entry,
    phdr_va,
    phnum: eh.phnum as u16,
    phentsize: eh.phentsize as u16,
    // Page-aligned up: the heap starts on a page boundary, and the loader
    // has already mapped whole pages for a segment whose `p_memsz` ends
    // mid-page, so the partial page belongs to the image, not the heap.
    image_end: (image_end + minixrs_kernel_shared::message::USER_PAGE_SIZE - 1)
        & !(minixrs_kernel_shared::message::USER_PAGE_SIZE - 1),
})
```

- [ ] **Step 3: Carry it through `ExecImage`**

In `kernel/src/arch/aarch64/userland.rs`, add to `pub(crate) struct ExecImage`:

```rust
/// Page-aligned first VA above the loaded image. `do_exec` returns it to PM,
/// which forwards it to VM as the process's heap origin.
pub image_end: u64,
```

and in `load_exec_image`'s `Ok(ExecImage { … })`:

```rust
image_end: loaded.image_end,
```

- [ ] **Step 4: Verify it cross-compiles**

Run: `cargo kernel-aarch64` Expected: builds, with **no warnings**. `ExecImage::image_end` has no
reader until Task 5, so it draws `field is never read`, and CI's `cargo clippy --workspace
--all-targets -- -D warnings` turns that into a failure for every task in between. **Add
`#[allow(dead_code)]` on the field with a `// Task 5 consumes this.` comment — this is mandatory,
not conditional.** Task 5 removes it. Same forward-declaration pattern `rust-style.md` prescribes,
and the one Task 6 uses for `region::exec` / `region::record_stack`.

Run: `cargo clippy -p minixrs-kernel --target aarch64-unknown-none --release` Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add kernel/src/boot_image/elf.rs kernel/src/arch/aarch64/userland.rs
git commit --signoff -m "kernel: report where a loaded image ends

The ELF loader already walks every PT_LOAD; it now records the highest one's
page-aligned end, which is where the process's heap should begin. VM has
been using a fixed HEAP_BASE unrelated to the image, which worked only
because nothing had an image large enough to reach it.

Tracked after load_segment succeeds, so the span has already passed
check_va and the saturating_add covers an unreachable case rather than
papering over a reachable one.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: `VM_EXEC` and the reply offset

**Files:**

- Modify: `kernel-shared/src/callnr.rs` (the VM request band, ~line 1258; the exec offsets near
  `EXEC_LEN_OFF`)
- Modify: `tools/gen-c-headers/src/callnr_h.rs:148-155` (the `VM_RQ_BASE` block)

**Interfaces:**

- Consumes: `VM_RQ_BASE` (existing)
- Produces: `pub const VM_EXEC: i32`, `pub const VM_EXEC_PROC_OFF: usize = 0`, `pub const
  VM_EXEC_IMAGE_END_OFF: usize = 8`, `pub const EXEC_IMAGE_END_OFF: usize = 40`. Tasks 5, 7 and 8
  consume these.

- [ ] **Step 1: Write the failing tests**

Add to `kernel-shared/src/callnr.rs`'s `tests` module:

```rust
    #[test]
    fn vm_exec_is_the_sixth_vm_request() {
        assert_eq!(VM_EXEC, VM_RQ_BASE + 5);
        assert_ne!(VM_EXEC, VM_FORK);
        assert_ne!(VM_EXEC, VM_MUNMAP);
        assert_ne!(VM_EXEC, VM_MMAP);
        assert_ne!(VM_EXEC, VM_BRK);
        assert_ne!(VM_EXEC, VM_PAGEFAULT);
        assert!(VM_EXEC > KERNEL_CALL + NR_KERN_CALLS as i32);
        assert_ne!(VM_EXEC, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn the_vm_exec_payload_fields_do_not_overlap() {
        // proc endpoint is 4 wide (i32); image_end is 8 (u64) and 8-byte
        // aligned, so 4..8 is padding — the same shape EXEC_LEN_OFF uses.
        assert_eq!(VM_EXEC_PROC_OFF, 0);
        assert_eq!(VM_EXEC_IMAGE_END_OFF, 8);
        assert!(VM_EXEC_PROC_OFF + 4 <= VM_EXEC_IMAGE_END_OFF);
        assert!(VM_EXEC_IMAGE_END_OFF + 8 <= 96);
        assert_eq!(VM_EXEC_IMAGE_END_OFF % 8, 0);
    }

    #[test]
    fn exec_image_end_is_reply_only_and_aliases_no_request_field() {
        // The request's last field is EXEC_LEN_OFF (32..40). A reply-only field
        // placed past it cannot alias anything a caller wrote, so a handler that
        // reads a request field after composing the reply reads its own value.
        assert_eq!(EXEC_IMAGE_END_OFF, 40);
        assert!(EXEC_LEN_OFF + 8 <= EXEC_IMAGE_END_OFF);
        assert!(EXEC_IMAGE_END_OFF + 8 <= 96);
        assert_eq!(EXEC_IMAGE_END_OFF % 8, 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p minixrs-kernel-shared callnr` Expected: FAIL — `cannot find value VM_EXEC in
this scope` and siblings.

- [ ] **Step 3: Add the constants**

In `kernel-shared/src/callnr.rs`, after `VM_FORK`:

```rust
/// PM → VM: a process has exec'd; reset its memory bookkeeping around the new
/// image. Payload: target endpoint ([`VM_EXEC_PROC_OFF`], `0..4`, i32) and the
/// loader's page-aligned image end ([`VM_EXEC_IMAGE_END_OFF`], `8..16`, u64).
/// Reply `m_type = OK`, or `EINVAL` if the endpoint maps to an out-of-range proc
/// number.
///
/// VM drops every region the proc's *previous* image accumulated and records
/// `image_end` as the origin for its heap, with the mmap arena a fixed gap
/// above. Before this request existed VM was never told an exec had happened at
/// all, so an exec'd process kept regions describing an address space the kernel
/// had already torn down — benign only because nothing yet asked VM about a
/// proc that had exec'd twice.
///
/// Sent **after** `SYS_EXEC`, which is the point of no return, so a failure here
/// cannot roll anything back and PM treats it as a diagnostic rather than an
/// error. Doing the bookkeeping first would mean unwinding it on every failed
/// exec, and `init`'s denial battery fires eight of those every boot.
pub const VM_EXEC: i32 = VM_RQ_BASE + 5;

/// `VM_EXEC` payload: target endpoint (i32).
pub const VM_EXEC_PROC_OFF: usize = 0;

/// `VM_EXEC` payload: the loader's page-aligned image end (u64).
///
/// `4..8` is left as padding so the u64 lands 8-byte aligned, matching
/// [`EXEC_LEN_OFF`] and the BDEV/CDEV offset fields.
pub const VM_EXEC_IMAGE_END_OFF: usize = 8;
```

and, next to the other `EXEC_*` offsets:

```rust
/// `SYS_EXEC` **reply**: the loader's page-aligned image end (u64, `40..48`).
///
/// Reply-only, and deliberately past [`EXEC_LEN_OFF`] (`32..40`) — the last
/// request field — so it aliases nothing the caller wrote. PM forwards the value
/// to VM as [`VM_EXEC_IMAGE_END_OFF`].
pub const EXEC_IMAGE_END_OFF: usize = 40;
```

Add the matching `const _` asserts beside the existing payload-layout asserts:

```rust
const _: () = assert!(VM_EXEC_PROC_OFF + 4 <= VM_EXEC_IMAGE_END_OFF);
const _: () = assert!(VM_EXEC_IMAGE_END_OFF + 8 <= 96);
const _: () = assert!(EXEC_LEN_OFF + 8 <= EXEC_IMAGE_END_OFF);
const _: () = assert!(EXEC_IMAGE_END_OFF + 8 <= 96);
```

- [ ] **Step 4: Emit the C header row**

In `tools/gen-c-headers/src/callnr_h.rs`, add to the `VM_RQ_BASE` block's member list (after the
`VM_FORK` entry, matching the surrounding style exactly):

```rust
("VM_EXEC", callnr::VM_EXEC),
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p minixrs-kernel-shared callnr` Expected: PASS.

Run: `cargo test -p minixrs-gen-c-headers` Expected: PASS. If a guard test asserts the VM band's
highest member, update it to `VM_EXEC` — the generator's own comment at `callnr_h.rs:576` already
anticipates "a `VM_RQ_BASE + 5` request".

Run: `cargo gen-c-headers -- --stdout | grep VM_EXEC` Expected: one line defining `VM_EXEC` as
`(VM_RQ_BASE + 5)`.

- [ ] **Step 6: Settle the `external/musl` submodule question, and record the answer**

[`abi.md`](../../conventions/abi.md) says a fork rebase and the `external/musl` submodule bump must
land in the **same PR** as any ABI change here, because the fork's port branch is force-pushed and
this repo would otherwise pin an orphaned commit.

Check whether the fork actually needs anything:

```bash
ls external/musl/src/minixrs/
grep -rn "VM_RQ_BASE\|VM_BRK\|VM_FORK\|VM_EXEC" external/musl/ || echo "no VM band references"
```

Expected: `_syscall.c`, `ipc.c`, `minixrs_internal.h`, and **no** VM-band references — the fork
vendors no generated header (they are built into `target/` and installed into the sysroot), and musl
calls nothing in the VM band. So this addition needs **no fork rebase and no submodule bump**.

Record that conclusion in the PR description explicitly. The rule exists to stop an orphaned pin;
silently skipping it looks identical to forgetting it, which is why the answer gets written down
rather than merely acted on. If the grep *does* hit, stop — that is a fork rebase, and it lands in
this same PR.

- [ ] **Step 7: Commit**

```bash
git add kernel-shared/src/callnr.rs tools/gen-c-headers/src/callnr_h.rs
git commit --signoff -m "abi: add VM_EXEC and the SYS_EXEC image-end reply field

Both additive. VM_RQ_BASE (0xC00) is outside the fully allocated
0x700..0xC00 span, and gen-c-headers already anticipates a VM_RQ_BASE + 5
request in its guard-name logic, so no existing number, layout, endpoint or
errno changes and no consumer breaks.

EXEC_IMAGE_END_OFF sits past EXEC_LEN_OFF, the last request field, so a
reply-only value cannot alias anything the caller wrote.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 5: `SYS_EXEC` returns the image end

**Files:**

- Modify: `kernel/src/system/do_exec.rs` (the success tail, around line 305)

**Interfaces:**

- Consumes: `ExecImage::image_end` (Task 3), `EXEC_IMAGE_END_OFF` (Task 4)
- Produces: the `SYS_EXEC` reply's `40..48` field. Task 8 reads it.

- [ ] **Step 1: Add a `u64` payload writer**

`do_exec.rs` has `read_i32` but no writer. Add beside it:

```rust
/// Write a native-endian `u64` into the message payload at `off`.
///
/// Mirrors `read_i32`'s convention. The caller is responsible for `off + 8 <=
/// 96`; every call site uses a `const` offset that `callnr.rs` const-asserts.
fn write_u64(msg: &mut Message, off: usize, v: u64) {
    msg.payload[off..off + 8].copy_from_slice(&v.to_ne_bytes());
}
```

- [ ] **Step 2: Write the field on the success path**

In `do_exec`, immediately before the final `OK`, after the trace block:

```rust
// The reply's one payload field. PM forwards it to VM as the exec'd
// process's heap origin (`VM_EXEC`); nothing in the kernel reads it back.
//
// Written after the trace rather than before it so the point-of-no-return
// sequence above stays one uninterrupted block — and it must be written on
// *every* success, not only on a traced one, which is exactly the kind of
// thing a sampled trace hides.
write_u64(msg, EXEC_IMAGE_END_OFF, img.image_end);
OK
```

Add `EXEC_IMAGE_END_OFF` to the `callnr` import list at the top of the file.

- [ ] **Step 3: Extend the trace line**

In the `[ksys SYS_EXEC]` trace, append `image_end` so the value is observable from the kernel side
too — Task 7's `[diag vm]` marker reports what VM *received*, and these two agreeing is what proves
the value survived the hop:

```rust
"[ksys SYS_EXEC] target={target_nr} name={name} src={src_name} entry={:#x} old_asid={old_asid} new_asid={} freed={freed} granter={granter_e} len={image_len} image_end={:#x}",
img.entry,
img.asid,
img.image_end,
```

Note the argument order: `{:#x}` placeholders consume positional arguments in order, so
`img.image_end` goes last, after `img.asid`.

- [ ] **Step 4: Verify it cross-compiles**

Run: `cargo kernel-aarch64` Expected: builds. **Delete the `#[allow(dead_code)]` and its `// Task 5
consumes this.` comment from `ExecImage::image_end`** — this task is its consumer, and a
forward-declaration allow left behind silences a real warning for the rest of the repo's life.

Run: `cargo clippy -p minixrs-kernel --target aarch64-unknown-none --release` Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add kernel/src/system/do_exec.rs
git commit --signoff -m "kernel: return the image end from SYS_EXEC

Written on every success rather than inside the sampled trace block, which
is the kind of thing a trace-gated write hides until the hundredth exec.

The [ksys SYS_EXEC] line also reports it, so the kernel's value and the one
VM receives can be compared in a boot log.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: Per-process region origins, and `Kind::Stack`

**Files:**

- Modify: `servers/vm/src/region.rs` (constants, `Kind`, `ClientRegions`, `set_brk`, `mmap`, the
  public API, tests)

**Interfaces:**

- Consumes: `uspace::{USER_REGION_LIMIT, USER_STACK_BASE, USER_STACK_TOP}` (Task 1)
- Produces: `pub fn exec(nr: i32, image_end: u64) -> Result<usize, i32>` (returns the number of
  regions dropped), `pub fn record_stack(nr: i32) -> Result<(), i32>`, `pub const MMAP_GAP: u64`,
  `Kind::Stack`. Task 7 calls `exec` and `record_stack`.

- [ ] **Step 1: Write the failing tests**

Add to `servers/vm/src/region.rs`'s `tests` module:

```rust
    #[test]
    fn a_fresh_client_still_uses_the_legacy_origins() {
        // Stub D and the boot servers never get a VM_EXEC, so the fixed origins
        // stay their default. This is the arm that keeps user_stub.S working.
        let mut c = ClientRegions::EMPTY;
        let brk = c.set_brk(HEAP_BASE + 0x1000).unwrap();
        assert_eq!(brk, HEAP_BASE + 0x1000);
        assert!(c.contains(HEAP_BASE));
    }

    #[test]
    fn exec_moves_the_heap_origin_to_the_image_end() {
        let mut c = ClientRegions::EMPTY;
        let image_end = 0x0030_2000;
        c.exec(image_end);
        // The first brk creates the heap at the recorded origin, not HEAP_BASE.
        let brk = c.set_brk(image_end + 0x1000).unwrap();
        assert_eq!(brk, image_end + 0x1000);
        assert!(c.contains(image_end));
        assert!(!c.contains(HEAP_BASE));
    }

    #[test]
    fn a_brk_below_the_recorded_origin_is_einval() {
        let mut c = ClientRegions::EMPTY;
        let image_end = 0x0030_2000;
        c.exec(image_end);
        assert_eq!(c.set_brk(image_end - 1), Err(EINVAL));
        // ...including an address that would have been valid under the legacy
        // origin, which is the regression this guards.
        assert_eq!(c.set_brk(HEAP_BASE + 0x1000), Err(EINVAL));
    }

    #[test]
    fn exec_drops_the_previous_images_regions() {
        let mut c = ClientRegions::EMPTY;
        c.set_brk(HEAP_BASE + 0x4000).unwrap();
        let old_mmap = c.mmap(0x2000).unwrap();
        assert!(c.contains(old_mmap));

        let dropped = c.exec(0x0030_2000);
        assert_eq!(dropped, 2, "heap + one mmap should have been dropped");
        assert!(!c.contains(old_mmap));
        assert!(!c.contains(HEAP_BASE));
    }

    #[test]
    fn the_mmap_arena_bumps_from_the_recorded_origin_plus_the_gap() {
        let mut c = ClientRegions::EMPTY;
        let image_end = 0x0030_2000;
        c.exec(image_end);
        let base = c.mmap(0x1000).unwrap();
        assert_eq!(base, image_end + MMAP_GAP);
        // The legacy pair still differ by exactly the same gap, so the constant
        // describes both worlds rather than only the new one.
        assert_eq!(MMAP_BASE, HEAP_BASE + MMAP_GAP);
    }

    #[test]
    fn a_recorded_stack_is_contained_but_is_not_a_heap_or_an_mmap() {
        let mut c = ClientRegions::EMPTY;
        c.record_stack();
        assert!(c.contains(USER_STACK_BASE));
        assert!(c.contains(USER_STACK_TOP - 1));
        assert!(!c.contains(USER_STACK_TOP), "the range is half-open");
        // The guard page below the stack must NOT be covered, or an overflow
        // would be silently resolved instead of faulting.
        assert!(!c.contains(USER_STACK_BASE - 1));
        // brk must not mistake it for a heap, and munmap must refuse it.
        assert_eq!(c.munmap(USER_STACK_BASE, 0x1000), Err(EINVAL));
    }

    #[test]
    fn the_region_ceiling_excludes_the_stack_and_its_guard_page() {
        let mut c = ClientRegions::EMPTY;
        // A brk that would reach the guard page is refused.
        assert_eq!(c.set_brk(REGION_LIMIT + 1), Err(ENOMEM));
        assert_eq!(REGION_LIMIT, USER_REGION_LIMIT);
        assert!(REGION_LIMIT < USER_STACK_BASE);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p minixrs-vm region` Expected: FAIL — `no method named exec found`, `cannot find
value MMAP_GAP`, and siblings.

- [ ] **Step 3: Re-point the ceiling and add the gap**

In `servers/vm/src/region.rs`, change the import to bring in the new constants and replace the
`REGION_LIMIT` definition:

```rust
use minixrs_kernel_shared::uspace::{USER_REGION_LIMIT, USER_STACK_BASE, USER_STACK_TOP};
```

```rust
/// Exclusive upper bound of **every** tracked region.
///
/// A re-export of [`USER_REGION_LIMIT`], which is where the user VA map is
/// defined. It used to be defined here as the base of the lowest kernel-owned
/// window; the stack moving to the top of process VA made that wrong — the first
/// thing above a growing region is now the stack's guard page, not a window.
///
/// The definition moved to `kernel-shared` rather than merely changing value,
/// because `kernel-shared::uspace` documents the whole map and could not name
/// the bound the map is really about: a shared crate cannot reference a server.
pub const REGION_LIMIT: u64 = USER_REGION_LIMIT;

/// Distance from a process's heap origin to its mmap arena origin.
///
/// Extracted from the legacy pair rather than invented: `MMAP_BASE - HEAP_BASE`
/// has been 16 MiB since slice 3.6, and applying the same gap to a per-process
/// origin keeps the heap exactly as much room to grow as it has always had.
pub const MMAP_GAP: u64 = 0x0100_0000;

// The gap describes the legacy pair too, or it would be a number that happens to
// work for new processes and silently disagrees with stub D's layout.
const _: () = assert!(MMAP_BASE == HEAP_BASE + MMAP_GAP);

// The legacy origins must still sit below the bound, which is now *lower* than
// it was — so these asserts do more work than before, not less.
const _: () = assert!(MMAP_BASE < REGION_LIMIT);
const _: () = assert!(HEAP_BASE < REGION_LIMIT);

// The ceiling must clear the stack and its guard page. This is the assert that
// replaces the old `REGION_LIMIT <= RAMDISK_WINDOW_BASE`: the windows are no
// longer what bounds a region, the stack is.
const _: () = assert!(REGION_LIMIT < USER_STACK_BASE);
```

Delete the now-unused `RAMDISK_WINDOW_BASE` / `USER_DEVICE_WINDOW_BASE` import and the `REGION_LIMIT
<= RAMDISK_WINDOW_BASE` assert. Update `HEAP_BASE`'s and `MMAP_BASE`'s doc comments to say they are
the **legacy** origins, used only by a process that has never been through `VM_EXEC` (stub D and the
boot servers).

- [ ] **Step 4: Add `Kind::Stack` and the per-process origins**

```rust
/// What a region is for. `Unused` marks a free slot.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Kind {
    Unused,
    Heap,
    Mmap,
    /// The initial stack the *kernel* mapped at image load.
    ///
    /// VM records it but never resolves a fault against it: every stack page is
    /// already mapped when the record is made, so the region is pure
    /// bookkeeping. What it buys is `fork` cloning a region set that describes
    /// the child's whole address space, and a fault in the guard page landing
    /// just outside a known neighbour instead of nowhere at all.
    Stack,
}
```

Give `ClientRegions` the two origins:

```rust
struct ClientRegions {
    regions: [Region; MAX_REGIONS],
    /// Origin of this process's heap. The legacy [`HEAP_BASE`] until a
    /// `VM_EXEC` records the image end.
    heap_origin: u64,
    /// Next free VA for an anonymous mmap. Bump-only: munmap never returns
    /// addresses here (matches a trivial mmap allocator; reuse waits for a real
    /// per-process VM layout).
    mmap_next: u64,
}

impl ClientRegions {
    const EMPTY: Self = Self {
        regions: [Region::EMPTY; MAX_REGIONS],
        heap_origin: HEAP_BASE,
        mmap_next: MMAP_BASE,
    };
```

- [ ] **Step 5: Use the origins in `set_brk`, and add `exec` / `record_stack`**

In `set_brk`, replace both mentions of `HEAP_BASE` with `self.heap_origin`:

```rust
if new_break < self.heap_origin {
    return Err(EINVAL);
}
```

```rust
*r = Region {
    start: self.heap_origin,
    end,
    kind: Kind::Heap,
};
```

Careful: the second is inside `for r in self.regions.iter_mut()`, which borrows `self` mutably, so
read the origin into a local before the loop:

```rust
let origin = self.heap_origin;
```

and use `origin` inside. Then add the two new methods:

```rust
    /// Reset this process's bookkeeping around a freshly exec'd image.
    ///
    /// Drops every region the *previous* image accumulated and records
    /// `image_end` as the heap origin, with the mmap arena [`MMAP_GAP`] above
    /// it. Returns how many regions were dropped — non-zero only for a proc that
    /// had touched memory before exec'ing, which is what makes the stale-region
    /// gap observable in a boot log.
    ///
    /// A full reset, not a merge: the old regions describe an address space the
    /// kernel tore down in `SYS_EXEC`, so every one of them is wrong.
    fn exec(&mut self, image_end: u64) -> usize {
        let dropped = self
            .regions
            .iter()
            .filter(|r| r.kind != Kind::Unused)
            .count();
        self.regions = [Region::EMPTY; MAX_REGIONS];
        self.heap_origin = image_end;
        // Saturating rather than checked: an image_end near u64::MAX cannot come
        // out of the loader (every segment passed `check_va`), and an arena
        // origin above REGION_LIMIT simply makes every mmap answer ENOMEM, which
        // is the correct outcome for a process with no address space left.
        self.mmap_next = image_end.saturating_add(MMAP_GAP);
        dropped
    }

    /// Record the initial stack the kernel mapped, as a [`Kind::Stack`] region.
    ///
    /// Idempotent: a second call replaces the existing stack region rather than
    /// consuming a second slot, so a re-exec cannot leak slots.
    fn record_stack(&mut self) {
        for r in self.regions.iter_mut() {
            if r.kind == Kind::Stack {
                *r = Region {
                    start: USER_STACK_BASE,
                    end: USER_STACK_TOP,
                    kind: Kind::Stack,
                };
                return;
            }
        }
        for r in self.regions.iter_mut() {
            if r.kind == Kind::Unused {
                *r = Region {
                    start: USER_STACK_BASE,
                    end: USER_STACK_TOP,
                    kind: Kind::Stack,
                };
                return;
            }
        }
        // No free slot: MAX_REGIONS is 16 and a proc uses heap + stack + mmaps,
        // so this is unreachable in practice. Dropping the record silently is
        // the right failure — the stack is mapped either way, and refusing the
        // exec over a bookkeeping slot would be worse than not recording it.
    }
```

In `mmap`, the arena origin needs no change: it already bumps from `self.mmap_next`, which `exec`
now seeds.

- [ ] **Step 6: Add the public wrappers**

```rust
/// Reset process `nr`'s regions around a freshly exec'd image whose page-aligned
/// end is `image_end` (the `VM_EXEC` path). Returns the number of stale regions
/// dropped, or `EINVAL` if `nr` is untrackable.
pub fn exec(nr: i32, image_end: u64) -> Result<usize, i32> {
    Ok(client_mut(nr).ok_or(EINVAL)?.exec(image_end))
}

/// Record the kernel-mapped initial stack as a region of process `nr`.
/// `EINVAL` if `nr` is untrackable.
pub fn record_stack(nr: i32) -> Result<(), i32> {
    client_mut(nr).ok_or(EINVAL)?.record_stack();
    Ok(())
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p minixrs-vm region` Expected: PASS, all seven new tests plus the existing ones.

Run: `cargo clippy -p minixrs-vm` Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add servers/vm/src/region.rs
git commit --signoff -m "vm: per-process region origins, Kind::Stack, and exec reset

REGION_LIMIT becomes a re-export of uspace::USER_REGION_LIMIT. It used to
be the base of the lowest kernel-owned window; with the stack at the top of
process VA the first thing above a growing region is the stack's guard
page, not a window, so the old definition was wrong rather than merely
stale.

HEAP_BASE and MMAP_BASE survive as the legacy origins for a process that
never goes through VM_EXEC -- stub D and the boot servers -- so user_stub.S
needs no change. MMAP_GAP is extracted from the legacy pair rather than
invented, so the constant describes both layouts.

exec() returns how many stale regions it dropped, which is what makes the
gap it closes observable in a boot log rather than merely fixed.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 7: VM serves `VM_EXEC`, and seeds the boot servers' stacks

**Files:**

- Modify: `servers/vm/src/main.rs` (imports, dispatch at `:109`, a new handler, `vm_init`)

**Interfaces:**

- Consumes: `region::{exec, record_stack}` (Task 6), `VM_EXEC`, `VM_EXEC_PROC_OFF`,
  `VM_EXEC_IMAGE_END_OFF` (Task 4), `diag_fmt` from `server-rt`
- **Required cleanup:** Task 6 left `#[allow(dead_code)]` on `region::exec` and
  `region::record_stack`, with comments naming this task as their consumer. **Delete both attributes
  and both comments** — this task wires up the callers, so the forward declaration is spent.
- **Required check — reject `image_end == 0`.** Task 3 established that `load_into` does *not*
  refuse an ELF with no `PT_LOAD`: the phdr walk `continue`s past every non-`PT_LOAD` header and
  falls into `Ok(..)`, so a branded `PT_NOTE`-only image loads having mapped nothing and reports
  `image_end == 0`. Nothing exploitable follows — such an image faults immediately — but a zero must
  never become a heap origin, which would seed a process's heap at VA 0 and make the first `brk`
  hand out page zero. `handle_exec` answers `EINVAL` and emits a `[diag vm] exec FAIL` line before
  calling `region::exec`. The check lives here, not in the loader: refusing the image there would
  change `exec`'s errno surface, which is ABI-visible and outside this slice.
- Produces: the `[diag vm] exec …` marker Task 12 asserts

- [ ] **Step 1: Add the handler**

In `servers/vm/src/main.rs`, add to the imports: `VM_EXEC`, `VM_EXEC_IMAGE_END_OFF`,
`VM_EXEC_PROC_OFF` from `callnr`, and `diag_fmt` from `server_rt`.

Add the dispatch arm at `:109`, after `VM_FORK`:

```rust
VM_EXEC => handle_exec(&mut msg),
```

and the handler beside `handle_fork`:

```rust
/// Handle a `VM_EXEC` request from PM: reset a freshly exec'd process's memory
/// bookkeeping around its new image.
///
/// PM passes the target endpoint (payload [`VM_EXEC_PROC_OFF`]) and the loader's
/// page-aligned image end ([`VM_EXEC_IMAGE_END_OFF`]). VM drops every region the
/// proc's previous image accumulated — they describe an address space `SYS_EXEC`
/// already tore down — records `image_end` as the heap origin, and records the
/// kernel-mapped stack. Reply (PM issued a SENDREC) with `m_type = OK`, or the
/// negative error from [`region::exec`].
///
/// The marker is unconditional rather than sampled. exec is rare — a handful per
/// boot — and this line is the *only* proof that the heap origin follows the
/// image, since no user process can reach `VM_BRK`: the shared USER privilege
/// has no `ipc_to` bit for VM (`kernel/src/proc/table.rs`'s `USER_IPC_TO`), and
/// opening that edge is pre-Phase-6 chunk 5's work.
fn handle_exec(msg: &mut Message) {
    let caller_e = msg.m_source;
    let target_e: Endpoint = rd_i32(msg, VM_EXEC_PROC_OFF);
    let image_end = rd_u64(msg, VM_EXEC_IMAGE_END_OFF);
    let target_nr = endpoint_proc(target_e).get();

    let reply_type = match region::exec(target_nr, image_end) {
        Ok(dropped) => {
            // The stack is recorded *after* the reset, which clears it along
            // with everything else — the kernel maps a fresh stack for the new
            // image, so the record has to be re-made, not preserved.
            let _ = region::record_stack(target_nr);
            diag_fmt(format_args!(
                "exec nr={target_nr} image_end={image_end:#x} heap={image_end:#x} mmap={:#x} dropped={dropped}",
                image_end.saturating_add(region::MMAP_GAP),
            ));
            OK
        }
        Err(e) => {
            diag_fmt(format_args!("exec FAIL nr={target_nr} rc={e}"));
            e
        }
    };

    msg.m_type = reply_type;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}
```

- [ ] **Step 2: Seed the boot servers' stack regions**

A boot server is loaded by `load_boot_server`, never by exec, so no `VM_EXEC` ever names it — but
the kernel maps it the same 16-page stack. Record those at init, where the proc set is known and the
range is a compile-time constant. Replace `vm_init`:

```rust
/// SEF fresh-init callback: publish VM's endpoint to DS under its name, so other
/// servers can look VM up by name (slice 4.2), and record the initial stack
/// every boot proc was given.
///
/// The boot procs never go through `VM_EXEC` — the kernel builds their address
/// spaces in `load_boot_server` — so this is the only place their stack region
/// can be recorded. The range is the same compile-time constant for every proc,
/// since each has its own TTBR0.
///
/// A failure to record is not a startup failure: the stack is mapped by the
/// kernel either way, and the region is bookkeeping that `fork` and the fault
/// path benefit from rather than depend on.
fn vm_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    for nr in 0..NR_BOOT_PROCS as i32 {
        let _ = region::record_stack(nr);
    }
    sef_publish_to_ds(name)
}
```

Import `NR_BOOT_PROCS` from `minixrs_kernel_shared::com`. **Verify the name first** — run `grep -n
"NR_BOOT_PROCS\|NR_SERVED_PROCS\|N_BOOT" kernel-shared/src/com.rs` and use whichever constant names
the boot proc count. If none exists, use `NR_SERVED_PROCS` and note in the comment that recording a
stack for a slot that holds no process is harmless: `client_mut` bounds-checks, and a region set for
an unoccupied slot is overwritten wholesale when the slot is next used (the `fork` doc comment makes
exactly this argument already).

- [ ] **Step 3: Verify it builds and lints**

Run: `cargo clippy -p minixrs-vm` Expected: no warnings.

Run: `cargo test -p minixrs-vm` Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add servers/vm/src/main.rs
git commit --signoff -m "vm: serve VM_EXEC and record the kernel-mapped stack

The [diag vm] exec marker is unconditional rather than sampled: exec is
rare, and this line is the only available proof that the heap origin
follows the image. No user process can reach VM_BRK -- the shared USER
privilege has no ipc_to bit for VM -- so the end-to-end proof waits for
pre-Phase-6 chunk 5, which opens that edge.

Boot servers never receive a VM_EXEC but are given the same stack by the
kernel, so vm_init records theirs.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 8: PM forwards the image end to VM

**Files:**

- Modify: `servers/pm/src/main.rs` — `handle_exec` (`:374`), `exec_from_fs` (`:404`),
  `sys_exec_name` (`:773`), `sys_exec_grant` (`:806`), the `handle_exec` call site (~`:212`), and a
  new `vm_exec` marshaller

**Interfaces:**

- Consumes: `EXEC_IMAGE_END_OFF` (Task 4), `VM_EXEC` + its offsets (Task 4), the `SYS_EXEC` reply
  field (Task 5), VM's handler (Task 7)

**`image_end` is only meaningful when `m_type == OK`.** The kernel writes `40..48` on the success
path alone, and `kernel/src/system/mod.rs` replies with the buffer it copied in from the caller — so
on a failed exec those bytes are whatever PM sent, not a kernel value. Spec V4's "reply-only, past
`EXEC_LEN_OFF`, aliases nothing a caller wrote" covers *request-field* aliasing and says nothing
about the failure reply.

Two things already contain this, and both must stay: the marshallers below return `Err` on `m.m_type
!= OK` **before** reading the field, and PM builds each request with `payload: [0u8; 96]` so those
bytes are zero going out and therefore zero coming back. Neither is load-bearing alone — keep both,
and do not "simplify" the `m_type` check into reading the field unconditionally. This is not
theoretical: init's denial battery fires eight failed execs every boot.

- Produces: nothing other tasks consume

- [ ] **Step 1: Change the two `SYS_EXEC` marshallers to return the image end**

`sys_exec_name` and `sys_exec_grant` both return `i32` and discard the reply payload. Change both to
`Result<u64, i32>`. For `sys_exec_name`, replace its tail:

```rust
let rc = ipc_sendrec(system, &mut m);
if rc != OK {
    return Err(rc);
}
if m.m_type != OK {
    return Err(m.m_type);
}
Ok(rd_u64(&m, EXEC_IMAGE_END_OFF))
```

and its signature: `fn sys_exec_name(system: Endpoint, target_e: Endpoint, name: &str) ->
Result<u64, i32>`. Make the identical change to `sys_exec_grant`'s signature and tail.

Update both doc comments: replace "Returns the kernel-call result; on `OK` the kernel has already
resumed the target at the new entry." with:

```rust
/// Returns the loader's page-aligned image end on success — PM forwards it to VM
/// as the new image's heap origin — or the kernel-call / kernel error. On
/// success the kernel has already resumed the target at the new entry.
```

Add `EXEC_IMAGE_END_OFF` to the `callnr` import list.

- [ ] **Step 2: Thread it through `exec_from_fs`**

```rust
fn exec_from_fs(
    system: Endpoint,
    vfs: Endpoint,
    caller_e: Endpoint,
    path: &str,
    argv0: &str,
) -> Result<u64, i32> {
    // `?`, not a `match`: the old body matched only because the function
    // returned `i32`. Once it returns `Result`, clippy's `question_mark` lint
    // rejects the `match` under CI's `-D warnings`.
    let (size, gid) = vfs_exec_stage(vfs, path)?;
    sys_exec_grant(system, caller_e, argv0, vfs, gid, size)
}
```

- [ ] **Step 3: Add the `VM_EXEC` marshaller**

```rust
/// `VM_EXEC` — tell VM that `target_e` has exec'd an image ending at
/// `image_end`, so it drops the previous image's regions and seeds the heap
/// origin there (SENDREC to VM).
///
/// Returns VM's reply `m_type`. **The caller ignores it**, and deliberately: this
/// runs after `SYS_EXEC`'s point of no return, so there is nothing to roll back
/// and no error to report to a caller that is already running its new image.
/// See [`handle_exec`] for why it cannot run earlier.
#[cfg_attr(test, allow(dead_code))]
fn vm_exec(vm: Endpoint, target_e: Endpoint, image_end: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: VM_EXEC,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, VM_EXEC_PROC_OFF, target_e);
    wr_u64(&mut m, VM_EXEC_IMAGE_END_OFF, image_end);
    let rc = ipc_sendrec(vm, &mut m);
    if rc != OK { rc } else { m.m_type }
}
```

Add `VM_EXEC`, `VM_EXEC_PROC_OFF`, `VM_EXEC_IMAGE_END_OFF` to the `callnr` import list.

- [ ] **Step 4: Call it from `handle_exec`**

Change the signature to take VM's endpoint and rewrite the tail:

```rust
fn handle_exec(system: Endpoint, vfs: Endpoint, vm: Endpoint, msg: &mut Message) {
    let caller_e = msg.m_source;
    // ...path::parse block unchanged, except the arms now yield Result<u64, i32>...
    let rc = match path::parse(&msg.payload[PM_EXEC_PATH_OFF..PM_EXEC_PATH_OFF + PM_EXEC_PATH_MAX])
    {
        Ok(path::Target::Module(name)) => sys_exec_name(system, caller_e, name),
        Ok(path::Target::Path { path, argv0 }) => exec_from_fs(system, vfs, caller_e, path, argv0),
        Err(e) => Err(e),
    };
    match rc {
        Ok(image_end) => {
            // Success: the kernel already resumed the caller at the new image —
            // no reply. VM is told *after* the point of no return, so a failure
            // here cannot be rolled back and is not relayed to anyone: the
            // process is running either way, and the worst case is VM keeping
            // the stale view it has kept for every exec until now.
            //
            // Doing this before SYS_EXEC would mean unwinding VM state on every
            // failed exec, and init's denial battery fires eight per boot.
            let _ = vm_exec(vm, caller_e, image_end);
        }
        Err(e) => reply(caller_e, msg, e),
    }
}
```

Update the call site in the main loop to pass `vm` — the endpoint is already bound at `:133` as `let
vm = boot_endpoint(VM_PROC_NR);`. If it is not in scope at the dispatch, thread it through the same
way `vfs` is.

- [ ] **Step 5: Verify it builds, lints, and its tests pass**

Run: `cargo clippy -p minixrs-pm` Expected: no warnings.

Run: `cargo test -p minixrs-pm` Expected: PASS. PM's tests cover `mproc` and `path`, neither of
which this touches; if a test calls `sys_exec_name` directly, update it for the new return type.

- [ ] **Step 6: Commit**

```bash
git add servers/pm/src/main.rs
git commit --signoff -m "pm: forward the exec'd image's end to VM

VM_EXEC runs after SYS_EXEC's point of no return, so its result is ignored
on purpose: the process is already running its new image, there is nothing
to roll back, and the worst case is VM keeping the stale view it has kept
for every exec until now. Running it first would mean unwinding VM state on
every failed exec, and init's denial battery fires eight per boot.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 9: Finish the rename, and re-aim MFS's tripwire

**Files:**

- Modify: `fs/mfs/src/lib.rs:67` (import), `:97-112` (the tripwire)
- Modify: `fs/mfs/src/main.rs` (the `BLOCK` / `STAGE` statics and the `Blocks` token)
- Modify: `servers/vfs/src/main.rs:167` (a comment naming the old constant)

**Interfaces:**

- Consumes: `uspace::USER_STACK_BYTES` (Task 1)
- Produces: nothing other tasks consume. This is the task that makes `cargo check --workspace` pass
  again.

- [ ] **Step 1: Re-aim the tripwire**

In `fs/mfs/src/lib.rs`, change the import to `USER_STACK_BYTES` and replace the tripwire:

```rust
// The tripwire that `uspace::USER_STACK_BYTES` exists for.
//
// It fired. Under the old one-page stack a block buffer *was* the whole stack,
// so MFS's block and staging buffers could not be locals and lived in `.bss`
// behind a capability token. The stack is 16 pages now and they are locals
// again — which is what the assertion was written to prompt: "at that point a
// local becomes plausible again, and the reasoning above deserves to be re-read
// rather than silently outlived."
//
// Re-aimed at the condition that would make locals wrong again. Two block-sized
// buffers plus ordinary frame overhead should not approach the stack, and the
// margin is deliberately generous: a frame is not just its named locals, and the
// failure mode is a silent fault turned into a SIGSEGV that
// `tests/qemu-boot.forbidden` cannot catch. The guard page (`uspace`'s
// `USER_STACK_GUARD_BYTES`) now turns that overflow into a fault rather than a
// silent walk into the mmap arena, but a fault is still a crash — the margin is
// what keeps it from happening.
const _: () = assert!(
    (MFS_BLOCK_SIZE as u64) * 4 <= USER_STACK_BYTES,
    "two block buffers no longer fit comfortably in a frame: re-read why they are locals"
);
```

- [ ] **Step 2: Update VFS's stale comment**

In `servers/vfs/src/main.rs:167`, the comment justifies `EXEC_STAGE` living in `.bss` by pointing at
the one-page stack. Update it to argue from the new number — the conclusion is unchanged, and that
is the point:

```rust
/// (`uspace::USER_STACK_BYTES`), so a 256 KiB frame would still overflow the
/// stack four times over even at 16 pages. `EXEC_STAGE` stays in `.bss`: the
/// stack grew enough to make a *block* buffer a local again (`fs/mfs`), not
/// enough to make a quarter-megabyte staging buffer one.
```

- [ ] **Step 3: Verify the whole workspace builds again**

Run: `cargo check --workspace` Expected: PASS. This is the first task since Task 1 where it does —
`SERVER_STACK_BYTES` now has no remaining references.

Run: `grep -rn --include='*.rs' SERVER_STACK_BYTES .` Expected: no output.

Run: `cargo clippy --workspace` Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add fs/mfs/src/lib.rs servers/vfs/src/main.rs
git commit --signoff -m "mfs+vfs: finish the stack rename and re-aim the tripwire

The tripwire in lib.rs was written to fire when the stack grew, on the
grounds that a local becomes plausible again at that point and the reasoning
deserves re-reading rather than silently outliving itself. It fired. This
commit re-aims it and records that the buffers are now viable locals; the
next one acts on that.

VFS's EXEC_STAGE stays in .bss and its comment now argues from 64 KiB. The
conclusion is unchanged, which is the point -- the stack grew enough to make
a block buffer a local, not a quarter-megabyte staging buffer.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 10: MFS's block and staging buffers become locals

**Files:**

- Modify: `fs/mfs/src/main.rs:129-160` (the `BlockBuf` / `StageBuf` statics and their `Sync` impls),
  `:169-230` (`Stage`), `:230-340` (`Blocks`), `:375-395` (`main`'s frame), and the `device()` /
  `stage()` constructors

**Interfaces:**

- Consumes: the re-aimed tripwire from Task 9
- Produces: nothing other tasks consume. **Independently rejectable** — a reviewer can accept Task
  9's rename and decline this cleanup without blocking the slice.

**Before you start — the thing that is not mechanical.** `Blocks` and `Stage` are not bare ownership
tokens. Each holds a **grant** naming its buffer's address, issued to the `memory` driver with
`CPF_READ | CPF_WRITE` (`Blocks::addr` is literally `BLOCK.0.get() as usize as u64`). Moving a
buffer into a frame moves the address the grant names, so the buffer must live in a frame that
outlives every safecopy against it. `main`'s frame is exactly that frame — it already holds
`grants`, `blocks`, `stage` and `mount` for this reason, and `main` never returns. Put the buffers
there and nowhere else.

- [ ] **Step 1: Give the two types a borrowed buffer**

In `fs/mfs/src/main.rs`, delete `BlockBuf`, `StageBuf`, their `unsafe impl Sync`, and the `BLOCK` /
`STAGE` statics. Give both capability types a lifetime and a `&mut` instead:

```rust
struct Blocks<'a> {
    /// The block buffer, borrowed from `main`'s frame.
    ///
    /// A `&mut` rather than the `UnsafeCell` static this used to reach: the
    /// static existed only because a one-page stack could not hold 4 KiB, and
    /// with it went a hand-written `Sync` impl whose soundness rested on an
    /// argument about MFS being single-threaded. The borrow checker makes that
    /// argument now, so the `unsafe` goes away rather than moving.
    buf: &'a mut [u8; MFS_BLOCK_SIZE],
    // ...the existing grant fields, unchanged...
}
```

and the same shape for `Stage<'a>`. Replace every `unsafe { &*BLOCK.0.get() }` / `unsafe { &mut
*BLOCK.0.get() }` with `&*self.buf` / `&mut *self.buf`, and every `BLOCK.0.get() as usize as u64`
with `self.buf.as_ptr() as usize as u64`.

- [ ] **Step 2: Move the buffers into `main`'s frame**

```rust
// `main`-frame values that outlive the receive loop. The two block buffers
// join them: each is named by a grant the `memory` driver safecopies
// against, so the frame holding them has to outlive every such copy —
// `main` never returns, which is the same reason `grants` lives here rather
// than in `init_fresh`.
let mut grants: GrantPool<GRANT_SLOTS> = GrantPool::new();
let mut block_buf = [0u8; MFS_BLOCK_SIZE];
let mut stage_buf = [0u8; MFS_BLOCK_SIZE];
let mut blocks = device(&mut grants, mem_endpoint(), &mut block_buf);
let mut stage = stage(&mut stage_buf);
```

Thread the `&mut` through `device()` and `stage()` and give every `Blocks` / `Stage` mention in a
signature its lifetime.

- [ ] **Step 3: Update the module documentation**

The module note at `fs/mfs/src/main.rs:35-49` explains that the buffer "is reached only through the
[`Blocks`] capability token" *because* it is a static. Rewrite that paragraph: the borrow discipline
survives and is now enforced by the compiler rather than by a hand-written `Sync` impl, and the
reason the static existed — a 4 KiB stack — is gone.

- [ ] **Step 4: Verify**

Run: `cargo clippy --workspace` Expected: no warnings.

Run: `cargo test -p minixrs-mfs` Expected: PASS — MFS's on-disk library is host-tested.

Run: `grep -n "unsafe impl Sync" fs/mfs/src/main.rs` Expected: no output for `BlockBuf` /
`StageBuf`.

Run a boot and check the filesystem battery still passes end to end — this touches the grants the
`memory` driver copies against, so a compile-clean refactor is not sufficient evidence:

```bash
timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  --no-default-features > /tmp/mfs.log 2>&1
grep -a "^\[diag mfs\]\|fs\." /tmp/mfs.log
```

Expected: the `fs.*` battery — including `fs.write ok n=32768 v=32768`, create/truncate, and the
`/etc/holey` hole probe — identical to a run on the merge base.

- [ ] **Step 5: Commit**

```bash
git add fs/mfs/src/main.rs
git commit --signoff -m "mfs: the block and staging buffers become locals

Both were .bss statics behind UnsafeCell and a hand-written Sync impl for
exactly one reason: a one-page stack could not hold 4 KiB. At 64 KiB they
are locals in main's frame, and the soundness argument that used to be a
comment about MFS being single-threaded is the borrow checker's now.

They live in main's frame specifically, not any frame: each is named by a
grant the memory driver safecopies against, so the buffer has to outlive
every such copy. main never returns -- the same reason grants already lives
there rather than in init_fresh.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 11: `bigprog` — the VA-ceiling regression fixture

**Files:**

- Create: `userland/bigprog/Cargo.toml`, `userland/bigprog/build.rs`, `userland/bigprog/user.ld`,
  `userland/bigprog/src/main.rs`
- Modify: `Cargo.toml` (workspace members), `kernel/build.rs` (pack it into the MXBI archive),
  `userland/init/src/main.rs:169` (`EXEC_TARGETS`)

**Interfaces:**

- Consumes: the whole map from Tasks 1-10
- Produces: the `bigprog ok` marker Task 12 asserts

- [ ] **Step 1: Copy `worker`'s scaffolding**

`userland/worker` is the closest existing shape: a freestanding EL0 program packed into the MXBI
archive as an exec target, not a boot server. Copy `Cargo.toml`, `build.rs` and `user.ld` from it,
renaming the crate to `minixrs-bigprog`, and read `kernel/build.rs`'s `worker` handling to add
`bigprog` the same way — tagged with `com::EXEC_ONLY_PROC_NR` so it is packed but never loaded at
boot.

Keep `user.ld`'s `FILEHDR PHDRS` idiom and its `0x00100000 + SIZEOF_HEADERS` base unchanged: the
fixture must differ from `worker` in exactly one respect, its size.

- [ ] **Step 2: Write the program**

`userland/bigprog/src/main.rs`:

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `bigprog` — the user VA map's regression fixture.
//!
//! A program whose only distinguishing feature is that it is too big for the
//! VA map Phase 5 shipped. Its `.bss` is 2 MiB, so the highest `PT_LOAD`'s
//! `p_vaddr + p_memsz` runs from the `0x0010_0000` load base to roughly
//! `0x0030_0000` — straight through `0x0020_0000`, where the initial stack page
//! used to be. Under the old map `load_exec_image` fails to map the stack with
//! `AlreadyMapped`, and the exec answers `ENOEXEC`.
//!
//! ## Why `.bss` and not `.rodata`
//!
//! The loader maps `ceil(p_memsz / PAGE_SIZE)` pages either way, so the VA
//! collision this fixture exists to prove is byte-identical. But `.bss` costs
//! `p_filesz = 0`, so the packed image stays kilobytes: the boot archive, the
//! 1 MiB rootfs and the boot budget are all untouched.
//!
//! The tradeoff, stated rather than discovered: this proves the *VA* ceiling,
//! not multi-MiB file I/O through MFS. That path is already proven by slice
//! 5.10a's 32 KiB write, and sizing a filesystem for a multi-MiB program is the
//! disk-root question Phase 6 slice 6.4 owns.
//!
//! The program touches its first and last `.bss` page — proving the pages are
//! really mapped, not merely promised by a program header — then reports through
//! fd 2 and exits.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    PM_EXIT, VFS_BUF_OFF, VFS_FD_OFF, VFS_LEN_OFF, VFS_WRITE,
};
use minixrs_kernel_shared::com::{PM_PROC_NR, VFS_PROC_NR, boot_endpoint};

/// 2 MiB — enough that the image's last `PT_LOAD` ends past `0x0030_0000`,
/// which is well clear of the `0x0020_0000` the old stack page occupied. Sized
/// for an unambiguous verdict, not for the minimum that would collide.
const BIG_BYTES: usize = 2 * 1024 * 1024;

/// The `.bss` array that makes this image large. `static mut` rather than a
/// local: it must land in a `PT_LOAD`'s `p_memsz`, which is the whole point —
/// a stack array would prove nothing about the image's VA span.
static mut BIG: [u8; BIG_BYTES] = [0u8; BIG_BYTES];

/// Standard error, pre-opened by VFS to the console.
const STDERR: i32 = 2;

#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // Touch the first and last page. `.bss` is satisfied by the loader's zeroed
    // frames, so a read proving them mapped is as strong as a write and cannot
    // be optimised into a store the compiler might sink.
    //
    // SAFETY: sole thread of this process; `BIG` is this program's own `.bss`,
    // and both indices are in bounds by construction.
    let (first, last) = unsafe {
        let p: *const u8 = (&raw const BIG).cast();
        let first = core::ptr::read_volatile(p);
        let last = core::ptr::read_volatile(p.add(BIG_BYTES - 1));
        (first, last)
    };

    if first == 0 && last == 0 {
        write_str(b"bigprog ok: bss pages mapped\n");
    } else {
        write_str(b"bigprog FAIL: bss not zeroed\n");
    }

    exit(0)
}
```

Add `write_str` and `exit` helpers by copying `worker`'s — read `userland/worker/src/main.rs` and
mirror its `VFS_WRITE` marshalling and `PM_EXIT` exactly rather than inventing a second spelling.

- [ ] **Step 3: Add it to init's exec rotation**

In `userland/init/src/main.rs`, extend `EXEC_TARGETS`. The array's doc comment says **`worker` must
stay first** — respect that; append:

```rust
/// `bigprog` is third: the user VA map's regression fixture (pre-Phase-6 chunk
/// 2). It is a module name, like `worker` — a 2 MiB `.bss` image does not need
/// to come off the filesystem to prove the VA ceiling is gone, and keeping it
/// out of the rootfs leaves the image size and the boot budget untouched.
const EXEC_TARGETS: [&str; 3] = ["worker", ROOTFS_HELLO_PATH, "bigprog"];
```

- [ ] **Step 4: Verify the image really is large, and boots**

Run: `cargo kernel-aarch64` Expected: builds.

Run (substitute the built path — `build-and-boot.md` documents the inspection command, and it is
**mandatory whenever a `user.ld` changes**):

```bash
llvm-readobj --elf-output-style=GNU --program-headers \
  target/aarch64-unknown-none/release/bigprog
```

Expected: the last `PT_LOAD` has a `MemSiz` over 2 MiB, a `FileSiz` in the kilobytes, and
`VirtAddr + MemSiz` past `0x0030_0000`. **Read the numbers**; do not assume them.

Run: `timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release
--no-default-features > /tmp/boot.log 2>&1; grep -a "bigprog" /tmp/boot.log` Expected: `bigprog ok:
bss pages mapped`. The stub-free build is the fast one to iterate on (`build-and-boot.md`: the
`fs.*` markers land at ~0.14% of a 60 s stub-free log).

- [ ] **Step 5: Commit**

```bash
git add userland/bigprog Cargo.toml kernel/build.rs userland/init/src/main.rs
git commit --signoff -m "userland: add bigprog, the VA-ceiling regression fixture

A 2 MiB .bss puts the highest PT_LOAD's span across 0x200000, where the
initial stack page used to be -- so under the old map this image fails to
map its own stack with AlreadyMapped and the exec answers ENOEXEC.

.bss rather than .rodata: the loader maps ceil(p_memsz / PAGE_SIZE) pages
either way, so the collision is byte-identical, but p_filesz stays zero and
the boot archive, the rootfs and the boot budget are untouched. It proves
the VA ceiling, not multi-MiB file I/O -- that path is already proven by
5.10a's 32 KiB write, and sizing a filesystem for a large program is the
disk-root question slice 6.4 owns.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 12: Markers, the boot matrix, and the tooling change plan

**Files:**

- Modify: `tests/qemu-boot.expected` (tighten `[exec] … sp=`, add the two new markers)
- Modify: `docs/plan.md` (check chunk 2), `docs/plans/phase-6-prep.md` (check chunk 2, record the
  tooling hand-off)
- Create: `docs/superpowers/plans/2026-09-19-user-va-map-tooling.md` (the change plan for the
  separate tooling session)

**Interfaces:**

- Consumes: every preceding task
- Produces: the completed slice

- [ ] **Step 1: Tighten the `sp` marker and add the new ones**

`tests/qemu-boot.expected:240` reads `[exec] argc=1 envc=0 auxv=4 sp=0x` — prefix-only, so it proves
nothing about *where* the stack is. Change it to pin the high stack, and add the two new markers:

```
[exec] argc=1 envc=0 auxv=4 sp=0x3fff
[diag vm] exec nr=
bigprog ok: bss pages mapped
```

**Also add `[diag vm] exec FAIL` to `tests/qemu-boot.forbidden`.** Task 7's zero-`image_end`
rejection and its bad-endpoint arm both print that prefix, and with no forbidden entry they could
fire on every boot while the run still passed. Note this moves the marker total by **2**, not 1 —
`check-boot-log.sh`'s PASS count sums both files.

Read [`testing-and-markers.md`](../../conventions/testing-and-markers.md) first: expectations are
first-occurrence-only, so the `[diag vm] exec` entry matches whichever exec happens first. That is
deliberate — the marker must appear for *every* exec, so the first one is a sufficient witness, and
pinning a specific `nr` or `image_end` would make the line rebuild-fragile.

- [ ] **Step 2: Run the full boot and read the log**

Run:

```bash
timeout 300 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  > /tmp/boot-full.log 2>&1
tools/check-boot-log.sh /tmp/boot-full.log
```

Expected: every marker passes. If `[exec] … sp=0x3fff` fails, read the actual `sp` in the log before
changing anything — an `sp` that is not in `0x3fff_xxxx` means the stack did not move, which is a
Task 2 defect, not a marker that needs loosening.

**Run boots serially, with nothing else building.** Measured on this branch at HEAD: `timeout 300`
passes 106/106, with the last required marker (`hello: errno ok`) at **68% of the window (~204 s)**
— adequate, not generous. A boot sharing the machine with a `cargo` build tips over that 32%
headroom and fails markers that have nothing wrong with them; one agent lost three boots to exactly
that and concluded the budget needed raising to 1800 s. It does not. Do not start a build, a
subagent, or a second boot while one is running.

**Measure the fraction inside the budget you actually use.** Extrapolating from a longer run
overestimates headroom badly — the same marker reads 6.90% of an 1800 s log, which scales to a
flattering "~124 s" — because log growth is not linear across the window. That is why
[`ci.md`](../../conventions/ci.md) asks for the fraction rather than the wall clock.

- [ ] **Step 3: Measure the boot-timing ratio**

`ci.md` requires the ratio, not the wall clock. For the last required marker:

```bash
grep -abo '<last required marker>' /tmp/boot-full.log | head -1   # byte offset
wc -c /tmp/boot-full.log                                          # total
```

Compare the fraction against the same measurement at the merge base (`git stash` the branch or build
`main` into a second log). Record both numbers in the PR description. The 600 s budget is not
expected to move; if the fraction climbed materially, say so with the two numbers rather than
raising the budget on a hunch.

- [ ] **Step 4: Run the mandatory three-boot matrix**

`ci.md` treats an image-base or stack move as a **mandatory** matrix run, and `$MINIXRS_SDK` does
not persist across shell invocations — each row must set it inline:

```bash
# Row 1: the SDK flavour
MINIXRS_SDK=~/toolchains/minixrs timeout 300 cargo run -p minixrs-kernel \
  --target aarch64-unknown-none --release > /tmp/row-sdk.log 2>&1

# Row 2: forced in-tree musl
MINIXRS_SDK=/nonexistent timeout 300 cargo run -p minixrs-kernel \
  --target aarch64-unknown-none --release > /tmp/row-musl.log 2>&1

# Row 3: moved-aside sysroot — needs MINIXRS_SDK=/nonexistent TOO, or the
# flavour selector never reaches the sysroot and this silently re-runs row 1.
mv ~/toolchains/minixrs/sysroot ~/toolchains/minixrs/sysroot.aside
MINIXRS_SDK=/nonexistent timeout 300 cargo run -p minixrs-kernel \
  --target aarch64-unknown-none --release > /tmp/row-aside.log 2>&1
mv ~/toolchains/minixrs/sysroot.aside ~/toolchains/minixrs/sysroot
```

Run `tools/check-boot-log.sh` on each. Confirm the flavour each row actually built — markers are
byte-identical across flavours, which is what makes a mis-measured row invisible.

Also run the linted-path check `ci.md` requires:

```bash
MINIXRS_SDK=/nonexistent cargo clippy -p minixrs-kernel \
  --target aarch64-unknown-none --release
```

- [ ] **Step 5: Write the tooling change plan**

Create `docs/superpowers/plans/2026-09-19-user-va-map-tooling.md` with the exact edits from spec §5
— `check-image.sh`'s constants and overlap rule, `check-driver.sh`'s removed assertion, LLVM patch
0006's dropped pin, and `selftest.sh`'s inverted fixture — plus V12's ordering. State at the top
that this repo's PR must land **first**: dropping the pin before the stack moves would link SDK
images straight onto the stack.

This file is the hand-off for a separate session in `~/src/tooling`. Per the cross-repo rule, **do
not edit that repo from this session.**

- [ ] **Step 6: Check the trackers**

A PR marks its own work complete. In `docs/plan.md`, change the Pre-Phase-6 cleanup line to `- [x]
**Chunk 2** — user VA map: high stack, larger stack, image-relative brk`, and make the same change
in `docs/plans/phase-6-prep.md`'s Status list. Add a line to the phase-6-prep chunk 2 section
pointing at the tooling plan and noting that the tooling half is outstanding.

Run: `~/.dprint/bin/dprint fmt` then `~/.dprint/bin/dprint check` and `python3
tools/check-md-links.py` Expected: check clean, 0 broken links.

- [ ] **Step 7: Commit**

```bash
git add tests/qemu-boot.expected docs/plan.md docs/plans/phase-6-prep.md \
        docs/superpowers/plans/2026-09-19-user-va-map-tooling.md
git commit --signoff -m "docs+tests: assert the new map, check chunk 2, hand off the tooling

The [exec] sp marker was prefix-only (sp=0x), so it proved a stack pointer
existed and nothing about where. Tightened to sp=0x3fff, which is the claim
the slice is actually making.

The tooling half -- check-image.sh's constants, and dropping the clang
--image-base pin that only ever compensated for the stack being at lld's
default base -- is written up for a separate session in ~/src/tooling, and
must land after this repo's PR: dropping the pin first would link SDK images
straight onto the stack.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 13: The documentation the rename falsifies

**Files:**

- Modify: `docs/conventions/servers-and-drivers.md:121`, `:390`, `:394`
- Modify: `docs/conventions/kernel.md:314`, `:359`, `:392`
- Modify: `docs/conventions/build-and-boot.md:56` (the stale constant) **and `:55-63`, the broken
  stack-frame recipe — see below**
- Modify: `docs/conventions/testing-and-markers.md` (add the starved-boot check — see below)
- Modify: `book/src/servers/overview.md:330-331`
- Modify: `book/src/libc/overview.md:264`

**Interfaces:**

- Consumes: everything. Run this last, so the prose describes what actually shipped.
- Produces: nothing code depends on.

**Why this task exists.** Wave B found ten live references to the one-page stack that no other task
owned — and three of them (`servers-and-drivers.md:390`, `:394`, `build-and-boot.md:56`) name
`uspace::SERVER_STACK_BYTES`, **a constant that no longer exists**. `CLAUDE.md` makes
`docs/conventions/` mandatory reading before working in an area, and `book/` is the canonical
how-it-works documentation, so these are not history files: they are instructions to the next agent,
and they would have shipped pointing at a name that does not compile.

`docs/plans/phase-4-*` and `phase-5-*` hits are **legitimate history and must stay** — they record
what was true in those slices. Only live guidance is in scope.

- [ ] **Step 1: Find every reference**

```bash
grep -rn --include='*.md' "SERVER_STACK_BYTES\|SERVER_STACK_VA" docs/conventions/ book/
grep -rn --include='*.md' "exactly one page\|one RW stack page\|stack is exactly" docs/conventions/ book/
```

Read each hit in context before editing. The count at the time of writing was ten; re-run rather
than trusting that number.

- [ ] **Step 2: Correct them**

Each is a claim about the *old* map. Rewrite to the new one rather than deleting the sentence — the
surrounding reasoning is usually still valid and only the number is wrong. Two need more than a
number swap:

- `book/src/libc/overview.md:264` — "Both sit a clear megabyte below `SERVER_STACK_VA` (`0x200000`)"
  is **doubly wrong** after this slice: the stack is not at `0x200000`, and V11 makes `0x200000` the
  SDK images' own load base. Re-argue it from `USER_REGION_LIMIT`.
- `docs/conventions/servers-and-drivers.md:394` — "`uspace::SERVER_STACK_BYTES` exists solely to
  carry `fs/mfs`'s `const _`" describes a tripwire that has since fired and been re-aimed. Say what
  it carries now.

- [ ] **Step 2b: Fix the broken stack-frame recipe** (`build-and-boot.md:55-63`)

This one is not stale prose, it is a **check that silently lies**, and this slice is what makes it
dangerous. Its grep is:

```
grep -oE 'sub[[:space:]]+sp, sp, #0x[0-9a-f]+'
```

which does **not** match `sub sp, sp, #0x1, lsl #12` — the form LLVM emits for every frame ≥ 4 KiB.
Verified against today's `minixrs-mfs`: the recipe reports **1248** bytes where `main`'s real frame
is **9440** (`0x1<<12` twice, plus `0x4e0`).

It under-reports by 7.5x, and it does so precisely on the large frames it exists to catch — which
this slice just made legal by growing the stack to 64 KiB and moving MFS's two 4 KiB buffers into
`main`. A recipe that reports a small number for a big frame is worse than no recipe.

Fix the pattern to handle both encodings and sum the pair LLVM emits for a single prologue, or
replace it with a note to read the prologue directly. Whatever you write, **run it against
`minixrs-mfs` and confirm it reports 9440**, not 1248. The surrounding prose also still says a
server gets one page; correct it with the rest.

- [ ] **Step 2c: Record the starved-boot check** (`testing-and-markers.md`)

A boot on a loaded host fails markers that are not broken, and the failure mimics a real regression
in an unrelated subsystem. This has now produced a wrong conclusion three times in one slice: once
"the budget must rise to 1800 s", once "the filesystem write path hangs", and once "the line I just
added broke the boot". Each was host contention.

Record the cheap discriminator, which costs one command:

```bash
grep -ao '\[ipc [0-9]*' <log> | tail -1      # final IPC counter, the throughput proxy
uptime; ps -eo pcpu,comm -r | head -5       # what was competing
```

A clean 300 s run on this branch reaches **~18.8 M** ticks; a passing 1200 s run reached **114 M**.
**Under ~10 M at timeout means the run was starved, not broken** — re-run it before believing any
marker failure, and do not change code on the strength of a starved boot. The control that settles
it in one step is to restore the merge base's copy of the changed file, rebuild, and boot again: if
HEAD fails identically, the host is the cause.

- [ ] **Step 3: Verify**

```bash
grep -rn --include='*.md' "SERVER_STACK_BYTES\|SERVER_STACK_VA" docs/ book/
```

and confirm the repaired recipe reports the real frame:

```bash
MFS=target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-mfs
# <the repaired recipe> -> must print 9440, not 1248
```

Expected: hits **only** under `docs/plans/phase-4-*` and `docs/plans/phase-5-*` (history), and in
this slice's own spec and plan where they name what was replaced.

Run `~/.dprint/bin/dprint fmt`, then `~/.dprint/bin/dprint check` and `python3
tools/check-md-links.py`. Both block in CI.

- [ ] **Step 4: Commit**

```bash
git add docs/conventions book/src
git commit --signoff -m "docs: retire the one-page stack from live guidance

Ten references across docs/conventions/ and book/ still described the
one-page stack, and three named uspace::SERVER_STACK_BYTES -- a constant
this slice deleted. CLAUDE.md makes docs/conventions/ mandatory reading
before working in an area, so these were not stale trivia: they were
instructions handing the next agent a name that does not compile.

docs/plans/phase-4-* and phase-5-* keep theirs. Those record what was true
in those slices, which is what a plan history is for.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Verification Before Completion

Before claiming the slice is done, run each of these and read the output:

- [ ] `cargo check --workspace` — clean
- [ ] `cargo clippy --workspace` — no warnings
- [ ] `cargo clippy -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features`
      — no warnings (the `boot-stubs`-off arm)
- [ ] `cargo test -p minixrs-kernel-shared` / `-p minixrs-vm` / `-p minixrs-mfs` / `-p minixrs-pm` /
      `-p minixrs-gen-c-headers` — all pass
- [ ] the `fs.*` marker battery identical to the merge base (Task 10 touches MFS's grants)
- [ ] the `external/musl` answer from Task 4 Step 6 written into the PR description
- [ ] `grep -rn --include='*.rs' SERVER_STACK_BYTES .` — no output
- [ ] `tools/check-boot-log.sh` green on the full default boot
- [ ] the three-boot matrix, all rows, with the flavour of each confirmed
- [ ] `~/.dprint/bin/dprint check` and `python3 tools/check-md-links.py` — clean
- [ ] the boot-timing ratio measured against the merge base, both numbers recorded
- [ ] `grep -rn --include='*.md' "SERVER_STACK_BYTES\|SERVER_STACK_VA" docs/ book/` — hits only in
      `docs/plans/phase-4-*`, `docs/plans/phase-5-*`, and this slice's own spec/plan

Then **stop**. Do not push and do not open a PR — run `/claude-md-management:revise-claude-md`
first, then surface the branch for review.

# musl Syscall Surface — the tooling half (hand-off to `~/src/tooling`)

> **This plan is executed in a different repository, in a different session.** Nothing here is
> edited or applied from the minixrs session that wrote it — the cross-repo rule forbids it, and the
> tooling repo signs its own commits. This file is the specification that session works from.

**Companion plan:** [`2026-10-28-musl-syscall-surface.md`](./2026-10-28-musl-syscall-surface.md)
(the minixrs half, tasks 1–9 plus 7a and 8a).\
**Spec:**
[`../specs/2026-10-28-musl-syscall-surface-design.md`](../specs/2026-10-28-musl-syscall-surface-design.md)
— M12 carries the three-boot-matrix reasoning, §4 the deferred cleanups.\
**Tracker:** [`../../plans/phase-6-prep.md`](../../plans/phase-6-prep.md), chunk 5 — its box is
checked for the OS-side work; this plan is what the "tooling hand-off — outstanding" line points at.

## What changed, and why tooling has anything to do

The musl fork gained a syscall shim: `src/minixrs/_syscall.c` went from six Linux syscalls to
fourteen (`read`, `readv`, `openat`, `close`, `lseek`, `brk`, `mmap`, `munmap` are new), plus
`src/minixrs/minixrs_internal.h` carrying the VM payload offsets. That is **fork commit
`4501ccf402fa4a85eb6dbf1b55df9f074187516b`** on the branch `feature/chunk5-syscall-surface` of
`minixrs/musl-minixrs`, off `minixrs` @ `6010533f`. It is signed, signed off, and **local only — not
yet pushed.**

The SDK sysroot at `$MINIXRS_SDK/sysroot` — `libc.a` and the installed `minixrs/*.h` — was built by
tooling's `scripts/build-sysroot.sh` from the **pre-shim** fork, `6010533f`. `sysroot/.stamp` says
`musl=6010533f…`. An SDK-built `/bin/fileio` therefore links against a `libc.a` whose syscall arm
still answers `-ENOSYS` to everything but the old six: it compiles and links (task 8 confirmed it
under `-Werror`, with `_end` = `0x20AFD0`) but cannot open a file.

## Ordering

```
1. fork branch pushed and merged     — minixrs/musl-minixrs gets 4501ccf (or its merge) on `minixrs`
2. minixrs PR lands                  — the external/musl submodule bump and the OS change together (D8)
3. tooling rebuilds the SDK sysroot  — build-sysroot.sh against the new fork commit
4. minixrs three-boot matrix row 1   — re-run against the rebuilt SDK
```

Steps 1 and 2 are the minixrs session's (the user pushes; this repo's D8 rule is that the fork bump
and the submodule bump travel together). Steps 3 and 4 are this plan. Step 3 should not run before
step 1: the sysroot has to be built from a fork commit that exists on the remote.

There is no ordering hazard of the kind the user-VA-map hand-off had: the OS half is safe in
isolation, and the SDK flavour simply fails its fileio markers until the sysroot is rebuilt. That is
a **loud** failure (missing `fileio:` markers in a row-1 boot), not a silent one.

## The edits

### 1. Rebuild the SDK sysroot from the new fork commit

Run tooling's `scripts/build-sysroot.sh` so that `libc.a` is built from the fork at `4501ccf` (or
the commit that merged it) and the `minixrs/*.h` headers are regenerated from the minixrs commit
that carries `VFS_LSEEK` and `VFS_PM_EXIT` — `callnr.h` gained rows and the shim includes it. Per
the libc chapter in the book, **any `kernel-shared` ABI change requires re-running
`build-sysroot.sh`**; this chunk is one.

Afterwards `$MINIXRS_SDK/sysroot/.stamp` should read `musl=<the new sha>` and a `minixrs=<sha>` at
or after the chunk-5 ABI commit (`6a2fdb8`, "abi: VFS_LSEEK, VFS_PM_EXIT, SEEK_*").

### 2. Check whether tooling pins a fork revision anywhere

Task 9's brief asks for this. Search the tooling repo for the old SHA and for the fork's branch
name:

```sh
grep -rn '6010533f\|musl-minixrs' ~/src/tooling --include='*.sh' --include='*.md' --include='*.toml' --include='*.json'
```

If a script, a manifest or a docs table pins `6010533f…`, move it to the new commit in the same
change. If nothing pins it (the expectation: `build-sysroot.sh` reads the fork's checked-out `HEAD`
and writes the SHA to the stamp), say so in the tooling commit message — but see step 3: a SHA pin
is not the only record tooling keeps of the fork.

### 3. Export the fork commit as `patches/musl/0002-*.patch`

Tooling records each fork as a `git format-patch` series under `patches/`, and the musl series today
is one patch, `patches/musl/0001-slice-5.6a-port-musl-to-minix.rs-IPC-aarch64.patch` — fork commit
`f1db256f`. Without this step the tooling repo's record of the fork stops at the pre-shim port even
after the sysroot is rebuilt from the new one.

With the forks-volume checkout (`$MINIXRS_FORKS_DIR/musl-minixrs`) at the commit the sysroot was
built from — `minixrs` after the merge of `4501ccf` — run:

```sh
scripts/export-patches.sh musl     # format-patch of ${MINIXRS_MUSL_BASE:-v1.2.6}..HEAD
```

Expect `export-patches: musl → 2 patch(es)`: `0001-…` unchanged in content (only the trailing git
version line may churn, which tooling's `CLAUDE.md` calls noise) and a new
`0002-minixrs-openat-read-readv-close-lseek-brk-mmap-munma.patch` — `format-patch` derives the name
from `4501ccf`'s subject and truncates it, so the missing `p` is not a typo. `format-patch` skips
merge commits, so the `6010533f`-style merge on `minixrs` contributes no patch. Commit the new file
in the same tooling change as the sysroot rebuild.

One mismatch to know about, not to fix here: tooling's `CLAUDE.md` and `export-patches.sh` describe
the forks as rebase-maintained and force-pushed. This chunk did not rebase — `4501ccf` is a plain
commit on top of `minixrs` and merges with a merge commit (spec M12), which is why the series only
grows by one patch and `0001` keeps its content.

### 4. Verification, in the tooling repo

1. `verify/check-driver.sh` — green, unchanged: this chunk does not touch the driver.
2. `verify/check-brand.sh` and `verify/check-image.sh` over an SDK-built `fileio` and `hello` — both
   `BRANDED` / `LOADABLE`. The SDK flavour links `fileio` with no linker script, so the image has
   four `PT_LOAD`s, `_end` near `0x20AFD0`, no `PT_TLS`.
3. Hand back to minixrs for step 4 of the ordering.

## Back in minixrs: three-boot-matrix row 1

With `$MINIXRS_SDK` pointing at the rebuilt SDK (see [`ci.md`](../../conventions/ci.md) for the
matrix and how to identify the flavour from the build-script warning, never the boot log):

```sh
# build and boot as for rows 2 and 3, with MINIXRS_SDK=~/toolchains/minixrs, then:
tools/check-boot-log.sh <captured-log>
```

Expect the **same markers rows 2 and 3 are measured against**: the `hello` and `fileio` markers
byte-identical to the musl flavour, `check-boot-log.sh` PASS with all 135 checks, and a build
warning reading `hello, fileio: built with the minix.rs SDK at <prefix> (<stamp>)`. Row 1 must be
**fully clean**; record it in the PR description, which lists which rows ran.

One thing differs from the musl flavour and is expected, not a defect: the SDK images load at lld's
default base (`0x200000`) rather than `0x100000`, so the exec trace's `image_end` for `fileio` is
not `0x128000`.

**One risk to check before the boot: the rebuilt SDK `fileio` may not fit `VFS_EXEC_MAX`.** VFS
stages a whole image in a fixed buffer of `callnr::VFS_EXEC_MAX` (256 KiB), and the musl-flavour
`fileio` already sits at 259,752 B of 262,144 — 2.3 KB of headroom. The SDK image linked against the
pre-shim sysroot was ~48 KB, so the rebuilt one is expected to fit easily, but nobody has linked it
against the shim yet. Over the ceiling is loud, not silent: `kernel/build.rs` panics with `fileio is
N bytes, past VFS's staging buffer`. The remedy is a minixrs change, not a tooling one — raise
`callnr::VFS_EXEC_MAX` (bounded above by `execimage::MAX_IMAGE_BYTES`, asserted in `callnr.rs`, and
pinned by a host test there that moves with it). It is not emitted into the C headers, so raising it
is not a D8 bump and needs no sysroot rebuild.

## Optional follow-up — not a tooling task

`FS_PATH_MAX` and the `VM_BRK` / `VM_MMAP` / `VM_MUNMAP` payload offsets are **not emitted** into
the generated C headers (`tools/gen-c-headers/src/callnr_h.rs`, in minixrs). The shim hand-spells
them — the path bound uses `PM_EXEC_PATH_MAX`, which `kernel-shared` pins equal to `FS_PATH_MAX`
with a `const _`, and the offsets sit in `minixrs_internal.h` beside the older `PM_EXIT_STATUS_OFF`.
If someone wants them generated, that is a small minixrs change plus a one-line shim edit in the
fork, and it would need this same sysroot rebuild afterwards. It is not required for anything here.

## What this plan does not change

- `check-brand.sh`, `check-image.sh` and the identity note — untouched.
- The LLVM driver and its patch series — the shim is libc, not the compiler.
- Anything under `$MINIXRS_SDK` from the minixrs session. The SDK is rebuilt by the tooling repo's
  own scripts, which is the only thing that writes there.

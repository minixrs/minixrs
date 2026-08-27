# Slice 5.10b — MFS create/truncate + `VFS_OPEN` flags: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a MinixFS file *exist* — `FS_CREATE` and `FS_TRUNC` in the FS band, honoured from `VFS_OPEN` as `O_CREAT` / `O_TRUNC` — and close slice 5.10a's mid-write zone leak by staging the client's bytes before anything is allocated.

**Architecture:** Two new FS-band requests reusing `FS_LOOKUP`'s wire codec verbatim; an inode allocator and directory-entry insertion in MFS that grow a directory through the same `place_zone` a file uses; a zone-freeing path whose ordering is the allocator's read backwards; a second 4 KiB `.bss` staging buffer in MFS behind its own capability token; and a `flags` field on `VFS_OPEN` whose policy lives in `open.rs` as total functions. Five new init boot probes prove it, including one that issues 256 failing writes and then a good one.

**Tech Stack:** Rust (`no_std` freestanding EL0 servers + host-tested `no_std` libraries), MinixFS v3 on a compile-time RAM image, QEMU aarch64 boot markers as the integration test.

**Spec:** `docs/superpowers/specs/2026-08-25-mfs-create-truncate-design.md` — decisions `C1`…`C11` are referenced by name throughout. Read it before Task 1.

## Global Constraints

Copied verbatim from the spec and from `CLAUDE.md`. **Every task's requirements implicitly include this section.**

- **SPDX header first.** Every new `.rs` file begins with `// SPDX-License-Identifier: BSD-3-Clause` then `// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors`, before any other content.
- **`checked_add`, never `+`, for offset/length arithmetic** in `servers/`, `drivers/`, `fs/`, `userland/`. `[profile.release]` sets `overflow-checks = false`, so `off + 4` *wraps* in the shipped binary while panicking under `cargo test`. Give every new accessor a `usize::MAX` test.
- **`fs/mfs` is `#![forbid(unsafe_code)]` unconditionally** — the library. The server's `main.rs` is where the two `.bss` buffers' `unsafe` lives, each with a `// SAFETY:` comment.
- **Nothing is held across a block fetch.** `Blocks::read` takes `&mut self` and returns a borrow tied to it. Every intermediate in the new paths is a `Copy` scalar.
- **Every device-derived loop has a cap.** A corrupt inode must not spin MFS, which would block VFS, which would block init.
- **A server stack is exactly one page** (`uspace::SERVER_STACK_BYTES`). A 4 KiB local faults into VM's SIGSEGV arm, which prints nothing `tests/qemu-boot.forbidden` catches. Buffers this size are `.bss` statics.
- **No granter field and no grant-offset field** anywhere in the FS band. The granter is the kernel-stamped `m_source`; VFS re-grants per round.
- **Errno relay rules:** a `BDEV_*` failure becomes `EIO` (the client addressed a *file*); a `SYS_SAFECOPY` failure is relayed **verbatim** (`EPERM` and `EFAULT` are different caller bugs).
- **MFS is degraded, never fatal and never a panic** past `sef_startup`.
- **Commits:** `git commit -s` (DCO sign-off, mandatory), GPG signing on (never `--no-gpg-sign`), never `--no-verify`. Work on branch `feature/slice-5.10b-mfs-create-truncate`. **A subagent may commit; it may never push, never open a PR, never `--force`.**
- **Blocking gates before any push:** `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings` and the same with `--no-default-features`; **and `cargo clippy -p minixrs-mfs --features server -- -D warnings`**, which is the only invocation that compiles MFS's `main.rs` at all.
- **`fs/mfs/src/main.rs` is behind `[[bin]] required-features = ["server"]`**, so no CI job compiles it. **Every line with a decision in it belongs in the library.**
- **Boot verification:** `timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot.log 2>&1` (exit 124 is the healthy status), then `tools/check-boot-log.sh /tmp/boot.log`. Budget ~5 s for rebuild + UEFI before the kernel's first byte. Grep with `grep -a`.

---

## File Structure

| File | Responsibility after this slice |
|---|---|
| `kernel-shared/src/callnr.rs` | `FS_CREATE`, `FS_TRUNC`, `NR_FS_MSGS = 6`, `VFS_FLAGS_OFF` |
| `kernel-shared/src/fcntl.rs` *(new)* | `open(2)` flag values and the "which bits are honoured" mask |
| `kernel-shared/src/rootfs.rs` | image contents ABI: `ROOTFS_NINODES = 128` + the six new paths and their bytes |
| `tools/gen-c-headers/src/callnr_h.rs` | the hand-maintained `bands()` list gains two rows |
| `tools/mkfs-mfs/src/manifest.rs` | `Entry::hole` + `Manifest::add_sparse` |
| `tools/mkfs-mfs/src/image.rs` | hole validation, hole-aware `blocks_for` / `write_data` |
| `tools/mkfs-mfs/src/verify.rs` | `free_inodes` |
| `kernel/build.rs` | manifest gains `/full`, `/etc/holey`, `/etc/deny`; headroom checks widen |
| `fs/mfs/src/write.rs` | `bitmap_clear`, `DirentSlot`/`dirent_slot`, `dir_append_offset`, `indirect_slots_used` |
| `fs/mfs/src/walk.rs` | `split_basename` |
| `fs/mfs/src/proto.rs` | `parse_trunc` |
| `fs/mfs/src/main.rs` | `Stage`, restaged `do_write`, `alloc_inode`, `do_create`, `free_zone`, `do_trunc` |
| `servers/vfs/src/open.rs` | `OpenRequest::flags`, `OpenFlags`, `validate_flags` |
| `servers/vfs/src/main.rs` | `do_open` routing, `fs_path_request`/`fs_create`/`fs_trunc`, `fs_denials` 10 → 14 |
| `userland/init/src/main.rs` | five new probes, `open_denials` 7 → 11 |
| `tests/qemu-boot.{expected,forbidden}` | the new markers and their FAIL spellings |
| `book/`, `docs/plan.md`, `docs/plans/phase-5-musl-fs.md` | documentation and slice status |

---

## Task 1: The ABI — FS band, `VFS_OPEN` flags, `fcntl.rs`, C headers

**Files:**
- Create: `kernel-shared/src/fcntl.rs`
- Modify: `kernel-shared/src/lib.rs` (add `pub mod fcntl;` in alphabetical position)
- Modify: `kernel-shared/src/callnr.rs` (the FS band near `FS_WRITE`; the VFS payload offsets near `VFS_PATH_LEN_OFF`)
- Modify: `tools/gen-c-headers/src/callnr_h.rs` (`bands()`, the "file-system requests" entry)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `callnr::{FS_CREATE, FS_TRUNC, VFS_FLAGS_OFF}` (`NR_FS_MSGS` becomes `6`); `fcntl::{O_ACCMODE, O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_TRUNC, O_KNOWN, O_UNKNOWN_BIT}`, all `i32`.

- [ ] **Step 1: Write `kernel-shared/src/fcntl.rs`**

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `open(2)` flags (slice 5.10b).
//!
//! **The Linux/musl values**, for decision D7's reason applied to a second ABI:
//! musl's `open()` passes its own `O_CREAT` straight to the syscall, so matching
//! the numbers means the `__minixrs_syscall` shim will need no translation table
//! when it grows `openat`. Today's only client is `userland/init`, which is Rust
//! — the choice is forward-looking, and that is the honest framing, not a claim
//! that C uses it now.
//!
//! **Not emitted by `tools/gen-c-headers`**, exactly like the `AT_*` auxv values
//! slice 5.5 added: musl's own `fcntl.h` defines these, and a second definition
//! in `minixrs/*.h` would be a redefinition in any translation unit that included
//! both. The `const _`s below are what pin the values instead.

/// Mask selecting the access mode.
pub const O_ACCMODE: i32 = 0o3;
/// Access mode: read only.
pub const O_RDONLY: i32 = 0;
/// Access mode: write only.
pub const O_WRONLY: i32 = 1;
/// Access mode: read and write.
pub const O_RDWR: i32 = 2;

/// Create the file if it does not exist.
pub const O_CREAT: i32 = 0o100;
/// Discard the contents of a file that does exist.
pub const O_TRUNC: i32 = 0o1000;

/// Every bit this build reads. Any other bit in an `open` request is `EINVAL`.
///
/// The access mode is in here because it is **accepted and ignored**: there is no
/// uid, no gid and no permission check anywhere in the tree, so honouring it
/// would be a check with nothing behind it. `O_CREAT` and `O_TRUNC` are in here
/// because they are acted on.
pub const O_KNOWN: i32 = O_ACCMODE | O_CREAT | O_TRUNC;

/// A flag bit this build does **not** honour — what a denial probe aims at.
///
/// Derived from [`O_KNOWN`] rather than written as a literal, so that a flag
/// becoming real makes the probe using it fail loudly instead of passing
/// vacuously. That is slice 5.8's `VFS_WRITE + 1` lesson and slice 5.10a's
/// `write-file` lesson, applied before the fact rather than after.
///
/// `O_KNOWN + 1` sets at least one bit outside `O_KNOWN` (the carry stops at the
/// first clear bit), and the mask keeps only those — so the result is non-zero
/// and disjoint from `O_KNOWN` for any non-negative `O_KNOWN`.
pub const O_UNKNOWN_BIT: i32 = (O_KNOWN + 1) & !O_KNOWN;

// The values are Linux's, and that identity is the whole point — a literal test
// rather than a restatement of the definitions above.
const _: () = assert!(O_ACCMODE == 3);
const _: () = assert!(O_RDONLY == 0);
const _: () = assert!(O_WRONLY == 1);
const _: () = assert!(O_RDWR == 2);
const _: () = assert!(O_CREAT == 64);
const _: () = assert!(O_TRUNC == 512);

// The access mode and the behaviour bits must not overlap, or masking one would
// silently read the other.
const _: () = assert!(O_ACCMODE & (O_CREAT | O_TRUNC) == 0);
const _: () = assert!(O_CREAT & O_TRUNC == 0);

// The denial probe's bit really is outside what this build honours.
const _: () = assert!(O_UNKNOWN_BIT != 0);
const _: () = assert!(O_UNKNOWN_BIT & O_KNOWN == 0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_mode_masks_out_of_a_combined_flag_word() {
        // The one piece of arithmetic a caller performs on these.
        assert_eq!((O_RDWR | O_CREAT | O_TRUNC) & O_ACCMODE, O_RDWR);
        assert_eq!((O_WRONLY | O_TRUNC) & O_ACCMODE, O_WRONLY);
        assert_eq!(O_CREAT & O_ACCMODE, O_RDONLY);
    }

    #[test]
    fn every_honoured_bit_is_in_the_known_mask() {
        for flag in [O_ACCMODE, O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_TRUNC] {
            assert_eq!(flag & !O_KNOWN, 0, "flag {flag:o} is outside O_KNOWN");
        }
    }

    #[test]
    fn the_unknown_probe_bit_is_rejected_by_the_known_mask() {
        // What `open::validate_flags` will test it with.
        assert_ne!(O_UNKNOWN_BIT & !O_KNOWN, 0);
    }
}
```

- [ ] **Step 2: Register the module**

In `kernel-shared/src/lib.rs`, add `pub mod fcntl;` next to the other `pub mod` lines, keeping alphabetical order (it lands between `execstack`/`execimage` and `grant` — check the file and match).

- [ ] **Step 3: Add the two FS requests**

In `kernel-shared/src/callnr.rs`, immediately after `pub const FS_WRITE: i32 = FS_RQ_BASE + 3;`:

```rust
/// VFS → FS server: create a regular file.
///
/// **Payload is [`FS_LOOKUP`]'s, field for field** — the path inline at
/// [`FS_PATH_OFF`], NUL-padded to [`FS_PATH_MAX`] — and **so is the reply**:
/// [`FS_INO_OFF`] / [`FS_MODE_OFF`] / [`FS_SIZE_OFF`], with `m_type = OK`. One
/// wire codec serves both, and VFS classifies either answer through the same
/// function. That is [`FS_WRITE`]-reuses-[`FS_READ`] applied to the control
/// plane.
///
/// The FS server resolves the parent itself, because the band's rule since slice
/// 5.8 is that the control plane travels inline and a create is a path operation.
/// Splitting the path in VFS and sending `{parent_ino, name}` would put path
/// syntax in two servers and cost an extra `FS_LOOKUP`.
///
/// **There is no mode field.** There is no uid, no gid and no permission logic
/// anywhere in the tree, so a mode would be a value nothing reads — and a field
/// with one legal value is worse than no field. The server creates
/// `I_REGULAR | 0o644` with `nlinks = 1`. `open(2)`'s `mode_t` argument is
/// dropped by VFS until a permission model exists.
///
/// **An existing name is `EEXIST`**, not the existing inode: VFS only sends this
/// after a lookup answered `ENOENT`, so the strict answer costs nothing and is
/// what `O_EXCL` will need. Returning the existing inode would make "created" and
/// "found" indistinguishable on the wire and hide a duplicate-entry bug behind a
/// success. `ENOENT` when the parent is missing, `ENOTDIR` when it is a file,
/// `ENOSPC` when there is no free inode or the directory cannot grow.
pub const FS_CREATE: i32 = FS_RQ_BASE + 4;

/// VFS → FS server: discard a regular file's contents.
///
/// Payload: the inode number at [`FS_INO_OFF`]`..+4` (i32). Reply `m_type` is
/// `OK`, with no payload.
///
/// **It truncates to zero and has no length field.** `O_TRUNC` is the only
/// client, and there is no `ftruncate()` anywhere in the tree — no VFS request,
/// no musl wrapper — so a length field would ship five unreachable behaviours
/// (shrink-to-N, extend, no-op, past-EOF, negative) to serve one reachable one.
/// It is a request of its own rather than a flag on [`FS_CREATE`] because VFS
/// must be able to truncate a file that already exists, which is precisely what
/// `O_TRUNC` means.
///
/// `EISDIR` for a directory and `EINVAL` for any other non-regular inode — the
/// same guards, with the same wording, `FS_WRITE` applies.
pub const FS_TRUNC: i32 = FS_RQ_BASE + 5;
```

Then change `pub const NR_FS_MSGS: usize = 4;` to `= 6;`.

- [ ] **Step 4: Add the `VFS_OPEN` flags field**

In `kernel-shared/src/callnr.rs`, after `pub const VFS_PATH_LEN_OFF: usize = 8;`:

```rust
/// Offset of the open flags in a `VFS_OPEN` payload (i32).
///
/// Values are [`crate::fcntl`]'s, which are musl's. A new *field* on an existing
/// request rather than a new request, so [`NR_VFS_MSGS`] does not move.
pub const VFS_FLAGS_OFF: usize = 12;
```

Replace the existing `const _: () = assert!(VFS_PATH_LEN_OFF + 4 <= 96);` with:

```rust
const _: () = assert!(VFS_PATH_LEN_OFF + 4 <= VFS_FLAGS_OFF);
const _: () = assert!(VFS_FLAGS_OFF + 4 <= 96);
```

Also update `VFS_OPEN`'s own doc comment (around line 542) so it names the third field.

- [ ] **Step 5: Add the two rows to the hand-maintained C-header band list**

In `tools/gen-c-headers/src/callnr_h.rs`, in the "file-system requests" `Band`, extend `members`:

```rust
            members: vec![
                ("FS_READSUPER", callnr::FS_READSUPER),
                ("FS_LOOKUP", callnr::FS_LOOKUP),
                ("FS_READ", callnr::FS_READ),
                ("FS_WRITE", callnr::FS_WRITE),
                ("FS_CREATE", callnr::FS_CREATE),
                ("FS_TRUNC", callnr::FS_TRUNC),
            ],
```

This list is **hand-maintained**: bumping `NR_FS_MSGS` without adding the rows leaves the constants silently absent from the generated header and the `c-headers` CI gate still passes, because it compiles a header that simply never mentions them. `every_band_member_list_matches_its_count` is what catches it — and it caught exactly this in slice 5.10a.

Do **not** add `O_CREAT`/`O_TRUNC` anywhere in `gen-c-headers`; see `fcntl.rs`'s module docs.

- [ ] **Step 6: Run the tests**

```bash
cargo test -p minixrs-kernel-shared
cargo test -p minixrs-gen-c-headers
```

Expected: PASS. If `every_band_member_list_matches_its_count` fails, Step 5 was skipped or miscounted.

- [ ] **Step 7: Regenerate and compile the C headers**

```bash
cargo gen-c-headers
clang -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsyntax-only \
  -ffreestanding -nostdlibinc --target=aarch64-unknown-linux-musl \
  -Itarget/gen-c-headers/include target/gen-c-headers/abi-selftest.c
grep -n 'FS_CREATE\|FS_TRUNC' target/gen-c-headers/include/minixrs/callnr.h
```

Expected: the clang invocation exits 0 and the grep prints both constants. The headers are a build artifact under `target/` and are **never committed**.

- [ ] **Step 8: Lint and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add kernel-shared/src/fcntl.rs kernel-shared/src/lib.rs kernel-shared/src/callnr.rs \
        tools/gen-c-headers/src/callnr_h.rs
git commit -s -m "feat(abi): FS_CREATE, FS_TRUNC, and the VFS_OPEN flags field (slice 5.10b)

The FS band grows to six requests. FS_CREATE reuses FS_LOOKUP's payload and
reply field for field, so one wire codec serves both and VFS classifies either
answer through the same function; FS_TRUNC carries an inode and nothing else,
because O_TRUNC is its only client and there is no ftruncate anywhere in the
tree to give a length field a second behaviour.

VFS_OPEN gains a third field rather than a second request. The flag values are
musl's, so the syscall shim will need no translation table when it grows
openat, and they are deliberately not emitted by gen-c-headers -- musl's own
fcntl.h defines them, and a second definition would be a redefinition in any
translation unit including both.

The bands() list in callnr_h.rs is hand-maintained, so the two rows there are
not optional: without them the c-headers gate compiles a header that never
mentions the constants and passes anyway."
```

---

## Task 2: The image — `rootfs.rs` contents, sparse files, `/full`, 128 inodes

**Files:**
- Modify: `kernel-shared/src/rootfs.rs`
- Modify: `tools/mkfs-mfs/src/manifest.rs`
- Modify: `tools/mkfs-mfs/src/image.rs`
- Modify: `tools/mkfs-mfs/src/verify.rs`
- Modify: `kernel/build.rs` (`build_rootfs`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `rootfs::{ROOTFS_NINODES, ROOTFS_DIRENT_SIZE, ROOTFS_HOLEY_PATH, ROOTFS_HOLEY_LEN, rootfs_holey_byte, ROOTFS_HOLEY_TEXT, ROOTFS_DENY_PATH, ROOTFS_FULL_DIR, ROOTFS_FULL_ENTRIES, ROOTFS_FULL_NEW_PATH, ROOTFS_DIRGROW_TEXT, ROOTFS_CREATE_PATH, ROOTFS_CREATE_TEXT, ROOTFS_LEAK_PATH, ROOTFS_LEAK_TEXT, ROOTFS_LEAK_PROBES, ROOTFS_RUNTIME_ZONES, ROOTFS_RUNTIME_INODES}`; `manifest::Entry::hole` and `Manifest::add_sparse(path, bytes, hole)`; `verify::free_inodes(img) -> Option<usize>`.

**Why `/full` and `/etc/holey` exist at all (C9, C10).** Two arms this slice adds are otherwise unreachable in *both* boot configurations, which is the failure mode `/etc/pattern` (5.7) and `device_teardown_selftest` (5.3) exist to prevent:

- Directory growth: `/` holds 4 entries and `/etc` holds 5, against 64 slots in a block. `/full` ships 62 empty files so its single block holds exactly 64 used slots, and **one** create at boot must allocate a directory zone. Forcing it from init instead would cost ~60 creates and several hundred device round trips.
- The `dirty` half of the inode write-back condition: with no `lseek`, every write starts at a descriptor's position and runs forward, so a write that assigns a zone **always** extends the file. The case needs a *hole below EOF*, which can only come from a sparse write (needs seek), an extending truncate (ruled out by C4), or the image. Slice 5.10a's hand-off claims `FS_TRUNC` makes it reachable; that is wrong, and this is the correction.

- [ ] **Step 1: Write the failing mkfs tests for the sparse entry**

Append to `tools/mkfs-mfs/src/image.rs`'s `mod tests`:

```rust
    #[test]
    fn a_sparse_file_reads_back_whole_but_allocates_only_its_tail() {
        // The hole occupies no zone, so the image spends one block on a
        // two-block file -- and `read_inode_bytes` still returns all 8192 bytes,
        // because a zero zone pointer *means* zeroes.
        let mut bytes = vec![0u8; 2 * MFS_BLOCK_SIZE];
        for (i, b) in bytes.iter_mut().enumerate().skip(MFS_BLOCK_SIZE) {
            *b = (i % 251) as u8;
        }
        let mut m = Manifest::new();
        m.add_sparse("/etc/holey", bytes.clone(), MFS_BLOCK_SIZE);
        let img = build_image(&m).expect("a one-hole file is buildable");

        assert_eq!(verify::read_file(&img, "/etc/holey"), Some(bytes));
        let (_, node) = verify::lookup(&img, "/etc/holey").expect("it is there");
        assert_eq!(node.zone[0], 0, "the hole must occupy no zone");
        assert_ne!(node.zone[1], 0, "the tail must be allocated");
        assert_eq!(node.size, 2 * MFS_BLOCK_SIZE as i32);
    }

    #[test]
    fn a_hole_that_is_not_whole_blocks_or_not_zero_is_refused() {
        // Both would have the image claim content it does not store.
        for (bytes, hole) in [
            (vec![0u8; 2 * MFS_BLOCK_SIZE], MFS_BLOCK_SIZE + 1), // not block-aligned
            (vec![1u8; 2 * MFS_BLOCK_SIZE], MFS_BLOCK_SIZE),     // prefix not zero
            (vec![0u8; MFS_BLOCK_SIZE], MFS_BLOCK_SIZE),         // wholly hole
        ] {
            let mut m = Manifest::new();
            m.add_sparse("/etc/holey", bytes, hole);
            assert_eq!(
                build_image(&m),
                Err(MkfsError::BadHole("/etc/holey".to_string())),
                "hole {hole}"
            );
        }
    }

    #[test]
    fn the_shared_dirent_size_matches_the_format_crate() {
        // `kernel-shared` cannot depend on `minixrs-mfs` (the dependency runs the
        // other way), so `ROOTFS_DIRENT_SIZE` is a duplicate. This crate depends
        // on both and is therefore the only place that can pin them equal.
        assert_eq!(
            minixrs_kernel_shared::rootfs::ROOTFS_DIRENT_SIZE,
            minixrs_mfs::dirent::DIRENT_SIZE
        );
    }

    #[test]
    fn a_directory_of_exactly_sixty_four_entries_fills_one_block() {
        // C10's arithmetic, checked against a real image rather than reasoned
        // about: `.` + `..` + ROOTFS_FULL_ENTRIES must be exactly one block, so
        // that the first create in it has to grow the directory.
        use minixrs_kernel_shared::rootfs::ROOTFS_FULL_ENTRIES;
        let mut m = Manifest::new();
        for i in 0..ROOTFS_FULL_ENTRIES {
            m.add(format!("/full/f{i:02}"), Vec::new());
        }
        let img = build_image(&m).expect("62 empty files fit");
        let (_, dir) = verify::lookup(&img, "/full").expect("the directory is there");
        assert_eq!(dir.size, MFS_BLOCK_SIZE as i32, "exactly full, not one short");
    }
```

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p minixrs-mkfs-mfs
```

Expected: FAIL — `add_sparse` and `MkfsError::BadHole` do not exist, and `ROOTFS_DIRENT_SIZE` / `ROOTFS_FULL_ENTRIES` are undefined.

- [ ] **Step 3: Add the image-contents constants to `kernel-shared/src/rootfs.rs`**

Change `ROOTFS_NINODES` and its doc:

```rust
/// Inodes in the root filesystem image.
///
/// **128, i.e. two inode-table blocks.** 64 was one block and ample through
/// slice 5.9; slice 5.10b's `/full` directory (see [`ROOTFS_FULL_ENTRIES`]) costs
/// 62 inodes on its own. Raising it shifts `first_data_zone` by one block, which
/// moves every zone number in the image — the layout unit tests and mkfs's
/// fixtures move with it.
pub const ROOTFS_NINODES: u32 = 128;
```

Append, after the `ROOTFS_SCRATCH_*` block:

```rust
/// Bytes per MinixFS v3 directory entry.
///
/// Duplicated from `minixrs_mfs::dirent::DIRENT_SIZE` — `fs/mfs` depends on this
/// crate, so the dependency cannot run the other way. `tools/mkfs-mfs` depends on
/// both and carries the test that pins them equal.
pub const ROOTFS_DIRENT_SIZE: usize = 64;

/// A **sparse** file the image ships, for the write-back proof.
///
/// Its first block is a hole and its second holds a pattern, so a write at
/// position 0 assigns `zone[0]` while `size` does not move. That is the only way
/// to reach the second half of MFS's write-back condition — "a zone was assigned
/// **or** the size grew" — because with no `lseek` every write runs forward from
/// a descriptor's position and therefore always extends the file. Slice 5.10a
/// left that half unproven and predicted `FS_TRUNC` would reach it; it does not,
/// and this file is the correction.
pub const ROOTFS_HOLEY_PATH: &str = "/etc/holey";

/// Length of [`ROOTFS_HOLEY_PATH`]: two blocks, the first of them a hole.
pub const ROOTFS_HOLEY_LEN: usize = 2 * BDEV_BLOCK_SIZE;

/// Byte `i` of [`ROOTFS_HOLEY_PATH`]'s **shipped** contents.
///
/// Zero throughout the hole — which is what a hole reads as, so the image is
/// self-consistent — and a position-dependent pattern after it. Skewed off
/// [`rootfs_scratch_byte`] and [`rootfs_pattern_byte`] so that reading the wrong
/// file is a mismatch rather than a coincidence.
pub const fn rootfs_holey_byte(i: usize) -> u8 {
    if i < BDEV_BLOCK_SIZE {
        0
    } else {
        ((i + 23) % ROOTFS_SCRATCH_PERIOD) as u8
    }
}

/// What init writes at position 0 of [`ROOTFS_HOLEY_PATH`], filling the hole.
pub const ROOTFS_HOLEY_TEXT: &[u8] = b"minix.rs holey: filled at zero\n";

/// A file the image ships **empty**, as the `EEXIST` probe's target.
///
/// Read by nothing else, so a probe that accidentally *succeeded* in creating a
/// second entry for this name would corrupt no proof but its own. It exists
/// because the probe re-resolves the name afterwards and compares inode numbers:
/// a dropped `EEXIST` would insert a duplicate entry shadowing the first,
/// silently, with every other marker still green.
pub const ROOTFS_DENY_PATH: &str = "/etc/deny";

/// A directory whose single block is **exactly full**, so that the first create
/// in it must allocate a second directory zone.
pub const ROOTFS_FULL_DIR: &str = "/full";

/// Files [`ROOTFS_FULL_DIR`] ships, all zero-length.
///
/// `.` and `..` plus these must be exactly one block of entries — the `const _`
/// below is what enforces it — so directory growth is on a boot marker rather
/// than being an arm no QEMU boot executes. They cost 62 inodes and no zones.
pub const ROOTFS_FULL_ENTRIES: usize = 62;

/// The create that must grow [`ROOTFS_FULL_DIR`].
pub const ROOTFS_FULL_NEW_PATH: &str = "/full/new";

/// What init writes to [`ROOTFS_FULL_NEW_PATH`].
pub const ROOTFS_DIRGROW_TEXT: &[u8] = b"minix.rs dirgrow by init\n";

/// A file that is **not** in the image, which init creates at boot.
pub const ROOTFS_CREATE_PATH: &str = "/etc/new";

/// What init writes to [`ROOTFS_CREATE_PATH`].
pub const ROOTFS_CREATE_TEXT: &[u8] = b"minix.rs created by init\n";

/// A file init creates to prove that a *failing* write allocates nothing.
pub const ROOTFS_LEAK_PATH: &str = "/etc/leak";

/// What init writes to [`ROOTFS_LEAK_PATH`] once the failing writes are done.
pub const ROOTFS_LEAK_TEXT: &[u8] = b"minix.rs leak: nothing lost\n";

/// Failing writes the leak probe issues before its one good write.
///
/// **[`ROOTFS_IMAGE_BLOCKS`], which is greater than any possible free-zone count
/// in the image**, so the probe is config-independent *by construction* rather
/// than by measurement — no number here differs between the musl, SDK and
/// sysroot-absent `hello` flavours, which is the slice-5.5/5.6 trap. Before the
/// staging fix each failure leaked one zone, so this many of them would exhaust
/// the image and the probe's final good write would answer `ENOSPC`.
pub const ROOTFS_LEAK_PROBES: usize = ROOTFS_IMAGE_BLOCKS as usize;

/// Zones the boot-time probes allocate at **runtime**, in total.
///
/// `/etc/scratch`'s eight data blocks and its indirect block
/// ([`ROOTFS_SCRATCH_GROWTH_ZONES`]), plus one each for `/etc/new`,
/// `/full/new`, `/etc/holey`'s filled hole and `/etc/leak`'s one good write, plus
/// one for `/full`'s second directory block. Checked against the *built* image by
/// `kernel/build.rs`, for the reason that check already carries: the image's
/// largest file is `/bin/hello`, whose size is a property of the toolchain
/// flavour, so no unit test over a fixture measures this image.
pub const ROOTFS_RUNTIME_ZONES: usize = ROOTFS_SCRATCH_GROWTH_ZONES + 5;

/// Inodes the boot-time probes allocate at runtime: `/etc/new`, `/full/new` and
/// `/etc/leak`.
pub const ROOTFS_RUNTIME_INODES: usize = 3;

// C10: `.` + `..` + the filler files must be exactly one block of entries. One
// short and the create finds a free slot; one over and mkfs has already grown the
// directory, so the arm this exists for stays unreachable either way.
const _: () = assert!(2 + ROOTFS_FULL_ENTRIES == BDEV_BLOCK_SIZE / ROOTFS_DIRENT_SIZE);

// The sparse file's hole is exactly its first block, and its tail is real.
const _: () = assert!(ROOTFS_HOLEY_LEN == 2 * BDEV_BLOCK_SIZE);
// ...and what init writes into the hole fits inside it, so the write assigns
// `zone[0]` and touches nothing else.
const _: () = assert!(!ROOTFS_HOLEY_TEXT.is_empty());
const _: () = assert!(ROOTFS_HOLEY_TEXT.len() < BDEV_BLOCK_SIZE);

// Each of the three created files fits one FS transfer, so its proof is one
// round trip and its marker's byte count is a literal.
const _: () = assert!(ROOTFS_CREATE_TEXT.len() <= BDEV_BLOCK_SIZE);
const _: () = assert!(ROOTFS_DIRGROW_TEXT.len() <= BDEV_BLOCK_SIZE);
const _: () = assert!(ROOTFS_LEAK_TEXT.len() <= BDEV_BLOCK_SIZE);

// The leak probe must out-number any free-zone count the image can have.
const _: () = assert!(ROOTFS_LEAK_PROBES >= ROOTFS_IMAGE_BLOCKS as usize);

// Necessary conditions only — the sufficient checks are `kernel/build.rs`'s,
// against the bytes it just built.
const _: () = assert!(ROOTFS_RUNTIME_ZONES < ROOTFS_IMAGE_BLOCKS as usize);
const _: () = assert!(ROOTFS_RUNTIME_INODES < ROOTFS_NINODES as usize);
```

- [ ] **Step 4: Add the sparse entry to the manifest**

In `tools/mkfs-mfs/src/manifest.rs`, extend `Entry` and `Manifest`:

```rust
pub struct Entry {
    /// Absolute path, exactly `/<dir>/<name>`.
    pub path: String,
    /// File contents, verbatim — the **whole** file, hole included.
    pub bytes: Vec<u8>,
    /// Leading bytes to leave **unallocated**: a hole.
    ///
    /// `0` for every ordinary file. When non-zero it must be a whole multiple of
    /// the block size and the corresponding prefix of `bytes` must already be
    /// zero — the image would otherwise claim content it does not store. Both are
    /// checked when the image is built, not here, so a caller assembling a
    /// manifest hears about every problem at once.
    ///
    /// **One variant with one caller**, and deliberately so: it exists for
    /// `/etc/holey`, whose whole job is making a zone assignment that does not
    /// move the file's size reachable. A general sparse-file description would be
    /// code with no second user.
    pub hole: usize,
}
```

`add` sets `hole: 0`; add beside it:

```rust
    /// Add a file with a leading hole. See [`Entry::hole`].
    pub fn add_sparse(
        &mut self,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        hole: usize,
    ) -> &mut Self {
        self.entries.push(Entry {
            path: path.into(),
            bytes: bytes.into(),
            hole,
        });
        self
    }
```

Fix `a_manifest_keeps_insertion_order_and_sums_its_bytes` if it constructs `Entry` literally.

- [ ] **Step 5: Make the writer hole-aware**

In `tools/mkfs-mfs/src/image.rs`:

Add the error variant next to `Duplicate`:

```rust
    /// An entry's hole is not a whole number of blocks, covers the whole file, or
    /// names a prefix of the contents that is not already zero.
    BadHole(String),
```

and its `Display` arm:

```rust
            Self::BadHole(p) => write!(
                f,
                "{p:?} has a hole that is not whole blocks of zeroes inside the file"
            ),
```

In `Tree::plan`'s validation loop (right after `split_path`), add:

```rust
            if entry.hole != 0 {
                let bad = || MkfsError::BadHole(entry.path.clone());
                if !entry.hole.is_multiple_of(MFS_BLOCK_SIZE) || entry.hole >= entry.bytes.len() {
                    return Err(bad());
                }
                if entry.bytes[..entry.hole].iter().any(|&b| b != 0) {
                    return Err(bad());
                }
            }
```

Make `blocks_for` take the hole:

```rust
/// Blocks a file occupies, including its indirect block if it needs one and
/// excluding any leading hole.
///
/// The *indirect* test is on the total block count, not the allocated one: a hole
/// does not renumber the zones after it, so a file whose last block sits past the
/// seventh still needs an indirect block however much of its front is missing.
fn blocks_for(len: usize, hole: usize) -> u32 {
    let total = len.div_ceil(MFS_BLOCK_SIZE);
    let holed = hole / MFS_BLOCK_SIZE;
    let indirect = usize::from(total > NR_DIRECT_ZONES);
    (total.saturating_sub(holed) + indirect) as u32
}
```

`check_block_budget` currently chains directory bytes and file bytes through one iterator; split it so each side passes its own hole (directories are never sparse, so they pass `0`):

```rust
    fn check_block_budget(&self) -> Result<(), MkfsError> {
        let have = self.available_zones();
        let mut needed = 0u32;
        let sized = self
            .dir_blocks
            .iter()
            .map(|(_, b)| (b.as_slice(), 0usize))
            .chain(
                self.manifest
                    .entries
                    .iter()
                    .map(|e| (e.bytes.as_slice(), e.hole)),
            );
        for (bytes, hole) in sized {
            if bytes.len() > max_file_bytes() {
                return Err(MkfsError::TooBig {
                    needed: blocks_for(bytes.len(), hole),
                    have,
                });
            }
            needed += blocks_for(bytes.len(), hole);
        }
        if needed > have {
            return Err(MkfsError::TooBig { needed, have });
        }
        Ok(())
    }
```

Make `write_data` skip the hole's blocks:

```rust
fn write_data(
    img: &mut Image,
    alloc: &mut ZoneAlloc,
    data: &[u8],
    hole: usize,
) -> Result<[u32; NR_TZONES], MkfsError> {
    let mut zone = [0u32; NR_TZONES];
    let mut indirect: Option<u32> = None;
    let hole_blocks = hole / MFS_BLOCK_SIZE;

    for (i, chunk) in data.chunks(MFS_BLOCK_SIZE).enumerate() {
        // A hole: no zone at all. The pointer stays 0, which is exactly what the
        // reader treats as a hole, and the prefix is already zero (checked in
        // `plan`), so the file's bytes are unchanged by not storing them.
        if i < hole_blocks {
            continue;
        }
        let z = alloc.alloc()?;
        img.block_mut(z)[..chunk.len()].copy_from_slice(chunk);
        ...unchanged...
    }
    Ok(zone)
}
```

Update its two call sites: the directory loop passes `0`, the file loop passes `entry.hole`.

- [ ] **Step 6: Add `free_inodes` to `verify.rs`**

```rust
/// Free inodes in `img` — [`free_zones`]'s twin.
///
/// This exists for one caller: `kernel/build.rs`, which needs to know that the
/// image it *just built* leaves room for the files init creates at boot. Like
/// `free_zones` it cannot be settled by a unit test on a fixture, because the
/// image's contents depend on the toolchain flavour.
pub fn free_inodes(img: &[u8]) -> Option<usize> {
    let l = image_layout(img)?;
    let sb = superblock(img)?;
    let mut free = 0;
    for ino in 1..=sb.ninodes {
        if bit_set(img, l.imap_start, imap_bit(ino))? {
            continue;
        }
        free += 1;
    }
    Some(free)
}
```

Import `imap_bit` alongside the existing `zmap_bit`.

- [ ] **Step 7: Put the new files in the image**

In `kernel/build.rs`'s `build_rootfs`, extend the imports and the manifest:

```rust
    use minixrs_kernel_shared::rootfs::{
        ROOTFS_CREATE_PATH, ROOTFS_DENY_PATH, ROOTFS_FULL_DIR, ROOTFS_FULL_ENTRIES,
        ROOTFS_HELLO_PATH, ROOTFS_HOLEY_LEN, ROOTFS_HOLEY_PATH, ROOTFS_MOTD, ROOTFS_MOTD_PATH,
        ROOTFS_PATTERN_LEN, ROOTFS_PATTERN_PATH, ROOTFS_RUNTIME_INODES, ROOTFS_RUNTIME_ZONES,
        ROOTFS_SCRATCH_PATH, rootfs_holey_byte, rootfs_pattern_byte,
    };
```

(`ROOTFS_CREATE_PATH` is imported only for the assertion message; drop it if unused.)

```rust
    let pattern: Vec<u8> = (0..ROOTFS_PATTERN_LEN).map(rootfs_pattern_byte).collect();
    let holey: Vec<u8> = (0..ROOTFS_HOLEY_LEN).map(rootfs_holey_byte).collect();

    let mut manifest = Manifest::new();
    manifest
        .add(ROOTFS_HELLO_PATH, hello_bytes.to_vec())
        .add(ROOTFS_MOTD_PATH, ROOTFS_MOTD.to_vec())
        .add(ROOTFS_PATTERN_PATH, pattern)
        .add(ROOTFS_SCRATCH_PATH, Vec::new())
        // Slice 5.10b. `/etc/holey` is sparse: its first block is a hole, so a
        // write at position 0 assigns a zone without moving the file's size --
        // the only way to reach the `dirty` half of MFS's write-back condition,
        // since with no `lseek` every write runs forward and extends the file.
        .add_sparse(ROOTFS_HOLEY_PATH, holey, minixrs_mfs::MFS_BLOCK_SIZE)
        // The `EEXIST` probe's target, read by nothing else.
        .add(ROOTFS_DENY_PATH, Vec::new());

    // `/full` ships exactly enough zero-length files that its single directory
    // block is full, so the one create init makes in it *must* allocate a second
    // directory zone. Without it, directory growth is an arm no QEMU boot
    // executes -- the failure mode `/etc/pattern` and the device-teardown
    // selftest exist to prevent. Names are formatted here rather than named in
    // `kernel-shared` because nothing at run time resolves them; only the count
    // is shared, and `rootfs.rs` const-asserts the arithmetic.
    for i in 0..ROOTFS_FULL_ENTRIES {
        manifest.add(format!("{ROOTFS_FULL_DIR}/f{i:02}"), Vec::new());
    }
```

Widen the headroom check and add its inode twin:

```rust
    let free = minixrs_mkfs_mfs::verify::free_zones(&img)
        .expect("the image just built decodes its own layout");
    assert!(
        free >= ROOTFS_RUNTIME_ZONES,
        "the root image leaves {free} free zones, but the boot-time probes need \
         {ROOTFS_RUNTIME_ZONES} to grow at runtime. Its contents have outgrown \
         ROOTFS_IMAGE_BLOCKS -- raise that constant, or shrink what the image ships."
    );

    let free = minixrs_mkfs_mfs::verify::free_inodes(&img)
        .expect("the image just built decodes its own layout");
    assert!(
        free >= ROOTFS_RUNTIME_INODES,
        "the root image leaves {free} free inodes, but the boot-time probes create \
         {ROOTFS_RUNTIME_INODES} files. Raise ROOTFS_NINODES."
    );
```

- [ ] **Step 8: Run the tests and fix the shifted fixtures**

```bash
cargo test -p minixrs-mkfs-mfs -p minixrs-mfs -p minixrs-kernel-shared
```

Expected: the four new tests PASS. **`ROOTFS_NINODES` 64 → 128 adds an inode-table block, so `first_data_zone` moves by one and every hard-coded zone number in `mkfs`'s and `layout.rs`'s fixtures moves with it.** That is R2 in the spec: the failures are red tests rather than a corrupt image, but the diff is wider than it looks. Update each expected number from the new `layout(...)` values rather than by trial and error.

- [ ] **Step 9: Build the kernel, which runs `build_rootfs`**

```bash
MINIXRS_SDK=/nonexistent cargo kernel-aarch64
```

Expected: success. A `TooManyInodes` / `TooBig` panic names the constant to raise; the two `assert!`s above name the headroom that ran out.

- [ ] **Step 10: Boot, and confirm nothing regressed**

```bash
timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot-t2.log 2>&1
tools/check-boot-log.sh /tmp/boot-t2.log
```

Expected: every existing marker still PASS. The image grew and `first_data_zone` moved, so `fs.selfcheck`, `fs.indirect`, `bdev.tail` and `fs.write` are the ones that would notice.

- [ ] **Step 11: Lint and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add kernel-shared/src/rootfs.rs tools/mkfs-mfs/src kernel/build.rs fs/mfs/src/layout.rs
git commit -s -m "feat(mkfs): sparse files, a full directory, and 128 inodes (slice 5.10b)

Two arms slice 5.10b adds are unreachable in both boot configurations unless
the image supplies the shape for them, which is the failure mode /etc/pattern
and the device-teardown selftest exist to prevent.

/full ships 62 empty files so its single block holds exactly 64 used slots and
one create at boot must allocate a directory zone. /etc/holey is sparse -- a
hole then a pattern -- so a write at position 0 assigns a zone without moving
the file's size. That second case is the one slice 5.10a said FS_TRUNC would
reach: it does not, because with no lseek every write runs forward from a
descriptor's position and therefore always extends the file, so the case needs
a hole below EOF and only the image can supply one.

The sparse entry stays one variant with one caller. Raising ROOTFS_NINODES to
128 shifts first_data_zone by an inode-table block, which moves every zone
number in the fixtures; the headroom checks against the built image gain an
inode twin, since the probes now create files as well as grow them."
```

---

## Task 3: MFS library policy — `bitmap_clear`, `dirent_slot`, `dir_append_offset`, `split_basename`, `parse_trunc`

**Files:**
- Modify: `fs/mfs/src/write.rs`
- Modify: `fs/mfs/src/walk.rs`
- Modify: `fs/mfs/src/proto.rs`
- Test: the `mod tests` in each of those three files

**Interfaces:**
- Consumes: nothing.
- Produces, all pure and host-tested:
  - `write::bitmap_clear(block: &mut [u8], bit: u32) -> Option<()>`
  - `write::DirentSlot` — `Occupied(usize)` / `Free(usize)` / `Full`
  - `write::dirent_slot(block: &[u8], want: &str) -> DirentSlot`
  - `write::dir_append_offset(size: i32) -> Result<u64, i32>`
  - `write::indirect_slots_used(size: i32, bs: usize) -> Result<usize, i32>`
  - `walk::split_basename(path: &str) -> Result<(&str, &str), i32>`
  - `proto::parse_trunc(msg: &Message) -> i32`

Everything with a decision in it goes here because `fs/mfs/src/main.rs` is behind `required-features = ["server"]` and is compiled by no CI job.

- [ ] **Step 1: Write the failing tests in `fs/mfs/src/write.rs`**

Append to `mod tests`:

```rust
    // ----- bitmap_clear -----------------------------------------------------

    #[test]
    fn clearing_a_bit_uses_the_same_ordering_as_setting_one() {
        // Same byte, same mask. A divergence between the two would free a
        // different zone than the one the caller named -- silent corruption.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_set(&mut b, 9), Some(()));
        assert_eq!(b[1], 0b0000_0010);
        assert_eq!(bitmap_clear(&mut b, 9), Some(()));
        assert_eq!(b[1], 0);
    }

    #[test]
    fn clearing_a_bit_leaves_its_neighbours_alone() {
        let mut b = [0xffu8; 8];
        assert_eq!(bitmap_clear(&mut b, 9), Some(()));
        assert_eq!(b[1], 0b1111_1101);
        assert_eq!(b[0], 0xff);
        assert_eq!(b[2], 0xff);
    }

    #[test]
    fn clearing_an_already_free_bit_is_a_no_op_not_an_error() {
        // Truncate walks a file's zone array, which may hold holes.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_clear(&mut b, 3), Some(()));
        assert_eq!(b, [0u8; 8]);
    }

    #[test]
    fn clearing_a_bit_past_the_block_is_none_not_a_panic() {
        // `bitmap_set`'s rule: a caller that mixed up its bitmap arithmetic finds
        // out, rather than writing into the wrong byte.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_clear(&mut b, 64), None);
        assert_eq!(bitmap_clear(&mut b, u32::MAX), None);
    }

    // ----- dirent_slot ------------------------------------------------------

    /// One directory block: `.`, `..`, then whatever `names` says, and free slots
    /// for the rest.
    fn dir_block(names: &[(u32, &str)]) -> [u8; BS] {
        let mut b = [0u8; BS];
        let mut at = 0usize;
        for (ino, name) in names {
            let e = crate::dirent::DirEntry::new(*ino, name.as_bytes()).unwrap();
            b[at..at + crate::dirent::DIRENT_SIZE].copy_from_slice(&e.to_le_bytes());
            at += crate::dirent::DIRENT_SIZE;
        }
        b
    }

    #[test]
    fn an_existing_name_is_occupied_at_its_own_slot() {
        let b = dir_block(&[(1, "."), (1, ".."), (7, "motd")]);
        assert_eq!(dirent_slot(&b, "motd"), DirentSlot::Occupied(2));
    }

    #[test]
    fn a_missing_name_reports_the_first_free_slot() {
        let b = dir_block(&[(1, "."), (1, ".."), (7, "motd")]);
        assert_eq!(dirent_slot(&b, "new"), DirentSlot::Free(3));
    }

    #[test]
    fn a_freed_slot_in_the_middle_is_the_one_reported() {
        // Directories are not compacted, so a removed entry leaves a zeroed slot
        // behind and a create should reuse it rather than growing the directory.
        let mut b = dir_block(&[(1, "."), (1, ".."), (7, "motd"), (8, "pattern")]);
        b[2 * crate::dirent::DIRENT_SIZE..3 * crate::dirent::DIRENT_SIZE].fill(0);
        assert_eq!(dirent_slot(&b, "new"), DirentSlot::Free(2));
    }

    #[test]
    fn an_existing_name_wins_over_an_earlier_free_slot() {
        // The one ordering that matters. If `Free` short-circuited, a create
        // would insert a duplicate entry *before* the real one -- and the reader
        // stops at the first match, so the original would be shadowed silently.
        let mut b = dir_block(&[(1, "."), (1, ".."), (7, "motd"), (8, "keep")]);
        b[2 * crate::dirent::DIRENT_SIZE..3 * crate::dirent::DIRENT_SIZE].fill(0);
        assert_eq!(dirent_slot(&b, "keep"), DirentSlot::Occupied(3));
    }

    #[test]
    fn a_block_with_every_slot_used_is_full() {
        let names: Vec<(u32, String)> = (0..BS / crate::dirent::DIRENT_SIZE)
            .map(|i| (i as u32 + 1, format!("f{i:02}")))
            .collect();
        let refs: Vec<(u32, &str)> = names.iter().map(|(i, n)| (*i, n.as_str())).collect();
        let b = dir_block(&refs);
        assert_eq!(dirent_slot(&b, "new"), DirentSlot::Full);
        // ...and a name that *is* there is still found in a full block.
        assert_eq!(dirent_slot(&b, "f00"), DirentSlot::Occupied(0));
    }

    #[test]
    fn a_short_block_decodes_only_whole_entries() {
        // A trailing partial entry is ignored rather than half-decoded, so a
        // short read cannot synthesize a free slot out of whatever followed it.
        let b = dir_block(&[(1, "."), (1, "..")]);
        assert_eq!(dirent_slot(&b[..2 * crate::dirent::DIRENT_SIZE], "new"), DirentSlot::Full);
        assert_eq!(dirent_slot(&b[..2 * crate::dirent::DIRENT_SIZE + 8], "new"), DirentSlot::Full);
        assert_eq!(dirent_slot(&[], "new"), DirentSlot::Full);
    }

    // ----- dir_append_offset ------------------------------------------------

    #[test]
    fn an_appended_entry_lands_at_the_directorys_current_end() {
        assert_eq!(dir_append_offset(0), Ok(0));
        assert_eq!(dir_append_offset(BS as i32), Ok(BS as u64));
    }

    #[test]
    fn a_size_that_is_not_a_whole_number_of_entries_is_eio() {
        // A corrupt directory inode. Appending at a misaligned offset would
        // splice an entry across two others.
        assert_eq!(dir_append_offset(1), Err(EIO));
        assert_eq!(dir_append_offset(crate::dirent::DIRENT_SIZE as i32 - 1), Err(EIO));
    }

    #[test]
    fn a_negative_or_oversized_directory_is_eio() {
        // `dir_size`'s rules, inherited: a corrupt inode, not a caller error.
        assert_eq!(dir_append_offset(-1), Err(EIO));
        assert_eq!(dir_append_offset(crate::walk::MAX_DIR_BYTES as i32 + 1), Err(EIO));
    }

    #[test]
    fn a_directory_at_the_cap_cannot_grow_and_is_enospc() {
        // Distinct from EIO: the directory is well-formed, it is simply full.
        // `MAX_DIR_BYTES` is a whole number of blocks and therefore of entries,
        // so this is exactly the boundary.
        let cap = crate::walk::MAX_DIR_BYTES as i32;
        assert_eq!(dir_append_offset(cap), Err(ENOSPC));
        assert_eq!(
            dir_append_offset(cap - crate::dirent::DIRENT_SIZE as i32),
            Ok((cap - crate::dirent::DIRENT_SIZE as i32) as u64),
            "one entry short of the cap still fits"
        );
    }

    // ----- indirect_slots_used ----------------------------------------------

    #[test]
    fn a_file_inside_the_direct_zones_reaches_no_indirect_slot() {
        assert_eq!(indirect_slots_used(0, BS), Ok(0));
        assert_eq!(indirect_slots_used(SEAM as i32, BS), Ok(0));
    }

    #[test]
    fn a_file_past_the_seam_reaches_one_slot_per_block_past_it() {
        assert_eq!(indirect_slots_used(SEAM as i32 + 1, BS), Ok(1));
        assert_eq!(indirect_slots_used(SEAM as i32 + BS as i32, BS), Ok(1));
        assert_eq!(indirect_slots_used(SEAM as i32 + BS as i32 + 1, BS), Ok(2));
        // 32 KiB -- what init's write proof produces -- examines two slots, not
        // the block's 1024. That bound is C8, and it is what lets truncate work
        // with a single block buffer.
        assert_eq!(indirect_slots_used(32 * 1024, BS), Ok(2));
    }

    #[test]
    fn the_slot_count_is_capped_at_the_blocks_own_pointers() {
        // A corrupt size must not walk past the indirect block.
        assert_eq!(indirect_slots_used(i32::MAX, BS), Ok(BS / 4));
    }

    #[test]
    fn a_negative_size_or_zero_block_is_eio() {
        assert_eq!(indirect_slots_used(-1, BS), Err(EIO));
        assert_eq!(indirect_slots_used(0, 0), Err(EIO));
    }
```

Add `use minixrs_kernel_shared::error::ENOSPC;` to `write.rs`'s imports.

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p minixrs-mfs
```

Expected: FAIL with "cannot find function `bitmap_clear`" and friends.

- [ ] **Step 3: Implement them in `fs/mfs/src/write.rs`**

```rust
/// Mark `bit` free. [`bitmap_set`]'s twin — **same byte, same mask**, because a
/// divergence between the two would free a different object than the caller
/// named.
///
/// `None` if the bit lies past the block, which is how a caller that mixed up its
/// bitmap arithmetic finds out rather than by writing into the wrong byte.
///
/// **Order matters at the call site, in both directions.** Allocation sets the
/// bit *before* anything references the object it names (see [`bitmap_set`]'s
/// callers), so a failure between the two leaks. Freeing runs the other way: the
/// reference is removed first, so this is called only once nothing points at the
/// object. Leak over corruption, stated once and applied both ways.
pub fn bitmap_clear(block: &mut [u8], bit: u32) -> Option<()> {
    let byte = block.get_mut((bit / 8) as usize)?;
    *byte &= !(1 << (bit % 8));
    Some(())
}

/// What one directory block has to say about a name.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DirentSlot {
    /// The name is already here, in slot `.0`.
    Occupied(usize),
    /// The name is not here, and slot `.0` is free.
    Free(usize),
    /// The name is not here and no slot is free.
    Full,
}

/// Scan one directory block for `want`, and for the first free slot.
///
/// **One pass**, because the create path needs both answers and this server has
/// exactly one block buffer — a second scan would be a second fetch.
///
/// **`Occupied` wins over `Free`, whatever the indices.** If a free slot
/// short-circuited the scan, a create could insert a duplicate entry *before* the
/// real one — and [`crate::walk::find_in_block`] stops at the first match, so the
/// original would be shadowed silently. That is why the free slot is remembered
/// and the whole block scanned anyway.
///
/// A trailing partial entry is ignored rather than half-decoded ([`crate::dirent`]'s
/// rule), so a short block cannot synthesize a free slot out of whatever followed
/// it.
pub fn dirent_slot(block: &[u8], want: &str) -> DirentSlot {
    let mut free: Option<usize> = None;
    for (i, chunk) in block.as_chunks::<DIRENT_SIZE>().0.iter().enumerate() {
        let Some(e) = DirEntry::from_le_bytes(chunk) else {
            continue;
        };
        if e.ino == 0 {
            if free.is_none() {
                free = Some(i);
            }
            continue;
        }
        // A name that is not valid UTF-8 decodes to "", which cannot equal any
        // component `parse_path` accepted -- so it cannot be matched by accident.
        if e.name_str() == want {
            return DirentSlot::Occupied(i);
        }
    }
    match free {
        Some(i) => DirentSlot::Free(i),
        None => DirentSlot::Full,
    }
}

/// Byte offset at which an appended directory entry goes, given the directory's
/// current size.
///
/// Used when no slot in any existing block is free: the entry lands at the end
/// and the directory grows by one entry — through exactly the allocator a file
/// grows through, which is why growth needs no second code path.
///
/// `EIO` for a size that is negative, past [`crate::walk::MAX_DIR_BYTES`], or not
/// a whole number of entries — all three are a corrupt directory inode, and
/// appending at a misaligned offset would splice an entry across two others.
/// `ENOSPC` when the appended entry would not fit under the cap, which is a
/// *full* directory rather than a corrupt one and is a different thing to tell a
/// caller.
pub fn dir_append_offset(size: i32) -> Result<u64, i32> {
    let size = crate::walk::dir_size(size)?;
    if !size.is_multiple_of(DIRENT_SIZE) {
        return Err(EIO);
    }
    let end = size.checked_add(DIRENT_SIZE).ok_or(EIO)?;
    if end > crate::walk::MAX_DIR_BYTES {
        return Err(ENOSPC);
    }
    Ok(size as u64)
}

/// How many single-indirect slots a file of `size` bytes reaches.
///
/// `0` for a file inside the direct zones. **This is what bounds truncate's slot
/// scan** (C8): a 32 KiB file examines two slots rather than the block's 1024.
/// Capped at [`ptrs_per_block`] anyway, because every device-derived loop in this
/// crate carries a cap and a corrupt size must not walk past the block.
///
/// `EIO` for a negative size — a corrupt inode rather than a caller error,
/// [`grow_size`]'s split.
pub fn indirect_slots_used(size: i32, bs: usize) -> Result<usize, i32> {
    if size < 0 || bs == 0 {
        return Err(EIO);
    }
    let blocks = (size as usize).div_ceil(bs);
    Ok(blocks
        .saturating_sub(NR_DIRECT_ZONES)
        .min(ptrs_per_block(bs)))
}
```

Add to the module's imports: `use crate::dirent::{DIRENT_SIZE, DirEntry};`.

- [ ] **Step 4: Write the failing `split_basename` tests in `fs/mfs/src/walk.rs`**

```rust
    #[test]
    fn a_path_splits_into_its_parent_and_its_final_component() {
        assert_eq!(split_basename("/etc/new"), Ok(("/etc", "new")));
        assert_eq!(split_basename("/full/new"), Ok(("/full", "new")));
    }

    #[test]
    fn a_top_level_name_has_the_root_as_its_parent() {
        // The `cut == 0` case: the parent is "/", not "".
        assert_eq!(split_basename("/new"), Ok(("/", "new")));
    }

    #[test]
    fn the_root_itself_is_eisdir() {
        // It names a directory, and a create whose target is a directory gets the
        // same errno `FS_TRUNC` and `VFS_OPEN` use for one.
        assert_eq!(split_basename("/"), Err(EISDIR));
    }

    #[test]
    fn a_trailing_slash_or_a_dot_component_is_einval() {
        // An empty final component names nothing; `.` and `..` are entries every
        // directory already carries, so creating them would insert a duplicate.
        for p in ["/etc/", "/etc/.", "/etc/..", "/."] {
            assert_eq!(split_basename(p), Err(EINVAL), "{p:?}");
        }
    }

    #[test]
    fn a_relative_path_has_no_separator_and_is_einval() {
        // Unreachable through `parse_path`, which refuses it first -- but a
        // second caller must not be able to reach a `rfind` that returns `None`.
        assert_eq!(split_basename("etc"), Err(EINVAL));
        assert_eq!(split_basename(""), Err(EINVAL));
    }

    #[test]
    fn a_final_component_past_the_name_field_is_enametoolong() {
        // It could not be written into a directory entry. Exactly NAME_MAX is
        // fine, because the name field is NUL-padded rather than terminated.
        let long = "x".repeat(NAME_MAX + 1);
        assert_eq!(split_basename(&format!("/etc/{long}")), Err(ENAMETOOLONG));
        let ok = "x".repeat(NAME_MAX);
        let path = format!("/etc/{ok}");
        assert_eq!(split_basename(&path), Ok(("/etc", ok.as_str())));
    }
```

The `format!` calls need `String`; `walk.rs`'s test module may declare `extern crate std;` locally, the precedent `brand.rs` set. Check whether it already does and add it if not.

- [ ] **Step 5: Implement `split_basename` in `fs/mfs/src/walk.rs`**

```rust
/// Split an absolute path into its parent directory and its final component.
///
/// `/etc/new` splits into `("/etc", "new")`, and `/new` into `("/", "new")` — the
/// parent is `"/"` rather than `""`, so it resolves through the ordinary walk.
///
/// The create path is the only caller, and it applies [`parse_path`] first, so
/// the length rules there are already enforced. What is left is what a *create*
/// needs on top of a lookup:
///
///   * `"/"` is `EISDIR`. It names the root directory, and a create whose target
///     is a directory gets the same errno `FS_TRUNC` and `VFS_OPEN` use for one.
///   * An empty final component (a trailing slash) is `EINVAL` — it names
///     nothing. So are `.` and `..`, which every directory already carries: a
///     create there would insert a duplicate of an entry that exists.
///   * A final component past [`crate::dirent::NAME_MAX`] is `ENAMETOOLONG`; it
///     could not be written into a directory entry at all.
pub fn split_basename(path: &str) -> Result<(&str, &str), i32> {
    if path == "/" {
        return Err(EISDIR);
    }
    let cut = path.rfind('/').ok_or(EINVAL)?;
    // `checked_add`, not `+`: this crate ships with `overflow-checks = false`.
    let name = path.get(cut.checked_add(1).ok_or(EINVAL)?..).ok_or(EINVAL)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(EINVAL);
    }
    if name.len() > NAME_MAX {
        return Err(ENAMETOOLONG);
    }
    let parent = if cut == 0 {
        "/"
    } else {
        path.get(..cut).ok_or(EINVAL)?
    };
    Ok((parent, name))
}
```

Add `EISDIR` to `walk.rs`'s error imports.

- [ ] **Step 6: Add `parse_trunc` to `fs/mfs/src/proto.rs`**

```rust
/// Read the inode number out of an `FS_TRUNC` request.
///
/// One field. It shares [`FS_INO_OFF`] with `FS_LOOKUP`'s reply and `FS_READ`'s
/// request for the reason that constant's docs give: it is one field, and the
/// number `FS_LOOKUP` hands out is the number every later request takes back.
///
/// Its own function rather than `parse_read(msg).ino`, so that a request whose
/// other three fields are meaningless cannot be read as though they were not.
pub fn parse_trunc(msg: &Message) -> i32 {
    rd_i32(msg, FS_INO_OFF)
}
```

and its test:

```rust
    #[test]
    fn parse_trunc_reads_the_inode_and_ignores_everything_else() {
        let mut m = empty();
        wr_i32(&mut m, FS_INO_OFF, 7);
        wr_i32(&mut m, FS_GRANT_OFF, 0x1234);
        wr_i32(&mut m, FS_LEN_OFF, 512);
        assert_eq!(parse_trunc(&m), 7);
        // A zeroed payload reads as inode 0, which the server refuses as EINVAL.
        assert_eq!(parse_trunc(&empty()), 0);
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

```bash
cargo test -p minixrs-mfs
```

Expected: PASS.

- [ ] **Step 8: Lint and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p minixrs-mfs --features server -- -D warnings
git add fs/mfs/src/write.rs fs/mfs/src/walk.rs fs/mfs/src/proto.rs
git commit -s -m "feat(mfs): the create and truncate policy, in the library (slice 5.10b)

bitmap_clear is bitmap_set's twin down to the byte and the mask, because a
divergence between them would free a different object than the caller named.
dirent_slot answers a directory block's two questions in one pass, since this
server has one block buffer -- and Occupied wins over Free whatever the
indices, or a create could insert a duplicate entry ahead of the real one and
the reader, which stops at the first match, would shadow the original
silently.

dir_append_offset splits a full directory (ENOSPC) from a corrupt one (EIO),
and indirect_slots_used is what bounds truncate's slot scan by the file's own
size rather than by the indirect block's 1024 pointers.

All of it lives here because fs/mfs/src/main.rs is behind
required-features = \"server\" and is compiled by no CI job."
```

---

## Task 4: The leak fix — a second staging buffer, and `do_write` restaged

**Files:**
- Modify: `fs/mfs/src/main.rs` (the buffer section near `BlockBuf`/`Blocks`; `main`; `do_write`)

**Interfaces:**
- Consumes: nothing from Tasks 1–3.
- Produces: `struct Stage` with `Stage::addr() -> u64` and `fill(&mut self, granter: Endpoint, gid: i32, len: usize) -> Result<&[u8], i32>`; `do_write` gains a `stage: &mut Stage` parameter.

**What this closes.** Slice 5.10a's `do_write` allocates the zone (step 3) *before* it copies the client's bytes (step 4), and that copy is **client-controlled**: VFS range-checks the caller's buffer but cannot check that it is mapped — the kernel's page-table walk is the gate (D5) — so `write(fd, unmapped_va, 4096)` reaches MFS with a well-formed magic grant and fails at the safecopy, leaving the bitmap bit set and the inode never written back. Looping it exhausts the image's free zones (185 in the musl flavour, so 93–185 calls) and every later write, legitimate ones included, answers `ENOSPC` for the rest of the boot.

**And why the fix is not a rollback.** Clearing the bit on the error path is *wrong* in one of the three cases 5.10a enumerated: an indirect slot whose indirect block already existed does not leak, because the block on disk still names the zone — so freeing the bit there would hand out a zone two files share, the exact corruption the allocation ordering exists to prevent. Staging the bytes *before* the allocation removes the question, and turns a three-case table into one sentence: **no client-controlled failure occurs after an allocation.**

**Why a second static and not a `main`-frame local.** A server stack is exactly one page; a 4 KiB local would put the frame base below the mapping, and VM turns that fault into a SIGSEGV that prints nothing `tests/qemu-boot.forbidden` catches. `.bss` is not the constrained resource here — the stack is. This does not relax `Blocks`'s discipline: `Stage` is a second capability token with a single purpose, and truncate deliberately does not borrow it (C8 removes the need), so the invariant stays one sentence rather than becoming a shared-buffer discipline.

- [ ] **Step 1: Add the buffer and its capability**

In `fs/mfs/src/main.rs`, after the `BLOCK` static and before `struct Blocks`:

```rust
/// `UnsafeCell`-wrapped staging buffer for one `FS_WRITE` round's client bytes.
/// See [`Stage`], and [`BlockBuf`] for why it is a static.
#[repr(transparent)]
struct StageBuf(UnsafeCell<[u8; MFS_BLOCK_SIZE]>);

// SAFETY: as `BlockBuf` — MFS is a single EL0 thread running a straight-line
// receive loop with no interrupt handlers of its own, so there is never a second
// accessor. Every path to the bytes goes through `Stage`, which is created
// exactly once in `main` and whose `&mut self` method is what serializes access
// within that thread.
unsafe impl Sync for StageBuf {}

static STAGE: StageBuf = StageBuf(UnsafeCell::new([0u8; MFS_BLOCK_SIZE]));

/// The capability to stage a client's bytes — one instance, created once in
/// [`main`], and the whole of slice 5.10b's leak fix.
///
/// It exists so that the client-controlled copy happens **before** the zone
/// allocation rather than after it, which turns "a failed write leaks a zone per
/// attempt, and clearing the bit again would be worse" into a one-line invariant:
/// *no client-controlled failure occurs after an allocation.*
///
/// It is **single-purpose** — it holds one round's client bytes and nothing else.
/// Truncate does not borrow it (its indirect scan is bounded by the file's own
/// size instead), so this buffer never has to be reasoned about as shared state,
/// and [`Blocks`]'s borrow discipline is untouched.
///
/// A grant is not needed: MFS is the *grantee* of this copy, so the destination
/// is an ordinary address in its own address space.
struct Stage;

impl Stage {
    /// Address of the staging buffer. Constant for the life of the process.
    ///
    /// Unused today — every copy names it through [`Stage::fill`] — but kept
    /// beside [`Blocks::addr`] because a future request that grants over this
    /// buffer is the obvious next use, and a wrong answer here would be a wild
    /// copy rather than a compile error.
    #[allow(dead_code)]
    fn addr() -> u64 {
        STAGE.0.get() as usize as u64
    }

    /// Copy `len` bytes out of the client's grant into the staging buffer, and
    /// hand back exactly what landed.
    ///
    /// `&mut self` for [`Blocks::read`]'s reason: this hands out the only
    /// reference into the buffer, and the borrow checker is what keeps it the
    /// only one.
    ///
    /// `granter` is the **kernel-stamped `m_source`**. There is no payload field
    /// for it and there must never be one: this server holds `SYS_SAFECOPY`, so a
    /// caller-supplied granter would aim a privileged cross-address-space copy
    /// wherever the caller pointed.
    fn fill(&mut self, granter: Endpoint, gid: i32, len: usize) -> Result<&[u8], i32> {
        if len > MFS_BLOCK_SIZE {
            // Unreachable: `clamp_write` caps a chunk at one block. `EIO` rather
            // than a truncated copy, so a future clamp bug is an errno instead of
            // a short write nobody notices.
            return Err(EIO);
        }
        let rc = sys_safecopy(
            SAFECOPY_FROM,
            granter,
            gid,
            0,
            STAGE.0.get() as usize as u64,
            len as u64,
        );
        if rc != OK {
            // Verbatim: `EPERM` ("your grant does not authorize this") and
            // `EFAULT` ("your buffer is not mapped") are different bugs on the
            // caller's side.
            return Err(rc);
        }
        // SAFETY: `&mut self` is held, so this is the only reference into the
        // buffer for as long as the returned one lives, and the copy above — the
        // only thing that writes these bytes — has completed.
        unsafe { (*STAGE.0.get()).get(..len).ok_or(EIO) }
    }
}
```

- [ ] **Step 2: Create it in `main` and thread it through the dispatch**

Beside `let mut blocks = device(&mut grants, mem_endpoint());`:

```rust
    let mut stage = Stage;
```

and in the dispatch `match`:

```rust
            FS_WRITE => do_write(&msg, caller_e, &mut blocks, &mut stage, &mount),
```

- [ ] **Step 3: Restage `do_write`**

Change the signature to `fn do_write(msg: &Message, granter: Endpoint, blocks: &mut Blocks, stage: &mut Stage, mount: &Option<Mount>) -> i32`.

Replace everything from the `let (zone, mut dirty) = …` line down to the `sys_safecopy` / `blocks.write` pair with:

```rust
    // Step 3 (new in slice 5.10b). The client's bytes are staged **before**
    // anything is allocated, which is what makes the invariant below true: no
    // client-controlled failure occurs after an allocation. See [`Stage`].
    let staged = match stage.fill(granter, req.gid, chunk.len) {
        Ok(bytes) => bytes,
        Err(e) => return e,
    };

    // Step 4. The only allocation, and nothing after it can now fail on the
    // client's account. `dirty` records whether the inode changed at all — see
    // step 6.
    let (zone, mut dirty) = match place_zone(blocks, mount, &mut node, req.pos) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Step 5. The guard names `MFS_BLOCK_SIZE`, not `mount.block_size`, because
    // it is `Blocks::write`'s flush length that has to be fully covered.
    if chunk.len < MFS_BLOCK_SIZE {
        // A partial write preserves the bytes around it, so the block has to be
        // read before it is spliced. A full-block write skips this: every byte is
        // about to be replaced.
        if let Err(e) = blocks.read(u64::from(zone)) {
            return e;
        }
    }
    let dst = blocks.buf_mut();
    let Some(window) = chunk
        .off_in_block
        .checked_add(chunk.len)
        .and_then(|end| dst.get_mut(chunk.off_in_block..end))
    else {
        // Unreachable: `clamp_write` guarantees a chunk lies inside one block. Say
        // `EIO` rather than indexing, so a future clamp bug is an errno instead of
        // a panic in a server nothing can restart.
        return EIO;
    };
    // Equal lengths by construction: the window is `chunk.len` wide and `fill`
    // returned exactly `chunk.len` bytes.
    window.copy_from_slice(staged);
    if let Err(e) = blocks.write(u64::from(zone)) {
        return e;
    }
```

`staged` borrows `stage` while `window` borrows `blocks` — different objects, so both borrows coexist.

Renumber the trailing write-back comment to "Step 6" and leave its text alone: the condition is still **"a zone was assigned *or* the size grew"**, and Task 8's `fs.hole` probe is what finally proves the first half.

- [ ] **Step 4: Rewrite `do_write`'s docstring**

Replace the two long paragraphs beginning "**A failure mid-write leaks a zone…**" and "**Whoever fixes this must not simply clear the bit…**" with:

```rust
/// **No client-controlled failure occurs after an allocation** (slice 5.10b).
/// That is what step 3 buys, and it is the whole of the fix for the leak slice
/// 5.10a shipped and documented: the copy out of the client's grant is the one
/// step here a caller can make fail — `write(fd, unmapped_va, len)` reaches this
/// server with a well-formed magic grant and faults on the kernel's page-table
/// walk — so doing it *before* the zone is allocated means a failed write
/// allocates nothing at all.
///
/// The fix is deliberately **not** a rollback. Clearing the bitmap bit on the
/// error path is wrong in one of the three cases: an indirect slot whose indirect
/// block already existed does not leak, because the block on disk still names the
/// zone, so freeing the bit there would hand out a zone two files share — the
/// corruption the allocation ordering exists to prevent. Staging first removes
/// the question rather than answering it three times.
///
/// `init`'s `fs.leak` boot marker is the proof: 256 failing writes, then one that
/// must succeed. Before this change the failures leaked more zones than the image
/// has free and the final write answered `ENOSPC`.
///
/// The device I/O *after* the allocation can still fail with `EIO` and still
/// leaks a zone. That class is unchanged and unreachable by a client: it needs
/// the ramdisk itself to fail.
```

Also update the numbered step list at the top of the docstring so it names the staging step, and update the module-level "Four things worth knowing" note to mention that there are now two buffers with two capabilities and why.

- [ ] **Step 5: Compile and lint**

```bash
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean. This is the only invocation that compiles `main.rs` at all.

- [ ] **Step 6: Boot and confirm the write path is unchanged**

```bash
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot-t4.log 2>&1
tools/check-boot-log.sh /tmp/boot-t4.log
```

Expected: every marker still PASS, `minix.rs init: fs.write ok n=32768 v=32768` included. **There is no new marker yet** — the fix's own proof arrives with init's `leak_probe` in Task 8. What this step establishes is that restaging did not break the happy path, which is 64 write rounds' worth of evidence.

- [ ] **Step 7: Commit**

```bash
git add fs/mfs/src/main.rs
git commit -s -m "fix(mfs): stage a write's bytes before allocating its zone (slice 5.10b)

Slice 5.10a allocated the zone before copying the client's bytes, and that copy
is the one step a caller can make fail: VFS range-checks a buffer but cannot
check that it is mapped, so write(fd, unmapped_va, len) arrives with a
well-formed magic grant and faults on the kernel's page-table walk, leaving the
bitmap bit set and the inode never written back. Looping it exhausted the
image's free zones and every later write answered ENOSPC for the rest of the
boot.

The fix is not a rollback, which 5.10a documented as wrong in one of the three
cases -- an indirect slot whose indirect block already existed does not leak,
and clearing its bit would hand out a zone two files share. Staging the bytes
into a second .bss buffer first removes the question: no client-controlled
failure now occurs after an allocation, which is one sentence in place of a
table.

The buffer is a static for the block buffer's reason -- a server stack is one
page -- and gets its own capability token. Truncate does not borrow it, so it
stays single-purpose and Blocks's borrow discipline is untouched."
```

---

## Task 5: MFS — `alloc_inode`, `do_create`, and directory growth

**Files:**
- Modify: `fs/mfs/src/main.rs` (`Mount`, `read_super`, the dispatch, and a new handler section)

**Interfaces:**
- Consumes: `callnr::FS_CREATE` (Task 1); `write::{dirent_slot, DirentSlot, dir_append_offset}` and `walk::split_basename` (Task 3).
- Produces: `do_create`, `create`, `find_free_slot`, `insert_entry`, `alloc_inode`; `Mount` gains `ninodes: u32`.

- [ ] **Step 1: Give `Mount` the superblock's inode count**

```rust
struct Mount {
    root: u32,
    block_size: usize,
    blocks: u32,
    /// Inodes the superblock says exist, i.e. the largest legal inode number.
    ///
    /// **Not derivable from `layout`**, whose `inode_blocks` is rounded up to
    /// whole blocks: using the rounded count as the allocator's limit would hand
    /// out inode numbers past the superblock's own `ninodes`, which no reader
    /// would then be able to address.
    ninodes: u32,
    layout: Layout,
}
```

and in `read_super`, add `ninodes: sb.ninodes,` to the returned `Mount`.

- [ ] **Step 2: Add `alloc_inode`, beside `alloc_zone`**

```rust
/// Allocate one inode: find a clear bit in the inode bitmap and set it.
///
/// [`alloc_zone`]'s twin, and the same ordering rule: **the bit is set before
/// anything names the inode**, so a failure part-way leaks an inode rather than
/// handing the same one out twice. A leak is recoverable by a future `fsck`; a
/// shared inode is silent corruption.
///
/// Bit *i* names inode *i* ([`layout::imap_bit`] is the identity map) and bit 0
/// is reserved because inode 0 does not exist — which is what makes `0` a usable
/// "free slot" marker in a directory entry.
///
/// Two bounds, and both are needed. The scan is capped at `layout.imap_blocks`,
/// because every device-derived loop here has a cap. And the *bit* limit is
/// `mount.ninodes + 1`, from the superblock: the bitmap is rounded up to whole
/// blocks, so its tail describes inodes past the real count, exactly as the zone
/// bitmap's does.
///
/// Unlike [`alloc_zone`] this does **not** touch the object it allocates. The
/// caller writes the new inode back before anything names it, which is the create
/// path's half of the ordering rule.
#[cfg_attr(test, allow(dead_code))]
fn alloc_inode(blocks: &mut Blocks, mount: &Mount) -> Result<u32, i32> {
    let bits_per_block = u32::try_from(mount.block_size.checked_mul(8).ok_or(EIO)?)
        .ok()
        .filter(|&b| b != 0)
        .ok_or(EIO)?;
    // Bit `i` names inode `i`, so the limit is one past the last inode number.
    let limit = mount.ninodes.checked_add(1).ok_or(EIO)?;

    for i in 0..mount.layout.imap_blocks {
        let block = mount.layout.imap_start.checked_add(i).ok_or(EIO)?;
        let from = i.checked_mul(bits_per_block).ok_or(EIO)?;
        if from >= limit {
            break;
        }
        let in_block_limit = limit.saturating_sub(from).min(bits_per_block);
        let buf = blocks.read(u64::from(block))?;
        // Bit 0 of the whole bitmap is reserved: there is no inode 0.
        let start = u32::from(i == 0);
        let Some(bit) = write::bitmap_find_free(buf, start, in_block_limit) else {
            continue;
        };
        let buf = blocks.buf_mut();
        write::bitmap_set(buf, bit).ok_or(EIO)?;
        blocks.write(u64::from(block))?;

        let ino = from.checked_add(bit).ok_or(EIO)?;
        if ino == 0 || ino > mount.ninodes {
            // Unreachable given the limits above; `EIO` rather than a wild inode
            // number, so a future bitmap-arithmetic bug is an errno.
            return Err(EIO);
        }
        return Ok(ino);
    }
    Err(ENOSPC)
}
```

- [ ] **Step 3: Add the mode a new file gets**

Beside the other module constants:

```rust
/// Mode a newly created file gets: a regular file, `rw-r--r--`.
///
/// A constant rather than a payload field, because there is no uid, no gid and no
/// permission check anywhere in the tree — a mode on the wire would be a value
/// nothing reads, and a field with one legal value is worse than no field. It
/// becomes a real field the moment a permission model exists, and `open(2)`'s
/// `mode_t` argument is dropped by VFS until then.
const NEW_FILE_MODE: u16 = I_REGULAR | 0o644;
```

Import `I_REGULAR` from `minixrs_mfs::inode` and `DIRENT_SIZE`, `DirEntry` from `minixrs_mfs::dirent`.

- [ ] **Step 4: Add the handler and its three helpers**

```rust
/// Serve one `FS_CREATE`: make a regular file and return it like a lookup.
///
/// The payload and the reply are [`FS_LOOKUP`]'s, field for field, so
/// [`proto::parse_lookup`] and [`proto::reply_lookup`] serve both — which is what
/// lets VFS classify either answer through one function.
#[cfg_attr(test, allow(dead_code))]
fn do_create(msg: &mut Message, blocks: &mut Blocks, mount: &Option<Mount>) -> i32 {
    let Some(mount) = mount else {
        return ENODEV;
    };
    // The path borrows `*msg`; the result is `Copy`, so that borrow is over
    // before the reply is written back into the same message.
    let created = match proto::parse_lookup(msg).and_then(walk::parse_path) {
        Ok(path) => create(blocks, mount, path),
        Err(e) => Err(e),
    };
    match created {
        Ok((ino, node)) => {
            proto::reply_lookup(msg, ino, node.mode, node.size);
            OK
        }
        Err(e) => e,
    }
}

/// Create `path`, and return the new `(inode number, inode)`.
///
/// **The inode is allocated and written back before the directory entry names
/// it.** That is the mirror of the zone rule, for the mirror reason: a failure
/// between the two orphans an inode — a leak, which a future `fsck` reclaims —
/// where the other order would leave a directory entry naming an inode that was
/// never written, so the name would resolve to whatever the inode table happened
/// to hold. Leak over corruption, in both directions.
#[cfg_attr(test, allow(dead_code))]
fn create(blocks: &mut Blocks, mount: &Mount, path: &str) -> Result<(u32, Inode), i32> {
    let (parent_path, name) = walk::split_basename(path)?;
    let (parent_ino, parent) = lookup(blocks, mount, parent_path)?;
    if !parent.is_dir() {
        return Err(ENOTDIR);
    }

    let free = find_free_slot(blocks, mount, &parent, name)?;

    let ino = alloc_inode(blocks, mount)?;
    let node = Inode {
        mode: NEW_FILE_MODE,
        nlinks: 1,
        ..Inode::EMPTY
    };
    // Timestamps stay 0: there is no clock a user-space filesystem can read yet,
    // and inventing a value would be worse than an obviously absent one. The
    // rule `do_write` already states for `mtime`.
    write_inode(blocks, mount, ino, &node)?;

    // `DirEntry::new` rejects an empty name, one past the field, and one holding
    // a NUL or a `/` — the last of which is what keeps a component from being
    // able to *contain* a path. `split_basename` has already refused all four, so
    // this is defence in depth rather than the gate.
    let entry = DirEntry::new(ino, name.as_bytes()).ok_or(EINVAL)?;
    insert_entry(blocks, mount, parent_ino, parent, free, &entry)?;
    Ok((ino, node))
}

/// Scan **every** block of `dir` for `name`, remembering the first free slot.
///
/// Returns that slot as `(the byte offset of its block, its index within the
/// block)`, or `None` when the directory has no free slot at all.
///
/// `EEXIST` as soon as the name is found — and the scan does not stop at the
/// first free slot, which is the point: a name living in a *later* block than the
/// first free slot would otherwise get a duplicate entry inserted ahead of it,
/// and the reader stops at the first match, so the original would be shadowed
/// silently.
///
/// Bounded by [`walk::dir_size`] like [`find_component`], and for the same
/// reason: a corrupt inode claiming `size = i32::MAX` would otherwise spin this
/// server, and through it VFS and init.
#[cfg_attr(test, allow(dead_code))]
fn find_free_slot(
    blocks: &mut Blocks,
    mount: &Mount,
    dir: &Inode,
    name: &str,
) -> Result<Option<(u64, usize)>, i32> {
    let size = walk::dir_size(dir.size)?;
    let mut free: Option<(u64, usize)> = None;
    let mut off = 0usize;
    while let Some(want) = walk::next_dir_chunk(off, size, mount.block_size) {
        // The block's borrow ends at this `match`: `DirentSlot` is `Copy`, so
        // nothing points into the buffer when the next fetch replaces it.
        let blk = fetch(blocks, mount, dir, off as u64)?;
        match write::dirent_slot(blk.get(..want).ok_or(EIO)?, name) {
            write::DirentSlot::Occupied(_) => return Err(EEXIST),
            write::DirentSlot::Free(i) if free.is_none() => free = Some((off as u64, i)),
            _ => {}
        }
        off = off.checked_add(want).ok_or(EIO)?;
    }
    Ok(free)
}

/// Write `entry` into `parent`, in a free slot or appended at the end.
///
/// **The append path allocates through [`place_zone`]** — a directory grows
/// through exactly the allocator a file does, which is why growth needs no second
/// code path and why its bitmap ordering is the one already proved. `/full` in the
/// boot image exists so that one create at boot takes this path.
///
/// The directory block is always read before it is spliced: one 64-byte entry is
/// written into it, so it is never covered whole — the case `do_write`'s
/// full-block skip exists for cannot arise here.
#[cfg_attr(test, allow(dead_code))]
fn insert_entry(
    blocks: &mut Blocks,
    mount: &Mount,
    parent_ino: u32,
    mut parent: Inode,
    free: Option<(u64, usize)>,
    entry: &DirEntry,
) -> Result<(), i32> {
    let (pos, off_in_block) = match free {
        // A free slot: `pos` names its block, and the index gives the offset
        // inside it.
        Some((pos, slot)) => (pos, slot.checked_mul(DIRENT_SIZE).ok_or(EIO)?),
        // An append: the offset is already entry-aligned, so it lands wherever
        // the directory currently ends.
        None => {
            let pos = write::dir_append_offset(parent.size)?;
            (pos, (pos % mount.block_size as u64) as usize)
        }
    };

    let (zone, mut dirty) = place_zone(blocks, mount, &mut parent, pos)?;
    blocks.read(u64::from(zone))?;
    let buf = blocks.buf_mut();
    let end = off_in_block.checked_add(DIRENT_SIZE).ok_or(EIO)?;
    let cell = buf.get_mut(off_in_block..end).ok_or(EIO)?;
    cell.copy_from_slice(&entry.to_le_bytes());
    blocks.write(u64::from(zone))?;

    if free.is_none() {
        let grown = write::grow_size(parent.size, pos, DIRENT_SIZE)?;
        if grown != parent.size {
            parent.size = grown;
            dirty = true;
        }
    }
    // The same condition `do_write` uses, for the same reason: a zone may have
    // been assigned without the size moving.
    if dirty {
        write_inode(blocks, mount, parent_ino, &parent)?;
    }
    Ok(())
}
```

- [ ] **Step 5: Route it**

Add `FS_CREATE => do_create(&mut msg, &mut blocks, &mount),` to the dispatch `match`, and `FS_CREATE` plus `EEXIST` to the imports.

- [ ] **Step 6: Compile and lint**

```bash
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 7: Boot and confirm no regression**

```bash
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot-t5.log 2>&1
tools/check-boot-log.sh /tmp/boot-t5.log
```

Expected: every existing marker still PASS. **Nothing sends `FS_CREATE` yet** — VFS gains that in Task 7 and init proves it in Task 8. What this step establishes is that adding a dispatch arm and a `Mount` field broke nothing.

- [ ] **Step 8: Commit**

```bash
git add fs/mfs/src/main.rs
git commit -s -m "feat(mfs): serve FS_CREATE, with an inode allocator (slice 5.10b)

alloc_inode is alloc_zone's twin down to the ordering rule: the bitmap bit is
set before anything names the inode, so a failure part-way leaks rather than
handing the same one out twice. Its bit limit comes from the superblock's
ninodes rather than from layout.inode_blocks, which is rounded up to whole
blocks -- using the rounded count would hand out inode numbers no reader can
address, which is why Mount now carries ninodes.

create writes the new inode back before the directory entry names it. That is
the mirror of the zone rule for the mirror reason: a failure between the two
orphans an inode, where the other order leaves a name resolving to whatever
the inode table held.

find_free_slot scans every block before using a free slot, because a name in a
later block would otherwise get a duplicate inserted ahead of it and the
reader, which stops at the first match, would shadow the original silently.
Directory growth goes through place_zone, the same allocator a file uses, so
there is no second path to diverge."
```

---

## Task 6: MFS — `do_trunc` and `free_zone`

**Files:**
- Modify: `fs/mfs/src/main.rs`

**Interfaces:**
- Consumes: `callnr::FS_TRUNC` (Task 1); `write::{bitmap_clear, indirect_slots_used}` and `proto::parse_trunc` (Task 3).
- Produces: `do_trunc`, `free_zones_of`, `free_zone`.

**A correction to the spec's §5.5 step 4.** It says bitmap blocks are "visited in order and each is read once". With a single block buffer that is not achievable while the *indirect* block also has to be consulted: reading a bitmap block evicts the indirect block, so each indirect slot costs a re-read. The implementation below does exactly that and says so. The cost is bounded by C8's slot count (two, for a 32 KiB file), which is the whole reason that bound exists.

- [ ] **Step 1: Add `free_zone`**

```rust
/// Clear one zone's bitmap bit — [`alloc_zone`]'s twin.
///
/// The zone is range-checked with the **write-side** predicate first
/// ([`write::write_zone_ok`]), not the reader's looser one: a corrupt pointer
/// below `first_data_zone` names a metadata block, and clearing its bit would
/// mark part of the filesystem's own bookkeeping available for a file to be
/// allocated over.
///
/// The scan bound is the same one [`alloc_zone`] uses, from the other end: the
/// bit's own block index must lie inside `layout.zmap_blocks`.
#[cfg_attr(test, allow(dead_code))]
fn free_zone(blocks: &mut Blocks, mount: &Mount, zone: u32) -> Result<(), i32> {
    if !write::write_zone_ok(zone, mount.layout.first_data_zone, mount.blocks) {
        return Err(EIO);
    }
    let bit = zmap_bit(zone, mount.layout.first_data_zone).ok_or(EIO)?;
    let bits_per_block = u32::try_from(mount.block_size.checked_mul(8).ok_or(EIO)?)
        .ok()
        .filter(|&b| b != 0)
        .ok_or(EIO)?;
    let index = bit / bits_per_block;
    if index >= mount.layout.zmap_blocks {
        return Err(EIO);
    }
    let block = mount.layout.zmap_start.checked_add(index).ok_or(EIO)?;
    blocks.read(u64::from(block))?;
    let buf = blocks.buf_mut();
    write::bitmap_clear(buf, bit % bits_per_block).ok_or(EIO)?;
    blocks.write(u64::from(block))
}
```

Import `zmap_bit` from `minixrs_mfs::layout`.

- [ ] **Step 2: Add `free_zones_of`**

```rust
/// Free the zones a truncated inode used to name.
///
/// Called **after** the zeroed inode has reached the device, so nothing
/// references these zones any more and a failure here can only leak.
///
/// Two bounds worth naming. The indirect block's slots are visited only as far as
/// the file's recorded size reached ([`write::indirect_slots_used`]) — a 32 KiB
/// file examines two, not the block's 1024. Zones past that size are **not**
/// freed: that is a leak, and it is the correct trade against holding a 4 KiB
/// indirect block across the bitmap's own read-modify-write, which a single block
/// buffer cannot do. For the same reason each slot costs a re-read of the
/// indirect block: freeing a zone evicts it.
///
/// The indirect block's own zone is freed **last**, after the zones it names, so
/// a failure part-way leaves those pointers still readable rather than orphaning
/// them.
#[cfg_attr(test, allow(dead_code))]
fn free_zones_of(
    blocks: &mut Blocks,
    mount: &Mount,
    zones: &[u32; NR_TZONES],
    size: i32,
) -> Result<(), i32> {
    for i in 0..NR_DIRECT_ZONES {
        let z = *zones.get(i).ok_or(EIO)?;
        if z != 0 {
            free_zone(blocks, mount, z)?;
        }
    }

    let indirect = *zones.get(SINGLE_INDIRECT_SLOT).ok_or(EIO)?;
    if indirect == 0 {
        return Ok(());
    }
    if !write::write_zone_ok(indirect, mount.layout.first_data_zone, mount.blocks) {
        return Err(EIO);
    }

    for slot in 0..write::indirect_slots_used(size, mount.block_size)? {
        // Re-read each time: `free_zone` below replaces the buffer's contents.
        // `u32` is `Copy`, so nothing points into it when that happens.
        let blk = blocks.read(u64::from(indirect))?;
        let z = zone_from_indirect(blk, slot).ok_or(EIO)?;
        if z != 0 {
            free_zone(blocks, mount, z)?;
        }
    }
    free_zone(blocks, mount, indirect)
}
```

Import `NR_TZONES` from `minixrs_mfs::inode`.

- [ ] **Step 3: Add the handler**

```rust
/// Serve one `FS_TRUNC`: discard a regular file's contents. Returns `OK` or a
/// negative errno.
///
/// **The zeroed inode is written back first, then the bitmap bits are cleared.**
/// That is the inverse of the allocator's ordering, for the same reason read the
/// other way: once the inode names no zones, a failure while freeing can only
/// leak. If the bits went first, a failure before the inode reached the device
/// would leave a live inode pointing at zones the allocator is free to hand out
/// — two files sharing a zone, the exact corruption `alloc_zone`'s ordering
/// exists to prevent.
///
/// **That ordering has no boot probe, and saying so is the point.** Reversing it
/// moves no marker, because it needs a failure *between* the two steps and
/// nothing a client can send induces one. It is a correct invariant guarding a
/// case this slice cannot reach — the slice-5.10a lesson about the `dirty`
/// condition, applied to a new rule rather than repeated by omission.
///
/// `EISDIR` for a directory and `EINVAL` for any other non-regular inode: the
/// same guards, with the same wording, [`do_write`] applies. An inode number that
/// is not addressable at all — zero included — is `EINVAL` from
/// [`read_inode`]'s own split.
#[cfg_attr(test, allow(dead_code))]
fn do_trunc(msg: &Message, blocks: &mut Blocks, mount: &Option<Mount>) -> i32 {
    let Some(mount) = mount else {
        return ENODEV;
    };
    let Ok(ino) = u32::try_from(proto::parse_trunc(msg)) else {
        return EINVAL;
    };

    let mut node = match read_inode(blocks, mount, ino) {
        Ok(node) => node,
        Err(e) => return e,
    };
    if node.is_dir() {
        return EISDIR;
    }
    if !node.is_reg() {
        return EINVAL;
    }

    // Everything the free below needs, captured as `Copy` scalars: nothing is
    // held across a block fetch.
    let zones = node.zone;
    let size = node.size;

    node.zone = [0u32; NR_TZONES];
    node.size = 0;
    if let Err(e) = write_inode(blocks, mount, ino, &node) {
        return e;
    }

    if let Err(e) = free_zones_of(blocks, mount, &zones, size) {
        // The inode is already durable and names nothing, so the file really is
        // empty; what failed is the reclaim. Report it — the caller's `open` must
        // not hand back a descriptor as though nothing went wrong — but the
        // filesystem is consistent, merely short some zones.
        return e;
    }
    OK
}
```

- [ ] **Step 4: Route it**

Add `FS_TRUNC => do_trunc(&msg, &mut blocks, &mount),` to the dispatch, and `FS_TRUNC` to the imports.

- [ ] **Step 5: Compile, lint, boot**

```bash
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot-t6.log 2>&1
tools/check-boot-log.sh /tmp/boot-t6.log
```

Expected: every existing marker still PASS. Nothing sends `FS_TRUNC` yet.

- [ ] **Step 6: Commit**

```bash
git add fs/mfs/src/main.rs
git commit -s -m "feat(mfs): serve FS_TRUNC (slice 5.10b)

The zeroed inode is written back before any bitmap bit is cleared -- the
inverse of the allocator's ordering, for the same reason read the other way.
Once the inode names no zones a failure while freeing can only leak; the other
order would leave a live inode pointing at zones the allocator is free to hand
out, which is two files sharing one.

That ordering has no boot probe and the docstring says so: reversing it needs a
failure between the two steps and nothing a client can send induces one. It is
a correct invariant guarding a case this slice cannot reach, recorded as
unproven rather than left to look covered -- the 5.10a lesson about the dirty
condition, applied to a new rule.

The indirect block's slots are walked only as far as the file's recorded size
reached, so a 32 KiB file examines two rather than 1024, and each costs a
re-read because freeing a zone evicts the block buffer. Zones past the recorded
size are not freed; that leak is the trade against holding an indirect block
across a bitmap read-modify-write, which one buffer cannot do."
```

---

## Task 7: VFS — the flags field, `O_CREAT` / `O_TRUNC` routing, and four denial probes

**Files:**
- Modify: `servers/vfs/src/open.rs`
- Modify: `servers/vfs/src/main.rs` (`do_open`, `fs_lookup`, `fs_denials`, `FS_DENIAL_PROBES`)
- Test: `servers/vfs/src/open.rs`'s `mod tests`

**Interfaces:**
- Consumes: `callnr::{FS_CREATE, FS_TRUNC, VFS_FLAGS_OFF}` and `fcntl::{O_ACCMODE, O_RDWR, O_CREAT, O_TRUNC, O_KNOWN}` (Task 1); MFS serving both requests (Tasks 5, 6); `rootfs::ROOTFS_DENY_PATH` (Task 2).
- Produces: `open::OpenRequest::flags`, `open::OpenFlags { create, truncate }`, `open::validate_flags(flags: i32) -> Result<OpenFlags, i32>`; `fs_path_request`, `fs_create`, `fs_trunc` in `main.rs`; `FS_DENIAL_PROBES = 14`.

- [ ] **Step 1: Write the failing `validate_flags` tests**

Append to `servers/vfs/src/open.rs`'s `mod tests`:

```rust
    #[test]
    fn parse_reads_all_three_fields_from_their_own_offsets() {
        // Three distinct values, so any swapped pair of offsets would fail.
        let mut m = request(BUF, 9);
        wr_i32(&mut m, VFS_FLAGS_OFF, O_CREAT);
        assert_eq!(
            parse(&m),
            OpenRequest {
                path: BUF,
                len: 9,
                flags: O_CREAT
            }
        );
    }

    #[test]
    fn a_bare_access_mode_honours_nothing_and_is_accepted() {
        // Accepted and ignored: there is no permission check anywhere in the
        // tree, so honouring the access mode would be a check with nothing
        // behind it -- `open(path, O_RDONLY)` then `write` is what this build
        // does, and pretending otherwise would be theatre.
        for mode in [O_RDONLY, O_WRONLY, O_RDWR] {
            assert_eq!(
                validate_flags(mode),
                Ok(OpenFlags {
                    create: false,
                    truncate: false
                }),
                "mode {mode}"
            );
        }
    }

    #[test]
    fn each_honoured_bit_is_reported_on_its_own_and_together() {
        assert_eq!(
            validate_flags(O_RDWR | O_CREAT),
            Ok(OpenFlags { create: true, truncate: false })
        );
        assert_eq!(
            validate_flags(O_RDWR | O_TRUNC),
            Ok(OpenFlags { create: false, truncate: true })
        );
        assert_eq!(
            validate_flags(O_RDWR | O_CREAT | O_TRUNC),
            Ok(OpenFlags { create: true, truncate: true })
        );
    }

    #[test]
    fn any_unimplemented_bit_is_einval() {
        // The case that matters is `O_APPEND`: ignoring it silently would have a
        // client's appends overwrite from position 0 and report success. So an
        // unimplemented flag fails loudly, and this function is where each one
        // lands as it becomes real.
        for flag in [O_UNKNOWN_BIT, 0o2000, 0o4000, i32::MIN] {
            assert_eq!(validate_flags(flag), Err(EINVAL), "flag {flag:o}");
            assert_eq!(validate_flags(O_RDWR | flag), Err(EINVAL), "flag {flag:o}");
        }
    }

    #[test]
    fn the_reserved_access_mode_value_is_einval() {
        // 3 is not an access mode. It is inside `O_ACCMODE` and therefore inside
        // `O_KNOWN`, so the mask cannot catch it -- without its own check it
        // would read as `O_RDONLY | something`.
        assert_eq!(validate_flags(O_ACCMODE), Err(EINVAL));
        assert_eq!(validate_flags(O_ACCMODE | O_CREAT), Err(EINVAL));
    }
```

Add `use minixrs_kernel_shared::callnr::VFS_FLAGS_OFF;` and the `fcntl` imports to the test module (or the file, as the compiler asks).

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p minixrs-vfs
```

Expected: FAIL — `OpenRequest` has no `flags` field and `validate_flags` does not exist.

- [ ] **Step 3: Implement them in `servers/vfs/src/open.rs`**

```rust
pub struct OpenRequest {
    /// The path buffer's address **in the caller's own address space**.
    pub path: u64,
    /// Its length in bytes, not counting any terminator the caller may have.
    pub len: i32,
    /// `open(2)` flags — [`minixrs_kernel_shared::fcntl`]'s values, which are
    /// musl's. Checked by [`validate_flags`].
    pub flags: i32,
}

pub fn parse(msg: &Message) -> OpenRequest {
    OpenRequest {
        path: rd_u64(msg, VFS_PATH_OFF),
        len: rd_i32(msg, VFS_PATH_LEN_OFF),
        flags: rd_i32(msg, VFS_FLAGS_OFF),
    }
}

/// What the honoured open flags amount to.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct OpenFlags {
    /// Create the file if the lookup says it is not there.
    pub create: bool,
    /// Discard the contents of a file that is there.
    pub truncate: bool,
}

/// Check `flags` and reduce them to what this build acts on.
///
/// The **access mode is accepted and ignored**. There is no uid, no gid and no
/// permission check anywhere in the tree, so honouring it would be a check with
/// nothing behind it: `open(path, O_RDONLY)` followed by a successful `write` is
/// what this build does, and refusing the write would be inventing an
/// authorization model in the one place it could be seen.
///
/// **Every other bit is `EINVAL`.** `O_APPEND` is the case that makes this the
/// right default: silently ignoring it would have a client's appends overwrite
/// from position 0 and report success. An unimplemented flag has to fail loudly,
/// and this function is where each one lands as it becomes real.
///
/// The reserved access-mode value `3` needs its own check, because it is *inside*
/// [`O_ACCMODE`] and therefore inside [`O_KNOWN`]: the mask cannot catch it, and
/// without this it would read as `O_RDONLY` with a stray bit.
pub fn validate_flags(flags: i32) -> Result<OpenFlags, i32> {
    if flags & !O_KNOWN != 0 {
        return Err(EINVAL);
    }
    if flags & O_ACCMODE > O_RDWR {
        return Err(EINVAL);
    }
    Ok(OpenFlags {
        create: flags & O_CREAT != 0,
        truncate: flags & O_TRUNC != 0,
    })
}
```

- [ ] **Step 4: Route `do_open`**

Replace `servers/vfs/src/main.rs`'s `do_open` body from `let req = open::parse(msg);` onward:

```rust
    let req = open::parse(msg);
    let len = match open::validate(req.len, req.path) {
        Ok(len) => len,
        Err(e) => return e,
    };
    let flags = match open::validate_flags(req.flags) {
        Ok(flags) => flags,
        Err(e) => return e,
    };
    if let Err(e) = ensure_mounted(mount, mfs) {
        return e;
    }

    let mut path = [0u8; FS_PATH_MAX];
    let rc = sys_copy(
        caller_e,
        req.path,
        SELF,
        buf_addr(&mut path[..len]),
        len as u64,
    );
    if rc != OK {
        // Verbatim: `EFAULT` (the caller's buffer is not mapped) is the client's
        // bug, and flattening it would hide which of its two pointers was wrong.
        return rc;
    }

    // `O_CREAT` is reached only when the lookup says `ENOENT`, so a create that
    // races nothing still hears `EEXIST` from MFS if the file appeared in
    // between — the strict answer, which is also what `O_EXCL` will need.
    let (ino, mode, created) = match fs_lookup(mfs, &path[..len]) {
        Ok((ino, mode, _size)) => (ino, mode, false),
        Err(e) if e == ENOENT && flags.create => match fs_create(mfs, &path[..len]) {
            Ok((ino, mode, _size)) => (ino, mode, true),
            Err(e) => return e,
        },
        Err(e) => return e,
    };

    // `classify` runs **before** `FS_TRUNC` is sent, which is what keeps
    // `O_TRUNC` on a directory from ever reaching MFS — where MFS's own `EISDIR`
    // guard would be the only thing between a probe and a freed directory.
    let entry = match open::classify(mode) {
        Ok(entry) => entry,
        Err(e) => return e,
    };

    // `O_CREAT | O_TRUNC` on a missing file takes the create arm and stops there:
    // a freshly created file is already empty, so truncating it would be a second
    // round trip to reach the state it is in. And the truncate happens **before**
    // the descriptor exists, so a failure leaves no descriptor onto a
    // half-truncated file.
    if flags.truncate && !created {
        let rc = fs_trunc(mfs, ino as i32);
        if rc != OK {
            return rc;
        }
    }

    match entry {
        // `classify` decides the *kind* of descriptor; the inode is filled in
        // here, because this is the layer that knows it.
        Fd::File { .. } => {
            fd::alloc(endpoint_proc(caller_e).get(), Fd::File { ino, pos: 0 }).unwrap_or_else(|e| e)
        }
        // No other variant is reachable — `classify` returns only `File` or an
        // error — but routing it explicitly means a future device-node arm is a
        // compile error to handle rather than a silent `EINVAL`.
        _ => EINVAL,
    }
```

**`Fd::File` gains no flags.** The access mode is ignored, `O_CREAT` and `O_TRUNC` are consumed at open time by definition, and every other bit is refused — so there is nothing left for a descriptor to remember. Update `do_open`'s docstring to say so.

- [ ] **Step 5: Add the two wire helpers, sharing `fs_lookup`'s marshaller**

Replace `fs_lookup`'s body with a delegation and add its two siblings:

```rust
/// Issue one path-shaped FS request — `FS_LOOKUP` or `FS_CREATE` — and return
/// `(inode, mode, size)`.
///
/// **One marshaller for both**, because the payloads and the replies are
/// identical field for field (slice 5.10b, C2). Sharing it is the point rather
/// than a saving: a create's answer is classified through exactly the same
/// `open::classify` a lookup's is, so the two cannot drift apart.
#[cfg_attr(test, allow(dead_code))]
fn fs_path_request(mfs: Endpoint, m_type: i32, path: &[u8]) -> Result<(u32, i32, i32), i32> {
    if path.is_empty() || path.len() >= FS_PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let mut m = Message {
        m_source: 0,
        m_type,
        payload: [0u8; 96],
    };
    // The payload starts zeroed, so writing the bytes *is* NUL-padding it.
    m.payload[FS_PATH_OFF..FS_PATH_OFF + path.len()].copy_from_slice(path);
    let trap_rc = ipc_sendrec(mfs, &mut m);
    if trap_rc != OK {
        return Err(trap_rc);
    }
    if m.m_type != OK {
        return Err(m.m_type);
    }
    let ino = rd_i32(&m, FS_INO_OFF);
    let Ok(ino) = u32::try_from(ino) else {
        // MFS answered `OK` with a nonsense inode. Nothing can be done with it,
        // and `EIO` would claim the device failed when the *server* did.
        return Err(EINVAL);
    };
    Ok((ino, rd_i32(&m, FS_MODE_OFF), rd_i32(&m, FS_SIZE_OFF)))
}

/// Resolve a path to `(inode, mode, size)`.
#[cfg_attr(test, allow(dead_code))]
fn fs_lookup(mfs: Endpoint, path: &[u8]) -> Result<(u32, i32, i32), i32> {
    fs_path_request(mfs, FS_LOOKUP, path)
}

/// Create a regular file and return it exactly as a lookup would.
#[cfg_attr(test, allow(dead_code))]
fn fs_create(mfs: Endpoint, path: &[u8]) -> Result<(u32, i32, i32), i32> {
    fs_path_request(mfs, FS_CREATE, path)
}

/// Issue one `FS_TRUNC` and return the reply `m_type` — `OK`, or a negative
/// errno.
///
/// No grant and no length: `O_TRUNC` is the only client and it always truncates
/// to zero, so there is nothing else to carry.
#[cfg_attr(test, allow(dead_code))]
fn fs_trunc(mfs: Endpoint, ino: i32) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: FS_TRUNC,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, FS_INO_OFF, ino);
    let trap_rc = ipc_sendrec(mfs, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}
```

Every existing `fs.*` marker staying green is what proves the `fs_lookup` refactor.

- [ ] **Step 6: Extend `fs_denials` by four**

These are direct FS requests, so this is the **only** place `EEXIST` and MFS's own `EISDIR` can be probed: VFS's `open` answers `EISDIR` from `classify` before either new request is ever sent. Insert before the `if denied == FS_DENIAL_PROBES` line:

```rust
    // Slice 5.10b: `FS_CREATE` on an existing name is `EEXIST` — **and the target
    // is unchanged afterwards**, which is the half that matters. A dropped
    // `EEXIST` would insert a second entry shadowing the first, silently, with
    // every other marker still green; re-resolving the name and comparing the
    // inode number is what makes the refusal mean "nothing changed" rather than
    // merely "an error came back". `/etc/deny` exists for this and is read by
    // nothing else.
    match (
        fs_lookup(mfs, ROOTFS_DENY_PATH.as_bytes()),
        fs_create(mfs, ROOTFS_DENY_PATH.as_bytes()),
        fs_lookup(mfs, ROOTFS_DENY_PATH.as_bytes()),
    ) {
        (Ok((before, _, _)), Err(EEXIST), Ok((after, _, _))) if before == after => denied += 1,
        _ => diag_fmt(format_args!("fs.deny FAIL create-exists")),
    }

    // A parent that is a file, not a directory. Delete MFS's `is_dir` gate and
    // this would splice a directory entry into `/etc/motd`'s data block.
    match fs_create(mfs, b"/etc/motd/x") {
        Err(rc) if rc == ENOTDIR => denied += 1,
        other => diag_fmt(format_args!(
            "fs.deny FAIL create-not-dir rc={}",
            match other {
                Ok(_) => OK,
                Err(rc) => rc,
            }
        )),
    }

    // `FS_TRUNC` on a directory, aimed at `/etc`. An accidental success frees
    // that directory's zones and every later `/etc` marker dies — destructive,
    // but *loud*, which is the property this convention asks for. And `EINVAL`
    // for inode 0, which does not exist.
    let etc = fs_lookup(mfs, b"/etc").map(|(ino, _, _)| ino as i32);
    let Ok(etc) = etc else {
        return diag_fmt(format_args!("fs.deny FAIL setup etc"));
    };
    for (name, ino, want) in [("trunc-dir", etc, EISDIR), ("trunc-ino0", 0, EINVAL)] {
        let rc = fs_trunc(mfs, ino);
        if rc == want {
            denied += 1;
        } else {
            diag_fmt(format_args!("fs.deny FAIL {name} rc={rc}"));
        }
    }
```

Change `const FS_DENIAL_PROBES: usize = 10;` to `= 14;` and extend its docstring with the four bullets. Add `EEXIST` and `ROOTFS_DENY_PATH` to the imports.

- [ ] **Step 7: Test, lint, and boot**

```bash
cargo test -p minixrs-vfs
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot-t7.log 2>&1
grep -a 'fs\.deny' /tmp/boot-t7.log
```

Expected: `[diag vfs] fs.deny ok n=14`. `tools/check-boot-log.sh` will still report `fs.deny ok n=10` MISSING — that marker file is updated in Task 9. Every *other* marker must PASS.

- [ ] **Step 8: Commit**

```bash
git add servers/vfs/src/open.rs servers/vfs/src/main.rs
git commit -s -m "feat(vfs): honour O_CREAT and O_TRUNC in VFS_OPEN (slice 5.10b)

The routing is lookup-first: a hit optionally truncates and hands back a
descriptor, a miss with O_CREAT creates and hands back a descriptor, and
anything else is the errno unchanged. O_CREAT | O_TRUNC on a missing file takes
the create arm and stops there, because a fresh file is already empty. classify
runs before FS_TRUNC is sent, so O_TRUNC on a directory never reaches MFS,
where MFS's own EISDIR guard would be the only thing between a probe and a
freed directory. And the truncate happens before the descriptor exists, so a
failure leaves no descriptor onto a half-truncated file.

The access mode is accepted and ignored -- there is no permission check
anywhere in the tree, so honouring it would be a check with nothing behind it
-- while every unimplemented bit is EINVAL, because silently ignoring O_APPEND
would have a client's appends overwrite from position 0 and report success.
Fd::File gains no flags: there is nothing left for a descriptor to remember.

FS_CREATE shares FS_LOOKUP's marshaller, since the payload and reply are
identical field for field, and the four new denial probes are the only place
EEXIST and MFS's own EISDIR can be reached. The EEXIST probe re-resolves its
target and compares inode numbers, because a dropped EEXIST would insert a
duplicate entry shadowing the first with every other marker still green."
```

---

## Task 8: init — five new probes, and `open.deny` 7 → 11

**Files:**
- Modify: `userland/init/src/main.rs`

**Interfaces:**
- Consumes: everything above. `fcntl::{O_RDWR, O_CREAT, O_TRUNC, O_UNKNOWN_BIT}`, `callnr::VFS_FLAGS_OFF`, and the `rootfs::ROOTFS_*` constants from Task 2.
- Produces: the five boot markers Task 9 pins.

init reports **through the path under test** — it has no `SYS_DIAGCTL`, being user-grade — so `ok` lines go to fd 1 and every `FAIL` to fd 2, both through VFS. Every count in a marker is a **literal**, because init has no way to format an integer; a `const _` beside each is what makes the constant moving underneath it loud at compile time.

- [ ] **Step 1: Give `open_request` a flags argument**

```rust
/// `open(path, flags)` through VFS. Returns the new descriptor, or a negative
/// errno.
#[cfg_attr(test, allow(dead_code))]
fn vfs_open_flags(vfs: Endpoint, path: &str, flags: i32) -> i32 {
    open_request(vfs, path.as_ptr() as usize as u64, path.len() as i32, flags)
}

/// `open(path, O_RDWR)` through VFS — every existing caller's intent.
///
/// `O_RDWR` rather than 0: the access mode is accepted and ignored by VFS, so
/// either works, and naming the one that matches what init then does with the
/// descriptor keeps a non-zero access mode on the live path.
#[cfg_attr(test, allow(dead_code))]
fn vfs_open(vfs: Endpoint, path: &str) -> i32 {
    vfs_open_flags(vfs, path, O_RDWR)
}
```

and in `open_request`, add the parameter and the field:

```rust
fn open_request(vfs: Endpoint, path: u64, len: i32, flags: i32) -> i32 {
    ...
    m.payload[VFS_FLAGS_OFF..VFS_FLAGS_OFF + 4].copy_from_slice(&flags.to_ne_bytes());
    ...
}
```

Update the three existing direct `open_request` call sites in `open_denials` to pass `O_RDWR`.

- [ ] **Step 2: Generalize `report_at` so a second marker can use it**

```rust
/// Report a failing step by marker, name and byte offset, on fd 2.
///
/// The offset is what makes the line worth more than its absence: `read` at 28672
/// is the indirect arm, `verify` at a multiple of 4096 is a whole lost block, and
/// a mismatch anywhere else is a splice bug. Hand-assembled like every other line
/// here — init has no formatting runtime.
#[cfg_attr(test, allow(dead_code))]
fn report_at(vfs: Endpoint, marker: &[u8], what: &[u8], at: usize) {
    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, b"minix.rs init: ");
    n = append(&mut line, n, marker);
    n = append(&mut line, n, b" FAIL ");
    n = append(&mut line, n, what);
    n = append(&mut line, n, b" off=");
    n = append_dec(&mut line, n, at as u64);
    n = append(&mut line, n, b"\n");
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}
```

Update `write_demo`'s three call sites to pass `b"fs.write"` as the new second argument.

- [ ] **Step 3: Add a shared "write it, re-open it, compare it" helper**

```rust
/// Create `path`, write `text` to it, close, re-open **without** `O_CREAT`, and
/// compare every byte back.
///
/// The re-open is the load-bearing half: it forces a fresh `FS_LOOKUP`, so the
/// file has to be findable by an ordinary path walk — which is what proves the
/// *directory entry* reached the device, rather than only the inode having been
/// allocated. Nothing on this path caches a size or an inode, which is exactly
/// what makes that test meaningful.
///
/// Returns `true` when every byte matched and the file is exactly `text.len()`
/// long. The trailing read is what turns `size >= len` into `size == len`.
///
/// `buf` is the caller's, because init's stack is one page and each caller
/// already has a `.rodata` constant to size it from.
#[cfg_attr(test, allow(dead_code))]
fn create_write_verify(vfs: Endpoint, path: &str, text: &[u8], buf: &mut [u8]) -> Result<(), &'static [u8]> {
    let fd = vfs_open_flags(vfs, path, O_RDWR | O_CREAT);
    if fd < 0 {
        return Err(b"open");
    }
    // VFS absorbs short writes (slice 5.4), so anything but the full count is a
    // failure here rather than something to retry.
    let n = vfs_write(vfs, fd, text);
    if n != text.len() as i32 {
        let _ = vfs_close(vfs, fd);
        return Err(b"write");
    }
    if vfs_close(vfs, fd) != OK {
        return Err(b"close");
    }

    let fd = vfs_open(vfs, path);
    if fd < 0 {
        return Err(b"reopen");
    }
    let want = match buf.get_mut(..text.len()) {
        Some(w) => w,
        None => {
            let _ = vfs_close(vfs, fd);
            return Err(b"buf");
        }
    };
    let n = vfs_read(vfs, fd, want);
    if n != text.len() as i32 {
        let _ = vfs_close(vfs, fd);
        return Err(b"read");
    }
    if want != text {
        let _ = vfs_close(vfs, fd);
        return Err(b"verify");
    }
    // One more read, which must be a clean EOF: the file is exactly this long,
    // not merely at least this long.
    let mut tail = [0u8; 1];
    let eof = vfs_read(vfs, fd, &mut tail);
    let _ = vfs_close(vfs, fd);
    if eof != 0 {
        return Err(b"eof");
    }
    Ok(())
}

/// Report a failing step of a create-shaped probe by marker and step name, fd 2.
#[cfg_attr(test, allow(dead_code))]
fn report_step(vfs: Endpoint, marker: &[u8], what: &[u8]) {
    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, b"minix.rs init: ");
    n = append(&mut line, n, marker);
    n = append(&mut line, n, b" FAIL ");
    n = append(&mut line, n, what);
    n = append(&mut line, n, b"\n");
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}
```

Note `text.len() <= FS_MAX_IO` for both callers, which `rootfs.rs`'s `const _`s pin, so the single `vfs_read` cannot be short for any reason but a defect.

- [ ] **Step 4: Add `create_demo` and `dirgrow_demo`**

```rust
/// Longest text either create-shaped probe compares, and the size of their
/// read-back buffer. `.rodata`-sized, not a guess.
const CREATE_BUF_LEN: usize = 64;

const _: () = assert!(CREATE_BUF_LEN >= ROOTFS_CREATE_TEXT.len());
const _: () = assert!(CREATE_BUF_LEN >= ROOTFS_DIRGROW_TEXT.len());
// The markers' `n=` are literals -- init cannot format an integer -- so these are
// what make the constants moving underneath them loud at compile time rather than
// a line that quietly means something else.
const _: () = assert!(ROOTFS_CREATE_TEXT.len() == 25);
const _: () = assert!(ROOTFS_DIRGROW_TEXT.len() == 25);

/// Create a file that is **not** in the image, write to it, and read it back.
///
/// `/etc/new` does not exist until this runs, so every part of it — the inode
/// bitmap bit, the inode itself, and the directory entry naming it — was produced
/// at run time. The re-open is without `O_CREAT`, so the file has to be findable
/// by an ordinary lookup.
///
/// Best-effort: init's job is to keep the system running, so a failure here is
/// reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn create_demo(vfs: Endpoint) {
    let mut buf = [0u8; CREATE_BUF_LEN];
    match create_write_verify(vfs, ROOTFS_CREATE_PATH, ROOTFS_CREATE_TEXT, &mut buf) {
        Ok(()) => {
            let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.create ok n=25\n");
        }
        Err(what) => report_step(vfs, b"fs.create", what),
    }
}

/// Create a file in a directory whose single block is **exactly full**.
///
/// `/full` ships 62 empty files, so with `.` and `..` its block holds 64 used
/// slots and this create *must* allocate a second directory zone. That arm is
/// otherwise unreachable in both boot configurations — `/` holds 4 entries and
/// `/etc` 6, against 64 slots — and an arm no QEMU boot executes is what the
/// `/etc/pattern` mandate and the device-teardown selftest exist to prevent.
///
/// The proof that growth *worked* is the read-back: the entry lands in the new
/// block, so a lookup finding it at all means the parent's new zone pointer and
/// its new size both reached the inode.
#[cfg_attr(test, allow(dead_code))]
fn dirgrow_demo(vfs: Endpoint) {
    let mut buf = [0u8; CREATE_BUF_LEN];
    match create_write_verify(vfs, ROOTFS_FULL_NEW_PATH, ROOTFS_DIRGROW_TEXT, &mut buf) {
        Ok(()) => {
            let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.dirgrow ok n=25\n");
        }
        Err(what) => report_step(vfs, b"fs.dirgrow", what),
    }
}
```

- [ ] **Step 5: Add `hole_demo`**

```rust
/// Bytes read back per verification read in [`hole_demo`]. A divisor of the
/// block, so no read straddles one and MFS's clamp never splits one.
const HOLE_CHUNK: usize = 512;

const _: () = assert!(HOLE_CHUNK > 0);
const _: () = assert!(BDEV_BLOCK_SIZE.is_multiple_of(HOLE_CHUNK));

/// Fill the hole at the front of a sparse file, and verify **the whole file**.
///
/// This is the only probe that reaches the second half of MFS's inode write-back
/// condition — "a zone was assigned **or** the size grew". `/etc/holey` ships two
/// blocks with the first a hole, so writing at position 0 assigns `zone[0]` while
/// `size` stays 8192: keying the write-back on the size alone would drop that
/// pointer while leaving its bitmap bit set, which is the bitmap and the inode
/// disagreeing about a live zone — corruption rather than a leak.
///
/// Slice 5.10a left that half unproven and predicted `FS_TRUNC` would reach it.
/// It does not: with no `lseek` every write runs forward from a descriptor's
/// position, so a write that assigns a zone always extends the file. The case
/// needs a hole *below* EOF, and only the image can supply one.
///
/// **Both windows are load-bearing and both are covered by verifying every
/// byte.** The text at 0 proves the assigned zone's pointer reached the inode
/// (drop `dirty` from the condition and the read returns zeroes); the pattern
/// from 4096 proves the write did not disturb the zone that was already there;
/// and the zeroes between them prove the rest of the hole still reads as a hole.
/// Verifying the whole file costs nothing over sampling it — there is no `lseek`,
/// so reaching any offset already read every byte before it.
///
/// Best-effort: a failure here is reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn hole_demo(vfs: Endpoint) {
    let fd = vfs_open(vfs, ROOTFS_HOLEY_PATH);
    if fd < 0 {
        return report_step(vfs, b"fs.hole", b"open");
    }
    let n = vfs_write(vfs, fd, ROOTFS_HOLEY_TEXT);
    if n != ROOTFS_HOLEY_TEXT.len() as i32 {
        let _ = vfs_close(vfs, fd);
        return report_step(vfs, b"fs.hole", b"write");
    }
    if vfs_close(vfs, fd) != OK {
        return report_step(vfs, b"fs.hole", b"close");
    }

    // Re-open: a fresh lookup, so the inode has to carry the new zone pointer for
    // these reads to return anything but zeroes.
    let fd = vfs_open(vfs, ROOTFS_HOLEY_PATH);
    if fd < 0 {
        return report_step(vfs, b"fs.hole", b"reopen");
    }

    let mut buf = [0u8; HOLE_CHUNK];
    let mut pos = 0usize;
    while pos < ROOTFS_HOLEY_LEN {
        let want = HOLE_CHUNK.min(ROOTFS_HOLEY_LEN - pos);
        let n = vfs_read(vfs, fd, &mut buf[..want]);
        if n <= 0 || n as usize > want {
            let _ = vfs_close(vfs, fd);
            return report_at(vfs, b"fs.hole", b"read", pos);
        }
        let got = n as usize;

        let mut i = 0usize;
        while i < got {
            let Some(at) = pos.checked_add(i) else {
                let _ = vfs_close(vfs, fd);
                return report_at(vfs, b"fs.hole", b"read", pos);
            };
            if buf[i] != holey_expected(at) {
                let _ = vfs_close(vfs, fd);
                return report_at(vfs, b"fs.hole", b"verify", at);
            }
            i += 1;
        }

        let Some(next) = pos.checked_add(got) else {
            let _ = vfs_close(vfs, fd);
            return report_at(vfs, b"fs.hole", b"read", pos);
        };
        pos = next;
    }

    // The file is exactly this long, not merely at least this long: a filled hole
    // must not have moved the size.
    let eof = vfs_read(vfs, fd, &mut buf[..1]);
    let _ = vfs_close(vfs, fd);
    if eof != 0 {
        return report_step(vfs, b"fs.hole", b"eof");
    }
    let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.hole ok\n");
}

/// Byte `i` of `/etc/holey` **after** the hole has been filled: init's text where
/// it wrote, and the shipped contents everywhere else.
#[cfg_attr(test, allow(dead_code))]
fn holey_expected(i: usize) -> u8 {
    match ROOTFS_HOLEY_TEXT.get(i) {
        Some(&b) => b,
        None => rootfs_holey_byte(i),
    }
}
```

- [ ] **Step 6: Add `trunc_demo`**

```rust
/// Truncate the file [`write_demo`] just filled, and prove it is empty.
///
/// **It must run after [`write_demo`]** — it truncates what that probe wrote, and
/// running it first would truncate an already-empty file and prove nothing. The
/// re-open forces a fresh lookup, so a size that never reached the inode shows up
/// here as a non-empty read rather than being papered over by a descriptor that
/// remembered something.
///
/// The `n=0` in the marker is the byte count the first read returned — a clean
/// EOF at position 0, which is what an empty file is.
///
/// Best-effort: a failure here is reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn trunc_demo(vfs: Endpoint) {
    let fd = vfs_open_flags(vfs, ROOTFS_SCRATCH_PATH, O_RDWR | O_TRUNC);
    if fd < 0 {
        return report_step(vfs, b"fs.trunc", b"open");
    }
    if vfs_close(vfs, fd) != OK {
        return report_step(vfs, b"fs.trunc", b"close");
    }

    let fd = vfs_open(vfs, ROOTFS_SCRATCH_PATH);
    if fd < 0 {
        return report_step(vfs, b"fs.trunc", b"reopen");
    }
    let mut buf = [0u8; 8];
    let n = vfs_read(vfs, fd, &mut buf);
    let _ = vfs_close(vfs, fd);
    if n != 0 {
        return report_at(vfs, b"fs.trunc", b"read", n.max(0) as usize);
    }
    let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.trunc ok n=0\n");
}
```

- [ ] **Step 7: Add `leak_probe`**

```rust
// The marker's `n=` is a literal, so this is what pins it to the constant.
const _: () = assert!(ROOTFS_LEAK_PROBES == 256);

/// Prove that a **failing** write allocates nothing — slice 5.10b's leak fix.
///
/// Creates `/etc/leak`, then issues [`ROOTFS_LEAK_PROBES`] writes whose buffer is
/// [`UNMAPPED_VA`]. Every one must answer `EFAULT`: VFS's own range check passes
/// (the address is inside the user range), VFS issues a magic grant naming it,
/// and the **kernel's page-table walk** is what refuses the copy — which is
/// precisely the client-controlled failure that used to leak a zone per attempt.
/// The descriptor's position does not advance on a failure, so every attempt aims
/// at the same hole at offset 0 and would allocate again.
///
/// Then one real write, which must succeed. Before the fix, 256 failures leaked
/// more zones than the image has free and this final write answered `ENOSPC`.
///
/// **The count is [`ROOTFS_IMAGE_BLOCKS`]**, which exceeds any possible free-zone
/// count in the image, so the probe is config-independent *by construction*
/// rather than by measurement: no number here differs between the musl, SDK and
/// sysroot-absent `hello` flavours. That is the slice-5.5/5.6 trap, avoided
/// rather than measured around.
///
/// Best-effort: a failure here is reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn leak_probe(vfs: Endpoint) {
    let fd = vfs_open_flags(vfs, ROOTFS_LEAK_PATH, O_RDWR | O_CREAT);
    if fd < 0 {
        return report_step(vfs, b"fs.leak", b"open");
    }

    let mut i = 0usize;
    while i < ROOTFS_LEAK_PROBES {
        // Built directly rather than through `vfs_write`, which takes a slice:
        // the whole point is a buffer address init cannot form a reference to.
        let rc = vfs_request(vfs, VFS_WRITE, fd, UNMAPPED_VA, LEAK_WRITE_LEN as i32);
        if rc != EFAULT {
            let _ = vfs_close(vfs, fd);
            return report_at(vfs, b"fs.leak", b"probe", i);
        }
        i += 1;
    }

    let n = vfs_write(vfs, fd, ROOTFS_LEAK_TEXT);
    let _ = vfs_close(vfs, fd);
    if n != ROOTFS_LEAK_TEXT.len() as i32 {
        // `ENOSPC` here is the un-fixed failure mode, and it is worth naming
        // separately from a short write.
        return report_step(
            vfs,
            b"fs.leak",
            if n == ENOSPC { b"enospc" } else { b"write" },
        );
    }
    let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.leak ok n=256\n");
}

/// Bytes each failing write asks for. Small: the copy never happens, so this only
/// has to be non-zero — a zero-length write is a legal no-op that VFS answers
/// before the grant is touched, which would probe nothing.
const LEAK_WRITE_LEN: usize = 64;

const _: () = assert!(LEAK_WRITE_LEN > 0);
```

Check `vfs_request`'s existing signature and match it; it is what `vfs_read`/`vfs_close` already go through.

- [ ] **Step 8: Order them in `main`'s prologue**

```rust
    announce(vfs);
    fs_demo(vfs);
    write_demo(vfs);
    // Slice 5.10b. `trunc_demo` runs **immediately after** `write_demo`: it
    // truncates what that probe wrote, and running it first would truncate an
    // empty file and prove nothing.
    trunc_demo(vfs);
    create_demo(vfs);
    dirgrow_demo(vfs);
    hole_demo(vfs);
    // The leak probe issues 256 deliberately failing writes, so it goes last
    // among the filesystem probes — the standing rule that a battery of
    // malformed requests must not be able to take the proofs before it down with
    // it if a peer wedges on one.
    leak_probe(vfs);
    exec_denials(pm, vfs);
```

- [ ] **Step 9: Extend `open_denials` by four**

Insert before the `close-twice` block, and change the marker and constant to 11:

```rust
    // Slice 5.10b: the flag-shaped refusals.
    for (name, path, flags, want) in [
        // `O_CREAT` cannot conjure a parent: `/no-such-dir` does not exist, so
        // MFS's own walk answers `ENOENT` for the *directory*, relayed unflattened.
        ("create-no-dir", "/no-such-dir/f", O_RDWR | O_CREAT, ENOENT),
        // ...nor replace a directory. The lookup *succeeds*, so the create arm is
        // never taken and `classify` answers `EISDIR`.
        ("create-is-dir", "/etc", O_RDWR | O_CREAT, EISDIR),
        // `O_TRUNC` on a directory is the same answer for the same reason, and it
        // is what keeps `FS_TRUNC` from ever being aimed at one. Delete
        // `classify`'s directory arm and this frees `/etc`'s zones.
        ("trunc-is-dir", "/etc", O_RDWR | O_TRUNC, EISDIR),
    ] {
        let rc = vfs_open_flags(vfs, path, flags);
        if rc == want {
            denied += 1;
        } else {
            return report_open_fail(vfs, name);
        }
    }

    // A flag bit this build does not honour. **Spelled relative to what it does
    // honour** ([`O_UNKNOWN_BIT`]), never as a literal, so that a flag becoming
    // real makes this probe fail loudly rather than pass vacuously — slice 5.8's
    // `VFS_WRITE + 1` probe and slice 5.10a's `write-file` probe are what that
    // rule is made of.
    if vfs_open_flags(vfs, ROOTFS_MOTD_PATH, O_RDWR | O_UNKNOWN_BIT) == EINVAL {
        denied += 1;
    } else {
        return report_open_fail(vfs, "bad-flag");
    }
```

Change the final line to `b"minix.rs init: open.deny ok n=11\n"`, set `const OPEN_DENIAL_PROBES: usize = 11;`, and extend `open_denials`' docstring with the four bullets.

- [ ] **Step 10: Lint and boot**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 180 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot-t8.log 2>&1
grep -a 'fs\.create\|fs\.dirgrow\|fs\.hole\|fs\.trunc\|fs\.leak\|open\.deny\|fs\.deny' /tmp/boot-t8.log
```

Expected, in prologue order:

```
minix.rs init: open.deny ok n=11
minix.rs init: fs.trunc ok n=0
minix.rs init: fs.create ok n=25
minix.rs init: fs.dirgrow ok n=25
minix.rs init: fs.hole ok
minix.rs init: fs.leak ok n=256
[diag vfs] fs.deny ok n=14
```

**The timeout is 180 s here, not 120.** The leak probe adds 256 round trips and this is the first boot that runs them; Task 9 measures the real cost and decides the CI budget. If a marker is missing, its `FAIL` spelling names the failing step — and before diagnosing anything, `grep -a 'error\[E' /tmp/boot-t8.log` to rule out a build failure, which produces a log with no kernel output at all and reports every marker MISSING.

- [ ] **Step 11: Commit**

```bash
git add userland/init/src/main.rs
git commit -s -m "test(init): boot proofs for create, growth, holes, truncate, and the leak fix (slice 5.10b)

Five probes, each reporting through the path under test because init is
user-grade and has no debug channel of its own.

fs.create and fs.dirgrow both create, write, close and re-open without O_CREAT,
so the file has to be findable by an ordinary lookup -- which is what proves
the directory entry reached the device and not merely that an inode was
allocated. dirgrow's target is /full, whose block ships exactly full, so its
create must grow the directory.

fs.hole is the only probe that reaches the second half of MFS's write-back
condition: /etc/holey's first block is a hole, so writing at position 0 assigns
a zone while the size does not move. It verifies every byte, not two windows --
there is no lseek, so reaching any offset already read everything before it,
and a mutation corrupting one byte in the middle would otherwise print ok.

fs.trunc runs immediately after write_demo, because truncating an empty file
proves nothing. fs.leak issues 256 failing writes and then one good one; the
count is ROOTFS_IMAGE_BLOCKS, which exceeds any free-zone count the image can
have, so no number in the marker differs between hello flavours.

open.deny grows to 11. The unknown-flag probe is spelled relative to what the
build honours rather than as a literal, so a flag becoming real makes it fail
loudly instead of passing vacuously."
```

---

## Task 9: Markers, the boot budget, mutation matrix, and the docs

**Files:**
- Modify: `tests/qemu-boot.expected`, `tests/qemu-boot.forbidden`
- Modify: `.github/workflows/ci.yml` (only if the measurement in Step 3 says so)
- Modify: `book/` — the filesystem and VFS chapters
- Modify: `docs/plan.md`, `docs/plans/phase-5-musl-fs.md`
- Modify: `CLAUDE.md` (a slice bullet, via the revise-claude-md skill)

- [ ] **Step 1: Update the marker files**

In `tests/qemu-boot.expected`, change `[diag vfs] fs.deny ok n=10` to `n=14` and `minix.rs init: open.deny ok n=7` to `n=11`, each with a comment naming what the four new probes are (mirroring how the existing counts are annotated). Then add the five new lines **in prologue order**, each with the comment style the file already uses — what the marker asserts, and what its absence would mean:

```
minix.rs init: fs.trunc ok n=0
minix.rs init: fs.create ok n=25
minix.rs init: fs.dirgrow ok n=25
minix.rs init: fs.hole ok
minix.rs init: fs.leak ok n=256
```

In `tests/qemu-boot.forbidden`, add the five FAIL spellings with one shared comment block:

```
# Slice 5.10b: a create/truncate probe ran and disagreed with itself. Distinct
# from the marker simply going missing (init never got that far): these
# spellings mean the probe executed and produced the wrong answer, and the ones
# carrying `off=` name the byte where it went wrong -- `fs.hole FAIL verify
# off=0` is a dropped zone pointer (the `dirty` half of the write-back
# condition), and `off=4096` is a write that disturbed the zone already there.
minix.rs init: fs.create FAIL
minix.rs init: fs.dirgrow FAIL
minix.rs init: fs.hole FAIL
minix.rs init: fs.trunc FAIL
minix.rs init: fs.leak FAIL
```

Then:

```bash
tools/check-boot-log.sh /tmp/boot-t8.log
```

Expected: every marker PASS, nothing forbidden found. **Do not** hand-copy the counts from this plan — recompute `n=25` from `ROOTFS_CREATE_TEXT.len()`; slice 5.8's plan said `n=30` for a 31-byte constant.

- [ ] **Step 2: Measure the boot cost against the merge base**

The leak probe's 256 extra round trips are the dominant new cost, and a slice can break the timing budget with "it passes locally" being no check at all. Measure the **last required marker's position as a fraction of a fixed-timeout log**, on the **musl** flavour — the one CI builds — against the same number at the merge base.

```bash
# After (current branch). Build first, so the rebuild does not land inside the
# timed run and skew the fraction.
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 240 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/after.log 2>&1
echo "after: $(grep -abo 'hello: errno ok' /tmp/after.log | head -1 | cut -d: -f1) of $(wc -c < /tmp/after.log)"
```

Then the merge base. **`git stash` does not give you the "before" once the slice is committed on a branch** — detach to the merge base, and stash only any uncommitted doc edits so `target/` and `target/musl-sysroot` survive and the two boots differ in nothing but the code:

```bash
git checkout --detach $(git merge-base main HEAD)
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release
timeout 240 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/before.log 2>&1
echo "before: $(grep -abo 'hello: errno ok' /tmp/before.log | head -1 | cut -d: -f1) of $(wc -c < /tmp/before.log)"
git checkout feature/slice-5.10b-mfs-create-truncate
```

Record both fractions in the PR description. **If the ratio climbs materially, raise `qemu-smoke`'s budget in `.github/workflows/ci.yml` with real headroom** — CI's TCG is slower than local, so think in the *ratio*, not in local wall-clock seconds. Both previous raises (45 → 120 → 240 s) came from exactly this measurement. The spec's R4 fallback, if the cost is unacceptable: derive the leak probe's count from the image's real free-zone count instead of from `ROOTFS_IMAGE_BLOCKS` — **but only if that number can be made config-independent**, which is why it is the fallback and not the design.

- [ ] **Step 3: Run the mutation matrix**

Apply, observe the named marker move, revert. **Against an uncommitted tree**, with every file you will mutate copied to the scratchpad *first* — including files this slice **adds**, since `git checkout -- <untracked file>` does not restore, it *errors*, and behind a `|| true` it leaves the mutation in the tree (slice 5.9 did exactly this).

```bash
SCRATCH=/private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/76546612-6807-4160-80ee-2f57c79353aa/scratchpad
mkdir -p "$SCRATCH/mutation"
for f in fs/mfs/src/main.rs fs/mfs/src/write.rs servers/vfs/src/main.rs \
         servers/vfs/src/open.rs userland/init/src/main.rs kernel-shared/src/fcntl.rs; do
  cp "$f" "$SCRATCH/mutation/$(echo "$f" | tr / _)"
done
```

| # | Mutation | Expected |
|---|---|---|
| 1 | Move the `stage.fill` call back to after `place_zone` in `do_write` | `fs.leak FAIL enospc` |
| 2 | Drop `dirty` from `do_write`'s write-back condition (`if dirty \|\| grown != …` → size only) | `fs.hole FAIL verify off=0` |
| 3 | In `create`, insert the dirent **before** `write_inode` | `fs.create FAIL verify` — the read-back is garbage |
| 4 | Free the bits before writing the inode in `do_trunc` (C7 reversed) | **no marker moves.** Record as *unproven*: it needs a failure between the two steps that nothing can induce |
| 5 | Delete `do_create`'s `EEXIST` arm (make `find_free_slot` ignore `Occupied`) | `fs.deny FAIL create-exists` — and the inode comparison is what catches it, not the errno alone |
| 6 | In `insert_entry`, return `ENOSPC` instead of appending when `free.is_none()` | `fs.dirgrow FAIL open` |
| 7 | `bitmap_clear` clears `bit + 1` | `fs.trunc` or a later write FAILs |
| 8 | `validate_flags` ignores unknown bits instead of `EINVAL` | `open.deny FAIL bad-flag` |

Row 4 is stated rather than hidden: it is a correct invariant this slice cannot probe, and saying so is the 5.10a `dirty` lesson applied to a new rule rather than repeated by omission.

Before recording **any** observation, rule out a build failure — `kernel/build.rs` panics on the nested server build, the log then holds no kernel output at all, and `check-boot-log.sh` reports every marker MISSING, which is indistinguishable from a mutation that worked:

```bash
grep -a 'error\[E' /tmp/mut.log || echo "no compile errors"
```

Restore from the scratchpad copies, never with `git checkout`, then prove the tree is clean:

```bash
for f in ...; do cp "$SCRATCH/mutation/$(echo "$f" | tr / _)" "$f"; diff -q "$SCRATCH/mutation/$(echo "$f" | tr / _)" "$f"; done
grep -rn MUTATION --include='*.rs' . || echo "clean"
```

The final `grep -rn MUTATION` sweep — never a restore command's exit status — is what proves the tree clean.

- [ ] **Step 4: Run the three-boot flavour matrix**

The **SDK flavour has zero CI coverage**, so this is local-only and mandatory whenever the image changes — and the image changed a lot here.

```bash
# 1. SDK, if one is installed.
MINIXRS_SDK=~/toolchains/minixrs cargo kernel-aarch64 && \
  timeout 240 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/sdk.log 2>&1; \
  tools/check-boot-log.sh /tmp/sdk.log

# 2. Forced in-tree musl -- what CI builds.
MINIXRS_SDK=/nonexistent cargo kernel-aarch64 && \
  timeout 240 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/musl.log 2>&1; \
  tools/check-boot-log.sh /tmp/musl.log

# 3. Sysroot moved aside -- and `MINIXRS_SDK=/nonexistent` is MANDATORY here.
#    On a machine with a usable SDK the flavour selector never reaches the
#    sysroot, so moving it aside alone re-runs row 1 and tests nothing.
mv target/musl-sysroot target/musl-sysroot.aside
MINIXRS_SDK=/nonexistent cargo kernel-aarch64 && \
  timeout 240 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/fallback.log 2>&1; \
  tools/check-boot-log.sh /tmp/fallback.log
mv target/musl-sysroot.aside target/musl-sysroot
```

Rows 1 and 3 lose the five C markers by design; every `fs.*`, `open.deny` and `fs.deny` marker must PASS in **all three**, which is the point — no number in a new marker may differ between flavours. The image's free-zone and free-inode headroom is what row 1 and row 3 are really testing (spec R1): `/bin/hello` is ~46 KB with the SDK and ~15 KB in the fallback, so the margins differ.

- [ ] **Step 5: Also run the stub-free configuration**

```bash
MINIXRS_SDK=/nonexistent cargo clippy -p minixrs-kernel --target aarch64-unknown-none --no-default-features -- -D warnings
timeout 240 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features > /tmp/nostub.log 2>&1
grep -a 'fs\.' /tmp/nostub.log
```

`--no-default-features` reaches the markers several times faster (stub C's kernel-call flood dominates a default boot), and it is the configuration where init's *first* reap really is a child it forked — the trap slice 5.5 recorded. Nothing this slice adds is keyed on a reap, but confirming the `fs.*` markers appear in both configurations is what makes them non-vacuous.

- [ ] **Step 6: Update the book**

`docs.yml` is path-filtered to `book/**`, so a slice that forgets the book ships a Pages site contradicting its own code, silently and indefinitely — slice 5.10a reached its final gate with the mdBook still asserting `BDEV_WRITE` answers `EROFS`. Every task in a subagent run sees one crate, so nobody re-reads the published docs; this step is where that gets caught.

```bash
grep -rn 'FS_LOOKUP\|FS_WRITE\|NR_FS_MSGS\|VFS_OPEN\|O_CREAT\|create\|truncate\|read-only\|read only' book/src/ | head -40
mdbook build book
```

Update at minimum: the FS-band request table (now six requests), the `VFS_OPEN` payload description (three fields), any sentence describing the filesystem as read-only or describing `open` as lookup-only, and the write path's description of what happens on a failed write (the leak is fixed; the docstring's three-case table is gone).

- [ ] **Step 7: Update the plan trackers**

In **both** `docs/plan.md` and `docs/plans/phase-5-musl-fs.md`:

- Flip **5.10a**'s stale `◀ ready (branch …, pending merge)` to `✓ shipped (PR #53, merged 2026-08-24)`. It is merged; the marker was never reconciled. Confirm the PR number and date from `git log` rather than from this plan.
- Mark **5.10b** `◀ ready (branch feature/slice-5.10b-mfs-create-truncate, pending merge)`.
- Slide `◀ next` to **5.11**.
- Reconcile any *other* older `◀ ready` markers against `git log` — stale "pending merge" labels accumulate otherwise.
- In `phase-5-musl-fs.md`'s 5.10b section, link the spec by relative path rather than restating it, and record the two corrections this slice made to 5.10a's hand-off: `FS_TRUNC` does **not** make the `dirty` case reachable (only a hole below EOF does), and the C7 truncate ordering is **unproven** for the same class of reason the `dirty` condition was.

- [ ] **Step 8: Revise CLAUDE.md**

```
/claude-md-management:revise-claude-md
```

The slice bullet should carry what a future slice cannot re-derive: the staging-buffer invariant and why it is not a rollback; that `Occupied` must win over `Free` across *all* blocks, not just within one; that the C7 ordering has no probe; that `/full` and `/etc/holey` exist because two arms are otherwise unreachable in both boot configurations; and that a denial probe's flag must be spelled relative to `O_KNOWN`.

- [ ] **Step 9: Final gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings
cargo clippy -p minixrs-kernel --target aarch64-unknown-none --no-default-features -- -D warnings
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo test -p minixrs-kernel-shared -p minixrs-gen-c-headers -p minixrs-mfs -p minixrs-mkfs-mfs -p minixrs-vfs
tools/check-dco.sh
git status --porcelain   # target/ artifacts must not be staged; no generated headers in the tree
```

- [ ] **Step 10: Commit, then STOP**

```bash
git add tests/ book/ docs/ CLAUDE.md .github/workflows/ci.yml
git commit -s -m "docs(5.10b): boot markers, book, and slice status for create/truncate

The five new markers and the two grown denial counts, with their FAIL
spellings added to the forbidden list -- a probe that ran and disagreed with
itself is a different thing from a marker that never appeared, and the ones
carrying off= name the byte where it went wrong.

The book is updated in the same change, because docs.yml is path-filtered to
book/** and a slice that forgets it ships a Pages site contradicting its own
code, silently and indefinitely; 5.10a reached its final gate that way.

The phase tracker also reconciles 5.10a's stale pending-merge marker, and
records the two corrections this slice made to its hand-off: FS_TRUNC does not
make the dirty write-back case reachable -- with no lseek every write runs
forward, so only a hole below EOF does, which is why the image now ships one --
and the truncate ordering is itself unproven, for the same class of reason."
```

**Then stop.** Pushing, opening a PR, and triggering CI all require the user's explicit approval. Surface the branch, the boot-timing measurement from Step 2, the mutation matrix results from Step 3 (row 4 included, as unproven), and the three-flavour matrix from Step 4, and ask.

---

## Self-review

Checked against the spec, section by section.

**Coverage.** §4.1 → Task 1; §4.2 → Task 1; §4.3 → Task 1; §4.4 → Task 2; §5.1 → Task 3; §5.2 → Task 5; §5.3 → Task 5; §5.4 → Task 4; §5.5 → Task 6; §5.6 → Task 7; §5.7 → Task 2; §5.8 → Task 8; §5.9 → Tasks 7 (`fs.deny`) and 8 (`open.deny`); §6 error taxonomy → Tasks 5–7 inline; §7 invariants → the docstrings each task specifies; §8 verification → Task 9. Every `C1`…`C11` has a task: C1/C2 Task 1+7, C3 Task 5, C4 Task 1+6, C5 Task 5, C6 Task 5, C7 Task 6, C8 Tasks 3+6, C9 Tasks 2+8, C10 Tasks 2+8, C11 Task 4.

**Two deviations from the spec, both deliberate and both stated at the point of use:**

1. **§5.5 step 4's batched bitmap walk is not implementable with one block buffer** — reading a bitmap block evicts the indirect block, so each slot costs a re-read. Task 6 says so in the code comment and in the docstring; the cost is bounded by C8's slot count, which is the reason that bound exists.
2. `Stage::addr()` is written but unused, carrying an `#[allow(dead_code)]` and a comment. It sits beside `Blocks::addr()` because a request that grants over the staging buffer is the obvious next use and a wrong answer there would be a wild copy rather than a compile error. If a reviewer prefers, deleting it is a one-line change with no consequence.

**Type consistency.** `Stage::fill` returns `Result<&[u8], i32>` and `do_write` calls it before `place_zone`; `find_free_slot` returns `Result<Option<(u64, usize)>, i32>` and `insert_entry` consumes exactly that; `fs_path_request` returns `Result<(u32, i32, i32), i32>` and `fs_lookup`/`fs_create` both delegate to it, so `do_open`'s two arms destructure the same shape; `validate_flags` returns `Result<OpenFlags, i32>` with the two bool fields `do_open` reads; `create_write_verify` returns `Result<(), &'static [u8]>` whose error feeds `report_step`; `report_at` gained a leading `marker` argument and all three existing call sites are updated in Task 8 Step 2.

**Arithmetic worth re-checking during implementation** (verify each against the code, do not take it from here):

- `/full`: `.` + `..` + 62 = 64 entries × 64 bytes = 4096 = one block exactly. `rootfs.rs`'s `const _` is what enforces it.
- Inodes: root + 3 directories + 6 `/etc`-and-`/bin` files + 62 filler = 72 shipped, + 3 created at boot = 75, against 128. The *sufficient* check is `kernel/build.rs`'s `free_inodes` assert against the built image.
- Runtime zones: 9 (`/etc/scratch`) + 1 (`/etc/new`) + 2 (`/full/new` and `/full`'s second block) + 1 (`/etc/holey`'s filled hole) + 1 (`/etc/leak`) = 14. Same: the build asserts it against the image.
- `O_KNOWN` = 3 | 64 | 512 = 579; `O_UNKNOWN_BIT` = 580 & !579 = 4, which is outside `O_KNOWN`. Two `const _`s pin both properties rather than the value.
- `ROOTFS_LEAK_PROBES` = 256 must exceed the free-zone count in **every** flavour: ~182 (musl), ~219 (SDK), ~227 (fallback). It does, and Task 9 Step 4 is what confirms it.

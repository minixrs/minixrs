# Slice 5.10a — MFS Write Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `write()` reach a real file on the MinixFS root image — `VFS_WRITE` on a file descriptor travels to MFS, which allocates zones and stores bytes through `BDEV_WRITE` on the `memory` ramdisk driver.

**Architecture:** One new FS-band request (`FS_WRITE`, reusing `FS_READ`'s payload), a zone allocator in `fs/mfs` covering direct and single-indirect zones, and a real store in `drivers/memory`. Every decision that can be made without a device lives in a pure, host-tested library module; the servers hold only IPC glue and ordering. Nothing new is allocated on any stack: MFS's single 4 KiB block buffer is reused in both directions.

**Tech Stack:** Rust `no_std` (nightly, pinned in `rust-toolchain.toml`), aarch64 QEMU, MinixFS v3 on-disk format, MINIX-style grants.

**Spec:** [`docs/superpowers/specs/2026-08-18-mfs-write-path-design.md`](../specs/2026-08-18-mfs-write-path-design.md) — decisions are cited below as **W1**–**W9**.

## Global Constraints

These apply to **every** task. They are project rules from `CLAUDE.md`, not preferences.

- **SPDX header first.** Every new `.rs` file begins with `// SPDX-License-Identifier: BSD-3-Clause` then `// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors`, before anything else including `//!` docs.
- **`checked_add`, never `+`, for offsets and lengths** in `servers/`, `drivers/`, `fs/`, `userland/`. `[profile.release]` sets `overflow-checks = false`, so `off + 4` *wraps* in the shipped binary while panicking under `cargo test`. Every new payload accessor gets a `usize::MAX` unit test.
- **Every commit is `git commit -s`** (DCO sign-off) and GPG-signed. Never `--no-gpg-sign`, never `--no-verify`. Verify with `git log -1 --format='%(trailers:key=Signed-off-by)'`.
- **Blocking gates before any push:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings`, the same with `--no-default-features`, and `cargo clippy -p minixrs-mfs --features server -- -D warnings`.
- **`fs/mfs`'s `main.rs` is behind `required-features = ["server"]`**, so no CI job except that one extra clippy step compiles it. Anything with a decision in it belongs in the lib, not `main.rs`.
- **No new stack buffers.** A server stack is exactly one page (`uspace::SERVER_STACK_BYTES` = 4096). Overrunning it faults into VM's SIGSEGV arm, which prints *nothing* `tests/qemu-boot.forbidden` catches. Check the largest frame with:
  ```sh
  "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump -d \
    target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-mfs \
    | grep -oE 'sub[[:space:]]+sp, sp, #0x[0-9a-f]+' | grep -oE '0x[0-9a-f]+' \
    | while read h; do printf '%d\n' "$h"; done | sort -n | tail -1
  ```
- **Boot to verify kernel/server behaviour**, never server-side logging (EL0 has no console):
  ```sh
  timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/boot.log 2>&1
  tools/check-boot-log.sh /tmp/boot.log
  ```
  Budget ~5 s for rebuild + UEFI before the first kernel byte. Grep the log with `grep -a`.
- **Bitmap bit order is fixed:** `byte = bit / 8`, `mask = 1 << (bit % 8)`. It matches `tools/mkfs-mfs`'s `Image::set_bit` and `verify.rs`'s `bit_set`; diverging silently corrupts every image.
- **The granter is always the kernel-stamped `m_source`,** never a payload field. No message in this slice grows a granter or a grant-offset field.

---

### Task 1: ABI — `FS_WRITE` and the scratch-file constants

**Files:**
- Modify: `kernel-shared/src/callnr.rs` (add `FS_WRITE` after `FS_READ` at ~line 782; `NR_FS_MSGS` at line 786; band tests from ~line 1500)
- Modify: `kernel-shared/src/rootfs.rs` (append after `rootfs_pattern_byte`, ~line 120)
- Test: inline `#[cfg(test)]` modules in both files

**Interfaces:**
- Consumes: nothing.
- Produces: `FS_WRITE: i32` (= `FS_RQ_BASE + 3` = `0x903`), `NR_FS_MSGS: usize = 4`, `ROOTFS_SCRATCH_PATH: &str`, `ROOTFS_SCRATCH_LEN: usize`, `ROOTFS_SCRATCH_PERIOD: usize`, `rootfs_scratch_byte(usize) -> u8`. The payload offsets are **unchanged** — `FS_WRITE` reuses `FS_INO_OFF` (0), `FS_GRANT_OFF` (4), `FS_LEN_OFF` (8), `FS_POS_OFF` (16).

- [ ] **Step 1: Write the failing tests**

In `kernel-shared/src/callnr.rs`, extend the existing FS-band test (search for `fn` containing `let msgs = [FS_READSUPER, FS_LOOKUP, FS_READ];`) and add a new one:

```rust
#[test]
fn fs_write_reuses_the_read_payload_offsets() {
    // W1: the same four fields at the same offsets. Not a coincidence to be
    // re-derived — the number `FS_LOOKUP` hands out is the number both
    // `FS_READ` and `FS_WRITE` take back, and one clamp/parse serves both.
    assert_eq!(FS_WRITE, FS_RQ_BASE + 3);
    assert_eq!(FS_INO_OFF, 0);
    assert_eq!(FS_GRANT_OFF, 4);
    assert_eq!(FS_LEN_OFF, 8);
    assert_eq!(FS_POS_OFF, 16);
    // The four fields are ordered, non-overlapping, and fit the 96-byte payload.
    let fields = [
        (FS_INO_OFF, 4),
        (FS_GRANT_OFF, 4),
        (FS_LEN_OFF, 4),
        (FS_POS_OFF, 8),
    ];
    assert_eq!(fields.len(), 4, "an FS_WRITE payload field was added");
    let mut end = 0usize;
    for (off, width) in fields {
        assert!(off >= end);
        end = off + width;
    }
    assert!(end <= 96);
}
```

In `kernel-shared/src/rootfs.rs`:

```rust
#[test]
fn the_scratch_file_spans_the_direct_indirect_seam() {
    // W9/W3: the write proof must cross 7 direct zones, or the single-indirect
    // allocation arm has no boot marker. Same reasoning ROOTFS_PATTERN_LEN
    // records for the read side.
    assert_eq!(ROOTFS_SCRATCH_LEN, 32 * 1024);
    assert_eq!(ROOTFS_SCRATCH_LEN.min(7 * BDEV_BLOCK_SIZE), 7 * BDEV_BLOCK_SIZE);
}

#[test]
fn the_scratch_generator_is_position_dependent_and_skewed_off_the_pattern() {
    // Skewed by 7 so a cross-file mix-up is a mismatch, not a coincidence.
    assert_ne!(rootfs_scratch_byte(0), rootfs_pattern_byte(0));
    // Non-repeating across a block: 251 is prime and coprime with 4096.
    assert_ne!(rootfs_scratch_byte(0), rootfs_scratch_byte(BDEV_BLOCK_SIZE));
    // Periodic with period 251, which is what lets init hold one source buffer.
    assert_eq!(rootfs_scratch_byte(0), rootfs_scratch_byte(ROOTFS_SCRATCH_PERIOD));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p minixrs-kernel-shared`
Expected: FAIL — `cannot find value FS_WRITE in this scope`, `cannot find value ROOTFS_SCRATCH_LEN in this scope`.

- [ ] **Step 3: Add the constants**

In `kernel-shared/src/callnr.rs`, immediately after `FS_READ`'s definition:

```rust
/// VFS → FS server: write bytes into a file.
///
/// **Payload is [`FS_READ`]'s, field for field** — inode, grant id, byte count,
/// position — because it is the same question in the other direction, and one
/// wire codec and one clamp serve both. The reply `m_type` is the byte count
/// written (`>= 0`), or a negative errno.
///
/// **A short write is normal here, not an error.** The FS server clamps every
/// request to the end of the block containing `pos`, so one call moves at most
/// [`FS_MAX_IO`] and usually less; VFS loops. That is [`CDEV_WRITE`]'s stance and
/// deliberately *not* [`BDEV_READ`]'s refuse-or-nothing — BDEV refuses because
/// its client is a filesystem that cannot interpret a fraction of a block, while
/// this request's client is VFS, whose whole job is hiding staging from POSIX.
///
/// The grant must carry `CPF_READ` (where [`FS_READ`]'s carries `CPF_WRITE`). The
/// kernel checks that in `verify_grant`; no server re-implements it. There is no
/// granter field and no grant-offset field: the granter is the kernel-stamped
/// `m_source`, and VFS issues a fresh grant over exactly the round's bytes.
pub const FS_WRITE: i32 = FS_RQ_BASE + 3;

/// Number of FS-band requests.
pub const NR_FS_MSGS: usize = 4;
```

Delete the old `pub const NR_FS_MSGS: usize = 3;` line.

Then update **every** existing enumeration of the FS band in the test module — search for `FS_READ,` and add `FS_WRITE,` beside it in each list. There are eight such lists (the cross-band distinctness sweeps, the `assert_ne!` loops, the contiguity check `let msgs = [FS_READSUPER, FS_LOOKUP, FS_READ];`, and the band-ordering tests). The contiguity check must become:

```rust
let msgs = [FS_READSUPER, FS_LOOKUP, FS_READ, FS_WRITE];
```

In `kernel-shared/src/rootfs.rs`, after `rootfs_pattern_byte`:

```rust
/// A file the root image ships **empty**, for slice 5.10a's write proof to fill.
///
/// Create does not exist until 5.10b, so the write path needs a target that is
/// already in the image. Zero-length rather than pre-sized: that makes
/// growth-from-nothing the ordinary path rather than a special case, and it keeps
/// `/etc/motd` and `/etc/pattern` — which are *read* proofs — untouched by a
/// probe that writes.
pub const ROOTFS_SCRATCH_PATH: &str = "/etc/scratch";

/// Bytes init writes to [`ROOTFS_SCRATCH_PATH`]: 32 KiB, i.e. 8 blocks.
///
/// **Mandatory rather than round.** Seven direct zones cover 28 KiB, so this
/// length is what puts the single-indirect *allocation* arm — and the allocation
/// of the indirect block itself — on a boot marker. The last zone is indirect
/// slot 0. Unlike [`ROOTFS_PATTERN_LEN`] this content is written at runtime, so
/// the length is a claim init proves rather than something the image asserts.
pub const ROOTFS_SCRATCH_LEN: usize = 32 * 1024;

/// Period of [`rootfs_scratch_byte`]. Prime, and coprime with the 4096-byte
/// block, so a lost, duplicated, or reordered block changes the bytes rather
/// than landing on the same value again — [`rootfs_pattern_byte`]'s reasoning.
///
/// It is *also* what lets init hold a single source buffer: init's write chunk is
/// a whole multiple of this, so every chunk's contents are identical and one
/// `const`-generated static is correct for all of them.
pub const ROOTFS_SCRATCH_PERIOD: usize = 251;

/// Byte `i` of what init writes to [`ROOTFS_SCRATCH_PATH`].
///
/// Skewed by 7 off [`rootfs_pattern_byte`] so that reading the wrong file is a
/// mismatch rather than a coincidence.
pub const fn rootfs_scratch_byte(i: usize) -> u8 {
    ((i + 7) % ROOTFS_SCRATCH_PERIOD) as u8
}

// The scratch file really does run past the direct zones, which is the whole
// reason its length is what it is. Anything shorter would make the
// single-indirect *allocation* arm unreachable without saying so.
const _: () = assert!(ROOTFS_SCRATCH_LEN > 7 * BDEV_BLOCK_SIZE);
// ...and it must stay inside the single-indirect span, which is what MFS's
// writer covers: 7 direct zones plus one block of 4-byte pointers.
const _: () = assert!(ROOTFS_SCRATCH_LEN <= (7 + BDEV_BLOCK_SIZE / 4) * BDEV_BLOCK_SIZE);
// The image has room for it: 8 data zones plus 1 indirect block, against an
// image whose other contents leave well over that free. A future image shrink
// fails here rather than at boot with ENOSPC.
const _: () = assert!(ROOTFS_SCRATCH_LEN / BDEV_BLOCK_SIZE + 1 < ROOTFS_IMAGE_BLOCKS as usize);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p minixrs-kernel-shared`
Expected: PASS, all tests.

- [ ] **Step 5: Regenerate and syntax-check the C headers**

`FS_WRITE` flows into the generated `minixrs/callnr.h`; the blocking `c-headers` CI gate compiles it hermetically.

Run:
```sh
cargo gen-c-headers
clang -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsyntax-only \
  -ffreestanding -nostdlibinc --target=aarch64-unknown-linux-musl \
  -Itarget/gen-c-headers/include target/gen-c-headers/abi-selftest.c
```
Expected: both succeed, no output. Confirm nothing was written into the tree: `git status --short` shows no files under `include/`.

- [ ] **Step 6: Commit**

```bash
git add kernel-shared/src/callnr.rs kernel-shared/src/rootfs.rs
git commit -s -m "feat(abi): FS_WRITE and the scratch-file constants (slice 5.10a)"
```

---

### Task 2: `drivers/memory` — `BDEV_WRITE` performs the store

**Files:**
- Modify: `drivers/memory/src/main.rs` (`do_write` at ~line 288-312; module docs at ~line 35)
- Modify: `drivers/memory/src/bdev.rs` (docs at ~line 40-58; tests from ~line 164)
- Test: inline `#[cfg(test)]` in `bdev.rs`

**Interfaces:**
- Consumes: `bdev::parse_read(&Message) -> ReadRequest`, `bdev::validate_read(ReadRequest, u64) -> Result<(u64, usize), i32>` — both already exist and are direction-agnostic.
- Produces: a `BDEV_WRITE` that returns the byte count stored (`>= 0`) instead of `EROFS`.

- [ ] **Step 1: Write the failing tests**

Append to `drivers/memory/src/bdev.rs`'s test module:

```rust
#[test]
fn a_write_validates_exactly_like_a_read() {
    // The payload and every geometry check are shared (W1's sibling on the BDEV
    // band). The one field that differs is the grant's access bit, which the
    // kernel checks in `verify_grant`, not this driver.
    let m = request(BDEV_MINOR_RAMDISK, 5, BDEV_BLOCK_SIZE as i32, 3);
    let req = parse_read(&m);
    assert_eq!(
        validate_read(req, 256),
        Ok((3 * BDEV_BLOCK_SIZE as u64, BDEV_BLOCK_SIZE))
    );
}

#[test]
fn an_over_long_write_is_refused_not_clamped() {
    // Same rule as the read, and for the same reason: a client that cannot
    // interpret a fraction of a block gains nothing from a short transfer, and
    // EIO stays reserved for Phase 6's real media errors.
    let m = request(BDEV_MINOR_RAMDISK, 5, BDEV_BLOCK_SIZE as i32 + 1, 0);
    assert_eq!(validate_read(parse_read(&m), 256), Err(EINVAL));
}

#[test]
fn a_write_past_the_device_is_einval() {
    let m = request(BDEV_MINOR_RAMDISK, 5, BDEV_BLOCK_SIZE as i32, 256);
    assert_eq!(validate_read(parse_read(&m), 256), Err(EINVAL));
}
```

If `request`, `BDEV_MINOR_RAMDISK`, or `EINVAL` are not already in scope in that test module, add them to its `use super::*;` block — check the existing tests at line 164-178 for the exact helper signature (`fn request(minor: i32, gid: i32, len: i32, block: u64) -> Message`).

- [ ] **Step 2: Run to verify they fail or pass**

Run: `cargo test -p minixrs-memory`
Expected: these three PASS immediately — `validate_read` is already direction-agnostic, which is the point. They are regression pins for the behaviour Step 3 starts depending on, not a red-to-green cycle. **If any fails, stop**: the shared-validation premise is wrong and `do_write` needs its own checks.

- [ ] **Step 3: Make the write real**

In `drivers/memory/src/main.rs`, replace `do_write` entirely:

```rust
/// Serve one `BDEV_WRITE`. Returns the reply `m_type`: the byte count stored
/// (`>= 0`), or a negative errno.
///
/// The mirror of [`do_read`], and deliberately built from the same parse and the
/// same validation: a write to minor 7 hears `ENXIO` ("no such device") before
/// anything else, the check-order discipline `validate_read` documents.
///
/// **The direction is the whole difference.** `SAFECOPY_FROM` pulls the client's
/// bytes into the device; the grant must therefore carry `CPF_READ` where a read
/// needs `CPF_WRITE`. The kernel enforces that in `verify_grant` — this driver
/// does not check it and must not, because a driver that re-derived the grant
/// rules would be a second place for them to drift.
///
/// The grant offset is `0`, not a payload field: `BDEV_WRITE` deliberately has
/// none, because every client grants a buffer whose block starts at its
/// beginning.
#[cfg_attr(test, allow(dead_code))]
fn do_write(caller_e: Endpoint, msg: &Message, va: u64, blocks: u64) -> i32 {
    let req = bdev::parse_read(msg);
    let (byte_off, n) = match bdev::validate_read(req, blocks) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if n == 0 {
        // A legal zero-length write. No grant is used and nothing is copied, so a
        // client polling with `len = 0` cannot use it to probe a grant.
        return 0;
    }

    let rc = sys_safecopy(SAFECOPY_FROM, caller_e, req.gid, 0, va + byte_off, n as u64);
    if rc != OK {
        // Verbatim: EPERM ("your grant does not authorize this") and EFAULT
        // ("your buffer is not mapped") are different bugs on the client's side.
        return rc;
    }
    n as i32
}
```

Update the call site in `main`'s dispatch loop (~line 127) — `do_write` now takes the caller:

```rust
            BDEV_WRITE => {
                let rc = do_write(caller_e, &msg, va, blocks);
                reply(caller_e, &mut msg, rc);
            }
```

Fix imports: add `SAFECOPY_FROM` to the `minixrs_server_rt` import list, and **remove `EROFS`** from the `minixrs_kernel_shared::error` import (it becomes unused and `-D warnings` will reject it).

Rewrite the module-doc paragraph at ~line 35 that begins **`//! **`BDEV_WRITE` answers `EROFS`, not `ENOSYS`.**`** as:

```rust
//! **`BDEV_WRITE` stores, as of slice 5.10a.** It was defined and answering
//! `EROFS` from slice 5.7 — deliberately not folded into the unknown-request
//! `ENOSYS` arm, because "does not know about writes" and "knows and refuses"
//! are different things to tell a client. That distinction is what made this a
//! one-arm change: the geometry validation was already here, so 5.10a replaced a
//! refusal with a `SAFECOPY_FROM` rather than adding a request.
```

- [ ] **Step 4: Verify it builds and boots**

Run:
```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p minixrs-memory
timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/t2.log 2>&1
tools/check-boot-log.sh /tmp/t2.log
```
Expected: clippy clean, tests pass, **all markers still PASS**. Nothing writes yet, so the boot is unchanged — except MFS's `BDEV_WRITE → EROFS` denial probe, which now fails. Note its marker name from the output; Task 4 re-points it.

- [ ] **Step 5: Commit**

```bash
git add drivers/memory/src/main.rs drivers/memory/src/bdev.rs
git commit -s -m "feat(memory): BDEV_WRITE stores instead of refusing (slice 5.10a)"
```

---

### Task 3: `fs/mfs` — the pure write policy and allocator

**Files:**
- Create: `fs/mfs/src/write.rs`
- Modify: `fs/mfs/src/lib.rs` (add `pub mod write;` beside `pub mod read;`)
- Test: inline `#[cfg(test)]` in `write.rs`

**Interfaces:**
- Consumes: `crate::walk::Chunk { len: usize, off_in_block: usize }`, `crate::inode::{NR_DIRECT_ZONES, SINGLE_INDIRECT_SLOT}`, `crate::read::ptrs_per_block`, `minixrs_kernel_shared::callnr::FS_MAX_IO`, `minixrs_kernel_shared::error::{EFBIG, EINVAL, EIO}`.
- Produces, for Task 4:
  - `pub enum ZoneSlot { Direct(usize), Indirect(usize), OutOfRange }`
  - `pub fn zone_slot_for_offset(off: u64, bs: usize) -> ZoneSlot`
  - `pub fn clamp_write(pos: u64, len: i32, bs: usize) -> Result<Chunk, i32>`
  - `pub fn bitmap_find_free(block: &[u8], from_bit: u32, limit_bits: u32) -> Option<u32>`
  - `pub fn bitmap_set(block: &mut [u8], bit: u32) -> Option<()>`
  - `pub fn grow_size(cur: i32, pos: u64, n: usize) -> Result<i32, i32>`

- [ ] **Step 1: Write the failing tests**

Create `fs/mfs/src/write.rs` containing **only** the SPDX header, the module docs, the `use` block, and this test module. The functions come in Step 3.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const BS: usize = crate::MFS_BLOCK_SIZE;
    /// First byte the single-indirect region covers: seven direct zones in.
    const SEAM: u64 = (NR_DIRECT_ZONES * crate::MFS_BLOCK_SIZE) as u64;
    /// One past the last byte the single-indirect span can address.
    const SPAN_END: u64 = SEAM + (BS / 4 * BS) as u64;

    // ----- zone_slot_for_offset ---------------------------------------------

    #[test]
    fn offset_zero_is_direct_slot_zero() {
        assert_eq!(zone_slot_for_offset(0, BS), ZoneSlot::Direct(0));
    }

    #[test]
    fn the_last_direct_byte_and_the_first_indirect_byte_are_adjacent() {
        assert_eq!(zone_slot_for_offset(SEAM - 1, BS), ZoneSlot::Direct(6));
        assert_eq!(zone_slot_for_offset(SEAM, BS), ZoneSlot::Indirect(0));
    }

    #[test]
    fn the_last_addressable_byte_is_the_last_indirect_slot() {
        assert_eq!(zone_slot_for_offset(SPAN_END - 1, BS), ZoneSlot::Indirect(BS / 4 - 1));
        assert_eq!(zone_slot_for_offset(SPAN_END, BS), ZoneSlot::OutOfRange);
    }

    #[test]
    fn a_zero_block_size_is_out_of_range_not_a_division_by_zero() {
        assert_eq!(zone_slot_for_offset(0, 0), ZoneSlot::OutOfRange);
    }

    // ----- clamp_write ------------------------------------------------------

    #[test]
    fn a_write_stops_at_the_end_of_its_block() {
        // W2: one call moves at most one block, so the server stages through its
        // single buffer. The caller re-sends for the rest.
        let c = clamp_write(4000, 4096, BS).unwrap();
        assert_eq!(c.off_in_block, 4000);
        assert_eq!(c.len, 96);
    }

    #[test]
    fn a_write_starting_on_a_block_boundary_may_fill_it() {
        let c = clamp_write(BS as u64, 4096, BS).unwrap();
        assert_eq!(c.off_in_block, 0);
        assert_eq!(c.len, BS);
    }

    #[test]
    fn a_write_is_capped_at_one_transfer() {
        // Asserts the cap's exact value, not `len <= cap` -- the latter passes
        // even if the cap is removed entirely, and this is its only test.
        let c = clamp_write(0, i32::MAX, BS).unwrap();
        assert_eq!(c.len, FS_MAX_IO);
    }

    #[test]
    fn a_write_past_end_of_file_is_allowed_because_that_is_how_a_file_grows() {
        // The one way clamp_write differs from clamp_read: a read clamps at EOF,
        // a write does not. No size is consulted here at all.
        let c = clamp_write(SEAM, 100, BS).unwrap();
        assert_eq!(c.len, 100);
        assert_eq!(c.off_in_block, 0);
    }

    #[test]
    fn a_write_past_the_single_indirect_span_is_efbig() {
        assert_eq!(clamp_write(SPAN_END, 1, BS), Err(EFBIG));
    }

    #[test]
    fn a_negative_length_is_einval() {
        // Left unchecked it would widen into a ~16 EiB u64 on the safecopy.
        assert_eq!(clamp_write(0, -1, BS), Err(EINVAL));
    }

    #[test]
    fn a_zero_block_size_is_einval() {
        assert_eq!(clamp_write(0, 1, 0), Err(EINVAL));
    }

    #[test]
    fn a_zero_length_write_is_ok_not_an_error() {
        assert_eq!(clamp_write(0, 0, BS), Ok(Chunk { len: 0, off_in_block: 0 }));
    }

    // ----- bitmap -----------------------------------------------------------

    #[test]
    fn a_free_bit_is_found_at_the_first_zero() {
        let mut b = [0u8; 8];
        b[0] = 0b0000_0111;
        assert_eq!(bitmap_find_free(&b, 0, 64), Some(3));
    }

    #[test]
    fn the_search_starts_where_it_is_told() {
        let mut b = [0u8; 8];
        b[0] = 0b0000_0111;
        assert_eq!(bitmap_find_free(&b, 5, 64), Some(5));
    }

    #[test]
    fn a_full_byte_is_skipped_and_the_next_free_bit_found() {
        let mut b = [0u8; 8];
        b[0] = 0xff;
        b[1] = 0b0000_0001;
        assert_eq!(bitmap_find_free(&b, 0, 64), Some(9));
    }

    #[test]
    fn a_full_bitmap_has_no_free_bit() {
        let b = [0xffu8; 8];
        assert_eq!(bitmap_find_free(&b, 0, 64), None);
    }

    #[test]
    fn the_limit_is_respected_even_when_the_block_is_longer() {
        // The zone bitmap is deliberately over-sized (layout.rs's module docs),
        // so bits past the real zone count must never be handed out.
        let b = [0u8; 8];
        assert_eq!(bitmap_find_free(&b, 0, 5), Some(0));
        assert_eq!(bitmap_find_free(&b, 5, 5), None);
    }

    #[test]
    fn setting_a_bit_uses_minix_ordering() {
        // byte = bit/8, mask = 1 << (bit%8) -- matches mkfs's Image::set_bit and
        // verify.rs's bit_set. Diverging silently corrupts every image.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_set(&mut b, 9), Some(()));
        assert_eq!(b[1], 0b0000_0010);
    }

    #[test]
    fn setting_a_bit_past_the_block_is_none_not_a_panic() {
        let mut b = [0u8; 8];
        assert_eq!(bitmap_set(&mut b, 64), None);
    }

    // ----- grow_size --------------------------------------------------------

    #[test]
    fn a_write_inside_the_file_does_not_shrink_it() {
        assert_eq!(grow_size(1000, 0, 10), Ok(1000));
    }

    #[test]
    fn a_write_past_the_end_extends_the_file() {
        assert_eq!(grow_size(1000, 990, 100), Ok(1090));
    }

    #[test]
    fn a_size_that_would_not_fit_the_on_disk_field_is_efbig() {
        // MinixFS stores size as a 32-bit field; wrapping it would report a huge
        // file as a tiny one.
        assert_eq!(grow_size(0, i32::MAX as u64, 1), Err(EFBIG));
        assert_eq!(grow_size(0, u64::MAX, 1), Err(EFBIG));
    }

    #[test]
    fn a_negative_stored_size_is_eio() {
        // A corrupt inode, not a caller error.
        assert_eq!(grow_size(-1, 0, 1), Err(EIO));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p minixrs-mfs`
Expected: FAIL to compile — `cannot find function zone_slot_for_offset`, etc.

- [ ] **Step 3: Implement**

Prepend the SPDX header and docs, then the implementation, above the test module in `fs/mfs/src/write.rs`:

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The write path's policy and allocator — everything about writing a MinixFS
//! file that can be decided without a device (slice 5.10a).
//!
//! `read.rs`'s twin, and split from it for the same reason: there is no I/O here
//! and no borrowed device state, so every rule carries a unit test that needs no
//! fake block driver. `main.rs` is behind `required-features = ["server"]` and
//! therefore invisible to every CI job, which makes "anything with a decision in
//! it lives in the lib" a hard rule rather than a preference.
//!
//! ## Where this differs from the reader, and why
//!
//! [`clamp_write`] consults **no file size**. A read clamps at EOF because there
//! is nothing past it; a write past EOF is how a file grows. The size is not an
//! input to the transfer at all — it is an *output*, computed by [`grow_size`]
//! after the bytes land.
//!
//! [`zone_slot_for_offset`] is [`crate::read::zone_for_offset`]'s allocating
//! twin. The reader asks *what zone is there*, and must distinguish a hole from
//! an unaddressable offset. The writer asks *where a zone would go*, which is a
//! different question with a different answer type — folding them together would
//! mean one of the two callers ignoring half of every result.
//!
//! ## Bit order is not a free choice
//!
//! `byte = bit / 8`, `mask = 1 << (bit % 8)` — identical to `tools/mkfs-mfs`'s
//! `Image::set_bit` and its `verify.rs` reader. The image is written by one and
//! read by the other two; a divergence here corrupts every image silently.

use crate::inode::NR_DIRECT_ZONES;
use crate::read::ptrs_per_block;
use crate::walk::Chunk;
use minixrs_kernel_shared::callnr::FS_MAX_IO;
use minixrs_kernel_shared::error::{EFBIG, EINVAL, EIO};

/// Where the zone backing a given file offset *would* live.
///
/// Contrast [`crate::read::ZoneLookup`], which reports what is actually there.
/// There is no `Hole` variant: a hole is not a property of the offset, it is a
/// property of the pointer the caller finds at the slot this names.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ZoneSlot {
    /// Slot `i` of the inode's direct zone array (`i < NR_DIRECT_ZONES`).
    Direct(usize),
    /// Slot `i` of the single-indirect block named by
    /// [`SINGLE_INDIRECT_SLOT`] of the inode's zone array.
    Indirect(usize),
    /// Past what the single-indirect span can address. Double-indirect is not
    /// implemented on either side of this crate.
    OutOfRange,
}

/// Which slot backs byte offset `off`.
pub fn zone_slot_for_offset(off: u64, bs: usize) -> ZoneSlot {
    if bs == 0 {
        return ZoneSlot::OutOfRange;
    }
    let index = off / bs as u64;
    if index < NR_DIRECT_ZONES as u64 {
        return ZoneSlot::Direct(index as usize);
    }
    let slot = index - NR_DIRECT_ZONES as u64;
    if slot >= ptrs_per_block(bs) as u64 {
        return ZoneSlot::OutOfRange;
    }
    ZoneSlot::Indirect(slot as usize)
}

/// What one `FS_WRITE` round may move.
///
/// Three rules, in the order they are applied:
///
/// 1. `len < 0` or `bs == 0` → `EINVAL`, before anything else. A negative length
///    left unchecked widens into a ~16 EiB `u64` byte count on the safecopy.
/// 2. An offset the single-indirect span cannot address → `EFBIG`. This is the
///    file-size limit, and it is reported before any device work happens.
/// 3. The transfer is clamped to [`FS_MAX_IO`] and to the end of the block
///    containing `pos`, so it never straddles two blocks — which is what lets the
///    server stage through one buffer.
///
/// **No size is consulted.** See the module docs.
pub fn clamp_write(pos: u64, len: i32, bs: usize) -> Result<Chunk, i32> {
    if len < 0 || bs == 0 {
        return Err(EINVAL);
    }
    if matches!(zone_slot_for_offset(pos, bs), ZoneSlot::OutOfRange) {
        return Err(EFBIG);
    }
    let off_in_block = (pos % bs as u64) as usize;
    let to_block_end = bs - off_in_block;
    let len = (len as u64)
        .min(FS_MAX_IO as u64)
        .min(to_block_end as u64) as usize;
    Ok(Chunk { len, off_in_block })
}

/// First clear bit at or after `from_bit`, below `limit_bits`.
///
/// `limit_bits` is not redundant with the block's length: the zone bitmap is
/// deliberately over-sized (see `layout.rs`'s module docs), so the tail of the
/// last bitmap block describes zones that do not exist and must never be handed
/// out.
pub fn bitmap_find_free(block: &[u8], from_bit: u32, limit_bits: u32) -> Option<u32> {
    let mut bit = from_bit;
    while bit < limit_bits {
        let byte = *block.get((bit / 8) as usize)?;
        if byte == 0xff {
            // Skip to the first bit of the next byte. Cheap, and a full bitmap
            // block is 32768 bits.
            bit = (bit | 7).checked_add(1)?;
            continue;
        }
        if byte & (1 << (bit % 8)) == 0 {
            return Some(bit);
        }
        bit = bit.checked_add(1)?;
    }
    None
}

/// Mark `bit` allocated. `None` if it lies past the block, which is how a caller
/// that mixed up its bitmap arithmetic finds out rather than by writing into the
/// wrong byte.
pub fn bitmap_set(block: &mut [u8], bit: u32) -> Option<()> {
    let byte = block.get_mut((bit / 8) as usize)?;
    *byte |= 1 << (bit % 8);
    Some(())
}

/// The file's size after `n` bytes land at `pos`.
///
/// `EFBIG` rather than a wrap: MinixFS stores size in a 32-bit field, and
/// wrapping it would report a huge file as a tiny one — a corruption that reads
/// back as truncation. `EIO` for a negative stored size, which is a corrupt
/// inode rather than anything the caller did.
pub fn grow_size(cur: i32, pos: u64, n: usize) -> Result<i32, i32> {
    if cur < 0 {
        return Err(EIO);
    }
    let end = pos.checked_add(n as u64).ok_or(EFBIG)?;
    if end > i32::MAX as u64 {
        return Err(EFBIG);
    }
    Ok((cur as u64).max(end) as i32)
}
```

Add to `fs/mfs/src/lib.rs`, beside the other `pub mod` lines:

```rust
pub mod write;
```

- [ ] **Step 4: Run tests to verify they pass**

Run:
```sh
cargo test -p minixrs-mfs
cargo clippy -p minixrs-mfs --all-targets -- -D warnings
```
Expected: all PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add fs/mfs/src/write.rs fs/mfs/src/lib.rs
git commit -s -m "feat(mfs): the pure write policy and zone allocator (slice 5.10a)"
```

---

### Task 4: `fs/mfs` server — `FS_WRITE`

**Files:**
- Modify: `fs/mfs/src/main.rs` (`Blocks` impl ~line 142-195; dispatch `match` ~line 269-274; add `do_write` after `do_read` ~line 425; `bdev_denials` ~line 809)
- Modify: `fs/mfs/src/proto.rs` (`ReadRequest` doc ~line 38-56)

**Interfaces:**
- Consumes: Task 3's `write::{ZoneSlot, zone_slot_for_offset, clamp_write, bitmap_find_free, bitmap_set, grow_size}`; Task 1's `FS_WRITE`; Task 2's real `BDEV_WRITE`. Existing: `proto::parse_read`, `read_inode`, `Blocks::read`, `Blocks::zeroed`, `bdev_request`, `walk::zone_ok`, `read::{inode_location, inode_at}`.
- Produces: an `FS_WRITE` arm. No new public API — this is the server binary.

- [ ] **Step 1: Widen the block grant and add the write primitives**

`Blocks` is the only path to the buffer, and it now needs both directions.

In `fs/mfs/src/main.rs`, find where the block grant is issued (in `fn device`, ~line 646) and change the access flags from `CPF_WRITE` to `CPF_READ | CPF_WRITE`. Add to that grant field's doc comment on the `gid` member:

```rust
    /// Grant naming [`BLOCK`] with the driver as grantee, `CPF_READ | CPF_WRITE`.
    /// Issued once at boot: the buffer is a static, so its address never changes
    /// and `GrantPool::ensure_registered` never re-fires.
    ///
    /// **Both directions on one grant** (W5): the driver writes into this buffer
    /// on a `BDEV_READ` and reads out of it on a `BDEV_WRITE`, and the grantee is
    /// the same driver either way. A second grant would name the same bytes to
    /// the same peer. The kernel checks the direction bit per call, so widening
    /// the flags does not widen what any single call may do.
```

Then add two methods to `impl Blocks`, after `zeroed`:

```rust
    /// The block buffer, mutable — for the splice a partial write performs.
    ///
    /// `&mut self` for the reason `read` takes it: this hands out the only
    /// reference into the buffer, and the borrow checker is what keeps it the
    /// only one.
    fn buf_mut(&mut self) -> &mut [u8; MFS_BLOCK_SIZE] {
        // SAFETY: as `read` — `&mut self` is held, so this is the only reference
        // into the buffer for as long as the returned one lives.
        unsafe { &mut *BLOCK.0.get() }
    }

    /// Store the buffer's current contents as block `block`.
    ///
    /// A short reply is [`EIO`] like a short read: this server cannot store a
    /// fraction of a block, which is exactly why `BDEV_WRITE` refuses an
    /// over-long request rather than clamping it.
    fn write(&mut self, block: u64) -> Result<(), i32> {
        let rc = bdev_request(
            self.mem,
            BDEV_WRITE,
            BDEV_MINOR_RAMDISK,
            self.gid,
            MFS_BLOCK_SIZE as i32,
            block,
        );
        if rc != MFS_BLOCK_SIZE as i32 {
            return Err(EIO);
        }
        Ok(())
    }
```

Add `BDEV_WRITE` to the `minixrs_kernel_shared::callnr` import list.

- [ ] **Step 2: Add the allocator and the write handler**

Insert after `do_read` in `fs/mfs/src/main.rs`:

```rust
/// Serve one `FS_WRITE`. Returns the reply `m_type`: the byte count written
/// (`>= 0`), or a negative errno.
///
/// The payload is `FS_READ`'s, so [`proto::parse_read`] parses it — see W1 in the
/// slice design. Same field for the granter, too: there is none, and there must
/// never be one. This server holds `SYS_SAFECOPY`, so a caller-supplied granter
/// would aim a privileged cross-address-space copy wherever the caller pointed.
///
/// **The step order is what makes one block buffer sufficient.** Each step
/// finishes with the buffer's contents either consumed or flushed before the next
/// begins; there is never a moment where two blocks are wanted at once, and the
/// borrow checker enforces it because every `Blocks` method takes `&mut self`.
///
///   1. Read the inode. `Inode` is `Copy`, so the buffer is free again at the
///      `let`.
///   2. Clamp — this is where `EFBIG` is decided, before any device work.
///   3. Resolve or allocate the zone. A freshly allocated zone is **zeroed and
///      written before its number is stored anywhere** (W4): the bitmap bit goes
///      first, so a failure between the two leaks a zone rather than sharing one.
///   4. Read the target block unless the write covers it whole, splice the
///      caller's bytes in through `SAFECOPY_FROM`, store the block.
///   5. Write the inode back if it changed.
#[cfg_attr(test, allow(dead_code))]
fn do_write(msg: &Message, granter: Endpoint, blocks: &mut Blocks, mount: &Option<Mount>) -> i32 {
    let Some(mount) = mount else {
        return ENODEV;
    };
    let req = proto::parse_read(msg);
    let Ok(ino) = u32::try_from(req.ino) else {
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

    let chunk = match write::clamp_write(req.pos, req.len, mount.block_size) {
        Ok(chunk) => chunk,
        Err(e) => return e,
    };
    if chunk.len == 0 {
        return 0;
    }

    // Step 3. `dirty` records whether the inode changed at all -- see step 5.
    let (zone, mut dirty) = match place_zone(blocks, mount, &mut node, req.pos) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Step 4.
    if chunk.len < mount.block_size {
        // A partial write preserves the bytes around it, so the block has to be
        // read before it is spliced. A full-block write skips this: every byte is
        // about to be replaced.
        if blocks.read(u64::from(zone)).is_err() {
            return EIO;
        }
    }
    let dst = blocks.buf_mut();
    let Some(end) = chunk.off_in_block.checked_add(chunk.len) else {
        return EIO;
    };
    let Some(window) = dst.get_mut(chunk.off_in_block..end) else {
        // Unreachable: `clamp_write` guarantees a chunk lies inside one block.
        // Say `EIO` rather than indexing, so a future clamp bug is an errno
        // instead of a panic in a server nothing can restart.
        return EIO;
    };
    let rc = sys_safecopy(
        SAFECOPY_FROM,
        granter,
        req.gid,
        0,
        window.as_mut_ptr() as usize as u64,
        chunk.len as u64,
    );
    if rc != OK {
        // Verbatim: `EPERM` ("your grant does not authorize this") and `EFAULT`
        // ("your buffer is not mapped") are different bugs on the caller's side.
        return rc;
    }
    if let Err(e) = blocks.write(u64::from(zone)) {
        return e;
    }

    // Step 5. The condition is "a zone was assigned **or** the size grew", not
    // "the size grew": filling a hole in the middle of an existing file assigns
    // `zone[i]` without moving `size` at all, and keying on size alone would drop
    // that pointer while leaving its bitmap bit set -- the bitmap and the inode
    // disagreeing about a live zone, which is corruption rather than a leak.
    let grown = match write::grow_size(node.size, req.pos, chunk.len) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if grown != node.size {
        node.size = grown;
        dirty = true;
    }
    if dirty {
        if let Err(e) = write_inode(blocks, mount, ino, &node) {
            return e;
        }
    }

    chunk.len as i32
}

/// Resolve the zone backing byte `pos` of `node`, allocating it — and any
/// indirect block it needs — if it is a hole.
///
/// Returns `(zone, inode changed)`. `node` is patched in place when a direct
/// pointer or the indirect pointer is assigned; the caller writes it back.
#[cfg_attr(test, allow(dead_code))]
fn place_zone(
    blocks: &mut Blocks,
    mount: &Mount,
    node: &mut Inode,
    pos: u64,
) -> Result<(u32, bool), i32> {
    match write::zone_slot_for_offset(pos, mount.block_size) {
        write::ZoneSlot::Direct(i) => {
            let existing = node.zone[i];
            if existing != 0 {
                return if walk::zone_ok(existing, mount.blocks) {
                    Ok((existing, false))
                } else {
                    Err(EIO)
                };
            }
            let zone = alloc_zone(blocks, mount)?;
            node.zone[i] = zone;
            Ok((zone, true))
        }
        write::ZoneSlot::Indirect(slot) => {
            let mut dirty = false;
            let mut indirect = node.zone[SINGLE_INDIRECT_SLOT];
            if indirect == 0 {
                indirect = alloc_zone(blocks, mount)?;
                node.zone[SINGLE_INDIRECT_SLOT] = indirect;
                dirty = true;
            }
            if !walk::zone_ok(indirect, mount.blocks) {
                return Err(EIO);
            }
            // Read the pointer out and let the borrow die immediately: `u32` is
            // `Copy`, so nothing points into the buffer when the next fetch
            // replaces it. `resolve_zone` is split for exactly this reason.
            let blk = blocks.read(u64::from(indirect))?;
            let existing = zone_from_indirect(blk, slot).ok_or(EIO)?;
            if existing != 0 {
                return if walk::zone_ok(existing, mount.blocks) {
                    Ok((existing, dirty))
                } else {
                    Err(EIO)
                };
            }
            let zone = alloc_zone(blocks, mount)?;
            // Re-read the indirect block (the allocation replaced the buffer),
            // patch the slot, store it.
            blocks.read(u64::from(indirect))?;
            let buf = blocks.buf_mut();
            let Some(cell) = slot
                .checked_mul(4)
                .and_then(|s| s.checked_add(4).map(|e| (s, e)))
                .and_then(|(s, e)| buf.get_mut(s..e))
            else {
                return Err(EIO);
            };
            cell.copy_from_slice(&zone.to_le_bytes());
            blocks.write(u64::from(indirect))?;
            Ok((zone, dirty))
        }
        // Unreachable: `clamp_write` already answered `EFBIG` for this offset.
        // Kept so a future clamp change is an errno, not a wrong zone.
        write::ZoneSlot::OutOfRange => Err(EFBIG),
    }
}

/// Allocate one zone: find a clear bit in the zone bitmap, set it, and zero the
/// zone it names.
///
/// **The bit is set before the zone is used**, so a failure part-way leaks a zone
/// rather than handing the same one out twice. A leak is recoverable by a future
/// `fsck`; a shared zone is silent corruption.
///
/// The scan is bounded by `layout.zmap_blocks` — every device-derived loop has a
/// cap, because a corrupt superblock must not spin this server and, through it,
/// VFS and init.
#[cfg_attr(test, allow(dead_code))]
fn alloc_zone(blocks: &mut Blocks, mount: &Mount) -> Result<u32, i32> {
    let bs = mount.block_size;
    let bits_per_block = (bs * 8) as u32;
    // The bitmap is based at `first_data_zone - 1` (MINIX's convention, recorded
    // in `layout::zmap_bit`), so bit 1 is the first data zone. `checked_sub`
    // rather than `-`: servers ship with `overflow-checks = false`, where an
    // underflow wraps silently and would hand out a wild zone number.
    let base = mount.layout.first_data_zone.checked_sub(1).ok_or(EIO)?;
    let limit = mount.blocks.saturating_sub(base);

    for i in 0..mount.layout.zmap_blocks {
        let block = mount.layout.zmap_start + i;
        let from = i * bits_per_block;
        if from >= limit {
            break;
        }
        let in_block_limit = (limit - from).min(bits_per_block);
        let buf = blocks.read(u64::from(block))?;
        // Bit 0 of the whole bitmap is reserved: it names a zone below
        // `first_data_zone` and is always marked in use.
        let start = if i == 0 { 1 } else { 0 };
        let Some(bit) = write::bitmap_find_free(buf, start, in_block_limit) else {
            continue;
        };
        let buf = blocks.buf_mut();
        write::bitmap_set(buf, bit).ok_or(EIO)?;
        blocks.write(u64::from(block))?;

        let zone = base
            .checked_add(from)
            .and_then(|z| z.checked_add(bit))
            .ok_or(EIO)?;
        if !walk::zone_ok(zone, mount.blocks) {
            return Err(EIO);
        }
        // W4: zero it before anyone can reach it. A fresh zone otherwise holds
        // whatever the previous owner left -- and for an indirect block, that
        // would be read as zone pointers.
        blocks.zeroed();
        blocks.write(u64::from(zone))?;
        return Ok(zone);
    }
    Err(ENOSPC)
}

/// Store `node` back into the inode table.
///
/// The read-modify-write half of [`read_inode`]: the inode is 64 bytes inside a
/// 4 KiB block, so the block has to be fetched before the slot can be patched.
#[cfg_attr(test, allow(dead_code))]
fn write_inode(blocks: &mut Blocks, mount: &Mount, ino: u32, node: &Inode) -> Result<(), i32> {
    let (block, slot) =
        inode_location(ino, &mount.layout, mount.block_size).ok_or(EINVAL)?;
    blocks.read(u64::from(block))?;
    let buf = blocks.buf_mut();
    let start = slot.checked_mul(INODE_SIZE).ok_or(EIO)?;
    let end = start.checked_add(INODE_SIZE).ok_or(EIO)?;
    let cell = buf.get_mut(start..end).ok_or(EIO)?;
    cell.copy_from_slice(&node.to_le_bytes());
    blocks.write(u64::from(block))
}
```

Add the dispatch arm in `main`'s `match msg.m_type`, after `FS_READ`:

```rust
            FS_WRITE => do_write(&msg, caller_e, &mut blocks, &mount),
```

Imports to add: `FS_WRITE` and `ENOSPC` (from `error`), `SAFECOPY_FROM` (from `server-rt`), `INODE_SIZE` and `SINGLE_INDIRECT_SLOT` (from `minixrs_mfs::inode`), `inode_location` (from `minixrs_mfs::read`), and `write` (the new module). Check the existing import block for the exact paths already in use.

- [ ] **Step 3: Re-point the `BDEV_WRITE` denial probe**

In `bdev_denials` (~line 809), find the probe expecting `EROFS` from a `BDEV_WRITE`. It now *succeeds* and would store the block buffer's contents over a real block. Replace it with a probe the kernel refuses, keeping the count identical:

```rust
    // A `BDEV_WRITE` whose grant carries only `CPF_WRITE`. The driver's
    // geometry checks all pass; what refuses it is the kernel's grant check,
    // which is exactly the guard that stops a client reading a device buffer
    // through a write-shaped request. Before slice 5.10a this probe expected
    // `EROFS` from the driver itself -- when the write became real, that
    // expectation would have become a *successful store*, so the probe had to
    // move to something still denied rather than quietly retire.
    Probe {
        name: "wr-dir",
        m_type: BDEV_WRITE,
        minor: BDEV_MINOR_RAMDISK,
        gid: write_only_gid,
        len: MFS_BLOCK_SIZE as i32,
        block: 1,
        want: EPERM,
    },
```

Match the surrounding `Probe` struct's exact field names and the existing grant-construction idiom — read lines 758-810 before editing. `write_only_gid` is a grant over `BLOCK` with `CPF_WRITE` only; issue it beside the other malformed grants the battery already builds. Keep the battery's total unchanged so its marker string does not move.

- [ ] **Step 4: Build and boot**

Run:
```sh
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings
timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/t4.log 2>&1
tools/check-boot-log.sh /tmp/t4.log
```
Expected: clippy clean, **all markers PASS** including MFS's denial battery. Nothing calls `FS_WRITE` yet.

- [ ] **Step 5: Check the stack frame**

Run the `llvm-objdump` command from Global Constraints against `minixrs-mfs`.
Expected: the largest frame is well under 4096. **If it is above ~3000, stop and report** — `do_write` and `place_zone` must not put a buffer on the stack.

- [ ] **Step 6: Commit**

```bash
git add fs/mfs/src/main.rs fs/mfs/src/proto.rs
git commit -s -m "feat(mfs): serve FS_WRITE with zone allocation (slice 5.10a)"
```

---

### Task 5: VFS — route file writes to MFS

**Files:**
- Modify: `servers/vfs/src/main.rs` (`do_write` ~line 377-421; add `fs_write` beside `fs_read` ~line 888)

**Interfaces:**
- Consumes: Task 1's `FS_WRITE`; existing `rw::{parse, validate, advance, Step}`, `fd::{resolve, advance}`, `Fd::File { ino, pos }`, `GrantPool::grant_magic`.
- Produces: nothing new for later tasks — this closes the path.

- [ ] **Step 1: Add the `FS_WRITE` marshaller**

In `servers/vfs/src/main.rs`, immediately after `fs_read`:

```rust
/// Issue one `FS_WRITE` and return the reply `m_type` — the byte count written,
/// or a negative errno.
///
/// [`fs_read`]'s twin, field for field, because the payload is the same one (W1).
/// No granter goes in the payload — MFS takes it from the kernel-stamped
/// `m_source` — and no grant *offset*: the grant covers exactly this round's
/// bytes, which is why [`write_file`] re-grants each time round.
#[cfg_attr(test, allow(dead_code))]
fn fs_write(mfs: Endpoint, ino: i32, gid: i32, len: i32, pos: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: FS_WRITE,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, FS_INO_OFF, ino);
    wr_i32(&mut m, FS_GRANT_OFF, gid);
    wr_i32(&mut m, FS_LEN_OFF, len);
    wr_u64(&mut m, FS_POS_OFF, pos);
    let trap_rc = ipc_sendrec(mfs, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// Drive `FS_WRITE` until `len` bytes are stored, and report the total.
///
/// The file-backed sibling of [`write_all`], and the same division of labour:
/// every decision about when to stop and what to report lives in [`rw::advance`],
/// which is where its four rules are documented and unit-tested. VFS loops for
/// `write` and does not loop for `read` because POSIX allows a short `read()` and
/// forbids an unexplained short `write()`.
///
/// **A fresh grant per round** (W6), unlike [`write_all`], which grants once and
/// advances a payload `offset`. `CDEV_WRITE` has an offset field and `FS_WRITE`
/// deliberately does not, so the grant is what moves. Each grant is revoked
/// before the next is issued, or a long write would exhaust the pool.
///
/// `len > 0` on entry, so at least one request always goes out and `len - off` is
/// never zero.
#[cfg_attr(test, allow(dead_code))]
fn write_file(
    mfs: Endpoint,
    grants: &mut GrantPool<GRANT_SLOTS>,
    caller_e: Endpoint,
    ino: u32,
    buf: u64,
    len: usize,
    pos: u64,
) -> i32 {
    let mut off = 0usize;
    loop {
        let Some(addr) = buf.checked_add(off as u64) else {
            return if off > 0 { off as i32 } else { EINVAL };
        };
        let gid = match grants.grant_magic(mfs, caller_e, addr, (len - off) as u64, CPF_READ) {
            Ok(gid) => gid,
            Err(e) => return if off > 0 { off as i32 } else { e },
        };
        let n = fs_write(mfs, ino as i32, gid, (len - off) as i32, pos + off as u64);
        let _ = grants.revoke(gid);
        match rw::advance(off, len, n) {
            rw::Step::More(next) => off = next,
            rw::Step::Done(rc) => return rc,
        }
    }
}
```

Add `FS_WRITE` to the `callnr` import list.

- [ ] **Step 2: Route the descriptor**

Replace `do_write`'s `Fd::File` arm. The function needs the MFS endpoint and the grant pool, which it already has, plus the caller — also already a parameter. Change the `match` to bind both variants and dispatch after validation:

```rust
fn do_write(
    caller_e: Endpoint,
    msg: &Message,
    grants: &mut GrantPool<GRANT_SLOTS>,
    tty: Endpoint,
    mfs: Endpoint,
) -> i32 {
    let req = rw::parse(msg);

    let target = match fd::resolve(endpoint_proc(caller_e).get(), req.fd) {
        Ok(Fd::CharDev { minor }) => Fd::CharDev { minor },
        // Slice 5.10a: this arm was `EROFS` from 5.8, defined and refused rather
        // than folded into `Unused` precisely so the write path would land in one
        // place. It now writes.
        Ok(Fd::File { ino, pos }) => Fd::File { ino, pos },
        // `resolve` maps a closed descriptor to `EBADF` itself, so this arm is
        // unreachable. It exists so a future `Fd` variant shows up as a compile
        // error to be routed, not a silent fallthrough.
        Ok(Fd::Unused) => return EBADF,
        Err(e) => return e,
    };

    let len = match rw::validate(req.len, req.buf) {
        Ok(len) => len,
        Err(e) => return e,
    };
    if len == 0 {
        // A legal empty write. No grant is issued, so a client polling with
        // `len = 0` cannot use it to probe the granting path.
        return 0;
    }

    match target {
        Fd::CharDev { minor } => {
            // The single-copy hop: the grant names the *caller's* memory, so the
            // kernel moves the bytes from the caller straight into the driver.
            let gid = match grants.grant_magic(tty, caller_e, req.buf, len as u64, CPF_READ) {
                Ok(gid) => gid,
                Err(e) => return e,
            };
            let written = write_all(tty, minor, gid, len);
            let _ = grants.revoke(gid);
            written
        }
        Fd::File { ino, pos } => {
            let written = write_file(mfs, grants, caller_e, ino, req.buf, len, pos);
            if written > 0 {
                // Only on real progress: advancing on an error would silently
                // move the descriptor past bytes nobody wrote. The `do_read`
                // shape.
                fd::advance(endpoint_proc(caller_e).get(), req.fd, written as u64);
            }
            written
        }
        Fd::Unused => EBADF,
    }
}
```

Update `do_write`'s call site in `main`'s dispatch to pass `mfs`. Remove `EROFS` from the `error` import if nothing else in the file uses it (grep first — `-D warnings` rejects an unused import).

- [ ] **Step 3: Build and boot**

Run:
```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p minixrs-vfs
timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/t5.log 2>&1
tools/check-boot-log.sh /tmp/t5.log
```
Expected: clippy clean, tests pass. **`open.deny` now FAILS** — its `write-file` probe expected `EROFS` and got a byte count. That is the landmine the design predicted; Task 7 retires the probe. Every other marker must still PASS. Confirm with `grep -a 'open.deny' /tmp/t5.log`.

- [ ] **Step 4: Check the stack frame**

Run the `llvm-objdump` command from Global Constraints against `minixrs-vfs`.
Expected: well under 4096.

- [ ] **Step 5: Commit**

```bash
git add servers/vfs/src/main.rs
git commit -s -m "feat(vfs): route VFS_WRITE on a file descriptor to MFS (slice 5.10a)"
```

---

### Task 6: The image gains an empty `/etc/scratch`

**Files:**
- Modify: `kernel/build.rs` (`build_rootfs` ~line 382-399)
- Modify: `tools/mkfs-mfs/src/verify.rs` (tests from ~line 155)
- Test: `tools/mkfs-mfs/src/image.rs` and `verify.rs` inline test modules

**Interfaces:**
- Consumes: Task 1's `ROOTFS_SCRATCH_PATH`. Existing `Manifest::add`, `build_image`, `verify::{lookup, read_file, zmap_bit_set, image_layout}`.
- Produces: a root image containing `/etc/scratch`, regular, size 0, no zones.

- [ ] **Step 1: Write the failing tests**

In `tools/mkfs-mfs/src/verify.rs`'s test module:

```rust
#[test]
fn a_zero_length_file_is_regular_empty_and_costs_no_zones() {
    // Slice 5.10a's write target ships empty, so growth-from-nothing is the
    // ordinary path rather than a special case. Nothing in the image had zero
    // length before, so this really is a new case for the writer.
    let mut m = Manifest::new();
    m.add("/etc/motd", b"hi\n".to_vec()).add("/etc/scratch", Vec::new());
    let img = build_image(&m).expect("image builds");

    let (_, node) = lookup(&img, "/etc/scratch").expect("scratch resolves");
    assert!(node.is_reg());
    assert_eq!(node.size, 0);
    assert_eq!(node.zone, [0u32; NR_TZONES]);
    assert_eq!(read_file(&img, "/etc/scratch"), Some(Vec::new()));
}

#[test]
fn the_image_leaves_room_for_the_scratch_file_to_grow() {
    // The write allocates 8 data zones plus 1 indirect block at runtime. If a
    // future image shrink made that impossible, init's first write would answer
    // ENOSPC at boot rather than failing here.
    let img = sample();
    let l = image_layout(&img).expect("layout");
    let free = (l.first_data_zone..ROOTFS_TAIL_BLOCK)
        .filter(|z| zmap_bit_set(&img, *z) == Some(false))
        .count();
    assert!(
        free >= ROOTFS_SCRATCH_LEN / MFS_BLOCK_SIZE + 1,
        "only {free} free zones"
    );
}
```

Add `ROOTFS_SCRATCH_LEN` and `NR_TZONES` to the test module's imports (`NR_TZONES` comes from `minixrs_mfs::inode`, which the module already imports from).

- [ ] **Step 2: Run to verify**

Run: `cargo test -p minixrs-mkfs-mfs`
Expected: `a_zero_length_file_...` may PASS (the writer may already handle it) or FAIL. Either is informative. `the_image_leaves_room...` should PASS. **If the zero-length test fails, fix `image.rs`** — most likely in `write_data` (an empty `chunks()` iterator leaves the zone array zeroed, which is correct) or in `blocks_for` (`blocks_for(0)` must be 0). Do not add special-casing that is not needed; run and see.

- [ ] **Step 3: Add the file to the boot image**

In `kernel/build.rs`'s `build_rootfs`:

```rust
    use minixrs_kernel_shared::rootfs::{
        ROOTFS_HELLO_PATH, ROOTFS_MOTD, ROOTFS_MOTD_PATH, ROOTFS_PATTERN_LEN, ROOTFS_PATTERN_PATH,
        ROOTFS_SCRATCH_PATH, rootfs_pattern_byte,
    };
```

and, after the `.add(ROOTFS_PATTERN_PATH, pattern)` line:

```rust
        // Slice 5.10a's write target: shipped **empty** on purpose. Create does
        // not exist until 5.10b, so the write path needs a file that is already
        // here — and starting at zero makes growth-from-nothing the ordinary
        // path, and keeps the read proofs (`/etc/motd`, `/etc/pattern`) out of
        // reach of a probe that writes.
        .add(ROOTFS_SCRATCH_PATH, Vec::new());
```

(Move the `;` off the `pattern` line onto this one.)

- [ ] **Step 4: Verify**

Run:
```sh
cargo test -p minixrs-mkfs-mfs
timeout 25 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/t6.log 2>&1
tools/check-boot-log.sh /tmp/t6.log
```
Expected: tests pass; boot unchanged except `open.deny` (still failing from Task 5). In particular `fs.mount ok root=1 bs=4096 blocks=256` and `fs.selfcheck`/`fs.indirect` must still PASS — the new entry must not have shifted any inode or zone the existing proofs depend on.

- [ ] **Step 5: Commit**

```bash
git add kernel/build.rs tools/mkfs-mfs/src/verify.rs tools/mkfs-mfs/src/image.rs
git commit -s -m "feat(mkfs): ship an empty /etc/scratch for the write proof (slice 5.10a)"
```

---

### Task 7: init writes, reads back, and reports

**Files:**
- Modify: `userland/init/src/main.rs` (`main`'s prologue ~line 194-204; `open_denials` ~line 486-543; `OPEN_DENIAL_PROBES` ~line 689; add `write_demo` after `fs_demo` ~line 438)

**Interfaces:**
- Consumes: Task 1's `ROOTFS_SCRATCH_PATH`/`_LEN`/`_PERIOD`/`rootfs_scratch_byte`; the whole path built in Tasks 2-6. Existing `vfs_open`, `vfs_write`, `vfs_read`, `vfs_close`, `append`, `report_line`.
- Produces: the boot markers `minix.rs init: fs.write ok n=32768 v=32768` and `minix.rs init: open.deny ok n=7`.

- [ ] **Step 1: Retire the landmined probe**

In `open_denials`, delete the whole `write-file` block:

```rust
    // ...and a file descriptor cannot be written to, on a read-only filesystem.
    let fd = vfs_open(vfs, ROOTFS_MOTD_PATH);
    if fd < 0 {
        return report_open_fail(vfs, "setup");
    }
    if vfs_write(vfs, fd, ROOTFS_MOTD_PATH.as_bytes()) == EROFS {
        denied += 1;
    } else {
        return report_open_fail(vfs, "write-file");
    }
```

The following `close-twice` block **needs `fd`**, so keep the open that preceded it:

```rust
    // Slice 5.10a retired the probe that used to sit here: it wrote to a
    // descriptor on `/etc/motd` expecting `EROFS`, and once the write path
    // became real that call *succeeded*, overwriting the first bytes of the file
    // `fs.selfcheck` exists to verify. A probe whose expectation silently
    // inverts is worse than no probe -- so it is gone and the count moved, which
    // is what makes the retirement a visible diff in `qemu-boot.expected`
    // rather than a marker that quietly means something else. Writing to a file
    // is now proved positively, by `write_demo`.
    let fd = vfs_open(vfs, ROOTFS_MOTD_PATH);
    if fd < 0 {
        return report_open_fail(vfs, "setup");
    }
    if vfs_close(vfs, fd) == OK && vfs_close(vfs, fd) == EBADF {
```

Change the marker and the constant:

```rust
        let _ = vfs_write(vfs, STDERR, b"minix.rs init: open.deny ok n=7\n");
```

```rust
const OPEN_DENIAL_PROBES: usize = 7;
```

Remove `EROFS` from init's `error` import if nothing else uses it.

- [ ] **Step 2: Add the write demo**

Insert after `fs_demo`:

```rust
// ----- Slice 5.10a: the write path ------------------------------------------

/// Bytes written per `write()` call.
///
/// A whole multiple of [`ROOTFS_SCRATCH_PERIOD`], which is what lets [`SCRATCH`]
/// be a single buffer: every chunk starts at an offset congruent to 0, so the
/// same bytes are correct for all of them.
///
/// Deliberately **not** a multiple of the 4096-byte block. Every chunk after the
/// first therefore starts mid-block and crosses a block boundary, so partial-block
/// splicing and MFS's clamp-to-the-block-end short write are exercised on the
/// boot marker rather than only in unit tests.
const SCRATCH_CHUNK: usize = 16 * ROOTFS_SCRATCH_PERIOD;

const _: () = assert!(SCRATCH_CHUNK % ROOTFS_SCRATCH_PERIOD == 0);
// ...and deliberately NOT a whole block, so every chunk after the first starts
// mid-block. `BDEV_BLOCK_SIZE`, not `CDEV_MAX_IO`: the block is what MFS clamps
// to, and the two constants differ by 16x.
const _: () = assert!(SCRATCH_CHUNK % BDEV_BLOCK_SIZE != 0);

/// One chunk's worth of the scratch pattern, generated at compile time.
///
/// `.rodata`, not a mutable static: init writes these bytes and never modifies
/// them, so the crate keeps zero `unsafe`. A stack buffer is out of the question
/// — init's stack is one page.
static SCRATCH: [u8; SCRATCH_CHUNK] = scratch_chunk();

const fn scratch_chunk() -> [u8; SCRATCH_CHUNK] {
    let mut b = [0u8; SCRATCH_CHUNK];
    let mut i = 0;
    while i < SCRATCH_CHUNK {
        b[i] = rootfs_scratch_byte(i);
        i += 1;
    }
    b
}

/// Bytes read back per verification window.
const SCRATCH_WINDOW: usize = 512;

/// First byte the single-indirect region covers. Mirrors `minixrs-mfs`'s
/// `NR_DIRECT_ZONES * MFS_BLOCK_SIZE`, spelled out locally because init does not
/// depend on that crate — it is a plain user program, `minixrs-ipc` and
/// `kernel-shared` only.
const SEAM: usize = 7 * BDEV_BLOCK_SIZE;

/// Where the read-back windows start.
///
/// All three are multiples of [`SCRATCH_WINDOW`] **and** lie inside a single
/// 4 KiB block, so each window is one clamped transfer and the arithmetic in
/// [`verify_window`] stays trivial. The middle one is the first byte the
/// *indirect* block covers — the one zone the allocator had to create an
/// indirect block for, and the only place that arm can hide. There is no
/// separate count constant: `SCRATCH_WINDOWS_AT.len()` is the count, so the
/// marker's `v=` cannot drift from the loop.
const SCRATCH_WINDOWS_AT: [usize; 3] = [0, SEAM, ROOTFS_SCRATCH_LEN - SCRATCH_WINDOW];

const _: () = assert!(SEAM < ROOTFS_SCRATCH_LEN);
const _: () = assert!(SEAM % SCRATCH_WINDOW == 0);
const _: () = assert!((ROOTFS_SCRATCH_LEN - SCRATCH_WINDOW) % SCRATCH_WINDOW == 0);

/// Exercise the slice-5.10a write path, and report the result over the console.
///
/// Writes [`ROOTFS_SCRATCH_LEN`] bytes to [`ROOTFS_SCRATCH_PATH`], which ships
/// empty, so every zone the file ends up with was allocated at runtime. 32 KiB is
/// eight blocks: seven direct zones and one indirect, so the file crosses the
/// seam and the allocation of the *indirect block itself* is on this marker. That
/// is the same reason `/etc/pattern` has the length it does, one layer up.
///
/// Then it closes, re-opens, and reads back three windows. The re-open matters:
/// it forces a fresh `FS_LOOKUP`, so a size that never reached the inode shows up
/// as a short read rather than being papered over by a descriptor that remembered
/// it. Nothing caches a file size anywhere along this path, which is exactly what
/// makes that test meaningful.
///
/// Three windows rather than all 32 KiB: the returned write count already proves
/// every byte was accepted, the seam is the only place the indirect arm can hide,
/// and ~60 more round trips on a boot this slice already lengthens buy nothing.
///
/// Reported through fd 1 — the path under test — which is the standing rule here:
/// init has no `SYS_DIAGCTL` (it is user-grade), so its proof and its report ride
/// the same machinery, and a broken write cannot print a clean line about itself.
///
/// Best-effort: init's job is to keep the system running, so a failure here is
/// reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn write_demo(vfs: Endpoint) {
    let fd = vfs_open(vfs, ROOTFS_SCRATCH_PATH);
    if fd < 0 {
        return report_line(vfs, b"fs.write FAIL open");
    }

    let mut off = 0usize;
    while off < ROOTFS_SCRATCH_LEN {
        let want = SCRATCH_CHUNK.min(ROOTFS_SCRATCH_LEN - off);
        let n = vfs_write(vfs, fd, &SCRATCH[..want]);
        if n != want as i32 {
            let _ = vfs_close(vfs, fd);
            return report_line(vfs, b"fs.write FAIL short");
        }
        off += want;
    }
    if vfs_close(vfs, fd) != OK {
        return report_line(vfs, b"fs.write FAIL close");
    }

    // Re-open: a fresh descriptor and a fresh lookup, so the size has to have
    // reached the inode for these reads to return anything.
    let fd = vfs_open(vfs, ROOTFS_SCRATCH_PATH);
    if fd < 0 {
        return report_line(vfs, b"fs.write FAIL reopen");
    }
    let mut pos = 0usize;
    let mut verified = 0usize;
    for start in SCRATCH_WINDOWS_AT {
        if !verify_window(vfs, fd, start, &mut pos) {
            let _ = vfs_close(vfs, fd);
            return report_line(vfs, b"fs.write FAIL verify");
        }
        verified += 1;
    }
    let _ = vfs_close(vfs, fd);

    if verified == SCRATCH_WINDOWS_AT.len() {
        let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.write ok n=32768 v=32768\n");
    }
}

/// Read [`SCRATCH_WINDOW`] bytes at `start` and check them against the generator.
///
/// `pos` is the descriptor's position, threaded through because there is no
/// `lseek`: the window is reached by reading forward and discarding, which is
/// also a second exercise of the read path across the seam. The windows are
/// visited in ascending order, so winding forward always suffices.
///
/// Both loops tolerate a short read, because MFS clamps every transfer to the end
/// of a block and a read that spans a boundary legitimately returns less than it
/// was asked for. `n <= 0` is a failure, not a retry: `0` is EOF, which at these
/// offsets means the file is shorter than it should be.
#[cfg_attr(test, allow(dead_code))]
fn verify_window(vfs: Endpoint, fd: i32, start: usize, pos: &mut usize) -> bool {
    let mut buf = [0u8; SCRATCH_WINDOW];

    while *pos < start {
        let want = SCRATCH_WINDOW.min(start - *pos);
        let n = vfs_read(vfs, fd, &mut buf[..want]);
        if n <= 0 {
            return false;
        }
        *pos += n as usize;
    }

    let mut got = 0usize;
    while got < SCRATCH_WINDOW {
        let n = vfs_read(vfs, fd, &mut buf[got..]);
        if n <= 0 {
            return false;
        }
        got += n as usize;
    }
    *pos += SCRATCH_WINDOW;

    let mut i = 0usize;
    while i < SCRATCH_WINDOW {
        if buf[i] != rootfs_scratch_byte(start + i) {
            return false;
        }
        i += 1;
    }
    true
}
```

**Why these three offsets.** `SEAM` is 28672 and the tail window starts at
32256; both are multiples of 512, and 32256 + 512 = 32768 = 8 x 4096, so the tail
window ends exactly at the last block's end. Every window therefore sits inside
one block and comes back in a single clamped transfer.

**One thing to check while writing this:** `report_line`'s existing signature
(~line 703) prefixes `minix.rs init: ` and appends `\n`. Read it and match — if it
does not, adjust the failure strings so every one comes out as
`minix.rs init: fs.write FAIL <reason>`.

- [ ] **Step 3: Call it from the prologue**

In `main`, between `fs_demo(vfs)` and `exec_denials(pm, vfs)`:

```rust
    // The write path runs after the read path and before the exec battery — the
    // prologue's standing rule, newest code last, so a hang inside it localizes
    // to the `fs.write` marker instead of taking 5.4's, 5.5's, 5.6's and 5.8's
    // with it. Do not reorder this prologue.
    write_demo(vfs);
```

Update `fs_demo`'s trailing comment and `exec_denials`' "runs last in the prologue" comment so they still describe the real order.

- [ ] **Step 4: Boot and verify**

Run:
```sh
cargo clippy --workspace --all-targets -- -D warnings
timeout 45 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/t7.log 2>&1
grep -a 'fs.write\|open.deny' /tmp/t7.log
```
Expected: `minix.rs init: fs.write ok n=32768 v=32768` and `minix.rs init: open.deny ok n=7`, each exactly once. A `FAIL` spelling names which step broke. **45 s, not 25** — the write demo lengthens the prologue.

- [ ] **Step 5: Commit**

```bash
git add userland/init/src/main.rs
git commit -s -m "feat(init): write, read back, and prove /etc/scratch (slice 5.10a)"
```

---

### Task 8: Markers, mutation tests, boot matrix, docs

**Files:**
- Modify: `tests/qemu-boot.expected`, `tests/qemu-boot.forbidden`
- Modify: `.github/workflows/ci.yml` (only if the budget measurement says so)
- Modify: `CLAUDE.md` (append a slice-5.10a convention bullet after the 5.9 one)

**Interfaces:**
- Consumes: everything above.
- Produces: a green `tools/check-boot-log.sh` in all four boot configurations.

- [ ] **Step 1: Update the marker files**

In `tests/qemu-boot.expected`, change:

```
minix.rs init: open.deny ok n=8
```
to
```
minix.rs init: open.deny ok n=7
```

and add, near the other `fs.*` init markers (~line 493), with a comment in the file's established style:

```
# Slice 5.10a: the write path, end to end. init writes 32 KiB to /etc/scratch --
# which ships empty, so every zone was allocated at runtime -- then re-opens and
# reads back three 512-byte windows, one of them the first indirect block. `n=`
# is the total the write path reported and `v=` the windows that matched: the
# two fail independently, since a write that moved every byte and mis-reported
# the count would still verify, and a count that was right about bytes that never
# reached the inode would still read back short.
minix.rs init: fs.write ok n=32768 v=32768
```

In `tests/qemu-boot.forbidden`, add:

```
# Slice 5.10a: the write path ran and disagreed with itself. Distinct from the
# marker simply going missing (init never got that far): this spelling means the
# write, the re-open, or the read-back executed and produced the wrong answer.
minix.rs init: fs.write FAIL
```

- [ ] **Step 2: Measure the boot-time cost**

The write demo adds roughly 130 device round trips. Measure before deciding anything:

```sh
# With the work in the tree, on the musl flavour CI actually builds:
MINIXRS_SDK=/nonexistent timeout 45 cargo run -p minixrs-kernel \
  --target aarch64-unknown-none --release > /tmp/after.log 2>&1
grep -abo 'hello: iov ok' /tmp/after.log | head -1   # last C marker's byte offset
wc -c /tmp/after.log

git stash
MINIXRS_SDK=/nonexistent timeout 45 cargo run -p minixrs-kernel \
  --target aarch64-unknown-none --release > /tmp/before.log 2>&1
grep -abo 'hello: iov ok' /tmp/before.log | head -1
wc -c /tmp/before.log
git stash pop
```

Compute each marker's byte offset ÷ total bytes. If the fraction moved materially (say past ~0.85 of the log), raise `qemu-smoke`'s timeout in `.github/workflows/ci.yml` **with real headroom** — CI's TCG is slower than this machine, and 5.9 went 45 s → 120 s for exactly this reason. Never trim the budget to whatever passes locally.

- [ ] **Step 3: Run the four-row boot matrix**

```sh
for cfg in "" "MINIXRS_SDK=/nonexistent"; do
  env $cfg timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
    > /tmp/m.log 2>&1
  echo "=== $cfg ==="; tools/check-boot-log.sh /tmp/m.log
done

timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release \
  --no-default-features > /tmp/m3.log 2>&1
grep -a 'fs.write\|open.deny' /tmp/m3.log

mv target/musl-sysroot /tmp/musl-sysroot-aside
timeout 120 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > /tmp/m4.log 2>&1
grep -a 'fs.write\|open.deny\|stack FAIL' /tmp/m4.log
mv /tmp/musl-sysroot-aside target/musl-sysroot
```

Expected: rows 1, 2 fully green. Row 3 (`--no-default-features`) has no stubs, so the stub markers the expected file requires are legitimately missing — everything else, including `fs.write ok n=32768 v=32768`, must be present and nothing forbidden. Row 4 (`/bin/hello` is the `worker` ELF) must show `fs.write ok`, no `stack FAIL`, and no hang. **Record all four results; they go in the PR body.**

- [ ] **Step 4: Mutation tests**

Snapshot **first** — `git checkout` errors on an added file rather than restoring it:

```sh
mkdir -p /tmp/claude-501/.../scratchpad/mut
cp fs/mfs/src/write.rs fs/mfs/src/main.rs drivers/memory/src/main.rs \
   userland/init/src/main.rs /path/to/scratchpad/mut/
```

Apply each mutation, boot, record which marker moved, restore from the scratchpad. **Before recording any observation, check the log actually built:** `grep -a 'error\[E' /tmp/mut.log`, or confirm unrelated markers still PASS — a mutation that fails to compile leaves a log with no kernel output at all and every marker MISSING, which is indistinguishable from a mutation that worked.

| # | Mutation | Predicted |
|---|---|---|
| 1 | in `alloc_zone`, delete the `write::bitmap_set(buf, bit)` call | every allocation returns the same zone → `fs.write FAIL verify` |
| 2 | in `do_write`, change step 5's condition to `if grown != node.size` only (drop `dirty`) | the size grows but a hole's pointer is lost → `fs.write FAIL verify` |
| 3 | in `alloc_zone`, delete the `blocks.zeroed()` before the store | a fresh indirect block holds garbage pointers → `fs.write FAIL verify` or `EIO` |
| 4 | in `drivers/memory`'s `do_write`, use `SAFECOPY_TO` instead of `SAFECOPY_FROM` | the buffer is overwritten instead of stored → `fs.write FAIL verify` |
| 5 | in MFS's re-pointed denial probe, grant `CPF_READ` instead of `CPF_WRITE` | MFS's `bdev.deny` marker |

Finish with:
```sh
grep -rn MUTATION . --include=*.rs
git status --short
```
Both must be clean. **That sweep, not the restore command's exit status, is what proves the tree clean.**

- [ ] **Step 5: Update `CLAUDE.md`**

Append a bullet after the slice-5.9 one, in the same voice and density — the load-bearing facts a future session must not re-derive:

- `FS_WRITE = FS_RQ_BASE + 3` reuses `FS_READ`'s payload verbatim (W1); a short `FS_WRITE` is normal (VFS loops), unlike `BDEV`'s refuse-or-nothing, and the two rules differ because BDEV's client cannot interpret half a block while VFS's job is hiding staging from POSIX.
- The single block buffer is what fixes `do_write`'s step order; `Blocks` gained `buf_mut`/`write` and its grant widened to `CPF_READ | CPF_WRITE` (one buffer, both directions, one static address).
- A zone's bitmap bit is set **before** its number is stored, so a mid-write failure leaks a zone rather than sharing one — and the inode write-back is keyed on "a zone was assigned **or** the size grew", because a hole filled mid-file moves no size and losing its pointer is corruption rather than a leak.
- VFS re-grants per round because the FS band has no grant-offset field, unlike the CDEV loop which advances a payload `offset`.
- The landmine: `open_denials`' `write-file` probe expected `EROFS` and became a *successful overwrite of `/etc/motd`*. Retired, count moved 8 → 7 so the change is a visible diff. Second occurrence of the 5.8 `VFS_WRITE + 1` lesson — **write every denial probe so that a growing capability makes it fail loudly, not pass vacuously.**

- [ ] **Step 6: Final gate sweep**

```sh
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings
cargo clippy -p minixrs-kernel --target aarch64-unknown-none --no-default-features -- -D warnings
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo test -p minixrs-kernel-shared -p minixrs-mfs -p minixrs-vfs -p minixrs-memory -p minixrs-mkfs-mfs -p minixrs-gen-c-headers
tools/check-dco.sh
```
Expected: all clean, DCO green for every commit on the branch.

- [ ] **Step 7: Commit**

```bash
git add tests/qemu-boot.expected tests/qemu-boot.forbidden CLAUDE.md docs/
git commit -s -m "test: boot markers and conventions for the MFS write path (slice 5.10a)"
```

---

## Notes for the executor

- **Do not create the PR without asking.** `CLAUDE.md`'s pre-PR checklist requires running `/claude-md-management:revise-claude-md` and confirming with Kevin first.
- **The spec is the argument; this plan is the sequence.** If an implementation detail here contradicts `docs/superpowers/specs/2026-08-18-mfs-write-path-design.md`, the spec wins — say so rather than silently following either.
- **Boot green at every task boundary except two, both known.** Tasks 1, 3 and 6 leave every marker passing.
  - Task 2 makes `BDEV_WRITE` real, which breaks MFS's `write` denial probe (`fs/mfs/src/main.rs:877`): it expects `EROFS` and will now get `EPERM`, because the probe's grant is `blocks.gid`, still `CPF_WRITE`-only, and `SAFECOPY_FROM` needs `CPF_READ`. So `[diag mfs] bdev.deny ok n=10` goes missing from Task 2 until **Task 4** re-points the probe. Do not fix it in Task 2 or 3 — the fix needs the dedicated write-only grant Task 4 introduces.
  - Task 5 knowingly breaks `open.deny`, and Task 7 fixes it.
  - **The Task 4 hazard this creates.** That probe names `block: 0` — the image header and the superblock — and `gid: good`, which *is* `blocks.gid`. Task 4 Step 1 widens `blocks.gid` to `CPF_READ | CPF_WRITE`. If Step 1 lands without Step 3, the probe stops being refused and writes 32 bytes of MFS's block buffer over block 0, destroying the superblock. Steps 1 and 3 must land together, and Task 4's re-pointed probe must use its own `CPF_WRITE`-only grant rather than `good`, or widening the shared grant re-arms it.
  - If any *other* marker moves, stop and find out why before continuing.

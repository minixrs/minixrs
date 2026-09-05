# Slice 5.11 — `/dev/null`, `/dev/zero`, `CDEV_READ` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two hardware-free character devices, served by the `memory` driver and reachable by path through VFS, with the CDEV band gaining the read request that `/dev/zero` needs.

**Architecture:** `kernel-shared` defines `CDEV_READ` and two minors; `server-rt` gains the shared CDEV request codec; the `memory` driver serves both minors for both requests; VFS's `Fd::CharDev` learns which driver it names, and a static device-node table intercepts `/dev/console`, `/dev/null`, `/dev/zero` after the path copy and before the mount. init proves all three devices through the POSIX path and the marker files pin the proof.

**Tech Stack:** Rust (pinned nightly in `rust-toolchain.toml`), `no_std` user-space crates, host unit tests via `cargo test -p <crate>`, QEMU boot verification via `tools/check-boot-log.sh`.

**Spec:** `docs/superpowers/specs/2026-09-05-dev-null-zero-design.md` — decisions `Z1…Z10` are cited by number below. Read it first.

## Global Constraints

- Every new `.rs` file starts with the two-line SPDX + copyright header (`// SPDX-License-Identifier: BSD-3-Clause` / `// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors`).
- Offset/length arithmetic in `server-rt`, `servers/`, `drivers/`, `userland/` uses `checked_add` / `saturating_*`, never bare `+` on a payload offset (release ships `overflow-checks = false`).
- `server-rt` stays `#![forbid(unsafe_code)]`; `kernel-shared` carries zero `unsafe`; `drivers/memory` gains no `unsafe` block.
- No new payload field may carry a granter. Every driver takes the granter from the kernel-stamped `m_source`.
- Every commit: `git commit -s` (DCO sign-off) and GPG-signed (default). Never `--no-verify`, never `--no-gpg-sign`. Commit messages end with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session:` trailers the session header prescribes.
- Before each commit: `cargo fmt --all` then `cargo clippy --workspace --all-targets -- -D warnings` on the touched crates must be clean (the pinned nightly's `-D warnings` gate).
- Boot verification runs in the stub-free config for iteration (`--no-default-features`, `timeout 60`) and in the default config for the checked-in verdict (`timeout 300`). Copy the log aside before `tools/check-boot-log.sh`.
- `MINIXRS_SDK=/nonexistent` on every boot that is meant to match CI (the musl flavour). It does not persist across shell invocations — set it on the same command line as the `cargo run`.
- Never push, never open a PR. Stop at the end of Task 9 and surface the branch.
- Working branch: `feature/slice-5.11-dev-null-zero` (already created, carries the spec).

## File map

| File | Responsibility | Task |
|---|---|---|
| `kernel-shared/src/callnr.rs` | `CDEV_READ`, `NR_CDEV_MSGS`, the two minors, the three device paths, doc rewrites, tripwire tests | 1 |
| `tools/gen-c-headers/src/callnr_h.rs` | `CDEV_READ` row, minor defines, define-list test | 1 |
| `server-rt/src/cdev.rs` (new), `server-rt/src/lib.rs` | shared CDEV request codec (Z9) | 2 |
| `drivers/tty/src/cdev.rs`, `drivers/tty/src/main.rs` | use the shared codec; comments | 2 |
| `drivers/memory/src/cdev.rs` (new), `drivers/memory/src/main.rs` | minor classification, validation, the two arms (Z3, Z4) | 3 |
| `servers/vfs/src/fd.rs` | `CharDriver`, `Fd::CharDev { dev, minor }` (Z5) | 4 |
| `servers/vfs/src/dev.rs` (new) | the device-node table and `lookup` (Z6) | 4 |
| `servers/vfs/src/main.rs` | `mem` endpoint, `do_open` reorder, `do_read`/`do_write` routing, `cdev_read`, `mem_denials` (Z6, Z7, Z8) | 5 |
| `userland/init/src/main.rs` | `dev_demo` (Z10), `dev-no-such`, `read-console` comment | 6 |
| `tests/qemu-boot.expected`, `tests/qemu-boot.forbidden` | markers | 7 |
| `book/src/drivers/overview.md`, `book/src/servers/overview.md`, `book/src/reference/syscalls.md`, `docs/plan.md`, `docs/plans/phase-5-musl-fs.md`, `CLAUDE.md` | docs, trackers, falsified-claim sweep | 8 |

---

### Task 1: The ABI — `CDEV_READ`, the minors, the paths, the headers

**Files:**
- Modify: `kernel-shared/src/callnr.rs:1064-1137` (CDEV band), plus its `#[cfg(test)]` module (`cdev_msgs_contiguous_from_base`, `cdev_msgs_distinct_from_other_ranges`, `cdev_max_io_fits_the_reply`, and every other band's "distinct from" list that names `CDEV_WRITE`)
- Modify: `kernel-shared/src/callnr.rs:716` (the FS-band "`CDEV_READ` precedent" sentence)
- Modify: `tools/gen-c-headers/src/callnr_h.rs:136-142` (band), `:345-361` (defines), `:540-551` (define-list test)
- Test: the two crates' existing `#[cfg(test)]` modules

**Interfaces:**
- Produces: `callnr::CDEV_READ: i32 = 0xB01`, `callnr::NR_CDEV_MSGS: usize = 2`, `callnr::CDEV_MINOR_NULL: i32 = 3`, `callnr::CDEV_MINOR_ZERO: i32 = 5`, `callnr::DEV_CONSOLE_PATH: &str = "/dev/console"`, `callnr::DEV_NULL_PATH: &str = "/dev/null"`, `callnr::DEV_ZERO_PATH: &str = "/dev/zero"`.

- [ ] **Step 1: Grow the band tripwires so they fail**

In `kernel-shared/src/callnr.rs`'s test module, change `cdev_msgs_contiguous_from_base` and `cdev_msgs_distinct_from_other_ranges`:

```rust
    #[test]
    fn cdev_msgs_contiguous_from_base() {
        // CDEV requests are contiguous from CDEV_RQ_BASE; NR_CDEV_MSGS locks a
        // character driver's dispatch coverage. Slice 5.11 grew this from one
        // request to two.
        let msgs = [CDEV_WRITE, CDEV_READ];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, CDEV_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_CDEV_MSGS);
    }

    #[test]
    fn cdev_msgs_distinct_from_other_ranges() {
        // Each CDEV request must stay distinct from every other band and the
        // KERNEL_CALL range, and below NOTIFY_MESSAGE — so a driver's m_type
        // dispatcher and the SEF classifier never collide.
        for m in [CDEV_WRITE, CDEV_READ] {
```

(leave the rest of that test's body as it is). Then find every *other* test array in the same module that lists `CDEV_WRITE` as a foreign request and add `CDEV_READ` beside it:

```bash
grep -n 'CDEV_WRITE,' kernel-shared/src/callnr.rs
```

Each hit inside a `for other in [` / `for m in [` list gets `CDEV_READ,` on the next line. Then replace the `cdev_max_io_fits_the_reply` test's last assertion and add a minor test:

```rust
    #[test]
    fn cdev_max_io_fits_the_reply() {
        // The reply `m_type` carries the byte count as an i32, so a full-size
        // transfer must round-trip through i32 — a count that overflowed would
        // land in the negative, errno-shaped band and read as a failure.
        assert_eq!(i32::try_from(CDEV_MAX_IO), Ok(256));
        assert_eq!(CDEV_MAX_IO, 256);
    }

    #[test]
    fn cdev_minors_take_minix3s_values_and_are_distinct() {
        // TTY's console is 0. The memory driver's two character minors take
        // MINIX 3's `NULL_DEV` / `ZERO_DEV` values from `include/minix/dmap.h`
        // (slice 5.11, Z3). Minors are a per-driver namespace, so only the
        // memory driver's own pair has to be distinct — nothing here compares
        // them with `BDEV_MINOR_RAMDISK`, and a collision there would be fine.
        assert_eq!(CDEV_MINOR_CONSOLE, 0);
        assert_eq!(CDEV_MINOR_NULL, 3);
        assert_eq!(CDEV_MINOR_ZERO, 5);
        assert_ne!(CDEV_MINOR_NULL, CDEV_MINOR_ZERO);
    }

    #[test]
    fn device_node_paths_are_absolute_and_distinct() {
        // VFS's device-node table matches these byte-for-byte and init opens
        // them by the same constants, which is why they live here rather than
        // in either crate.
        let paths = [DEV_CONSOLE_PATH, DEV_NULL_PATH, DEV_ZERO_PATH];
        for p in paths {
            assert!(p.starts_with("/dev/"), "{p}");
            assert!(p.len() < FS_PATH_MAX, "{p} would be ENAMETOOLONG");
        }
        assert_ne!(DEV_NULL_PATH, DEV_ZERO_PATH);
        assert_ne!(DEV_CONSOLE_PATH, DEV_NULL_PATH);
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p minixrs-kernel-shared cdev 2>&1 | tail -20`
Expected: compile errors naming `CDEV_READ`, `CDEV_MINOR_NULL`, `CDEV_MINOR_ZERO`, `DEV_*_PATH` as unresolved.

- [ ] **Step 3: Define the constants and rewrite the band docs**

In `kernel-shared/src/callnr.rs`, replace the `CDEV_WRITE` doc paragraph that begins "There is deliberately no `CDEV_READ`" and the `NR_CDEV_MSGS` line, and the `CDEV_MINOR_CONSOLE` doc, with:

```rust
/// … (keep the existing `CDEV_WRITE` doc up to and including the "stage through a
/// small stack buffer with no allocator." sentence, then:)
///
/// [`CDEV_READ`] is the same payload with the copy running the other way.
pub const CDEV_WRITE: i32 = CDEV_RQ_BASE;

/// Client → character driver: read bytes from a device minor (slice 5.11).
///
/// Payload: [`CDEV_WRITE`]'s, field for field — minor, grant id, byte count, and
/// the offset within the granted range. The grant must carry `CPF_WRITE` and name
/// the driver as its grantee; the driver fills the bytes with
/// `SYS_SAFECOPY(SAFECOPY_TO, m_source, …)`. There is no granter field and there
/// must never be one — the rule the whole band lives by.
///
/// Reply `m_type` is the **number of bytes read** (`>= 0`), or a negative errno.
/// **`0` is EOF, and a short read is legal.** That is POSIX `read()`'s contract
/// and the one VFS already assumes for `FS_READ`: VFS sends one request and
/// reports the count, with no retry loop. `/dev/null` answers `0` on every read;
/// `/dev/zero` never answers `0` for a positive count.
///
/// This request existed only as a plan note until 5.11 — the 5.3 text said
/// `/dev/null` and `/dev/zero` would be "new minors, not new requests", which is
/// true of null and of *writing* zero and false of *reading* it. TTY does not
/// serve it until Phase 6 gives it RX (`SYS_IRQCTL`), and answers it `ENOSYS`
/// from its unknown-request arm until then.
pub const CDEV_READ: i32 = CDEV_RQ_BASE + 1;

/// Number of character-device requests defined so far. Locks a driver's
/// dispatch coverage the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_CDEV_MSGS: usize = 2;
```

and, in place of the existing `CDEV_MINOR_CONSOLE` doc + const:

```rust
/// The console minor: TTY's UART, and TTY's only minor — any other is `ENXIO`
/// there.
///
/// **Minors are a per-driver namespace.** This is TTY's 0; the memory driver's
/// ramdisk is `BDEV_MINOR_RAMDISK` 0; [`CDEV_MINOR_NULL`] and [`CDEV_MINOR_ZERO`]
/// are CDEV minors *of the memory driver*. The request band, never the minor
/// value, is what tells the ramdisk and the character minors apart on that
/// driver — so nothing asserts `CDEV_MINOR_*` against `BDEV_MINOR_*`, and a
/// numeric collision would be fine.
pub const CDEV_MINOR_CONSOLE: i32 = 0;

/// `/dev/null`, served by the memory driver (slice 5.11). MINIX 3's `NULL_DEV`
/// from `include/minix/dmap.h`: every read answers `0`, every write discards.
pub const CDEV_MINOR_NULL: i32 = 3;

/// `/dev/zero`, served by the memory driver (slice 5.11). MINIX 3's `ZERO_DEV`:
/// a read fills the whole request with zeroes, a write discards.
pub const CDEV_MINOR_ZERO: i32 = 5;

/// The paths VFS's device-node table answers with these minors (slice 5.11).
///
/// Here rather than in VFS so init's probes and VFS's table cannot drift: VFS
/// matches these byte-for-byte ahead of the FS lookup (there is no `/dev` on
/// the image and no device inode — the deliberate simplification D11 names), and
/// init opens them by the same constants. **Not** emitted in the generated C
/// headers: a C program spells `"/dev/null"` itself.
pub const DEV_CONSOLE_PATH: &str = "/dev/console";
/// See [`DEV_CONSOLE_PATH`].
pub const DEV_NULL_PATH: &str = "/dev/null";
/// See [`DEV_CONSOLE_PATH`].
pub const DEV_ZERO_PATH: &str = "/dev/zero";
```

Also at `callnr.rs:716`, reword the FS-band sentence so it no longer claims `CDEV_READ` is absent. Replace the clause "on the `CDEV_READ` precedent that a request absent until it has a consumer is better absent than stubbed" with "on the precedent `CDEV_READ` set from 5.3 to 5.11: a request without a consumer is better absent than stubbed, and gets defined the moment one exists".

- [ ] **Step 4: Run the kernel-shared tests**

Run: `cargo test -p minixrs-kernel-shared 2>&1 | tail -5`
Expected: `test result: ok.` with the new tests listed as passed.

- [ ] **Step 5: Grow the header generator's tripwire, watch it fail, then add the row**

In `tools/gen-c-headers/src/callnr_h.rs`, in the define-list test (around line 540) change the array to 13 entries:

```rust
        let offsets: [(&str, i64); 13] = [
            ("VFS_FD_OFF", callnr::VFS_FD_OFF as i64),
            ("VFS_LEN_OFF", callnr::VFS_LEN_OFF as i64),
            ("VFS_BUF_OFF", callnr::VFS_BUF_OFF as i64),
            ("PM_EXEC_PATH_OFF", callnr::PM_EXEC_PATH_OFF as i64),
            ("PM_EXEC_PATH_MAX", callnr::PM_EXEC_PATH_MAX as i64),
            ("CDEV_MINOR_OFF", callnr::CDEV_MINOR_OFF as i64),
            ("CDEV_GRANT_OFF", callnr::CDEV_GRANT_OFF as i64),
            ("CDEV_LEN_OFF", callnr::CDEV_LEN_OFF as i64),
            ("CDEV_OFFSET_OFF", callnr::CDEV_OFFSET_OFF as i64),
            ("CDEV_MAX_IO", callnr::CDEV_MAX_IO as i64),
            ("CDEV_MINOR_CONSOLE", callnr::CDEV_MINOR_CONSOLE as i64),
            ("CDEV_MINOR_NULL", callnr::CDEV_MINOR_NULL as i64),
            ("CDEV_MINOR_ZERO", callnr::CDEV_MINOR_ZERO as i64),
        ];
```

Run: `cargo test -p minixrs-gen-c-headers 2>&1 | grep -E 'FAILED|panicked|the character-device' | head`
Expected: two failures — `every_band_member_list_matches_its_count` ("the character-device requests band lists 1 members but NR_CDEV_MSGS is 2") and the define-list test (`CDEV_MINOR_NULL` not emitted).

Then in `bands()`:

```rust
        Band {
            title: "character-device requests",
            base_name: "CDEV_RQ_BASE",
            base: callnr::CDEV_RQ_BASE,
            count: Some(("NR_CDEV_MSGS", callnr::NR_CDEV_MSGS)),
            members: vec![
                ("CDEV_WRITE", callnr::CDEV_WRITE),
                ("CDEV_READ", callnr::CDEV_READ),
            ],
        },
```

and after the `CDEV_MINOR_CONSOLE` define:

```rust
    f.define_dec("CDEV_MINOR_CONSOLE", callnr::CDEV_MINOR_CONSOLE.into());
    f.define_dec("CDEV_MINOR_NULL", callnr::CDEV_MINOR_NULL.into());
    f.define_dec("CDEV_MINOR_ZERO", callnr::CDEV_MINOR_ZERO.into());
```

and extend the block comment above the offsets (the one ending "musl's write() goes to VFS.") with two more lines:

```rust
        "",
        "CDEV_READ (slice 5.11) is the same payload with the copy running the other",
        "way: the grant carries CPF_WRITE and the driver fills the client's buffer.",
        "CDEV_MINOR_NULL / CDEV_MINOR_ZERO are minors of the memory driver, not TTY:",
        "minors are a per-driver namespace.",
```

- [ ] **Step 6: Run the generator's tests and the hermetic C check**

Run:
```bash
cargo test -p minixrs-gen-c-headers 2>&1 | tail -3
cargo gen-c-headers
clang -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsyntax-only \
  -ffreestanding -nostdlibinc --target=aarch64-unknown-linux-musl \
  -Itarget/gen-c-headers/include target/gen-c-headers/abi-selftest.c && echo C-OK
grep -n 'CDEV_READ\|CDEV_MINOR_NULL\|CDEV_MINOR_ZERO' target/gen-c-headers/include/minixrs/callnr.h
```
Expected: `test result: ok.`, `C-OK`, and three grep hits.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p minixrs-kernel-shared -p minixrs-gen-c-headers --all-targets -- -D warnings
git add kernel-shared/src/callnr.rs tools/gen-c-headers/src/callnr_h.rs
git commit -s -m "abi(5.11): CDEV_READ, the null/zero minors, and the device-node paths

CDEV_READ = CDEV_RQ_BASE + 1 (NR_CDEV_MSGS 1 -> 2), CDEV_WRITE's payload
with the copy running the other way. CDEV_MINOR_NULL = 3 and
CDEV_MINOR_ZERO = 5 take MINIX 3's NULL_DEV/ZERO_DEV values; minors are a
per-driver namespace. DEV_*_PATH are the strings VFS's table matches and
init opens. The 5.3 claim that 5.11 would be minors only is corrected in
the band docs; the header generator gains the row and the defines.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 2: The shared CDEV codec in `server-rt`, and TTY on top of it

**Files:**
- Create: `server-rt/src/cdev.rs`
- Modify: `server-rt/src/lib.rs` (add `pub mod cdev;`)
- Modify: `drivers/tty/src/cdev.rs` (drop `WriteRequest`/`parse_write`, take `Request`)
- Modify: `drivers/tty/src/main.rs:154-155` (`do_write`'s parse call), module-doc paragraph
- Test: `server-rt/src/cdev.rs` (new tests), `drivers/tty/src/cdev.rs` (existing tests, retargeted)

**Interfaces:**
- Consumes: `callnr::CDEV_{MINOR,GRANT,LEN,OFFSET}_OFF` (unchanged), Task 1's docs.
- Produces: `minixrs_server_rt::cdev::Request { minor: i32, gid: i32, len: i32, offset: u64 }` (`Copy + PartialEq + Debug`), `minixrs_server_rt::cdev::parse(&Message) -> Request`. TTY's `cdev::validate_write(Request) -> Result<usize, i32>` keeps its name and contract.

- [ ] **Step 1: Write the codec module with its tests**

Create `server-rt/src/cdev.rs`:

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The CDEV request codec — the four payload fields `CDEV_WRITE` and `CDEV_READ`
//! share (slice 5.11, lifted out of `drivers/tty`).
//!
//! Two drivers decode this payload now: TTY for the console, `memory` for
//! `/dev/null` and `/dev/zero`. So the parse lives here, the way slice 5.3
//! lifted `rd_i32` and friends the moment a second server needed them.
//! **Validation stays in each driver.** Which minors exist and whether a request
//! is clamped are driver facts, not band facts — TTY clamps to `CDEV_MAX_IO`
//! because it stages through a stack buffer, the memory driver clamps nothing
//! because it stages nothing.
//!
//! **There is no granter field, and no way to express one.** A driver takes the
//! granter from the kernel-stamped `m_source`; a payload field would make every
//! grant-holding driver a confused deputy, aiming a privileged cross-address-space
//! copy wherever its client pointed.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MINOR_OFF, CDEV_OFFSET_OFF};

use crate::payload::{rd_i32, rd_u64};

/// A parsed `CDEV_WRITE` or `CDEV_READ` request. Field-for-field the payload,
/// with no interpretation applied — the driver's validator does that.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// Device minor. A per-driver namespace: `CDEV_MINOR_CONSOLE` on TTY,
    /// `CDEV_MINOR_NULL` / `CDEV_MINOR_ZERO` on the memory driver.
    pub minor: i32,
    /// Grant id naming the client's buffer. The access bit it must carry is the
    /// one the *direction* needs — `CPF_READ` for a write, `CPF_WRITE` for a read
    /// — checked by the kernel's `verify_grant`, never re-derived by a driver.
    pub gid: i32,
    /// Bytes the client asked for. May be negative on the wire; the validator
    /// rejects that before it can widen into a huge `u64`.
    pub len: i32,
    /// Offset within the granted range to start at. Advanced by the client across
    /// a short-write loop; range-checked against the grant by the kernel, so it
    /// passes through here unvalidated.
    pub offset: u64,
}

/// Read a CDEV request out of a message payload.
///
/// Total: every field is a fixed-offset scalar read that cannot fail (the payload
/// accessors return `0` for an out-of-range offset), so a malformed request
/// becomes an invalid *value* the driver's validator rejects, never a panic.
pub fn parse(msg: &Message) -> Request {
    Request {
        minor: rd_i32(msg, CDEV_MINOR_OFF),
        gid: rd_i32(msg, CDEV_GRANT_OFF),
        len: rd_i32(msg, CDEV_LEN_OFF),
        offset: rd_u64(msg, CDEV_OFFSET_OFF),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{wr_i32, wr_u64};

    /// Build a well-formed CDEV payload. The granter is deliberately not a
    /// parameter — there is no field for it.
    fn request(minor: i32, gid: i32, len: i32, offset: u64) -> Message {
        let mut m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        wr_i32(&mut m, CDEV_MINOR_OFF, minor);
        wr_i32(&mut m, CDEV_GRANT_OFF, gid);
        wr_i32(&mut m, CDEV_LEN_OFF, len);
        wr_u64(&mut m, CDEV_OFFSET_OFF, offset);
        m
    }

    #[test]
    fn parse_reads_every_field_from_its_own_offset() {
        // Four distinct values, so a swapped pair of offsets would fail.
        let m = request(5, 0x0030_0001, 64, 4096);
        assert_eq!(
            parse(&m),
            Request {
                minor: 5,
                gid: 0x0030_0001,
                len: 64,
                offset: 4096,
            }
        );
    }

    #[test]
    fn parse_of_a_zeroed_payload_is_all_zeroes_and_does_not_panic() {
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        assert_eq!(
            parse(&m),
            Request {
                minor: 0,
                gid: 0,
                len: 0,
                offset: 0,
            }
        );
    }

    #[test]
    fn the_offset_is_passed_through_unvalidated() {
        // Range-checking it is the kernel's job (`verify_grant` tests
        // `offset + bytes <= grant.len`); clamping it here would break the
        // short-write loop a client drives with it.
        for offset in [0u64, 1, 256, u64::MAX] {
            assert_eq!(parse(&request(0, 1, 16, offset)).offset, offset);
        }
    }
}
```

In `server-rt/src/lib.rs`, after `mod classify;` add `pub mod cdev;` (public module, no re-export: callers write `minixrs_server_rt::cdev::parse`).

- [ ] **Step 2: Run server-rt's tests**

Run: `cargo test -p minixrs-server-rt cdev 2>&1 | tail -5`
Expected: 3 tests pass.

- [ ] **Step 3: Retarget TTY onto the codec**

In `drivers/tty/src/cdev.rs`:

1. Replace the module doc's first paragraph with:
   ```rust
   //! `CDEV_WRITE` validation — the driver's pure logic.
   //!
   //! The four-field parse moved to `server-rt::cdev` in slice 5.11, when the
   //! memory driver became the second decoder of this payload. What stays here is
   //! everything TTY-specific: which minor exists (only the console), and the
   //! clamp to [`CDEV_MAX_IO`] that a stack staging buffer forces.
   ```
   and keep the two "careful about" paragraphs.
2. Delete the `WriteRequest` struct and `parse_write` fn. Replace the imports with:
   ```rust
   use minixrs_kernel_shared::callnr::{CDEV_MAX_IO, CDEV_MINOR_CONSOLE};
   use minixrs_kernel_shared::error::{EINVAL, ENXIO};
   use minixrs_kernel_shared::grant::grant_valid;
   use minixrs_server_rt::cdev::Request;
   ```
3. `pub fn validate_write(req: Request) -> Result<usize, i32>` — body unchanged. In its doc, replace bullet 1's second sentence ("Slice 5.11's `/dev/null` and `/dev/zero` become additional minors here.") with: "`/dev/null` and `/dev/zero` are minors of the *memory* driver, not of TTY, so this check stays an equality."
4. In the tests: `use minixrs_server_rt::cdev::parse;` plus the `wr_*` imports it already has; replace every `parse_write(` with `parse(` and every `WriteRequest` with `Request`. Delete `parse_reads_every_field_from_its_own_offset` and `the_offset_is_passed_through_unvalidated` (both moved to `server-rt`). Keep `parse_of_a_zeroed_payload_yields_a_rejectable_request` — it is about `validate_write`.

In `drivers/tty/src/main.rs`, change the parse line in `do_write`:

```rust
    let req = minixrs_server_rt::cdev::parse(msg);
```

and add to the module doc's "Three things that differ from a server" a fourth short paragraph after the safecopy one:

```rust
//! **`CDEV_READ` lands in the unknown-request arm, on purpose.** Slice 5.11
//! defined the request for the memory driver's `/dev/zero`; TTY cannot serve it
//! until Phase 6 gives it RX (`SYS_IRQCTL`), and `ENOSYS` — "this driver does not
//! know the request" — is the honest answer until then. VFS routes a console
//! `read()` here anyway, so Phase 6 adds one arm below and changes VFS not at all.
```

- [ ] **Step 4: Run TTY's tests, lint both crates**

Run:
```bash
cargo test -p minixrs-tty 2>&1 | tail -3
cargo fmt --all
cargo clippy -p minixrs-server-rt -p minixrs-tty --all-targets -- -D warnings
```
Expected: TTY's 7 remaining tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add server-rt/src/cdev.rs server-rt/src/lib.rs drivers/tty/src/cdev.rs drivers/tty/src/main.rs
git commit -s -m "server-rt(5.11): lift the CDEV request codec out of TTY

Two drivers decode the same four fields now, so the parse moves to
server-rt::cdev the way rd_i32 moved in 5.3. Validation stays per
driver. TTY's main.rs notes that CDEV_READ deliberately lands in its
unknown-request arm until Phase 6.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 3: The memory driver serves the two minors

**Files:**
- Create: `drivers/memory/src/cdev.rs`
- Modify: `drivers/memory/src/main.rs` (module doc, imports, `mod cdev;`, two dispatch arms, two handlers, the `ZEROS` static)
- Test: `drivers/memory/src/cdev.rs`

**Interfaces:**
- Consumes: `minixrs_server_rt::cdev::{Request, parse}` (Task 2); `callnr::{CDEV_READ, CDEV_WRITE, CDEV_MINOR_NULL, CDEV_MINOR_ZERO, CDEV_MAX_IO}` (Task 1); `sys_safecopy(direction, granter, gid, offset, addr, bytes) -> i32`.
- Produces: `cdev::Minor { Null, Zero }`, `cdev::classify(i32) -> Result<Minor, i32>`, `cdev::validate(Request) -> Result<(Minor, usize), i32>`, `cdev::zero_chunk(len: usize, done: usize) -> usize`. The driver answers `CDEV_WRITE` with `len` for both minors, `CDEV_READ` with `0` (null) or `len` (zero).

- [ ] **Step 1: Write the pure module with failing tests**

Create `drivers/memory/src/cdev.rs`:

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `/dev/null` and `/dev/zero` — the memory driver's character minors, and the
//! pure logic behind them (slice 5.11).
//!
//! MINIX 3's memory driver owns these two devices beside its ramdisks, and so
//! does this one, under MINIX's minor numbers (`NULL_DEV` 3, `ZERO_DEV` 5). They
//! share the driver with the BDEV ramdisk but not a namespace: a minor is
//! per-request-band, so `BDEV_MINOR_RAMDISK` 0 and these two never meet.
//!
//! ## Two things this module is careful about
//!
//! **Nothing is clamped.** `CDEV_MAX_IO` exists because TTY stages through a
//! 256-byte stack buffer. A `/dev/null` write moves nothing at all and a
//! `/dev/zero` read copies from a constant, so both answer the *whole* request in
//! one round; the zero read merely walks the grant in `CDEV_MAX_IO`-sized
//! `SYS_SAFECOPY` calls ([`zero_chunk`]) because that is the constant that
//! already exists.
//!
//! **The check order is TTY's.** Minor, then length, then grant id — so a request
//! that is wrong in two ways hears the same first error from either driver.

use minixrs_kernel_shared::callnr::{CDEV_MAX_IO, CDEV_MINOR_NULL, CDEV_MINOR_ZERO};
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;
use minixrs_server_rt::cdev::Request;

/// The character minors this driver serves.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Minor {
    /// `/dev/null`: reads are EOF, writes discard.
    Null,
    /// `/dev/zero`: reads fill with zeroes, writes discard.
    Zero,
}

/// Map a minor number to a device, or `ENXIO`.
///
/// `CDEV_MINOR_CONSOLE` (0) is `ENXIO` *here*: it is TTY's minor, and minors are
/// a per-driver namespace.
pub fn classify(minor: i32) -> Result<Minor, i32> {
    match minor {
        CDEV_MINOR_NULL => Ok(Minor::Null),
        CDEV_MINOR_ZERO => Ok(Minor::Zero),
        _ => Err(ENXIO),
    }
}

/// Decide which device `req` names and how many bytes it covers, or which errno
/// to reply.
///
/// 1. Unknown minor → `ENXIO`.
/// 2. `len < 0` → `EINVAL`. Unchecked it would widen into a ~16 EiB `u64`.
/// 3. An invalid grant id → `EINVAL`. Checked even for `/dev/null`, whose write
///    never touches the grant: a client that sent garbage should hear so, and the
///    kernel re-validates everything real on the copies that do happen.
/// 4. Otherwise the full length. **No clamp** — see the module note.
pub fn validate(req: Request) -> Result<(Minor, usize), i32> {
    let minor = classify(req.minor)?;
    if req.len < 0 {
        return Err(EINVAL);
    }
    if !grant_valid(req.gid) {
        return Err(EINVAL);
    }
    Ok((minor, req.len as usize))
}

/// Bytes the next `SYS_SAFECOPY` of a `/dev/zero` read moves, given the request's
/// total and how much has already landed.
///
/// `min(len - done, CDEV_MAX_IO)`, saturating so a caller that has somehow
/// overshot gets `0` (a loop terminator) rather than a wrapped huge count.
pub fn zero_chunk(len: usize, done: usize) -> usize {
    len.saturating_sub(done).min(CDEV_MAX_IO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::CDEV_MINOR_CONSOLE;
    use minixrs_kernel_shared::grant::{GRANT_INVALID, grant_id};

    const GOOD_GID: i32 = grant_id(3, 1);

    fn req(minor: i32, gid: i32, len: i32) -> Request {
        Request {
            minor,
            gid,
            len,
            offset: 0,
        }
    }

    #[test]
    fn the_two_minors_classify_and_everything_else_is_enxio() {
        assert_eq!(classify(CDEV_MINOR_NULL), Ok(Minor::Null));
        assert_eq!(classify(CDEV_MINOR_ZERO), Ok(Minor::Zero));
        // The console is TTY's minor, not this driver's.
        for minor in [CDEV_MINOR_CONSOLE, 1, 2, 4, 6, 7, -1, i32::MAX] {
            assert_eq!(classify(minor), Err(ENXIO), "minor {minor}");
        }
    }

    #[test]
    fn a_normal_request_passes_its_whole_length_through() {
        // No clamp: lengths past CDEV_MAX_IO come back whole (Z4).
        for len in [0i32, 1, 64, CDEV_MAX_IO as i32, CDEV_MAX_IO as i32 + 1, 4096, i32::MAX] {
            assert_eq!(
                validate(req(CDEV_MINOR_ZERO, GOOD_GID, len)),
                Ok((Minor::Zero, len as usize)),
                "len {len}"
            );
            assert_eq!(
                validate(req(CDEV_MINOR_NULL, GOOD_GID, len)),
                Ok((Minor::Null, len as usize)),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_negative_length_is_einval() {
        for len in [-1i32, -256, i32::MIN] {
            assert_eq!(validate(req(CDEV_MINOR_ZERO, GOOD_GID, len)), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn an_invalid_grant_id_is_einval_even_for_null() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            assert_eq!(validate(req(CDEV_MINOR_NULL, gid, 16)), Err(EINVAL), "gid {gid}");
            assert_eq!(validate(req(CDEV_MINOR_ZERO, gid, 16)), Err(EINVAL), "gid {gid}");
        }
    }

    #[test]
    fn the_minor_check_precedes_the_length_and_grant_checks() {
        // TTY's order, so a doubly-wrong request hears the same first error from
        // either driver.
        assert_eq!(validate(req(9, GRANT_INVALID, -5)), Err(ENXIO));
        // ...and a bad grant is reported on a zero-length request, not masked.
        assert_eq!(validate(req(CDEV_MINOR_NULL, GRANT_INVALID, 0)), Err(EINVAL));
    }

    #[test]
    fn zero_chunk_walks_the_request_in_cdev_max_io_steps() {
        assert_eq!(zero_chunk(64, 0), 64);
        assert_eq!(zero_chunk(CDEV_MAX_IO, 0), CDEV_MAX_IO);
        assert_eq!(zero_chunk(CDEV_MAX_IO + 1, 0), CDEV_MAX_IO);
        assert_eq!(zero_chunk(CDEV_MAX_IO + 1, CDEV_MAX_IO), 1);
        assert_eq!(zero_chunk(4096, 4096), 0);
        // Overshoot saturates to a loop terminator rather than wrapping.
        assert_eq!(zero_chunk(10, 11), 0);
    }
}
```

- [ ] **Step 2: Run the driver's tests to see them fail to compile, then wire the module**

Run: `cargo test -p minixrs-memory cdev 2>&1 | tail -3`
Expected: fails — `mod cdev` is not declared yet (or, if you declared it first, passes; either way proceed).

In `drivers/memory/src/main.rs`:

1. After `mod bdev;` add `mod cdev;`.
2. Extend the `callnr` import list with `CDEV_MAX_IO, CDEV_READ, CDEV_WRITE` (keep it sorted; `cargo fmt` will not reorder identifiers inside braces, so put them in alphabetical position).
3. Rewrite the crate doc's first line and add a paragraph. First line becomes:
   ```rust
   //! minix.rs `memory` driver — the boot ramdisk (slice 5.7, decision D3), plus
   //! `/dev/null` and `/dev/zero` as of slice 5.11.
   ```
   and after the "Four things worth knowing" list add:
   ```rust
   //! **Two character minors ride the same driver, on a different band.** MINIX 3's
   //! memory driver owns `/dev/null` and `/dev/zero` beside its ramdisks, and so does
   //! this one — `CDEV_WRITE` discards and answers the full count, `CDEV_READ`
   //! answers `0` for null and fills the whole request for zero. Nothing is clamped
   //! (there is no staging buffer to protect), and a null/zero *write* issues no
   //! `SYS_SAFECOPY` at all, so a write with an unmapped buffer succeeds — Linux's
   //! behaviour, and the reason no `bad-buf` probe may ever be aimed at `/dev/null`.
   //! See `cdev.rs`.
   ```
4. Add the two arms to the `match msg.m_type` in `main`, between `BDEV_WRITE` and `_`:
   ```rust
            // Slice 5.11: the character minors. Same driver, different band —
            // `cdev::classify` refuses the ramdisk's minor 0 here, because a minor
            // is per band, not per driver.
            CDEV_WRITE => {
                let rc = do_cdev_write(&msg);
                reply(caller_e, &mut msg, rc);
            }
            CDEV_READ => {
                let rc = do_cdev_read(caller_e, &msg);
                reply(caller_e, &mut msg, rc);
            }
   ```
5. Add the static and the two handlers before `reply`:
   ```rust
   /// The bytes a `/dev/zero` read is served from: one `CDEV_MAX_IO` window of
   /// zeroes, copied at advancing grant offsets until the request is filled.
   ///
   /// A static rather than a `main`-frame local for the reason MFS's block buffer
   /// is one: the address never changes. (At 256 bytes the one-page stack was never
   /// the concern; `.bss` is simply the right home for a constant the kernel reads
   /// through a copy call.)
   static ZEROS: [u8; CDEV_MAX_IO] = [0u8; CDEV_MAX_IO];

   /// Serve one `CDEV_WRITE` to a character minor. Returns the reply `m_type`: the
   /// byte count "written" (`>= 0`), or a negative errno.
   ///
   /// Both minors discard, so **no `SYS_SAFECOPY` is issued** — the grant is
   /// checked for shape only ([`cdev::validate`]) — and the whole count comes back
   /// in one round, `CDEV_MAX_IO` being TTY's staging limit rather than a band rule.
   /// A write whose buffer is unmapped therefore succeeds, as it does on Linux,
   /// because nothing reads the buffer.
   #[cfg_attr(test, allow(dead_code))]
   fn do_cdev_write(msg: &Message) -> i32 {
       let req = minixrs_server_rt::cdev::parse(msg);
       match cdev::validate(req) {
           Ok((_, n)) => n as i32,
           Err(e) => e,
       }
   }

   /// Serve one `CDEV_READ` from a character minor. Returns the reply `m_type`: the
   /// byte count read (`0` is EOF), or a negative errno.
   ///
   /// `/dev/null` is EOF. `/dev/zero` pushes [`ZEROS`] into the client's granted
   /// buffer one [`cdev::zero_chunk`] at a time, advancing the grant offset, and
   /// reports the full length — a short read is *legal* here but there is no
   /// reason to give one. `caller_e` is the kernel-stamped source of this very
   /// message: the granter can be nothing else.
   ///
   /// A negative `SYS_SAFECOPY` result is relayed verbatim (`EPERM` vs `EFAULT` are
   /// different client bugs) — **unless bytes already landed**, in which case the
   /// progress is reported, `write_all`'s rule from slice 5.4: those bytes really
   /// are in the buffer.
   #[cfg_attr(test, allow(dead_code))]
   fn do_cdev_read(caller_e: Endpoint, msg: &Message) -> i32 {
       let req = minixrs_server_rt::cdev::parse(msg);
       let (minor, len) = match cdev::validate(req) {
           Ok(v) => v,
           Err(e) => return e,
       };
       match minor {
           cdev::Minor::Null => 0,
           cdev::Minor::Zero => {
               let mut done = 0usize;
               while done < len {
                   let chunk = cdev::zero_chunk(len, done);
                   let Some(offset) = req.offset.checked_add(done as u64) else {
                       return if done == 0 { EINVAL } else { done as i32 };
                   };
                   let rc = sys_safecopy(
                       SAFECOPY_TO,
                       caller_e,
                       req.gid,
                       offset,
                       ZEROS.as_ptr() as usize as u64,
                       chunk as u64,
                   );
                   if rc != OK {
                       return if done == 0 { rc } else { done as i32 };
                   }
                   done += chunk;
               }
               len as i32
           }
       }
   }
   ```
   `EINVAL` needs adding to the `error` import list (`use minixrs_kernel_shared::error::{EINVAL, ENOSYS, OK};`). `len as i32` is lossless: `len` came in as a non-negative `i32`.

- [ ] **Step 3: Test, lint, and check the stack frame**

Run:
```bash
cargo test -p minixrs-memory 2>&1 | tail -3
cargo fmt --all
cargo clippy -p minixrs-memory --all-targets -- -D warnings
```
Expected: all tests pass (the 6 new plus bdev's), clippy clean. Then build the kernel once so the driver ELF exists and check its largest frame is unchanged:

```bash
MINIXRS_SDK=/nonexistent cargo kernel-aarch64 2>&1 | tail -2
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump -d \
  target/minixrs-user/aarch64-unknown-minixrs/release/minixrs-memory \
  | grep -oE 'sub[[:space:]]+sp, sp, #0x[0-9a-f]+' | grep -oE '0x[0-9a-f]+' \
  | while read h; do printf '%d\n' "$h"; done | sort -n | tail -1
```
Expected: a number well under 4096 (5.10b's driver was a few hundred bytes).

- [ ] **Step 4: Commit**

```bash
git add drivers/memory/src/cdev.rs drivers/memory/src/main.rs
git commit -s -m "memory(5.11): serve /dev/null and /dev/zero as CDEV minors 3 and 5

CDEV_WRITE discards and answers the full count with no copy; CDEV_READ
answers 0 for null and fills the whole request for zero from a 256-byte
static, walking the grant in CDEV_MAX_IO steps. Nothing is clamped --
there is no staging buffer to protect. Check order is TTY's.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 4: VFS's fd table names its driver, and the device-node table

**Files:**
- Modify: `servers/vfs/src/fd.rs:70-125` (imports, `Fd`, `DEFAULT_ROW`), `:280-282` and `:389-393` (test fixtures)
- Create: `servers/vfs/src/dev.rs`
- Modify: `servers/vfs/src/main.rs:120` (`mod dev;`) — **only** the module declaration and the `use fd::Fd;` line; the handler changes are Task 5. To keep the crate compiling at the end of this task, also apply the three mechanical pattern edits listed in Step 3.
- Test: `servers/vfs/src/fd.rs`, `servers/vfs/src/dev.rs`

**Interfaces:**
- Consumes: `callnr::{CDEV_MINOR_CONSOLE, CDEV_MINOR_NULL, CDEV_MINOR_ZERO, DEV_CONSOLE_PATH, DEV_NULL_PATH, DEV_ZERO_PATH}` (Task 1).
- Produces: `fd::CharDriver { Tty, Memory }` (`Copy + PartialEq + Eq + Debug`); `fd::Fd::CharDev { dev: CharDriver, minor: i32 }`; `dev::NR_DEV_NODES: usize = 3`; `dev::lookup(path: &[u8]) -> Option<Fd>`.

- [ ] **Step 1: Change the variant and watch the fixtures fail**

In `servers/vfs/src/fd.rs`, add above `pub enum Fd`:

```rust
/// Which character driver a [`Fd::CharDev`] descriptor talks to (slice 5.11).
///
/// An enum rather than an `Endpoint`, because [`DEFAULT_ROW`] is a `const` and a
/// DS-resolved endpoint is a runtime value. `main.rs`'s `cdev_endpoint` is the
/// one place this becomes an address.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CharDriver {
    /// TTY: the console, minor `CDEV_MINOR_CONSOLE`.
    Tty,
    /// The memory driver: `CDEV_MINOR_NULL` and `CDEV_MINOR_ZERO`.
    Memory,
}
```

and change the variant:

```rust
    /// A character device: the console on TTY, or `/dev/null` / `/dev/zero` on
    /// the memory driver, routed by `dev`.
    CharDev {
        /// The driver that owns `minor`. Minors are a per-driver namespace, so
        /// this is half of the address, not decoration.
        dev: CharDriver,
        /// Device minor, e.g. [`CDEV_MINOR_CONSOLE`]. Passed through to the
        /// driver, which is the one that decides whether it exists (`ENXIO`).
        minor: i32,
    },
```

`DEFAULT_ROW`'s three entries become `Fd::CharDev { dev: CharDriver::Tty, minor: CDEV_MINOR_CONSOLE }`. In the module note, rewrite "fds 0, 1, and 2 name the console character device in every row" to "fds 0, 1, and 2 name the console — TTY's `CDEV_MINOR_CONSOLE` — in every row" and the "**today only the console**" phrase in the `CharDev` doc is gone with the doc above.

Run: `cargo test -p minixrs-vfs 2>&1 | grep -E '^error' | head`
Expected: errors at the two test fixtures (`CONSOLE`, and the `minor: 7` lines) and in `main.rs`'s three `Fd::CharDev { minor }` patterns.

- [ ] **Step 2: Fix the fixtures**

In the tests: `const CONSOLE: Fd = Fd::CharDev { dev: CharDriver::Tty, minor: CDEV_MINOR_CONSOLE };` and at the `minor: 7` site use a memory-driver entry so the fixture exercises the new field:

```rust
        r[1][3] = Fd::CharDev {
            dev: CharDriver::Memory,
            minor: 7,
        };

        assert_eq!(resolve_in(&r, 0, 1), Ok(CONSOLE));
        assert_eq!(resolve_in(&r, 1, 1), Err(EBADF));
        assert_eq!(
            resolve_in(&r, 1, 3),
            Ok(Fd::CharDev {
                dev: CharDriver::Memory,
                minor: 7,
            })
        );
```

- [ ] **Step 3: Mechanically patch `main.rs` so the crate compiles (routing is Task 5)**

In `servers/vfs/src/main.rs`: leave `use fd::Fd;` as it is (Task 5 widens it) and add after `mod fd;`:

```rust
// Consumed by `do_open` in the next commit; the allow goes with it.
#[allow(dead_code)]
mod dev;
```

Then the three pattern sites:

- `do_write`: `Ok(Fd::CharDev { minor }) => Fd::CharDev { minor },` → `Ok(Fd::CharDev { dev, minor }) => Fd::CharDev { dev, minor },` and the arm `Fd::CharDev { minor } => {` → `Fd::CharDev { dev: _, minor } => {` (Task 5 replaces the `_`).
- `do_read`: `Ok(Fd::CharDev { .. }) => return ENOSYS,` stays as-is for now.
- `do_open`'s `_ => return EINVAL,` arm stays.

`CharDriver` will be an unused import until Task 5 — add it *in Task 5* instead if clippy's `-D warnings` flags it here; the point of this step is only that `cargo test -p minixrs-vfs` compiles.

- [ ] **Step 4: Write the device table with failing tests**

Create `servers/vfs/src/dev.rs`:

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The device-node table: three paths VFS answers itself, ahead of the mount
//! (slice 5.11, decision Z6).
//!
//! There is no `/dev` on the image and no device inode — the deliberate
//! simplification decision D11 names — so `open` matches these paths
//! **byte-for-byte** before it consults the filesystem, and a hit becomes a
//! [`Fd::CharDev`] naming the driver and minor. Everything else, `/dev/other`
//! included, falls through to MFS unchanged and answers whatever the FS path
//! answers (`ENOENT`, from the walk of a `/dev` that does not exist).
//!
//! Exact match is the whole contract. `/dev//null`, `/dev/./null`, a trailing
//! slash, a trailing NUL, a case variant — all misses, all MFS's problem. A real
//! `/dev` with inodes replaces this table; the `CharDriver` resolution stays.
//!
//! Pure and host-tested; the copy-in and the descriptor allocation are `main.rs`'s.

use minixrs_kernel_shared::callnr::{
    CDEV_MINOR_CONSOLE, CDEV_MINOR_NULL, CDEV_MINOR_ZERO, DEV_CONSOLE_PATH, DEV_NULL_PATH,
    DEV_ZERO_PATH,
};

use crate::fd::{CharDriver, Fd};

/// Rows in the table.
pub const NR_DEV_NODES: usize = 3;

/// The table: path, driver, minor. Paths come from `kernel-shared` so init's
/// probes cannot drift from what VFS matches.
static DEV_NODES: [(&str, CharDriver, i32); NR_DEV_NODES] = [
    (DEV_CONSOLE_PATH, CharDriver::Tty, CDEV_MINOR_CONSOLE),
    (DEV_NULL_PATH, CharDriver::Memory, CDEV_MINOR_NULL),
    (DEV_ZERO_PATH, CharDriver::Memory, CDEV_MINOR_ZERO),
];

/// Resolve `path` — the exact bytes VFS copied in, no terminator — against the
/// table.
pub fn lookup(path: &[u8]) -> Option<Fd> {
    DEV_NODES
        .iter()
        .find(|(p, _, _)| p.as_bytes() == path)
        .map(|&(_, dev, minor)| Fd::CharDev { dev, minor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_resolves_to_its_driver_and_minor() {
        assert_eq!(
            lookup(b"/dev/console"),
            Some(Fd::CharDev {
                dev: CharDriver::Tty,
                minor: CDEV_MINOR_CONSOLE,
            })
        );
        assert_eq!(
            lookup(b"/dev/null"),
            Some(Fd::CharDev {
                dev: CharDriver::Memory,
                minor: CDEV_MINOR_NULL,
            })
        );
        assert_eq!(
            lookup(b"/dev/zero"),
            Some(Fd::CharDev {
                dev: CharDriver::Memory,
                minor: CDEV_MINOR_ZERO,
            })
        );
    }

    #[test]
    fn anything_but_an_exact_match_misses() {
        // Each of these must reach MFS, not the table: a prefix, a suffix, a
        // doubled slash, a dot component, a case variant, a stray terminator, the
        // empty path, and the `/dev` directory itself.
        for path in [
            &b"/dev/nul"[..],
            b"/dev/null/",
            b"/dev/nullx",
            b"/dev//null",
            b"/dev/./null",
            b"/dev/NULL",
            b"/dev/null\0",
            b"dev/null",
            b"",
            b"/dev",
            b"/dev/",
            b"/dev/nope",
        ] {
            assert_eq!(lookup(path), None, "{:?}", core::str::from_utf8(path));
        }
    }

    #[test]
    fn the_table_has_no_duplicate_paths_and_uses_both_drivers() {
        for (i, (a, _, _)) in DEV_NODES.iter().enumerate() {
            for (b, _, _) in &DEV_NODES[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(DEV_NODES.iter().any(|(_, d, _)| *d == CharDriver::Tty));
        assert!(DEV_NODES.iter().any(|(_, d, _)| *d == CharDriver::Memory));
        assert_eq!(DEV_NODES.len(), NR_DEV_NODES);
    }
}
```

- [ ] **Step 5: Run VFS's tests, lint, commit**

Run:
```bash
cargo test -p minixrs-vfs 2>&1 | tail -3
cargo fmt --all
cargo clippy -p minixrs-vfs --all-targets -- -D warnings
```
Expected: all pass and clippy clean (the module-level `allow` covers `dev::lookup` until Task 5 consumes it). Commit:

```bash
git add servers/vfs/src/fd.rs servers/vfs/src/dev.rs servers/vfs/src/main.rs
git commit -s -m "vfs(5.11): Fd::CharDev names its driver; the device-node table

CharDriver { Tty, Memory } on the variant (an enum, not an Endpoint,
because DEFAULT_ROW is a const), and dev.rs: three exact-match paths
from kernel-shared resolving to (driver, minor). Routing lands next.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 5: VFS routes — open intercept, read to the driver, write by driver, the `mem` peer, `mem.deny`

**Files:**
- Modify: `servers/vfs/src/main.rs` — imports; `main` (peer resolution, dispatch, prologue); `do_write`; `write_all`; `do_open` (reorder + intercept + doc list); `do_read`; new `cdev_endpoint`, `mem_endpoint`, `cdev_request`/`cdev_read`, `mem_denials`; `cdev_write` becomes a wrapper

**Interfaces:**
- Consumes: `dev::lookup`, `fd::CharDriver` (Task 4); `callnr::{CDEV_READ, CDEV_MINOR_ZERO}` (Task 1); `com::MEM_PROC_NR`; memory driver contract (Task 3).
- Produces: diag lines `mem.ds ok ep=N` / `mem.ds FAIL rc=R fallback=E`, `mem.deny ok n=5` / `mem.deny FAIL <name> rc=R`; `VFS_READ` on a device descriptor returns the driver's count; `VFS_OPEN` of a `DEV_*_PATH` returns a descriptor without touching MFS.

- [ ] **Step 1: Imports and peer resolution**

In the `callnr` import add `CDEV_MINOR_ZERO, CDEV_READ`; in the `com` import add `MEM_PROC_NR`; change `use fd::Fd;` to `use fd::{CharDriver, Fd};` and delete the `#[allow(dead_code)]` (and its comment) Task 4 put on `mod dev;`. In `main`, after `let mfs = mfs_endpoint();`:

```rust
    // Slice 5.11: the third peer. `/dev/null` and `/dev/zero` live on the memory
    // driver, so a device read or write needs its endpoint — resolved once, like
    // the other two, for the same reason.
    let mem = mem_endpoint();
```

and after `fs_denials(&mut grants, mfs, mount);`:

```rust
    // Slice 5.11: the memory driver's CDEV refusals, last for the same reason.
    mem_denials(&mut grants, mem);
```

Update the comment above `let tty = tty_endpoint();` ("Resolve the two peers once") to say three peers: "every `VFS_WRITE` targets TTY or the memory driver, every `VFS_OPEN`/`VFS_READ` targets MFS or a driver". Change the dispatch:

```rust
            VFS_WRITE => do_write(caller_e, &msg, &mut grants, tty, mem, mfs),
            VFS_OPEN => do_open(caller_e, &msg, &mut mount, mfs),
            VFS_READ => do_read(caller_e, &msg, &mut grants, tty, mem, mfs),
```

Add beside `mfs_endpoint`:

```rust
/// Resolve the memory driver's endpoint through DS, falling back to its boot
/// endpoint (slice 5.11). [`tty_endpoint`]'s contract, third copy: the DS chain
/// `ds < tty < memory < mfs < vfs` is packing order in `kernel/build.rs`, and a
/// failed lookup keeps every device marker alive while `mem.ds ok` disappears.
#[cfg_attr(test, allow(dead_code))]
fn mem_endpoint() -> Endpoint {
    let mut key = [0u8; SYS_GETINFO_NAME_LEN];
    key[0..6].copy_from_slice(b"memory");
    match sef_retrieve_from_ds(&key) {
        Ok(ep) => {
            diag_fmt(format_args!("mem.ds ok ep={ep}"));
            ep
        }
        Err(rc) => {
            let ep = boot_endpoint(MEM_PROC_NR);
            diag_fmt(format_args!("mem.ds FAIL rc={rc} fallback={ep}"));
            ep
        }
    }
}

/// The one place a [`CharDriver`] becomes an address.
fn cdev_endpoint(dev: CharDriver, tty: Endpoint, mem: Endpoint) -> Endpoint {
    match dev {
        CharDriver::Tty => tty,
        CharDriver::Memory => mem,
    }
}
```

(Check the memory driver's DS key: `grep -n 'copy_from_slice(b"memory")' fs/mfs/src/main.rs` — MFS looks it up the same way; match its spelling exactly.)

- [ ] **Step 2: `do_write` and `write_all` route by driver**

`do_write` signature gains `mem: Endpoint` between `tty` and `mfs`. Its `CharDev` arm:

```rust
        Fd::CharDev { dev, minor } => {
            // The single-copy hop: the grant names the *caller's* memory, so the
            // kernel moves the bytes from the caller straight into the driver —
            // TTY for the console, the memory driver for `/dev/null`/`/dev/zero`.
            let driver = cdev_endpoint(dev, tty, mem);
            let gid = match grants.grant_magic(driver, caller_e, req.buf, len as u64, CPF_READ) {
                Ok(gid) => gid,
                Err(e) => return e,
            };
            let written = write_all(driver, minor, gid, len);
            let _ = grants.revoke(gid);
            written
        }
```

`write_all(driver: Endpoint, minor: i32, gid: i32, len: usize)` — rename the parameter and the call inside (`cdev_write(driver, …)`); in its doc, "through a working TTY" → "through a working driver".

- [ ] **Step 3: `do_read` sends `CDEV_READ`**

Replace `do_read`'s resolve block and tail:

```rust
    let target = match fd::resolve(proc_nr, req.fd) {
        Ok(Fd::File { ino, pos }) => Fd::File { ino, pos },
        // Slice 5.11 (Z7): routed to its driver, TTY included. TTY does not serve
        // `CDEV_READ` until Phase 6 gives it RX and answers `ENOSYS` from its
        // unknown-request arm — the same errno this arm used to short-circuit
        // locally, now the driver's answer rather than VFS's guess about it. So
        // Phase 6 changes TTY and nothing here.
        Ok(Fd::CharDev { dev, minor }) => Fd::CharDev { dev, minor },
        Ok(Fd::Unused) => return EBADF,
        Err(e) => return e,
    };

    let len = match rw::validate(req.len, req.buf) {
        Ok(len) => len,
        Err(e) => return e,
    };
    if len == 0 {
        // A legal empty read. No grant is issued, so a client polling with
        // `len = 0` cannot use it to probe the granting path.
        return 0;
    }

    match target {
        Fd::File { ino, pos } => {
            let gid = match grants.grant_magic(mfs, caller_e, req.buf, len as u64, CPF_WRITE) {
                Ok(gid) => gid,
                Err(e) => return e,
            };
            let n = fs_read(mfs, ino as i32, gid, len as i32, pos);
            let _ = grants.revoke(gid);

            if n > 0 {
                // Only on real progress: advancing on an error or on EOF would
                // silently move the descriptor past bytes nobody read.
                fd::advance(proc_nr, req.fd, n as u64);
            }
            n
        }
        Fd::CharDev { dev, minor } => {
            // One round, no loop (the `FS_READ` stance: a short read is legal),
            // and no `fd::advance` — a character device has no position.
            let driver = cdev_endpoint(dev, tty, mem);
            let gid = match grants.grant_magic(driver, caller_e, req.buf, len as u64, CPF_WRITE) {
                Ok(gid) => gid,
                Err(e) => return e,
            };
            let n = cdev_read(driver, minor, gid, len as i32, 0);
            let _ = grants.revoke(gid);
            n
        }
        Fd::Unused => EBADF,
    }
```

Signature: `fn do_read(caller_e, msg, grants, tty: Endpoint, mem: Endpoint, mfs: Endpoint) -> i32`. In its doc comment, replace the sentence about `CDEV_READ` not existing (if any) and add: "A device descriptor takes the same shape against its driver, with `CDEV_READ` in place of `FS_READ` and no position to advance."

- [ ] **Step 4: `do_open` intercepts device paths after the copy, before the mount**

Reorder the body:

```rust
    let flags = match open::validate_flags(req.flags) {
        Ok(flags) => flags,
        Err(e) => return e,
    };

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

    // Slice 5.11 (Z6): device nodes are answered here — after the path is in,
    // **before** the mount is consulted, because a device open must not need a
    // filesystem. `O_CREAT` and `O_TRUNC` are ignored on a hit, Linux's behaviour
    // for a device node: creating an existing name is a plain open, and
    // truncating a device has no meaning. Every other flag rule already ran.
    if let Some(entry) = dev::lookup(&path[..len]) {
        return match fd::alloc(endpoint_proc(caller_e).get(), entry) {
            Ok(fd) => fd,
            Err(e) => e,
        };
    }

    if let Err(e) = ensure_mounted(mount, mfs) {
        return e;
    }
```

Update the doc comment's numbered list to the new order (1 validate, 2 flags, 3 copy, 4 device table, 5 mount, 6 MFS resolve, 7 lowest free fd, 8 `O_TRUNC` last) and reword the `_ => return EINVAL` arm's comment in the alloc match to: "No other variant is reachable — `classify` returns only `File` or an error, and the device arm lives above, before the mount — but routing it explicitly keeps a new `Fd` variant a compile error here rather than a silent `EINVAL`."

- [ ] **Step 5: `cdev_request`, `cdev_read`, `cdev_write`**

Replace `cdev_write` with a generic sender and two wrappers:

```rust
/// Issue one CDEV request to `driver` and return the reply `m_type` — the byte
/// count moved, or a negative errno.
///
/// No granter goes in the payload: the driver takes it from the kernel-stamped
/// `m_source`, so this message cannot aim a driver's privileged `SYS_SAFECOPY`
/// anywhere but VFS's own address space (or, through a magic grant VFS issued,
/// exactly the client buffer VFS named).
#[cfg_attr(test, allow(dead_code))]
fn cdev_request(driver: Endpoint, m_type: i32, minor: i32, gid: i32, len: i32, offset: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, CDEV_MINOR_OFF, minor);
    wr_i32(&mut m, CDEV_GRANT_OFF, gid);
    wr_i32(&mut m, CDEV_LEN_OFF, len);
    wr_u64(&mut m, CDEV_OFFSET_OFF, offset);
    let trap_rc = ipc_sendrec(driver, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// One `CDEV_WRITE`. See [`cdev_request`].
#[cfg_attr(test, allow(dead_code))]
fn cdev_write(driver: Endpoint, minor: i32, gid: i32, len: i32, offset: u64) -> i32 {
    cdev_request(driver, CDEV_WRITE, minor, gid, len, offset)
}

/// One `CDEV_READ` (slice 5.11). See [`cdev_request`].
#[cfg_attr(test, allow(dead_code))]
fn cdev_read(driver: Endpoint, minor: i32, gid: i32, len: i32, offset: u64) -> i32 {
    cdev_request(driver, CDEV_READ, minor, gid, len, offset)
}
```

- [ ] **Step 6: `mem_denials`**

Add after `cdev_denials`:

```rust
/// Bytes granted to the memory driver by [`mem_denials`]. A `main`-frame local's
/// worth; nothing here ever succeeds in writing them.
const MEM_DENY_LEN: usize = 32;

/// Probe the memory driver's CDEV refusals (slice 5.11, Z8) — the ones VFS's own
/// device table can never send, because it maps only minors that exist.
///
/// Each probe is well-formed in every respect but one:
///
///   - `bad-minor-w` / `bad-minor-r` — a good grant aimed at minor 7, on each
///     request. `ENXIO` from the driver's `classify`; nothing is wrong with the
///     grant.
///   - `bad-len` — a negative length, which unchecked would widen into a ~16 EiB
///     `u64` on the copy. `EINVAL`.
///   - `bad-gid` — `GRANT_INVALID`. `EINVAL`, the driver's local reject of the one
///     value that can never name a grant.
///   - `read-only-grant` — a `CDEV_READ` through a grant carrying only `CPF_READ`.
///     The driver passes it to `SYS_SAFECOPY(SAFECOPY_TO)` in good faith and the
///     *kernel* refuses the direction on `verify_grant`'s access check. `EPERM`,
///     relayed verbatim — the read-path twin of `cdev.deny`'s `not-mine`, and
///     the first `CPF_WRITE`-required refusal any boot marker exercises.
#[cfg_attr(test, allow(dead_code))]
fn mem_denials(grants: &mut GrantPool<GRANT_SLOTS>, mem: Endpoint) {
    let mut buf = [0u8; MEM_DENY_LEN];
    let addr = buf_addr(&mut buf);
    let len = MEM_DENY_LEN as u64;
    let (Ok(readable), Ok(writable)) = (
        grants.grant_direct(mem, addr, len, CPF_READ),
        grants.grant_direct(mem, addr, len, CPF_WRITE),
    ) else {
        return diag_fmt(format_args!("mem.deny FAIL setup"));
    };

    let n = MEM_DENY_LEN as i32;
    // (name, request, minor, len, grant, expected reply)
    let probes: [(&str, i32, i32, i32, i32, i32); 5] = [
        ("bad-minor-w", CDEV_WRITE, 7, n, readable, ENXIO),
        ("bad-minor-r", CDEV_READ, 7, n, writable, ENXIO),
        ("bad-len", CDEV_READ, CDEV_MINOR_ZERO, -1, writable, EINVAL),
        ("bad-gid", CDEV_READ, CDEV_MINOR_ZERO, n, GRANT_INVALID, EINVAL),
        ("read-only-grant", CDEV_READ, CDEV_MINOR_ZERO, n, readable, EPERM),
    ];

    let mut denied = 0usize;
    for (name, m_type, minor, len, gid, want) in probes {
        let rc = cdev_request(mem, m_type, minor, gid, len, 0);
        if rc == want {
            denied += 1;
        } else {
            diag_fmt(format_args!("mem.deny FAIL {name} rc={rc}"));
        }
    }
    if denied == probes.len() {
        diag_fmt(format_args!("mem.deny ok n={denied}"));
    }
    let _ = grants.revoke(readable);
    let _ = grants.revoke(writable);
}
```

Also update the crate doc's write-path diagram caption: "TTY" → "the driver (TTY for the console, the memory driver for `/dev/null` and `/dev/zero`)", one sentence, and add to the read-path section a line: "A device descriptor reads through `CDEV_READ` against its driver instead of `FS_READ` against MFS, one round, no position (slice 5.11)."

- [ ] **Step 7: Test, lint, boot the stub-free config**

```bash
cargo test -p minixrs-vfs 2>&1 | tail -3
cargo fmt --all
cargo clippy -p minixrs-vfs --all-targets -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings
MINIXRS_SDK=/nonexistent timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features > /private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad/t5.log 2>&1
grep -a 'mem.ds\|mem.deny\|cdev.deny\|fs.deny\|open.deny\|read-console' /private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad/t5.log
```
Expected: `[diag vfs] mem.ds ok ep=…`, `[diag vfs] mem.deny ok n=5`, `cdev.deny ok n=2`, `fs.deny ok n=14`, and init's `open.deny ok n=11` still present (its `read-console` probe still hears `ENOSYS`, now from TTY). Check `grep -a 'error\[E' t5.log` is empty before trusting any absence.

- [ ] **Step 8: Commit**

```bash
git add servers/vfs/src/main.rs
git commit -s -m "vfs(5.11): device opens ahead of the mount, reads via CDEV_READ, the mem peer

do_open matches dev::lookup after the path copy and before ensure_mounted
(O_CREAT/O_TRUNC ignored on a hit). do_read sends CDEV_READ to whichever
driver the descriptor names -- TTY answers ENOSYS from its unknown arm, so
the read-console probe is unchanged. do_write routes by CharDriver. VFS
resolves the memory driver from DS (mem.ds) and probes its five CDEV
refusals last in the prologue (mem.deny).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 6: init proves the three devices

**Files:**
- Modify: `userland/init/src/main.rs` — imports; `main` (call site after `fs_demo`); new `dev_demo`, `zero_demo`, `null_demo`, `console_demo` + consts; `open_denials` (`dev-no-such`, the `read-console` comment, count 12); `OPEN_DENIAL_PROBES`; the `open_denials` doc bullets

**Interfaces:**
- Consumes: `callnr::{DEV_CONSOLE_PATH, DEV_NULL_PATH, DEV_ZERO_PATH}` (Task 1); the existing `vfs_open`/`vfs_read`/`vfs_write`/`vfs_close`/`report_line`/`report_open_fail` helpers.
- Produces: markers `minix.rs init: dev.zero ok n=64`, `minix.rs init: dev.null ok n=35`, `minix.rs init: dev.console ok` (written through the `/dev/console` fd), `minix.rs init: open.deny ok n=12`; failure spellings `minix.rs init: dev.{zero,null,console} FAIL <step>`.

- [ ] **Step 1: The probes**

Add to the `callnr` import: `DEV_CONSOLE_PATH, DEV_NULL_PATH, DEV_ZERO_PATH`. In `main`, after `fs_demo(vfs);` and its comment:

```rust
    // Slice 5.11: the device nodes. After the read path (a device open goes
    // through `VFS_OPEN` like any other) and before the write battery, so a hang
    // here localizes to the `dev.*` markers. Cheap — no filesystem traffic.
    dev_demo(vfs);
```

Add a new section after `fd_demo`/`open_denials` (before the "Slice 5.10a: the write path" banner):

```rust
// ----- Slice 5.11: the device nodes ------------------------------------------

/// Bytes each device probe reads. A local in the probe's frame — init's stack is
/// one page — and the whole buffer is checked, not sampled.
const DEV_BUF_LEN: usize = 64;

/// What the buffer holds *before* a read: every byte must change on `/dev/zero`
/// and no byte may change on `/dev/null`. Chosen so neither `0x00` nor `0xFF`
/// could pass by accident.
const DEV_POISON: u8 = 0xA5;

// The literals in the `ok` lines below are pinned to the constants they report:
// init cannot format an integer, so a count that drifted would print a stale
// number and the marker would go stale with it.
const _: () = assert!(DEV_BUF_LEN == 64);
const _: () = assert!(HELLO.len() == 35);

/// Prove `/dev/zero`, `/dev/null`, and `/dev/console` through the POSIX path.
///
/// Three probes, three markers, each reporting its first failing step by name so
/// a `FAIL` line says *which* device and *what* went wrong. Every probe closes
/// what it opened, so the descriptor table [`open_denials`] and [`write_demo`]
/// see afterwards is the one they expect.
#[cfg_attr(test, allow(dead_code))]
fn dev_demo(vfs: Endpoint) {
    zero_demo(vfs);
    null_demo(vfs);
    console_demo(vfs);
}

/// `/dev/zero`: a read fills the whole request, and a second read is **not** EOF.
///
/// That second read is what separates zero from null — swap the two minors in
/// VFS's table and this prints `dev.zero FAIL short` (null answers `0`) while
/// [`null_demo`] fails on its write count or its EOF.
#[cfg_attr(test, allow(dead_code))]
fn zero_demo(vfs: Endpoint) {
    let mut buf = [DEV_POISON; DEV_BUF_LEN];
    let fd = vfs_open(vfs, DEV_ZERO_PATH);
    if fd < 0 {
        return report_line(vfs, b"minix.rs init: dev.zero FAIL open");
    }
    let n1 = vfs_read(vfs, fd, &mut buf);
    if n1 != DEV_BUF_LEN as i32 {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: dev.zero FAIL short");
    }
    if buf.iter().any(|&b| b != 0) {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: dev.zero FAIL dirty");
    }
    let n2 = vfs_read(vfs, fd, &mut buf);
    if n2 != DEV_BUF_LEN as i32 {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: dev.zero FAIL eof");
    }
    if vfs_close(vfs, fd) != OK {
        return report_line(vfs, b"minix.rs init: dev.zero FAIL close");
    }
    let _ = vfs_write(vfs, STDOUT, b"minix.rs init: dev.zero ok n=64\n");
}

/// `/dev/null`: a write reports its whole count, a read is EOF and touches
/// nothing.
#[cfg_attr(test, allow(dead_code))]
fn null_demo(vfs: Endpoint) {
    let mut buf = [DEV_POISON; DEV_BUF_LEN];
    let fd = vfs_open(vfs, DEV_NULL_PATH);
    if fd < 0 {
        return report_line(vfs, b"minix.rs init: dev.null FAIL open");
    }
    if vfs_write(vfs, fd, HELLO.as_bytes()) != HELLO.len() as i32 {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: dev.null FAIL write");
    }
    if vfs_read(vfs, fd, &mut buf) != 0 {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: dev.null FAIL read");
    }
    if buf.iter().any(|&b| b != DEV_POISON) {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: dev.null FAIL touched");
    }
    if vfs_close(vfs, fd) != OK {
        return report_line(vfs, b"minix.rs init: dev.null FAIL close");
    }
    let _ = vfs_write(vfs, STDOUT, b"minix.rs init: dev.null ok n=35\n");
}

/// `/dev/console`: the marker is written **through the new descriptor**, not fd 1
/// (Z10). Printing it on fd 1 after a successful open would prove only that a
/// number came back; routing the table's console row to the memory driver makes
/// this line vanish, which is the proof that the row points at TTY.
#[cfg_attr(test, allow(dead_code))]
fn console_demo(vfs: Endpoint) {
    let fd = vfs_open(vfs, DEV_CONSOLE_PATH);
    if fd < 0 {
        return report_line(vfs, b"minix.rs init: dev.console FAIL open");
    }
    let line = b"minix.rs init: dev.console ok\n";
    let n = vfs_write(vfs, fd, line);
    let closed = vfs_close(vfs, fd);
    if n != line.len() as i32 {
        return report_line(vfs, b"minix.rs init: dev.console FAIL write");
    }
    if closed != OK {
        report_line(vfs, b"minix.rs init: dev.console FAIL close");
    }
}
```

- [ ] **Step 2: `open_denials` grows, and the `read-console` comment is corrected**

First array:

```rust
    for (name, path, want) in [
        ("no-such", "/no-such-file", ENOENT),
        ("is-dir", "/etc", EISDIR),
        // Slice 5.11: a `/dev` path the device table does not know. It must fall
        // through to MFS — where there is no `/dev` at all — and answer `ENOENT`
        // from the walk. A table that claimed the whole prefix would answer
        // something else, and a table that matched by prefix would open null.
        ("dev-no-such", "/dev/nope", ENOENT),
    ] {
```

The console-read block:

```rust
    // A console descriptor cannot be read from *yet*: since slice 5.11 VFS routes
    // the read to TTY as a real `CDEV_READ`, and TTY answers `ENOSYS` from its
    // unknown-request arm until Phase 6 gives it RX. Same errno as before the
    // slice, now the driver's answer rather than VFS's guess about it.
    let mut buf = [0u8; 8];
    if vfs_read(vfs, STDOUT, &mut buf) == ENOSYS {
```

`OPEN_DENIAL_PROBES` 11 → 12, its `const _` assert to 12, the final line to `b"minix.rs init: open.deny ok n=12\n"`. In the doc comment above `open_denials`, replace the `read-console` bullet with "`read-console` — `read()` on fd 1. `ENOSYS`, not `EBADF`: the descriptor is good, and TTY does not serve `CDEV_READ` until Phase 6, answering it from its unknown-request arm (slice 5.11 made VFS send the request instead of guessing)." and add a bullet "`dev-no-such` — `/dev/nope`. `ENOENT` from MFS's walk: the device table does not claim the `/dev` prefix, only three exact paths."

- [ ] **Step 3: Build, lint, boot stub-free, read the markers**

```bash
cargo fmt --all
cargo clippy -p minixrs-init --all-targets -- -D warnings
MINIXRS_SDK=/nonexistent timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release --no-default-features > /private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad/t6.log 2>&1
grep -a 'error\[E' /private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad/t6.log
grep -a 'dev\.\|open.deny\|mem\.' /private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad/t6.log
```
Expected: no compile errors; `dev.zero ok n=64`, `dev.null ok n=35`, `dev.console ok`, `open.deny ok n=12`, `mem.ds ok`, `mem.deny ok n=5`; no `FAIL`.

- [ ] **Step 4: Commit**

```bash
git add userland/init/src/main.rs
git commit -s -m "init(5.11): prove /dev/zero, /dev/null and /dev/console; open.deny 11 -> 12

zero: 64 bytes read, all zero, a second read is not EOF. null: the
35-byte hello is accepted whole, a read is EOF and touches nothing.
console: the marker is written through the /dev/console descriptor, not
fd 1. dev-no-such proves the table does not claim the /dev prefix, and
the read-console probe's comment now says who answers ENOSYS.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 7: Marker files and the default-config boot

**Files:**
- Modify: `tests/qemu-boot.expected` (after `[diag vfs] cdev.deny ok n=2` for the VFS prologue lines; after `fs.deny ok n=14`'s block for `mem.deny`; after `fs.fd ok match=1` for the three `dev.*` lines; `open.deny ok n=11` → `n=12` with its commentary)
- Modify: `tests/qemu-boot.forbidden`

- [ ] **Step 1: Add the markers with commentary**

After the `[diag vfs] cdev.deny ok n=2` entry, insert:

```
# ---------------------------------------------------------------------------
# Slice 5.11: /dev/null and /dev/zero on the memory driver, and CDEV_READ.
# ---------------------------------------------------------------------------

# VFS resolves the memory driver through DS -- the third link in the chain
# `ds < tty < memory < mfs < vfs`, which is packing order in kernel/build.rs and
# nothing stronger. A failed lookup falls back to the boot endpoint and prints
# `mem.ds FAIL` instead, so every device marker below stays alive and only this
# one disappears: an ordering regression turns CI red here specifically.
[diag vfs] mem.ds ok ep=
```

After the `[diag vfs] fs.deny ok n=14` entry (and its comment block), insert:

```
# The memory driver's CDEV refusals, probed from VFS's prologue because VFS's own
# device table can never send them (it maps only minors that exist). Five, each
# well-formed but for one thing: minor 7 on a write and on a read (ENXIO from the
# driver's classify, on both requests independently), a negative length (EINVAL),
# GRANT_INVALID (EINVAL), and a CDEV_READ through a grant carrying only CPF_READ
# -- the driver passes it to SYS_SAFECOPY(SAFECOPY_TO) in good faith and the
# KERNEL refuses the direction (EPERM, relayed verbatim). That last one is the
# first CPF_WRITE-required refusal any boot marker exercises. Drop a check and
# its probe succeeds, turning this into `mem.deny FAIL <name>`.
[diag vfs] mem.deny ok n=5
```

After the `minix.rs init: fs.fd ok match=1` entry, insert:

```
# /dev/zero through the whole POSIX path: open by path (VFS's device table,
# consulted after the path copy and BEFORE the mount -- a device needs no
# filesystem), then a 64-byte read that must come back whole and all-zero into a
# buffer poisoned with 0xA5, then a SECOND read that must also be 64 rather than
# EOF. The second read is what separates zero from null: swap the two minors in
# VFS's table and this prints `dev.zero FAIL short`. Clamp the driver's read and
# it prints the same. n= is the constant, pinned by a const assert.
minix.rs init: dev.zero ok n=64

# /dev/null: the 35-byte hello line is written and the WHOLE count must come
# back in one round (the memory driver does not clamp -- there is no staging
# buffer to protect), then a read must be EOF and must leave the poisoned buffer
# untouched. A write here issues no copy at all, which is why no bad-buffer
# probe may ever be aimed at /dev/null: it would succeed.
minix.rs init: dev.null ok n=35

# /dev/console: this line is written THROUGH the descriptor that open("/dev/
# console") returned, not through fd 1. Printing it on fd 1 after the open would
# prove only that a number came back; routing the table's console row to the
# memory driver makes this line vanish (discarded by null, or ENXIO), which is
# the proof that the row points at TTY.
minix.rs init: dev.console ok
```

Change `minix.rs init: open.deny ok n=11` to `n=12` and append to its comment block:

```
#
# Slice 5.11 adds `dev-no-such`: open("/dev/nope") must fall through the device
# table to MFS and answer ENOENT from the walk of a /dev that does not exist --
# a table that claimed the whole prefix, or matched by prefix, fails this. The
# read-console probe keeps ENOSYS, but the answer now comes from TTY's
# unknown-request arm over a real CDEV_READ rather than from VFS short-circuiting.
```

In `tests/qemu-boot.forbidden`, append:

```
# Slice 5.11: a device probe ran and disagreed with itself. Distinct from the
# marker simply going missing (init never got that far): these spellings mean the
# open, read, write, or close executed and produced the wrong answer, and the
# word after FAIL names the step -- `dev.zero FAIL short` is null answering for
# zero (or a clamp), `dev.null FAIL touched` is a read that wrote, `dev.console
# FAIL write` is the table's console row not reaching TTY.
minix.rs init: dev.zero FAIL
minix.rs init: dev.null FAIL
minix.rs init: dev.console FAIL
# ...and VFS's prologue battery against the memory driver's CDEV validator.
[diag vfs] mem.deny FAIL
```

- [ ] **Step 2: Default-config boot and the checker**

```bash
S=/private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release 2>&1 | tail -1
MINIXRS_SDK=/nonexistent timeout 300 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > $S/t7.log 2>&1
cp $S/t7.log $S/t7.check.log
tools/check-boot-log.sh $S/t7.check.log | tail -5
```
Expected: `PASS` on every marker (the total grows from 97 to 102), `FORBIDDEN: none`. If a marker is MISSING, `grep -a 'error\[E'` first.

- [ ] **Step 3: Measure the boot-budget ratio against the merge base**

```bash
S=/private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad
# after (the log above):
A=$(grep -abo 'minix.rs hello: errno ok' $S/t7.log | head -1 | cut -d: -f1); T=$(wc -c < $S/t7.log); echo "after: $A / $T = $(echo "scale=4; $A/$T" | bc)"
# before: detach to the merge base, stash nothing (docs edits are committed), rebuild, boot
git stash list  # must be empty
git checkout --detach $(git merge-base HEAD origin/main)
MINIXRS_SDK=/nonexistent cargo build -p minixrs-kernel --target aarch64-unknown-none --release 2>&1 | tail -1
MINIXRS_SDK=/nonexistent timeout 300 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > $S/base.log 2>&1
B=$(grep -abo 'minix.rs hello: errno ok' $S/base.log | head -1 | cut -d: -f1); TB=$(wc -c < $S/base.log); echo "before: $B / $TB = $(echo "scale=4; $B/$TB" | bc)"
git checkout feature/slice-5.11-dev-null-zero
```
Expected: the two fractions within a couple of percentage points of each other (the slice adds roughly a dozen IPC round trips). Record both numbers in the Task 9 ledger; if the ratio jumped by more than 1.1×, stop and report before continuing.

- [ ] **Step 4: Commit**

```bash
git add tests/qemu-boot.expected tests/qemu-boot.forbidden
git commit -s -m "tests(5.11): the device markers -- dev.zero/null/console, mem.ds, mem.deny

Five new required markers, open.deny 11 -> 12, and the FAIL spellings
forbidden. Verified on the default config (102 markers PASS, none
forbidden); boot ratio measured against the merge base.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 8: Docs, trackers, and the falsified-claim sweep

**Files:**
- Modify: `book/src/drivers/overview.md:170-203` (CDEV protocol), `:205` (TTY intro), after `:345-368` (a new `### The character minors` under the memory section)
- Modify: `book/src/servers/overview.md:210-227` (descriptor table), `:229` (read path — one sentence)
- Modify: `book/src/reference/syscalls.md:211` (CDEV row; also fix the stale VFS row while there)
- Modify: `docs/plan.md:537-538`, `docs/plans/phase-5-musl-fs.md:1483` (5.10b header), `:1555-1565` (5.11 entry), `:657` (5.3 text), `:307` (D11 line)
- Modify: `CLAUDE.md:355` (the 5.3 sentence) and a new 5.11 bullet after the 5.10b ones
- Modify: any file the sweep in Step 4 finds

- [ ] **Step 1: The book's driver chapter**

In `book/src/drivers/overview.md`, CDEV section: change "Phase 5 defines one:" to "Phase 5 defines two, sharing one payload:", and in the table's grant-id row "names the client's source buffer" → "names the client's buffer (`CPF_READ` for a write, `CPF_WRITE` for a read)". Replace the final paragraph ("`CDEV_READ` is deliberately absent…") with:

```markdown
**`CDEV_READ` is the same payload, copy reversed** (slice 5.11). The reply is
the byte count read, `0` is EOF, and a short read is legal — POSIX `read()`'s
contract and the one VFS already assumes for `FS_READ`, so VFS sends one request
and reports what came back. It existed only as a plan note until `/dev/zero`
needed it: the 5.3 text said the two devices would be "new minors, not new
requests", which is true of `/dev/null` and of writing `/dev/zero` and false of
reading it. TTY does not serve it until Phase 6 gives it RX (`SYS_IRQCTL`), and
answers it `ENOSYS` from its unknown-request arm until then — VFS routes a
console `read()` there anyway, so Phase 6 changes TTY and nothing else.

**Minors are a per-driver namespace.** TTY's console is 0; the memory driver's
`/dev/null` and `/dev/zero` are CDEV minors 3 and 5 (MINIX 3's `NULL_DEV` and
`ZERO_DEV`), on the same driver as BDEV minor 0's ramdisk. The request band, not
the minor value, tells them apart.
```

Under `## TTY`, in the `cdev.rs` bullet, "`parse_write` reads the four payload fields, `validate_write` applies…" → "`validate_write` applies… (the four-field parse moved to `server-rt::cdev` in 5.11, when the memory driver became its second user)".

After the memory section's `### The driver has no `unsafe` block` subsection, add:

```markdown
### The character minors

Since slice 5.11 the same driver serves `/dev/null` (CDEV minor 3) and
`/dev/zero` (minor 5), as MINIX 3's memory driver does beside its ramdisks. They
share the driver with the BDEV ramdisk but not a namespace — a minor is per
request band — so `cdev::classify` refuses minor 0 here, which is TTY's console.

Both minors discard a `CDEV_WRITE` and answer the **whole** count with no copy
at all; `/dev/null` answers a `CDEV_READ` with `0`, and `/dev/zero` fills the
whole request from a 256-byte static, walking the grant in `CDEV_MAX_IO` steps.
Nothing is clamped: `CDEV_MAX_IO` protects TTY's stack staging buffer, and there
is no staging here. Two consequences worth knowing. A `/dev/null` write with an
unmapped buffer *succeeds*, as it does on Linux, because nothing reads the
buffer — so no bad-buffer probe may ever be aimed at it. And the driver still
has no `unsafe` block: both arms are kernel calls.

VFS probes the validator from its prologue (`[diag vfs] mem.deny ok n=5`),
because VFS's own device table maps only minors that exist and could never send
a bad one. One of those five is the first `CPF_WRITE`-required refusal any boot
marker exercises: a `CDEV_READ` through a read-only grant, refused by the kernel's
`verify_grant` and relayed as `EPERM`.
```

- [ ] **Step 2: The book's servers chapter and the syscalls reference**

In `book/src/servers/overview.md`, `### The descriptor table`: after the sentence ending "lets init write before any filesystem exists.", add:

```markdown
Since slice 5.11 a character-device entry names its **driver** as well as its
minor (`Fd::CharDev { dev: CharDriver, minor }`, with `CharDriver` an enum
because the default row is a `const` and a DS-resolved endpoint is not) — minors
are a per-driver namespace, so the driver is half the address. `open` consults a
three-row **device-node table** (`servers/vfs/src/dev.rs`: `/dev/console`,
`/dev/null`, `/dev/zero`, matched byte-for-byte) after copying the path in and
*before* touching the mount, so a device open needs no filesystem; `O_CREAT` and
`O_TRUNC` are ignored on a hit, Linux's behaviour for a device node. Everything
else falls through to MFS, `/dev/other` included, and there is no `/dev` on the
image at all. A device `read()` is one `CDEV_READ` against the descriptor's
driver, no loop and no position; a console `read()` therefore reaches TTY and
hears `ENOSYS` from its unknown-request arm until Phase 6.
```

In `book/src/reference/syscalls.md`'s band table: `CDEV_RQ_BASE` row → "character drivers: `WRITE` (slice 5.3) / `READ` (5.11)"; and while there, the `VFS_RQ_BASE` row says only `WRITE (slice 5.4)` — correct it to "VFS: `WRITE` / `OPEN` / `READ` / `CLOSE` / `EXEC_STAGE`" and the `(reserved) 0x900` row to "`FS_RQ_BASE` | `0x900` | MFS: `READSUPER` / `LOOKUP` / `READ` / `WRITE` / `CREATE` / `TRUNC`" — stale since 5.8, caught by this sweep.

- [ ] **Step 3: Trackers and CLAUDE.md**

`docs/plan.md`: the 5.10b line becomes `✓ shipped (PR #54, merged 2026-09-02)`; the 5.11 line becomes `◀ ready (branch `feature/slice-5.11-dev-null-zero`, pending merge)` and gains the correction: `- **5.11** stretch: `/dev/null` + `/dev/zero` on the memory driver + `CDEV_READ` ◀ ready (…)`.

`docs/plans/phase-5-musl-fs.md`: the 5.10b `####` header (line 1483) → `✓ shipped (PR #54, merged 2026-09-02)`. Replace the 5.11 entry (`### Slice 5.11 (stretch) …` through its `**Proof:**` paragraph) with:

```markdown
#### Slice 5.11 (stretch): `/dev/null` + `/dev/zero` + `CDEV_READ` ◀ ready (branch `feature/slice-5.11-dev-null-zero`, pending merge)

Full design — decisions `Z1…Z10`, the per-component breakdown, the error
taxonomy, and the mutation plan — lives in
[`docs/superpowers/specs/2026-09-05-dev-null-zero-design.md`](../superpowers/specs/2026-09-05-dev-null-zero-design.md)
and is not duplicated here.

**Scope, as shipped:** `CDEV_READ` (`CDEV_RQ_BASE + 1`, `NR_CDEV_MSGS` 1 → 2;
`CDEV_WRITE`'s payload with the copy reversed, `0` is EOF, short reads legal);
`CDEV_MINOR_NULL = 3` / `CDEV_MINOR_ZERO = 5` (MINIX 3's values) served by the
memory driver with no clamp; the CDEV request codec lifted into `server-rt`;
VFS's `Fd::CharDev` names its driver and a three-row device-node table
intercepts `/dev/console`, `/dev/null`, `/dev/zero` after the path copy and
before the mount; a console `read()` now reaches TTY and hears `ENOSYS` from its
unknown-request arm. **The 5.3 note that 5.11 would be "new minors, not new
requests" was wrong for reading `/dev/zero`**, and is corrected wherever it was
copied.

**Proof:** `dev.zero ok n=64` (64 bytes, all zero, a second read not EOF),
`dev.null ok n=35` (whole count accepted, read is EOF and touches nothing),
`dev.console ok` (written *through* the `/dev/console` descriptor), `mem.ds ok`,
`mem.deny ok n=5`, `open.deny` 11 → 12 (`/dev/nope` → `ENOENT`).
```

At line 657 (`5.3`'s "`CDEV_READ` is deliberately absent (Phase 6).") → "`CDEV_READ` was absent until 5.11 defined it for `/dev/zero`; TTY serves it in Phase 6." At the D11 line: "`/dev/null` + `/dev/zero` via the memory driver's CDEV minors (5.11)" → "`/dev/null` + `/dev/zero` via the memory driver's CDEV minors, plus the `CDEV_READ` request reading zero needs (5.11)".

`CLAUDE.md` line 355: replace "`CDEV_READ` is absent until Phase 6 (RX needs `SYS_IRQCTL`); 5.11's `/dev/null`/`/dev/zero` are new **minors**, not new requests." with "`CDEV_READ` was absent until slice 5.11 defined it (reading `/dev/zero` needs it; TTY serves it in Phase 6, when RX gets `SYS_IRQCTL`)." Then add, after the last 5.10b bullet in the Code Conventions list:

```markdown
- **`/dev/null`, `/dev/zero`, and `CDEV_READ` (slice 5.11):** `CDEV_READ = CDEV_RQ_BASE + 1`
  (`NR_CDEV_MSGS` 1 → 2) is `CDEV_WRITE`'s payload with the copy reversed — the grant carries
  `CPF_WRITE`, the driver pushes with `SAFECOPY_TO`, **`0` is EOF and a short read is legal**, so
  VFS sends one request and never loops (the `FS_READ` stance). The 5.3 plan text saying 5.11
  would be "minors, not requests" was wrong for *reading* zero; four in-tree copies of it were
  corrected. `/dev/null` and `/dev/zero` are **CDEV minors 3 and 5 of the memory driver**
  (MINIX 3's `NULL_DEV`/`ZERO_DEV`), and **minors are a per-driver namespace**: the same driver's
  ramdisk is BDEV minor 0, the request band tells them apart, and nothing asserts `CDEV_MINOR_*`
  against `BDEV_MINOR_*`. The memory driver **never clamps** — `CDEV_MAX_IO` protects TTY's
  stack staging buffer and there is no staging here — so a null/zero write answers the whole
  count with **no copy at all** (an unmapped buffer *succeeds*, Linux's behaviour: never aim a
  `bad-buf` probe at `/dev/null`), and a zero read fills the whole request from a 256-byte
  static in `CDEV_MAX_IO` steps, reporting partial progress on a mid-way failure (5.4's rule).
  The four-field CDEV parse lives in **`server-rt::cdev`** now that two drivers decode it;
  validation stays per driver. VFS's `Fd::CharDev { dev: CharDriver, minor }` names its driver
  (an enum, not an `Endpoint`, because `DEFAULT_ROW` is a `const`), and `servers/vfs/src/dev.rs`
  is a three-row **device-node table** consulted **after the path copy and before the mount**
  (a device open needs no filesystem; `O_CREAT`/`O_TRUNC` ignored on a hit; exact byte match, no
  `/dev` on the image, `/dev/other` falls through to MFS's `ENOENT`). **A console `read()` is
  now a real `CDEV_READ` to TTY**, which answers `ENOSYS` from its unknown-request arm until
  Phase 6 — same errno init's `read-console` probe always expected, now the driver's answer
  rather than VFS's guess, so Phase 6 adds one TTY arm and touches VFS not at all. The memory
  driver's validator is probed from VFS's prologue (`mem.deny ok n=5`, last, after `fs.deny`),
  including the first `CPF_WRITE`-required kernel refusal any marker exercises. init's
  `dev.console ok` is written **through** the `/dev/console` descriptor, never fd 1: that is the
  only thing that proves the table row points at TTY. Paths are `callnr::DEV_*_PATH` so init and
  VFS cannot drift.
```

- [ ] **Step 4: The falsified-claim sweep**

```bash
grep -rn 'no .CDEV_READ\|CDEV_READ. is deliberately absent\|CDEV_READ. is absent\|not new request\|new \*minors\*\|new \*\*minors\*\*\|only the console\|Only .CDEV_MINOR_CONSOLE\|until slice 5.11\|until 5.11\|Slice 5.11.s\|WriteRequest\|parse_write\|does not exist until Phase 6' \
  --include='*.rs' --include='*.md' . | grep -v '^./target' | grep -v '^./external' | grep -v 'docs/superpowers/'
```

Every hit is either already rewritten above or must be rewritten now. Known sites at branch time: `servers/vfs/src/main.rs:694` (deleted by Task 5 — confirm), `servers/vfs/src/fd.rs` module note (Task 4 — confirm), `docs/plans/phase-5-musl-fs.md:657` and the D11 line (Step 3), `book/src/drivers/overview.md` (Step 1), `CLAUDE.md:355` (Step 3), `drivers/tty/src/cdev.rs` (Task 2 — confirm), `kernel-shared/src/callnr.rs:716,1104-1106,1124-1125` (Task 1 — confirm), `userland/init/src/main.rs:499` (Task 6 — confirm). `fs/mfs/src/walk.rs:18`'s "on the `CDEV_READ` precedent that a request without a consumer is better absent than stubbed" stays true as a statement about the precedent — reword only if it claims the request is *still* absent. `PRE6-RECOMMEND.md` is untracked and not part of this branch; leave it.

Then the count-tripwire sweep:

```bash
grep -rn 'assert_eq!(.*\.len(), [0-9]' kernel-shared/src/callnr.rs tools/gen-c-headers/src/callnr_h.rs servers/vfs/src userland/init/src drivers/memory/src drivers/tty/src | grep -v NR_
```
Every literal count must have been grown by the task that added the thing it counts (`OPEN_DENIAL_PROBES`, the 13-entry define list, `NR_DEV_NODES`).

- [ ] **Step 5: Build the book, commit**

```bash
mdbook build book 2>&1 | tail -2   # via the mdbook-preview skill if mdbook is not installed
git add book/src docs/plan.md docs/plans/phase-5-musl-fs.md CLAUDE.md $(git diff --name-only)
git commit -s -m "docs(5.11): book, trackers, CLAUDE.md, and the CDEV_READ claim sweep

The CDEV chapter gains the read request and the per-driver minor
namespace; the memory section gains its character minors; VFS's table
and fd shape are documented. 5.10b flips to shipped, 5.11 to ready with
the spec linked. Every 'CDEV_READ is absent / 5.11 is minors only' copy
is rewritten, and the stale VFS/FS rows in the band reference are fixed.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_017HuYDtsgEsaK3EitFNiaZP"
```

---

### Task 9: Mutation matrix, whole-branch review, final gates

**Files:**
- Read-only over the whole branch; the ledger in the scratchpad (`$S/ledger.md`)

- [ ] **Step 1: Snapshot every file the matrix mutates**

```bash
S=/private/tmp/claude-501/-Users-kevinbarnard-src-minixrs/d473a02b-2a5d-499a-b52d-798a03065536/scratchpad
mkdir -p $S/snap
for f in servers/vfs/src/dev.rs servers/vfs/src/main.rs drivers/memory/src/cdev.rs drivers/memory/src/main.rs; do mkdir -p $S/snap/$(dirname $f); cp $f $S/snap/$f; done
git status --short   # must show nothing but the untracked PRE6-RECOMMEND.md / .gemini / .claude edits that predate the branch
```

- [ ] **Step 2: Run each mutation stub-free, record the marker that moved, restore from the snapshot**

For each row, apply the edit with a `// MUTATION` comment, boot with `MINIXRS_SDK=/nonexistent timeout 60 cargo run … --no-default-features > $S/m<N>.log 2>&1`, `grep -a 'error\[E' $S/m<N>.log` (must be empty), grep the named marker, then `cp $S/snap/<file> <file>`.

| # | Mutation | Grep | Expected |
|---|---|---|---|
| 1 | `dev.rs`: delete the `DEV_ZERO_PATH` row (and drop `NR_DEV_NODES` to 2) | `dev.zero` | `dev.zero FAIL open` |
| 2 | `dev.rs`: swap the minors on the null and zero rows | `dev.zero\|dev.null` | `dev.zero FAIL short` and `dev.null FAIL write` or `FAIL read` |
| 3 | `memory/cdev.rs` `zero_chunk`: `.min(16)` and `main.rs` `do_cdev_read`: `return 16` after the first chunk (a 64-byte probe cannot see a 256 clamp) | `dev.zero` | `dev.zero FAIL short` |
| 4 | `memory/cdev.rs` `classify`: `_ => Ok(Minor::Null)` | `mem.deny` | `mem.deny FAIL bad-minor-w` |
| 5 | `dev.rs`: console row → `CharDriver::Memory` | `dev.console` | line absent (and possibly `dev.console FAIL write` on fd 2 if ENXIO) |
| 6 | `vfs/main.rs` `do_open`: move the `dev::lookup` block below `ensure_mounted` | all `dev.*` | **no marker moves** — record as unproven |
| 7 | `vfs/main.rs` `do_read`: restore `Ok(Fd::CharDev { dev: CharDriver::Tty, .. }) => return ENOSYS` before routing | `open.deny` | **no marker moves** — record as unproven |

After the last restore:

```bash
for f in servers/vfs/src/dev.rs servers/vfs/src/main.rs drivers/memory/src/cdev.rs drivers/memory/src/main.rs; do diff -q $S/snap/$f $f; done
grep -rn MUTATION --include='*.rs' . | grep -v '^./target'
git status --short
```
Expected: no diffs, no `MUTATION` hits, a clean tree.

- [ ] **Step 3: Host and lint gates, the full matrix**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
MINIXRS_SDK=/nonexistent cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D warnings
MINIXRS_SDK=/nonexistent cargo clippy -p minixrs-kernel --target aarch64-unknown-none --no-default-features -- -D warnings
cargo clippy -p minixrs-mfs --features server -- -D warnings
cargo test --workspace 2>&1 | grep -E 'test result|FAILED' 
cargo gen-c-headers && clang -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsyntax-only -ffreestanding -nostdlibinc --target=aarch64-unknown-linux-musl -Itarget/gen-c-headers/include target/gen-c-headers/abi-selftest.c && echo C-OK
tools/check-dco.sh
```
Expected: all clean, every `test result: ok`, `C-OK`, DCO passes on every branch commit.

- [ ] **Step 4: Whole-branch review**

Write the diff to a file and hand it to a fresh reviewer (subagent) with three explicit asks: verify the arithmetic in `do_cdev_read`'s loop and `zero_chunk` by hand; check every doc comment and test the branch touched against what a *later* task in the branch added (the 5.10b defect class); and re-read `book/src/drivers/overview.md` and `book/src/servers/overview.md` against the code.

```bash
git diff $(git merge-base HEAD origin/main)..HEAD > $S/branch.diff
wc -l $S/branch.diff
```

Fix every confirmed finding in a `fix(5.11): review findings — …` commit, re-running Steps 2–3 for anything the fix touches.

- [ ] **Step 5: Pre-PR checklist — then stop**

Run `/claude-md-management:revise-claude-md` (the 5.11 bullet was written in Task 8; this pass checks whether the *conventions* sections need anything from this session). Then **stop**: do not push, do not open a PR. Report the branch, the ledger (every ruling made on the user's behalf, with what it costs if wrong), the boot-ratio numbers from Task 7, the mutation table with its two unproven rows, and the review findings.

---

## Self-review against the spec

- §4.1 band + docs + tripwires → Task 1. §4.2 headers → Task 1. §4.3 codec → Task 2.
- §5.1 TTY → Task 2. §5.2 memory driver → Task 3 (`classify`, `validate`, `zero_chunk`, `ZEROS`, both arms, partial-progress rule, `checked_add`).
- §5.3 fd → Task 4. §5.4 dev table → Task 4 (paths moved to `kernel-shared` — a plan-level refinement of the spec's `&[u8]` literals, recorded in Task 1's `DEV_*_PATH` doc; the spec's "exact byte match" contract is unchanged).
- §5.5 do_open → Task 5 step 4. §5.6 do_read/do_write/endpoints → Task 5 steps 1–3, 5. §5.7 mem_denials → Task 5 step 6.
- §5.8 dev_demo → Task 6 step 1. §5.9 open_denials → Task 6 step 2.
- §8.1 host → Tasks 1–6 + Task 9 step 3. §8.2 boot → Task 7. §8.3 mutations → Task 9 step 2. §8.4 sweep → Task 8 step 4.
- §10 needs no task. Spec Z1–Z10 each cited at its implementing step.

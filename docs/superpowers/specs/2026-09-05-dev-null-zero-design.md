# Slice 5.11 — `/dev/null`, `/dev/zero`, and `CDEV_READ`

**Date:** 2026-09-05
**Status:** design, pending review
**Branch:** `feature/slice-5.11-dev-null-zero`
**Predecessor:** slice 5.10b (create/truncate + `VFS_OPEN` flags, PR #54, merged 2026-09-02)

Phase 5's milestone is met and both 5.10 halves have shipped. This is the last
stretch slice the phase plan names: two character devices that need no hardware,
served by the `memory` driver the way MINIX 3's own memory driver serves them,
and reachable by path through VFS.

The plan's 5.3 notes said 5.11 would be "new *minors*, not new request
numbers". That is true of `/dev/null` and of *writing* `/dev/zero`. It is false
of *reading* `/dev/zero`: a driver has to fill the caller's buffer, and no
request exists that asks a character driver to do that. So this slice defines
`CDEV_READ`. The plan is corrected rather than worked around (§3, Z1).

The design decisions are labelled `Z1…Z10`, continuing 5.10a's `W…` and 5.10b's
`C…` convention: slice-local, distinct from the phase-level `D1…D13`, which are
locked.

---

## 1. What exists, and what the slice changes

| Component | Today | After 5.11 |
|---|---|---|
| `kernel-shared` `callnr.rs` | CDEV band is `CDEV_WRITE` alone (`NR_CDEV_MSGS = 1`); one minor, `CDEV_MINOR_CONSOLE` | `+ CDEV_READ` (2); `+ CDEV_MINOR_NULL = 3`, `+ CDEV_MINOR_ZERO = 5` |
| `tools/gen-c-headers` | emits `CDEV_WRITE`, `CDEV_MINOR_CONSOLE` | `+ CDEV_READ` row, `+` both minors |
| `server-rt` | `rd_i32`/`rd_u64` payload accessors | `+ cdev.rs`: the shared four-field CDEV request codec |
| `drivers/tty` | `cdev.rs` parses and validates `CDEV_WRITE` | parse moves to `server-rt`; `validate_write` unchanged; unknown-request arm still answers `CDEV_READ` with `ENOSYS` |
| `drivers/memory` | BDEV only (`BDEV_READ`/`BDEV_WRITE` on minor 0) | `+ cdev.rs` (pure) and `CDEV_WRITE`/`CDEV_READ` arms for minors 3 and 5 |
| `servers/vfs` `fd.rs` | `Fd::CharDev { minor }` — implicitly TTY | `Fd::CharDev { dev: CharDriver, minor }`, `enum CharDriver { Tty, Memory }` |
| `servers/vfs` | `do_open` always resolves through MFS; `do_read` on a `CharDev` is a local `ENOSYS` | `+ dev.rs`: static device-node table consulted ahead of the mount; `do_read` sends `CDEV_READ`; `do_write` routes by driver; `+ mem.ds` lookup; `+ mem.deny` battery |
| `userland/init` | read / write / create / exec / denial batteries | `+ dev_demo` (zero, null, console); `open.deny` 11 → 12 |
| `tests/qemu-boot.*` | 97 expected markers | `+ dev.zero`, `dev.null`, `dev.console`, `mem.ds ok`, `mem.deny ok`; `open.deny ok n=12`; three `FAIL` spellings forbidden |
| `book/` | `drivers/overview.md` says `CDEV_READ` is absent and 5.11 is minors-only | CDEV section, memory-driver section, VFS fd/device-table section rewritten |

Four standing facts constrain what follows.

**The CDEV payload carries no granter and no way to name one.** The driver takes
the granter from the kernel-stamped `m_source` (the 5.2/5.3 confused-deputy
rule). `CDEV_READ` inherits that: it is `CDEV_WRITE`'s payload with the copy
running the other way.

**A server stack is exactly one page.** Anything a handler needs across a
kernel call is a `main`-frame local or a static, never a large local. The zero
source is a static (§5.2).

**VFS does not loop on read.** POSIX allows a short `read()`; `FS_READ` is
short at EOF regardless; `do_read` sends one request and reports what came
back. `CDEV_READ` is designed to that contract (Z2).

**DS publish-before-retrieve is ordering by MXBI position only.** The chain is
`ds < tty < memory < mfs < vfs`. VFS's new lookup of `memory` is satisfied by
the same packing order that already satisfies MFS's, and like every other
lookup it falls back to `boot_endpoint` with a distinguishable diag line.

---

## 2. Scope

**In:**

- `CDEV_READ`, one new CDEV-band request, and two new minors.
- The memory driver serving both minors for both requests.
- VFS: the driver discriminator on `Fd::CharDev`, the device-node table, the
  read routing, the DS lookup, the denial battery.
- init's proof, the marker files, the header generator, the book, the trackers.
- One refactor in passing: the CDEV request codec lifted into `server-rt`
  because a second driver now decodes it (Z9).

**Out (with owners):**

- musl `open`/`read`/`close` wrappers and any C-side probe. `VFS_OPEN`,
  `VFS_READ`, `VFS_CLOSE` already exist, so the fork can add them later with no
  ABI bump; doing it here would pair a cross-repo PR with this one for no
  Phase-5 gain. → **a Phase 6/7 slice that needs a C program to open a file.**
- TTY RX. `CDEV_READ` now exists; TTY does not serve it. → **Phase 6, with
  `SYS_IRQCTL`.** That slice adds one arm to TTY's receive loop and nothing to
  VFS.
- An on-image `/dev` directory, device inodes, `mknod`, path normalisation
  (`/dev//null`, `/dev/./null`). Exact-match interception is the deliberate
  simplification D11 names. → **whenever a real `/dev` is wanted, likely with
  the disk root in Phase 6.**
- `/dev/mem`, `/dev/kmem`, `/dev/random`, `/dev/full`. Nothing consumes them.
- `lseek` on a device, `O_NONBLOCK`, `ioctl`. musl's `ioctl` stays `-ENOTTY`.

---

## 3. Decisions

**Z1. `CDEV_READ` is a real request, not a special-cased minor.** Reading
`/dev/zero` needs the driver to write into the caller's buffer through a
`CPF_WRITE` grant; no existing request asks a character driver for that.
Faking it in VFS (a `SYS_COPY` from a VFS zero buffer) would make VFS a device,
leave the memory driver's CDEV arm unbuilt, and still leave Phase 6 to add
`CDEV_READ` for TTY. The 5.3 plan text is corrected everywhere it was copied
(§8.4).

**Z2. `CDEV_READ` may be short, and a reply of 0 is EOF.** `FS_READ`'s contract
and the one `do_read` already assumes: VFS sends one request and reports the
count. `/dev/null` answers 0 on every read; `/dev/zero` never answers 0 for a
positive `len`. TTY, in Phase 6, may answer short when the RX FIFO has fewer
bytes than asked.

**Z3. Minors take MINIX 3's values: `NULL_DEV = 3`, `ZERO_DEV = 5`.** From
`include/minix/dmap.h`. Costs nothing and keeps the memory driver's minor map
recognisable to anyone reading `drivers/storage/memory/memory.c`. Minors are a
**per-driver** namespace: TTY's console is 0, the memory driver's ramdisk is
BDEV minor 0, and these two are CDEV minors 3 and 5 on the same driver. The
request band tells the ramdisk and the character minors apart, never the minor
value — and `BDEV_MINOR_RAMDISK` / `CDEV_MINOR_NULL` colliding numerically
would be fine, which is why nothing asserts they differ.

**Z4. The memory driver never clamps.** `CDEV_MAX_IO` exists because TTY stages
through a 256-byte stack buffer. `/dev/null` moves nothing, and `/dev/zero`'s
source is a constant, so both reply the full `len` in one round: a write
discards `len` bytes with no copy at all, and a read of zero loops
`SYS_SAFECOPY` over a 256-byte all-zero static at advancing grant offsets until
`len` is out. The chunk size is `CDEV_MAX_IO` because it is the constant that
already exists, not because anything here is staged.

**Z5. `Fd::CharDev` names its driver.** `CharDev { dev: CharDriver, minor }`
with `enum CharDriver { Tty, Memory }`, VFS-local. An `Endpoint` in the variant
was rejected: `DEFAULT_ROW` is a `const`, and a DS-resolved endpoint is a
runtime value. The enum is resolved to an endpoint by one function in `main.rs`
that owns both resolved peers.

**Z6. The device-node table runs after the path copy and before the mount.** A
device open must not need a filesystem — that is what makes the devices
synthetic — and the path has to be read before it can be matched. Today
`ensure_mounted` precedes the `SYS_COPY`; reordering changes no observable
errno at boot (the mount succeeds before init runs) and is the honest order.
On a table hit `O_CREAT` and `O_TRUNC` are **ignored**, the Linux behaviour for
a device node: `O_CREAT` on an existing name is a plain open, and truncating a
device has no meaning. Every other flag rule (`validate_flags`) still runs
first, so an unknown bit is `EINVAL` on a device path too. A miss falls through
unchanged, so `/dev/other` reaches MFS and answers `ENOENT` from the walk of a
`/dev` that does not exist.

**Z7. `do_read` routes every `CharDev` to its driver, TTY included.** VFS keeps
no per-driver capability table. A `read()` on the console sends `CDEV_READ` to
TTY, whose unknown-request arm answers `ENOSYS` — the same errno the local
short-circuit answers today, so init's `read-console` probe keeps its expected
value while its comment stops being true and is rewritten. Phase 6 then
changes TTY and nothing else. The one extra IPC round trip per console read
is paid by no boot path except that probe.

**Z8. The memory driver's denials are probed from VFS's prologue.** `ENXIO`
for a bad minor can never travel through VFS's table (it maps only known
minors), so like TTY's `cdev.deny` and MFS's `bdev.deny` the battery aims raw
requests at the driver from a server that can grant: `mem.deny`, direct grants
over a VFS local, run **last** in the prologue after `fs_denials` (the standing
rule: a battery that might wedge a peer goes behind every positive proof).

**Z9. The CDEV request codec moves to `server-rt`.** Two drivers now decode the
same four fields at the same offsets. `server-rt/src/cdev.rs` carries
`Request { minor, gid, len, offset }` and `parse(&Message) -> Request` with the
parse tests; TTY's `cdev.rs` keeps `validate_write` and its tests and drops its
own struct; the memory driver's `cdev.rs` reuses the struct. This is the 5.3
precedent — `rd_i32` and friends were lifted when the second server needed
them — applied to the next layer up. Validation stays per driver, because the
minor set and the clamp are driver facts.

**Z10. The console proof writes through the `/dev/console` descriptor.** A
marker printed on fd 1 after opening `/dev/console` would prove the open
returned a number. Printing the marker *through* the new descriptor is the only
thing that proves the table row points at TTY; routing that row to the memory
driver makes the line vanish, which is the mutation §8.3 lists.

---

## 4. ABI (`kernel-shared`)

### 4.1 The CDEV band

```rust
pub const CDEV_WRITE: i32 = CDEV_RQ_BASE;
/// Client → character driver: read bytes from a device minor.
pub const CDEV_READ: i32 = CDEV_RQ_BASE + 1;
pub const NR_CDEV_MSGS: usize = 2;

pub const CDEV_MINOR_CONSOLE: i32 = 0; // TTY
pub const CDEV_MINOR_NULL: i32 = 3;    // memory — MINIX 3's NULL_DEV
pub const CDEV_MINOR_ZERO: i32 = 5;    // memory — MINIX 3's ZERO_DEV
```

`CDEV_READ` payload: `CDEV_MINOR_OFF`, `CDEV_GRANT_OFF`, `CDEV_LEN_OFF`,
`CDEV_OFFSET_OFF` — `CDEV_WRITE`'s, verbatim. The grant must carry `CPF_WRITE`
and name the driver as grantee; the driver copies with
`SYS_SAFECOPY(SAFECOPY_TO, m_source, …)`. Reply `m_type` is the byte count
(`>= 0`; `0` is EOF) or a negative errno. A short read is legal (Z2).

Doc comments rewritten: `CDEV_WRITE`'s "there is deliberately no `CDEV_READ`"
paragraph, and `CDEV_MINOR_CONSOLE`'s "any other minor is `ENXIO` until 5.11".
The band comment gains one paragraph on the per-driver minor namespace (Z3).

Tripwires to grow, not just satisfy: the test at `callnr.rs` that walks the
CDEV band and asserts `msgs.len() == NR_CDEV_MSGS` gains `CDEV_READ`; the minor
test beside `assert_eq!(CDEV_MINOR_CONSOLE, 0)` gains both new values and
asserts the three are distinct. The existing `const _` band-order guards need
no edit — they are written in terms of `NR_CDEV_MSGS`.

### 4.2 `tools/gen-c-headers`

`bands()`'s hand-maintained CDEV member list gains `("CDEV_READ", …)`;
`callnr_h.rs` emits `CDEV_MINOR_NULL` and `CDEV_MINOR_ZERO` beside
`CDEV_MINOR_CONSOLE`, and the define-list test that pins the emitted values
gains both rows. `every_band_member_list_matches_its_count` is what fails if
the row is forgotten. The header comment on `CDEV_WRITE` having no granter
field is extended to say the same of `CDEV_READ`.

Additive, so inside the D8 freeze; but the SDK sysroot stamp snapshots the
installed headers, so tooling's `build-sysroot.sh` should be re-run after
merge. Nothing in the fork consumes `CDEV_*`, so the two flavours stay
byte-identical until then regardless.

### 4.3 `server-rt/src/cdev.rs` (new, Z9)

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Request { pub minor: i32, pub gid: i32, pub len: i32, pub offset: u64 }
pub fn parse(msg: &Message) -> Request
```

Total, `checked_add`-clean through `rd_i32`/`rd_u64`. Tests: every field from
its own offset (four distinct values), and a zeroed payload yields a request
that validates to `Ok(0)` under TTY's rules — that second one moves *with*
`validate_write` and stays in TTY. `server-rt` stays `#![forbid(unsafe_code)]`.

---

## 5. Component design

### 5.1 `drivers/tty`

`cdev.rs` loses `WriteRequest` and `parse_write` in favour of
`minixrs_server_rt::cdev::{Request, parse}`; `validate_write(Request)` and its
eight tests are unchanged in substance. The doc line "Slice 5.11's `/dev/null`
and `/dev/zero` become additional minors here" is replaced: they are minors of
the *memory* driver, and TTY's minor check stays exactly `== CDEV_MINOR_CONSOLE`.
`main.rs` is untouched: `CDEV_READ` lands in the `_ => ENOSYS` arm, which is the
answer Z7 relies on, and the module note says so.

### 5.2 `drivers/memory` — `cdev.rs` (new, pure) and two arms

```rust
pub enum Minor { Null, Zero }
pub fn classify(minor: i32) -> Result<Minor, i32>           // ENXIO otherwise
pub fn validate(req: Request) -> Result<(Minor, usize), i32>
```

`validate`, in order: minor → `ENXIO`; `len < 0` → `EINVAL`; `!grant_valid(gid)`
→ `EINVAL`; then `Ok((minor, len))` with **no clamp** (Z4). Order matches TTY's
so a request wrong in two ways reports the same first error from either
driver.

`main.rs`:

- `CDEV_WRITE` → `do_cdev_write`: validate, reply `len`. No `SYS_SAFECOPY` is
  issued for either minor — the grant is checked for shape only. This is
  documented as deliberate: a `/dev/null` write with an unmapped buffer
  succeeds, exactly as on Linux, because nothing reads the buffer.
- `CDEV_READ` → `do_cdev_read`: validate; `Null` → `0`; `Zero` → loop
  `sys_safecopy(SAFECOPY_TO, caller_e, gid, offset + done, ZEROS, chunk)` with
  `chunk = min(CDEV_MAX_IO, len - done)` until `done == len`, reply `len`. A
  negative safecopy result is relayed verbatim, and **on a failure after
  partial progress the progress is reported** (`write_all`'s rule from 5.4:
  those bytes really landed).
- `static ZEROS: [u8; CDEV_MAX_IO] = [0; CDEV_MAX_IO];` — a static, not a
  `main`-frame local, for the address-stability reason MFS's block buffer
  gives; 256 bytes, so it is not the one-page-stack concern either way.
- The module note's "no `unsafe` block" claim still holds: both arms are
  kernel calls through `server-rt`.

The `bdev.rs` / `cdev.rs` split mirrors the driver's existing shape: pure
policy in a sibling module, IPC in `main.rs`.

### 5.3 `servers/vfs` — `fd.rs`

```rust
pub enum CharDriver { Tty, Memory }
pub enum Fd {
    Unused,
    CharDev { dev: CharDriver, minor: i32 },
    File { ino: u32, pos: u64 },
}
```

`DEFAULT_ROW` names `CharDriver::Tty` for 0/1/2. `advance_in` is unchanged
(a no-op for any `CharDev`). The module note's "today only the console" line is
rewritten. Every `Fd::CharDev { minor }` pattern in `main.rs` and the fixtures
becomes a compile error, which is the point of doing it as a field rather than
a second variant.

### 5.4 `servers/vfs` — `dev.rs` (new, pure)

```rust
pub const NR_DEV_NODES: usize = 3;
static DEV_NODES: [(&str, CharDriver, i32); NR_DEV_NODES] = [
    (DEV_CONSOLE_PATH, CharDriver::Tty,    CDEV_MINOR_CONSOLE),
    (DEV_NULL_PATH,    CharDriver::Memory, CDEV_MINOR_NULL),
    (DEV_ZERO_PATH,    CharDriver::Memory, CDEV_MINOR_ZERO),
];
pub fn lookup(path: &[u8]) -> Option<Fd>
```

The three path strings live in `kernel-shared::callnr` as `DEV_CONSOLE_PATH`,
`DEV_NULL_PATH`, `DEV_ZERO_PATH` (not emitted in the C headers), so the table
and init's probes cannot drift; the table stores `&str` and compares bytes.

Exact byte equality on the whole path (the `len` VFS copied, no NUL). Tests:
each row resolves to its `(dev, minor)`; `/dev/null/`, `/dev/nul`, `/dev/NULL`,
`dev/null`, `/dev/null\0` and the empty path all miss; the table has no
duplicate paths (a `const _` cannot iterate `&[u8]` equality, so this is a
`#[test]`).

### 5.5 `servers/vfs` — `do_open`

Order after this slice:

1. `open::parse`, `open::validate` (len, `ENAMETOOLONG`, range).
2. `open::validate_flags` — still refuses unknown bits on any path.
3. `sys_copy` the path in. `EFAULT` relayed verbatim.
4. **`dev::lookup(&path[..len])`.** On `Some(entry)`: `fd::alloc(proc_nr, entry)`
   and return it. `flags.create` and `flags.truncate` are not consulted (Z6).
5. `ensure_mounted` — moved here from before step 3 (Z6).
6. The existing lookup / create / classify / alloc / truncate sequence,
   untouched.

The comment on the `_ => return EINVAL` arm in the existing alloc match
("a future device-node arm is a compile error to handle") is now wrong in a
useful way: the device arm lives in step 4, not in that match, and the comment
says so.

### 5.6 `servers/vfs` — `do_read`, `do_write`, endpoints

`main` resolves `mem = mem_endpoint()` beside `tty` and `mfs` — key `"memory"`,
diag `mem.ds ok ep=N` / `mem.ds FAIL rc=R fallback=E`. A small
`fn cdev_endpoint(dev: CharDriver, tty: Endpoint, mem: Endpoint) -> Endpoint`
is the single place the enum becomes an address; `do_read` and `do_write` take
both endpoints.

`do_read`: `Ok(Fd::CharDev { dev, minor })` → `grant_magic(cdev_endpoint(dev),
caller_e, buf, len, CPF_WRITE)`, one `cdev_read(ep, minor, gid, len, 0)`,
revoke, return the count. No `fd::advance` (nothing to advance). The local
`ENOSYS` short-circuit is deleted (Z7).

`do_write`: `Fd::CharDev { dev, minor }` → `write_all(cdev_endpoint(dev), minor,
gid, len)`. `write_all`'s parameter is renamed from `tty` to `driver`; its
loop is unchanged.

`cdev_write` (the prologue helper) gains a sibling `cdev_read`, both thin
`SENDREC` wrappers over the shared payload layout.

### 5.7 `servers/vfs` — `mem_denials` (Z8)

After `fs_denials`, five probes over a 32-byte `main`-frame local, each a
direct grant from VFS's pool:

| name | request | minor | len | grant | expect |
|---|---|---|---|---|---|
| `bad-minor-w` | `CDEV_WRITE` | 7 | 32 | `CPF_READ` | `ENXIO` |
| `bad-minor-r` | `CDEV_READ` | 7 | 32 | `CPF_WRITE` | `ENXIO` |
| `bad-len` | `CDEV_READ` | zero | −1 | `CPF_WRITE` | `EINVAL` |
| `bad-gid` | `CDEV_READ` | zero | 32 | `GRANT_INVALID` | `EINVAL` |
| `read-only-grant` | `CDEV_READ` | zero | 32 | `CPF_READ` | `EPERM` |

The last is the kernel's `verify_grant` refusing the direction, relayed
verbatim by the driver — the read-path twin of `cdev.deny`'s `not-mine`. Marker
`[diag vfs] mem.deny ok n=5`, or `mem.deny FAIL <name>`.

### 5.8 `userland/init` — `dev_demo`

Called from `main` immediately after `fs_demo` and before `write_demo`. Three
parts, each reporting its own marker, each `return`ing on the first failure so
a `FAIL` line names the step:

**zero.** `open("/dev/zero")` → fd. A 64-byte local filled with `0xA5`. `read`
→ must be 64 and every byte 0. `read` again → must be 64 (not EOF — Z2's
distinction between the two devices). `close` → `OK`.
`minix.rs init: dev.zero ok n=64` / `dev.zero FAIL open|short|dirty|eof|close`.

**null.** `open("/dev/null")` → fd. `write` the 35-byte `HELLO` → must be 35.
`read` into the re-poisoned buffer → must be 0 with every byte still `0xA5`.
`close` → `OK`. `minix.rs init: dev.null ok n=35` /
`dev.null FAIL open|write|read|touched|close`.

**console.** `open("/dev/console")` → fd. `write` the line
`minix.rs init: dev.console ok\n` **through that descriptor** (Z10) → must be
its length; `close`. The failure spelling `dev.console FAIL` goes to fd 2.

`const _: () = assert!(HELLO.len() == 35);` and `DEV_BUF_LEN == 64` pin the
literals. The buffer is a local (init's stack is one page; 64 bytes is fine,
and the frame outlives the SENDREC — `fs_demo`'s `MOTD_BUF` reasoning).

Descriptor numbers are not asserted here — `fd_demo` already proves
lowest-free — but each part closes what it opened so `open_denials` and
`write_demo` see the table they expect.

### 5.9 `open_denials`

`+ dev-no-such`: `open("/dev/nope")` → `ENOENT`. Proves the table does not
claim the `/dev` prefix: the path misses, falls through to MFS, and MFS's walk
fails at the `dev` component. `OPEN_DENIAL_PROBES` 11 → 12, marker
`open.deny ok n=12`. The `read-console` probe's expected `ENOSYS` is unchanged;
its doc bullet becomes "TTY does not serve `CDEV_READ` until Phase 6 gives it
RX, and answers it from its unknown-request arm."

---

## 6. Error taxonomy

| Situation | Where decided | Errno |
|---|---|---|
| `CDEV_*` with a minor the driver lacks | driver `validate` | `ENXIO` |
| negative `len` | driver `validate` | `EINVAL` |
| `GRANT_INVALID` / negative gid | driver `validate` | `EINVAL` |
| `CDEV_READ` through a grant without `CPF_WRITE` | kernel `verify_grant`, relayed | `EPERM` |
| `CDEV_READ` on zero into an unmapped buffer | kernel copy engine, relayed | `EFAULT` (partial progress reported if any) |
| `CDEV_WRITE` to null/zero with an unmapped buffer | nobody — no copy is made | success, `len` |
| `CDEV_READ` sent to TTY | TTY unknown-request arm | `ENOSYS` |
| `open("/dev/<unknown>")` | MFS walk | `ENOENT` |
| `open("/dev/null", O_CREAT \| O_TRUNC)` | VFS `do_open` step 4 | success |
| `open("/dev/null", <unknown bit>)` | VFS `validate_flags` | `EINVAL` |
| `read`/`write` on a device fd with `len == 0` | VFS, before any grant | `0` |

---

## 7. Invariants

- **No granter field, anywhere in the band.** `CDEV_READ` copies to `m_source`.
- **`/dev/zero` never returns 0 for a positive `len`; `/dev/null` always does.**
- **The device table is consulted before the mount and after flag validation.**
- **A miss in the table changes nothing about the FS path.** Every 5.8–5.10b
  errno for a `/`-rooted path is preserved.
- **The memory driver still contains no `unsafe` block and never dereferences
  a client buffer.** Both new arms are kernel calls.
- **The minor namespace is per driver.** Nothing asserts `CDEV_MINOR_*` against
  `BDEV_MINOR_*`.
- **TTY's behaviour is unchanged on every path a marker reaches.** Its only
  edits are the codec import and two comments.

---

## 8. Verification

### 8.1 Host

- `cargo test -p minixrs-kernel-shared` — grown band and minor tests.
- `cargo test -p minixrs-gen-c-headers` — member list and define list.
- `cargo test -p minixrs-server-rt` — the codec.
- `cargo test -p minixrs-tty`, `-p minixrs-memory`, `-p minixrs-vfs` —
  validate order, no-clamp, table matching, fd fixtures.
- `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D
  warnings`; both `clippy-kernel` configurations; `cargo clippy -p minixrs-mfs
  --features server`; `cargo gen-c-headers` + the hermetic `clang -fsyntax-only`
  line from CLAUDE.md.

### 8.2 Boot

New required markers in `tests/qemu-boot.expected`, each with its commentary:
`[diag vfs] mem.ds ok`, `[diag vfs] mem.deny ok n=5`,
`minix.rs init: dev.zero ok n=64`, `minix.rs init: dev.null ok n=35`,
`minix.rs init: dev.console ok`, and `open.deny ok n=12` replacing `n=11`.
Forbidden: `minix.rs init: dev.zero FAIL`, `dev.null FAIL`, `dev.console FAIL`,
`[diag vfs] mem.deny FAIL`.

Both configurations boot clean (`--no-default-features` for iteration, default
for the checked-in verdict). Boot-budget ratio measured on the musl flavour
against the merge base, expected within noise: the new traffic is roughly a
dozen round trips.

### 8.3 Mutations (stub-free config, uncommitted tree, snapshot first)

| Mutation | Expected movement |
|---|---|
| delete the `/dev/zero` table row | `dev.zero FAIL open` (`ENOENT` from MFS) |
| swap the null and zero minors in the table | `dev.zero FAIL short` (0 from null) and `dev.null FAIL write`-or-`read` |
| clamp the memory driver's zero read to 16 bytes (a 64-byte probe cannot see a `CDEV_MAX_IO` clamp, so the mutation has to cut below the probe) | `dev.zero FAIL short` |
| drop the minor check in memory `validate` | `mem.deny FAIL bad-minor-w` |
| route the console row to `Memory` | `dev.console ok` line vanishes (write discarded by null-or-ENXIO) |
| `do_open` consults the table after `ensure_mounted` | no marker moves — recorded as unproven, the 5.10b habit |
| `do_read` keeps the local `ENOSYS` for `Tty` | no marker moves — recorded as unproven |

### 8.4 Falsified-claim sweep (whole branch, before the review)

Grep and rewrite every copy of: "no `CDEV_READ` until Phase 6", "new minors,
not new request numbers", "only `CDEV_MINOR_CONSOLE` exists", "any other minor
is `ENXIO` until 5.11", "today only the console", and the `_ => return EINVAL`
device-arm comment in `do_open`. Known sites at branch time: `callnr.rs` (two),
`drivers/tty/src/cdev.rs` (two), `servers/vfs/src/main.rs` (two),
`servers/vfs/src/fd.rs`, `userland/init/src/main.rs`,
`book/src/drivers/overview.md`, `docs/plans/phase-5-musl-fs.md` §5.3 and the
D11 line, and CLAUDE.md's 5.3 bullet. The two "on the `CDEV_READ` precedent"
lines (`callnr.rs` FS band, `fs/mfs/src/walk.rs`) refer to a request being
absent *until it has a consumer* — still true, reworded to say the precedent is
now honoured rather than pending.

---

## 9. Risks

- **The `Fd::CharDev` shape change touches every VFS match.** Mitigated by the
  compiler: a field, not a new variant, so no arm can be missed silently.
- **Reordering `ensure_mounted` past the path copy.** No boot-visible change,
  but a future denial probe that expects a mount error before an `EFAULT` would
  see the reverse. Documented in `do_open`'s step list.
- **A `/dev/null` write with a bad buffer succeeds.** Deliberate (Linux
  behaviour) and documented; it means `bad-buf`-style probes must not be aimed
  at `/dev/null`.
- **DS fallback for `memory` in VFS.** Same shape as the two existing lookups;
  the fallback keeps every marker but `mem.ds ok`.
- **Sysroot stamp drift.** Additive headers; re-run tooling's
  `build-sysroot.sh` after merge so the SDK flavour's `callnr.h` matches.

---

## 10. What a later slice will need

- **Phase 6 TTY RX:** a `CDEV_READ` arm in TTY's loop backed by an IRQ-fed
  buffer; VFS is already routing. Retire the `read-console` probe or flip its
  expectation to a real byte count then.
- **A real `/dev`:** replace `dev::lookup` with device inodes on the image
  (`mknod` in `mkfs-mfs`, a char-device mode bit in `open::classify`), keeping
  the `CharDriver` resolution as the dev-number-to-endpoint map.
- **musl `open`/`read`/`close`:** three `case`s in the fork's `_syscall.c` over
  the existing `VFS_*` requests; no ABI change.
- **`lseek`:** the first thing that will let a boot probe reach `advance_in`'s
  no-op on a `CharDev` deliberately rather than by construction.

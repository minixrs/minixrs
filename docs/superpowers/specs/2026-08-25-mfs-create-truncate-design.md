# Slice 5.10b — create, truncate, and the `VFS_OPEN` flags

**Date:** 2026-08-25
**Status:** design, pending review
**Branch:** `feature/slice-5.10b-mfs-create-truncate`
**Predecessor:** slice 5.10a (the MFS write path, PR #53, merged 2026-08-24)

Slice 5.10a made a MinixFS file writable. It could not make one *exist*: the
write proof needed `/etc/scratch` shipped in the image, because create did not
exist. This slice closes that, adds truncation, and pays off the two items
5.10a's review deferred by name — the mid-write zone leak, and the unprobed
`dirty` half of the inode write-back condition.

The design decisions are labelled `C1…C11`, continuing 5.10a's `W1…W9`
convention: slice-local, distinct from the phase-level `D1…D13`, which are
locked.

---

## 1. What exists, and what the slice changes

| Component | Today | After 5.10b |
|---|---|---|
| `kernel-shared` `callnr.rs` | FS band ends at `FS_WRITE` (`NR_FS_MSGS = 4`) | `+ FS_CREATE`, `+ FS_TRUNC` (6); `VFS_OPEN` gains a flags field |
| `kernel-shared` | no open-flag constants anywhere | `+ fcntl.rs` — `O_CREAT` / `O_TRUNC` / the access-mode mask |
| `fs/mfs` lib `write.rs` | `bitmap_find_free`, `bitmap_set` | `+ bitmap_clear`, `+ dirent-slot and directory-growth policy` |
| `fs/mfs` server | `FS_READSUPER`/`LOOKUP`/`READ`/`WRITE` | `+ FS_CREATE`, `+ FS_TRUNC`, `+ alloc_inode`, `+ free_zone`; `do_write` stages before it allocates |
| `servers/vfs` | `do_open` ignores flags; every open is of an existing file | honours `O_CREAT` / `O_TRUNC`, rejects unknown bits |
| `tools/mkfs-mfs` | four files, one directory level, no sparse files | `+ /full` (62 empty files), `+ /etc/holey` (sparse), `+ /etc/deny` |
| `kernel-shared` `rootfs.rs` | `ROOTFS_NINODES = 64` | `128`, plus the new paths and their content constants |
| `userland/init` | read / write / exec / denial batteries | `+ create`, `+ truncate`, `+ dirgrow`, `+ hole`, `+ leak` probes |

Four existing facts constrain everything below, three of them unchanged from
5.10a and one new.

**MFS has exactly one 4 KiB block buffer**, reached only through the `Blocks`
capability token, whose `read(&mut self) -> Result<&[u8; N], _>` makes "hold a
block across the next fetch" a borrow-check error. This slice adds a *second*
static buffer (§5.4) with a different job and its own token; it does not relax
`Blocks`'s discipline.

**A server stack is exactly one page.** `.bss` is not the constrained resource —
the stack is. That is what makes a second 4 KiB static the cheap fix and a 4 KiB
local a fault into VM's SIGSEGV arm, which prints nothing the forbidden list
catches.

**The FS band carries no grant-offset field**, so VFS re-grants per round (W6).
Nothing here changes that.

**There is no `lseek`.** Every write is sequential from a descriptor's own
position, which is what makes §5.7's `/etc/holey` necessary rather than
decorative — see C9.

---

## 2. Scope

**In:**

- `FS_CREATE` and `FS_TRUNC` — two new FS-band requests.
- An inode allocator, directory-entry insertion, and directory growth in MFS.
- Zone freeing (truncate's half of the allocator).
- `VFS_OPEN` grows a flags field; `O_CREAT` and `O_TRUNC` are honoured.
- The mid-write zone leak is fixed by staging the client's bytes before the
  allocation, and the fix is proved by a boot probe that exhausts nothing.
- The `dirty` half of the write-back condition becomes reachable and is probed.

**Out (later):** `unlink`, `rmdir`, `mkdir`, `rename`, `O_EXCL`, `O_APPEND`,
`lseek`/`ftruncate`, double-indirect zones, permission checking, `fsync`,
`atime`/`mtime` (there is still no clock a user-space filesystem can read).

---

## 3. Decisions

**C1 — `FS_CREATE` carries the full path, and MFS resolves the parent itself.**
The FS band's rule since 5.8 is that the control plane travels inline; a create
is a path operation, so it takes a path. Rejected: VFS splitting the path and
sending `{parent_ino, name}`, which puts path syntax in two servers and costs an
extra `FS_LOOKUP` for the parent; and a single `FS_OPEN {path, flags}` doing
lookup-or-create-or-truncate, which pushes POSIX flag policy into the filesystem,
where VFS cannot see or test it.

**C2 — `FS_CREATE`'s payload is `FS_LOOKUP`'s verbatim, and so is its reply.**
Path inline at `FS_PATH_OFF`, NUL-padded to `FS_PATH_MAX`; the reply carries
`ino`/`mode`/`size` at the offsets `FS_LOOKUP` already defines. One wire codec
for both, and VFS classifies either answer through the same `open::classify`.
This is `FS_WRITE`-reuses-`FS_READ` (W1) applied to the control plane.

**C3 — no mode field on `FS_CREATE`.** There is no uid, no gid check, and no
permission logic anywhere in the tree, so a mode would be a value nothing reads
and a field with one legal value is worse than no field (the 5.8 no-`PUTNODE`
rule, and 5.10b's own C4). MFS creates `I_REGULAR | 0o644` with `nlinks = 1`. The
field arrives with a permission model, and `open(2)`'s `mode_t` argument is
dropped by VFS until then.

**C4 — `FS_TRUNC` truncates to zero and has no length field.** `O_TRUNC` is the
only client, and there is no `ftruncate()` on any path in the tree — no VFS
request, no musl wrapper. A length field would ship five unreachable behaviours
(shrink-to-N, extend, no-op, past-EOF, negative) to serve one reachable one.
Rejected also: folding truncation into `FS_CREATE`, which would leave VFS unable
to truncate a file that already exists — precisely what `O_TRUNC` means.

**C5 — `FS_CREATE` on an existing name is `EEXIST`.** VFS only sends it after a
lookup returned `ENOENT`, so the strict answer costs nothing and is what
`O_EXCL` will need. The alternative — returning the existing inode — makes
"created" and "found" indistinguishable on the wire and hides a duplicate-entry
bug behind a success.

**C6 — the inode is allocated and written back *before* the directory entry is
inserted.** The mirror of 5.10a's zone rule, for the mirror reason: a failure
between the two orphans an inode (a leak, recoverable by a future `fsck`),
whereas the opposite order leaves a directory entry naming an inode that was
never written — a name resolving to whatever the table held, which is corruption.
Leak over corruption, stated once and applied in both directions.

**C7 — truncate writes the zeroed inode back *first*, then frees the bits.** The
inverse ordering, for the same reason read the other way: once the inode names no
zones, freeing a bit can only leak if it fails; if the bits were cleared first, a
failure before the inode reached the device would leave a live inode pointing at
zones the allocator is free to hand out — two files sharing a zone, the exact
corruption 5.10a's ordering exists to prevent.

**C8 — the indirect block's slot scan is bounded by the file's own size.** A
32 KiB file examines two indirect slots, not 1024. Zones past the recorded size
are not freed; that is a leak, and it is the correct trade against holding a
4 KiB indirect block across the bitmap fetches a general scan would need. Every
device-derived loop still has a cap (`ptrs_per_block`), per 5.8's rule.

**C9 — the `dirty` probe needs a sparse file in the image, and `FS_TRUNC` does
not make it reachable.** 5.10a's hand-off says truncate "is what finally makes
reachable" a zone assigned without `size` moving. That is wrong, and it is worth
recording why: with no `lseek`, every write starts at a descriptor's position and
proceeds forward, so a write that assigns a zone always extends the file. The
case needs a **hole below EOF**, which can only come from a sparse write (needs
seek), an extending truncate (ruled out by C4), or the image. Hence `/etc/holey`:
size 8192, `zone[0]` a hole, `zone[1]` holding a known pattern. Writing at
position 0 assigns `zone[0]` while `size` stays 8192 — the case, exactly.

**C10 — directory growth is implemented, and the image is built so one create
reaches it.** `/` holds 4 entries and `/etc` holds 5, against 64 slots in a
block, so growth is unreachable in both boot configurations as the image stands —
and an arm no QEMU boot executes is what the `/etc/pattern` mandate (5.7) and the
`device_teardown_selftest` (5.3) exist to prevent. Forcing it from init would
cost ~60 creates and several hundred device round trips. `mkfs` therefore ships
`/full` with 62 empty files, so its single block holds exactly 64 used slots and
**one** create at boot must allocate a directory zone. Cost: 62 inodes (hence
`ROOTFS_NINODES` 64 → 128, one extra inode block) and no data zones.

**C11 — the leak fix is a second staging buffer, not a rollback.** 5.10a's
docstring records why clearing the bitmap bit on the error path is wrong in one
of the three cases (an indirect slot whose indirect block already existed: the
block on disk still names the zone, so freeing the bit hands it out twice).
Staging the client's bytes into a second 4 KiB `.bss` buffer *before*
`place_zone` removes the question: after the fix, nothing client-controlled can
fail after an allocation, and the invariant is one line instead of a table.

---

## 4. ABI (`kernel-shared`)

### 4.1 The FS band

```
FS_CREATE = FS_RQ_BASE + 4   (0x904)
FS_TRUNC  = FS_RQ_BASE + 5   (0x905)
NR_FS_MSGS: 4 -> 6
```

Still clear of `BDEV_RQ_BASE = 0xA00`; `callnr_h.rs`'s
`bands_are_in_ascending_numeric_order` and the band-ceiling `const _`s cover it.

- `FS_CREATE` request: path at `FS_PATH_OFF`, NUL-padded to `FS_PATH_MAX` (C2).
  Reply: `FS_INO_OFF` / `FS_MODE_OFF` / `FS_SIZE_OFF`, `m_type = OK`.
- `FS_TRUNC` request: inode at `FS_INO_OFF`. Reply `m_type = OK`, no payload.

**`tools/gen-c-headers/src/callnr_h.rs`'s `bands()` list is hand-maintained** and
must gain both rows. `cargo test -p minixrs-gen-c-headers` is what catches an
omission — the `c-headers` CI gate compiles a header that simply never mentions
the constant, and passes. 5.10a hit this.

### 4.2 `VFS_OPEN`

```
VFS_FLAGS_OFF = 12   (i32)
```

The payload becomes `path` (u64 @ 0), `len` (i32 @ 8), `flags` (i32 @ 12). The
existing `const _` chain gains `VFS_PATH_LEN_OFF + 4 <= VFS_FLAGS_OFF` and
`VFS_FLAGS_OFF + 4 <= 96`. `NR_VFS_MSGS` does not change — this is a new field on
an existing request, not a new request.

### 4.3 `kernel-shared/src/fcntl.rs` (new)

```
O_ACCMODE = 0o3, O_RDONLY = 0, O_WRONLY = 1, O_RDWR = 2
O_CREAT   = 0o100        (64)
O_TRUNC   = 0o1000       (512)
```

The **Linux/musl values**, for D7's reason applied to a second ABI: musl's
`open()` passes its own `O_CREAT` straight to the syscall, so matching the
numbers means the `__minixrs_syscall` shim will need no translation table when it
grows `openat`. Today's only client is init, which is Rust — the choice is
forward-looking, and that is the honest framing, not a claim that C uses it now.

**Not emitted by `gen-c-headers`**, exactly like the `AT_*` auxv values (5.5):
musl's own `fcntl.h` defines these, and a second definition in `minixrs/*.h`
would be a redefinition in any translation unit including both. A comment at the
constants records where the values come from, and a `const _` pins them.

### 4.4 `rootfs.rs`

```
ROOTFS_NINODES: 64 -> 128            (C10; one extra inode block)
ROOTFS_HOLEY_PATH    = "/etc/holey"   + ROOTFS_HOLEY_LEN (8192), the hole layout,
                                        and rootfs_holey_byte() for its pattern
ROOTFS_HOLEY_TEXT    = the bytes init writes at position 0 of it
ROOTFS_DENY_PATH     = "/etc/deny"    (empty; the EEXIST probe's target, §5.9)
ROOTFS_FULL_DIR      = "/full"
ROOTFS_FULL_ENTRIES  = 62             (fills its block; see C10)
ROOTFS_FULL_NEW_PATH = "/full/new"    (the create that must grow the directory)
ROOTFS_CREATE_PATH   = "/etc/new"     + ROOTFS_CREATE_TEXT, the bytes init writes
ROOTFS_LEAK_PATH     = "/etc/leak"
ROOTFS_LEAK_PROBES   = ROOTFS_IMAGE_BLOCKS as usize   (256; see §5.8)
```

Contents live here, not in `build.rs` or in init, so the proof is a *check*
against a shared constant rather than a transcription — the rule 5.8 established
when `build_rootfs` lost its literals.

---

## 5. Component design

### 5.1 `fs/mfs` lib — `write.rs`

Three additions, all pure, all unit-tested, none touching a device:

- **`bitmap_clear(block, bit) -> Option<()>`** — `bitmap_set`'s twin, same bit
  order (`byte = bit/8`, `mask = 1 << (bit%8)`), same `None` for a bit past the
  block. The ordering comment on `bitmap_set` gains its counterpart.
- **`dirent_slot(block, want) -> DirentSlot`** — scan one directory block for
  either the name (`Occupied`) or the first `ino == 0` slot (`Free(index)`), else
  `Full`. One pass, because the create path needs both answers and a second scan
  would be a second fetch.
- **`dir_append_offset(size) -> Result<u64, i32>`** — where an appended entry
  goes when no slot is free, with the `MAX_DIR_BYTES` cap applied and the
  `checked_add` discipline every offset in `fs/` uses.

### 5.2 `fs/mfs` server — `alloc_inode`

`alloc_zone`'s twin over the inode bitmap: bounded by `layout.imap_blocks`, bits
limited by `ninodes + 1` (bit *i* names inode *i*; bit 0 is reserved because
inode 0 does not exist), the bit set before anything references it, `ENOSPC` when
the scan runs out.

`Mount` gains a `ninodes: u32` field. It cannot be derived from `layout`, whose
`inode_blocks` is rounded up — using the rounded count as the limit would hand
out inode numbers past the superblock's `ninodes`.

### 5.3 `fs/mfs` server — `do_create`

Steps, in an order C6 fixes:

1. Parse the path; `parse_path` for the syntax rules. Split off the final
   component — the basename — and resolve the parent by walking the rest through
   the existing `lookup` machinery. A parent that is not a directory is
   `ENOTDIR`; a missing parent is `ENOENT`.
2. Scan the parent for the basename and for a free slot in one pass. Present →
   `EEXIST` (C5). Record the free slot's `(block offset, index)` or that there is
   none.
3. `alloc_inode`, then **write the new inode back** — `I_REGULAR | 0o644`,
   `nlinks = 1`, `size = 0`, zones zeroed, timestamps left at 0 (there is no
   clock; 5.10a's rule, unchanged).
4. Insert the entry. Into the free slot if there was one; otherwise append at
   `dir_append_offset(size)`, allocating through **`place_zone`** — a directory
   grows through exactly the allocator a file does, which is why growth needs no
   second code path — and grow the parent's `size` by `DIRENT_SIZE`.
5. Write the parent inode back if its size or zones changed.
6. Reply `ino` / `mode` / `size`, the `FS_LOOKUP` reply shape (C2).

### 5.4 `fs/mfs` server — `do_write`, restaged

One structural change and no behavioural one on the happy path. A second `.bss`
static, `STAGE`, behind its own capability token, is filled from the client's
grant **before** `place_zone`:

```
read inode -> checks -> clamp -> grow_size
  -> SAFECOPY_FROM the client's grant into STAGE      <-- moved here
  -> place_zone                                        (the only allocation)
  -> read-modify-write the data block from STAGE
  -> write the inode back if dirty or grown
```

`STAGE` stays single-purpose: it holds one round's client bytes and nothing else.
Truncate does not borrow it (C8 removes the need), so the invariant is one
sentence — *no client-controlled failure can occur after an allocation* — rather
than a shared-buffer discipline.

The device I/O after the allocation can still fail with `EIO` and still leaks a
zone. That class is unchanged and unreachable by a client: it needs the ramdisk
itself to fail.

### 5.5 `fs/mfs` server — `do_trunc`

1. Read the inode; `EISDIR` for a directory, `EINVAL` for anything not regular.
2. Capture the seven direct zone numbers and the indirect pointer (`Copy`
   scalars — nothing is held across a fetch), and the size, which bounds the
   indirect scan (C8).
3. **Write the inode back zeroed** — all ten pointers 0, `size = 0` (C7).
4. Free the direct zones' bits, then, if there was an indirect block, free the
   bits of the zones its in-size slots name and finally the indirect block's own
   bit. Bitmap blocks are visited in order and each is read once, cleared for
   every zone that falls in it, and written once — the zones of one file are
   adjacent in practice, so this is one read and one write.
5. Reply `OK`.

### 5.6 `servers/vfs` — `do_open`

`open.rs` gains the flags half, still as total functions over plain values:

- **`parse`** reads the third field.
- **`validate_flags(flags) -> Result<OpenFlags, i32>`** — honour `O_CREAT` and
  `O_TRUNC`; accept and ignore the access mode (there is no permission checking
  anywhere, so honouring it would be theatre); **`EINVAL` for any other bit**, so
  a future `O_APPEND` fails loudly instead of silently overwriting from position
  0 and reporting success.

`main.rs`'s `do_open` becomes:

```
lookup(path)
  Ok(dir)                  -> EISDIR                      (unchanged; classify)
  Ok(file) + O_TRUNC       -> FS_TRUNC, then allocate the fd
  Ok(file)                 -> allocate the fd             (unchanged)
  Err(ENOENT) + O_CREAT    -> FS_CREATE, then allocate the fd
  Err(e)                   -> e
```

`O_CREAT | O_TRUNC` on a missing file takes the create arm and stops there: a
freshly created file is already empty, so truncating it would be a second round
trip to reach the state it is in. `O_TRUNC` is applied *before* the descriptor
exists, so a failed truncate leaves no descriptor onto a half-truncated file.

**`Fd::File` is unchanged** — no flags are stored on the descriptor. The access
mode is ignored (§5.6), `O_CREAT` and `O_TRUNC` are consumed at open time by
definition, and every other bit is refused, so there is nothing left for a
descriptor to remember. `EISDIR` is decided by `classify` as
today and is therefore reached before either new request — `O_TRUNC` on a
directory never sends `FS_TRUNC`, which matters because MFS's own `EISDIR` guard
would then be the only thing between a probe and a freed directory.

### 5.7 `tools/mkfs-mfs`

- **A sparse entry.** The manifest gains an explicit way to describe a file with
  a leading hole — one variant, used once, for `/etc/holey`. The writer must skip
  allocating the hole's zone and still record the full size; `verify.rs` must
  read it back as zeroes, which is the read path's existing `Hole` rule.
- **`/full`** with `ROOTFS_FULL_ENTRIES` (62) zero-length files, so the directory
  is exactly one block of 64 used slots including `.` and `..` (C10).
- **`/etc/deny`**, empty, as the `EEXIST` probe's target (§5.9).
- `ROOTFS_NINODES` 64 → 128 shifts `first_data_zone` by one block; the layout
  unit tests and `verify.rs`'s fixtures move with it, and the existing scratch
  headroom check must be re-run against the built image, not a fixture (the fix
  in `b12f789`).

### 5.8 `userland/init` — the proof

Five new probes. Ordering is load-bearing where noted; the existing prologue
order is otherwise untouched.

| Probe | What it does | Marker |
|---|---|---|
| `create_demo` | `open("/etc/new", O_CREAT)` → write → close → re-open without `O_CREAT` → read back → compare | `fs.create ok n=<len>` |
| `dirgrow_demo` | create in `/full`, whose block is exactly full → write → re-open → read back | `fs.dirgrow ok n=<len>` |
| `hole_demo` | write at position 0 of `/etc/holey`; re-open; verify **both** the new bytes at 0 and the untouched pattern at 4096 | `fs.hole ok` |
| `trunc_demo` | `/etc/scratch`, 32 KiB from the 5.10a demo, opened `O_TRUNC`; re-open; read must be EOF at once | `fs.trunc ok n=0` |
| `leak_probe` | create `/etc/leak`; `ROOTFS_LEAK_PROBES` writes through the unmapped VA, each of which must answer `EFAULT`; then a real write that must succeed | `fs.leak ok n=256` |

**`trunc_demo` must run after `write_demo`** — it truncates what that probe
wrote, and running it first would truncate an empty file and prove nothing.

**`leak_probe`'s count is `ROOTFS_IMAGE_BLOCKS`**, which is greater than any
possible free-zone count in the image, so the probe is config-independent by
construction rather than by measurement — the rule that a marker must not carry a
number that differs between the musl and worker flavours. Each probe writes at
position 0 of a file whose inode is never written back, so every attempt targets
a hole and, under today's code, leaks a zone; the final real write answers
`ENOSPC` without the fix.

**`hole_demo` verifies two windows and both are load-bearing**: the bytes at 0
prove the assigned zone's pointer reached the inode (dropping `dirty` from the
write-back condition loses it and the read returns zeroes), and the pattern at
4096 proves the write did not disturb the zone that was already there.

### 5.9 Denial batteries

**`open.deny` (init), 7 → 11.** New probes, all band- or flag-relative rather
than literal, per the rule that a growing capability must make a denial probe
fail loudly rather than pass vacuously:

- `O_CREAT` into a missing directory → `ENOENT`
- `O_CREAT` on an existing directory → `EISDIR`
- `O_TRUNC` on a directory → `EISDIR`
- an unimplemented flag bit → `EINVAL`

**`fs.deny` (VFS), 10 → 14.** These are direct FS requests, so they are the only
place `EEXIST` and MFS's own `EISDIR` can be probed:

- `FS_CREATE` on an existing name → `EEXIST`
- `FS_CREATE` whose parent is a file → `ENOTDIR`
- `FS_TRUNC` on a directory → `EISDIR`
- `FS_TRUNC` on inode 0 → `EINVAL`

The `EEXIST` probe targets **`/etc/deny`**, which exists for this and is read by
nothing else, and it **re-looks-up the target afterwards and compares the inode
number**. Without that, a dropped `EEXIST` would insert a second entry shadowing
the first — silently, with every other marker still green. This is the 5.8
`VFS_WRITE + 1` and 5.10a `write-file` lesson applied before the fact rather
than after.

The `FS_TRUNC`-on-a-directory probe aims at `/etc`. An accidental success frees
that directory's zones and every later `/etc` marker dies — destructive, but
loud, which is the property the convention asks for.

---

## 6. Error taxonomy

| Condition | Answer | Why |
|---|---|---|
| `FS_CREATE`, name exists | `EEXIST` | C5 |
| `FS_CREATE`, parent missing | `ENOENT` | the parent is what was named |
| `FS_CREATE`, parent is a file | `ENOTDIR` | 5.8's rule for an intermediate component |
| `FS_CREATE`, no free inode | `ENOSPC` | `alloc_zone`'s precedent |
| `FS_CREATE`, directory full and no zone | `ENOSPC` | growth failed, not the name |
| `FS_CREATE`, component > `NAME_MAX` | `ENAMETOOLONG` | `parse_path`, unchanged |
| `FS_TRUNC` on a directory | `EISDIR` | `do_write`'s guard, same wording |
| `FS_TRUNC` on a non-regular inode | `EINVAL` | `do_write`'s guard |
| `FS_TRUNC`, inode not addressable | `EINVAL` | `read_inode`'s existing split |
| any device failure | `EIO` | W7 — the client addressed a file |
| `SYS_SAFECOPY` failure | relayed verbatim | W7 — `EPERM` and `EFAULT` are different caller bugs |
| `VFS_OPEN`, unimplemented flag bit | `EINVAL` | §5.6 |
| `VFS_OPEN`, `O_CREAT` on a directory | `EISDIR` | `classify`, before either new request |

---

## 7. Invariants

1. **A bitmap bit is set before anything references the object it names**, and
   cleared only after nothing does. Allocation (5.10a) and freeing (C7) are the
   two directions of one rule.
2. **No client-controlled failure occurs after an allocation** (C11). This is
   what `STAGE` buys, and `fs.leak` is its boot proof.
3. **The inode is written back when a zone was assigned *or* the size grew**, not
   on size alone. Unproven since 5.10a; `fs.hole` proves it here.
4. **Nothing is held across a block fetch.** `Blocks` enforces it structurally;
   every intermediate in the new paths is a `Copy` scalar.
5. **Every device-derived loop has a cap** — the imap scan, the dirent scan, the
   indirect slot scan, the bitmap-block walk.
6. **A directory grows through the same allocator a file does.** No second
   allocation path exists to diverge.

---

## 8. Verification

**Unit (host, every CI job):** `bitmap_clear`'s bit order and out-of-range
`None`; `dirent_slot`'s three answers including a block whose slots are all used;
`dir_append_offset` at the `MAX_DIR_BYTES` boundary and its overflow guard;
`validate_flags` for each honoured bit, the access mode, and a rejected bit;
`classify` unchanged. `cargo test -p minixrs-gen-c-headers` for the band rows,
and `cargo clippy -p minixrs-mfs --features server`, which is the only job that
compiles MFS's `main.rs` at all.

**Boot markers:** the five new `fs.*` lines, the two grown denial counts, and
every existing marker unchanged. `tests/qemu-boot.expected` gains the new lines
in the same PR as the code.

**Mutation matrix** (apply, observe the named marker move, revert — against an
uncommitted tree, with the files snapshotted to the scratchpad first, including
files this slice *adds*, since `git checkout` does not restore an untracked file):

| Mutation | Expected |
|---|---|
| Move the `STAGE` copy back after `place_zone` | `fs.leak FAIL` (`ENOSPC` on the final write) |
| Drop `dirty` from the write-back condition | `fs.hole FAIL` at offset 0 |
| Insert the dirent before writing the inode (C6 reversed) | `fs.create FAIL` — the read-back is garbage |
| Free the bits before writing the inode (C7 reversed) | no marker moves; recorded as **unproven**, since it needs a failure between the two steps that nothing can induce |
| Drop `EEXIST` from `do_create` | `fs.deny FAIL` *and* the inode-comparison check |
| Return `Full` instead of appending when no slot is free | `fs.dirgrow FAIL` |
| `bitmap_clear` clearing the wrong bit | `fs.trunc` or a later write FAILs |

The C7 row is stated rather than hidden: it is a correct invariant that this
slice cannot probe, and saying so is the 5.10a `dirty` lesson applied to the new
rule rather than repeated by omission.

**Boot budget.** The leak probe is 256 extra round trips and is the dominant new
cost. The slice must measure the last required marker's position as a fraction of
a fixed-timeout log on the **musl** flavour (`grep -abo <marker> log | head -1`
divided by `wc -c log`), against the same number at the merge base — detached to
the merge base with only the doc edits stashed, so `target/` and
`target/musl-sysroot` survive, and with `cargo build` run before the timed
`cargo run`. If the ratio climbs materially the `qemu-smoke` budget goes up with
headroom, in the ratio rather than in local wall-clock seconds. Both previous
raises (45 → 120 → 240 s) came from exactly this measurement.

**Three-boot matrix** unchanged in form: SDK, forced in-tree musl
(`MINIXRS_SDK=/nonexistent`), and moved-aside sysroot **with
`MINIXRS_SDK=/nonexistent`** — without which the third row silently re-runs the
first.

---

## 9. Risks

**R1 — the image grows and the scratch headroom shrinks.** `/full`'s directory
block, `/etc/holey`'s data zone, `/etc/deny`, and the extra inode block cost
about four zones out of the 185 free ones measured in the musl flavour. The
existing headroom check runs against the built image and will catch a real
squeeze; the risk is that it catches it in the *SDK* flavour only, where
`/bin/hello` is four times smaller and the margin is different. Run the three-boot
matrix.

The inode budget after the bump: 10 image inodes (root, `bin`, `etc`, `full`,
`hello`, `motd`, `pattern`, `scratch`, `holey`, `deny`) plus `/full`'s 62 plus
three created at boot — 75 of 128, with the margin sized so a later slice adding
files does not immediately re-bump it.

**R2 — `ROOTFS_NINODES` shifts `first_data_zone`.** Every layout fixture and
every zone number in `mkfs`'s tests moves. These are unit-tested, so the failure
is a red test rather than a corrupt image, but the diff is wider than it looks.

**R3 — the sparse-file entry is one-use machinery.** Justified by C9 and by
nothing else. It stays one variant with one caller; a general sparse-file
description would be code with no second user.

**R4 — 256 failing writes is a real boot cost.** Each is two IPC round trips and
one inode read. If the measured ratio moves too far, the fallback is to derive
the probe count from the free-zone count the image actually has rather than from
`ROOTFS_IMAGE_BLOCKS` — but only if that number can be made config-independent,
which is why it is the fallback and not the design.

---

## 10. What a later slice will need

`unlink` (which needs the freeing half of C7 pointed at a named entry, and
`nlinks` bookkeeping that this slice writes but never decrements), `O_EXCL` (C5
already provides its answer), `O_APPEND` (currently `EINVAL`, and the flag
validator is where it lands), `lseek` — which is what would finally let a probe
reach the C7 ordering and the sparse-write case without a shipped sparse file —
and `mkdir`, which is `do_create` with a different mode plus the `.`/`..`
entries.

# Slice 5.10a — the MFS write path

**Date:** 2026-08-18
**Status:** design, pending review
**Branch:** `feature/slice-5.10a-mfs-write-path`
**Predecessor:** slice 5.9 (exec-from-FS, PR #52, merged 2026-08-04 — Phase 5 milestone B)

Phase 5's plan carries slice 5.10 as a five-line stretch sketch: *"`BDEV_WRITE`
consumer side; MFS write/create/truncate (+ the write-side FS requests);
`VFS_WRITE` to real fds routes to MFS (fd>2), and `VFS_OPEN` grows `O_CREAT`-lite."*
That is four separable things. This document designs the first two as **slice
5.10a** and defers create/truncate/`O_CREAT` to **5.10b**, mirroring the 5.9a/5.9b
split.

---

## 1. What exists, and what the slice changes

The read path is complete and the write path's landing sites were all left
deliberately, each with a comment naming this slice:

| Component | Today | After 5.10a |
|---|---|---|
| `drivers/memory` | `BDEV_WRITE` validates geometry, answers `EROFS` | performs the store |
| `fs/mfs` lib | `read.rs`: `zone_for_offset`, `ptrs_per_block`, `inode_at` | `+ write.rs`: allocation + write policy |
| `fs/mfs` server | `FS_READSUPER`/`FS_LOOKUP`/`FS_READ` | `+ FS_WRITE` |
| `servers/vfs` | `do_write`'s `Ok(Fd::File { .. }) => EROFS` | routes to `FS_WRITE`, loops |
| `tools/mkfs-mfs` | writes `/etc/motd`, `/etc/pattern`, `/bin/hello` | `+ /etc/scratch`, zero-length |
| `userland/init` | read + exec + denial batteries | `+ write_demo` |

Three existing facts constrain everything below.

**MFS has exactly one 4 KiB block buffer.** It is a `.bss` static reached only
through the `Blocks` capability token, whose `read(&mut self) -> Result<&[u8; N], _>`
makes "hold one block across the fetch of the next" a borrow-check error. A
server stack is one page, so there is no second buffer to be had and none may be
introduced.

**The ramdisk mapping is already `Prot::RW_DATA`.** No kernel change is needed to
make the device writable; slice 5.7 mapped it read-write against exactly this
slice.

**The FS band carries no grant-offset field.** Slice 5.8 fixed that deliberately:
*"VFS issues a fresh grant over exactly the round's bytes."* The write loop must
honour it (§5), rather than growing the payload.

---

## 2. Scope

**In (5.10a):**

- `BDEV_WRITE` becomes a real store in `drivers/memory`.
- `FS_WRITE` — one new FS-band request.
- MFS gains a zone allocator and a write path covering direct zones **and**
  single-indirect, symmetric with the read path.
- VFS routes `VFS_WRITE` on an `Fd::File` to MFS and loops.
- `/etc/scratch` (zero-length) enters the root image; init writes it, reads it
  back, and reports through the path under test.

**Out (5.10b):** `FS_CREATE`, `FS_TRUNC`, inode allocation, directory-entry
insertion, `VFS_OPEN` flags, `O_CREAT` / `O_TRUNC`.

**Out (later):** `unlink`, `mkdir`, double-indirect zones, `fsync`, atime/mtime
(there is no clock a user-space FS can read yet), journaling, `MFSFLAG_CLEAN`
bookkeeping.

---

## 3. Decisions

Labelled `W1…W9` rather than continuing the phase-level `D1…D13`, which are
locked and describe the phase, not a slice.

**W1 — `FS_WRITE` reuses `FS_READ`'s payload verbatim.** Same four fields at the
same offsets (`FS_INO_OFF`, `FS_GRANT_OFF`, `FS_LEN_OFF`, `FS_POS_OFF`), reply
`m_type` is the byte count. This is the `VFS_READ`-reuses-`VFS_WRITE` precedent
from 5.8. It also keeps `rw.rs`'s direction-agnostic step rules applicable
without a second parser.

**W2 — a short `FS_WRITE` is normal, not an error.** MFS clamps every request to
the end of the block containing `pos`, so one call moves at most one block and
usually less. This is `CDEV_WRITE`'s stance, deliberately *not* `BDEV_READ`'s
refuse-or-nothing. The reason the two differ is unchanged from 5.7/5.8: BDEV
refuses because its client is a filesystem that cannot interpret a fraction of a
block; here the client is VFS, whose whole job is hiding staging from POSIX.

**W3 — the write path allocates direct *and* single-indirect zones.** Asymmetry
with the read path would be surprising, and an allocation arm CI cannot reach is
the failure mode `/etc/pattern` exists to prevent (slice 5.7). Double-indirect
stays out, as it is in the reader.

**W4 — a freshly allocated zone is zeroed before use, always.** Not only when the
write is partial. A newly allocated data zone holds whatever the previous owner
left; a newly allocated *indirect* block holds what would otherwise be read as
zone pointers. Zeroing unconditionally costs one extra `BDEV_WRITE` per
allocation and removes a whole class of read-your-neighbour bug. Consistency
beats the saved round trip.

**W5 — MFS's grant over its block buffer widens to `CPF_READ | CPF_WRITE`.** One
buffer, used in both directions, one grant issued once at boot over a static
address so `ensure_registered` still never re-fires. A second grant would buy
nothing: the grantee is the same driver either way, and the kernel checks the
direction bit per call.

**W6 — VFS re-grants per round.** The FS band has no grant-offset field (5.8),
so each `FS_WRITE` gets a fresh magic grant over `buf + off` for `len - off`
bytes, revoked before the next. This differs from the CDEV write loop, which
grants once and advances a payload `offset` — because `CDEV_WRITE` *has* that
field and `FS_WRITE` deliberately does not.

**W7 — device failures are `EIO`; `SYS_SAFECOPY` failures are relayed verbatim.**
Unchanged from 5.8, and the two rules only look contradictory. A `BDEV_WRITE`
that fails is answering a question MFS's caller did not ask (it addressed a
*file*), so the device's errno is noise. A safecopy failure against VFS's grant
is `EPERM` (bad grant) or `EFAULT` (unmapped buffer), which are different caller
bugs and must stay distinguishable.

**W8 — the inode is written back on every `FS_WRITE` that grows the file.** MFS
holds no per-open state — that is why the FS band has no `PUTNODE` — so it cannot
know a further write is coming and cannot defer the update. The cost is one
read-modify-write of the inode block per growing call; §8 measures it.

**W9 — the proof file is a new, zero-length `/etc/scratch`.** Create does not
exist until 5.10b, so the target must be in the image already. A zero-length file
makes growth-from-nothing the default path rather than a special case, and it
keeps `/etc/motd` and `/etc/pattern` — which are *read* proofs — untouched.

---

## 4. ABI (`kernel-shared`)

### 4.1 The FS band

```rust
pub const FS_WRITE: i32 = FS_RQ_BASE + 3;
pub const NR_FS_MSGS: usize = 4;          // was 3
```

The band is `0x900..0xA00`; four of 256 slots are used and the existing
`const _` ordering guards (`FS_RQ_BASE + (NR_FS_MSGS - 1) < BDEV_RQ_BASE`) hold
unchanged. No new offset constants: W1 reuses `FS_READ`'s four.

The `callnr.rs` host tests that enumerate the band gain `FS_WRITE` — including
`the_server_band_space_below_vm_is_fully_allocated`, the contiguity check
(`FS_RQ_BASE + i`), the cross-band distinctness sweep, and the payload
field-count assertions, whose `"an FS_READ payload field was added"` message
style extends to the write.

The grant named in `FS_GRANT_OFF` must carry `CPF_READ` for a write, where a read
needs `CPF_WRITE`. That is the kernel's check in `verify_grant`, not MFS's, and
is stated in the doc comment rather than re-implemented.

### 4.2 `rootfs`

```rust
pub const ROOTFS_SCRATCH_PATH: &str = "/etc/scratch";
pub const ROOTFS_SCRATCH_LEN: usize = 32 * 1024;      // 8 blocks
pub const ROOTFS_SCRATCH_PERIOD: usize = 251;
pub const fn rootfs_scratch_byte(i: usize) -> u8 { ((i + 7) % ROOTFS_SCRATCH_PERIOD) as u8 }
```

The generator is `rootfs_pattern_byte`'s shape and inherits its reasoning: 251 is
prime and coprime with 4096, so a lost, duplicated, or reordered block changes
the bytes rather than landing on the same value again. The `+ 7` skew makes
scratch content distinguishable from pattern content, so a cross-file mix-up
shows up as a mismatch instead of a coincidence.

`ROOTFS_SCRATCH_LEN` is 32768 — exactly 8 blocks, so the file's last zone is
indirect slot 0. `const _` guards assert it exceeds the seven direct zones (the
`ROOTFS_PATTERN_LEN` precedent) and that it stays inside the single-indirect
span.

The file is **written at runtime, not at mkfs time**, so its length is a claim
init proves rather than something the image asserts.

---

## 5. Component design

### 5.1 `drivers/memory` — `BDEV_WRITE`

`bdev.rs`'s `parse_read` / `validate_read` are already direction-agnostic; they
gain a doc line saying so and write-direction tests, and the over-long request
stays `EINVAL` (a short block write is as useless as a short block read, and
`EIO` stays reserved for Phase 6's real media errors).

`main.rs`'s `BDEV_WRITE` arm replaces `Ok(_) => EROFS` with

```
sys_safecopy(SAFECOPY_FROM, caller_e, req.gid, 0, va + byte_off, n)
```

— the exact mirror of the read arm's `SAFECOPY_TO`, reply `= n`. `EROFS` leaves
the driver's imports, and the module doc's "5.10 replaces this line" paragraph is
rewritten to record what the direction bit now means.

**Landmine:** MFS's `bdev_denials` battery currently probes `BDEV_WRITE → EROFS`
and feeds a counted marker. That probe becomes a *successful write to block 0* —
which would corrupt the superblock. It is re-pointed to a `BDEV_WRITE` naming a
`CPF_WRITE`-only grant, which the kernel refuses with `EPERM`. The probe count is
unchanged and it now covers the direction bit, which nothing covered before.

### 5.2 `fs/mfs` lib — `write.rs`

Pure, I/O-free, host-tested, `forbid(unsafe_code)` — the `read.rs` shape. Every
offset/length expression uses `checked_add` (servers ship with
`overflow-checks = false`).

```rust
pub fn clamp_write(pos: u64, len: i32, bs: usize) -> Result<Chunk, i32>;
pub enum ZoneSlot { Direct(usize), Indirect(usize), OutOfRange }
pub fn zone_slot_for_offset(off: u64, bs: usize) -> ZoneSlot;
pub fn bitmap_find_free(block: &[u8], from_bit: u32, limit_bits: u32) -> Option<u32>;
pub fn bitmap_set(block: &mut [u8], bit: u32) -> Option<()>;
pub fn grow_size(cur: i32, pos: u64, n: usize) -> Result<i32, i32>;
```

- `clamp_write` ends the chunk at the block boundary (W2), rejects a negative
  length with `EINVAL`, and answers `EFBIG` past the single-indirect span. It is
  `clamp_read`'s twin and differs in one way worth a comment: a read clamps at
  EOF, a write does not — writing past EOF is how a file grows.
- `zone_slot_for_offset` is `zone_for_offset`'s allocating twin. `zone_for_offset`
  answers *what zone is there*, distinguishing `Hole` from `OutOfRange`;
  `zone_slot_for_offset` answers *where a zone would go*, which is what the
  allocator needs and what the reader has no use for.
- `bitmap_*` are byte/bit arithmetic over a borrowed block, so they are testable
  without a device and reusable by 5.10b's inode allocator unchanged.
- `grow_size` caps at `i32::MAX` and returns `EFBIG` rather than wrapping;
  MinixFS stores size as a 32-bit field.

### 5.3 `fs/mfs` server — `do_write`

`Blocks` gains two methods keeping the existing borrow discipline:

```rust
fn buf_mut(&mut self) -> &mut [u8; MFS_BLOCK_SIZE];   // borrowed from &mut self
fn write(&mut self, block: u64) -> Result<(), i32>;   // BDEV_WRITE of the buffer
```

The sequence, ordered so the single buffer is never wanted twice at once. Each
step completes — buffer contents consumed or flushed — before the next begins.

1. **Resolve the inode.** Read the inode block, decode; `Inode` is `Copy`, so the
   borrow dies at the `let` and the buffer is free again. Reject a directory with
   `EISDIR`; reject an unmounted server with `ENODEV`.
2. **Clamp** (`clamp_write`) → `(zone index, offset in block, n)`.
3. **Resolve or allocate the zone.**
   - `ZoneSlot::Direct(i)`: if `inode.zone[i] == 0`, allocate; patch the in-memory
     `Inode`.
   - `ZoneSlot::Indirect(slot)`: if `inode.zone[7] == 0`, allocate the indirect
     block, **zero it and write it out** (W4), patch the `Inode`. Then read the
     indirect block, take slot's value, and if it is 0 allocate a data zone, patch
     the slot, write the indirect block back.
   - `ZoneSlot::OutOfRange`: `EFBIG` (already caught in step 2; kept as a
     defence-in-depth arm, not a second gate).

   `alloc_zone` walks the zone bitmap a block at a time, bounded by
   `layout.zmap_blocks` — every device-derived loop has a cap, the 5.8 rule, because
   a corrupt superblock must not spin MFS and through it VFS and init. It sets the
   bit, writes that bitmap block back, and returns the zone number, or `ENOSPC`.
   Freshly allocated data zones are zeroed and written (W4).
4. **Splice.** If `n < block_size`, read the target block first; otherwise skip the
   read, since the write covers it whole. `sys_safecopy(SAFECOPY_FROM, vfs_e,
   gid, 0, buf_addr + off_in_block, n)` — the granter is the **kernel-stamped
   `m_source`**, never a payload field, which is the rule every grant-consuming
   site in this tree follows. Then `Blocks::write` the block.
5. **Update the inode** if it changed (W8): read the inode block, patch the
   64-byte slot's size and zone array, write it back.

   The condition is **"a zone was assigned *or* the size grew", not "the size
   grew"**. Filling a hole in the middle of an existing file assigns
   `inode.zone[i]` without moving `size` at all; keying the write-back on size
   alone would drop that pointer on the floor and the bytes would read back as a
   hole on the next open, while the zone stayed marked in use in the bitmap. That
   is invariant 5 failing in the *unsafe* direction — the bitmap and the inode
   disagreeing about a live zone — and it is the one ordering mistake in this
   sequence that corrupts rather than leaks.

Reply `m_type = n`.

No `unsafe` is added: `buf_mut` uses the same `UnsafeCell` accessor the existing
`zeroed()` does, under the same `&mut self` serialization argument.

### 5.4 `servers/vfs` — routing

`do_write`'s file arm becomes the mirror of `do_read`'s, plus a loop:

```
loop over rw::advance(off, len, n):
    gid = grants.grant_magic(mfs, caller_e, buf + off, len - off, CPF_READ)   // W6
    n   = fs_write(mfs, ino, gid, (len - off) as i32, pos + off)
    grants.revoke(gid)
```

`rw::advance`'s four rules are already direction-agnostic and were factored out in
5.4 precisely so a misbehaving peer's cases are testable: it clamps `off` with
`.min(len)` so an over-reporting server cannot walk past the buffer, breaks on
`n == 0` rather than spinning, and reports partial progress on an error after
progress (POSIX: those bytes really went out). None of that changes.

`fd::advance(proc_nr, fd, total)` runs on real progress only, the `do_read` shape.
`EROFS` leaves `do_write` — after 5.10a there is no descriptor VFS refuses to
write — and returns in 5.10b if a full filesystem needs it.

VFS keeps looping for write and keeps *not* looping for read: POSIX allows a short
`read()` and forbids an unexplained short `write()`. That asymmetry is already
documented in `rw.rs` and is unchanged.

### 5.5 `tools/mkfs-mfs`

`/etc/scratch` joins the manifest as a zero-length entry. The writer must handle a
0-byte file: no zones allocated, `size = 0`, `nlinks = 1`, mode `I_REGULAR`. That
is a real gap today — every current entry has content — so it gets its own
`image.rs` test and a `verify.rs` assertion that the entry exists, is regular, and
is empty.

The block-budget precheck already names the constant it blew; the scratch file
adds nothing to it at build time. At *runtime* the write allocates 8 data zones
plus 1 indirect block against roughly 180 free blocks of the 256-block image, so the
headroom is order-of-magnitude and needs no new guard — but `rootfs.rs` gets a
`const _` recording the arithmetic, so a future image shrink fails at compile time
instead of at boot with `ENOSPC`.

### 5.6 `userland/init` — the proof

New `write_demo(vfs)`, placed in the prologue **after `fs_demo` and before
`exec_denials`**. The prologue's ordering rule is stated twice in `main` and is
load-bearing: newest code last, so a hang localizes to its own markers instead of
blacking out the older ones. `write_demo` is newer than `fs_demo` and older than
nothing, so it goes between — and `exec_denials` keeps its comment about being
last.

- **Source:** one immutable `const`-generated 4016-byte static
  (`16 × ROOTFS_SCRATCH_PERIOD`), the `LOOP_LINE` precedent. Because 4016 is a
  multiple of the generator's period, every chunk start is congruent to 0 and the
  *same* static is the correct content for every chunk — one buffer, no `unsafe`,
  no mutable static. 4016 is deliberately **not** a multiple of 4096, so partial-block
  splicing and boundary-crossing short writes are exercised on every chunk but the
  first.
- **Write:** open `/etc/scratch`, write `ROOTFS_SCRATCH_LEN` bytes as 8 full chunks
  plus a 640-byte remainder, summing the returned counts. Crossing 28672 puts the
  single-indirect allocation on this marker.
- **Verify:** close, re-open (so the read starts from a fresh descriptor and a
  fresh `FS_LOOKUP`, proving the size really was persisted to the inode), and read
  back **three 512-byte windows** into a stack local — at offset 0, straddling the
  28672-byte direct/indirect seam, and at the tail — comparing each byte against
  `rootfs_scratch_byte`. Three windows rather than the whole file: the total count
  already proves the bytes were accepted, the seam window is the only place the
  indirect arm can hide, and 64 extra round trips buy nothing the seam does not.
- **Report** through fd 1, the path under test (the 5.4 rule):
  `minix.rs init: fs.write ok n=32768 v=3`, with distinguishable failure forms
  (`fs.write FAIL short n=…`, `fs.write FAIL verify w=…`, `fs.write FAIL open rc=…`).

**The landmine this slice must defuse.** `open_denials`' eighth probe today is:

```rust
if vfs_write(vfs, fd, ROOTFS_MOTD_PATH.as_bytes()) == EROFS { denied += 1; }
```

— a write to a descriptor on `/etc/motd`, expecting the read-only refusal. After
5.10a that write **succeeds and overwrites `/etc/motd`'s first 9 bytes**, which is
both a silently retired probe and active corruption of a read proof. This is
exactly the class 5.8 hit when `("no-such", VFS_WRITE + 1, …)` became a real
request. The probe is **retired**, the battery drops to seven, and the marker
becomes `open.deny ok n=7` — a visible diff in `tests/qemu-boot.expected` rather
than a count that quietly means something else. `OPEN_DENIAL_PROBES` is the single
constant that changes.

---

## 6. Error taxonomy

| Condition | Errno | Raised by |
|---|---|---|
| Descriptor is not open / out of range | `EBADF` | VFS |
| Descriptor is a console, write | *(succeeds — CDEV path)* | VFS |
| Negative length, unusable buffer address | `EINVAL` | VFS `rw::validate` |
| Nothing mounted | `ENODEV` | MFS |
| Inode is a directory | `EISDIR` | MFS |
| Position past the single-indirect span | `EFBIG` | MFS `clamp_write` |
| Size would exceed `i32::MAX` | `EFBIG` | MFS `grow_size` |
| Zone bitmap exhausted | `ENOSPC` | MFS `alloc_zone` |
| Any `BDEV_READ`/`BDEV_WRITE` failure | `EIO` | MFS (W7) |
| Grant bad / buffer unmapped | `EPERM` / `EFAULT`, verbatim | kernel, relayed by MFS (W7) |
| Block out of range, over-long request | `EINVAL` | `drivers/memory` |
| Grant lacks `CPF_READ` | `EPERM` | kernel `verify_grant` |

MFS stays **degraded, never fatal and never a panic** past `sef_startup`, the 5.8
rule: a filesystem that cannot allocate answers `ENOSPC` to writes and keeps
serving reads.

---

## 7. Invariants

1. **One block buffer, one live borrow.** Every new path holds `&mut Blocks`
   across exactly one device operation. The borrow checker enforces it; any
   error it raises here is a real aliasing bug, not a nuisance.
2. **A zone is zeroed before it is reachable.** No allocation patches a pointer
   into an inode or an indirect block before the target's contents have been
   written (W4).
3. **Every device-derived loop is capped.** The bitmap scan is bounded by
   `layout.zmap_blocks`; a corrupt superblock cannot spin MFS.
4. **The granter is `m_source`.** No request in this slice carries a granter or a
   grant-offset field, and none may grow one.
5. **The bitmap and the inode agree.** A zone's bit is set *before* its number is
   stored, so a failure between the two leaks a zone rather than sharing one. A
   leak is recoverable by a future `fsck`; a shared zone is silent corruption.

---

## 8. Verification

**Host tests.** Every function in `write.rs`, including: clamping at a block
boundary; `EFBIG` one byte past the indirect span; a bitmap whose only free bit is
the last in the block; a bitmap that is entirely full; `bitmap_set` on an
out-of-range bit; `grow_size` at `i32::MAX`; `zone_slot_for_offset` at 0, at the
last direct byte, at the first indirect byte, and past the end. Plus a
`usize::MAX` overflow test on every new payload accessor — the 5.8 rule that a
crate's own accessors need it, not just `server-rt`'s. `mkfs-mfs` gains a
zero-length-entry round trip; `drivers/memory`'s `bdev.rs` gains
write-direction cases; VFS's `rw.rs` needs nothing new, which is the point of it
already being direction-agnostic.

**Boot markers.** `tests/qemu-boot.expected` gains `fs.write ok n=32768 v=3` and
changes `open.deny ok n=8` → `n=7`. `tests/qemu-boot.forbidden` gains the
`fs.write FAIL` prefix. Verified with `tools/check-boot-log.sh`.

**Mutation tests** — apply, observe the named marker move, revert from a
scratchpad copy taken *before the first run* (`git checkout` does not restore an
added file, it errors), and finish with `grep -rn MUTATION` over the tree as the
proof of cleanliness, never the restore command's exit status. Each mutation is
checked for `error\[E` in the log first, since a build failure is indistinguishable
from a working mutation:

| Mutation | Predicted marker |
|---|---|
| `alloc_zone` skips `bitmap_set` | second allocation returns the same zone → `fs.write FAIL verify` |
| `do_write` skips the inode size update | re-open sees size 0 → `fs.write FAIL verify` (read clamps at EOF) |
| a freshly allocated indirect block is not zeroed | garbage zone pointer → `fs.write FAIL` with `EIO`, or a verify mismatch |
| `BDEV_WRITE` uses `SAFECOPY_TO` | the buffer is overwritten instead of stored → verify mismatch |
| the re-pointed `bdev_denials` probe's grant flag | MFS's denial count marker |

**Boot matrix, all four rows** (the standing rule): default/SDK;
`MINIXRS_SDK=/nonexistent` (the in-tree musl flavour CI actually builds);
`--no-default-features`; and musl sysroot moved aside so `/bin/hello` holds the
`worker` ELF.

**Boot budget.** The write demo adds roughly 16 `FS_WRITE`s, each costing up to
seven BDEV round trips (bitmap read/write, indirect read/write, data read/write,
inode read/write), plus three `FS_READ`s — call it ~130 extra device round trips,
against the ~50 that staging `/bin/hello` already costs. That is a real increase
under TCG. The marker's byte-position as a fraction of a fixed-timeout log will be
measured against `HEAD` on the **musl** flavour (`grep -abo` the marker, divide by
`wc -c`, compare with the work stashed), and the `qemu-smoke` budget raised with
real headroom if it moved — not trimmed to whatever passes locally. Slice 5.9's
experience is the precedent: 45 s → 120 s, because CI's TCG is slower than this
machine.

---

## 9. Risks

**Free-space accounting is unproven at runtime.** Nothing today reads the zone
bitmap's occupancy, so a mkfs bug that marked the whole device in use would
surface as `ENOSPC` on init's first write rather than at build time. `verify.rs`
gains a free-zone count assertion, which is cheap and turns that into a build
failure.

**A partial failure leaks zones.** Invariant 5 makes the leak the *safe*
direction, but there is no `fsck` and no free-on-error unwind. Accepted for a
RAM-backed image whose lifetime is one boot; recorded here so 5.10b's create path
does not assume otherwise.

**The 4 KiB server stack.** No new stack buffer is introduced anywhere — MFS's
block buffer stays static, init's source is a `.rodata` static, and init's verify
window is 512 bytes. The largest-frame check
(`llvm-objdump | grep 'sub sp, sp, #0x…'`, converted to decimal before sorting)
runs on MFS and VFS before the PR, since both grew handlers.

**SDK flavour has no CI coverage.** Unchanged by this slice, but the boot matrix
above is the mitigation and is not optional.

---

## 10. Questions raised in review, and how they were settled

Settled 2026-08-18, before implementation. Recorded rather than deleted: each one
had a defensible alternative, and a future reader deserves to know it was
considered.

1. **`open.deny ok n=8` → `n=7`. Retire the probe.** §5.6's `write-file` probe is
   removed outright rather than re-pointed at a still-denied write (an unmapped
   buffer → `EFAULT`) that would have kept the count at 8. The changed count is
   the point: it forces a visible diff in `tests/qemu-boot.expected`, so the
   retirement is reviewed rather than absorbed. A marker string that survives
   while its meaning changes underneath is the exact failure 5.8 hit.

2. **Three verify windows, not a full read-back.** Offset 0, the 28672-byte
   direct/indirect seam, and the tail — 512 bytes each. The returned write count
   already proves every byte was accepted; the seam is the only place the
   indirect arm can hide; and ~60 additional round trips on a boot this slice is
   already lengthening buy a mid-file corruption case that no mutation in §8
   produces. If a future mutation *does* produce one, widen the windows rather
   than reading the whole file.

3. **`docs/plans/phase-5-musl-fs.md` points here.** That file's 5.10 section
   becomes a short summary naming the 5.10a/5.10b split and linking this
   document, rather than duplicating it. Its stale `◀ ready` marker on 5.9 is
   flipped to `✓ shipped (PR #52, merged 2026-08-04)` in the same edit, along
   with `docs/plan.md`'s — the reconciliation CLAUDE.md requires when opening a
   new slice.

---

## 11. What 5.10b will need

Recorded so 5.10a's shape does not foreclose it: `FS_CREATE` (parent inode +
name, or full path inline — the FS band's path-travels-inline rule), an inode
allocator (`bitmap_find_free`/`bitmap_set` are already general), directory-entry
insertion into a free `ino == 0` slot with directory growth when none is free,
`FS_TRUNC`, and a flags field in the `VFS_OPEN` payload for `O_CREAT` / `O_TRUNC`
/ `O_APPEND`. `EROFS` returns to VFS then, if at all, only for a full filesystem.

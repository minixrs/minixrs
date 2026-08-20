// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs MFS — the MinixFS v3 file-system server (slice 5.8; writable as of
//! slice 5.10a).
//!
//! The first *file system* in minix.rs, and the piece that makes a path resolve
//! to bytes. It sits between VFS and a block driver:
//!
//! ```text
//!   init ──VFS_OPEN/READ──► VFS ──FS_LOOKUP/READ──► MFS ──BDEV_READ──► memory
//!                            │                       │                    │
//!                            │                       └── grant: block buf ─┘
//!                            └── magic grant: init's buffer ───────────────┘
//! ```
//!
//! Slice 5.7 built everything under it — the compile-time MinixFS image, the
//! ramdisk driver, and the whole `minixrs-mfs` format library. This binary is the
//! glue: SEF startup, the FS-band receive loop, one block buffer, and the grants
//! at either end of it. **Every line with a decision in it is in the library**
//! (`proto.rs`, `walk.rs`), because this file is behind
//! `required-features = ["server"]` and therefore compiled by no CI job except the
//! QEMU boot smoke test — see the note in `Cargo.toml`.
//!
//! ## Four things worth knowing
//!
//! **The block buffer is a `.bss` static, not a `main`-frame local.** A boot
//! server's stack is exactly one page and a block is exactly one page, so a local
//! would put the frame base *below* the mapping — and that fault is turned into a
//! SIGSEGV by VM's out-of-region arm, which prints nothing the forbidden list
//! catches. This does not contradict TTY's "stage in `main`'s frame, not a
//! static": that rule's load-bearing half is *the buffer must outlive every call
//! that names it*, and a static satisfies it strictly better — the address never
//! changes, so the grant issued over it at boot never needs re-registering.
//!
//! **It is reached only through the [`Blocks`] capability token.** [`Blocks::read`]
//! takes `&mut self` and returns a reference borrowed from it, so "hold a
//! directory block across the next fetch" is a **borrow-check error** rather than
//! a promise. That is the aliasing hazard a two-buffer design would relocate
//! rather than remove, and it is a class of bug this repo has shipped before
//! (slice 5.3's `free_frame` / `is_usable_pa`). Every intermediate the walk needs
//! is a small `Copy` value: an `Inode`, a `u32` inode number, a `u32` zone.
//!
//! **Two error-relay rules, which read as contradictory and are not.** A failed
//! `BDEV_READ` becomes `EIO`: this server's client addressed a *file*, and the
//! device beneath it is an implementation detail whose errno would mean nothing
//! there. A failed `SYS_SAFECOPY` against VFS's grant is relayed **verbatim**,
//! because `EPERM` ("your grant does not authorize this") and `EFAULT` ("your
//! buffer is not mapped") are different bugs on the caller's side — the slice-5.3
//! rule.
//!
//! **Degraded, never fatal, and never a panic.** Past `sef_startup` every failure
//! leaves the server answering: a failed mount leaves the [`Mount`] as `None` and
//! every FS request answers `ENODEV`, the `memory` driver's `blocks = 0`
//! precedent. A spinning or panicking MFS would block VFS, which would block init,
//! which would take every slice-5.4-to-5.6 marker with it.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test`. The test harness needs `std` and its own entry point, so
// `no_std`/`no_main` and the `_start` shim below are gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

use core::cell::UnsafeCell;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_BLOCK_OFF, BDEV_GRANT_OFF, BDEV_LEN_OFF, BDEV_MAX_IO, BDEV_MINOR_OFF, BDEV_MINOR_RAMDISK,
    BDEV_READ, BDEV_RQ_BASE, BDEV_WRITE, FS_LOOKUP, FS_READ, FS_READSUPER, FS_WRITE, GET_RAMDISK,
    NR_BDEV_MSGS, SAFECOPY_FROM, SAFECOPY_TO, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{MEM_PROC_NR, PM_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{
    EFBIG, EINVAL, EIO, EISDIR, ENODEV, ENOENT, ENOSPC, ENOSYS, ENOTDIR, ENXIO, EPERM, OK,
};
use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE, GRANT_INVALID};
use minixrs_kernel_shared::rootfs::{
    IMAGE_HDR_LEN, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL, ROOTFS_IMAGE_BLOCKS, ROOTFS_MOTD,
    ROOTFS_MOTD_PATH, ROOTFS_PATTERN_PATH, ROOTFS_TAIL_BLOCK, rootfs_pattern_byte,
};
use minixrs_mfs::MFS_BLOCK_SIZE;
use minixrs_mfs::inode::{INODE_SIZE, Inode, NR_DIRECT_ZONES, ROOT_INODE, SINGLE_INDIRECT_SLOT};
use minixrs_mfs::layout::{Layout, layout};
use minixrs_mfs::read::{
    ZoneLookup, inode_at, inode_location, zone_for_offset, zone_from_indirect,
};
use minixrs_mfs::superblock::{SUPER_OFFSET, SUPER_ON_DISK_LEN, Superblock};
use minixrs_mfs::{proto, walk, write};
use minixrs_server_rt::{
    GrantPool, SefConfig, diag_fmt, sef_publish_to_ds, sef_retrieve_from_ds, sef_startup,
    sys_getinfo, sys_safecopy, wr_i32, wr_u64,
};

/// Simultaneously outstanding grants. Four are used — the block buffer's, and the
/// three malformed ones the denial battery needs — and the pool costs `N * 32`
/// bytes of `main`'s one-page frame, so 8 is ample headroom at 256 bytes.
const GRANT_SLOTS: usize = 8;

// ---------------------------------------------------------------------------
// The block buffer, and the capability that reaches it.
// ---------------------------------------------------------------------------

/// `UnsafeCell`-wrapped static block buffer. See the module note for why it is a
/// static, and [`Blocks`] for what stops it being aliased.
///
/// Same shape as `servers/ds/src/registry.rs`'s table and `servers/vm`'s regions:
/// a `#[repr(transparent)]` newtype with a hand-written `Sync`.
#[repr(transparent)]
struct BlockBuf(UnsafeCell<[u8; MFS_BLOCK_SIZE]>);

// SAFETY: MFS is a single EL0 thread running a straight-line receive loop with no
// interrupt handlers of its own, so there is never a second accessor. Every path
// to the bytes goes through `Blocks`, which is created exactly once in `main` and
// whose `&mut self` methods are what serialize access within that thread.
unsafe impl Sync for BlockBuf {}

static BLOCK: BlockBuf = BlockBuf(UnsafeCell::new([0u8; MFS_BLOCK_SIZE]));

/// The capability to fetch a block — one instance, created once in [`main`].
///
/// The whole point is the signature of [`Blocks::read`]: it takes `&mut self` and
/// returns a reference *borrowed from that*, so a caller cannot hold one block
/// while fetching the next. Every borrow-check error it produces is a real
/// aliasing bug — the kernel writes into this buffer during a `BDEV_READ`, which
/// happens inside `read` while `&mut self` is held and no shared reference is
/// outstanding.
struct Blocks {
    /// The block driver, resolved through DS at boot.
    mem: Endpoint,
    /// Grant naming [`BLOCK`] with the driver as grantee, `CPF_READ | CPF_WRITE`.
    /// Issued once at boot: the buffer is a static, so its address never changes
    /// and `GrantPool::ensure_registered` never re-fires.
    ///
    /// **Both directions on one grant** (W5): the driver writes into this buffer
    /// on a `BDEV_READ` and reads out of it on a `BDEV_WRITE`, and the grantee is
    /// the same driver either way. A second grant would name the same bytes to
    /// the same peer. The kernel checks the direction bit per call, so widening
    /// the flags does not widen what any single call may do.
    ///
    /// [`GRANT_INVALID`] if that grant could not be issued, in which case every
    /// read fails the kernel's grant check and the server degrades to `ENODEV` —
    /// the `memory` driver's `blocks = 0` state, one layer up.
    gid: i32,
}

impl Blocks {
    /// Address of the block buffer, for the grant issued over it. Constant for
    /// the life of the process.
    fn addr() -> u64 {
        BLOCK.0.get() as usize as u64
    }

    /// Fetch block `block` from the device, replacing the buffer's contents.
    ///
    /// A driver failure — a bad grant, a block past the end, a short reply — is
    /// [`EIO`]: the caller addressed a *file*, so the device's own errno would be
    /// answering a question it did not ask. A short reply is included on purpose;
    /// this server cannot interpret a fraction of a block, which is exactly why
    /// `BDEV_READ` refuses an over-long request rather than clamping it.
    fn read(&mut self, block: u64) -> Result<&[u8; MFS_BLOCK_SIZE], i32> {
        let rc = bdev_request(
            self.mem,
            BDEV_READ,
            BDEV_MINOR_RAMDISK,
            self.gid,
            MFS_BLOCK_SIZE as i32,
            block,
        );
        if rc != MFS_BLOCK_SIZE as i32 {
            return Err(EIO);
        }
        // SAFETY: `&mut self` is held, so no other reference into the buffer is
        // outstanding, and the `BDEV_READ` above — the only thing that writes
        // these bytes — has completed. The returned lifetime is elided to
        // `&mut self`'s, so the borrow checker keeps it that way.
        Ok(unsafe { &*BLOCK.0.get() })
    }

    /// The buffer, zeroed — what a **hole** reads as.
    ///
    /// A hole is a zone pointer that is legitimately zero: the file is sparse
    /// there and reading it yields zeroes, which is what a hole *means*. Reusing
    /// the block buffer rather than carrying a second zero page is the whole
    /// reason this is a method — there is no spare page to carry.
    ///
    /// `tools/mkfs-mfs` writes no sparse files, so nothing in the boot image
    /// reaches this. But a hole is a legal image, and answering one with stale
    /// bytes from the previous block would be silent corruption.
    fn zeroed(&mut self) -> &[u8; MFS_BLOCK_SIZE] {
        // SAFETY: as `read` above — `&mut self` is held, so this is the only
        // reference into the buffer, and the `&mut` the fill borrows dies at the
        // end of that statement, before the shared reference is formed.
        unsafe {
            let p = BLOCK.0.get();
            (*p).fill(0);
            &*p
        }
    }

    /// The block buffer, mutable — for the splice a partial write performs.
    ///
    /// `&mut self` for the reason [`Blocks::read`] takes it: this hands out the
    /// only reference into the buffer, and the borrow checker is what keeps it
    /// the only one.
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
}

/// A mounted filesystem: what the superblock said, plus the layout derived from
/// it.
///
/// All `Copy` scalars, and there is **no per-open state anywhere in this server** —
/// which is why the FS band needs no `PUTNODE`: there is no node to put.
#[derive(Copy, Clone)]
struct Mount {
    root: u32,
    block_size: usize,
    blocks: u32,
    layout: Layout,
}

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` can
/// dive straight into Rust without setting up a stack itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main` (and
// the helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let sef = sef_startup(SefConfig {
        init_fresh: Some(mfs_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        // No recovery and nothing to print: a failed handshake means this server
        // never learned its own endpoint, so it cannot even announce the failure.
        loop {
            core::hint::spin_loop()
        }
    });

    // `main`-frame values that outlive the receive loop. The grant pool cannot
    // live in `init_fresh`, whose frame is gone by the time a grantee safecopies —
    // the rule `server-rt` keeps `GrantPool` a value rather than a static for.
    let mut grants: GrantPool<GRANT_SLOTS> = GrantPool::new();
    let mut blocks = device(&mut grants, mem_endpoint());
    let mut mount = mount_root(&mut blocks);

    if let Some(m) = mount {
        selfcheck(&mut blocks, &m);
    }
    // The two probes that exercise the *device* rather than the filesystem run
    // last, and the order is load-bearing in the direction the denial battery
    // makes it: those requests are deliberately malformed, so a driver that
    // wedged on one would take everything after it down with it. Putting them
    // behind the filesystem proof means such a wedge localizes to the `bdev.*`
    // markers instead of blacking out `mount` / `fs.selfcheck` / `fs.indirect`.
    // Do not tidy this prologue into alphabetical order.
    tail_probe(&mut blocks);
    bdev_denials(&mut grants, &blocks);

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        // Capture the caller *first*: it is both the reply target and the granter
        // of any buffer this request names, and the dispatch below overwrites
        // `msg.m_source` on the way out.
        let caller_e = msg.m_source;
        let rc = match msg.m_type {
            FS_READSUPER => do_readsuper(&mut msg, &mut blocks, &mut mount),
            FS_LOOKUP => do_lookup(&mut msg, &mut blocks, &mount),
            FS_READ => do_read(&msg, caller_e, &mut blocks, &mount),
            FS_WRITE => do_write(&msg, caller_e, &mut blocks, &mount),
            // Reply rather than drop (TTY's rule): this server's clients are all
            // inside a SENDREC, and a dropped request blocks the caller forever.
            _ => ENOSYS,
        };
        reply(caller_e, &mut msg, rc);
    }
}

/// SEF fresh-init callback: publish to DS. Nothing else — the device and the
/// mount are set up in `main`'s frame, for the reason [`Blocks`] documents.
#[cfg_attr(test, allow(dead_code))]
fn mfs_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(name)
}

// ---------------------------------------------------------------------------
// Request handlers.
// ---------------------------------------------------------------------------

/// Serve one `FS_READSUPER`: re-read the device's superblock and report it.
///
/// **It really re-reads**, rather than replaying what [`mount_root`] decoded at
/// boot. Two things follow, both wanted: VFS's own `fs.mount` marker is an
/// independent round trip rather than an echo of MFS's, and a boot-time mount
/// failure is retried the first time a client asks — the same "failure is not
/// cached" stance VFS takes on its lazy root mount.
///
/// The converse is deliberate too: **a failed re-read leaves a previously
/// successful mount installed**, because `*mount` is only assigned on success. A
/// transient device error must not unmount a filesystem other descriptors are
/// still reading through — the failure is reported to this one caller and nothing
/// else changes.
#[cfg_attr(test, allow(dead_code))]
fn do_readsuper(msg: &mut Message, blocks: &mut Blocks, mount: &mut Option<Mount>) -> i32 {
    if proto::parse_readsuper(msg) != BDEV_MINOR_RAMDISK {
        // The *device* does not exist. Nothing is wrong with the request itself.
        return ENXIO;
    }
    let m = match read_super(blocks) {
        Ok(m) => m,
        Err(e) => return e,
    };
    *mount = Some(m);
    proto::reply_readsuper(msg, m.root, m.block_size, m.blocks);
    OK
}

/// Serve one `FS_LOOKUP`: resolve a path to `(inode, mode, size)`.
#[cfg_attr(test, allow(dead_code))]
fn do_lookup(msg: &mut Message, blocks: &mut Blocks, mount: &Option<Mount>) -> i32 {
    let Some(mount) = mount else {
        return ENODEV;
    };
    // The path borrows `*msg`; the walk's result is `Copy`, so that borrow is over
    // before the reply is written back into the same message.
    let found = match proto::parse_lookup(msg).and_then(walk::parse_path) {
        Ok(path) => lookup(blocks, mount, path),
        Err(e) => Err(e),
    };
    match found {
        Ok((ino, node)) => {
            proto::reply_lookup(msg, ino, node.mode, node.size);
            OK
        }
        Err(e) => e,
    }
}

/// Serve one `FS_READ`. Returns the reply `m_type`: bytes read (`>= 0`, `0` is
/// EOF), or a negative errno.
///
/// The checks, in order — each is the first thing that can be wrong given the
/// ones before it:
///
/// 1. Something is mounted (`ENODEV`).
/// 2. The inode number is a non-negative `u32` (`EINVAL`). A client got it from an
///    `FS_LOOKUP`, so anything else is a malformed request rather than a missing
///    file.
/// 3. The inode reads back and names a regular file — `EISDIR` for a directory
///    specifically, because "you cannot read a directory with `read`" is a
///    different thing to tell a caller than "that is not a file at all".
/// 4. The length and position clamp to something transferable
///    ([`walk::clamp_read`]), and `0` bytes — end of file — replies **before** the
///    grant is touched, so a client polling at EOF cannot use it to probe the
///    granting path. TTY and VFS apply the same rule to a zero-length write.
///
/// Then the copy. `granter` is the **kernel-stamped `m_source`** — the one fact in
/// this request a client cannot forge. There is no payload field for it and there
/// must never be one: this server holds `SYS_SAFECOPY`, so a caller-supplied
/// granter would aim a privileged cross-address-space copy wherever the caller
/// pointed.
#[cfg_attr(test, allow(dead_code))]
fn do_read(msg: &Message, granter: Endpoint, blocks: &mut Blocks, mount: &Option<Mount>) -> i32 {
    let Some(mount) = mount else {
        return ENODEV;
    };
    let req = proto::parse_read(msg);
    let Ok(ino) = u32::try_from(req.ino) else {
        return EINVAL;
    };

    let node = match read_inode(blocks, mount, ino) {
        Ok(node) => node,
        Err(e) => return e,
    };
    if node.is_dir() {
        return EISDIR;
    }
    if !node.is_reg() {
        return EINVAL;
    }

    let chunk = match walk::clamp_read(node.size, req.pos, req.len, mount.block_size) {
        Ok(chunk) => chunk,
        Err(e) => return e,
    };
    if chunk.len == 0 {
        return 0;
    }

    let src = match fetch(blocks, mount, &node, req.pos) {
        Ok(blk) => blk,
        Err(e) => return e,
    };
    let Some(bytes) = chunk
        .off_in_block
        .checked_add(chunk.len)
        .and_then(|end| src.get(chunk.off_in_block..end))
    else {
        // Unreachable: `clamp_read` guarantees a chunk lies inside one block. Say
        // `EIO` rather than indexing, so a future clamp bug is an errno instead of
        // a panic in a server nothing can restart.
        return EIO;
    };

    let rc = sys_safecopy(
        SAFECOPY_TO,
        granter,
        req.gid,
        0,
        bytes.as_ptr() as usize as u64,
        chunk.len as u64,
    );
    if rc != OK {
        // Verbatim: `EPERM` ("your grant does not authorize this") and `EFAULT`
        // ("your buffer is not mapped") are different bugs on the caller's side.
        return rc;
    }
    chunk.len as i32
}

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
/// begins, so there is never a moment where two blocks are wanted at once.
///
///   1. Read the inode. [`Inode`] is `Copy`, so the buffer is free again at the
///      `let`.
///   2. Clamp, and compute the resulting size. Both are pure, and this is where
///      `EFBIG` is decided — before any device work, so a rejected request has
///      allocated nothing.
///   3. Resolve or allocate the zone. A freshly allocated zone is **zeroed and
///      written before its number is stored anywhere** (W4): the bitmap bit goes
///      first, so a failure between the two leaks a zone rather than sharing one.
///   4. Read the target block unless the write covers it whole, splice the
///      caller's bytes in through `SAFECOPY_FROM`, store the block.
///   5. Write the inode back if it changed.
///
/// **What the [`Blocks`] token does and does not guarantee.** It guarantees
/// *aliasing*: no block can be held across another's fetch, because every method
/// takes `&mut self` and hands back a borrow tied to it, so the borrow checker
/// rejects the attempt. It does **not** guarantee *identity* — [`Blocks::write`]
/// flushes whatever is resident under the block number it is handed, so
/// `buf_mut(); …; write(other)` would store one block's bytes as another and
/// still compile. Every call site here is therefore responsible for having made
/// the resident block the one it names, and the two that flush say which block
/// they just filled: step 4 writes `zone`, which it either read at the top of the
/// step or is about to overwrite whole.
///
/// **A failure mid-write leaks a zone, and this is a reachable denial of service
/// — an accepted limitation, not a benign one.** Step 3 allocates before step 4
/// copies, so a safecopy that fails leaves the bitmap bit set with the inode
/// never written back. The copy is *client-controlled*: `rw::validate` in VFS
/// range-checks the caller's buffer but cannot check that it is mapped (the
/// kernel's page-table walk is the gate — D5), so `write(fd, unmapped_va, 4096)`
/// reaches here with a well-formed magic grant and fails at the copy. Looping it
/// exhausts the image's free zones — **185 in the musl flavour**, so 93–185 calls
/// — and every later write, legitimate ones included, answers `ENOSPC` for the
/// rest of the boot. Nothing in the shipped boot reaches it (init writes a good
/// buffer; `worker` and `hello` write no files), which is why it is deferred
/// rather than fixed here.
///
/// **Whoever fixes this must not simply clear the bit on the error path.** The
/// three outcomes differ, and only two leak:
///
///   * *Direct slot, new zone* — the bit is durable, `node.zone[i]` is not.
///     Orphaned; a retry allocates again. **Leaks, 1 zone per attempt.**
///   * *Indirect slot, indirect block already existed* — the bit is durable and
///     so is the indirect block that points at the zone. A retry finds
///     `existing != 0` and reuses it. **Does not leak**, and clearing the bit
///     here would hand out a zone the indirect block still names: two files
///     sharing one, which is the corruption this ordering exists to prevent.
///   * *Indirect slot, indirect block also new* — two bits durable, nothing
///     durable referencing either. **Leaks, 2 zones per attempt.**
///
/// The cheaper fix is not a rollback at all: stage the caller's bytes into a
/// second buffer *before* step 3, so no client-controlled failure can occur after
/// an allocation. That costs one page of `.bss` — the one-page limit in this
/// server is the *stack*, not `.bss` — and buys a one-line invariant in place of
/// the table above. Deferred to slice 5.10b, which reworks this path for
/// `O_CREAT` regardless.
///
/// **`mtime` and `ctime` are not updated, on purpose.** There is no clock a
/// user-space filesystem can read yet — MFS holds no `SYS_SETALARM` grant and
/// there is no time server — so a written file keeps the timestamps
/// `tools/mkfs-mfs` stamped into it. Inventing a value would be worse than an
/// obviously stale one, and this becomes a real field to fill the moment a clock
/// is reachable.
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
        // A legal zero-length write, answered before the grant is touched, so a
        // client polling with `len = 0` cannot use it to probe the granting path.
        // TTY, VFS and the `memory` driver all apply the same rule.
        return 0;
    }
    // Computed here rather than at step 5, where it reads more naturally: it is a
    // pure function of values already in hand, and evaluating it after a zone has
    // been allocated and its bitmap bit written would leak that zone on an `EFBIG`
    // this could have reported before touching the device at all.
    let grown = match write::grow_size(node.size, req.pos, chunk.len) {
        Ok(s) => s,
        Err(e) => return e,
    };

    // Step 3. `dirty` records whether the inode changed at all — see step 5.
    let (zone, mut dirty) = match place_zone(blocks, mount, &mut node, req.pos) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Step 4. The guard names `MFS_BLOCK_SIZE`, not `mount.block_size`, because
    // it is `Blocks::write`'s flush length that has to be fully covered — the
    // mount's block size is checked equal to it at mount time, but this is the
    // constant the skip actually depends on, and `lib.rs`'s `const _` chain pins
    // `FS_MAX_IO >= MFS_BLOCK_SIZE == BDEV_BLOCK_SIZE` so a clamped chunk can
    // reach it.
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
    // that pointer while leaving its bitmap bit set — the bitmap and the inode
    // disagreeing about a live zone, which is corruption rather than a leak.
    if grown != node.size {
        node.size = grown;
        dirty = true;
    }
    if dirty && let Err(e) = write_inode(blocks, mount, ino, &node) {
        return e;
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
            let existing = *node.zone.get(i).ok_or(EIO)?;
            if existing != 0 {
                return if write::write_zone_ok(existing, mount.layout.first_data_zone, mount.blocks)
                {
                    Ok((existing, false))
                } else {
                    Err(EIO)
                };
            }
            let zone = alloc_zone(blocks, mount)?;
            *node.zone.get_mut(i).ok_or(EIO)? = zone;
            Ok((zone, true))
        }
        write::ZoneSlot::Indirect(slot) => {
            let mut dirty = false;
            let mut indirect = node.zone[SINGLE_INDIRECT_SLOT];
            if indirect == 0 {
                // The indirect block is allocated through the same path as a data
                // zone, which is what makes it *zeroed*: every one of its 1024
                // slots reads back as a hole rather than as whatever the previous
                // owner left there, which this code would take for zone pointers.
                indirect = alloc_zone(blocks, mount)?;
                node.zone[SINGLE_INDIRECT_SLOT] = indirect;
                dirty = true;
            }
            if !write::write_zone_ok(indirect, mount.layout.first_data_zone, mount.blocks) {
                return Err(EIO);
            }
            // Read the pointer out and let the borrow die immediately: `u32` is
            // `Copy`, so nothing points into the buffer when the next fetch
            // replaces it. `resolve_zone` is split for exactly this reason.
            let blk = blocks.read(u64::from(indirect))?;
            let existing = zone_from_indirect(blk, slot).ok_or(EIO)?;
            if existing != 0 {
                return if write::write_zone_ok(existing, mount.layout.first_data_zone, mount.blocks)
                {
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
                .and_then(|s| Some((s, s.checked_add(4)?)))
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
/// `fsck`; a shared zone is silent corruption. See [`do_write`] for why that leak
/// is a *reachable* denial of service rather than a benign one, and for the case
/// in which clearing the bit again would be the corruption this avoids.
///
/// The scan is bounded by `layout.zmap_blocks` — every device-derived loop has a
/// cap, because a corrupt superblock must not spin this server and, through it,
/// VFS and init.
#[cfg_attr(test, allow(dead_code))]
fn alloc_zone(blocks: &mut Blocks, mount: &Mount) -> Result<u32, i32> {
    let bits_per_block = u32::try_from(mount.block_size.checked_mul(8).ok_or(EIO)?)
        .ok()
        .filter(|&b| b != 0)
        .ok_or(EIO)?;
    // The bitmap is based at `first_data_zone - 1` (MINIX's convention, recorded
    // in `layout::zmap_bit`), so bit 1 is the first data zone. `checked_sub`
    // rather than `-`: servers ship with `overflow-checks = false`, where an
    // underflow wraps silently and would hand out a wild zone number.
    let base = mount.layout.first_data_zone.checked_sub(1).ok_or(EIO)?;
    let limit = mount.blocks.saturating_sub(base);

    for i in 0..mount.layout.zmap_blocks {
        let block = mount.layout.zmap_start.checked_add(i).ok_or(EIO)?;
        let from = i.checked_mul(bits_per_block).ok_or(EIO)?;
        if from >= limit {
            break;
        }
        // The zone bitmap is deliberately over-sized (`layout.rs`'s module docs),
        // so the tail of the last block describes zones that do not exist.
        let in_block_limit = limit.saturating_sub(from).min(bits_per_block);
        let buf = blocks.read(u64::from(block))?;
        // Bit 0 of the whole bitmap is reserved: it names a zone below
        // `first_data_zone` and is always marked in use.
        let start = u32::from(i == 0);
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
        if !write::write_zone_ok(zone, mount.layout.first_data_zone, mount.blocks) {
            return Err(EIO);
        }
        // W4: zero it before anyone can reach it. A fresh zone otherwise holds
        // whatever the previous owner left — and for an indirect block, that
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
///
/// No `zone_ok` check on the block, unlike [`read_inode`]: every caller has
/// already read this same inode through that function, so the block it names was
/// checked there — and if it were not, the driver's own range check makes the
/// fetch below `EIO` rather than a write to a block outside the device.
#[cfg_attr(test, allow(dead_code))]
fn write_inode(blocks: &mut Blocks, mount: &Mount, ino: u32, node: &Inode) -> Result<(), i32> {
    let (block, slot) = inode_location(ino, &mount.layout, mount.block_size).ok_or(EINVAL)?;
    blocks.read(u64::from(block))?;
    let buf = blocks.buf_mut();
    let start = slot.checked_mul(INODE_SIZE).ok_or(EIO)?;
    let end = start.checked_add(INODE_SIZE).ok_or(EIO)?;
    let cell = buf.get_mut(start..end).ok_or(EIO)?;
    cell.copy_from_slice(&node.to_le_bytes());
    blocks.write(u64::from(block))
}

// ---------------------------------------------------------------------------
// The reader. `tools/mkfs-mfs`'s `verify.rs` is the reference implementation —
// same decoders, same zone resolver, same walk, with `block()` replaced by a
// `BDEV_READ` and every `Vec` replaced by streaming through one buffer.
// ---------------------------------------------------------------------------

/// Decode the device's superblock and derive its layout.
#[cfg_attr(test, allow(dead_code))]
fn read_super(blocks: &mut Blocks) -> Result<Mount, i32> {
    let blk = blocks.read(0)?;
    let raw = blk
        .get(SUPER_OFFSET..SUPER_OFFSET + SUPER_ON_DISK_LEN)
        .ok_or(EIO)?;
    let sb = Superblock::from_le_bytes(raw).ok_or(EINVAL)?;
    // `EINVAL`, not `EIO`: the device answered perfectly well, it just does not
    // hold a filesystem this build reads.
    sb.validate().map_err(|_| EINVAL)?;
    if sb.block_size as usize != MFS_BLOCK_SIZE {
        return Err(EINVAL);
    }
    Ok(Mount {
        root: ROOT_INODE,
        block_size: MFS_BLOCK_SIZE,
        blocks: sb.zones,
        layout: layout(sb.ninodes, sb.zones, MFS_BLOCK_SIZE),
    })
}

/// Fetch inode `ino`.
///
/// `EINVAL` when the number is not addressable at all (zero, or past the inode
/// table) — a statement about the *number*, which is what both callers passed in.
/// `EIO` when the block cannot be read or the slot cannot be decoded — a statement
/// about the *image*. A corrupt directory entry naming an inode past the table
/// therefore surfaces as `EINVAL`, which is the one imprecision in that split.
#[cfg_attr(test, allow(dead_code))]
fn read_inode(blocks: &mut Blocks, mount: &Mount, ino: u32) -> Result<Inode, i32> {
    let (block, slot) = inode_location(ino, &mount.layout, mount.block_size).ok_or(EINVAL)?;
    if !walk::zone_ok(block, mount.blocks) {
        return Err(EIO);
    }
    let blk = blocks.read(u64::from(block))?;
    inode_at(blk, slot).ok_or(EIO)
}

/// Which zone backs byte `pos` of `node` — `None` for a hole.
///
/// Split out of [`fetch`] rather than inlined so the indirect block's borrow of
/// the buffer provably ends before the data block replaces it: this returns a
/// `u32`, so nothing is left pointing into the buffer when it does.
#[cfg_attr(test, allow(dead_code))]
fn resolve_zone(
    blocks: &mut Blocks,
    mount: &Mount,
    node: &Inode,
    pos: u64,
) -> Result<Option<u32>, i32> {
    match zone_for_offset(node, pos, mount.block_size) {
        ZoneLookup::Direct(zone) => Ok(Some(zone)),
        ZoneLookup::Indirect { zone, slot } => {
            if !walk::zone_ok(zone, mount.blocks) {
                return Err(EIO);
            }
            let blk = blocks.read(u64::from(zone))?;
            let z = zone_from_indirect(blk, slot).ok_or(EIO)?;
            // An unused slot of an indirect block is zero, which is a hole
            // exactly like a zero direct pointer.
            Ok(if z == 0 { None } else { Some(z) })
        }
        ZoneLookup::Hole => Ok(None),
        // Past what the single-indirect span can address. `EIO` rather than
        // zeroes: the format cannot describe this offset, so answering with data
        // would be inventing it. See `read.rs`'s module docs.
        ZoneLookup::OutOfRange => Err(EIO),
    }
}

/// Fetch the block backing byte `pos` of `node`, holes included.
#[cfg_attr(test, allow(dead_code))]
fn fetch<'a>(
    blocks: &'a mut Blocks,
    mount: &Mount,
    node: &Inode,
    pos: u64,
) -> Result<&'a [u8; MFS_BLOCK_SIZE], i32> {
    match resolve_zone(blocks, mount, node, pos)? {
        Some(zone) if walk::zone_ok(zone, mount.blocks) => blocks.read(u64::from(zone)),
        // A zone the image names but the device cannot hold.
        Some(_) => Err(EIO),
        None => Ok(blocks.zeroed()),
    }
}

/// Resolve an absolute path to `(inode number, inode)`.
///
/// `"/"` is the root, and resolves without a single directory read. Each component
/// is looked up through its parent's own entries, so a path naming a file as an
/// intermediate component is `ENOTDIR` rather than a read of that file's bytes as
/// directory entries — `verify.rs`'s rule.
#[cfg_attr(test, allow(dead_code))]
fn lookup(blocks: &mut Blocks, mount: &Mount, path: &str) -> Result<(u32, Inode), i32> {
    let mut ino = mount.root;
    let mut node = read_inode(blocks, mount, ino)?;
    for component in walk::components(path) {
        if !node.is_dir() {
            return Err(ENOTDIR);
        }
        ino = find_component(blocks, mount, &node, component)?;
        node = read_inode(blocks, mount, ino)?;
    }
    Ok((ino, node))
}

/// Find `name` in directory `dir`, streaming one block at a time.
///
/// `verify.rs` materializes the whole directory into a `Vec` and iterates it;
/// there is no allocator here and no second buffer, so this asks about each block
/// in turn and keeps nothing but a `u32`. [`walk::dir_size`] is what bounds the
/// loop — a corrupt inode claiming `size = i32::MAX` would otherwise spin — and
/// [`walk::next_dir_chunk`] is the round itself, in the library because this file
/// is compiled by no CI job.
#[cfg_attr(test, allow(dead_code))]
fn find_component(blocks: &mut Blocks, mount: &Mount, dir: &Inode, name: &str) -> Result<u32, i32> {
    let size = walk::dir_size(dir.size)?;
    let mut off = 0usize;
    while let Some(want) = walk::next_dir_chunk(off, size, mount.block_size) {
        if let Some(ino) = scan_block(blocks, mount, dir, off as u64, want, name)? {
            return Ok(ino);
        }
        // `want <= size - off` by construction, so this cannot overflow — but
        // `--release` builds this crate with `overflow-checks = false`, where a
        // bare `+` would wrap silently rather than reproduce the `cargo test`
        // panic, so every offset add in `fs/` is spelled out.
        off = off.checked_add(want).ok_or(EIO)?;
    }
    Err(ENOENT)
}

/// Search one directory block. Returns the inode number **by value**, so the
/// buffer's borrow is over before the caller fetches that inode's own block.
#[cfg_attr(test, allow(dead_code))]
fn scan_block(
    blocks: &mut Blocks,
    mount: &Mount,
    dir: &Inode,
    pos: u64,
    want: usize,
    name: &str,
) -> Result<Option<u32>, i32> {
    let blk = fetch(blocks, mount, dir, pos)?;
    Ok(walk::find_in_block(blk.get(..want).ok_or(EIO)?, name))
}

/// Read up to one chunk of `path` at `pos`, borrowing the bytes out of the block
/// buffer.
///
/// The boot probes' shared machinery. It deliberately stops short of a safecopy —
/// there is no second address space involved and no second buffer to copy into —
/// so what it proves is the *reader*: lookup, inode decode, zone resolution, and
/// one block fetch, over the real image.
#[cfg_attr(test, allow(dead_code))]
fn read_chunk<'a>(
    blocks: &'a mut Blocks,
    mount: &Mount,
    path: &str,
    pos: u64,
    len: usize,
) -> Result<&'a [u8], i32> {
    let (_, node) = lookup(blocks, mount, path)?;
    if !node.is_reg() {
        return Err(EINVAL);
    }
    let chunk = walk::clamp_read(node.size, pos, len as i32, mount.block_size)?;
    if chunk.len == 0 {
        return Ok(&[]);
    }
    let blk = fetch(blocks, mount, &node, pos)?;
    let end = chunk.off_in_block.checked_add(chunk.len).ok_or(EIO)?;
    blk.get(chunk.off_in_block..end).ok_or(EIO)
}

// ---------------------------------------------------------------------------
// Boot prologue. Everything below is best-effort: a failure is reported and
// stepped over, never fatal.
// ---------------------------------------------------------------------------

/// Resolve the `memory` driver's endpoint through DS, falling back to its boot
/// endpoint.
///
/// The lookup is the point — a client should not hard-code a peer's boot proc
/// number — but DS publish-before-retrieve is **not** guaranteed by construction.
/// It works because `kernel/build.rs` packs `memory` before `mfs`. Rather than let
/// that archive ordering become load-bearing, a failed lookup falls back and emits
/// a *distinguishable* diag line: the rest of the boot still runs and still proves
/// the FS path, while the required `bdev.ds ok` marker disappears and CI goes red
/// on the ordering regression specifically.
#[cfg_attr(test, allow(dead_code))]
fn mem_endpoint() -> Endpoint {
    let mut key = [0u8; SYS_GETINFO_NAME_LEN];
    key[0..6].copy_from_slice(b"memory");
    match sef_retrieve_from_ds(&key) {
        Ok(ep) => {
            diag_fmt(format_args!("bdev.ds ok ep={ep}"));
            ep
        }
        Err(rc) => {
            let ep = boot_endpoint(MEM_PROC_NR);
            diag_fmt(format_args!("bdev.ds FAIL rc={rc} fallback={ep}"));
            ep
        }
    }
}

/// Grant the block buffer to the driver and hand back the fetch capability.
///
/// **Issued once, and never re-issued.** The buffer is a static, so its address
/// cannot change and `GrantPool::ensure_registered` never fires again — the
/// concrete benefit of the buffer not living in a stack frame.
///
/// On failure the grant id is [`GRANT_INVALID`], every read fails the kernel's
/// grant check, the mount fails, and the server answers `ENODEV` to everything:
/// degraded and still replying, the `memory` driver's `blocks = 0` precedent.
#[cfg_attr(test, allow(dead_code))]
fn device(grants: &mut GrantPool<GRANT_SLOTS>, mem: Endpoint) -> Blocks {
    // Both directions on one grant: `CPF_WRITE` because the driver copies *into*
    // this buffer on a `BDEV_READ`, `CPF_READ` because it copies *out of* it on a
    // `BDEV_WRITE`. The kernel checks the direction bit per call, so this is not a
    // widening of what any single request may do — see the `gid` field's docs.
    let gid = match grants.grant_direct(
        mem,
        Blocks::addr(),
        MFS_BLOCK_SIZE as u64,
        CPF_READ | CPF_WRITE,
    ) {
        Ok(gid) => gid,
        Err(rc) => {
            diag_fmt(format_args!("mount FAIL grant rc={rc}"));
            GRANT_INVALID
        }
    };
    Blocks { mem, gid }
}

/// Mount the ramdisk and announce the geometry.
///
/// `blocks=` comes from the superblock's `s_zones`, while the `memory` driver's
/// own `blocks=` marker comes from the image *header* — two independently derived
/// numbers that must agree, which is what makes the pair worth more than either.
#[cfg_attr(test, allow(dead_code))]
fn mount_root(blocks: &mut Blocks) -> Option<Mount> {
    match read_super(blocks) {
        Ok(m) => {
            diag_fmt(format_args!(
                "mount ok root={} bs={} blocks={}",
                m.root, m.block_size, m.blocks
            ));
            Some(m)
        }
        Err(rc) => {
            diag_fmt(format_args!("mount FAIL rc={rc}"));
            None
        }
    }
}

/// Byte offset at which `/etc/pattern` crosses into its single-indirect zones.
const INDIRECT_POS: u64 = (NR_DIRECT_ZONES * MFS_BLOCK_SIZE) as u64;

/// Bytes each boot probe compares. Small on purpose: nothing is copied out of the
/// block buffer, so this is a comparison length rather than a staging size.
const PROBE_LEN: usize = 32;

/// Prove the whole reader over the real image, twice, at the two offsets that
/// exercise different arms of it.
///
/// **`/etc/motd`** is the direct-zone case, compared **byte for byte** against
/// [`ROOTFS_MOTD`] — the constant `kernel/build.rs` built the image from, so this
/// is a check rather than a transcription. Content, not length: a path that moved
/// the right *number* of wrong bytes is the bug a length check cannot see.
///
/// **`/etc/pattern` at [`INDIRECT_POS`]** is the single-indirect case, and it is
/// **the only thing in the whole boot that reaches that arm** — which is why slice
/// 5.7 made that file mandatory rather than filler. Keyed on `/etc/pattern` and
/// not on `/bin/hello`: `hello` is ~200 KB with a real C toolchain but the 15 KB
/// `worker` ELF in the sysroot-absent fallback, which fits inside the seven direct
/// zones, so a proof keyed on it would be vacuous in exactly the configuration
/// CI's non-QEMU jobs build. This file's size is constant in both.
#[cfg_attr(test, allow(dead_code))]
fn selfcheck(blocks: &mut Blocks, mount: &Mount) {
    match read_chunk(blocks, mount, ROOTFS_MOTD_PATH, 0, ROOTFS_MOTD.len()) {
        Ok(got) if got == ROOTFS_MOTD => {
            diag_fmt(format_args!("fs.selfcheck ok n={} match=1", got.len()))
        }
        Ok(got) => diag_fmt(format_args!("fs.selfcheck FAIL n={}", got.len())),
        Err(rc) => diag_fmt(format_args!("fs.selfcheck FAIL rc={rc}")),
    }

    match read_chunk(blocks, mount, ROOTFS_PATTERN_PATH, INDIRECT_POS, PROBE_LEN) {
        Ok(got) if got.len() == PROBE_LEN && pattern_matches(got, INDIRECT_POS) => {
            diag_fmt(format_args!("fs.indirect ok match=1"))
        }
        Ok(got) => diag_fmt(format_args!("fs.indirect FAIL n={}", got.len())),
        Err(rc) => diag_fmt(format_args!("fs.indirect FAIL rc={rc}")),
    }
}

/// Do `got`'s bytes equal `/etc/pattern`'s contents from file offset `pos`?
///
/// `false` rather than a panic for an offset that does not fit a `usize` or whose
/// arithmetic would wrap: both are unreachable (`pos` is a compile-time constant
/// here), but this crate ships with `overflow-checks = false`, so a bare `+` would
/// wrap into a *matching* byte in release while panicking under `cargo test`.
#[cfg_attr(test, allow(dead_code))]
fn pattern_matches(got: &[u8], pos: u64) -> bool {
    let Ok(base) = usize::try_from(pos) else {
        return false;
    };
    got.iter().enumerate().all(|(i, &b)| {
        base.checked_add(i)
            .is_some_and(|off| b == rootfs_pattern_byte(off))
    })
}

/// Read the image's reserved last block and check its label — slice 5.7's probe,
/// relocated here now that MFS is the block device's real client.
///
/// The label deliberately differs from the header's, which is what distinguishes
/// "the blob was copied" from "the blob's first page was copied 256 times", and
/// what proves the `block` field reaches the right page: the driver's own `tail=1`
/// check proves only the kernel's copy loop, not BDEV indexing.
#[cfg_attr(test, allow(dead_code))]
fn tail_probe(blocks: &mut Blocks) {
    match blocks.read(u64::from(ROOTFS_TAIL_BLOCK)) {
        Ok(blk) if blk[..IMAGE_LABEL_LEN] == IMAGE_TAIL_LABEL => {
            diag_fmt(format_args!("bdev.tail ok match=1"))
        }
        Ok(_) => diag_fmt(format_args!("bdev.tail FAIL label")),
        Err(rc) => diag_fmt(format_args!("bdev.tail FAIL rc={rc}")),
    }
}

/// One denial probe: a request well-formed in every respect but one.
#[cfg_attr(test, allow(dead_code))]
struct Probe {
    name: &'static str,
    m_type: i32,
    minor: i32,
    gid: i32,
    len: i32,
    block: u64,
    want: i32,
}

/// Probe every `BDEV_READ` refusal a successful read does not exercise — slice
/// 5.7's battery, relocated verbatim from VFS now that MFS is the block device's
/// only client.
///
/// None of the ten is reachable from the live path, which is the whole reason they
/// exist: a check no marker exercises is a check that can regress silently.
///
///   - `bad-minor` — a good grant aimed at minor 7. `ENXIO`, from the driver's own
///     minor check: the *device* does not exist; nothing is wrong with the grant.
///   - `bad-block` — one block past the end. `EINVAL` rather than `EIO`: a block
///     device's size is known to its client, so this is a caller bug.
///   - `too-long` — one byte more than `BDEV_MAX_IO`. The deliberate departure
///     from CDEV: `EINVAL`, not a short read.
///   - `neg-len` — a negative length, which unchecked would widen into a ~16 EiB
///     `u64` byte count for the kernel copy.
///   - `not-mine` — the same bytes granted to **PM** instead of the driver. The
///     driver passes it to `SYS_SAFECOPY` in good faith and the *kernel* refuses
///     it, on `verify_grant`'s `who_to` check — the property that makes a grant id
///     safe to pass around at all.
///   - `read-only` — a `CPF_READ`-only grant used as a copy *destination*: the
///     access mask in the write direction.
///   - `wr-dir` — a `BDEV_WRITE` whose grant carries only `CPF_WRITE`. Every
///     geometry check in the driver passes; what refuses it is the *kernel's*
///     grant check on the copy direction, which is the guard that stops a client
///     reading a device buffer back out through a write-shaped request. Until
///     slice 5.10a this probe was `write`, expecting `EROFS` from the driver
///     itself — when the write became real that expectation would have turned
///     into a *successful store*, so the probe moved to something still denied
///     rather than quietly retiring. It also aims at block 1, the one block
///     `START_BLOCK = 2` leaves spare, so a regression in the guard is a wasted
///     write rather than a destroyed superblock.
///   - `wr-minor` — a write to a device that does not exist. `ENXIO`, and the
///     point is the *order*: the driver validates the minor before it touches the
///     grant, so a bad device is reported as one rather than as a grant failure.
///     It rides `write_only` and block 1 for the same reason `wr-dir` does — the
///     probe proves the same thing either way, and if the minor check ever
///     regressed the kernel would still refuse the copy instead of storing a
///     block.
///   - `unknown` — a request one past the band. `ENOSYS`, and **the reply itself
///     is the assertion**: a driver that dropped an unknown request would leave
///     this server blocked in its SENDREC forever.
///
/// Plus one probe that is not a BDEV request at all: `SYS_GETINFO(GET_RAMDISK)`
/// issued by MFS, which the kernel must refuse with `EPERM`. The ramdisk is mapped
/// into exactly one address space, so that gate is what stops the VA being handed
/// to a process where it faults — and MFS is the strongest caller available to
/// test it with, being the one server that would otherwise have a use for such a
/// VA.
///
/// Every probe grants over the block buffer's own address: it is a static, always
/// mapped, and needs no stack local on a one-page stack.
#[cfg_attr(test, allow(dead_code))]
fn bdev_denials(grants: &mut GrantPool<GRANT_SLOTS>, blocks: &Blocks) {
    let addr = Blocks::addr();
    let len = IMAGE_HDR_LEN as u64;
    // Three deliberately malformed grants. `write_only` is **not** `blocks.gid`:
    // that one carries `CPF_READ | CPF_WRITE` as of slice 5.10a, so a `BDEV_WRITE`
    // named on it would succeed and store the block buffer's contents onto the
    // device. This one is missing exactly the bit the copy direction needs.
    let (Ok(not_mine), Ok(read_only), Ok(write_only)) = (
        grants.grant_direct(boot_endpoint(PM_PROC_NR), addr, len, CPF_WRITE),
        grants.grant_direct(blocks.mem, addr, len, CPF_READ),
        grants.grant_direct(blocks.mem, addr, len, CPF_WRITE),
    ) else {
        return diag_fmt(format_args!("bdev.deny FAIL setup"));
    };

    let n = len as i32;
    let good = blocks.gid;
    let probes = [
        Probe {
            name: "bad-minor",
            m_type: BDEV_READ,
            minor: 7,
            gid: good,
            len: n,
            block: 0,
            want: ENXIO,
        },
        Probe {
            name: "bad-block",
            m_type: BDEV_READ,
            minor: BDEV_MINOR_RAMDISK,
            gid: good,
            len: n,
            block: u64::from(ROOTFS_IMAGE_BLOCKS),
            want: EINVAL,
        },
        Probe {
            name: "too-long",
            m_type: BDEV_READ,
            minor: BDEV_MINOR_RAMDISK,
            gid: good,
            len: BDEV_MAX_IO as i32 + 1,
            block: 0,
            want: EINVAL,
        },
        Probe {
            name: "neg-len",
            m_type: BDEV_READ,
            minor: BDEV_MINOR_RAMDISK,
            gid: good,
            len: -1,
            block: 0,
            want: EINVAL,
        },
        Probe {
            name: "not-mine",
            m_type: BDEV_READ,
            minor: BDEV_MINOR_RAMDISK,
            gid: not_mine,
            len: n,
            block: 0,
            want: EPERM,
        },
        Probe {
            name: "read-only",
            m_type: BDEV_READ,
            minor: BDEV_MINOR_RAMDISK,
            gid: read_only,
            len: n,
            block: 0,
            want: EPERM,
        },
        Probe {
            name: "wr-dir",
            m_type: BDEV_WRITE,
            minor: BDEV_MINOR_RAMDISK,
            gid: write_only,
            len: n,
            block: 1,
            want: EPERM,
        },
        Probe {
            name: "wr-minor",
            m_type: BDEV_WRITE,
            minor: 7,
            gid: write_only,
            len: n,
            block: 1,
            want: ENXIO,
        },
        Probe {
            name: "unknown",
            m_type: BDEV_RQ_BASE + NR_BDEV_MSGS as i32,
            minor: BDEV_MINOR_RAMDISK,
            gid: good,
            len: n,
            block: 0,
            want: ENOSYS,
        },
    ];

    let mut denied = 0usize;
    for p in &probes {
        let rc = bdev_request(blocks.mem, p.m_type, p.minor, p.gid, p.len, p.block);
        if rc == p.want {
            denied += 1;
        } else {
            diag_fmt(format_args!("bdev.deny FAIL {} rc={rc}", p.name));
        }
    }

    // The kernel's own gate, which no BDEV message can reach.
    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    let rc = sys_getinfo(GET_RAMDISK, &mut m);
    if rc == EPERM {
        denied += 1;
    } else {
        diag_fmt(format_args!("bdev.deny FAIL getinfo rc={rc}"));
    }

    if denied == probes.len() + 1 {
        diag_fmt(format_args!("bdev.deny ok n={denied}"));
    }
    for gid in [not_mine, read_only, write_only] {
        let _ = grants.revoke(gid);
    }
}

// ---------------------------------------------------------------------------
// Wire helpers.
// ---------------------------------------------------------------------------

/// Issue one block-device request and return the reply `m_type` — the byte count
/// read, or a negative errno.
///
/// `m_type` is a parameter rather than hardcoded to `BDEV_READ` so the
/// `BDEV_WRITE` and unknown-request probes ride the same marshaling as a real
/// read; a probe built by a second, hand-written marshaller would prove less.
///
/// No granter goes in the payload — the driver takes it from the kernel-stamped
/// `m_source`, so this message cannot aim the driver's privileged `SYS_SAFECOPY`
/// anywhere but this server's own address space. There is no grant-offset field
/// either: the block buffer's block starts at its beginning.
#[cfg_attr(test, allow(dead_code))]
fn bdev_request(mem: Endpoint, m_type: i32, minor: i32, gid: i32, len: i32, block: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, BDEV_MINOR_OFF, minor);
    wr_i32(&mut m, BDEV_GRANT_OFF, gid);
    wr_i32(&mut m, BDEV_LEN_OFF, len);
    wr_u64(&mut m, BDEV_BLOCK_OFF, block);
    let trap_rc = ipc_sendrec(mem, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// Reply to a SENDREC caller: stamp `m_type`, zero `m_source` (the kernel
/// overwrites it on delivery), and SEND the message back. A copy of TTY's, for the
/// reason TTY's exists.
#[cfg_attr(test, allow(dead_code))]
fn reply(target_e: Endpoint, msg: &mut Message, m_type: i32) {
    msg.m_type = m_type;
    msg.m_source = 0;
    let _ = ipc_send(target_e, msg);
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}

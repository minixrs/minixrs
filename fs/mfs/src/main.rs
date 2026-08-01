// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs MFS — the MinixFS v3 file-system server, read-only (slice 5.8).
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
    BDEV_READ, BDEV_RQ_BASE, BDEV_WRITE, FS_LOOKUP, FS_READ, FS_READSUPER, GET_RAMDISK,
    NR_BDEV_MSGS, SAFECOPY_TO, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{MEM_PROC_NR, PM_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{
    EINVAL, EIO, EISDIR, ENODEV, ENOENT, ENOSYS, ENOTDIR, ENXIO, EPERM, EROFS, OK,
};
use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE, GRANT_INVALID};
use minixrs_kernel_shared::rootfs::{
    IMAGE_HDR_LEN, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL, ROOTFS_IMAGE_BLOCKS, ROOTFS_MOTD,
    ROOTFS_MOTD_PATH, ROOTFS_PATTERN_PATH, ROOTFS_TAIL_BLOCK, rootfs_pattern_byte,
};
use minixrs_mfs::MFS_BLOCK_SIZE;
use minixrs_mfs::inode::{Inode, NR_DIRECT_ZONES, ROOT_INODE};
use minixrs_mfs::layout::{Layout, layout};
use minixrs_mfs::read::{
    ZoneLookup, inode_at, inode_location, zone_for_offset, zone_from_indirect,
};
use minixrs_mfs::superblock::{SUPER_OFFSET, SUPER_ON_DISK_LEN, Superblock};
use minixrs_mfs::{proto, walk};
use minixrs_server_rt::{
    GrantPool, SefConfig, diag_fmt, sef_publish_to_ds, sef_retrieve_from_ds, sef_startup,
    sys_getinfo, sys_safecopy, wr_i32, wr_u64,
};

/// Simultaneously outstanding grants. Three are used — the block buffer's, and
/// the two malformed ones the denial battery needs — and the pool costs `N * 32`
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
    /// Grant naming [`BLOCK`] with the driver as grantee, `CPF_WRITE`. Issued
    /// once at boot: the buffer is a static, so its address never changes and
    /// `GrantPool::ensure_registered` never re-fires.
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
/// loop — a corrupt inode claiming `size = i32::MAX` would otherwise spin.
#[cfg_attr(test, allow(dead_code))]
fn find_component(blocks: &mut Blocks, mount: &Mount, dir: &Inode, name: &str) -> Result<u32, i32> {
    let size = walk::dir_size(dir.size)?;
    let mut off = 0usize;
    while off < size {
        let want = (size - off).min(mount.block_size);
        if let Some(ino) = scan_block(blocks, mount, dir, off as u64, want, name)? {
            return Ok(ino);
        }
        off += want;
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
    // `CPF_WRITE`, because the driver copies *into* this buffer.
    let gid = match grants.grant_direct(mem, Blocks::addr(), MFS_BLOCK_SIZE as u64, CPF_WRITE) {
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
#[cfg_attr(test, allow(dead_code))]
fn pattern_matches(got: &[u8], pos: u64) -> bool {
    got.iter()
        .enumerate()
        .all(|(i, &b)| b == rootfs_pattern_byte(pos as usize + i))
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
///   - `write` — `BDEV_WRITE`, which must answer `EROFS`. Fold that arm into the
///     driver's `_` case and this becomes `ENOSYS`, which is the whole reason the
///     request has a number of its own.
///   - `write-bad-minor` — a write to a device that does not exist. `ENXIO`, not
///     `EROFS`: the driver validates before it refuses, so it never asserts
///     read-onlyness about a device it does not have.
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
    let (Ok(not_mine), Ok(read_only)) = (
        grants.grant_direct(boot_endpoint(PM_PROC_NR), addr, len, CPF_WRITE),
        grants.grant_direct(blocks.mem, addr, len, CPF_READ),
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
            name: "write",
            m_type: BDEV_WRITE,
            minor: BDEV_MINOR_RAMDISK,
            gid: good,
            len: n,
            block: 0,
            want: EROFS,
        },
        Probe {
            name: "write-bad-minor",
            m_type: BDEV_WRITE,
            minor: 7,
            gid: good,
            len: n,
            block: 0,
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
    for gid in [not_mine, read_only] {
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

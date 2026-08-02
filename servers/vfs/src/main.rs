// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs VFS (virtual file system) server — the POSIX write path (slice 5.4)
//! and read path (5.8), plus the grant and console demos slices 5.2 and 5.3 left
//! behind as regression probes.
//!
//! Slice 4.2 stood VFS up as a real boot server: it boots through the SEF
//! framework and publishes its endpoint to DS. Slice 5.2 gave it the granting half
//! of the first cross-address-space copy ([`grant_test`]), and 5.3 made it the
//! first client of the TTY console driver ([`tty_demo`]).
//!
//! Slice 5.4 makes it a *file system* server, in the one sense that matters at
//! this stage: an ordinary user process can now write to a descriptor. See
//! [`do_write`]. Slice 5.8 gives it something to read — a real filesystem behind
//! [`do_open`] / [`do_read`] / [`do_close`], served by MFS.
//!
//! ## The write path, and its one copy
//!
//! ```text
//! user ──VFS_WRITE{fd,buf,len}──► VFS ──CDEV_WRITE{minor,gid,len,off}──► TTY
//!                                  │                                      │
//!                                  └── magic grant: caller's buf ──────────┘
//!                                          (kernel copies, once)
//! ```
//!
//! VFS resolves the descriptor, issues a **magic** (third-party) grant naming the
//! *caller's* buffer with the driver as grantee, and forwards the id. TTY then
//! safecopies straight out of the caller's address space into its staging buffer.
//! The bytes never pass through VFS — there is exactly one copy, from the process
//! that wrote them to the driver that transmits them, which is the whole point of
//! the D4 grant design.
//!
//! Three properties hold that path together, each of which has its own probe in
//! the boot log:
//!
//! **The grant's owner is the kernel-stamped `m_source`.** VFS holds `SYS_PROC`,
//! which is what makes a magic grant legal for it at all; a caller-supplied owner
//! field would therefore let *any* VFS client aim a privileged cross-address-space
//! copy at a third party's memory. `VFS_WRITE` has no such field. This is slice
//! 5.2/5.3's confused-deputy rule applied to the granting side.
//!
//! **VFS absorbs short writes.** `CDEV_MAX_IO` is a driver staging detail; a
//! `write()` return value is not allowed to expose it. So [`write_all`] re-sends
//! with `offset` advanced until the buffer is out and reports the total.
//!
//! **Every request gets a reply.** VFS's clients are all inside a SENDREC, so a
//! dropped message blocks the caller forever — TTY's rule, and now VFS's.
//!
//! ## The read path, and its two copies
//!
//! ```text
//! user ──VFS_OPEN{path,len}───► VFS ──SYS_COPY──────────► (the path, into VFS)
//!                                │
//!                                └──FS_LOOKUP{path}─────► MFS  → (ino, mode)
//!
//! user ──VFS_READ{fd,buf,len}──► VFS ──FS_READ{ino,gid,len,pos}──► MFS
//!                                 │                                 │
//!                                 └── magic grant: caller's buf ────┘
//! ```
//!
//! **Two copies, not one**, and the difference from the write path above is
//! deliberate: MFS stages a block through its own buffer (device → MFS) before
//! safecopying the requested slice out of it (MFS → caller). A MinixFS read is
//! rarely block-aligned in both the file and the destination, and a *hole* has no
//! device block to copy from at all — so the staging cannot be elided, and this is
//! MINIX 3's own shape. Only the second copy is VFS's grant; the bytes still never
//! pass through VFS.
//!
//! **`SYS_COPY` reads the path, and its source is the kernel-stamped `m_source`.**
//! `SYS_COPY` has no per-target authorization whatsoever, so a payload-supplied
//! source would let any client read any process's memory through VFS. This is the
//! same confused-deputy rule as the granting side, in its sharpest form.
//!
//! **VFS does not loop on read.** It loops on `write` because a driver's staging
//! limit may not reach `write()`'s return value; `read()` is explicitly allowed to
//! return less than asked for, and a file read is short at EOF regardless.
//!
//! ## What slice 5.8 took away
//!
//! Slice 5.7's `bdev_demo` / `bdev_denials` are **gone**. They existed because
//! there was no other client for the block device and no successful `SAFECOPY_TO`
//! anywhere in the tree; MFS is now the real client, so the whole battery moved
//! there verbatim (`bdev.ds` / `bdev.tail` / `bdev.deny` are `[diag mfs]` lines
//! now). With them go VFS's `rootfs::*` / `BDEV_*` / `GET_RAMDISK` imports: VFS is
//! back to knowing nothing about block devices, which is the layering the FS band
//! exists to create.
//!
//! Built as a freestanding aarch64 ELF (see `servers/vfs/user.ld`), packed into
//! the kernel's boot-image archive by `kernel/build.rs`, and loaded into its own
//! per-process AddrSpace at boot by `arch::aarch64::userland::load_boot_server`.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` (no host tests yet — the SEF/IPC glue is QEMU-verified). The
// `_start` shim and panic handler are gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod fd;
mod open;
mod rw;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_MINOR_RAMDISK, CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MAX_IO, CDEV_MINOR_CONSOLE,
    CDEV_MINOR_OFF, CDEV_OFFSET_OFF, CDEV_WRITE, FS_GRANT_OFF, FS_INO_OFF, FS_LEN_OFF, FS_LOOKUP,
    FS_MODE_OFF, FS_PATH_MAX, FS_PATH_OFF, FS_POS_OFF, FS_READ, FS_READSUPER, FS_RQ_BASE,
    FS_SUPER_BLOCK_SIZE_OFF, FS_SUPER_BLOCKS_OFF, FS_SUPER_MINOR_OFF, FS_SUPER_ROOT_OFF,
    NR_FS_MSGS, PM_GRANT_TEST, SYS_GETINFO_NAME_LEN, VFS_CLOSE, VFS_OPEN, VFS_READ, VFS_WRITE,
};
use minixrs_kernel_shared::com::{MFS_PROC_NR, PM_PROC_NR, TTY_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::{Endpoint, SELF, endpoint_proc};
use minixrs_kernel_shared::error::{
    EBADF, EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOSYS, ENOTDIR, ENXIO, EPERM, EROFS, OK,
};
use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE};
use minixrs_kernel_shared::rootfs::ROOTFS_MOTD_PATH;
use minixrs_server_rt::{
    GrantPool, SefConfig, buf_addr, diag_fmt, rd_i32, sef_publish_to_ds, sef_retrieve_from_ds,
    sef_startup, sys_copy, wr_i32, wr_u64,
};

use fd::Fd;

/// Bytes VFS grants PM in the slice-5.2 demo.
const GRANT_TEST_LEN: usize = 64;

/// Number of simultaneously outstanding grants VFS can hold. VFS's stack is one
/// page, and the pool costs `N * 32` bytes; 8 is ample for the demo.
const GRANT_SLOTS: usize = 8;

/// The granted buffer. A `static` (not `static mut`) so it lands in `.rodata`,
/// which the ELF loader maps read-only — exactly right for a `CPF_READ` grant,
/// and it means the kernel's `Prot::writable` check would reject any attempt to
/// safecopy *into* it.
static GRANT_TEST_BUF: [u8; GRANT_TEST_LEN] = grant_test_pattern();

/// A deterministic, non-trivial byte pattern. Deterministic so PM's checksum is
/// a fixed value across runs; non-trivial (not `i`, not a constant) so a copy
/// that lost or duplicated a chunk still changes the sum.
const fn grant_test_pattern() -> [u8; GRANT_TEST_LEN] {
    let mut b = [0u8; GRANT_TEST_LEN];
    let mut i = 0;
    while i < GRANT_TEST_LEN {
        b[i] = (i as u8).wrapping_mul(37).wrapping_add(11);
        i += 1;
    }
    b
}

// ----- Slice 5.3: the CDEV console demo ------------------------------------

/// The line VFS asks TTY to print. In `.rodata` (a `static`, not `static mut`),
/// which the ELF loader maps read-only — exactly right for a `CPF_READ` grant.
static TTY_BANNER: [u8; 35] = *b"minix.rs vfs: hello via CDEV_WRITE\n";

/// Length of the short-write probe: eight bytes past what one `CDEV_WRITE` can
/// carry, so the reply must come back clamped.
const TTY_RULE_LEN: usize = CDEV_MAX_IO + 8;

/// A buffer deliberately longer than [`CDEV_MAX_IO`], whose byte at index
/// `CDEV_MAX_IO - 1` is `\n` — so the *clamped* write renders as exactly one
/// console line. The eight bytes past the clamp spell `TRUNCATE`, which therefore
/// must never appear on the console: if they do, the driver ignored its own limit.
static TTY_RULE: [u8; TTY_RULE_LEN] = tty_rule_pattern();

const fn tty_rule_pattern() -> [u8; TTY_RULE_LEN] {
    let mut b = [b'-'; TTY_RULE_LEN];
    let prefix = b"minix.rs vfs: short-write rule ";
    let mut i = 0;
    while i < prefix.len() {
        b[i] = prefix[i];
        i += 1;
    }
    // Ends the line at the clamp boundary.
    b[CDEV_MAX_IO - 1] = b'\n';
    // Past the clamp: must never reach the console.
    let tail = b"TRUNCATE";
    let mut j = 0;
    while j < tail.len() {
        b[CDEV_MAX_IO + j] = tail[j];
        j += 1;
    }
    b
}

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` can
/// dive straight into Rust without setting up a stack itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `sef_startup` learns VFS's endpoint/name and runs `vfs_init`, which
    // publishes VFS's endpoint to DS. The publish SENDREC blocks until DS is in
    // its receive loop — safe at boot (DS's init does no IPC). No signal
    // handling. On startup failure there is no recovery and nothing to print.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(vfs_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        loop {
            core::hint::spin_loop()
        }
    });

    // The grant pool is a `main`-frame value that outlives the receive loop
    // below — it cannot live in `init_fresh`, whose frame is gone by the time a
    // grantee safecopies. (`server-rt` keeps the table as a value rather than a
    // static precisely so it can be owned like this.)
    let mut grants: GrantPool<GRANT_SLOTS> = GrantPool::new();

    // Resolve the two peers once, before serving anyone: every `VFS_WRITE`
    // targets TTY and every `VFS_OPEN`/`VFS_READ` targets MFS, and a DS lookup
    // per request would be a round trip per request for endpoints that cannot
    // change.
    let tty = tty_endpoint();
    let mfs = mfs_endpoint();

    // The mount, and the one piece of VFS state that outlives a request. `None`
    // until something succeeds; a failure is deliberately **not** cached, so the
    // next `open` retries — see [`ensure_mounted`].
    let mut mount: Option<Mount> = None;

    grant_test(&mut grants);
    tty_demo(&mut grants, tty);
    mount_root(&mut mount, mfs);
    // The FS denial battery runs **last**, and the order is load-bearing: those
    // requests are deliberately malformed, so a peer that wedged on one would
    // take everything after it down. Behind the mount proof, such a wedge
    // localizes to `fs.deny` instead of blacking out 5.2's, 5.3's, and 5.4's
    // markers as well. Do not tidy this prologue into alphabetical order.
    fs_denials(&mut grants, mfs, mount);

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        // Capture the caller *first*: it is the reply target, the owner named by
        // a transfer's magic grant, the source of an `open`'s path copy, and the
        // process whose fd table is consulted — and the dispatch below overwrites
        // `msg.m_source` on the way out.
        let caller_e = msg.m_source;
        let rc = match msg.m_type {
            VFS_WRITE => do_write(caller_e, &msg, &mut grants, tty),
            VFS_OPEN => do_open(caller_e, &msg, &mut mount, mfs),
            VFS_READ => do_read(caller_e, &msg, &mut grants, mfs),
            VFS_CLOSE => do_close(caller_e, &msg),
            // Reply rather than drop (TTY's rule): VFS's clients are all inside a
            // SENDREC, and a dropped request blocks the caller forever. DS can
            // afford to drop one only because nothing SENDRECs it in anger.
            _ => ENOSYS,
        };
        reply(caller_e, &mut msg, rc);
    }
}

/// Serve one `VFS_WRITE`. Returns the reply `m_type`: bytes written (`>= 0`), or
/// a negative errno.
///
/// The checks, in order — each is the first thing that can be wrong given the
/// ones before it:
///
/// 1. The descriptor resolves, through the caller's own row of the fd table. The
///    caller is named by the kernel-stamped `m_source`, so a client cannot ask
///    about another process's descriptors.
/// 2. `len < 0` → `EINVAL`. Left unchecked it would widen into a ~16 EiB `u64`
///    byte count for the grant.
/// 3. `len == 0` → `0`, a legal empty write. Deliberately *before* the grant, so
///    a client polling with `len = 0` gets no grant issued at all and cannot use
///    it to probe the granting path. TTY applies the same rule one layer down.
/// 4. The buffer lies in the user address range. This is `user_range_ok`, the
///    no-alignment variant grants use — a `write()` buffer is a byte buffer.
///    Defence in depth rather than the load-bearing gate: the kernel's copy
///    engine walks the caller's page tables and answers `EFAULT` for anything
///    unmapped regardless (D5), which is what makes a bad pointer an errno here
///    instead of a fault in the caller.
///
/// Then the grant. `caller_e` is the **kernel-stamped `m_source`** — the one fact
/// in this whole request a client cannot forge. VFS holds `SYS_PROC`, which is
/// what makes a magic grant legal for it; taking the owner from the payload
/// instead would let any VFS client aim TTY's privileged `SYS_SAFECOPY` at a
/// third party's address space through VFS. There is no payload field for it, and
/// there must never be one.
#[cfg_attr(test, allow(dead_code))]
fn do_write(
    caller_e: Endpoint,
    msg: &Message,
    grants: &mut GrantPool<GRANT_SLOTS>,
    tty: Endpoint,
) -> i32 {
    let req = rw::parse(msg);

    let minor = match fd::resolve(endpoint_proc(caller_e).get(), req.fd) {
        Ok(Fd::CharDev { minor }) => minor,
        // **Defined and refused**, rather than folded into the `Unused` case
        // below: `EROFS` says "this descriptor names a file and the filesystem is
        // read-only", which is a different thing to tell a caller than `EBADF`.
        // That distinction is the whole reason this arm exists, and slice 5.10
        // replaces this one line with the write path — the `BDEV_WRITE` precedent
        // exactly.
        Ok(Fd::File { .. }) => return EROFS,
        // `resolve` maps a closed descriptor to `EBADF` itself, so this arm is
        // unreachable. It exists so a future `Fd` variant shows up as a compile
        // error to be routed, not a silent fallthrough — which is precisely how
        // the arm above got written.
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

    // The single-copy hop: the grant names the *caller's* memory, so the kernel
    // moves the bytes from the caller straight into the driver.
    let gid = match grants.grant_magic(tty, caller_e, req.buf, len as u64, CPF_READ) {
        Ok(gid) => gid,
        // Verbatim: `ENOMEM` (VFS is out of grant slots) and a `SYS_SETGRANT`
        // failure are different problems, and neither is the client's fault.
        Err(e) => return e,
    };
    let written = write_all(tty, minor, gid, len);
    let _ = grants.revoke(gid);
    written
}

/// Drive `CDEV_WRITE` until `len` bytes are out, and report the total.
///
/// This is the IPC half only: every decision about when to stop and what to
/// report lives in [`rw::advance`], which is where its rules are documented and
/// unit-tested. Two of those rules — a driver reporting `0`, and a driver
/// reporting more than it was asked for — are unreachable through a working TTY,
/// so keeping them out of this loop is what makes them testable at all.
///
/// `len > 0` on entry (`do_write` returns early on an empty write), so at least
/// one request always goes out and `len - off` is never zero.
#[cfg_attr(test, allow(dead_code))]
fn write_all(tty: Endpoint, minor: i32, gid: i32, len: usize) -> i32 {
    let mut off = 0usize;
    loop {
        let n = cdev_write(tty, minor, gid, (len - off) as i32, off as u64);
        match rw::advance(off, len, n) {
            rw::Step::More(next) => off = next,
            rw::Step::Done(rc) => return rc,
        }
    }
}

// ---------------------------------------------------------------------------
// Slice 5.8: the read path — open, read, close, against MFS.
// ---------------------------------------------------------------------------

/// What `FS_READSUPER` said about the mounted filesystem.
///
/// Deliberately **not** a per-open cache of anything: it holds geometry, not
/// state, and no file's size lives here or anywhere else in VFS. EOF is a read
/// returning `0`, one source of truth — which is also exactly what slice 5.10's
/// writes would have invalidated in a cached size.
#[derive(Copy, Clone)]
struct Mount {
    root: i32,
    block_size: i32,
    blocks: i32,
}

/// Resolve MFS's endpoint through DS, falling back to its boot endpoint.
///
/// A copy of [`tty_endpoint`], carrying the same contract: the lookup is the
/// point, but DS publish-before-retrieve is **not** guaranteed by construction —
/// it works because `kernel/build.rs` packs `mfs` before `vfs`. A failed lookup
/// falls back and emits a *distinguishable* line, so the rest of the boot still
/// proves the FS path while the required `fs.ds ok` marker disappears and CI goes
/// red on the ordering regression specifically.
#[cfg_attr(test, allow(dead_code))]
fn mfs_endpoint() -> Endpoint {
    let mut key = [0u8; SYS_GETINFO_NAME_LEN];
    key[0..3].copy_from_slice(b"mfs");
    match sef_retrieve_from_ds(&key) {
        Ok(ep) => {
            diag_fmt(format_args!("fs.ds ok ep={ep}"));
            ep
        }
        Err(rc) => {
            let ep = boot_endpoint(MFS_PROC_NR);
            diag_fmt(format_args!("fs.ds FAIL rc={rc} fallback={ep}"));
            ep
        }
    }
}

/// Mount the root filesystem if it is not mounted already.
///
/// **A failure is not cached.** `mount` stays `None`, so the next `open` tries
/// again — which is what makes the boot-time attempt below a *report* rather than
/// a commitment, and what lets VFS survive coming up before MFS has finished its
/// own device bring-up. MFS re-reads the superblock on every `FS_READSUPER`, so a
/// retry is a real retry rather than a replay of a cached answer.
#[cfg_attr(test, allow(dead_code))]
fn ensure_mounted(mount: &mut Option<Mount>, mfs: Endpoint) -> Result<Mount, i32> {
    if let Some(m) = *mount {
        return Ok(m);
    }
    let m = fs_readsuper(mfs, BDEV_MINOR_RAMDISK)?;
    *mount = Some(m);
    Ok(m)
}

/// Mount at boot and announce the geometry.
///
/// The numbers are MFS's, arriving over a real `FS_READSUPER` round trip rather
/// than being read out of MFS's own boot-time decode — so this marker and MFS's
/// `mount ok` are independent statements, and a broken FS band shows up here while
/// MFS's own line stays green.
#[cfg_attr(test, allow(dead_code))]
fn mount_root(mount: &mut Option<Mount>, mfs: Endpoint) {
    match ensure_mounted(mount, mfs) {
        Ok(m) => diag_fmt(format_args!(
            "fs.mount ok root={} bs={} blocks={}",
            m.root, m.block_size, m.blocks
        )),
        Err(rc) => diag_fmt(format_args!("fs.mount FAIL rc={rc}")),
    }
}

/// Serve one `VFS_OPEN`. Returns the reply `m_type`: the new descriptor (`>= 0`),
/// or a negative errno.
///
/// The checks, in order — each is the first thing that can be wrong given the
/// ones before it:
///
/// 1. The path's length and buffer are sane ([`open::validate`]). Length first,
///    so a malformed request never reaches the copy.
/// 2. Something is mounted (retried here, not cached — see [`ensure_mounted`]).
/// 3. The path bytes copy in. **This is the first live consumer of decision D4's
///    "`SYS_COPY` for small control-plane reads" sentence**, and the
///    confused-deputy rule in its sharpest form: `SYS_COPY` has *no per-target
///    authorization at all* — the caller's `k_call_mask` bit is the whole check —
///    so a payload-supplied source process would let any client read any process's
///    memory through VFS. The source is `caller_e`, the kernel-stamped `m_source`,
///    and there is no payload field for it.
/// 4. MFS resolves the path, and [`open::classify`] turns the mode into either a
///    descriptor entry or an errno.
/// 5. The entry lands at the caller's lowest free descriptor.
///
/// The path buffer is a **local**, not a static: it is 64 bytes, it is written
/// into by the kernel's copy, and it must not be shared between requests.
#[cfg_attr(test, allow(dead_code))]
fn do_open(caller_e: Endpoint, msg: &Message, mount: &mut Option<Mount>, mfs: Endpoint) -> i32 {
    let req = open::parse(msg);
    let len = match open::validate(req.len, req.path) {
        Ok(len) => len,
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

    let (ino, mode) = match fs_lookup(mfs, &path[..len]) {
        Ok(found) => found,
        Err(e) => return e,
    };
    match open::classify(mode) {
        // `classify` decides the *kind* of descriptor; the inode is filled in
        // here, because this is the layer that knows it.
        Ok(Fd::File { .. }) => {
            fd::alloc(endpoint_proc(caller_e).get(), Fd::File { ino, pos: 0 }).unwrap_or_else(|e| e)
        }
        // No other variant is reachable — `classify` returns only `File` or an
        // error — but routing it explicitly means a future device-node arm is a
        // compile error to handle rather than a silent `EINVAL`.
        Ok(_) => EINVAL,
        Err(e) => e,
    }
}

/// Serve one `VFS_READ`. Returns the reply `m_type`: bytes read (`>= 0`, `0` is
/// EOF), or a negative errno.
///
/// **VFS does not loop here, and that asymmetry with [`do_write`] is the design.**
/// `write()` may not expose a driver's staging limit, so `write_all` re-sends
/// until the buffer is out; `read()` is explicitly allowed to return less than
/// asked for, and a file read is short at EOF regardless. So one round trip, and
/// the count MFS reports is the count the client hears.
///
/// The grant is **fresh, and covers exactly the bytes this round moves** — which
/// is what lets `FS_READ` have no grant-offset field at all. It carries
/// `CPF_WRITE` because the bytes travel *into* the caller's buffer, and it is a
/// **magic** grant naming the caller as owner, so MFS safecopies straight into the
/// caller's address space and VFS never touches the data. Owner is the
/// kernel-stamped `m_source`; there is no payload field for it and there must
/// never be one.
#[cfg_attr(test, allow(dead_code))]
fn do_read(
    caller_e: Endpoint,
    msg: &Message,
    grants: &mut GrantPool<GRANT_SLOTS>,
    mfs: Endpoint,
) -> i32 {
    let req = rw::parse(msg);
    let proc_nr = endpoint_proc(caller_e).get();

    // The fd table's borrow ends here: `Fd` is `Copy`, so the destructuring `let`
    // leaves values behind and nothing is held across the SENDREC below. That is
    // the rule `fd.rs`'s module note states, and this is the call site it is
    // about.
    let (ino, pos) = match fd::resolve(proc_nr, req.fd) {
        Ok(Fd::File { ino, pos }) => (ino, pos),
        // There is no `CDEV_READ` until Phase 6 (RX needs `SYS_IRQCTL`), so a
        // console descriptor has nothing to read from. `ENOSYS` rather than
        // `EBADF`: the descriptor is perfectly good, the operation is what does
        // not exist yet.
        Ok(Fd::CharDev { .. }) => return ENOSYS,
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

    let gid = match grants.grant_magic(mfs, caller_e, req.buf, len as u64, CPF_WRITE) {
        Ok(gid) => gid,
        Err(e) => return e,
    };
    let n = fs_read(mfs, ino as i32, gid, len as i32, pos);
    let _ = grants.revoke(gid);

    if n > 0 {
        // Only on real progress: advancing on an error or on EOF would silently
        // move the descriptor past bytes nobody read.
        fd::advance(proc_nr, req.fd, n as u64);
    }
    n
}

/// Serve one `VFS_CLOSE`. Returns `OK` or `EBADF`.
///
/// `rw::parse` reads the descriptor from the same offset `read` and `write` use,
/// which is the whole payload; the other two fields are ignored rather than
/// validated, because a `close` that refused a nonzero `len` would be inventing a
/// rule the request does not have.
///
/// Nothing is sent to MFS: it keeps no per-open state, so there is no node to put
/// back — which is why the FS band has no `PUTNODE`.
#[cfg_attr(test, allow(dead_code))]
fn do_close(caller_e: Endpoint, msg: &Message) -> i32 {
    let req = rw::parse(msg);
    match fd::close(endpoint_proc(caller_e).get(), req.fd) {
        Ok(()) => OK,
        Err(e) => e,
    }
}

/// Issue one `FS_READSUPER` and decode the geometry it reports.
#[cfg_attr(test, allow(dead_code))]
fn fs_readsuper(mfs: Endpoint, minor: i32) -> Result<Mount, i32> {
    let mut m = Message {
        m_source: 0,
        m_type: FS_READSUPER,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, FS_SUPER_MINOR_OFF, minor);
    let trap_rc = ipc_sendrec(mfs, &mut m);
    if trap_rc != OK {
        return Err(trap_rc);
    }
    if m.m_type != OK {
        return Err(m.m_type);
    }
    Ok(Mount {
        root: rd_i32(&m, FS_SUPER_ROOT_OFF),
        block_size: rd_i32(&m, FS_SUPER_BLOCK_SIZE_OFF),
        blocks: rd_i32(&m, FS_SUPER_BLOCKS_OFF),
    })
}

/// Issue one `FS_LOOKUP` and return `(inode, mode)`.
///
/// The path travels **inline**, NUL-padded into the request's fixed field — not
/// through a grant. It is control plane rather than the data path grants were
/// provisioned for, it costs MFS no staging buffer, and it means this request has
/// no granter for anyone to spoof. The size in the reply is deliberately dropped:
/// VFS caches no file's size (see [`Mount`]).
///
/// A path that fills the field with no room for the NUL is refused here rather
/// than sent, so the wire never carries an unterminated one.
#[cfg_attr(test, allow(dead_code))]
fn fs_lookup(mfs: Endpoint, path: &[u8]) -> Result<(u32, i32), i32> {
    if path.is_empty() || path.len() >= FS_PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let mut m = Message {
        m_source: 0,
        m_type: FS_LOOKUP,
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
    Ok((ino, rd_i32(&m, FS_MODE_OFF)))
}

/// Issue one `FS_READ` and return the reply `m_type` — the byte count, or a
/// negative errno.
///
/// No granter goes in the payload: MFS takes it from the kernel-stamped
/// `m_source`, so this message cannot aim MFS's privileged `SYS_SAFECOPY` anywhere
/// but at the grant VFS itself issued. And no grant *offset*: the grant covers
/// exactly this round's bytes.
#[cfg_attr(test, allow(dead_code))]
fn fs_read(mfs: Endpoint, ino: i32, gid: i32, len: i32, pos: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: FS_READ,
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

/// Send a raw `FS_LOOKUP` whose path field is written verbatim, for the probes
/// that need a malformed one.
///
/// Deliberately bypasses [`fs_lookup`]'s own cap: a probe built by a second,
/// hand-written marshaller would prove less, but a probe that its own client
/// refuses before sending proves nothing at all. This is the narrowest possible
/// bypass — the same message, with the length check lifted.
#[cfg_attr(test, allow(dead_code))]
fn fs_lookup_raw(mfs: Endpoint, field: &[u8; FS_PATH_MAX]) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: FS_LOOKUP,
        payload: [0u8; 96],
    };
    m.payload[FS_PATH_OFF..FS_PATH_OFF + FS_PATH_MAX].copy_from_slice(field);
    let trap_rc = ipc_sendrec(mfs, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// Probe every FS-band refusal the live path does not exercise.
///
/// Slice 5.1's lesson again: a check no marker exercises is a check that can
/// regress silently. Each probe is well-formed in every respect but one, and none
/// of them is reachable from init's `open`/`read` — VFS refuses most of these
/// before the wire, which is exactly why they have to be sent from here.
///
///   - `no-such` — a path that does not exist. `ENOENT`, from MFS's directory
///     scan reaching the end of the root without a match.
///   - `not-dir` — `/etc/motd/x`, naming a *file* as an intermediate component.
///     `ENOTDIR`, from the `is_dir` gate in MFS's walk; delete it and MFS would
///     read a file's bytes as directory entries.
///   - `relative` — a path with no leading `/`. `EINVAL`: there is no working
///     directory on minix.rs, so this is malformed rather than missing.
///   - `too-long` — a path field with no NUL anywhere. `ENAMETOOLONG`, never a
///     silently truncated path resolving to some other file. Sent through
///     [`fs_lookup_raw`] because VFS's own cap would refuse it first.
///   - `bad-ino` — `FS_READ` on inode 0, which does not exist. `EINVAL`.
///   - `read-dir` — `FS_READ` on the root inode, a directory. `EISDIR`, and the
///     second line of defence behind `open`'s: a client that obtained a directory
///     inode another way still cannot read it.
///   - `bad-minor` — `FS_READSUPER` on minor 7. `ENXIO`: the *device* does not
///     exist; nothing is wrong with the request.
///   - `unknown` — a request one past the band. `ENOSYS`, and **the reply itself
///     is the assertion**: a server that dropped an unknown request would leave
///     VFS blocked in its SENDREC forever and take every later marker with it.
///   - `not-mine` — a valid read whose grant names **PM** instead of MFS. MFS
///     passes it to `SYS_SAFECOPY` in good faith and the *kernel* refuses it, on
///     `verify_grant`'s `who_to` check.
///   - `read-only` — a `CPF_READ`-only grant used as a copy *destination*: the
///     access mask in the direction a read needs.
///
/// Best-effort throughout: a failure here must not stop VFS from serving.
#[cfg_attr(test, allow(dead_code))]
fn fs_denials(grants: &mut GrantPool<GRANT_SLOTS>, mfs: Endpoint, mount: Option<Mount>) {
    let mut denied = 0usize;

    // The root's inode number comes from the mount [`mount_root`] just took, not
    // from a literal `1`: MFS reports it in `FS_READSUPER` and this battery is the
    // only place VFS would have had to know it independently. Nothing to probe
    // without a mount — every lookup below would answer `ENODEV` — so say which
    // setup step failed rather than reporting eight probe failures.
    let Some(mount) = mount else {
        return diag_fmt(format_args!("fs.deny FAIL setup mount"));
    };

    // Path-shaped refusals. Each is a real `FS_LOOKUP` through the real
    // marshaller, so a probe cannot pass by being built differently.
    for (name, path, want) in [
        ("no-such", "/no-such-file", ENOENT),
        ("not-dir", "/etc/motd/x", ENOTDIR),
        ("relative", "etc/motd", EINVAL),
    ] {
        match fs_lookup(mfs, path.as_bytes()) {
            Err(rc) if rc == want => denied += 1,
            other => diag_fmt(format_args!(
                "fs.deny FAIL {name} rc={}",
                match other {
                    Ok(_) => OK,
                    Err(rc) => rc,
                }
            )),
        }
    }

    // A path field with no terminator, sent past VFS's own cap.
    let rc = fs_lookup_raw(mfs, &[b'a'; FS_PATH_MAX]);
    if rc == ENAMETOOLONG {
        denied += 1;
    } else {
        diag_fmt(format_args!("fs.deny FAIL too-long rc={rc}"));
    }

    // A real inode to aim the grant probes at, and the root's number for the
    // directory probe. A lookup failure here is reported once rather than as four
    // separate probe failures.
    let Ok((motd_ino, _)) = fs_lookup(mfs, ROOTFS_MOTD_PATH.as_bytes()) else {
        return diag_fmt(format_args!("fs.deny FAIL setup lookup"));
    };

    let mut buf = [0u8; 32];
    let addr = buf_addr(&mut buf);
    let len = buf.len() as u64;
    let (Ok(good), Ok(not_mine), Ok(read_only)) = (
        grants.grant_direct(mfs, addr, len, CPF_WRITE),
        grants.grant_direct(boot_endpoint(PM_PROC_NR), addr, len, CPF_WRITE),
        grants.grant_direct(mfs, addr, len, CPF_READ),
    ) else {
        return diag_fmt(format_args!("fs.deny FAIL setup grant"));
    };

    let n = len as i32;
    for (name, ino, gid, want) in [
        ("bad-ino", 0, good, EINVAL),
        ("read-dir", mount.root, good, EISDIR),
        ("not-mine", motd_ino as i32, not_mine, EPERM),
        ("read-only", motd_ino as i32, read_only, EPERM),
    ] {
        let rc = fs_read(mfs, ino, gid, n, 0);
        if rc == want {
            denied += 1;
        } else {
            diag_fmt(format_args!("fs.deny FAIL {name} rc={rc}"));
        }
    }

    // A device that does not exist, and a request that does not exist.
    match fs_readsuper(mfs, 7) {
        Err(rc) if rc == ENXIO => denied += 1,
        _ => diag_fmt(format_args!("fs.deny FAIL bad-minor")),
    }
    let mut m = Message {
        m_source: 0,
        m_type: FS_RQ_BASE + NR_FS_MSGS as i32,
        payload: [0u8; 96],
    };
    let rc = if ipc_sendrec(mfs, &mut m) == OK {
        m.m_type
    } else {
        OK
    };
    if rc == ENOSYS {
        denied += 1;
    } else {
        diag_fmt(format_args!("fs.deny FAIL unknown rc={rc}"));
    }

    if denied == FS_DENIAL_PROBES {
        diag_fmt(format_args!("fs.deny ok n={denied}"));
    }
    for gid in [good, not_mine, read_only] {
        let _ = grants.revoke(gid);
    }
}

/// Probes [`fs_denials`] runs. Named rather than counted inline so adding one
/// without counting it is a marker that vanishes rather than one that silently
/// under-reports.
const FS_DENIAL_PROBES: usize = 10;

/// Reply to a SENDREC caller: stamp `m_type`, zero `m_source` (the kernel
/// overwrites it on delivery), and SEND the message back. A copy of TTY's, for
/// the reason TTY's exists.
#[cfg_attr(test, allow(dead_code))]
fn reply(target_e: Endpoint, msg: &mut Message, m_type: i32) {
    msg.m_type = m_type;
    msg.m_source = 0;
    let _ = ipc_send(target_e, msg);
}

/// Slice 5.2 demo: direct-grant [`GRANT_TEST_BUF`] to PM and tell PM about it.
///
/// The grant id travels **in-band**, in the `PM_GRANT_TEST` payload, which is
/// how grant ids really travel — `CDEV_WRITE {minor, grant_id, len, offset}` has
/// the same shape, and takes its granter from `m_source` for the same reason. DS
/// could not carry one: `DS_PUBLISH` registers the kernel-stamped `m_source` and
/// ignores the payload, which is exactly its anti-spoof property.
///
/// `ipc_send` blocks until PM's receive loop picks the message up, so the demo
/// is self-synchronizing — no assumption is made about the two servers' SEF init
/// ordering. There is no cycle: VFS's own init only SENDRECs DS, and PM never
/// sends to VFS.
///
/// The buffer's raw address rides along too, so PM can read the same bytes a
/// second time with the ungranted `SYS_COPY` and compare.
///
/// A **second** grant over the same read-only buffer goes out claiming
/// `CPF_WRITE` — a granter lying about what its own memory permits. Nothing
/// stops a granter writing that entry, and the kernel copies through its own
/// HHDM alias where EL0 permission bits do not apply, so the only thing between
/// that lie and a corrupted `.rodata` page is the copy engine's explicit
/// `Prot::writable` check. PM probes it (`grant_denials`); this is the grant-path
/// analogue of slice 5.1's fourth bad-pointer probe.
///
/// Best-effort throughout: a failure here must not stop VFS from serving.
#[cfg_attr(test, allow(dead_code))]
fn grant_test(grants: &mut GrantPool<GRANT_SLOTS>) {
    let pm = boot_endpoint(PM_PROC_NR);
    let addr = (&raw const GRANT_TEST_BUF) as usize as u64;
    let len = GRANT_TEST_LEN as u64;
    let (Ok(gid), Ok(rw_gid)) = (
        grants.grant_direct(pm, addr, len, CPF_READ),
        grants.grant_direct(pm, addr, len, CPF_READ | CPF_WRITE),
    ) else {
        return;
    };

    // No granter field: PM reads that off the kernel-stamped `m_source`, so
    // nothing in this payload can redirect PM's privileged copies at a third
    // party.
    let mut msg = Message {
        m_source: 0,
        m_type: PM_GRANT_TEST,
        payload: [0u8; 96],
    };
    msg.payload[0..4].copy_from_slice(&gid.to_ne_bytes());
    msg.payload[4..8].copy_from_slice(&(GRANT_TEST_LEN as i32).to_ne_bytes());
    msg.payload[8..12].copy_from_slice(&rw_gid.to_ne_bytes());
    msg.payload[16..24].copy_from_slice(&addr.to_ne_bytes());
    let _ = ipc_send(pm, &mut msg);
}

/// Slice 5.3 demo: drive the TTY console driver over `CDEV_WRITE` directly.
///
/// Kept as a regression battery now that [`do_write`] is the real path, because
/// it proves three contracts the real path does not reach: the direct-grant form
/// (`do_write` issues magic grants), the *visible* short write (`do_write`
/// deliberately hides it behind [`write_all`]), and the two `CDEV_WRITE` refusals
/// a well-formed `write()` never provokes.
///
/// Four things get proven, in order:
///
/// 1. **DS lookup.** `tty` was resolved through DS by [`tty_endpoint`] rather than
///    hard-coded to `boot_endpoint(TTY_PROC_NR)`, which is what a real client
///    does. On failure that helper falls back to the boot endpoint and says so.
/// 2. **A real write.** Grant the banner read-only to TTY, send `CDEV_WRITE`, and
///    check the reply equals the granted length. `match=1` rather than the length
///    itself: self-checking, and it does not have to be re-derived if the banner
///    text changes.
/// 3. **A short write.** Ask for `CDEV_MAX_IO + 8` bytes and require exactly
///    `CDEV_MAX_IO` back. This is the POSIX contract that lets a driver stage
///    through a small buffer, and it is the difference between "write returns a
///    count" and "write returns success".
/// 4. **Two denials.** Each request is valid in every respect but one, so removing
///    the corresponding check makes it *succeed* and turns the marker into a
///    `FAIL`. See [`cdev_denials`].
///
/// Best-effort throughout: a failure here must not stop VFS from serving.
#[cfg_attr(test, allow(dead_code))]
fn tty_demo(grants: &mut GrantPool<GRANT_SLOTS>, tty: Endpoint) {
    // 2. The banner, through a read-only direct grant.
    let banner_addr = (&raw const TTY_BANNER) as usize as u64;
    let banner_len = TTY_BANNER.len() as u64;
    let Ok(gid) = grants.grant_direct(tty, banner_addr, banner_len, CPF_READ) else {
        return diag_fmt(format_args!("cdev.write FAIL grant"));
    };
    let rc = cdev_write(tty, CDEV_MINOR_CONSOLE, gid, banner_len as i32, 0);
    if rc == banner_len as i32 {
        // The reply *is* the byte count, so equality with the granted length is
        // the whole assertion. A driver replying `OK` (0) fails this.
        diag_fmt(format_args!("cdev.write ok match=1"));
    } else {
        diag_fmt(format_args!("cdev.write FAIL rc={rc}"));
    }
    let _ = grants.revoke(gid);

    // 3. The short write: more than one call can carry.
    let rule_addr = (&raw const TTY_RULE) as usize as u64;
    let Ok(rule_gid) = grants.grant_direct(tty, rule_addr, TTY_RULE_LEN as u64, CPF_READ) else {
        return diag_fmt(format_args!("cdev.short FAIL grant"));
    };
    let rc = cdev_write(tty, CDEV_MINOR_CONSOLE, rule_gid, TTY_RULE_LEN as i32, 0);
    if rc == CDEV_MAX_IO as i32 {
        diag_fmt(format_args!("cdev.short ok n={rc}"));
    } else {
        diag_fmt(format_args!("cdev.short FAIL rc={rc}"));
    }
    let _ = grants.revoke(rule_gid);

    // 4. The requests that must be refused.
    cdev_denials(grants, tty, banner_addr, banner_len);
}

/// Resolve TTY's endpoint through DS, falling back to its boot endpoint.
///
/// The lookup is the point — a client should not hard-code a peer's boot proc
/// number — but DS publish-before-retrieve is **not** guaranteed by construction.
/// It works today because `kernel/build.rs` packs TTY before VFS, so TTY's
/// `DS_PUBLISH` reaches DS's FIFO first and completes before VFS's own
/// `sef_startup` returns. Rather than let that archive ordering become load-bearing,
/// a failed lookup falls back to `boot_endpoint(TTY_PROC_NR)` and emits a
/// *distinguishable* diag line: the rest of the demo still runs and still proves the
/// CDEV path, while the required `cdev.ds ok` marker disappears and CI goes red on
/// the ordering regression specifically.
#[cfg_attr(test, allow(dead_code))]
fn tty_endpoint() -> Endpoint {
    let mut key = [0u8; SYS_GETINFO_NAME_LEN];
    key[0..3].copy_from_slice(b"tty");
    match sef_retrieve_from_ds(&key) {
        Ok(ep) => {
            diag_fmt(format_args!("cdev.ds ok ep={ep}"));
            ep
        }
        Err(rc) => {
            let ep = boot_endpoint(TTY_PROC_NR);
            diag_fmt(format_args!("cdev.ds FAIL rc={rc} fallback={ep}"));
            ep
        }
    }
}

/// Probe the two `CDEV_WRITE` refusals that no successful write exercises.
///
/// Slice 5.1's lesson, again: a check no marker exercises is a check that can
/// regress silently. Each probe is well-formed in every respect but one.
///
///   - `bad-minor` — a perfectly good grant aimed at minor 7. The driver's own
///     minor check is the only thing that stops it, and the answer is `ENXIO`
///     because the *device* does not exist; nothing is wrong with the grant.
///   - `not-mine` — a grant over the same bytes, issued to **PM** instead of TTY.
///     TTY passes it to `SYS_SAFECOPY` in good faith and the *kernel* refuses it,
///     on `verify_grant`'s `who_to == caller's stored endpoint` check. `EPERM`,
///     and it is the check that makes a grant id safe to pass around at all: a
///     leaked id is useless to anyone but its named grantee.
#[cfg_attr(test, allow(dead_code))]
fn cdev_denials(grants: &mut GrantPool<GRANT_SLOTS>, tty: Endpoint, addr: u64, len: u64) {
    let (Ok(good), Ok(not_mine)) = (
        grants.grant_direct(tty, addr, len, CPF_READ),
        grants.grant_direct(boot_endpoint(PM_PROC_NR), addr, len, CPF_READ),
    ) else {
        return diag_fmt(format_args!("cdev.deny FAIL setup"));
    };

    let probes: [(&str, i32, i32, i32); 2] = [
        ("bad-minor", 7, good, ENXIO),
        ("not-mine", CDEV_MINOR_CONSOLE, not_mine, EPERM),
    ];

    let mut denied = 0usize;
    for (name, minor, gid, want) in probes {
        let rc = cdev_write(tty, minor, gid, len as i32, 0);
        if rc == want {
            denied += 1;
        } else {
            diag_fmt(format_args!("cdev.deny FAIL {name} rc={rc}"));
        }
    }
    if denied == probes.len() {
        diag_fmt(format_args!("cdev.deny ok n={denied}"));
    }
    let _ = grants.revoke(good);
    let _ = grants.revoke(not_mine);
}

/// Issue one `CDEV_WRITE` to `tty` and return the reply `m_type` — the byte count
/// written, or a negative errno.
///
/// No granter goes in the payload: TTY takes it from the kernel-stamped `m_source`,
/// so this message cannot aim TTY's privileged `SYS_SAFECOPY` anywhere but VFS's own
/// address space.
#[cfg_attr(test, allow(dead_code))]
fn cdev_write(tty: Endpoint, minor: i32, gid: i32, len: i32, offset: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: CDEV_WRITE,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, CDEV_MINOR_OFF, minor);
    wr_i32(&mut m, CDEV_GRANT_OFF, gid);
    wr_i32(&mut m, CDEV_LEN_OFF, len);
    wr_u64(&mut m, CDEV_OFFSET_OFF, offset);
    let trap_rc = ipc_sendrec(tty, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// SEF fresh-init callback: publish VFS's endpoint to DS under its name. DS
/// registers the caller's kernel-stamped endpoint, so `_endpoint` from
/// `GET_WHOAMI` is not sent.
#[cfg_attr(test, allow(dead_code))]
fn vfs_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(name)
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}

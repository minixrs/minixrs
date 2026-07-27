// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs VFS (virtual file system) server — the POSIX write path (slice 5.4),
//! plus the grant and console demos slices 5.2 and 5.3 left behind as regression
//! probes.
//!
//! Slice 4.2 stood VFS up as a real boot server: it boots through the SEF
//! framework and publishes its endpoint to DS. Slice 5.2 gave it the granting half
//! of the first cross-address-space copy ([`grant_test`]), and 5.3 made it the
//! first client of the TTY console driver ([`tty_demo`]).
//!
//! Slice 5.4 makes it a *file system* server, in the one sense that matters at
//! this stage: an ordinary user process can now write to a descriptor. See
//! [`do_write`].
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
mod write;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_BLOCK_OFF, BDEV_GRANT_OFF, BDEV_LEN_OFF, BDEV_MAX_IO, BDEV_MINOR_OFF, BDEV_MINOR_RAMDISK,
    BDEV_READ, BDEV_RQ_BASE, BDEV_WRITE, CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MAX_IO,
    CDEV_MINOR_CONSOLE, CDEV_MINOR_OFF, CDEV_OFFSET_OFF, CDEV_WRITE, GET_RAMDISK, NR_BDEV_MSGS,
    PM_GRANT_TEST, SYS_GETINFO_NAME_LEN, VFS_WRITE,
};
use minixrs_kernel_shared::com::{MEM_PROC_NR, PM_PROC_NR, TTY_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::{Endpoint, endpoint_proc};
use minixrs_kernel_shared::error::{EBADF, EINVAL, ENOSYS, ENXIO, EPERM, EROFS, OK};
use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE};
use minixrs_kernel_shared::rootfs::{
    IMAGE_HDR_LEN, IMAGE_LABEL, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL, ROOTFS_IMAGE_BLOCKS,
    ROOTFS_TAIL_BLOCK,
};
use minixrs_server_rt::{
    GrantPool, SefConfig, buf_addr, diag_fmt, sef_publish_to_ds, sef_retrieve_from_ds, sef_startup,
    sys_getinfo, wr_i32, wr_u64,
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

    // Resolve the console driver once, before serving anyone: the demos below and
    // every `VFS_WRITE` that follows all target it, and a DS lookup per write
    // would be a round-trip per write for an endpoint that cannot change.
    let tty = tty_endpoint();

    grant_test(&mut grants);
    tty_demo(&mut grants, tty);
    // The block demo runs **last**, and the order is load-bearing: it is the
    // newest and least-proven code in this prologue, so a hang inside it localizes
    // to the `bdev.*` markers instead of blacking out 5.2's, 5.3's, and 5.4's as
    // well. Do not tidy this prologue into alphabetical order.
    bdev_demo(&mut grants, mem_endpoint());

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
        // the write's magic grant, and the process whose fd table is consulted —
        // and the dispatch below overwrites `msg.m_source` on the way out.
        let caller_e = msg.m_source;
        match msg.m_type {
            VFS_WRITE => {
                let rc = do_write(caller_e, &msg, &mut grants, tty);
                reply(caller_e, &mut msg, rc);
            }
            // Reply rather than drop (TTY's rule): VFS's clients are all inside a
            // SENDREC, and a dropped request blocks the caller forever. DS can
            // afford to drop one only because nothing SENDRECs it in anger.
            _ => reply(caller_e, &mut msg, ENOSYS),
        }
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
    let req = write::parse(msg);

    let minor = match fd::resolve(endpoint_proc(caller_e).get(), req.fd) {
        Ok(Fd::CharDev { minor }) => minor,
        // `resolve` maps a closed descriptor to `EBADF` itself, so this arm is
        // unreachable today. It exists so slice 5.8's regular-file variant shows
        // up as a compile error to be routed, not a silent fallthrough.
        Ok(Fd::Unused) => return EBADF,
        Err(e) => return e,
    };

    let len = match write::validate(req.len, req.buf) {
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
/// report lives in [`write::advance`], which is where its rules are documented and
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
        match write::advance(off, len, n) {
            write::Step::More(next) => off = next,
            write::Step::Done(rc) => return rc,
        }
    }
}

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

// ----- Slice 5.7: the BDEV ramdisk demo -------------------------------------

/// Bytes each block probe moves: the image header's length, which is also the
/// tail label's.
///
/// Small on purpose. VFS's stack is one page, so a whole-block (4096-byte) local
/// would run it off the end and fault into VM's out-of-region SIGSEGV arm — which
/// prints no `!!! EL0 data abort` for the forbidden list to catch. That is the
/// slice-5.5 lesson, and it is why this demo does not read a superblock (which
/// starts at byte 1024) either.
const BDEV_PROBE_LEN: usize = IMAGE_HDR_LEN;

/// One denial probe: a request that is well-formed in every respect but one.
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

/// Resolve the `memory` driver's endpoint through DS, falling back to its boot
/// endpoint.
///
/// A copy of [`tty_endpoint`], and it carries the same contract: the lookup is the
/// point, but DS publish-before-retrieve is **not** guaranteed by construction — it
/// works because `kernel/build.rs` packs `memory` before `vfs`. So a failed lookup
/// falls back and emits a *distinguishable* line: the rest of the demo still runs
/// and still proves the BDEV path, while the required `bdev.ds ok` marker
/// disappears and CI goes red on the ordering regression specifically.
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

/// Slice 5.7 demo: read the ramdisk's first and last blocks over `BDEV_READ`.
///
/// **Why a live client at all**, when slice 5.8 is what really needs BDEV: there is
/// no successful `SAFECOPY_TO` anywhere else in the tree. Its only other uses are
/// PM's two denial probes, which expect `EPERM` and `EFAULT`. `BDEV_READ` is the
/// first copy that writes *into* a grantee's buffer and succeeds, so without this
/// the kernel's `CPF_WRITE` + `Prot::writable` success path would stay unproven
/// until 5.8 — which would then be debugging a new protocol, a new server, and a
/// never-exercised copy direction simultaneously. The anti-spoof rule (the absence
/// of a granter field) also cannot be host-tested, and `EROFS`-vs-`ENOSYS` is just
/// a comment until something observes the difference.
///
/// Four things get proven, in order:
///
/// 1. **DS lookup**, by [`mem_endpoint`] above.
/// 2. **A real read.** The reply *is* the byte count, so `n=32` fails for a driver
///    that replied `OK`.
/// 3. **The header survived** IPC, the grant, and the safecopy.
/// 4. **The `block` field reached the right page.** Head and tail do not subsume
///    each other: a driver that ignored `block` would return the header for both,
///    and the *driver's* own `tail=1` check proves only that the kernel's copy loop
///    reached the end of the blob, not that BDEV indexes into it correctly.
///
/// The granted buffer is a **local**, not a `static`: the driver writes into it, and
/// VFS's other granted buffers are all `.rodata` (right for `CPF_READ`, and the
/// kernel's `Prot::writable` check would refuse a copy into one).
///
/// Best-effort throughout: a failure here must not stop VFS from serving.
#[cfg_attr(test, allow(dead_code))]
fn bdev_demo(grants: &mut GrantPool<GRANT_SLOTS>, mem: Endpoint) {
    let mut buf = [0u8; BDEV_PROBE_LEN];
    let addr = buf_addr(&mut buf);
    let len = BDEV_PROBE_LEN as u64;

    // `CPF_WRITE`, because the driver copies *into* this buffer. That direction is
    // what makes this the first live exercise of the kernel's write-side grant
    // checks.
    let Ok(gid) = grants.grant_direct(mem, addr, len, CPF_WRITE) else {
        return diag_fmt(format_args!("bdev.read FAIL grant"));
    };

    // 2 + 3. Block 0: the image header.
    let rc = bdev_request(mem, BDEV_READ, BDEV_MINOR_RAMDISK, gid, len as i32, 0);
    if rc == len as i32 {
        diag_fmt(format_args!("bdev.read ok n={rc}"));
    } else {
        diag_fmt(format_args!("bdev.read FAIL rc={rc}"));
    }
    if buf[..IMAGE_LABEL_LEN] == IMAGE_LABEL {
        diag_fmt(format_args!("bdev.head ok match=1"));
    } else {
        diag_fmt(format_args!("bdev.head FAIL"));
    }

    // 4. The last block: the tail label, which differs from the header's. Zero the
    // buffer first — otherwise a read that moved nothing at all would leave the
    // header bytes in place and the comparison below would be against stale data
    // rather than against what the driver returned.
    buf = [0u8; BDEV_PROBE_LEN];
    let rc = bdev_request(
        mem,
        BDEV_READ,
        BDEV_MINOR_RAMDISK,
        gid,
        len as i32,
        u64::from(ROOTFS_TAIL_BLOCK),
    );
    if rc == len as i32 && buf[..IMAGE_LABEL_LEN] == IMAGE_TAIL_LABEL {
        diag_fmt(format_args!("bdev.tail ok match=1"));
    } else {
        diag_fmt(format_args!("bdev.tail FAIL rc={rc}"));
    }
    let _ = grants.revoke(gid);

    bdev_denials(grants, mem, addr);
}

/// Probe every `BDEV_READ` refusal that a successful read does not exercise.
///
/// Slice 5.1's lesson, again: a check no marker exercises is a check that can
/// regress silently. Each probe is well-formed in every respect but one.
///
///   - `bad-minor` — a good grant aimed at minor 7. The driver's own minor check
///     is the only thing that stops it: `ENXIO`, because the *device* does not
///     exist; nothing is wrong with the grant.
///   - `bad-block` — one block past the end. `EINVAL` rather than `EIO`: the
///     device's size is known to its client, so this is a caller bug.
///   - `too-long` — one byte more than [`BDEV_MAX_IO`]. The deliberate departure
///     from CDEV: `EINVAL`, not a short read.
///   - `neg-len` — a negative length, which unchecked would widen into a ~16 EiB
///     `u64` byte count for the kernel copy.
///   - `not-mine` — the same bytes granted to **PM** instead of the driver. The
///     driver passes it to `SYS_SAFECOPY` in good faith and the *kernel* refuses
///     it, on `verify_grant`'s `who_to` check.
///   - `read-only` — a `CPF_READ`-only grant used as a copy *destination*.
///     **The access mask in the write direction, which slice 5.3 never
///     exercised**: every grant in the tree until now was read-side.
///   - `write` — `BDEV_WRITE`, which must answer `EROFS`. Fold that arm into the
///     driver's `_` case and this becomes `ENOSYS`, which is the whole reason the
///     request has a number of its own.
///   - `unknown` — a request one past the band. `ENOSYS`, and **the reply itself is
///     the assertion**: a driver that dropped an unknown request would leave VFS
///     blocked in its SENDREC forever and take every later marker with it.
///
/// Plus one probe that is not a BDEV request at all: `SYS_GETINFO(GET_RAMDISK)`
/// issued by VFS, which the kernel must refuse with `EPERM`. The ramdisk is mapped
/// into exactly one address space, so that gate is what stops the VA being handed
/// to a process where it faults — and VFS, a `SYS_PROC` server that holds the call,
/// is the strongest caller available to test it with.
#[cfg_attr(test, allow(dead_code))]
fn bdev_denials(grants: &mut GrantPool<GRANT_SLOTS>, mem: Endpoint, addr: u64) {
    let len = BDEV_PROBE_LEN as u64;
    let (Ok(good), Ok(not_mine), Ok(read_only)) = (
        grants.grant_direct(mem, addr, len, CPF_WRITE),
        grants.grant_direct(boot_endpoint(PM_PROC_NR), addr, len, CPF_WRITE),
        grants.grant_direct(mem, addr, len, CPF_READ),
    ) else {
        return diag_fmt(format_args!("bdev.deny FAIL setup"));
    };

    let n = len as i32;
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
        // A write to a device that does not exist. `ENXIO`, not `EROFS`: the
        // driver validates the request before refusing it, so it never asserts
        // read-onlyness about a device it does not have. Without this probe the
        // `BDEV_WRITE` arm could be reduced back to a bare `EROFS` and no marker
        // would move.
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
        let rc = bdev_request(mem, p.m_type, p.minor, p.gid, p.len, p.block);
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
    for gid in [good, not_mine, read_only] {
        let _ = grants.revoke(gid);
    }
}

/// Issue one block-device request and return the reply `m_type` — the byte count
/// read, or a negative errno.
///
/// `m_type` is a parameter rather than hardcoded to `BDEV_READ` so the `BDEV_WRITE`
/// and unknown-request probes ride the same marshaling as a real read; a probe
/// built by a second, hand-written marshaller would prove less.
///
/// No granter goes in the payload — the driver takes it from the kernel-stamped
/// `m_source`, so this message cannot aim the driver's privileged `SYS_SAFECOPY`
/// anywhere but VFS's own address space. There is no grant-offset field either.
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

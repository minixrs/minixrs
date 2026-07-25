// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs VFS (virtual file system) server — skeletal boot (slice 4.2), plus the
//! slice-5.2 grant demo and the slice-5.3 console demo.
//!
//! Slice 4.2 stood VFS up as a real boot server: it boots through the SEF
//! framework and publishes its endpoint to DS, proving the multi-server boot
//! path and the DS registry end to end. It does *no* file operations yet — the
//! PM↔VFS fork/exec work protocol needs file descriptors and is Phase 5 — so the
//! receive loop simply drops any application traffic that arrives.
//!
//! Slice 5.2 gives VFS the granting half of the first real cross-address-space
//! copy: it direct-grants a checksummed read-only buffer to PM and hands PM the
//! grant id in a `PM_GRANT_TEST` message. See [`grant_test`].
//!
//! Slice 5.3 makes VFS the first client of the TTY console driver: it looks TTY up
//! in DS and drives `CDEV_WRITE` — a real write, a short write, and two refusals.
//! See [`tty_demo`]. That is not throwaway wiring: slice 5.4 puts VFS's fd 1 and 2
//! on this exact path.
//!
//! Built as a freestanding aarch64 ELF (see `servers/vfs/user.ld`), packed into
//! the kernel's boot-image archive by `kernel/build.rs`, and loaded into its own
//! per-process AddrSpace at boot by `arch::aarch64::userland::load_boot_server`.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` (no host tests yet — the SEF/IPC glue is QEMU-verified). The
// `_start` shim and panic handler are gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MAX_IO, CDEV_MINOR_CONSOLE, CDEV_MINOR_OFF, CDEV_OFFSET_OFF,
    CDEV_WRITE, PM_GRANT_TEST, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{PM_PROC_NR, TTY_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{ENXIO, EPERM, OK};
use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE};
use minixrs_server_rt::{
    GrantPool, SefConfig, diag_fmt, sef_publish_to_ds, sef_retrieve_from_ds, sef_startup, wr_i32,
    wr_u64,
};

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
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
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
    grant_test(&mut grants);
    tty_demo(&mut grants);

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        // No file ops yet (Phase 5): receive and drop application traffic. The
        // SEF framework still services control traffic (pings/signals/re-init)
        // inside `receive`; only the application messages it hands back are
        // discarded here.
        let _ = sef.receive(&mut msg);
    }
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

/// Slice 5.3 demo: drive the TTY console driver over `CDEV_WRITE`.
///
/// VFS is the natural client — it is already the granter in the 5.2 demo, it
/// already owns a [`GrantPool`], and slice 5.4 puts its fd 1/2 on exactly this
/// path, so the wiring is not throwaway.
///
/// Four things get proven, in order:
///
/// 1. **DS lookup.** VFS asks DS for TTY's endpoint rather than hard-coding
///    `boot_endpoint(TTY_PROC_NR)`, which is what a real client does. On failure it
///    *does* fall back to the boot endpoint and says so — see [`tty_endpoint`].
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
fn tty_demo(grants: &mut GrantPool<GRANT_SLOTS>) {
    let tty = tty_endpoint();

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

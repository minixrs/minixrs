// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs VFS (virtual file system) server — skeletal boot (slice 4.2), plus
//! the slice-5.2 grant demo.
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
//! Built as a freestanding aarch64 ELF (see `servers/vfs/user.ld`), packed into
//! the kernel's boot-image archive by `kernel/build.rs`, and loaded into its own
//! per-process AddrSpace at boot by `arch::aarch64::userland::load_boot_server`.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` (no host tests yet — the SEF/IPC glue is QEMU-verified). The
// `_start` shim and panic handler are gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

use minixrs_ipc::ipc_send;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{PM_GRANT_TEST, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::com::{PM_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::grant::{CPF_READ, CPF_WRITE};
use minixrs_server_rt::{GrantPool, SefConfig, sef_publish_to_ds, sef_startup};

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
/// how grant ids really travel (slice 5.3's `CDEV_WRITE {minor, granter,
/// grant_id, len}` has the same shape). DS could not carry one: `DS_PUBLISH`
/// registers the kernel-stamped `m_source` and ignores the payload, which is
/// exactly its anti-spoof property.
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

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `memory` driver — the boot ramdisk (slice 5.7, decision D3), plus
//! `/dev/null` and `/dev/zero` as of slice 5.11.
//!
//! The first *block* device in minix.rs, and the storage MFS lands on in slice 5.8.
//! `kernel/build.rs` builds a MinixFS v3 image at compile time and packs it into
//! the MXBI archive as a non-ELF blob; the kernel copies it into RAM frames and
//! maps them into this driver's address space at [`RAMDISK_VA`] while building it.
//! This driver asks where (`SYS_GETINFO(GET_RAMDISK)`, which only it may call) and
//! then serves `BDEV_READ`.
//!
//! MINIX 3 boots the same shape, and Phase 6 swaps virtio-blk in underneath an
//! unchanged MFS — which is why nothing here knows what a filesystem is.
//!
//! ## Five things worth knowing
//!
//! **This driver never dereferences the mapping, and contains no `unsafe` block.**
//! (The only `unsafe` tokens in the crate are the two ELF-only attributes on
//! `_start`, which every freestanding binary here carries.) A client transfer is
//! `SYS_SAFECOPY`, and even the boot self-check goes through `SYS_COPY(SELF →
//! SELF)` rather than a raw load. So a page the kernel failed to map surfaces as
//! an `EFAULT` *return value* from a kernel call — a better diagnostic than TTY's
//! equivalent, which would be an EL0 data abort — and there is no MMIO sibling
//! module and no new Sonar exclusion.
//!
//! **The granter comes from `m_source`.** This driver holds `SYS_SAFECOPY`; its
//! clients do not. A granter field in the payload would let any client aim a
//! privileged cross-address-space copy at a third party's memory through it.
//! `bdev.rs` has no way to express one.
//!
//! **An over-long or out-of-range request is `EINVAL`.** Not a short read (a
//! filesystem cannot interpret half a block) and not `EIO` (which stays reserved
//! for Phase 6's real media errors). See `bdev.rs`.
//!
//! **`BDEV_WRITE` stores, as of slice 5.10a.** It was defined and answering
//! `EROFS` from slice 5.7 — deliberately not folded into the unknown-request
//! `ENOSYS` arm, because "does not know about writes" and "knows and refuses" are
//! different things to tell a client. That distinction is what made this a
//! one-arm change: the geometry validation was already here, so 5.10a replaced a
//! refusal with a `SAFECOPY_FROM` rather than adding a request.
//!
//! **Two character minors ride the same driver, on a different band.** MINIX 3's
//! memory driver owns `/dev/null` and `/dev/zero` beside its ramdisks, and so does
//! this one — `CDEV_WRITE` discards and answers the full count, `CDEV_READ`
//! answers `0` for null and fills the whole request for zero. Nothing is clamped
//! (there is no staging buffer to protect), and a null/zero *write* issues no
//! `SYS_SAFECOPY` at all, so a write with an unmapped buffer succeeds — Linux's
//! behaviour, and the reason no `bad-buf` probe may ever be aimed at `/dev/null`.
//! See `cdev.rs`.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `bdev`'s logic gets host-runnable unit tests. The test harness
// needs `std` and its own entry point, so `no_std`/`no_main` and the `_start` shim
// below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod bdev;
mod cdev;

use minixrs_ipc::ipc_send;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_BLOCK_SIZE, BDEV_READ, BDEV_WRITE, CDEV_MAX_IO, CDEV_READ, CDEV_WRITE, GET_RAMDISK,
    GETINFO_RAMDISK_LEN_OFF, GETINFO_RAMDISK_VA_OFF, SAFECOPY_FROM, SAFECOPY_TO,
    SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::endpoint::{Endpoint, SELF};
use minixrs_kernel_shared::error::{EINVAL, ENOSYS, OK};
use minixrs_kernel_shared::rootfs::{
    HDR_BLOCKS_OFF, IMAGE_HDR_LEN, IMAGE_LABEL, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL,
};
use minixrs_kernel_shared::uspace::RAMDISK_VA;
use minixrs_server_rt::{
    SefConfig, buf_addr, diag_fmt, rd_u64, sef_publish_to_ds, sef_startup, sys_copy, sys_getinfo,
    sys_safecopy,
};

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` can dive
/// straight into Rust without setting up a stack itself.
// Gate the whole shim to `not(test)`: under `cargo test` the crate links as a
// normal host executable, and an exported `_start` would collide with the C
// runtime's. `.text._start` is an ELF section name, gated to the minixrs target
// so `cargo check --workspace` on a Mach-O host still type-checks.
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
        init_fresh: Some(mem_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        // No recovery and nothing to print: a failed handshake means this driver
        // never learned its own endpoint, so it cannot even announce the failure.
        loop {
            core::hint::spin_loop()
        }
    });

    // Where the ramdisk is, and how big. Locals in `main`'s frame — not statics,
    // and not `mem_init`'s frame, whose stack is gone by the time a client calls —
    // the same rule `GrantPool` and TTY's staging buffer follow.
    //
    // Deliberately *after* `sef_startup` rather than inside `mem_init`: the kernel
    // gates `GET_RAMDISK` on the caller's proc number, so a failure here means the
    // kernel's pre-map arm and its getinfo gate disagree — worth its own diag line
    // and a degraded-but-alive driver, not an aborted startup that also takes the
    // `sef ready` marker with it.
    let (va, blocks) = ramdisk_geometry();

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        // Capture the caller *first*: it is both the reply target and the granter,
        // and the dispatch below overwrites `msg.m_source` on the way out.
        let caller_e = msg.m_source;
        match msg.m_type {
            BDEV_READ => {
                let rc = do_read(caller_e, &msg, va, blocks);
                reply(caller_e, &mut msg, rc);
            }
            // A distinct number from the `_` arm below, not folded into `ENOSYS`
            // — that difference is what let 5.7 answer `EROFS` and 5.10a turn it
            // into a real store without a client-visible change of shape.
            BDEV_WRITE => {
                let rc = do_write(caller_e, &msg, va, blocks);
                reply(caller_e, &mut msg, rc);
            }
            // Slice 5.11: the character minors. Same driver, different band —
            // `cdev::classify` refuses the ramdisk's minor 0 here, because minors
            // are a per-driver namespace and the band, not the minor value, is
            // what tells the ramdisk and the character minors apart here.
            CDEV_WRITE => {
                let rc = do_cdev_write(&msg);
                reply(caller_e, &mut msg, rc);
            }
            CDEV_READ => {
                let rc = do_cdev_read(caller_e, &msg);
                reply(caller_e, &mut msg, rc);
            }
            // Unlike DS, reply rather than drop — the caller is inside a SENDREC
            // and would otherwise block forever.
            _ => reply(caller_e, &mut msg, ENOSYS),
        }
    }
}

/// SEF fresh-init callback: publish to DS. Nothing else — see [`ramdisk_geometry`]
/// for why the ramdisk lookup happens in `main`'s frame instead.
#[cfg_attr(test, allow(dead_code))]
fn mem_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(name)
}

/// Ask the kernel where the ramdisk is, check it over, and announce the result.
///
/// Returns `(va, blocks)`. On any failure `blocks` is `0`, which makes every
/// subsequent `BDEV_READ` fail its range check with `EINVAL` rather than letting a
/// bad geometry reach `SYS_SAFECOPY`. A zero-block device is exactly the right
/// degenerate state: clients hear "out of range" for every block, and the driver
/// still answers every request.
#[cfg_attr(test, allow(dead_code))]
fn ramdisk_geometry() -> (u64, u64) {
    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    let rc = sys_getinfo(GET_RAMDISK, &mut msg);
    if rc != OK {
        diag_fmt(format_args!("ramdisk FAIL getinfo rc={rc}"));
        return (RAMDISK_VA, 0);
    }
    let va = rd_u64(&msg, GETINFO_RAMDISK_VA_OFF);
    let len = rd_u64(&msg, GETINFO_RAMDISK_LEN_OFF);

    let blocks = match bdev::validate_geometry(len) {
        Ok(blocks) => blocks,
        Err(e) => {
            diag_fmt(format_args!("ramdisk FAIL geometry len={len} rc={e}"));
            return (va, 0);
        }
    };
    if selfcheck(va, len, blocks) {
        (va, blocks)
    } else {
        (va, 0)
    }
}

/// Prove the mapping is real, complete, and holds the image this build packed.
///
/// Two 32-byte reads through `SYS_COPY(SELF → SELF)`, which the kernel supports:
/// `do_copy` short-circuits `SELF` for both endpoints, and `copy_between_as` over
/// two disjoint ranges of one address space is an ordinary copy. Going through a
/// kernel call rather than dereferencing is what keeps this crate free of `unsafe`
/// blocks and turns an unmapped page into an errno instead of a fault.
///
/// The **head** read checks the image label and cross-checks the header's own
/// block count against the length the kernel reported — binding mkfs's geometry to
/// the kernel's copy, two independently derived numbers.
///
/// The **tail** read is the one that earns its keep. It reads the reserved last
/// block, whose label deliberately differs from the header's, so a kernel copy
/// loop that failed to advance — mapping 256 pages of block 0 — fails here while
/// every header check still passes. Nothing else in the boot proves the copy
/// reached the end of the blob.
///
/// Deliberately a *device*-level check: no superblock is decoded and
/// `minixrs-mfs` is not a dependency, because a block driver must not depend on
/// the filesystem format. `tools/mkfs-mfs`'s round trip is what proves the header
/// cannot disagree with the real superblock.
#[cfg_attr(test, allow(dead_code))]
fn selfcheck(va: u64, len: u64, blocks: u64) -> bool {
    let mut buf = [0u8; IMAGE_HDR_LEN];

    let rc = sys_copy(SELF, va, SELF, buf_addr(&mut buf), IMAGE_HDR_LEN as u64);
    if rc != OK {
        diag_fmt(format_args!("ramdisk FAIL head rc={rc}"));
        return false;
    }
    if buf[..IMAGE_LABEL_LEN] != IMAGE_LABEL {
        diag_fmt(format_args!("ramdisk FAIL label"));
        return false;
    }
    let hdr_blocks = rd_hdr_u32(&buf, HDR_BLOCKS_OFF);
    if u64::from(hdr_blocks) != blocks {
        diag_fmt(format_args!(
            "ramdisk FAIL blocks hdr={hdr_blocks} dev={blocks}"
        ));
        return false;
    }

    // `len` is non-zero and a whole number of blocks (`validate_geometry`), so this
    // cannot underflow and names the last block's first byte.
    let tail_va = va + len - BDEV_BLOCK_SIZE as u64;
    let rc = sys_copy(
        SELF,
        tail_va,
        SELF,
        buf_addr(&mut buf),
        IMAGE_HDR_LEN as u64,
    );
    if rc != OK {
        diag_fmt(format_args!("ramdisk FAIL tail rc={rc}"));
        return false;
    }
    if buf[..IMAGE_LABEL_LEN] != IMAGE_TAIL_LABEL {
        diag_fmt(format_args!("ramdisk FAIL tail label"));
        return false;
    }

    diag_fmt(format_args!("ramdisk ok blocks={blocks} tail=1"));
    true
}

/// Read a little-endian `u32` out of the image header.
///
/// `from_le_bytes` rather than one of `server-rt`'s payload accessors: those
/// describe *message* fields, which are native-endian by construction because the
/// kernel copies them between two processes on the same machine. This is an
/// on-disk image written by a build host, so it is explicitly little-endian — the
/// same assumption the MXBI archive already makes.
fn rd_hdr_u32(buf: &[u8; IMAGE_HDR_LEN], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Serve one `BDEV_READ`. Returns the reply `m_type`: the byte count read
/// (`>= 0`), or a negative errno.
#[cfg_attr(test, allow(dead_code))]
fn do_read(caller_e: Endpoint, msg: &Message, va: u64, blocks: u64) -> i32 {
    let req = bdev::parse_read(msg);
    let (byte_off, n) = match bdev::validate_read(req, blocks) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if n == 0 {
        // A legal zero-length read. No grant is used and nothing is copied, so a
        // client polling with `len = 0` cannot use it to probe a grant.
        return 0;
    }

    // Push the bytes into the client's granted buffer. `caller_e` is the
    // kernel-stamped source of this very message — the granter can be nothing else.
    //
    // The grant offset is `0`, not a payload field: `BDEV_READ` deliberately has
    // none, because every client through slice 5.9 grants a buffer whose block
    // starts at its beginning.
    let rc = sys_safecopy(SAFECOPY_TO, caller_e, req.gid, 0, va + byte_off, n as u64);
    if rc != OK {
        // Verbatim: EPERM ("your grant does not authorize this") and EFAULT ("your
        // buffer is not mapped") are different bugs on the client's side.
        return rc;
    }
    n as i32
}

/// Serve one `BDEV_WRITE`. Returns the reply `m_type`: the byte count stored
/// (`>= 0`), or a negative errno.
///
/// The mirror of [`do_read`], and deliberately built from the same parse and the
/// same validation: a write to minor 7 hears `ENXIO` ("no such device") before
/// anything else, the check-order discipline `validate_read` documents.
///
/// **The direction is the whole difference.** `SAFECOPY_FROM` pulls the client's
/// bytes into the device; the grant must therefore carry `CPF_READ` where a read
/// needs `CPF_WRITE`. The kernel enforces that in `verify_grant` — this driver
/// does not check it and must not, because a driver that re-derived the grant
/// rules would be a second place for them to drift.
///
/// The grant offset is `0`, not a payload field: `BDEV_WRITE` deliberately has
/// none, because every client grants a buffer whose block starts at its
/// beginning.
#[cfg_attr(test, allow(dead_code))]
fn do_write(caller_e: Endpoint, msg: &Message, va: u64, blocks: u64) -> i32 {
    let req = bdev::parse_read(msg);
    let (byte_off, n) = match bdev::validate_read(req, blocks) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if n == 0 {
        // A legal zero-length write. No grant is used and nothing is copied, so a
        // client polling with `len = 0` cannot use it to probe a grant.
        return 0;
    }

    let rc = sys_safecopy(SAFECOPY_FROM, caller_e, req.gid, 0, va + byte_off, n as u64);
    if rc != OK {
        // Verbatim: EPERM ("your grant does not authorize this") and EFAULT
        // ("your buffer is not mapped") are different bugs on the client's side.
        return rc;
    }
    n as i32
}

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
                done = done.saturating_add(chunk);
            }
            len as i32
        }
    }
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

// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `init` — PID 1, the first user process (slice 4.8).
//!
//! Unlike the demo stubs it replaces, `init` is a real boot module: it is packed
//! into the boot-image archive with its true proc number (`INIT_PROC_NR = 10`)
//! and loaded + made runnable by the ordinary boot loop
//! (`kernel/src/arch/aarch64/userland.rs`), not hand-released by PM. It runs as
//! an ordinary user process — the shared `USER_PRIV_ID` privilege (SENDREC to PM
//! and VFS, and no kernel calls at all) — so the whole process lifecycle flows
//! through PM, and every byte of output through VFS, in the POSIX shape
//! (user → server, never user → kernel).
//!
//! `init` is the live exercise for the Phase-4 process machinery that the
//! slice-4.6/4.7 stub E demonstrated: it forks a child, the child execs the
//! `worker` binary (which runs a few `PM_GETPID` round-trips then exits), and
//! the parent `wait`s
//! to reap the zombie before looping to fork again. Each cycle recycles the same
//! fork-pool slot with an advancing endpoint generation — observable in the boot
//! trace as `SYS_FORK` / `SYS_EXEC` / `SYS_EXIT` triples.
//!
//! Slice 5.4 gives it a voice. Before the fork loop it writes to **fd 1 and fd
//! 2** through VFS, which grants the buffer on to the TTY driver — the POSIX
//! write path, end to end, from an ordinary user process. init has no debug
//! channel of its own (`SYS_DIAGCTL` is a kernel call, and a user proc holds
//! none), so it reports *through the path under test*: if the path regresses the
//! lines change or vanish, and the boot markers go with them. See [`announce`].
//!
//! Built as a freestanding aarch64 ELF (`userland/init/user.ld`). It uses
//! `minix-ipc` directly — no `server-rt`/SEF, because it is a plain user program,
//! not a server. The `_start` shim and panic handler are gated to `not(test)`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    CDEV_MAX_IO, PM_EXEC, PM_FORK, PM_WAIT, VFS_BUF_OFF, VFS_FD_OFF, VFS_LEN_OFF, VFS_WRITE,
};
use minixrs_kernel_shared::com::{PM_PROC_NR, VFS_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EBADF, EFAULT, EINVAL, ENOSYS, OK};
use minixrs_kernel_shared::message::USER_VA_TOP;
use minixrs_kernel_shared::uspace::USER_DEVICE_WINDOW_BASE;

/// The boot loader primes `SP_EL0` before `eret`, so `_start` can dive straight
/// into Rust without setting up a stack itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

/// Standard output, pre-opened by VFS to the console — POSIX's inheritance
/// convention, which is what lets init write before any filesystem exists.
const STDOUT: i32 = 1;
/// Standard error, also the console. Used for the denial summary specifically so
/// that line proves fd 2 resolves as well as fd 1.
const STDERR: i32 = 2;

/// The milestone line: the first bytes an ordinary user process ever put on the
/// console under its own POSIX call.
const HELLO: &str = "minix.rs init: hello via VFS write\n";

/// A line long enough that no single `CDEV_WRITE` can carry it, so VFS must loop.
///
/// Sized from [`CDEV_MAX_IO`] — the driver's staging limit — even though a user
/// process has no business knowing that number: reading it here is what keeps the
/// probe *meaningful* if the limit ever moves, instead of silently degrading into
/// a line that fits in one transfer. That the constant is invisible in `write()`'s
/// result is the property under test.
const LOOP_LINE_LEN: usize = CDEV_MAX_IO + 32;

/// The line itself: a rule of dashes ending in a marker that only reaches the
/// console if VFS re-sent with `offset` advanced. Without the loop the console
/// gets the first [`CDEV_MAX_IO`] bytes and `vfs-loop-end` is never printed.
static LOOP_LINE: [u8; LOOP_LINE_LEN] = loop_line_pattern();

const fn loop_line_pattern() -> [u8; LOOP_LINE_LEN] {
    let mut b = [b'-'; LOOP_LINE_LEN];
    let prefix = b"minix.rs init: vfs long write ";
    let mut i = 0;
    while i < prefix.len() {
        b[i] = prefix[i];
        i += 1;
    }
    // The tail sits at the very end, past the clamp, and ends the line.
    let tail = b"vfs-loop-end\n";
    let base = LOOP_LINE_LEN - tail.len();
    let mut j = 0;
    while j < tail.len() {
        b[base + j] = tail[j];
        j += 1;
    }
    b
}

const _: () = assert!(LOOP_LINE_LEN > CDEV_MAX_IO);

/// A virtual address that is **not** mapped in init's address space, for the
/// `EFAULT` probe.
///
/// init's image is linked at `0x0010_0000` (`user.ld`) and the kernel maps it a
/// single stack page at `0x0020_0000`, so `0x0030_0000` is a hole. It costs init
/// no page fault: the kernel's copy engine *walks* the page tables and answers
/// `EFAULT` (D5) rather than dereferencing, so this probe cannot SIGSEGV the
/// process making it.
///
/// The probe only proves what it claims if the address is well-formed enough to
/// reach the page-table walk — an address VFS's own `user_range_ok` pre-check
/// rejected would answer `EFAULT` one hop earlier and silently test the wrong
/// thing. The guards below pin exactly that: non-NULL and below `USER_VA_TOP`, so
/// every range check on the way down passes, and clear of the kernel's device
/// window, which *is* mapped in some address spaces. The two layout facts it also
/// depends on — the link base and the stack VA — stay prose, because neither is a
/// shared constant a user program can see.
const UNMAPPED_VA: u64 = 0x0030_0000;

const _: () = assert!(UNMAPPED_VA != 0);
const _: () = assert!(UNMAPPED_VA < USER_VA_TOP);
const _: () = assert!(UNMAPPED_VA < USER_DEVICE_WINDOW_BASE);

/// Build a request message to PM: no payload, `m_source` is stamped by the kernel.
#[cfg_attr(test, allow(dead_code))]
fn pm_msg(m_type: i32) -> Message {
    Message {
        m_source: 0,
        m_type,
        payload: [0u8; 96],
    }
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let pm = boot_endpoint(PM_PROC_NR);
    // By boot endpoint, not through DS: init's privilege opens `ipc_to` to PM and
    // VFS alone, so it could not ask DS even if it wanted to. That is the point of
    // the user grade — a user process talks to servers, not to the registry.
    let vfs = boot_endpoint(VFS_PROC_NR);

    announce(vfs);

    loop {
        // fork(): PM replies to both halves of this SENDREC — the child sees
        // `m_type == 0`, the parent sees the child pid (`> 0`); a negative value
        // is an errno (e.g. `EAGAIN` when the fork table is full).
        let mut m = pm_msg(PM_FORK);
        let _ = ipc_sendrec(pm, &mut m);

        match m.m_type {
            0 => {
                // Child: replace this image with the `worker` binary. PM issues
                // `SYS_EXEC` and the kernel resumes us at worker's `_start`, so
                // this SENDREC never returns on success.
                let mut e = pm_msg(PM_EXEC);
                let _ = ipc_sendrec(pm, &mut e);
                // Unreachable on success; park defensively if exec ever failed.
                loop {
                    core::hint::spin_loop()
                }
            }
            n if n > 0 => {
                // Parent: reap the child (blocks until it exits), then loop to
                // fork the next one.
                let mut w = pm_msg(PM_WAIT);
                let _ = ipc_sendrec(pm, &mut w);
            }
            _ => {
                // Transient fork failure (table full): back off briefly, retry.
                for _ in 0..1024 {
                    core::hint::spin_loop()
                }
            }
        }
    }
}

/// Exercise the slice-5.4 write path, and report the result over it.
///
/// Three things go to the console, in order:
///
/// 1. **The banner** ([`HELLO`], fd 1) — the milestone. A user process, holding no
///    kernel calls and no grant table of its own, put bytes on the console by
///    calling `write`.
/// 2. **A line longer than one `CDEV_WRITE`** ([`LOOP_LINE`], fd 1), and the
///    *count* it returned. Two independent halves of the same contract: the tail
///    marker appears only if VFS re-sent with `offset` advanced, and `match=1`
///    says VFS reported the whole buffer rather than the driver's clamp. Neither
///    subsumes the other — a `write()` that moved every byte but returned `OK`
///    would print the tail and still be broken, which is exactly the bug a caller
///    looping on a short write would then hit forever.
/// 3. **Three denial probes and a summary** (fd 2, so that descriptor is proven
///    too). Each probe is well-formed in every respect but one — the slice-5.1
///    lesson that a check no marker exercises is a check that can regress
///    silently.
///
/// `match=1` rather than the length itself, the slice-5.3 convention: it is
/// self-checking, needs no formatting machinery (init has none), and does not have
/// to be re-derived when the line's length changes.
///
/// Best-effort: init's job is to keep the system running, so a failure here is
/// reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn announce(vfs: Endpoint) {
    let _ = vfs_write(vfs, STDOUT, HELLO.as_bytes());

    let rc = vfs_write(vfs, STDOUT, &LOOP_LINE);
    let verdict = if rc == LOOP_LINE_LEN as i32 {
        "minix.rs init: vfs.long ok match=1\n"
    } else {
        "minix.rs init: vfs.long FAIL\n"
    };
    let _ = vfs_write(vfs, STDOUT, verdict.as_bytes());

    deny_probes(vfs);
}

/// The `VFS_WRITE` requests that must be refused, each valid but for one field.
///
///   - `bad-fd` — a perfectly good buffer written to descriptor 3, which is in
///     range of the fd row but not open. `EBADF`, from VFS's fd table; make the
///     table accept any descriptor and this write *succeeds*.
///   - `no-such` — a request number one past `VFS_WRITE`. `ENOSYS`, from VFS's
///     dispatch — and the reply itself is the assertion: a server that dropped an
///     unknown request instead of replying would leave init blocked here forever,
///     taking the whole boot with it.
///   - `bad-buf` — an unmapped buffer ([`UNMAPPED_VA`]). `EFAULT`, from the
///     kernel's page-table walk in the middle of TTY's safecopy, relayed back out
///     through the driver and VFS unchanged. It proves the errno survives two
///     hops without being flattened into a generic failure.
///   - `bad-len` — a negative byte count. `EINVAL`, and the check matters more
///     than it looks: left unchecked the length widens into a ~16 EiB `u64` on the
///     grant VFS is about to issue over the caller's buffer.
///
/// The one arm with no probe here is a **zero**-length write, which is a legal
/// no-op rather than a denial and would need a distinct positive marker to say
/// anything. `write::validate` covers it — and the fact that it is checked before
/// the buffer, so a zero-length write never issues a grant — in host tests.
#[cfg_attr(test, allow(dead_code))]
fn deny_probes(vfs: Endpoint) {
    let addr = HELLO.as_ptr() as usize as u64;
    let len = HELLO.len() as i32;

    // (name, request, fd, buffer, length, expected reply)
    let probes: [(&str, i32, i32, u64, i32, i32); 4] = [
        ("bad-fd", VFS_WRITE, 3, addr, len, EBADF),
        ("no-such", VFS_WRITE + 1, STDOUT, addr, len, ENOSYS),
        ("bad-buf", VFS_WRITE, STDOUT, UNMAPPED_VA, len, EFAULT),
        ("bad-len", VFS_WRITE, STDOUT, addr, -1, EINVAL),
    ];

    for (name, m_type, fd, buf, len, want) in probes {
        if vfs_request(vfs, m_type, fd, buf, len) != want {
            return report_fail(vfs, name);
        }
    }
    let _ = vfs_write(vfs, STDERR, "minix.rs init: vfs.deny ok n=4\n".as_bytes());
}

/// Report the first failing probe by name, on fd 2.
///
/// Hand-assembled into a stack buffer: init has no formatting runtime (no
/// `server-rt`, so no `diag_fmt`), and the buffer's address is what VFS grants on
/// to the driver — legal because init stays blocked in the SENDREC for the whole
/// copy, so the frame outlives every use of it.
#[cfg_attr(test, allow(dead_code))]
fn report_fail(vfs: Endpoint, name: &str) {
    const PREFIX: &str = "minix.rs init: vfs.deny FAIL ";
    let mut line = [0u8; 64];
    let mut n = 0;
    for src in [PREFIX.as_bytes(), name.as_bytes(), b"\n"] {
        for b in src {
            if n < line.len() {
                line[n] = *b;
                n += 1;
            }
        }
    }
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}

/// `write(fd, buf, buf.len())` through VFS. Returns the byte count written, or a
/// negative errno.
#[cfg_attr(test, allow(dead_code))]
fn vfs_write(vfs: Endpoint, fd: i32, buf: &[u8]) -> i32 {
    vfs_request(
        vfs,
        VFS_WRITE,
        fd,
        buf.as_ptr() as usize as u64,
        buf.len() as i32,
    )
}

/// Send one request to VFS and return the reply `m_type`.
///
/// The buffer travels as a **raw address in init's own address space**, not as a
/// grant: init is an ordinary user process with no grant table and no privilege to
/// issue one. VFS is what turns this address into a magic grant naming init as the
/// owner — and it takes that owner from the kernel-stamped `m_source`, never from
/// anything in this payload, which is why there is no field here for it.
///
/// `m_type` is a parameter rather than a constant so the unknown-request probe can
/// reuse the exact same wire format and vary only the number.
#[cfg_attr(test, allow(dead_code))]
fn vfs_request(vfs: Endpoint, m_type: i32, fd: i32, buf: u64, len: i32) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type,
        payload: [0u8; 96],
    };
    m.payload[VFS_FD_OFF..VFS_FD_OFF + 4].copy_from_slice(&fd.to_ne_bytes());
    m.payload[VFS_LEN_OFF..VFS_LEN_OFF + 4].copy_from_slice(&len.to_ne_bytes());
    m.payload[VFS_BUF_OFF..VFS_BUF_OFF + 8].copy_from_slice(&buf.to_ne_bytes());

    let trap_rc = ipc_sendrec(vfs, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}

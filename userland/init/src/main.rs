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
//! slice-4.6/4.7 stub E demonstrated: it forks a child, the child execs a
//! boot-image binary, and the parent `wait`s to reap the zombie before looping
//! to fork again. Each cycle recycles the same fork-pool slot with an advancing
//! endpoint generation — observable in the boot trace as `SYS_FORK` /
//! `SYS_EXEC` / `SYS_EXIT` triples.
//!
//! Slice 5.6 makes the exec target the *caller's* choice — `PM_EXEC` carries a
//! name now, instead of PM hardcoding one — and init alternates between
//! [`EXEC_TARGETS`]: `worker` (5.5's exec-stack probe) and `hello` (the first C
//! program on minix.rs). Alternating keeps both proofs live; switching outright
//! would have retired the exec-ABI markers to make room for the C ones.
//!
//! Slice 5.4 gives it a voice. Before the fork loop it writes to **fd 1 and fd
//! 2** through VFS, which grants the buffer on to the TTY driver — the POSIX
//! write path, end to end, from an ordinary user process. init has no debug
//! channel of its own (`SYS_DIAGCTL` is a kernel call, and a user proc holds
//! none), so it reports *through the path under test*: if the path regresses the
//! lines change or vanish, and the boot markers go with them. See [`announce`].
//!
//! Slice 5.5 gives it one more thing to say. `SYS_EXEC` now hands a new image a
//! SysV initial stack, and `worker` checks that frame against its own `sp` and
//! encodes the verdict as its exit status — so init, which reaps it anyway,
//! prints the **first** such status and nothing thereafter. See
//! [`report_exec_stack`].
//!
//! Built as a freestanding aarch64 ELF (`userland/init/user.ld`). It uses
//! `minixrs-ipc` directly — no `server-rt`/SEF, because it is a plain user program,
//! not a server. The `_start` shim and panic handler are gated to `not(test)`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_BLOCK_SIZE, CDEV_MAX_IO, FS_PATH_MAX, NR_VFS_MSGS, PM_EXEC, PM_EXEC_PATH_MAX,
    PM_EXEC_PATH_OFF, PM_FORK, PM_WAIT, VFS_BUF_OFF, VFS_CLOSE, VFS_EXEC_PATH_OFF, VFS_EXEC_STAGE,
    VFS_FD_OFF, VFS_LEN_OFF, VFS_OPEN, VFS_PATH_LEN_OFF, VFS_PATH_OFF, VFS_READ, VFS_RQ_BASE,
    VFS_WRITE,
};
use minixrs_kernel_shared::com::{PM_PROC_NR, VFS_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{
    EBADF, EFAULT, EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOEXEC, ENOSYS, EPERM, OK,
};
use minixrs_kernel_shared::execstack::EXEC_STACK_PROBE_PASS;
use minixrs_kernel_shared::message::USER_VA_TOP;
use minixrs_kernel_shared::rootfs::{
    ROOTFS_HELLO_PATH, ROOTFS_MOTD, ROOTFS_MOTD_PATH, ROOTFS_SCRATCH_LEN, ROOTFS_SCRATCH_PATH,
    ROOTFS_SCRATCH_PERIOD, rootfs_scratch_byte,
};
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

/// What `init` execs, in rotation — one per fork cycle.
///
/// **`worker` must stay first.** It is slice 5.5's exec-stack probe, and the
/// verdict is keyed on the *first* child's pid ([`main`]'s `probe_pid`), so
/// reordering this array silently retires the `exec stack ok` marker while
/// leaving every other marker in place.
///
/// The two entries are now *different forms*, not just different programs.
/// `worker` is a boot-image module name — slice 4.7's form, and the only thing
/// keeping it live. [`ROOTFS_HELLO_PATH`] is an absolute path, so it goes through
/// slice 5.9's whole chain: PM → VFS → MFS → the ramdisk, staged, granted, and
/// read by the kernel through that grant. Since 5.9 dropped `hello` from the boot
/// archive there is no other way to reach it, which is what makes the C markers
/// *proof* of exec-from-FS rather than merely compatible with it.
///
/// Alternating rather than switching outright is still the point: retiring
/// `worker` would take the exec-ABI proof down with it. Both traces land inside
/// the `[ksys SYS_EXEC]` head carve-out (6 calls), so both are observable.
const EXEC_TARGETS: [&str; 2] = ["worker", ROOTFS_HELLO_PATH];

// Each target has to fit the `PM_EXEC` payload field, which is a whole path's
// worth since slice 5.9 — a module name that fit the old 16-byte field still
// fits. What PM caps at `EXEC_NAME_LEN` is a path's *basename*, not the path.
const _: () = {
    let mut i = 0;
    while i < EXEC_TARGETS.len() {
        assert!(!EXEC_TARGETS[i].is_empty(), "an empty target is EINVAL");
        assert!(EXEC_TARGETS[i].len() < PM_EXEC_PATH_MAX);
        i += 1;
    }
};

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
    // The read path runs after the console write path, and the order is
    // load-bearing in the usual direction: a hang inside it localizes to the
    // `fs.*` markers instead of taking 5.4's, 5.5's and 5.6's with it. Do not
    // reorder this prologue.
    fs_demo(vfs);
    // The filesystem write path runs after the read path it depends on — the
    // same rule — so a hang inside it localizes to the `fs.write` marker instead
    // of taking 5.4's, 5.5's, 5.6's and 5.8's with it. See [`write_demo`].
    write_demo(vfs);
    // The exec battery runs last in the prologue, the standing rule: a hang
    // inside it localizes to `exec.deny` instead of taking 5.4's, 5.5's, 5.6's,
    // 5.8's and 5.10a's markers with it. Every probe execs *init itself* and
    // must fail — see [`exec_denials`].
    exec_denials(pm, vfs);

    // One-shot: the exec-ABI verdict of the first child init forks (slice 5.5).
    //
    // Keyed on that child's **pid**, not on "the first reap", because the two are
    // not the same reap. PM seeds the demo stubs as init's children, and stub D
    // is SIGSEGV'd on purpose early in boot — so init's first `wait()` returns
    // that zombie, whose status has nothing to do with exec and happens to be 0.
    // Reporting it would print `exec stack ok` no matter what the frame looked
    // like. `alloc_pid` never hands out 0, so it doubles as the "not yet" value.
    let mut probe_pid = 0;
    let mut reported = false;
    let mut cycle = 0usize;

    loop {
        // Chosen *before* the fork, so both halves of the SENDREC agree on it:
        // the child is a copy of this address space taken during `SYS_FORK`, so
        // it inherits this value and needs no way to ask which turn it is. The
        // parent advances `cycle` only after a successful fork.
        let target = EXEC_TARGETS[cycle % EXEC_TARGETS.len()];

        // fork(): PM replies to both halves of this SENDREC — the child sees
        // `m_type == 0`, the parent sees the child pid (`> 0`); a negative value
        // is an errno (e.g. `EAGAIN` when the fork table is full).
        let mut m = pm_msg(PM_FORK);
        let _ = ipc_sendrec(pm, &mut m);

        match m.m_type {
            0 => {
                // Child: replace this image with `target`. PM issues `SYS_EXEC`
                // and the kernel resumes us at the new image's `_start`, so this
                // SENDREC never returns on success.
                let _ = exec_request(pm, target.as_bytes());
                // Unreachable on success; park defensively if exec ever failed.
                loop {
                    core::hint::spin_loop()
                }
            }
            n if n > 0 => {
                cycle = cycle.wrapping_add(1);
                // Parent: `n` is the new child's pid. Remember the first one —
                // that is the process whose exec-ABI verdict gets reported.
                if probe_pid == 0 {
                    probe_pid = n;
                }
                // Reap a child (blocks until one exits), then loop to fork the
                // next. `wait()` replies the reaped child's pid, so the verdict
                // is reported only when the child that ran `worker` comes back —
                // and only once, since the loop runs forever and a line per
                // cycle would bury the console.
                let mut w = pm_msg(PM_WAIT);
                let rc = ipc_sendrec(pm, &mut w);
                if !reported && rc == OK && w.m_type == probe_pid {
                    reported = true;
                    report_exec_stack(vfs, rd_i32(&w, 0));
                }
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
    //
    // `no-such` is **band-relative**, not `VFS_WRITE + 1`. It was the latter until
    // slice 5.8, at which point `VFS_WRITE + 1` became `VFS_OPEN` — a real request
    // that would have answered `EINVAL` for this payload and quietly retired the
    // marker below while every other line stayed green. One past the *band* is the
    // only spelling that stays an unknown request as the band grows, and it is the
    // form `bdev_denials` already used.
    let probes: [(&str, i32, i32, u64, i32, i32); 4] = [
        ("bad-fd", VFS_WRITE, 3, addr, len, EBADF),
        (
            "no-such",
            VFS_RQ_BASE + NR_VFS_MSGS as i32,
            STDOUT,
            addr,
            len,
            ENOSYS,
        ),
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

// ----- Slice 5.8: the read path ---------------------------------------------

/// Bytes init reads `/etc/motd` into.
///
/// A **local** in [`fs_demo`]'s frame, sized to hold the whole file with room to
/// spare so one `read()` empties it and the second is a clean EOF. init's stack is
/// one page, so this stays small deliberately — and it is what VFS grants on to
/// MFS, which is legal because init stays blocked in the SENDREC for the whole
/// copy, so the frame outlives every use of it.
const MOTD_BUF_LEN: usize = 64;

// The buffer must be able to hold the whole file, or the "second read is EOF"
// half of the proof would be measuring the buffer rather than the file.
const _: () = assert!(MOTD_BUF_LEN > ROOTFS_MOTD.len());

/// Read a file out of the root filesystem and print it — the slice-5.8 milestone.
///
/// Three things get proven, in order, and none subsumes another:
///
/// 1. **The milestone.** `/etc/motd`'s bytes reach the console: `mkfs` → MXBI blob
///    → RAM frames → BDEV → MFS → magic grant → init → VFS → TTY → serial. The
///    line is the file's own contents, so it cannot be printed by anything that
///    did not read the file.
/// 2. **`fs.read`.** Emitted only when the first read moved bytes *and the second
///    returned `0`* — which says the descriptor's position advanced (proved
///    nowhere else) and that EOF is `0` rather than an error. Self-checking: no
///    build-time constant appears in the condition, so it cannot go stale when the
///    file's contents change.
/// 3. **`fs.fd`.** open → 3, open → 4, close both, re-open → 3. The lowest-free
///    contract, and the only exercise of `VFS_CLOSE` anywhere.
///
/// Best-effort: init's job is to keep the system running, so a failure here is
/// reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn fs_demo(vfs: Endpoint) {
    let mut buf = [0u8; MOTD_BUF_LEN];

    let fd = vfs_open(vfs, ROOTFS_MOTD_PATH);
    if fd < 0 {
        return report_line(vfs, b"minix.rs init: fs.read FAIL open");
    }

    let n1 = vfs_read(vfs, fd, &mut buf);
    if n1 > 0 {
        // The milestone. Written from the buffer the filesystem filled, so this
        // line is the file rather than a copy of it kept somewhere else.
        let _ = vfs_write(vfs, STDOUT, &buf[..n1 as usize]);
    }
    // The same descriptor again: `pos` advanced past the end, so this must be a
    // clean EOF rather than the same bytes a second time.
    let n2 = vfs_read(vfs, fd, &mut buf);
    let verdict: &[u8] = if n1 > 0 && n2 == 0 {
        b"minix.rs init: fs.read ok match=1\n"
    } else {
        b"minix.rs init: fs.read FAIL\n"
    };
    let _ = vfs_write(vfs, STDOUT, verdict);
    let _ = vfs_close(vfs, fd);

    fd_demo(vfs);
    open_denials(vfs);
}

/// Prove the descriptor allocator: lowest free, and freed by `close`.
///
/// The numbers are absolute rather than relative because they are a *contract*:
/// fds 0/1/2 are pre-opened in every process, so the first `open` must be 3. An
/// allocator scanning from the wrong end would return 7 here and pass any
/// "did I get a descriptor" check.
///
/// The re-open is what makes `close` observable at all — without it a `close` that
/// did nothing would look identical.
#[cfg_attr(test, allow(dead_code))]
fn fd_demo(vfs: Endpoint) {
    let a = vfs_open(vfs, ROOTFS_MOTD_PATH);
    let b = vfs_open(vfs, ROOTFS_MOTD_PATH);
    let closed_a = vfs_close(vfs, a);
    let closed_b = vfs_close(vfs, b);
    let again = vfs_open(vfs, ROOTFS_MOTD_PATH);

    let ok = a == 3 && b == 4 && closed_a == OK && closed_b == OK && again == 3;
    let _ = vfs_close(vfs, again);
    let _ = vfs_write(
        vfs,
        STDOUT,
        if ok {
            &b"minix.rs init: fs.fd ok match=1\n"[..]
        } else {
            &b"minix.rs init: fs.fd FAIL\n"[..]
        },
    );
}

/// The `open`/`read`/`close` requests that must be refused, each valid but for
/// one thing.
///
///   - `no-such` — a path with no such file. `ENOENT`, from MFS's directory scan.
///   - `is-dir` — `/etc`, a directory. `EISDIR`, from VFS's `open::classify`;
///     delete that arm and this *succeeds*, handing back a descriptor every later
///     `read` would have to refuse.
///   - `too-long` — a path filling the FS band's inline field. `ENAMETOOLONG`,
///     refused by VFS before the wire, so a truncated path can never resolve to a
///     different file.
///   - `bad-len` — a negative path length, which unchecked would widen into a
///     ~16 EiB `u64` on the `SYS_COPY`.
///   - `bad-buf` — an unmapped path buffer ([`UNMAPPED_VA`]). `EFAULT` from the
///     kernel's page-table walk in the middle of VFS's `SYS_COPY`, relayed back
///     unflattened — the read-path twin of the write path's `bad-buf`.
///   - `read-console` — `read()` on fd 1. `ENOSYS`, not `EBADF`: the descriptor is
///     perfectly good, and `CDEV_READ` is what does not exist until Phase 6.
///   - `close-twice` — closing an already-closed descriptor. `EBADF`, which is
///     what makes the `fs.fd` re-open above mean something.
///
/// There used to be a `write-file` probe here, expecting `EROFS` from a `write()`
/// on a file descriptor. Slice 5.10a made that write succeed, so the probe went
/// from asserting a refusal to silently corrupting `/etc/motd` — see the comment
/// at its old site. [`write_demo`] replaces it with a positive proof.
///
/// Reported as a count on fd 2, so that descriptor stays exercised on the path
/// that matters.
#[cfg_attr(test, allow(dead_code))]
fn open_denials(vfs: Endpoint) {
    let mut denied = 0usize;

    for (name, path, want) in [
        ("no-such", "/no-such-file", ENOENT),
        ("is-dir", "/etc", EISDIR),
    ] {
        let rc = vfs_open(vfs, path);
        if rc == want {
            denied += 1;
        } else {
            return report_open_fail(vfs, name);
        }
    }

    // Malformed `VFS_OPEN` payloads, built directly so VFS's own checks are what
    // refuse them. The path address is valid in every case but `bad-buf`.
    let addr = ROOTFS_MOTD_PATH.as_ptr() as usize as u64;
    for (name, path, len, want) in [
        ("too-long", addr, FS_PATH_MAX as i32, ENAMETOOLONG),
        ("bad-len", addr, -1, EINVAL),
        ("bad-buf", UNMAPPED_VA, 9, EFAULT),
    ] {
        if open_request(vfs, path, len) == want {
            denied += 1;
        } else {
            return report_open_fail(vfs, name);
        }
    }

    // A console descriptor cannot be read from...
    let mut buf = [0u8; 8];
    if vfs_read(vfs, STDOUT, &mut buf) == ENOSYS {
        denied += 1;
    } else {
        return report_open_fail(vfs, "read-console");
    }

    // Slice 5.10a retired the probe that used to sit here: it wrote to a
    // descriptor on `/etc/motd` expecting `EROFS`, and once the write path became
    // real that call *succeeded*, overwriting the first bytes of the very file
    // `fs.selfcheck` exists to verify. A probe whose expectation silently inverts
    // is worse than no probe — so it is gone and the count moved, which is what
    // makes the retirement a visible diff in `qemu-boot.expected` rather than a
    // marker that quietly means something else. Writing to a file is now proved
    // positively, by [`write_demo`].
    //
    // The open stays: `close-twice` below needs a live descriptor.
    let fd = vfs_open(vfs, ROOTFS_MOTD_PATH);
    if fd < 0 {
        return report_open_fail(vfs, "setup");
    }
    if vfs_close(vfs, fd) == OK && vfs_close(vfs, fd) == EBADF {
        denied += 1;
    } else {
        return report_open_fail(vfs, "close-twice");
    }

    if denied == OPEN_DENIAL_PROBES {
        let _ = vfs_write(vfs, STDERR, b"minix.rs init: open.deny ok n=7\n");
    }
}

// ----- Slice 5.10a: the write path ------------------------------------------

/// Bytes written per `write()` call.
///
/// A whole multiple of [`ROOTFS_SCRATCH_PERIOD`], which is what lets [`SCRATCH`]
/// be a single buffer: every chunk starts at an offset congruent to 0, so the
/// same bytes are correct for all of them.
///
/// Deliberately **not** a multiple of the 4096-byte block. Every chunk after the
/// first therefore starts mid-block and crosses a block boundary, so partial-block
/// splicing and MFS's clamp-to-the-block-end short write are exercised on the boot
/// marker rather than only in unit tests.
const SCRATCH_CHUNK: usize = 16 * ROOTFS_SCRATCH_PERIOD;

const _: () = assert!(SCRATCH_CHUNK.is_multiple_of(ROOTFS_SCRATCH_PERIOD));
// ...and deliberately NOT a whole block, so every chunk after the first starts
// mid-block. `BDEV_BLOCK_SIZE`, not `CDEV_MAX_IO`: the block is what MFS clamps
// to, and the two constants differ by 16x.
const _: () = assert!(!SCRATCH_CHUNK.is_multiple_of(BDEV_BLOCK_SIZE));

/// One chunk's worth of the scratch pattern, generated at compile time.
///
/// `.rodata`, not a mutable static: init writes these bytes and never modifies
/// them, so the crate keeps zero `unsafe`. A stack buffer is out of the question —
/// init's stack is one page, and a 4 KiB local would fault into VM's SIGSEGV arm,
/// which prints nothing `tests/qemu-boot.forbidden` catches.
static SCRATCH: [u8; SCRATCH_CHUNK] = scratch_chunk();

const fn scratch_chunk() -> [u8; SCRATCH_CHUNK] {
    let mut b = [0u8; SCRATCH_CHUNK];
    let mut i = 0;
    while i < SCRATCH_CHUNK {
        b[i] = rootfs_scratch_byte(i);
        i += 1;
    }
    b
}

/// Bytes read back per verification window.
const SCRATCH_WINDOW: usize = 512;

/// First byte the single-indirect region covers. Mirrors `minixrs-mfs`'s
/// `NR_DIRECT_ZONES * MFS_BLOCK_SIZE`, spelled out locally because init does not
/// depend on that crate — it is a plain user program, `minixrs-ipc` and
/// `kernel-shared` only.
const SEAM: usize = 7 * BDEV_BLOCK_SIZE;

/// Where the read-back windows start.
///
/// All three are multiples of [`SCRATCH_WINDOW`] **and** lie inside a single 4 KiB
/// block, so each window is one clamped transfer and the arithmetic in
/// [`verify_window`] stays trivial. The middle one is the first byte the
/// *indirect* block covers — the one zone the allocator had to create an indirect
/// block for, and the only place that arm can hide. There is no separate count
/// constant: `SCRATCH_WINDOWS_AT.len()` is the count, so the marker's `v=` cannot
/// drift from the loop.
const SCRATCH_WINDOWS_AT: [usize; 3] = [0, SEAM, ROOTFS_SCRATCH_LEN - SCRATCH_WINDOW];

const _: () = assert!(SEAM < ROOTFS_SCRATCH_LEN);
const _: () = assert!(SEAM.is_multiple_of(SCRATCH_WINDOW));
const _: () = assert!((ROOTFS_SCRATCH_LEN - SCRATCH_WINDOW).is_multiple_of(SCRATCH_WINDOW));
// Strictly ascending and non-overlapping: [`verify_window`] reaches each window by
// winding the descriptor *forward*, so an out-of-order or overlapping entry would
// make it wind past its own target and verify the wrong bytes.
const _: () = assert!(SCRATCH_WINDOWS_AT[0] + SCRATCH_WINDOW <= SCRATCH_WINDOWS_AT[1]);
const _: () = assert!(SCRATCH_WINDOWS_AT[1] + SCRATCH_WINDOW <= SCRATCH_WINDOWS_AT[2]);
// The marker's `n=` and `v=` are literals — init has no way to format an integer —
// so these are what make a constant moving underneath them loud at compile time
// rather than a line that quietly means something else. The `OPEN_DENIAL_PROBES`
// reasoning, applied to a marker that carries two numbers.
const _: () = assert!(ROOTFS_SCRATCH_LEN == 32768);
const _: () = assert!(SCRATCH_WINDOWS_AT.len() == 3);

/// Exercise the slice-5.10a write path, and report the result over the console.
///
/// Writes [`ROOTFS_SCRATCH_LEN`] bytes to [`ROOTFS_SCRATCH_PATH`], which ships
/// empty, so every zone the file ends up with was allocated at runtime. 32 KiB is
/// eight blocks: seven direct zones and one indirect, so the file crosses the seam
/// and the allocation of the *indirect block itself* is on this marker. That is the
/// same reason `/etc/pattern` has the length it does, one layer up.
///
/// Then it closes, re-opens, and reads back three windows. The re-open matters: it
/// forces a fresh `FS_LOOKUP`, so a size that never reached the inode shows up as a
/// short read rather than being papered over by a descriptor that remembered it.
/// Nothing caches a file size anywhere along this path, which is exactly what makes
/// that test meaningful.
///
/// Three windows rather than all 32 KiB: the returned write count already proves
/// every byte was accepted, the seam is the only place the indirect arm can hide,
/// and ~60 more round trips on a boot this slice already lengthens buy nothing.
///
/// Reported through fd 1 — the path under test — which is the standing rule here:
/// init has no `SYS_DIAGCTL` (it is user-grade), so its proof and its report ride
/// the same machinery, and a broken write cannot print a clean line about itself.
///
/// Best-effort: init's job is to keep the system running, so a failure here is
/// reported and stepped over, never fatal.
#[cfg_attr(test, allow(dead_code))]
fn write_demo(vfs: Endpoint) {
    let fd = vfs_open(vfs, ROOTFS_SCRATCH_PATH);
    if fd < 0 {
        return report_line(vfs, b"minix.rs init: fs.write FAIL open");
    }

    let mut off = 0usize;
    while off < ROOTFS_SCRATCH_LEN {
        let want = SCRATCH_CHUNK.min(ROOTFS_SCRATCH_LEN - off);
        // VFS absorbs short writes (slice 5.4), so anything but the full count is
        // a failure here rather than something to retry.
        let n = vfs_write(vfs, fd, &SCRATCH[..want]);
        if n != want as i32 {
            let _ = vfs_close(vfs, fd);
            return report_line(vfs, b"minix.rs init: fs.write FAIL short");
        }
        let Some(next) = off.checked_add(want) else {
            let _ = vfs_close(vfs, fd);
            return report_line(vfs, b"minix.rs init: fs.write FAIL short");
        };
        off = next;
    }
    // The total, checked rather than merely accumulated: the loop's own invariant
    // already implies it, and stating it anyway is what keeps `n=32768` in the
    // marker a claim about bytes on the filesystem instead of about this loop.
    if off != ROOTFS_SCRATCH_LEN {
        let _ = vfs_close(vfs, fd);
        return report_line(vfs, b"minix.rs init: fs.write FAIL total");
    }
    if vfs_close(vfs, fd) != OK {
        return report_line(vfs, b"minix.rs init: fs.write FAIL close");
    }

    // Re-open: a fresh descriptor and a fresh lookup, so the size has to have
    // reached the inode for these reads to return anything.
    let fd = vfs_open(vfs, ROOTFS_SCRATCH_PATH);
    if fd < 0 {
        return report_line(vfs, b"minix.rs init: fs.write FAIL reopen");
    }
    let mut pos = 0usize;
    let mut verified = 0usize;
    for start in SCRATCH_WINDOWS_AT {
        if !verify_window(vfs, fd, start, &mut pos) {
            let _ = vfs_close(vfs, fd);
            return report_line(vfs, b"minix.rs init: fs.write FAIL verify");
        }
        verified += 1;
    }
    let _ = vfs_close(vfs, fd);

    if verified == SCRATCH_WINDOWS_AT.len() {
        let _ = vfs_write(vfs, STDOUT, b"minix.rs init: fs.write ok n=32768 v=3\n");
    }
}

/// Read [`SCRATCH_WINDOW`] bytes at `start` and check them against the generator.
///
/// `pos` is the descriptor's position, threaded through because there is no
/// `lseek`: the window is reached by reading forward and discarding, which is also
/// a second exercise of the read path across the seam. The windows are visited in
/// ascending order, so winding forward always suffices.
///
/// Both loops tolerate a short read, because MFS clamps every transfer to the end
/// of a block and a read that spans a boundary legitimately returns less than it
/// was asked for. `n <= 0` is a failure, not a retry: `0` is EOF, which at these
/// offsets means the file is shorter than it should be — which is precisely what a
/// size that never reached the inode looks like. A peer reporting *more* than it
/// was asked for is a failure too, which is also what keeps the arithmetic below
/// unable to run off the buffer.
#[cfg_attr(test, allow(dead_code))]
fn verify_window(vfs: Endpoint, fd: i32, start: usize, pos: &mut usize) -> bool {
    let mut buf = [0u8; SCRATCH_WINDOW];

    while *pos < start {
        let want = SCRATCH_WINDOW.min(start - *pos);
        let n = vfs_read(vfs, fd, &mut buf[..want]);
        if n <= 0 || n as usize > want {
            return false;
        }
        match pos.checked_add(n as usize) {
            Some(next) => *pos = next,
            None => return false,
        }
    }

    let mut got = 0usize;
    while got < SCRATCH_WINDOW {
        let room = SCRATCH_WINDOW - got;
        let n = vfs_read(vfs, fd, &mut buf[got..]);
        if n <= 0 || n as usize > room {
            return false;
        }
        match got.checked_add(n as usize) {
            Some(next) => got = next,
            None => return false,
        }
    }
    match pos.checked_add(SCRATCH_WINDOW) {
        Some(next) => *pos = next,
        None => return false,
    }

    let mut i = 0usize;
    while i < SCRATCH_WINDOW {
        let Some(at) = start.checked_add(i) else {
            return false;
        };
        if buf[i] != rootfs_scratch_byte(at) {
            return false;
        }
        i += 1;
    }
    true
}

// ----- Slice 5.9: the exec-from-FS denial battery ---------------------------

/// The `PM_EXEC` requests that must be refused, each valid but for one thing —
/// **and every one of them is init exec'ing *itself***.
///
/// That is safe precisely because of the invariant the whole design rests on: a
/// failed exec leaves the caller on its old image, untouched, because nothing is
/// committed until `SYS_EXEC` reaches its point of no return. So this battery is
/// not merely a set of probes, it **is** the rollback proof — if any one of them
/// half-succeeded, init would be replaced mid-loop and every marker after this
/// point would vanish along with the system.
///
///   - `no-such` — an absolute path with no such file. `ENOENT`, from MFS's
///     directory scan, relayed by VFS and PM unflattened.
///   - `is-dir` — `/etc`, a directory. `EISDIR` from VFS's `open::classify`,
///     which the stager reuses; delete that arm and VFS would stage a directory's
///     raw blocks and hand them to the loader as an ELF.
///   - `relative` — `etc/motd`, no leading `/`. **`ENOENT`, not `EINVAL`**: PM
///     reads it as a *module* name (that is what the discriminator means), and no
///     such module exists, so the kernel answers `ENOENT`. Expecting that rather
///     than a validator's `EINVAL` is the assertion — the distinction is the
///     discriminator working, not failing.
///   - `too-long` — a payload field with no NUL anywhere. `ENAMETOOLONG`, never a
///     truncated path resolving to some other program.
///   - `empty` — an all-NUL field. `EINVAL`; it names nothing.
///   - `no-module` — a bare name that is not in the boot archive. `ENOENT`, from
///     `module_by_name`, and the arm that proves the name form still exists.
///   - **`not-elf`** — `/etc/motd`. It stages perfectly: the path resolves, VFS
///     reads all 31 bytes, PM grants them to the kernel, and the *loader* refuses
///     them. `ENOEXEC`. The strongest probe here by some distance — it drives the
///     entire staging chain, the loader's rejection, and the rollback in one
///     message, and it is the only thing that distinguishes `ENOEXEC` from the
///     `ENOMEM` every loader failure used to fold into.
///   - `stage-not-pm` — `VFS_EXEC_STAGE` sent by init *directly*. `EPERM`, from
///     VFS's not-PM guard, and the only exercise of it anywhere.
///
/// Reported as a count on fd 1, through the write path under test, since init has
/// no debug channel of its own.
#[cfg_attr(test, allow(dead_code))]
fn exec_denials(pm: Endpoint, vfs: Endpoint) {
    let mut denied = 0usize;

    for (name, target, want) in [
        ("no-such", "/no-such-file", ENOENT),
        ("is-dir", "/etc", EISDIR),
        // A relative path is a *module* name by the leading-`/` rule, and no such
        // module is packed — so the honest answer is `ENOENT` from the archive
        // lookup, not `EINVAL`. Expecting the right one is the assertion.
        ("relative", "etc/motd", ENOENT),
        ("no-module", "nosuchmod", ENOENT),
        ("not-elf", ROOTFS_MOTD_PATH, ENOEXEC),
    ] {
        if exec_request(pm, target.as_bytes()) == want {
            denied += 1;
        } else {
            return report_exec_fail(vfs, name);
        }
    }

    // A field with no NUL anywhere, and one that is entirely NUL. Built as raw
    // byte fields so PM's own checks are what refuse them.
    if exec_request(pm, &[b'a'; PM_EXEC_PATH_MAX]) == ENAMETOOLONG {
        denied += 1;
    } else {
        return report_exec_fail(vfs, "too-long");
    }
    if exec_request(pm, b"") == EINVAL {
        denied += 1;
    } else {
        return report_exec_fail(vfs, "empty");
    }

    // VFS's not-PM guard: init asking VFS to stage an image on its own account.
    // Reaches VFS at all only because init's `ipc_to` includes it (the shared
    // USER privilege), which is exactly the situation the guard exists for.
    let mut m = Message {
        m_source: 0,
        m_type: VFS_EXEC_STAGE,
        payload: [0u8; 96],
    };
    let p = ROOTFS_HELLO_PATH.as_bytes();
    m.payload[VFS_EXEC_PATH_OFF..VFS_EXEC_PATH_OFF + p.len()].copy_from_slice(p);
    let rc = if ipc_sendrec(vfs, &mut m) == OK {
        m.m_type
    } else {
        OK
    };
    if rc == EPERM {
        denied += 1;
    } else {
        return report_exec_fail(vfs, "stage-not-pm");
    }

    if denied == EXEC_DENIAL_PROBES {
        let _ = vfs_write(vfs, STDOUT, b"minix.rs init: exec.deny ok n=8\n");
    }
}

/// Probes [`exec_denials`] runs, named rather than counted inline — the
/// `OPEN_DENIAL_PROBES` reason: adding one without counting it must make the
/// marker *vanish*, which CI catches, rather than silently under-report, which it
/// cannot. The `n=` in the marker is this number spelled out, because init has no
/// way to format an integer.
const EXEC_DENIAL_PROBES: usize = 8;

/// Send one `PM_EXEC` with `target` written verbatim into the payload field, and
/// return the reply `m_type`.
///
/// Verbatim rather than through a checked marshaller, because two of the probes
/// are field-level malformations (no NUL, all NUL) that a marshaller would refuse
/// before the wire — and a probe its own client rejects proves nothing about PM.
/// The `min` is the only guard, and it cannot fire for anything this file sends.
///
/// **On success this never returns**, since the kernel resumes the caller at the
/// new image. Every call here is expected to fail; that is the point.
#[cfg_attr(test, allow(dead_code))]
fn exec_request(pm: Endpoint, target: &[u8]) -> i32 {
    let mut m = pm_msg(PM_EXEC);
    let n = target.len().min(PM_EXEC_PATH_MAX);
    m.payload[PM_EXEC_PATH_OFF..PM_EXEC_PATH_OFF + n].copy_from_slice(&target[..n]);
    let trap_rc = ipc_sendrec(pm, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// Report a failing `exec` probe by name, on fd 2.
#[cfg_attr(test, allow(dead_code))]
fn report_exec_fail(vfs: Endpoint, name: &str) {
    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, b"minix.rs init: exec.deny FAIL ");
    n = append(&mut line, n, name.as_bytes());
    n = append(&mut line, n, b"\n");
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}

/// Probes [`open_denials`] runs, named rather than counted inline — VFS's
/// `FS_DENIAL_PROBES` reason: adding one without counting it must make the marker
/// *vanish*, which CI catches, rather than silently under-report, which it cannot.
///
/// The `n=` in the marker line is this number spelled out as a literal, because
/// init has no formatting machinery and the line is a `grep -aF` marker. The two
/// move together; the const below is what makes forgetting the second half loud.
const OPEN_DENIAL_PROBES: usize = 7;

/// Report a failing `open` probe by name, on fd 2.
#[cfg_attr(test, allow(dead_code))]
fn report_open_fail(vfs: Endpoint, name: &str) {
    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, b"minix.rs init: open.deny FAIL ");
    n = append(&mut line, n, name.as_bytes());
    n = append(&mut line, n, b"\n");
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}

/// Print one fixed line on fd 2, with a trailing newline.
#[cfg_attr(test, allow(dead_code))]
fn report_line(vfs: Endpoint, text: &[u8]) {
    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, text);
    n = append(&mut line, n, b"\n");
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}

/// `open(path)` through VFS. Returns the new descriptor, or a negative errno.
#[cfg_attr(test, allow(dead_code))]
fn vfs_open(vfs: Endpoint, path: &str) -> i32 {
    open_request(vfs, path.as_ptr() as usize as u64, path.len() as i32)
}

/// Send one `VFS_OPEN` and return the reply `m_type`.
///
/// The path travels as a **raw address in init's own address space**: init is an
/// ordinary user process with no grant table, so VFS is what reads the bytes out —
/// with `SYS_COPY`, taking the source from the kernel-stamped `m_source` and never
/// from this payload, which is why there is no field here for it.
///
/// The address and length are parameters rather than a `&str` so the malformed
/// probes can vary each independently.
#[cfg_attr(test, allow(dead_code))]
fn open_request(vfs: Endpoint, path: u64, len: i32) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: VFS_OPEN,
        payload: [0u8; 96],
    };
    m.payload[VFS_PATH_OFF..VFS_PATH_OFF + 8].copy_from_slice(&path.to_ne_bytes());
    m.payload[VFS_PATH_LEN_OFF..VFS_PATH_LEN_OFF + 4].copy_from_slice(&len.to_ne_bytes());

    let trap_rc = ipc_sendrec(vfs, &mut m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}

/// `read(fd, buf, buf.len())` through VFS. Returns the byte count (`0` is EOF), or
/// a negative errno.
#[cfg_attr(test, allow(dead_code))]
fn vfs_read(vfs: Endpoint, fd: i32, buf: &mut [u8]) -> i32 {
    vfs_request(
        vfs,
        VFS_READ,
        fd,
        buf.as_mut_ptr() as usize as u64,
        buf.len() as i32,
    )
}

/// `close(fd)` through VFS. Returns `OK` or `EBADF`.
#[cfg_attr(test, allow(dead_code))]
fn vfs_close(vfs: Endpoint, fd: i32) -> i32 {
    vfs_request(vfs, VFS_CLOSE, fd, 0, 0)
}

/// Report the first failing probe by name, on fd 2.
///
/// Hand-assembled into a stack buffer: init has no formatting runtime (no
/// `server-rt`, so no `diag_fmt`), and the buffer's address is what VFS grants on
/// to the driver — legal because init stays blocked in the SENDREC for the whole
/// copy, so the frame outlives every use of it.
#[cfg_attr(test, allow(dead_code))]
fn report_fail(vfs: Endpoint, name: &str) {
    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, b"minix.rs init: vfs.deny FAIL ");
    n = append(&mut line, n, name.as_bytes());
    n = append(&mut line, n, b"\n");
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}

/// Report the exec-ABI verdict the first reaped child carried out (slice 5.5).
///
/// `worker` reads its own `sp` and checks the SysV initial stack `SYS_EXEC` built
/// — `argc`/`argv`/`envp`/auxv — then exits with `0` or the number of the first
/// failing check. That verdict has to leave the process somehow, and a console
/// line per fork cycle would flood the log, so it rides out as the exit status
/// and init prints it once. The kernel's own `[exec]` trace says only that the
/// kernel *thinks* it wrote a frame; this line is the EL0 half of the proof.
///
/// `status` is the `W_EXITCODE` PM encodes: the exit code sits in bits 8..16.
///
/// Success is a **positive sentinel**, not 0, because 0 is what a child that
/// never got as far as reporting leaves behind: PM's kill path marks a signalled
/// process a zombie without setting `exit_status`, so a worker that faulted on a
/// malformed frame — which VM turns into a SIGSEGV, not the unhandled EL0 abort
/// the forbidden list watches for — would otherwise be reaped as a pass. That
/// case gets its own `no-verdict` spelling so the log names it.
#[cfg_attr(test, allow(dead_code))]
fn report_exec_stack(vfs: Endpoint, status: i32) {
    let code = (status >> 8) & 0xff;
    if code == EXEC_STACK_PROBE_PASS {
        let _ = vfs_write(vfs, STDOUT, b"minix.rs init: exec stack ok\n");
        return;
    }

    let mut line = [0u8; 64];
    let mut n = append(&mut line, 0, b"minix.rs init: exec stack FAIL ");
    if code == 0 {
        n = append(&mut line, n, b"no-verdict\n");
    } else {
        n = append(&mut line, n, b"code=");
        let mut digits = [0u8; 3];
        let mut d = 0;
        let mut v = code as u32;
        loop {
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
            d += 1;
            if v == 0 || d == digits.len() {
                break;
            }
        }
        digits[..d].reverse();
        n = append(&mut line, n, &digits[..d]);
        n = append(&mut line, n, b"\n");
    }
    // fd 2, like init's other failure report and worker's own — failures go to
    // stderr, and it keeps that descriptor exercised on the path that matters.
    let _ = vfs_write(vfs, STDERR, &line[..n]);
}

/// Append `src` to `line[..n]`, dropping any overflow, and return the new length.
///
/// The whole of init's string formatting: no allocator, no `core::fmt`, and a
/// truncating append rather than a fallible one, because a report that does not
/// quite fit is still worth printing.
#[cfg_attr(test, allow(dead_code))]
fn append(line: &mut [u8], mut n: usize, src: &[u8]) -> usize {
    for &b in src {
        if n < line.len() {
            line[n] = b;
            n += 1;
        }
    }
    n
}

/// Read a native-endian `i32` out of a reply payload.
#[cfg_attr(test, allow(dead_code))]
fn rd_i32(m: &Message, off: usize) -> i32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&m.payload[off..off + 4]);
    i32::from_ne_bytes(b)
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

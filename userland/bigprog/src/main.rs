// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `bigprog` — the user VA map's regression fixture.
//!
//! A program whose only distinguishing feature is that it is too big for the
//! VA map Phase 5 shipped. Its `.bss` is 2 MiB, so the highest `PT_LOAD`'s
//! `p_vaddr + p_memsz` runs from the `0x0010_0000` load base to roughly
//! `0x0030_0000` — straight through `0x0020_0000`, where the initial stack page
//! used to be. Under the old map `load_exec_image` fails to map the stack with
//! `AlreadyMapped`, and the exec answers `ENOEXEC`.
//!
//! ## Why `.bss` and not `.rodata`
//!
//! The loader maps `ceil(p_memsz / PAGE_SIZE)` pages either way, so the VA
//! collision this fixture exists to prove is byte-identical. But `.bss` costs
//! `p_filesz = 0`, so the packed image stays kilobytes: the boot archive, the
//! 1 MiB rootfs and the boot budget are all untouched.
//!
//! The tradeoff, stated rather than discovered: this proves the *VA* ceiling,
//! not multi-MiB file I/O through MFS. That path is already proven by slice
//! 5.10a's 32 KiB write, and sizing a filesystem for a multi-MiB program is the
//! disk-root question Phase 6 slice 6.4 owns.
//!
//! The program touches its first and last `.bss` page — proving the pages are
//! really mapped, not merely promised by a program header — then reports through
//! fd 2 and exits.
//!
//! Like [`worker`](../../worker/src/main.rs) it is packed into the boot-image
//! archive with `com::EXEC_ONLY_PROC_NR`, so it is resolvable by name for
//! `SYS_EXEC` and never loaded at boot. It uses `minixrs-ipc` directly — no
//! `server-rt`/SEF, because it is a plain user program, not a server.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{PM_EXIT, VFS_BUF_OFF, VFS_FD_OFF, VFS_LEN_OFF, VFS_WRITE};
use minixrs_kernel_shared::com::{PM_PROC_NR, VFS_PROC_NR, boot_endpoint};

/// 2 MiB — enough that the image's last `PT_LOAD` ends past `0x0030_0000`,
/// which is well clear of the `0x0020_0000` the old stack page occupied. Sized
/// for an unambiguous verdict, not for the minimum that would collide.
const BIG_BYTES: usize = 2 * 1024 * 1024;

/// The `.bss` array that makes this image large. `static mut` rather than a
/// local: it must land in a `PT_LOAD`'s `p_memsz`, which is the whole point —
/// a stack array would prove nothing about the image's VA span.
static mut BIG: [u8; BIG_BYTES] = [0u8; BIG_BYTES];

/// Standard error, pre-opened by VFS to the console.
const STDERR: i32 = 2;

/// ELF entry point. Unlike `worker`'s this one is an ordinary function: bigprog
/// makes no claim about the initial stack frame — that is 5.5's probe, and it
/// still runs, first in the rotation — so nothing here needs `sp` untouched.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // Touch the first and last page. `.bss` is satisfied by the loader's zeroed
    // frames, so a read proving them mapped is as strong as a write and cannot
    // be optimised into a store the compiler might sink.
    //
    // SAFETY: sole thread of this process; `BIG` is this program's own `.bss`,
    // and both indices are in bounds by construction.
    let (first, last) = unsafe {
        let p: *const u8 = (&raw const BIG).cast();
        let first = core::ptr::read_volatile(p);
        let last = core::ptr::read_volatile(p.add(BIG_BYTES - 1));
        (first, last)
    };

    if first == 0 && last == 0 {
        write_str(b"bigprog ok: bss pages mapped\n");
    } else {
        write_str(b"bigprog FAIL: bss not zeroed\n");
    }

    exit(0)
}

/// `write(STDERR, buf, buf.len())` through VFS — the slice-5.4 path, marshalled
/// exactly as [`worker`'s `vfs_write`](../../worker/src/main.rs) does.
///
/// The buffer travels as a raw address in this process's own address space: a
/// user process holds no grant table, and VFS is what turns the address into a
/// magic grant naming this process (taken from the kernel-stamped `m_source`,
/// never from the payload). Best-effort: the console line *is* the verdict, so
/// a failed send is indistinguishable from a failed exec, which is the marker
/// file's job to catch.
#[cfg_attr(test, allow(dead_code))]
fn write_str(buf: &[u8]) {
    let vfs = boot_endpoint(VFS_PROC_NR);
    let mut m = Message {
        m_source: 0,
        m_type: VFS_WRITE,
        payload: [0u8; 96],
    };
    m.payload[VFS_FD_OFF..VFS_FD_OFF + 4].copy_from_slice(&STDERR.to_ne_bytes());
    m.payload[VFS_LEN_OFF..VFS_LEN_OFF + 4].copy_from_slice(&(buf.len() as i32).to_ne_bytes());
    m.payload[VFS_BUF_OFF..VFS_BUF_OFF + 8]
        .copy_from_slice(&(buf.as_ptr() as usize as u64).to_ne_bytes());
    let _ = ipc_sendrec(vfs, &mut m);
}

/// `exit(status)` through PM — `worker`'s spelling. PM tears the caller down via
/// `SYS_EXIT` rather than replying, so this SENDREC never returns; the loop is
/// there for the type, not for the schedule.
#[cfg_attr(test, allow(dead_code))]
fn exit(status: i32) -> ! {
    let pm = boot_endpoint(PM_PROC_NR);
    let mut msg = Message {
        m_source: 0,
        m_type: PM_EXIT,
        payload: [0u8; 96],
    };
    msg.payload[0..4].copy_from_slice(&status.to_ne_bytes());
    let _ = ipc_sendrec(pm, &mut msg);

    // Unreachable: PM never replies to a dead child.
    loop {
        core::hint::spin_loop()
    }
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}

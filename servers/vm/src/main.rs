//! MINIX 4 VM (virtual memory) server.
//!
//! The first *real* user-space process (introduced in slice 3.4a as a build
//! milestone). Built as a freestanding aarch64 ELF (see `servers/vm/user.ld`),
//! embedded into the kernel image by `kernel/build.rs`, and loaded into its
//! own per-process AddrSpace at boot by `arch::aarch64::userland::vm_bootstrap`.
//!
//! Slice 3.4b makes it functional: a `RECEIVE(ANY)` loop that resolves page
//! faults. When an EL0 process faults on an unmapped page, the kernel records
//! the fault, blocks the faulter on `RTS_PAGEFAULT`, and sends VM a
//! `VM_PAGEFAULT` message (`m_source` = faulter, payload = fault addr/flags).
//! VM maps a fresh page into the faulter via `SYS_VMCTL(VMCTL_PT_MAP)` and
//! unblocks it via `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`. No `alloc` crate — the
//! kernel owns the frame allocator; VM only drives policy.
//!
//! Region tracking, `VM_BRK`, and `VM_MMAP` arrive in slices 3.5/3.6; for now
//! every fault is resolved with a fresh writable page.

#![no_std]
#![no_main]

use minix4_ipc::{ipc_receive, ipc_sendrec};
use minix4_kernel_shared::Message;
use minix4_kernel_shared::callnr::{
    SYS_VMCTL, VM_PAGEFAULT, VMCTL_CLEAR_PAGEFAULT, VMCTL_PROT_WRITE, VMCTL_PT_MAP,
};
use minix4_kernel_shared::com::{SYSTEM, boot_endpoint};
use minix4_kernel_shared::endpoint::{ANY, Endpoint};

/// aarch64 4 KiB page size — VM only needs to page-align fault addresses.
const PAGE_SIZE: u64 = 4096;

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start`
/// can dive straight into Rust without setting up a stack itself.
#[unsafe(no_mangle)]
// `.text._start` is an ELF section name; gate it to the bare-metal target so
// `cargo check --workspace` on a Mach-O host (which rejects the specifier)
// still type-checks this crate. `ENTRY(_start)` in user.ld roots the symbol
// either way, so ordering it first in `.text` is a nicety, not a requirement.
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    let system = boot_endpoint(SYSTEM);
    let mut msg = Message { m_source: 0, m_type: 0, payload: [0u8; 96] };
    loop {
        if ipc_receive(ANY, &mut msg) != 0 {
            continue;
        }
        if msg.m_type == VM_PAGEFAULT {
            handle_pagefault(system, msg.m_source, rd_u64(&msg, 0));
        }
        // Other request types (VM_BRK / VM_MMAP) arrive in 3.5/3.6.
    }
}

/// Resolve a page fault for `faulting_e` at `far`: map a fresh writable page,
/// then clear the kernel-recorded fault so the faulter retries and proceeds.
fn handle_pagefault(system: Endpoint, faulting_e: Endpoint, far: u64) {
    let page = far & !(PAGE_SIZE - 1);

    // SYS_VMCTL(VMCTL_PT_MAP, target=faulting_e, vaddr=page, prot=WRITE).
    // The kernel allocates + maps the frame; we ignore the returned PA (region
    // tracking that would record it lands in slice 3.5).
    let mut m = Message { m_source: 0, m_type: SYS_VMCTL, payload: [0u8; 96] };
    wr_i32(&mut m, 0, VMCTL_PT_MAP);
    wr_i32(&mut m, 4, faulting_e);
    wr_u64(&mut m, 8, page);
    wr_i32(&mut m, 16, VMCTL_PROT_WRITE);
    let _ = ipc_sendrec(system, &mut m);

    // SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT, target=faulting_e) — unblock the faulter.
    let mut m = Message { m_source: 0, m_type: SYS_VMCTL, payload: [0u8; 96] };
    wr_i32(&mut m, 0, VMCTL_CLEAR_PAGEFAULT);
    wr_i32(&mut m, 4, faulting_e);
    let _ = ipc_sendrec(system, &mut m);
}

// Native-endian payload accessors, mirroring the kernel's `do_vmctl` reads.
fn rd_u64(m: &Message, off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&m.payload[off..off + 8]);
    u64::from_ne_bytes(b)
}

fn wr_i32(m: &mut Message, off: usize, v: i32) {
    m.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

fn wr_u64(m: &mut Message, off: usize, v: u64) {
    m.payload[off..off + 8].copy_from_slice(&v.to_ne_bytes());
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

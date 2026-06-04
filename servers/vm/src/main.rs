//! MINIX 4 VM (virtual memory) server.
//!
//! Slice 3.4a: the first *real* user-space process. Built as a freestanding
//! aarch64 ELF (see `servers/vm/user.ld`), embedded into the kernel image by
//! `kernel/build.rs`, and loaded into its own per-process AddrSpace at boot by
//! `arch::aarch64::userland::vm_bootstrap`.
//!
//! For 3.4a the body is intentionally minimal: a `RECEIVE(ANY)` loop. Nothing
//! sends to VM yet, so the very first receive blocks the server (it leaves the
//! run queue and therefore can't starve the higher-numbered-band EL0 stubs),
//! while still proving the whole pipeline works — the ELF built, embedded,
//! loaded, mapped, and *executed* at EL0 (the RECEIVE shows up as VM's SVC in
//! the kernel's IPC head trace). Slice 3.4b turns this into the real
//! page-fault resolution loop (handle `VM_PAGEFAULT` → `SYS_VMCTL`).

#![no_std]
#![no_main]

use minix4_ipc::ipc_receive;
use minix4_kernel_shared::Message;
use minix4_kernel_shared::endpoint::ANY;

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
    let mut msg = Message { m_source: 0, m_type: 0, payload: [0u8; 96] };
    loop {
        // Blocks forever in 3.4a (no senders yet). 3.4b dispatches on
        // `msg.m_type == VM_PAGEFAULT` here.
        let _ = ipc_receive(ANY, &mut msg);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

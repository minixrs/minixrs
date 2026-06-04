// MINIX 4 Microkernel
#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

// The kernel crate is only meaningful on bare-metal targets (`target_os =
// "none"`). When `cargo check --workspace` runs against the host target
// (macos / linux), we collapse to a no-op `main` so the workspace stays
// checkable; the ELF-only `link_section` attributes, the `_start` entry
// path, and the panic handler all rely on the bare-metal target.

#[cfg(target_os = "none")]
mod arch;
#[cfg(target_os = "none")]
mod boot_image;
#[cfg(target_os = "none")]
mod clock;
#[cfg(target_os = "none")]
mod ipc;
#[cfg(target_os = "none")]
mod mm;
#[cfg(target_os = "none")]
mod panic;
#[cfg(target_os = "none")]
mod proc;
#[cfg(target_os = "none")]
mod system;
#[cfg(target_os = "none")]
mod uart;

#[cfg(target_os = "none")]
use core::fmt::Write;

/// Scheduler tick rate (Hz). 100 Hz → 10 ms ticks, matching the classic
/// MINIX 3 cadence. Combined with the per-stub `quantum_ms = 5`, each task
/// gets ~50 ms of CPU before preemption.
#[cfg(target_os = "none")]
const TICK_HZ: u64 = 100;

#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    // Resolve the UART MMIO virtual address before any output. Limine maps
    // PL011 into the HHDM (under base revision 2, the [0, 4 GiB) blanket
    // map covers device memory); we fall back to the physical address if
    // the bootloader didn't populate the response.
    let hhdm = arch::limine_hhdm_offset().unwrap_or(0);
    arch::set_uart_base(hhdm as usize + arch::PL011_PHYS_BASE);

    arch::init();

    let mut con = uart::Uart::new();
    let _ = writeln!(con, "MINIX 4 booting on aarch64");

    if arch::limine_base_revision_supported() {
        let _ = writeln!(con, "HHDM offset: {hhdm:#018x}");
    } else {
        let _ = writeln!(
            con,
            "Limine base revision unsupported (loader is too old)"
        );
    }

    proc::init();
    let _ = writeln!(con);
    let _ = proc::dump_tables(&mut con);

    // Slice 3.1a: capture Limine's HHDM offset and populate the physical
    // frame allocator from the memmap. Both are prerequisites for the
    // address-space API (`arch::aarch64::addrspace::AddrSpace`) — which
    // slice 3.1b's `userland_bootstrap` now drives in earnest, one
    // AddrSpace per EL0 stub.
    // SAFETY: single-threaded boot; this is the only writer of HHDM_OFFSET
    // and the only initializer of the allocator.
    unsafe {
        mm::set_hhdm_offset(hhdm);
        mm::init_from_limine_memmap();
    }

    // Slice 2.4: bring up the interrupt controller and timer, populate two
    // EL0 stub tasks, enqueue them, and hand control to the scheduler.
    // SAFETY: single-threaded boot; DAIF still masked from Limine handoff.
    unsafe { arch::aarch64::gic::init() };
    // SAFETY: same; gic is configured first so the timer's PPI 27 is routable.
    unsafe {
        arch::aarch64::gic::enable_ppi(arch::aarch64::timer::INTID_VIRT_TIMER, 0x80);
    }
    // SAFETY: same; programs CNTV_TVAL_EL0 + CNTV_CTL_EL0.
    unsafe { arch::aarch64::timer::init(TICK_HZ) };

    let _ = writeln!(
        con,
        "\nentering EL0 stub tasks (preemption demo: A/B interleaved by timer)..."
    );

    // SAFETY: single-threaded boot context; no other reference into the
    // page-table arena, user pages, or stub proc slots exists. Bootstrap
    // populates both stub slots and enqueues them.
    unsafe { arch::userland_bootstrap() };

    // SAFETY: at least one proc is enqueued (we just enqueued two); first
    // eret transitions from EL1 boot context into EL0 user execution.
    unsafe { proc::sched::run() }
}

#[cfg(not(target_os = "none"))]
fn main() {}

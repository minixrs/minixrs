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
mod panic;
#[cfg(target_os = "none")]
mod proc;
#[cfg(target_os = "none")]
mod uart;

#[cfg(target_os = "none")]
use core::fmt::Write;

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

    arch::halt()
}

#[cfg(not(target_os = "none"))]
fn main() {}

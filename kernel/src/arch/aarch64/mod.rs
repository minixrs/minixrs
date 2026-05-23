pub mod context;
pub mod exception;
pub mod gic;
pub mod irq;
pub mod limine;
pub mod mmu;
pub mod timer;
pub mod uart;
pub mod userland;

pub use context::ArchRegisterFrame;
pub use uart::Pl011 as Uart;
pub use uart::{set_base as set_uart_base, PL011_PHYS_BASE};
pub use userland::userland_bootstrap;

pub fn init() {
    exception::install_vectors();
}

pub fn halt() -> ! {
    loop {
        // SAFETY: WFE halts the CPU until an event; no memory access, no flags.
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

pub fn limine_hhdm_offset() -> Option<u64> {
    limine::hhdm_offset()
}

pub fn limine_base_revision_supported() -> bool {
    limine::base_revision_supported()
}

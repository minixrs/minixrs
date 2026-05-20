//! Minimal PL011 UART driver for early kernel output.
//!
//! Targets the QEMU `virt` machine, where the PL011 MMIO base is fixed at
//! physical `0x0900_0000`. QEMU pre-initializes the controller for
//! 8-N-1 / 115200, so we only need to poll the TX-FIFO-full flag and write
//! the data register.
//!
//! Under Limine protocol revision 3 on aarch64, device memory is *not*
//! identity-mapped; physical addresses are reached through the Higher Half
//! Direct Map (HHDM). `set_base(virt)` records the post-HHDM virtual base
//! before any UART use. Calling `Pl011::new()` before `set_base` returns a
//! sink that silently drops writes -- this prevents the panic handler from
//! triple-faulting during very early bootstrap.
//!
//! A proper console abstraction (with locking, per-arch base discovery from
//! the device tree, virtio-console, etc.) arrives in Phase 6.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, Ordering};

pub const PL011_PHYS_BASE: usize = 0x0900_0000;

const DR_OFFSET: usize = 0x00;
const FR_OFFSET: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;

static UART_BASE: AtomicUsize = AtomicUsize::new(0);

pub fn set_base(virt: usize) {
    UART_BASE.store(virt, Ordering::Release);
}

pub struct Pl011 {
    base: usize,
}

impl Pl011 {
    pub fn new() -> Self {
        Self {
            base: UART_BASE.load(Ordering::Acquire),
        }
    }

    pub fn putc(&self, c: u8) {
        if self.base == 0 {
            return; // uninitialised; drop the byte
        }
        // SAFETY: `base` was set via `set_base` from a virtual address that
        // points at PL011 MMIO (either the identity map or HHDM_offset +
        // PL011_PHYS_BASE). Polling FR.TXFF before writing DR is the
        // documented send sequence (ARM PL011 TRM r1p5 section 3.3).
        unsafe {
            let fr = (self.base + FR_OFFSET) as *mut u32;
            while read_volatile(fr) & FR_TXFF != 0 {}
            let dr = (self.base + DR_OFFSET) as *mut u32;
            write_volatile(dr, c as u32);
        }
    }
}

impl fmt::Write for Pl011 {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.putc(b'\r');
            }
            self.putc(b);
        }
        Ok(())
    }
}

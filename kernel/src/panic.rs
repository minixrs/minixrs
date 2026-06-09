// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
use crate::arch;
use core::fmt::Write;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Phase 1: no global logger and no lock yet. Constructing a fresh UART
    // handle is fine because the kernel is single-CPU + interrupts-disabled
    // for now; a proper Mutex-guarded console arrives with the kernel heap
    // in Phase 2.
    let mut uart = arch::Uart::new();
    let _ = writeln!(uart);
    let _ = writeln!(uart, "!!! KERNEL PANIC: {info}");
    arch::halt()
}

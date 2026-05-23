//! Exception vector installation and the Phase 1 panic-on-trap handler.
//!
//! The vector table itself lives in `vectors.S` (16 ARMv8 vectors, each
//! saving the register file into an `ExceptionFrame` on the stack and
//! calling into `exception_entry` below). Phase 1 treats every exception
//! as fatal: we print the relevant system registers and panic. Real IRQ,
//! syscall, and page-fault handlers replace this in Phase 2 onward.

use crate::arch::aarch64::uart::Pl011;
use core::arch::asm;
use core::fmt::Write;

#[repr(C)]
pub struct ExceptionFrame {
    pub gprs: [u64; 31], // x0..x30, mirror of the stp/str sequence in vectors.S
    pub spsr_el1: u64,
    pub elr_el1: u64,
    pub esr_el1: u64,
    pub far_el1: u64,
    pub _pad: u64,
}

// Frame layout is hand-encoded in vectors.S (sub sp, sp, #0x120 + the stp/str
// sequence). Catch accidental field reordering or resizing at compile time
// instead of as silent stack corruption on the first real exception.
const _: () = assert!(core::mem::size_of::<ExceptionFrame>() == 0x120);

unsafe extern "C" {
    static _vector_table: u8;
}

pub fn install_vectors() {
    let addr = core::ptr::addr_of!(_vector_table) as u64;
    // SAFETY: VBAR_EL1 is writable at EL1. The address points at the linker-
    // provided vector table symbol, aligned to 2 KiB by `.balign 0x800` in
    // vectors.S (required: VBAR_EL1[10:0] are RES0). The table is in .text,
    // never freed.
    unsafe {
        asm!(
            "msr vbar_el1, {0}",
            "isb",
            in(reg) addr,
            options(nomem, nostack),
        );
    }
}

#[unsafe(no_mangle)]
extern "C" fn exception_entry(frame: &ExceptionFrame, kind: u64) -> ! {
    let mut uart = Pl011::new();
    let _ = writeln!(uart);
    let _ = writeln!(uart, "!!! kernel exception (vector index {kind})");
    let ec = (frame.esr_el1 >> 26) & 0x3F;
    let iss = frame.esr_el1 & 0xFF_FFFF;
    let _ = writeln!(
        uart,
        "    ESR_EL1  = {:#018x}  (EC = {:#04x}, ISS = {:#08x})",
        frame.esr_el1, ec, iss
    );
    let _ = writeln!(
        uart,
        "    ELR_EL1  = {:#018x}  FAR_EL1  = {:#018x}",
        frame.elr_el1, frame.far_el1
    );
    let _ = writeln!(uart, "    SPSR_EL1 = {:#018x}", frame.spsr_el1);
    panic!(
        "aarch64 exception: kind={kind} ESR_EL1={:#x}",
        frame.esr_el1
    );
}

/// Diagnose-and-panic helper for EL0 sync exceptions that aren't `SVC`.
///
/// `trap.S` calls this from vector slot 8 when ESR_EL1.EC ≠ 0x15 (SVC64).
/// Slice 2.3 doesn't expect any other EL0 sync exception in the happy path,
/// so we treat them as fatal — same policy as slice 2.2's blanket
/// `exception_entry`.
#[unsafe(no_mangle)]
extern "C" fn el0_sync_unexpected(esr: u64, elr: u64, far: u64) -> ! {
    let mut uart = Pl011::new();
    let ec = (esr >> 26) & 0x3F;
    let iss = esr & 0xFF_FFFF;
    let _ = writeln!(uart);
    let _ = writeln!(uart, "!!! unexpected EL0 sync exception (not SVC)");
    let _ = writeln!(
        uart,
        "    ESR_EL1  = {esr:#018x}  (EC = {ec:#04x}, ISS = {iss:#08x})",
    );
    let _ = writeln!(uart, "    ELR_EL1  = {elr:#018x}  FAR_EL1  = {far:#018x}");
    panic!("EL0 sync exception: EC={ec:#x} ESR_EL1={esr:#x}");
}

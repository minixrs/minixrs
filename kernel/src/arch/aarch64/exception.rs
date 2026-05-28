//! Exception vector installation and the Phase 1 panic-on-trap handler.
//!
//! The vector table itself lives in `vectors.S` (16 ARMv8 vectors, each
//! saving the register file into an `ExceptionFrame` on the stack and
//! calling into `exception_entry` below). Phase 1 treats every exception
//! as fatal: we print the relevant system registers and panic. Real IRQ,
//! syscall, and page-fault handlers replace this in Phase 2 onward.

use crate::arch::aarch64::addrspace::{Prot, map_page_in};
use crate::arch::aarch64::mmu::{self, PAGE_SIZE};
use crate::arch::aarch64::uart::Pl011;
use crate::mm::alloc_frame;
use crate::proc::flags::RTS_PAGEFAULT;
use crate::proc::page_fault::{PFF_INSTR, PFF_PERMISSION, PFF_WRITE, PageFaultState};
use crate::proc::sched;
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
/// Slice 3.1b extends slice 2.3's generic dump with a one-page decoder
/// for EC=0x20 (instruction abort from a lower EL) and EC=0x24 (data
/// abort from a lower EL): IFSC/DFSC, WnR, ISV. Slice 3.2's real
/// `do_page_fault` lands on top of this scaffold — for now the policy
/// is still "halt".
#[unsafe(no_mangle)]
extern "C" fn el0_sync_unexpected(esr: u64, elr: u64, far: u64) -> ! {
    let mut uart = Pl011::new();
    let ec = (esr >> 26) & 0x3F;
    let iss = esr & 0xFF_FFFF;
    let _ = writeln!(uart);
    match ec {
        0x20 => {
            // Instruction abort, lower EL. ISS[5:0] = IFSC.
            let ifsc = iss & 0x3F;
            let _ = writeln!(
                uart,
                "!!! EL0 instruction abort (translation/permission)"
            );
            let _ = writeln!(
                uart,
                "    IFSC = {:#04x} ({})",
                ifsc,
                fsc_name(ifsc)
            );
        }
        0x24 => {
            // Data abort, lower EL. ISS[5:0]=DFSC, ISS[6]=WnR, ISS[24]=ISV.
            let dfsc = iss & 0x3F;
            let wnr = (iss >> 6) & 1;
            let isv = (iss >> 24) & 1;
            let _ = writeln!(
                uart,
                "!!! EL0 data abort (translation/permission)"
            );
            let _ = writeln!(
                uart,
                "    DFSC = {:#04x} ({})  WnR = {}  ISV = {}",
                dfsc,
                fsc_name(dfsc),
                wnr,
                isv,
            );
        }
        _ => {
            let _ = writeln!(uart, "!!! unexpected EL0 sync exception (not SVC)");
        }
    }
    let _ = writeln!(
        uart,
        "    ESR_EL1  = {esr:#018x}  (EC = {ec:#04x}, ISS = {iss:#08x})",
    );
    let _ = writeln!(uart, "    ELR_EL1  = {elr:#018x}  FAR_EL1  = {far:#018x}");
    panic!("EL0 sync exception: EC={ec:#x} ESR_EL1={esr:#x}");
}

/// EL0 page-fault handler (slice 3.2).
///
/// `trap.S` calls this from vector slot 8 for every non-SVC EL0 sync
/// exception, passing `(ESR_EL1, ELR_EL1, FAR_EL1)`. It returns for faults
/// it resolves; for anything it can't, it tail-calls
/// [`el0_sync_unexpected`] (`-> !`, the halt path).
///
/// Flow: classify the fault, stash it in the proc's
/// [`PageFaultState`](crate::proc::page_fault::PageFaultState), block the
/// proc on `RTS_PAGEFAULT`, then — for this slice only — resolve heap-window
/// *translation* faults inline (the kernel stands in for the not-yet-existing
/// VM server; permission faults halt, as re-protecting is a VM-server job).
/// On return, `trap.S` runs `el1_svc_tail` (= `sched::schedule_next`) and
/// `el1_return_to_user`, so the now-runnable faulting proc is rescheduled
/// and retries the aborting instruction (aarch64 leaves `ELR_EL1` at the
/// faulting insn).
///
/// Slice 3.4 replaces the inline resolve with a kernel-originated
/// `VM_PAGEFAULT` send: the proc then stays blocked on `RTS_PAGEFAULT`
/// across the reschedule until VM answers with
/// `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`. The block/record half here is
/// already shaped for that.
#[unsafe(no_mangle)]
extern "C" fn do_page_fault(esr: u64, elr: u64, far: u64) {
    let ec = (esr >> 26) & 0x3F;
    let iss = esr & 0xFF_FFFF;

    // Only EL0 instruction (0x20) / data (0x24) aborts are faults we try to
    // resolve; any other non-SVC sync exception is a genuine bug.
    if ec != 0x20 && ec != 0x24 {
        el0_sync_unexpected(esr, elr, far); // -> !
    }

    // Arch-neutral classification for `page_fault_state` (read by VM in 3.3+).
    let fsc = iss & 0x3F;
    let mut flags = 0u32;
    if ec == 0x20 {
        flags |= PFF_INSTR;
    }
    if ec == 0x24 && (iss >> 6) & 1 != 0 {
        flags |= PFF_WRITE;
    }
    if matches!(fsc, 0x0D..=0x0F) {
        flags |= PFF_PERMISSION;
    }

    // Record + block under a tightly-scoped borrow, then drop it before any
    // rts transition. `rts_set`/`rts_unset` re-borrow the same slot via
    // `dequeue`/`enqueue` (`proc_slot_mut`), so holding `p` live across them
    // and using it afterward would alias the slot — the
    // two-&mut-from-one-UnsafeCell hazard (see CLAUDE.md; mirrors the
    // borrow scoping in `ipc::send`/`ipc::receive`). Capture every scalar the
    // inline resolve needs here instead.
    let (window, name, ttbr0_pa, asid);
    {
        // SAFETY: exception context — single-threaded, DAIF.I masked. The
        // faulting proc is CURRENT and not otherwise borrowed.
        let p = unsafe { sched::current_proc_mut() }.expect("page fault: no current proc");
        p.page_fault_state = PageFaultState { addr: far, flags, ip: elr };
        window = p.heap_window;
        name = p.name[0];
        ttbr0_pa = p.ttbr0_pa;
        asid = p.asid;
        // SAFETY: rts_set captures `nr` then ends its &mut Proc borrow before
        // dequeue; the outer `p` borrow ends with this block.
        unsafe { sched::rts_set(p, RTS_PAGEFAULT) };
    }

    // No VM yet (slice 3.4 sends VM_PAGEFAULT here); a fault outside the
    // kernel-resolved heap window is unrecoverable → halt with the decoder.
    if !window.contains(far) {
        el0_sync_unexpected(esr, elr, far); // -> !
    }
    // The inline resolve only maps fresh frames, so it satisfies translation
    // faults. A permission fault means the page is already mapped with the
    // wrong AP bits — `map_page_in` would return `AlreadyMapped` and the
    // `.expect` below would panic misleadingly. Re-protecting an existing
    // mapping is a VM-server job, so halt explicitly instead.
    if flags & PFF_PERMISSION != 0 {
        el0_sync_unexpected(esr, elr, far); // -> !
    }

    // --- kernel-as-VM resolution (slice 3.4 lifts this into the VM server) ---
    let page_base = far & !((PAGE_SIZE as u64) - 1);

    let frame = alloc_frame().expect("page fault: out of frames");
    map_page_in(ttbr0_pa, page_base, frame.addr(), Prot::RW_DATA)
        .expect("page fault: map_page_in");
    // SAFETY: ASID-tagged TLBI; the faulting proc's TTBR0 is the live one.
    unsafe { mmu::flush_tlb_asid(asid) };

    let mut uart = Pl011::new();
    let _ = writeln!(
        uart,
        "[pf] proc={} far={:#x} -> alloc frame={:#x}, map RW, retry",
        name as char,
        far,
        frame.addr(),
    );

    // SAFETY: fresh tightly-scoped borrow; the rts_set block above already
    // ended. Clearing the fault state and unblocking happen together.
    {
        let p = unsafe { sched::current_proc_mut() }.expect("page fault: no current proc");
        p.page_fault_state = PageFaultState::EMPTY;
        // SAFETY: rts_unset captures `nr` then ends the borrow before enqueue.
        unsafe { sched::rts_unset(p, RTS_PAGEFAULT) };
    }
}

/// Tiny FSC-name decoder. Covers the codes a stub on a fresh AddrSpace
/// would actually hit. The full table (ARM ARM D13.2.40) has ~30 codes;
/// we just want boot-log triage to be tractable. Slice 3.2's real PF
/// handler will branch on FSC and replace this with structured handling.
fn fsc_name(fsc: u64) -> &'static str {
    match fsc {
        0x04 => "translation fault, L0",
        0x05 => "translation fault, L1",
        0x06 => "translation fault, L2",
        0x07 => "translation fault, L3",
        0x09 => "access flag fault, L1",
        0x0A => "access flag fault, L2",
        0x0B => "access flag fault, L3",
        0x0D => "permission fault, L1",
        0x0E => "permission fault, L2",
        0x0F => "permission fault, L3",
        0x21 => "alignment fault",
        _ => "other",
    }
}

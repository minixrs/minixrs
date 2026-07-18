// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Slice-3.1b userland bootstrap: per-process address spaces with 8-bit
//! ARMv8 ASIDs.
//!
//! Each of the three EL0 stubs (A, B, C — the slice 2.5 / 2.6 regression
//! coverage) gets its own [`AddrSpace`] built from the slice-3.1a frame
//! allocator. Code and stack pages are allocated as fresh frames, the
//! stub blob is copied in via HHDM (`mm::phys_to_hhdm`), and the
//! resulting `(ttbr0_pa, asid)` is stored on the proc slot. The scheduler
//! ([`crate::proc::sched::schedule_next`]) installs that TTBR0 on every
//! EL1 → EL0 transition.
//!
//! What slice 3.1a's smoke test exercised in isolation — `AddrSpace::new`,
//! `map_page`, `walk_pt`, the free-list reuse on `destroy` — now drives
//! the three real EL0 stubs.
//!
//! What this slice does *not* do:
//!   - Recover from page faults. EC=0x20 / EC=0x24 still panic via
//!     [`super::exception::el0_sync_unexpected`]; slice 3.1b extends that
//!     handler's ISS decoder so the dump is informative, but the policy
//!     is still "halt" — real `do_page_fault` + `RTS_PAGEFAULT` land in
//!     slice 3.2.
//!   - Cross-AS IPC delivery. The SVC handler runs in the caller's TTBR0,
//!     so single-AS reads in `ipc::message` still walk correctly; slice
//!     3.4's HHDM-walk redesign of `flush_deliver_msg` swaps that out
//!     once VM exists.
//!   - Install a kernel-shared global mapping for the EL0 stubs. The
//!     code page is mapped RO + EL0-executable into each per-proc AS
//!     individually (zero shared frames).

use core::sync::atomic::Ordering;

use minixrs_kernel_shared::callnr::{KERNEL_CALL, SYS_GETINFO};
use minixrs_kernel_shared::com::{
    RS_PROC_NR, SCHED_PROC_NR, STUB_A_PROC_NR, STUB_B_PROC_NR, STUB_C_PROC_NR, STUB_D_PROC_NR,
    SYSTEM, VM_PROC_NR, boot_endpoint,
};
use minixrs_kernel_shared::{PrivId, ProcNr};

use crate::arch::aarch64::addrspace::{AddrSpace, Prot, walk_leaves};
use crate::arch::aarch64::asid::alloc_asid;
use crate::arch::aarch64::mmu::{self, PAGE_SIZE, flush_icache_range};
use crate::mm::{Frame, alloc_frame, free_frame, phys_to_hhdm};
use crate::proc::bitmap::{set_call_bit, set_sys_bit};
use crate::proc::flags::{BILLABLE, PREEMPTIBLE, RTS_NO_PRIV, SRV_T, SYS_PROC, USR_T};
use crate::proc::sched;
use crate::proc::table::{priv_slot_mut, proc_index, proc_slot_mut, proc_table_ref};
use crate::proc::{HeapWindow, Priv, Proc};

// ----- EL0 virtual addresses ------------------------------------------------

/// VA at which stub A's code page is mapped.
pub const USER_CODE_VA_A: u64 = 0x0040_0000; // 4 MiB
/// VA at which stub A's stack page is mapped.
pub const USER_STACK_VA_A: u64 = 0x0080_0000; // 8 MiB

/// VA at which stub B's code page is mapped.
pub const USER_CODE_VA_B: u64 = 0x0041_0000;
/// VA at which stub B's stack page is mapped.
pub const USER_STACK_VA_B: u64 = 0x0081_0000;

/// VA at which stub C's code page is mapped.
pub const USER_CODE_VA_C: u64 = 0x0042_0000;
/// VA at which stub C's stack page is mapped.
pub const USER_STACK_VA_C: u64 = 0x0082_0000;

/// VA at which stub D's code page is mapped.
pub const USER_CODE_VA_D: u64 = 0x0043_0000;
/// VA at which stub D's stack page is mapped.
pub const USER_STACK_VA_D: u64 = 0x0083_0000;

// Stub D's heap VA (`0x0100_0000`) lives only in `user_stub.S`'s stub D blob
// and in the VM server's `region::HEAP_BASE` (slice 3.5): D issues `VM_BRK` to
// grow its heap, then touches it. The kernel needs no heap-window constant —
// faults route to VM, which gates them on its per-proc region table.

// Stub proc numbers (slots 11..=15, just past the boot image) live in
// `kernel-shared::com` since slice 4.5 — PM's mproc table seeds them, so the
// kernel and PM must agree on one source.

/// Privilege-table slots for the dedicated-priv stubs A–D. Boot image uses
/// 0..=15; 16, 17, 18, and 19 are the first free entries. The next slot up
/// (`proc::table::USER_PRIV_ID` = 20) is the shared USER priv that init (PID 1)
/// and every forked child use — no stub owns it.
const STUB_A_PRIV_ID: PrivId = PrivId::new(16);
const STUB_B_PRIV_ID: PrivId = PrivId::new(17);
const STUB_C_PRIV_ID: PrivId = PrivId::new(18);
const STUB_D_PRIV_ID: PrivId = PrivId::new(19);

// SPSR_EL1 to install on each stub's `eret`. Matches slice 2.4: IRQs
// unmasked at EL0 (bit 7 = 0), debug + SError + FIQ masked. EL0t. Shared with
// `system::do_exec`, which resets an exec'd proc to the same EL0 state.
pub(crate) const STUB_SPSR_EL0: u64 = 0x340;

// ----- EL0 stub blobs (linked into the kernel image by `user_stub.S`) ------

unsafe extern "C" {
    static _user_stub_a_start: u8;
    static _user_stub_a_end: u8;
    static _user_stub_b_start: u8;
    static _user_stub_b_end: u8;
    static _user_stub_c_start: u8;
    static _user_stub_c_end: u8;
    static _user_stub_d_start: u8;
    static _user_stub_d_end: u8;
}

/// VA at which each boot server's stack page is mapped. Distinct from the server
/// ELF segments (loaded from `0x0010_0000` up by `servers/*/user.ld`) and from
/// every EL0 stub VA. Shared across all servers: each has its own per-process
/// TTBR0, so the same low VA resolves to a distinct frame per server with no
/// collision (the same reason `user.ld`'s `0x0010_0000` base is shared).
const SERVER_STACK_VA: u64 = 0x0020_0000; // 2 MiB

// ----- Bootstrap ----------------------------------------------------------

/// Build three per-process address spaces (one per EL0 stub), populate
/// their proc + priv slots, and enqueue them on the scheduler.
///
/// Compared to slice 2.5/2.6 this:
///   - Drops the static `L0_TABLE`/.../`L3_STACK_TABLE` + `USER_*_PAGE_*`
///     arrays. Every frame is allocated from `mm::alloc_frame`.
///   - Hands each stub its own `(ttbr0_pa, asid)` via
///     [`build_stub`] + [`AddrSpace`].
///   - No longer installs a single TTBR0 from boot context; the scheduler
///     installs per-proc TTBR0 + ASID on every EL1 → EL0 transition.
///
/// SAFETY: must be called exactly once from `kmain` after `proc::init`,
/// `mm::set_hhdm_offset`, and `mm::init_from_limine_memmap`. Single-
/// threaded boot context; no other reference into the named proc slots
/// or the frame allocator may be live concurrently.
pub unsafe fn userland_bootstrap() {
    // 1. Verify Limine left MAIR_EL1 + TCR_EL1 in the shape our walker
    //    + per-proc TTBR0 install expect. The slice-3.1b TCR_EL1.AS = 0
    //    assert was added inside `assert_tcr_el1_ttbr0_ready`.
    mmu::assert_mair_normal_wb();
    mmu::assert_tcr_el1_ttbr0_ready();

    // 2. Clear TCR_EL1.EPD0 once. Per-proc TTBR0 install happens at every
    //    context switch via `proc::sched::schedule_next`.
    // SAFETY: DAIF still masked from Limine handoff; single-threaded boot.
    unsafe { mmu::enable_ttbr0_walks_once() };

    // 2.5. Load every boot server from the embedded MXBI archive (slice 4.2).
    //      VM is packed first so it takes ASID 1 and is enqueued first; its
    //      `RECEIVE(ANY)` blocks immediately (no senders yet), leaving the run
    //      queue free for the band-8 stubs. Each server's proc/priv slot was
    //      already populated by `proc::init` → `init_boot_image`; this fills in
    //      the address space + EL0 entry state, clears RTS_NO_PRIV, and enqueues.
    // SAFETY: single-threaded boot; sole writer of each server's proc slot + the
    // frame allocator.
    unsafe {
        let image = crate::boot_image::BootImage::get();
        for (nr, elf) in image.iter() {
            // A negative proc-nr (`com::EXEC_ONLY_PROC_NR`) tags an archive
            // module that is not a boot server — it is packed only so
            // `SYS_EXEC` can resolve it by name (slice 4.7's `worker`), and it
            // has no proc/priv slot to load into. Skip it.
            if nr.get() < 0 {
                continue;
            }
            load_boot_server(nr, elf);
        }
    }

    // 3. Build each stub. Sequential calls — each build_stub allocates
    //    fresh frames + a fresh AddrSpace and writes its proc slot.
    // SAFETY: single-threaded boot; build_stub only touches its own
    // allocated state + the named proc slot.
    unsafe {
        build_stub(
            STUB_A_PROC_NR,
            Some(STUB_A_PRIV_ID),
            b'A',
            &_user_stub_a_start,
            &_user_stub_a_end,
            USER_CODE_VA_A,
            USER_STACK_VA_A,
            HeapWindow::EMPTY,
            false,
        );
        build_stub(
            STUB_B_PROC_NR,
            Some(STUB_B_PRIV_ID),
            b'B',
            &_user_stub_b_start,
            &_user_stub_b_end,
            USER_CODE_VA_B,
            USER_STACK_VA_B,
            HeapWindow::EMPTY,
            false,
        );
        build_stub(
            STUB_C_PROC_NR,
            Some(STUB_C_PRIV_ID),
            b'C',
            &_user_stub_c_start,
            &_user_stub_c_end,
            USER_CODE_VA_C,
            USER_STACK_VA_C,
            HeapWindow::EMPTY,
            false,
        );
        build_stub(
            STUB_D_PROC_NR,
            Some(STUB_D_PRIV_ID),
            b'D',
            &_user_stub_d_start,
            &_user_stub_d_end,
            USER_CODE_VA_D,
            USER_STACK_VA_D,
            // D issues `VM_BRK` to grow its heap and touches it (slice 3.5),
            // then `VM_MMAP`/`VM_MUNMAP` an anonymous region (slice 3.6). Faults
            // route to the VM server, which gates them on its region table. The
            // kernel `heap_window` fast path stays unused, so EMPTY.
            HeapWindow::EMPTY,
            false,
        );
    }

    // 3.5. Pre-delegate stub C to the user-space SCHED server (slice 4.3). C is
    //      a CPU-bound SYS_GETINFO loop, so it regularly exhausts its quantum,
    //      exercising the kernel → SCHED `SCHEDULING_NO_QUANTUM` → `SYS_SCHEDULE`
    //      round-trip. Every other proc stays kernel-scheduled (`scheduler ==
    //      NONE`), including SCHED itself (a scheduler must not schedule itself).
    //      This stands in for the `SCHEDULING_START` a real RS/PM will issue once
    //      they exist (slice 4.5/4.6); until then the kernel claims C directly.
    // SAFETY: single-threaded boot; sole borrow of stub C's slot.
    unsafe {
        let p = proc_slot_mut(STUB_C_PROC_NR).expect("stub C proc slot in range");
        p.scheduler = boot_endpoint(SCHED_PROC_NR);
    }

    // 4. Install priv slots — unchanged from slice 2.6.
    // SAFETY: single-threaded boot; sequential mutable borrows on
    // priv-table slots 16, 17, 18.
    unsafe { install_stub_privs() };

    // 5. Enqueue the runnable stubs A–D. (The fork-loop stub E was retired in
    //    slice 4.8; init (PID 1) drives fork/exec/wait as a real boot process.)
    // SAFETY: single-threaded boot; no other PROC_TABLE / RUNQ borrows
    // live at this point.
    unsafe {
        sched::enqueue(STUB_A_PROC_NR);
        sched::enqueue(STUB_B_PROC_NR);
        sched::enqueue(STUB_C_PROC_NR);
        sched::enqueue(STUB_D_PROC_NR);
    }

    // 6. One-shot diagnostic: dump per-proc (ttbr0_pa, asid) so the
    //    boot log proves each stub has its own distinct AS.
    //    SAFETY: single-threaded boot; read-only.
    unsafe { print_addrspace_summary() };
}

/// Load one boot server's ELF (`elf`) into a fresh address space, prime its
/// entry/stack registers, clear the boot `RTS_NO_PRIV` block, and enqueue it.
/// Generalizes the slice-3.4 `vm_bootstrap` over the MXBI archive (slice 4.2).
///
/// Unlike the hand-coded stubs (which use ad-hoc proc/priv slots), a boot server
/// occupies its *real* boot slot `nr`: `proc::init` → `init_boot_image` already
/// populated its name, priority, endpoint, and privilege slot (`trap_mask =
/// SRV_T` → RECEIVE ANY + SEND to any active slot; full `k_call_mask`). So this
/// fills in only the address space + EL0 entry state. No `install_*_priv` helper
/// is needed.
///
/// A freshly built user address space ready to become a proc's image: its
/// page-table root PA, ASID, program entry VA, and initial stack pointer.
/// Produced by [`load_exec_image`]; consumed by boot-server load and
/// `system::do_exec`.
pub(crate) struct ExecImage {
    pub ttbr0_pa: u64,
    pub asid: u8,
    pub entry: u64,
    pub sp_top: u64,
}

/// Build a fresh address space from an ELF image: allocate the L0 root, load
/// the `PT_LOAD` segments (BSS satisfied for free — `alloc_frame` zeroes), map
/// one zeroed RW stack page at [`SERVER_STACK_VA`], and allocate an ASID. The
/// `AddrSpace` is `mem::forget`-ed — the page-table tree is now owned via the
/// returned `ttbr0_pa` (tear it down with the `do_exit` teardown sequence, never
/// `AddrSpace::destroy`). Returns `None` on OOM or a malformed ELF, freeing any
/// partial tree first so nothing leaks (the do_fork `copy_addrspace` contract).
///
/// The stack VA is shared with every boot server because each image gets its own
/// TTBR0, so the same low VA resolves to a distinct frame per proc.
///
/// SAFETY: single-threaded EL1; the sole caller of the frame allocator + ASID
/// pool for its duration. Must run after `mm::init_from_limine_memmap`.
pub(crate) unsafe fn load_exec_image(elf: &[u8]) -> Option<ExecImage> {
    let mut aspace = AddrSpace::new().ok()?;

    let entry = match crate::boot_image::elf::load_into(elf, &mut aspace) {
        Ok(e) => e,
        Err(_) => {
            destroy_addrspace_with_leaves(aspace);
            return None;
        }
    };

    // Stack: one zeroed RW page; SP starts at its top.
    let stack_frame = match alloc_frame() {
        Some(f) => f,
        None => {
            destroy_addrspace_with_leaves(aspace);
            return None;
        }
    };
    if aspace
        .map_page(SERVER_STACK_VA, stack_frame.addr(), Prot::RW_DATA)
        .is_err()
    {
        // `map_page` failed before linking the leaf, so free the orphan stack
        // frame explicitly; the leaf sweep below won't see it.
        free_frame(stack_frame);
        destroy_addrspace_with_leaves(aspace);
        return None;
    }

    let ttbr0_pa = aspace.ttbr0_pa;
    // SAFETY: single-threaded EL1 context; sole accessor of the ASID pool.
    let asid = unsafe { alloc_asid() };
    // The page-table tree is durable via `ttbr0_pa`; don't run `destroy`.
    core::mem::forget(aspace);
    Some(ExecImage {
        ttbr0_pa,
        asid,
        entry,
        sp_top: SERVER_STACK_VA + PAGE_SIZE as u64,
    })
}

/// Free every mapped leaf frame of a partially-built address space, then the
/// tree itself. Used only on the [`load_exec_image`] error paths, before any
/// ASID is allocated — so unlike `do_exit::teardown_addrspace` there is nothing
/// to flush or recycle.
fn destroy_addrspace_with_leaves(aspace: AddrSpace) {
    let ttbr0_pa = aspace.ttbr0_pa;
    let _ = walk_leaves(ttbr0_pa, &mut |_va, pa, _prot| {
        free_frame(Frame::from_addr(pa));
        Ok(())
    });
    aspace.destroy();
}

/// The page-table tree is durable via `Proc::ttbr0_pa` for the same reason as
/// [`build_stub`]: `load_exec_image` `mem::forget`s the `AddrSpace`.
///
/// SAFETY: single-threaded boot; the only writer of `nr`'s proc slot and the
/// frame allocator here. Must run after `mm::init_from_limine_memmap` and
/// before `sched::run`. `nr` must be a boot server already populated by
/// `init_boot_image` (blocked on `RTS_NO_PRIV`).
unsafe fn load_boot_server(nr: ProcNr, elf: &[u8]) {
    use crate::arch::aarch64::uart::Pl011;
    use core::fmt::Write;

    // SAFETY: single-threaded boot; sole caller of the frame allocator + ASID
    // pool at this point.
    let img = unsafe { load_exec_image(elf) }.expect("server image load failed");

    // SAFETY: single-threaded boot; sole borrow of this server's slot.
    unsafe {
        let p = proc_slot_mut(nr).expect("server proc slot in range");
        p.regs.elr_el1 = img.entry;
        p.regs.sp_el0 = img.sp_top;
        p.regs.spsr_el1 = STUB_SPSR_EL0;
        p.ttbr0_pa = img.ttbr0_pa;
        p.asid = img.asid;
        p.next_ready = None;
        // Clear the boot RTS_NO_PRIV: the server now has an address space.
        p.rts_flags.store(0, Ordering::Relaxed);

        let name = core::str::from_utf8(&p.name)
            .unwrap_or("?")
            .trim_end_matches('\0');
        let _ = writeln!(
            Pl011::new(),
            "[as] {} nr={} ttbr0_pa={:#x} asid={} entry={:#x}",
            name,
            p.nr.get(),
            p.ttbr0_pa,
            p.asid,
            img.entry,
        );
    }

    // SAFETY: single-threaded boot; no other PROC_TABLE / RUNQ borrow is live.
    unsafe { sched::enqueue(nr) };
}

/// Build one stub's address space: allocate L0 + code + stack frames,
/// copy the stub blob into the code frame, install RX/RW mappings, and
/// write `(ttbr0_pa, asid)` into the proc slot.
///
/// `aspace` is intentionally `mem::forget`-ed at the end: the page-table
/// tree is now owned by the proc (via its `ttbr0_pa`); the kernel will
/// only reach into it again via HHDM walks. `AddrSpace::destroy` would
/// recursively free every L1/L2/L3 frame plus the L0 root, which is
/// exactly what we don't want.
///
/// SAFETY: single-threaded boot; this is the only writer of `nr`'s proc
/// slot. `stub_start..stub_end` must be a valid contiguous rodata range
/// in the kernel image; the linker guarantees both.
unsafe fn build_stub(
    nr: ProcNr,
    priv_id: Option<PrivId>,
    id: u8,
    stub_start: *const u8,
    stub_end: *const u8,
    code_va: u64,
    stack_va: u64,
    heap_window: HeapWindow,
    frozen: bool,
) {
    // Per-proc page-table tree. AddrSpace::new allocates and zeroes the
    // L0 root via the frame allocator.
    let mut aspace = AddrSpace::new().expect("AddrSpace::new failed during userland_bootstrap");
    let ttbr0_pa = aspace.ttbr0_pa;

    // Code frame: allocate, copy stub bytes in via HHDM, flush I-cache,
    // map RO + EL0-executable.
    let code_frame = alloc_frame().expect("code frame alloc failed during userland_bootstrap");
    // SAFETY: stub_start/stub_end are rodata symbols inside the kernel
    // image; the new code frame is exclusively ours (just allocated, not
    // yet mapped into any AS) and HHDM-mapped.
    unsafe { copy_stub_into_frame(code_frame, stub_start, stub_end) };
    aspace
        .map_page(code_va, code_frame.addr(), Prot::RO_CODE)
        .expect("map_page(code) during userland_bootstrap");

    // Stack frame: zeroed by `alloc_frame`, mapped RW + EL0-no-execute.
    let stack_frame = alloc_frame().expect("stack frame alloc failed during userland_bootstrap");
    aspace
        .map_page(stack_va, stack_frame.addr(), Prot::RW_DATA)
        .expect("map_page(stack) during userland_bootstrap");

    // Hand the AddrSpace's root over to the proc slot.
    // SAFETY: single-threaded boot; this is the only `&mut Proc` borrow
    // at this nr right now.
    let asid = unsafe { alloc_asid() };
    unsafe {
        let p = proc_slot_mut(nr).expect("stub proc slot in range");
        populate_stub_slot(
            p,
            nr,
            priv_id,
            id,
            code_va,
            stack_va + PAGE_SIZE as u64,
            ttbr0_pa,
            asid,
            heap_window,
            frozen,
        );
    }

    // The page-table tree is durable via `ttbr0_pa`. Drop the AddrSpace
    // value without running its (no-op-today, but defensively forget'd)
    // destructor — leaving `AddrSpace::destroy` reserved for exit/exec
    // paths that actually want to tear an AS down.
    core::mem::forget(aspace);
}

/// Copy `[stub_start, stub_end)` into `frame` (resolved via HHDM) and
/// flush the I-cache so the upcoming EL0 fetch sees the new bytes.
///
/// SAFETY: `frame` must be exclusively owned (not yet mapped anywhere)
/// and the HHDM mapping for its PA must be live. `stub_start..stub_end`
/// must be a valid byte range in the kernel image.
unsafe fn copy_stub_into_frame(frame: Frame, stub_start: *const u8, stub_end: *const u8) {
    // SAFETY: end >= start by linker layout; offset_from is well-defined
    // on a single rodata symbol pair.
    let len = unsafe { stub_end.offset_from(stub_start) } as usize;
    assert!(len > 0 && len <= PAGE_SIZE);
    // SAFETY: caller's invariants hold; we have the sole pointer to this
    // frame, and HHDM is the only mapping.
    unsafe {
        let dst = phys_to_hhdm(frame.addr()) as *mut u8;
        core::ptr::copy_nonoverlapping(stub_start, dst, len);
        flush_icache_range(dst as u64, len);
    }
}

/// Write all per-stub fields into `p`. Extends slice 2.5's helper with
/// `ttbr0_pa` and `asid` — the rest mirrors slice 2.6.
fn populate_stub_slot(
    p: &mut Proc,
    nr: ProcNr,
    priv_id: Option<PrivId>,
    id: u8,
    entry_va: u64,
    stack_top: u64,
    ttbr0_pa: u64,
    asid: u8,
    heap_window: HeapWindow,
    frozen: bool,
) {
    // Name: `id` followed by "-stub\0..." padding. Lets the clock tick
    // handler print `name[0]` to identify which stub is running.
    p.name = [0; 16];
    p.name[0] = id;
    p.name[1] = b'-';
    p.name[2] = b's';
    p.name[3] = b't';
    p.name[4] = b'u';
    p.name[5] = b'b';

    p.nr = nr;
    p.endpoint = boot_endpoint(nr);
    p.priv_id = priv_id;
    p.priority = crate::proc::table::SRV_Q;
    // 5 ticks per quantum = 50 ms at 100 Hz.
    p.quantum_ms = 5;
    p.quantum_left = p.quantum_ms as u64;

    // EL0 entry state.
    p.regs.elr_el1 = entry_va;
    p.regs.sp_el0 = stack_top;
    p.regs.spsr_el1 = STUB_SPSR_EL0;

    // Per-proc address space. Stored on the slot; the scheduler reads
    // both on every EL1 → EL0 transition.
    p.ttbr0_pa = ttbr0_pa;
    p.asid = asid;

    // Kernel-resolved heap window (slice 3.2). Empty for A/B/C.
    p.page_fault_state = crate::proc::PageFaultState::EMPTY;
    p.heap_window = heap_window;

    // Run-queue link starts cleared — `sched::enqueue` sets it.
    p.next_ready = None;

    // Mark runnable — or, for a frozen stub (E), leave it blocked awaiting a
    // privilege: RTS_NO_PRIV is the same "built but not yet privileged" state
    // a forked child will hold in 4.6, cleared by `SYS_PRIVCTL`. The address
    // space above is fully built either way, so `schedule_next`'s
    // ttbr0/asid asserts hold whenever the proc first becomes runnable.
    let rts = if frozen { RTS_NO_PRIV } else { 0 };
    p.rts_flags.store(rts, Ordering::Relaxed);
}

/// Install priv slots for A, B, C. Unchanged from slice 2.6.
///
/// SAFETY: single-threaded boot; touches priv-table slots 16, 17, 18
/// sequentially.
unsafe fn install_stub_privs() {
    // SAFETY: sequential mutable borrow.
    unsafe {
        install_one_stub_priv(STUB_A_PRIV_ID, STUB_A_PROC_NR, STUB_B_PRIV_ID);
    }
    // SAFETY: A's borrow has been dropped.
    unsafe {
        install_one_stub_priv(STUB_B_PRIV_ID, STUB_B_PROC_NR, STUB_A_PRIV_ID);
    }
    // SAFETY: B's borrow has been dropped.
    unsafe {
        install_stub_c_priv();
    }
    // SAFETY: C's borrow has been dropped.
    unsafe {
        install_stub_d_priv();
    }
}

/// Install stub C's priv slot. Differs from the A/B helper: trap_mask =
/// USR_T (SENDREC only), ipc_to opened just to SYSTEM, k_call_mask
/// opened just to `SYS_GETINFO`.
///
/// SAFETY: single-threaded boot; mutates only priv-table slot
/// `STUB_C_PRIV_ID`.
unsafe fn install_stub_c_priv() {
    let system_priv_id = {
        // SAFETY: read-only snapshot; no live `&mut Proc` here.
        let table = unsafe { proc_table_ref() };
        let idx = proc_index(SYSTEM).expect("SYSTEM in proc table");
        table[idx]
            .priv_id
            .expect("SYSTEM priv populated by proc::init")
    };

    // SAFETY: priv index in-range; no overlapping reference held.
    let pr: &mut Priv =
        unsafe { priv_slot_mut(STUB_C_PRIV_ID).expect("stub C priv slot in range") };
    pr.id = STUB_C_PRIV_ID;
    pr.proc_nr = Some(STUB_C_PROC_NR);
    pr.flags = SYS_PROC | BILLABLE | PREEMPTIBLE;
    pr.trap_mask = USR_T;
    pr.ipc_to.fill(0);
    set_sys_bit(&mut pr.ipc_to, system_priv_id);
    pr.k_call_mask.fill(0);
    set_call_bit(&mut pr.k_call_mask, (SYS_GETINFO - KERNEL_CALL) as usize);
    pr.notify_pending.fill(0);
    pr.asyn_pending.fill(0);
    pr.sig_mgr = boot_endpoint(RS_PROC_NR);
}

/// Install stub D's priv slot (slice 3.5/3.6). D drives `brk` and, since slice
/// 3.6, `mmap`/`munmap`: it issues `VM_BRK` / `VM_MMAP` / `VM_MUNMAP` SENDRECs to
/// the VM server, then touches the regions it created. All three ride the same
/// D→VM edge, so D gets `trap_mask = USR_T` (SENDREC only, like stub C) with
/// `ipc_to` opened to VM's priv slot and nothing else. `k_call_mask` stays
/// empty — D never calls the kernel directly; it talks to VM, and its page
/// faults reach VM via the kernel (`mini_pf_send`, which performs no permission
/// check).
///
/// VM's own `ipc_to` was filled by `init_boot_image` only for the active boot
/// priv slots `[0, n_active)` (≈ 0..15); stub D's slot is 19, so VM cannot
/// reply to D's SENDREC out of the box. This helper also opens that reverse
/// direction (VM → D) as a separate, sequential priv-slot borrow.
///
/// SAFETY: single-threaded boot; mutates priv-table slots `STUB_D_PRIV_ID`
/// and VM's slot, one mutable borrow at a time.
unsafe fn install_stub_d_priv() {
    // Resolve VM's priv id from a read-only snapshot (mirrors stub C resolving
    // SYSTEM). No live `&mut Proc`/`&mut Priv` is held here.
    let vm_priv_id = {
        let table = unsafe { proc_table_ref() };
        let idx = proc_index(VM_PROC_NR).expect("VM in proc table");
        table[idx].priv_id.expect("VM priv populated by proc::init")
    };

    // Stub D's own priv slot: SENDREC to VM, nothing else.
    // SAFETY: priv index in-range; no overlapping reference held.
    {
        let pr: &mut Priv =
            unsafe { priv_slot_mut(STUB_D_PRIV_ID).expect("stub D priv slot in range") };
        pr.id = STUB_D_PRIV_ID;
        pr.proc_nr = Some(STUB_D_PROC_NR);
        pr.flags = SYS_PROC | BILLABLE | PREEMPTIBLE;
        pr.trap_mask = USR_T;
        pr.ipc_to.fill(0);
        set_sys_bit(&mut pr.ipc_to, vm_priv_id);
        pr.k_call_mask.fill(0);
        pr.notify_pending.fill(0);
        pr.asyn_pending.fill(0);
        pr.sig_mgr = boot_endpoint(RS_PROC_NR);
    }

    // Open VM → D so VM can reply to D's SENDREC. Separate borrow: D's `&mut
    // Priv` above has been dropped.
    // SAFETY: priv index in-range; no overlapping reference held.
    {
        let vm_pr: &mut Priv = unsafe { priv_slot_mut(vm_priv_id).expect("VM priv slot in range") };
        set_sys_bit(&mut vm_pr.ipc_to, STUB_D_PRIV_ID);
    }
}

/// Set the per-stub priv slot. `peer_priv_id` is the only target the
/// stub is permitted to send/notify (encoded as a single bit in
/// `ipc_to`).
///
/// SAFETY: single-threaded boot; mutates only the priv slot at `id`.
unsafe fn install_one_stub_priv(id: PrivId, owner: ProcNr, peer_priv_id: PrivId) {
    // SAFETY: priv index in-range; no overlapping reference held.
    let pr: &mut Priv = unsafe { priv_slot_mut(id).expect("stub priv slot in range") };
    pr.id = id;
    pr.proc_nr = Some(owner);
    pr.flags = SYS_PROC | BILLABLE | PREEMPTIBLE;
    pr.trap_mask = SRV_T;
    pr.ipc_to.fill(0);
    set_sys_bit(&mut pr.ipc_to, peer_priv_id);
    pr.k_call_mask.fill(0);
    pr.notify_pending.fill(0);
    pr.asyn_pending.fill(0);
    pr.sig_mgr = boot_endpoint(RS_PROC_NR);
}

/// Print one line per stub showing its `(ttbr0_pa, asid)`. Run once at
/// the end of `userland_bootstrap`; proves each stub has a distinct
/// per-proc address space.
///
/// SAFETY: single-threaded boot; read-only borrows on proc slots 11..=14.
unsafe fn print_addrspace_summary() {
    use crate::arch::aarch64::uart::Pl011;
    use core::fmt::Write;
    let mut uart = Pl011::new();
    let _ = writeln!(uart);
    for &nr in &[
        STUB_A_PROC_NR,
        STUB_B_PROC_NR,
        STUB_C_PROC_NR,
        STUB_D_PROC_NR,
    ] {
        // SAFETY: sequential read-only borrow of the slot; no other
        // reference held while we read.
        let p = unsafe { proc_slot_mut(nr).expect("stub slot in range") };
        let _ = writeln!(
            uart,
            "[as] stub {} nr={} ttbr0_pa={:#x} asid={}",
            p.name[0] as char,
            p.nr.get(),
            p.ttbr0_pa,
            p.asid,
        );
    }
}

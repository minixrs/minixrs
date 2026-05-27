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
    // address-space API (`arch::aarch64::addrspace::AddrSpace`).
    // SAFETY: single-threaded boot; this is the only writer of HHDM_OFFSET
    // and the only initializer of the allocator.
    unsafe {
        mm::set_hhdm_offset(hhdm);
        mm::init_from_limine_memmap();
    }

    // Slice 3.1a smoke test: build a throwaway AddrSpace, install 4 distinct
    // mappings, walk each back to its PA, then destroy. Verifies that the
    // frame allocator, intermediate-table allocation, walk_pt, and the
    // free-list-after-destroy path all work end-to-end. Removed in 3.1b
    // once the real per-proc AddrSpaces are in place.
    mm_smoke_test(&mut con);

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

/// One-shot kernel-side test of the slice-3.1a memory APIs. Allocates a
/// throwaway [`arch::aarch64::addrspace::AddrSpace`], installs four 4 KiB
/// mappings at distinct user VAs (each backed by a fresh frame), walks
/// every mapping to verify the VA→PA translation matches what we
/// installed, then tears the AddrSpace down. Frees go onto the free list,
/// so a follow-up alloc returns one of them — that's the final check.
///
/// Prints `[mm] frame_alloc OK / map OK / walk OK / free OK` on success;
/// panics with a precise message on any inconsistency. The smoke test is
/// removed in slice 3.1b once per-proc AddrSpaces replace the static
/// `userland.rs` page tables.
#[cfg(target_os = "none")]
fn mm_smoke_test(con: &mut uart::Uart) {
    use arch::aarch64::addrspace::{AddrSpace, Prot};

    let _ = writeln!(con, "\n[mm] smoke test starting");

    // Track the four (VA, PA) pairs we install so we can re-verify after
    // re-walking.
    let mut mappings: [(u64, u64); 4] = [(0, 0); 4];

    let mut aspace = AddrSpace::new().expect("AddrSpace::new failed (no L0 frame)");
    let l0_pa = aspace.ttbr0_pa;
    let _ = writeln!(con, "[mm] frame_alloc OK ttbr0_pa={l0_pa:#x}");

    // Four distinct user VAs in three different L2 slots, so the walker is
    // exercised across multiple intermediate-table allocations.
    let test_vas: [u64; 4] = [
        0x0000_0000_0080_0000,
        0x0000_0000_0090_0000,
        0x0000_0001_0000_0000,
        0x0000_0040_0000_0000,
    ];

    for (i, &va) in test_vas.iter().enumerate() {
        let frame = mm::alloc_frame().expect("smoke: alloc_frame ran out");
        let pa = frame.addr();
        aspace
            .map_page(va, pa, Prot::RW_DATA)
            .unwrap_or_else(|e| panic!("map_page({va:#x}) failed: {e:?}"));
        mappings[i] = (va, pa);
    }
    let _ = writeln!(con, "[mm] map OK (4 mappings installed)");

    for (va, expected_pa) in mappings.iter() {
        let got = aspace
            .walk_pt(*va)
            .unwrap_or_else(|| panic!("walk_pt({va:#x}) returned None"));
        assert!(
            got == *expected_pa,
            "walk_pt({va:#x}) returned {got:#x}, expected {expected_pa:#x}",
        );
    }
    // Negative case: walking an unmapped VA must return None.
    let unmapped = 0x0000_0000_0000_8000;
    assert!(
        aspace.walk_pt(unmapped).is_none(),
        "walk_pt({unmapped:#x}) returned Some for an unmapped VA"
    );
    let _ = writeln!(con, "[mm] walk OK (all 4 mappings + 1 unmapped check)");

    // Leaf frames live outside the AddrSpace's ownership — free them
    // explicitly (the AddrSpace only owns the L0/L1/L2/L3 tree).
    for (_va, pa) in mappings.iter() {
        mm::free_frame(mm::Frame::from_addr(*pa));
    }

    aspace.destroy();

    // `destroy()` frees the L0 root last; the free list is LIFO so the
    // next `alloc_frame` must return that same PA. Confirms the
    // free-list path is hot (rather than alloc still bumping fresh
    // frames out of the region).
    let reused = mm::alloc_frame().expect("smoke: no frame returned after free");
    let reused_pa = reused.addr();
    assert!(
        reused_pa == l0_pa,
        "smoke: expected free-list reuse of L0 PA {l0_pa:#x}, got {reused_pa:#x}",
    );
    // Return it so the free list inherits a non-empty state for the next
    // slice; the bump regions are never repopulated by `free_frame`, so
    // the frame would otherwise sit idle outside the allocator's reach.
    mm::free_frame(reused);

    let _ = writeln!(con, "[mm] free OK (destroy + reuse verified)");
}

#[cfg(not(target_os = "none"))]
fn main() {}

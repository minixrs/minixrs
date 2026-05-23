//! Slice-2.4 userland bootstrap: minimal TTBR0 setup + two EL0 stub tasks.
//!
//! Builds a single 4-level page-table walk in static `.bss` storage that
//! maps four user pages, two per stub task:
//!   - `USER_CODE_VA_A` / `USER_CODE_VA_B` → a single shared kernel-owned
//!     page filled with the EL0 stub instructions
//!     (`_user_stub_start..._user_stub_end`). RO + EL0 X.
//!   - `USER_STACK_VA_A` / `USER_STACK_VA_B` → two kernel-owned pages used
//!     as each stub's EL0 stack. RW + EL0, no execute.
//!
//! Both stubs share the same code blob — they are distinguishable only by
//! `Proc::name` (the clock tick handler prints `name[0]` per tick to
//! produce the interleaved A/B output that demonstrates preemption).
//!
//! After populating each stub's process slot, the bootstrap enqueues both
//! into the priority-banded run queue (`proc::sched::enqueue`). The caller
//! (`kmain`) then transfers control to the scheduler via `proc::sched::run`.
//!
//! Phase 3's VM server replaces this entire file with proper per-process
//! address spaces, ELF loading, and copy-on-write semantics.

use core::cell::UnsafeCell;
use core::sync::atomic::Ordering;

use minix4_kernel_shared::ProcNr;
use minix4_kernel_shared::com::boot_endpoint;

use crate::arch::aarch64::limine::kernel_va_to_pa;
use crate::arch::aarch64::mmu::{
    self, ATTR_IDX_NORMAL, PAGE_SIZE, PTE_AF, PTE_AP_RO_EL0, PTE_AP_RW_EL0, PTE_PXN,
    PTE_SH_INNER, PTE_UXN, PageTable, pte_attr_idx,
};
use crate::proc::Proc;
use crate::proc::sched;
use crate::proc::table::proc_slot_mut;

// ----- EL0 virtual addresses ------------------------------------------------

/// VA at which stub A's code page is mapped.
pub const USER_CODE_VA_A: u64 = 0x0040_0000; // 4 MiB
/// VA at which stub A's stack page is mapped.
pub const USER_STACK_VA_A: u64 = 0x0080_0000; // 8 MiB

/// VA at which stub B's code page is mapped (one 4 KiB page above A).
/// Same 2 MiB L2 slot as A, so the L3_CODE table is shared.
pub const USER_CODE_VA_B: u64 = 0x0041_0000;
/// VA at which stub B's stack page is mapped. Same 2 MiB L2 slot as A's
/// stack, so the L3_STACK table is shared.
pub const USER_STACK_VA_B: u64 = 0x0081_0000;

/// First proc-table slot beyond the boot image — `ProcNr(11)`. Boot procs
/// occupy `0..=INIT_PROC_NR` (i.e. `0..=10`).
const STUB_A_PROC_NR: ProcNr = ProcNr::new(11);
/// Second stub slot, just after A.
const STUB_B_PROC_NR: ProcNr = ProcNr::new(12);

// SPSR_EL1 to install on each stub's `eret`. Compared to slice 2.3's
// `0x3C0` (all four DAIF bits masked), we clear bit 7 (`I`) so that IRQs
// are unmasked at EL0 — that's the whole point of slice 2.4. Bits set:
//   - bit 9 (D) = 1     // debug mask
//   - bit 8 (A) = 1     // SError mask
//   - bit 7 (I) = 0     // IRQs ENABLED at EL0
//   - bit 6 (F) = 1     // FIQ mask (we don't handle FIQ in slice 2.4)
//   - bits 3:0 (M)     = 0 // EL0t
const STUB_SPSR_EL0: u64 = 0x340;

// ----- Static storage ------------------------------------------------------

/// 4 KiB-aligned raw byte page, used for the EL0 stubs' code and stacks.
#[repr(C, align(4096))]
struct UserPage(UnsafeCell<[u8; PAGE_SIZE]>);
// SAFETY: single-threaded boot context; no concurrent access. Same invariant
// documented on `ProcStorage` / `PrivStorage` in `proc::table`.
unsafe impl Sync for UserPage {}

impl UserPage {
    const EMPTY: Self = Self(UnsafeCell::new([0; PAGE_SIZE]));
}

#[repr(transparent)]
struct PtSlot(UnsafeCell<PageTable>);
// SAFETY: single-threaded boot; mutation is confined to `userland_bootstrap`.
unsafe impl Sync for PtSlot {}

static L0_TABLE: PtSlot = PtSlot(UnsafeCell::new(PageTable::EMPTY));
static L1_TABLE: PtSlot = PtSlot(UnsafeCell::new(PageTable::EMPTY));
static L2_TABLE: PtSlot = PtSlot(UnsafeCell::new(PageTable::EMPTY));
static L3_CODE_TABLE: PtSlot = PtSlot(UnsafeCell::new(PageTable::EMPTY));
static L3_STACK_TABLE: PtSlot = PtSlot(UnsafeCell::new(PageTable::EMPTY));

/// Single shared code page — both stubs execute the same instructions.
static USER_CODE_PAGE: UserPage = UserPage::EMPTY;
/// Per-task stack pages.
static USER_STACK_PAGE_A: UserPage = UserPage::EMPTY;
static USER_STACK_PAGE_B: UserPage = UserPage::EMPTY;

// ----- EL0 stub blob (linked into the kernel image by `user_stub.S`) -------

unsafe extern "C" {
    static _user_stub_start: u8;
    static _user_stub_end: u8;
}

// ----- Bootstrap ----------------------------------------------------------

/// Wire up TTBR0 mappings, copy the EL0 stub into its shared code page,
/// populate both stub proc slots, and enqueue them on the run queue.
///
/// SAFETY: must be called exactly once from `kmain` before any EL0 code
/// runs. Single-threaded boot context; no other reference into the static
/// page-table arena, user pages, or `PROC_TABLE[STUB_*_PROC_NR]` may exist
/// concurrently.
pub unsafe fn userland_bootstrap() {
    // 1. Verify Limine left MAIR_EL1's index 0 as Normal WB and that
    //    TCR_EL1's TTBR0-side fields match what `activate_user_ttbr0`
    //    expects.
    mmu::assert_mair_normal_wb();
    mmu::assert_tcr_el1_ttbr0_ready();

    // 2. Resolve physical addresses for our static storage.
    let l0_pa = kernel_pa_of(L0_TABLE.0.get() as u64);
    let l1_pa = kernel_pa_of(L1_TABLE.0.get() as u64);
    let l2_pa = kernel_pa_of(L2_TABLE.0.get() as u64);
    let l3_code_pa = kernel_pa_of(L3_CODE_TABLE.0.get() as u64);
    let l3_stack_pa = kernel_pa_of(L3_STACK_TABLE.0.get() as u64);
    let code_page_pa = kernel_pa_of(USER_CODE_PAGE.0.get() as u64);
    let stack_a_pa = kernel_pa_of(USER_STACK_PAGE_A.0.get() as u64);
    let stack_b_pa = kernel_pa_of(USER_STACK_PAGE_B.0.get() as u64);

    // 3. Build the user mappings. Both code VAs share the same physical
    //    code page; stacks are per-task. The four VAs split into two L2
    //    slots (one for the 0x40_xxxx code pair, one for the 0x80_xxxx
    //    stack pair), each backed by a single L3 table.
    let code_attrs =
        PTE_AF | PTE_SH_INNER | PTE_AP_RO_EL0 | PTE_PXN | pte_attr_idx(ATTR_IDX_NORMAL);
    let stack_attrs = PTE_AF
        | PTE_SH_INNER
        | PTE_AP_RW_EL0
        | PTE_PXN
        | PTE_UXN
        | pte_attr_idx(ATTR_IDX_NORMAL);

    // SAFETY: single-threaded boot; no other references into the table
    // storage exist. PAs were resolved above.
    unsafe {
        let l0 = &mut *L0_TABLE.0.get();
        let l1 = &mut *L1_TABLE.0.get();
        let l2 = &mut *L2_TABLE.0.get();
        let l3_code = &mut *L3_CODE_TABLE.0.get();
        let l3_stack = &mut *L3_STACK_TABLE.0.get();

        mmu::map_4k(
            l0, l1, l2, l3_code,
            l1_pa, l2_pa, l3_code_pa,
            USER_CODE_VA_A, code_page_pa, code_attrs,
        );
        mmu::map_4k(
            l0, l1, l2, l3_code,
            l1_pa, l2_pa, l3_code_pa,
            USER_CODE_VA_B, code_page_pa, code_attrs,
        );
        mmu::map_4k(
            l0, l1, l2, l3_stack,
            l1_pa, l2_pa, l3_stack_pa,
            USER_STACK_VA_A, stack_a_pa, stack_attrs,
        );
        mmu::map_4k(
            l0, l1, l2, l3_stack,
            l1_pa, l2_pa, l3_stack_pa,
            USER_STACK_VA_B, stack_b_pa, stack_attrs,
        );
    }

    // 4. Copy the EL0 stub into its (shared) code page; clean+invalidate
    //    so the upcoming EL0 fetches see the new bytes.
    // SAFETY: `_user_stub_*` are rodata symbols inside the kernel image;
    // start ≤ end is enforced by the linker output of user_stub.S.
    let stub_len = unsafe {
        (&_user_stub_end as *const u8).offset_from(&_user_stub_start as *const u8) as usize
    };
    assert!(stub_len > 0 && stub_len <= PAGE_SIZE);
    // SAFETY: USER_CODE_PAGE is a 4 KiB-aligned BSS page; we hold the only
    // mutable reference (single-threaded boot).
    unsafe {
        let dst = USER_CODE_PAGE.0.get() as *mut u8;
        core::ptr::copy_nonoverlapping(&_user_stub_start as *const u8, dst, stub_len);
        mmu::flush_icache_range(USER_CODE_PAGE.0.get() as u64, stub_len);
    }

    // 5. Install TTBR0 with our L0 root.
    // SAFETY: DAIF is still masked from boot.
    unsafe { mmu::activate_user_ttbr0(l0_pa) };

    // 6. Populate each stub's proc slot. Borrows are sequential, never
    //    overlapping. We rely on `proc::init` (slice 2.2) having left
    //    slots 11 and 12 in the RTS_SLOT_FREE state.
    // SAFETY: single-threaded boot; sequential mutable borrows.
    unsafe {
        let pa = proc_slot_mut(STUB_A_PROC_NR).expect("STUB_A_PROC_NR within table");
        populate_stub_slot(
            pa,
            STUB_A_PROC_NR,
            b'A',
            USER_CODE_VA_A,
            USER_STACK_VA_A + PAGE_SIZE as u64,
        );
    }
    // SAFETY: same — A's borrow has been dropped before we take B.
    unsafe {
        let pb = proc_slot_mut(STUB_B_PROC_NR).expect("STUB_B_PROC_NR within table");
        populate_stub_slot(
            pb,
            STUB_B_PROC_NR,
            b'B',
            USER_CODE_VA_B,
            USER_STACK_VA_B + PAGE_SIZE as u64,
        );
    }

    // 7. Enqueue both on the scheduler. They run in FIFO order within the
    //    same priority band (SRV_Q); the timer will preempt A after one
    //    quantum and B will get its first slice.
    // SAFETY: single-threaded boot; sched module's invariants documented
    // on `enqueue`. No other PROC_TABLE / RUNQ borrows live.
    unsafe {
        sched::enqueue(STUB_A_PROC_NR);
        sched::enqueue(STUB_B_PROC_NR);
    }
}

fn populate_stub_slot(p: &mut Proc, nr: ProcNr, id: u8, entry_va: u64, stack_top: u64) {
    // Name: `id` followed by "stub\0..." padding. Lets the clock tick
    // handler print `name[0]` to identify which stub is running, while
    // `dump_tables` shows a recognizable per-stub string.
    p.name = [0; 16];
    p.name[0] = id;
    p.name[1] = b'-';
    p.name[2] = b's';
    p.name[3] = b't';
    p.name[4] = b'u';
    p.name[5] = b'b';

    p.nr = nr;
    p.endpoint = boot_endpoint(nr);
    p.priv_id = None; // No Priv slot — trap-mask enforcement is slice 2.5.
    p.priority = crate::proc::table::SRV_Q;
    // 5 ticks per quantum = 50 ms at 100 Hz. Short enough to see frequent
    // A/B switches in the boot demo, long enough that the do_ipc log lines
    // (one per SVC) still get a few in per burst.
    p.quantum_ms = 5;
    p.quantum_left = p.quantum_ms as u64;

    // EL0 entry state.
    p.regs.elr_el1 = entry_va;
    p.regs.sp_el0 = stack_top;
    p.regs.spsr_el1 = STUB_SPSR_EL0;

    // Run-queue link starts cleared — `sched::enqueue` will set it.
    p.next_ready = None;

    // Mark runnable.
    p.rts_flags.store(0, Ordering::Relaxed);
}

/// Translate a kernel-image VA to PA via Limine's kernel-address response.
fn kernel_pa_of(va: u64) -> u64 {
    kernel_va_to_pa(va).expect(
        "Limine did not populate the kernel-address response — bootloader too old?",
    )
}

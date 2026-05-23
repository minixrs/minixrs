//! Slice-2.3 userland bootstrap: minimal TTBR0 setup + EL0 stub task.
//!
//! Builds a single 4-level page-table walk in static `.bss` storage that
//! maps two pages:
//!   - `USER_CODE_VA` → a kernel-owned page filled with the EL0 stub
//!     instructions (`_user_stub_start..._user_stub_end`). RO + EL0 X.
//!   - `USER_STACK_VA` → a kernel-owned page used as the stub's EL0 stack.
//!     RW + EL0, no execute.
//!
//! Populates the first free user slot in the process table (ProcNr 11 — the
//! slot immediately after the boot servers, currently empty) with the EL0
//! entry state, and returns it for `proc::sched::switch_to_user` to consume.
//!
//! Phase 3's VM server replaces all of this with proper per-process address
//! spaces; slice 2.4 reuses the page-table arena to add a second EL0 stub
//! task that exercises timer-driven preemption.

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
use crate::proc::table::proc_slot_mut;

// ----- EL0 virtual addresses ------------------------------------------------

/// Virtual address at which the EL0 stub's code page is mapped.
pub const USER_CODE_VA: u64 = 0x0040_0000; // 4 MiB
/// Virtual address at which the EL0 stub's stack page is mapped.
pub const USER_STACK_VA: u64 = 0x0080_0000; // 8 MiB

/// First proc-table slot beyond the boot image. Boot procs occupy
/// `0..=INIT_PROC_NR` (i.e. `0..=10`), so `ProcNr(11)` is the first free
/// user slot. Slice 2.6 will replace this ad-hoc occupant with a
/// properly-loaded INIT.
const STUB_PROC_NR: ProcNr = ProcNr::new(11);

// ----- Static storage ------------------------------------------------------

/// 4 KiB-aligned raw byte page, used for the EL0 stub's code and stack.
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

static USER_CODE_PAGE: UserPage = UserPage::EMPTY;
static USER_STACK_PAGE: UserPage = UserPage::EMPTY;

// ----- EL0 stub blob (linked into the kernel image by `user_stub.S`) -------

unsafe extern "C" {
    static _user_stub_start: u8;
    static _user_stub_end: u8;
}

// ----- Bootstrap ----------------------------------------------------------

/// Wire up TTBR0 mappings, copy the EL0 stub into its user page, populate
/// the stub's process slot, and return it.
///
/// SAFETY: must be called exactly once from `kmain` before any EL0 code
/// runs. Single-threaded boot context; no other reference into the static
/// page-table arena, user pages, or `PROC_TABLE[STUB_PROC_NR]` may exist
/// concurrently.
pub unsafe fn userland_bootstrap() -> &'static mut Proc {
    // 1. Verify Limine left MAIR_EL1's index 0 as Normal WB and that
    //    TCR_EL1's TTBR0-side fields match what `activate_user_ttbr0`
    //    expects. We never rewrite either register — these asserts are the
    //    bootstrap's only guard against a Limine config drift.
    mmu::assert_mair_normal_wb();
    mmu::assert_tcr_el1_ttbr0_ready();

    // 2. Resolve physical addresses for our static storage. The page tables
    //    and the user code/stack pages all live in the kernel image (`.bss`
    //    section, mapped via TTBR1), so we go through Limine's kernel-
    //    address response rather than HHDM.
    let l0_pa = kernel_pa_of(L0_TABLE.0.get() as u64);
    let l1_pa = kernel_pa_of(L1_TABLE.0.get() as u64);
    let l2_pa = kernel_pa_of(L2_TABLE.0.get() as u64);
    let l3_code_pa = kernel_pa_of(L3_CODE_TABLE.0.get() as u64);
    let l3_stack_pa = kernel_pa_of(L3_STACK_TABLE.0.get() as u64);
    let code_page_pa = kernel_pa_of(USER_CODE_PAGE.0.get() as u64);
    let stack_page_pa = kernel_pa_of(USER_STACK_PAGE.0.get() as u64);

    // 3. Build the two user mappings. The arena holds 5 tables total: L0,
    //    L1, L2 (shared because both VAs fall in the same 1 GiB L1 entry),
    //    and one L3 each (the code/stack VAs straddle different 2 MiB L2
    //    slots, so they need separate L3 tables).
    let code_attrs = PTE_AF | PTE_SH_INNER | PTE_AP_RO_EL0 | PTE_PXN | pte_attr_idx(ATTR_IDX_NORMAL);
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
            USER_CODE_VA, code_page_pa, code_attrs,
        );
        mmu::map_4k(
            l0, l1, l2, l3_stack,
            l1_pa, l2_pa, l3_stack_pa,
            USER_STACK_VA, stack_page_pa, stack_attrs,
        );
    }

    // 4. Copy the EL0 stub into its code page (via the kernel VA), then
    //    clean the data side to PoU and invalidate the i-cache so the
    //    upcoming EL0 fetch sees the new bytes.
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

    // 5. Install TTBR0 with our L0 root. After this, USER_CODE_VA and
    //    USER_STACK_VA resolve to the pages we just prepared.
    // SAFETY: DAIF is masked from boot (Limine hands us a state with all
    //   four DAIF bits set). The L0 root we just built is valid.
    unsafe { mmu::activate_user_ttbr0(l0_pa) };

    // 6. Populate the stub's proc slot with its EL0 entry state.
    // SAFETY: single-threaded boot; nobody else holds a reference into the
    // stub slot. Slice 2.2's `proc::init` left ProcNr 11 in the
    // RTS_SLOT_FREE state.
    let p = unsafe { proc_slot_mut(STUB_PROC_NR) }
        .expect("STUB_PROC_NR within proc table");
    populate_stub_slot(p);
    p
}

fn populate_stub_slot(p: &mut Proc) {
    // Name "el0stub" + NUL fill.
    p.name = *b"el0stub\0\0\0\0\0\0\0\0\0";
    p.nr = STUB_PROC_NR;
    p.endpoint = boot_endpoint(STUB_PROC_NR);
    p.priv_id = None; // No Priv slot — trap-mask enforcement is slice 2.5.
    p.priority = crate::proc::table::SRV_Q;
    p.quantum_ms = 0;
    p.quantum_left = 0;

    // EL0 entry state.
    p.regs.elr_el1 = USER_CODE_VA;
    p.regs.sp_el0 = USER_STACK_VA + PAGE_SIZE as u64;
    // SPSR_EL1 = 0x3C0:
    //   bits 9:6  (DAIF) = 1111 — keep interrupts masked until slice 2.4.
    //   bit  4    (M[4]) = 0    — AArch64.
    //   bits 3:0  (M[3:0]) = 0  — EL0t (SP = SP_EL0).
    p.regs.spsr_el1 = 0x3C0;

    // Mark runnable — RTS_SLOT_FREE was set by Proc::EMPTY; clearing all
    // RTS bits makes the slot a live, ready process.
    p.rts_flags.store(0, Ordering::Relaxed);
}

/// Translate a kernel-image VA to PA via Limine's kernel-address response,
/// panicking with a usable message if Limine didn't fill that response in.
fn kernel_pa_of(va: u64) -> u64 {
    kernel_va_to_pa(va).expect(
        "Limine did not populate the kernel-address response — bootloader too old?",
    )
}

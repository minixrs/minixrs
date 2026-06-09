//! Per-process memory regions tracked by the VM server (slice 3.5).
//!
//! VM is the user-space authority on what virtual addresses a process may
//! touch. Before slice 3.5 it resolved *every* page fault by blindly mapping a
//! fresh page; now it consults a per-process region table so that only faults
//! inside a known region (today: the heap) are satisfied. Out-of-region faults
//! are a SIGSEGV (handled by the caller in `main.rs`).
//!
//! The table is a static `[ClientRegions; MAX_CLIENTS]` indexed by process
//! number. This mirrors the kernel's `PROC_TABLE` convention: an
//! `UnsafeCell<[T; N]>` inside a `#[repr(transparent)]` newtype with
//! `unsafe impl Sync`. The single-mutator invariant is even simpler here than
//! in the kernel — VM is a single EL0 thread with no interrupt handlers of its
//! own (IRQs trap into the kernel, never into VM), so the table is only ever
//! touched from VM's straight-line receive loop.
//!
//! `MAX_CLIENTS = 16` covers every boot process (proc numbers `0..=15`,
//! including stub D at `14`). Phase 4's `fork` will churn proc numbers past
//! this cap and revisit the keying; a 1024-slot table (`NR_PROCS`) would burn
//! ~512 KiB of BSS — 128 frames the ELF loader would map at boot — for no
//! benefit while only the boot stubs exist.

use core::cell::UnsafeCell;

use minix4_kernel_shared::error::EINVAL;

/// Fixed heap origin. Until PM supplies a real per-process memory layout
/// (Phase 4), VM and stub D agree on this VA by convention: `brk` grows the
/// heap as `[HEAP_BASE, new_break)` and stub D writes inside that range.
pub const HEAP_BASE: u64 = 0x0100_0000;

/// aarch64 4 KiB page.
const PAGE_SIZE: u64 = 4096;

/// Proc-number range the table can key. Boot procs are `0..=15`.
const MAX_CLIENTS: usize = 16;

/// Regions tracked per process. One heap today; `mmap` regions (slice 3.6)
/// reuse the spare slots.
const MAX_REGIONS: usize = 4;

/// What a region is for. `Unused` marks a free slot.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Kind {
    Unused,
    Heap,
}

/// A half-open virtual-address range `[start, end)` and what it backs.
#[derive(Copy, Clone)]
struct Region {
    start: u64,
    end: u64,
    kind: Kind,
}

impl Region {
    const EMPTY: Self = Self { start: 0, end: 0, kind: Kind::Unused };

    fn contains(&self, addr: u64) -> bool {
        self.kind != Kind::Unused && addr >= self.start && addr < self.end
    }
}

/// One process's region set.
#[derive(Copy, Clone)]
struct ClientRegions {
    regions: [Region; MAX_REGIONS],
}

impl ClientRegions {
    const EMPTY: Self = Self { regions: [Region::EMPTY; MAX_REGIONS] };

    /// True if `addr` falls inside one of this client's regions.
    fn contains(&self, addr: u64) -> bool {
        self.regions.iter().any(|r| r.contains(addr))
    }

    /// Set the program break to `new_break`, growing or creating the heap
    /// region as `[HEAP_BASE, page_align_up(new_break))`. Returns the resulting
    /// break, or `EINVAL` if `new_break` is below `HEAP_BASE`, the page-aligned
    /// break would overflow `u64`, or no region slot is free for a new heap.
    fn set_brk(&mut self, new_break: u64) -> Result<u64, i32> {
        if new_break < HEAP_BASE {
            return Err(EINVAL);
        }
        // page_align_up, guarding the round-up add against wraparound near
        // `u64::MAX` (a silent wrap would yield a tiny `end` in `--release`).
        let end = new_break
            .checked_add(PAGE_SIZE - 1)
            .map(|v| v & !(PAGE_SIZE - 1))
            .ok_or(EINVAL)?;

        // Grow the existing heap region if present.
        for r in self.regions.iter_mut() {
            if r.kind == Kind::Heap {
                r.end = end;
                return Ok(end);
            }
        }
        // Otherwise claim a free slot for a fresh heap region.
        for r in self.regions.iter_mut() {
            if r.kind == Kind::Unused {
                *r = Region { start: HEAP_BASE, end, kind: Kind::Heap };
                return Ok(end);
            }
        }
        Err(EINVAL)
    }
}

/// `UnsafeCell`-wrapped static table. See the module-level note for the
/// single-mutator invariant that makes the `Sync` impl sound.
#[repr(transparent)]
struct RegionTable(UnsafeCell<[ClientRegions; MAX_CLIENTS]>);

// SAFETY: VM is a single-threaded EL0 process with no interrupt handlers of
// its own; the table is only ever accessed from VM's straight-line receive
// loop, so there is never concurrent access.
unsafe impl Sync for RegionTable {}

static TABLE: RegionTable =
    RegionTable(UnsafeCell::new([ClientRegions::EMPTY; MAX_CLIENTS]));

/// In-range proc number → table index, or `None` if `nr` is a kernel task
/// (negative) or past the boot cap.
fn client_idx(nr: i32) -> Option<usize> {
    let idx = usize::try_from(nr).ok()?;
    (idx < MAX_CLIENTS).then_some(idx)
}

/// Borrow the client's region set immutably (read path).
fn client_ref(nr: i32) -> Option<&'static ClientRegions> {
    let idx = client_idx(nr)?;
    // SAFETY: single-mutator invariant (module note); shared read, `idx` in
    // range. No `&mut` to the table is live during VM's straight-line loop.
    let table = unsafe { &*TABLE.0.get() };
    Some(&table[idx])
}

/// Borrow the client's region set mutably (write path).
fn client_mut(nr: i32) -> Option<&'static mut ClientRegions> {
    let idx = client_idx(nr)?;
    // SAFETY: single-mutator invariant (module note); `idx < MAX_CLIENTS`.
    let table = unsafe { &mut *TABLE.0.get() };
    Some(&mut table[idx])
}

/// True if `addr` falls inside one of `nr`'s regions. The VM fault path only
/// satisfies faults for which this returns true.
pub fn contains(nr: i32, addr: u64) -> bool {
    client_ref(nr).is_some_and(|client| client.contains(addr))
}

/// Set process `nr`'s program break to `new_break`, growing or creating its
/// heap region as `[HEAP_BASE, page_align_up(new_break))`. Returns the
/// resulting break on success, or `EINVAL` if `nr` is untrackable or
/// `new_break` is below `HEAP_BASE`.
///
/// No frames are mapped here — pages fault in lazily on first touch and are
/// resolved through [`contains`] in the fault path.
pub fn set_brk(nr: i32, new_break: u64) -> Result<u64, i32> {
    client_mut(nr).ok_or(EINVAL)?.set_brk(new_break)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_brk_creates_heap_region() {
        let mut c = ClientRegions::EMPTY;
        let brk = c.set_brk(HEAP_BASE + 0x4000).unwrap();
        assert_eq!(brk, HEAP_BASE + 0x4000);
        assert!(c.contains(HEAP_BASE));
        assert!(c.contains(HEAP_BASE + 0x3FFF));
        // Half-open: the break itself is the first byte *past* the region.
        assert!(!c.contains(HEAP_BASE + 0x4000));
        assert!(!c.contains(HEAP_BASE - 1));
    }

    #[test]
    fn set_brk_grows_existing_heap_in_place() {
        let mut c = ClientRegions::EMPTY;
        c.set_brk(HEAP_BASE + 0x4000).unwrap();
        let brk = c.set_brk(HEAP_BASE + 0x8000).unwrap();
        assert_eq!(brk, HEAP_BASE + 0x8000);
        // Still exactly one heap region, now covering the grown range.
        let heaps = c.regions.iter().filter(|r| r.kind == Kind::Heap).count();
        assert_eq!(heaps, 1);
        assert!(c.contains(HEAP_BASE + 0x4000));
        assert!(c.contains(HEAP_BASE + 0x7FFF));
        assert!(!c.contains(HEAP_BASE + 0x8000));
    }

    #[test]
    fn set_brk_shrinks_heap() {
        let mut c = ClientRegions::EMPTY;
        c.set_brk(HEAP_BASE + 0x8000).unwrap();
        let brk = c.set_brk(HEAP_BASE + 0x4000).unwrap();
        assert_eq!(brk, HEAP_BASE + 0x4000);
        assert!(c.contains(HEAP_BASE + 0x3FFF));
        assert!(!c.contains(HEAP_BASE + 0x4000)); // shrunk away
    }

    #[test]
    fn set_brk_rounds_break_up_to_page() {
        let mut c = ClientRegions::EMPTY;
        let brk = c.set_brk(HEAP_BASE + 1).unwrap();
        assert_eq!(brk, HEAP_BASE + PAGE_SIZE);
        assert!(c.contains(HEAP_BASE + PAGE_SIZE - 1));
        assert!(!c.contains(HEAP_BASE + PAGE_SIZE));
    }

    #[test]
    fn set_brk_page_aligned_break_is_unchanged() {
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.set_brk(HEAP_BASE + PAGE_SIZE), Ok(HEAP_BASE + PAGE_SIZE));
    }

    #[test]
    fn set_brk_below_heap_base_is_einval() {
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.set_brk(HEAP_BASE - 1), Err(EINVAL));
        assert_eq!(c.set_brk(0), Err(EINVAL));
    }

    #[test]
    fn set_brk_overflow_is_einval_not_wrap() {
        let mut c = ClientRegions::EMPTY;
        // Without the checked_add guard, page_align_up wraps to a tiny `end`.
        assert_eq!(c.set_brk(u64::MAX), Err(EINVAL));
        assert_eq!(c.set_brk(u64::MAX - (PAGE_SIZE - 2)), Err(EINVAL));
        // The largest break that still aligns without overflow succeeds.
        let max_ok = u64::MAX & !(PAGE_SIZE - 1);
        assert_eq!(c.set_brk(max_ok), Ok(max_ok));
    }

    #[test]
    fn contains_is_false_for_untracked_client() {
        // No region set has been created for proc 14 in the global table.
        assert!(!contains(14, HEAP_BASE));
    }

    #[test]
    fn contains_rejects_out_of_range_proc_numbers() {
        assert!(!contains(-1, HEAP_BASE)); // kernel task
        assert!(!contains(MAX_CLIENTS as i32, HEAP_BASE)); // past the cap
    }

    #[test]
    fn empty_client_contains_nothing() {
        let c = ClientRegions::EMPTY;
        assert!(!c.contains(HEAP_BASE));
        assert!(!c.contains(0));
    }
}

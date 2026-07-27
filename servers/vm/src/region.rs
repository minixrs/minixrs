// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
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
//! `MAX_CLIENTS` is sized from the shared [`NR_SERVED_PROCS`] ceiling so the
//! table covers the whole proc-number range the user-space servers track: the
//! boot procs and stubs `0..15` plus PM's fork pool `[15, NR_SERVED_PROCS)`
//! (`VM_FORK` records a child's inherited regions keyed by its proc number). A
//! full `NR_PROCS`-slot table would burn hundreds of KiB of BSS the ELF loader
//! must map at boot; the shared ceiling keeps it right-sized and in lockstep
//! with PM's mproc table.

use core::cell::UnsafeCell;

use minixrs_kernel_shared::com::NR_SERVED_PROCS;
use minixrs_kernel_shared::error::{EINVAL, ENOMEM};
use minixrs_kernel_shared::uspace::{RAMDISK_WINDOW_BASE, USER_DEVICE_WINDOW_BASE};

/// Fixed heap origin. Until PM supplies a real per-process memory layout
/// (Phase 4), VM and stub D agree on this VA by convention: `brk` grows the
/// heap as `[HEAP_BASE, new_break)` and stub D writes inside that range.
pub const HEAP_BASE: u64 = 0x0100_0000;

/// Origin of the per-process anonymous-mmap arena. VM bump-allocates mmap
/// addresses upward from here, so a mapping never collides with stub D's code
/// (`0x0043_0000`), stack (`0x0083_0000`), or heap
/// (`[0x0100_0000, 0x0100_8000)` today). `0x0200_0000` sits a clean 16 MiB above
/// `HEAP_BASE`, leaving the heap room to grow before it could reach the arena.
///
/// The arena is bump-only — munmap never returns addresses to it (reuse waits for
/// a real per-process VM layout). Since slice 5.3 it is **capped** at
/// [`REGION_LIMIT`] rather than growing without bound: the kernel now pre-maps a
/// device window at [`USER_DEVICE_WINDOW_BASE`], and an unbounded mmap loop would
/// eventually hand a client an address inside it. Past the cap, `mmap` returns
/// `ENOMEM`.
pub const MMAP_BASE: u64 = 0x0200_0000;

/// Exclusive upper bound of **every** tracked region: the base of the **lowest**
/// kernel-owned window, which is the device window (slice 5.3). No heap and no
/// mmap may extend past here.
///
/// A `const _` on the *bases* would not be enough — it proves only where each
/// region starts, and both grow on request: the mmap arena's bump cursor advances,
/// and `set_brk` raises the heap's `end` to whatever the client asks for. So both
/// carry a runtime check against this bound, returning `ENOMEM`.
///
/// Why it matters even though nothing is exploitable today: a kernel-owned window's
/// whole purpose is to be kernel-owned in *every* address space, so Phase 6 can
/// pre-map a device page into any driver without first asking whether VM already
/// promised that VA to the process's heap.
///
/// **This constant does not move when a window is added.** Slice 5.7's ramdisk
/// window sits at 2 GiB, above the device window, precisely so that the lowest
/// kernel-owned window stays the low-water mark and VM needs no edit — the
/// disjoint-and-ascending assert in `kernel-shared::uspace` is what enforces that,
/// and the assert below is this file's half of it.
pub const REGION_LIMIT: u64 = USER_DEVICE_WINDOW_BASE;

// Both region origins must sit below the bound, or their first request would be
// refused outright.
const _: () = assert!(MMAP_BASE < USER_DEVICE_WINDOW_BASE);
const _: () = assert!(HEAP_BASE < USER_DEVICE_WINDOW_BASE);

// ...and the bound must stay at or below every *other* kernel-owned window, or a
// window added above it would be reachable by a growing heap or mmap arena.
const _: () = assert!(REGION_LIMIT <= RAMDISK_WINDOW_BASE);

/// aarch64 4 KiB page.
const PAGE_SIZE: u64 = 4096;

/// Proc-number range the table can key. Boot procs and stubs occupy `0..15`; PM
/// allocates forked children from the pool above that (kernel proc-nr = PM mproc
/// slot, `[15, NR_SERVED_PROCS)` — see `servers/pm/src/mproc.rs`), so the table
/// must cover the whole fork pool for `VM_FORK` to record a child's inherited
/// regions. Derived from the shared ceiling; the guard below rejects any local
/// under-sizing that would leave a PM-allocatable child unaddressable here.
const MAX_CLIENTS: usize = NR_SERVED_PROCS;

const _: () = assert!(MAX_CLIENTS >= NR_SERVED_PROCS);

/// Regions tracked per process: one heap plus several `mmap` regions in the
/// spare slots. Sixteen leaves room for a loader's segments, a heap, a stack,
/// and a handful of anonymous mmaps once Phase 5 runs real programs.
const MAX_REGIONS: usize = 16;

/// What a region is for. `Unused` marks a free slot.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Kind {
    Unused,
    Heap,
    Mmap,
}

/// A half-open virtual-address range `[start, end)` and what it backs.
#[derive(Copy, Clone)]
struct Region {
    start: u64,
    end: u64,
    kind: Kind,
}

impl Region {
    const EMPTY: Self = Self {
        start: 0,
        end: 0,
        kind: Kind::Unused,
    };

    fn contains(&self, addr: u64) -> bool {
        self.kind != Kind::Unused && addr >= self.start && addr < self.end
    }
}

/// One process's region set.
#[derive(Copy, Clone)]
struct ClientRegions {
    regions: [Region; MAX_REGIONS],
    /// Next free VA for an anonymous mmap. Bump-only: munmap never returns
    /// addresses here (matches a trivial mmap allocator; reuse waits for
    /// Phase 4's real VM layout).
    mmap_next: u64,
}

impl ClientRegions {
    const EMPTY: Self = Self {
        regions: [Region::EMPTY; MAX_REGIONS],
        mmap_next: MMAP_BASE,
    };

    /// True if `addr` falls inside one of this client's regions.
    fn contains(&self, addr: u64) -> bool {
        self.regions.iter().any(|r| r.contains(addr))
    }

    /// Set the program break to `new_break`, growing or creating the heap
    /// region as `[HEAP_BASE, page_align_up(new_break))`. Returns the resulting
    /// break. Errors: `EINVAL` if `new_break` is below `HEAP_BASE`, the page-aligned
    /// break would overflow `u64`, or no region slot is free for a new heap;
    /// `ENOMEM` if the break would carry the heap past [`REGION_LIMIT`] into the
    /// kernel's device window.
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
        // Same cap the mmap arena carries, and for the same reason: a heap grows to
        // whatever the client asks for, so the `HEAP_BASE < REGION_LIMIT` const
        // assert bounds only where it starts. Checked *after* the page-align, since
        // rounding up can carry a break that was just under the bound over it, and
        // *before* any region is mutated, so a refused brk changes nothing.
        if end > REGION_LIMIT {
            return Err(ENOMEM);
        }

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
                *r = Region {
                    start: HEAP_BASE,
                    end,
                    kind: Kind::Heap,
                };
                return Ok(end);
            }
        }
        Err(EINVAL)
    }

    /// Allocate an anonymous mmap region of `len` bytes. `len` is rounded up to
    /// a whole page; the base address is bump-allocated from `mmap_next`.
    /// Returns the chosen base. Errors: `EINVAL` if `len` is 0 or the round-up /
    /// bump would overflow `u64`; `ENOMEM` if no region slot is free, or if the
    /// bump would carry the arena past [`REGION_LIMIT`] into the kernel's device
    /// window.
    fn mmap(&mut self, len: u64) -> Result<u64, i32> {
        if len == 0 {
            return Err(EINVAL);
        }
        // page_align_up, guarding the round-up add against wraparound.
        let size = len
            .checked_add(PAGE_SIZE - 1)
            .map(|v| v & !(PAGE_SIZE - 1))
            .ok_or(EINVAL)?;
        let start = self.mmap_next;
        let end = start.checked_add(size).ok_or(EINVAL)?;
        // The arena is bump-only, so without this cap a long-running mmap loop
        // would eventually hand out an address inside the kernel's device window
        // (`USER_DEVICE_WINDOW_BASE`) and VM would try to resolve faults there.
        // `ENOMEM` is the honest answer: address space, not frames, ran out.
        if end > REGION_LIMIT {
            return Err(ENOMEM);
        }

        for r in self.regions.iter_mut() {
            if r.kind == Kind::Unused {
                *r = Region {
                    start,
                    end,
                    kind: Kind::Mmap,
                };
                self.mmap_next = end;
                return Ok(start);
            }
        }
        Err(ENOMEM)
    }

    /// Unmap the `Mmap` region based at `addr`, marking its slot `Unused` and
    /// returning the page-aligned `[start, end)` range whose backing pages the
    /// caller must sweep with `VMCTL_PT_UNMAP`. The match is keyed on the region
    /// *base*, so an over- or under-stated `len` can never unmap a neighbor; the
    /// returned `end` is additionally capped at the region's own `end` so an
    /// overstated `len` cannot drive the sweep into the heap and free its
    /// frames. `EINVAL` if `len` is 0, no `Mmap` region starts at `addr`, or
    /// `len` overflows. Rejecting `len == 0` (as POSIX does, and symmetric with
    /// [`mmap`](Self::mmap)) avoids dropping a region's tracking while leaving
    /// its already-faulted-in frames mapped and orphaned.
    fn munmap(&mut self, addr: u64, len: u64) -> Result<(u64, u64), i32> {
        if len == 0 {
            return Err(EINVAL);
        }
        let size = len
            .checked_add(PAGE_SIZE - 1)
            .map(|v| v & !(PAGE_SIZE - 1))
            .ok_or(EINVAL)?;
        let end = addr.checked_add(size).ok_or(EINVAL)?;

        for r in self.regions.iter_mut() {
            if r.kind == Kind::Mmap && r.start == addr {
                let sweep_end = end.min(r.end);
                *r = Region::EMPTY;
                return Ok((addr, sweep_end));
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

static TABLE: RegionTable = RegionTable(UnsafeCell::new([ClientRegions::EMPTY; MAX_CLIENTS]));

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

/// Allocate an anonymous mmap region of `len` bytes for process `nr`, with VM
/// choosing the base address. Returns the base on success; `EINVAL` if `nr` is
/// untrackable or `len` is 0/overflowing; `ENOMEM` if no region slot is free.
///
/// No frames are mapped here — pages fault in lazily on first touch and are
/// resolved through [`contains`] in the fault path.
pub fn mmap(nr: i32, len: u64) -> Result<u64, i32> {
    client_mut(nr).ok_or(EINVAL)?.mmap(len)
}

/// Drop process `nr`'s mmap region based at `addr` and return the page-aligned
/// `[start, end)` range whose backing pages the caller must unmap. `EINVAL` if
/// `nr` is untrackable, `len` is 0, or no `Mmap` region starts at `addr`.
pub fn munmap(nr: i32, addr: u64, len: u64) -> Result<(u64, u64), i32> {
    client_mut(nr).ok_or(EINVAL)?.munmap(addr, len)
}

/// Clone `parent_nr`'s whole region set into `child_nr` (the `VM_FORK` path).
/// The kernel already copied the child's page tables in `SYS_FORK`; this copies
/// VM's own bookkeeping so the child's later brk/mmap/fault lookups inherit the
/// parent's heap/mmap regions (and its `mmap_next` bump cursor). `EINVAL` if
/// either proc number is untrackable. An untracked parent (never touched
/// memory) clones as an empty set, which is correct.
///
/// There is deliberately no per-exit VM teardown yet (no `VM_EXIT`), so a child's
/// region set outlives the kernel proc. That is benign: the assignment below is a
/// *full overwrite* of `child_nr`'s entry, so a recycled proc number never
/// inherits stale regions from a previous occupant, and the fault path only ever
/// resolves addresses for live procs.
pub fn fork(parent_nr: i32, child_nr: i32) -> Result<(), i32> {
    // Snapshot the parent by value first (ClientRegions is Copy) so we never
    // hold two live borrows into TABLE at once.
    let parent = *client_ref(parent_nr).ok_or(EINVAL)?;
    *client_mut(child_nr).ok_or(EINVAL)? = parent;
    Ok(())
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

        // The largest break that still *aligns* without overflowing. Before slice
        // 5.3 this returned `Ok(max_ok)`; it is now `ENOMEM`, refused by the
        // `REGION_LIMIT` cap rather than by the overflow guard. That distinction is
        // the point of keeping the case: `ENOMEM` witnesses that the align-up
        // produced a genuinely huge `end`, because a wrapped `end` would have been
        // *small*, sailed under the cap, and come back `Ok`. So the two rejection
        // reasons must stay distinguishable — collapsing either to the other would
        // hide a wrap.
        let max_ok = !(PAGE_SIZE - 1);
        assert_eq!(c.set_brk(max_ok), Err(ENOMEM));
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

    #[test]
    fn mmap_creates_region_and_returns_base() {
        let mut c = ClientRegions::EMPTY;
        let a = c.mmap(0x2000).unwrap();
        assert_eq!(a, MMAP_BASE);
        assert!(c.contains(MMAP_BASE));
        assert!(c.contains(MMAP_BASE + 0x1FFF));
        // Half-open: the byte at the region end is *past* the mapping.
        assert!(!c.contains(MMAP_BASE + 0x2000));
    }

    #[test]
    fn mmap_rounds_len_up_to_page() {
        let mut c = ClientRegions::EMPTY;
        let a = c.mmap(1).unwrap();
        assert!(c.contains(a + PAGE_SIZE - 1));
        assert!(!c.contains(a + PAGE_SIZE));
        // The next mmap starts a full page above, not one byte above.
        let b = c.mmap(1).unwrap();
        assert_eq!(b, a + PAGE_SIZE);
    }

    #[test]
    fn mmap_bumps_address_each_call() {
        let mut c = ClientRegions::EMPTY;
        let a = c.mmap(0x2000).unwrap();
        let b = c.mmap(0x1000).unwrap();
        assert_eq!(b, a + 0x2000);
        assert!(c.contains(a));
        assert!(c.contains(b));
    }

    #[test]
    fn mmap_zero_len_is_einval() {
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.mmap(0), Err(EINVAL));
    }

    #[test]
    fn mmap_overflowing_len_is_einval_not_wrap() {
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.mmap(u64::MAX), Err(EINVAL));
    }

    #[test]
    fn mmap_enomem_when_no_slot_free() {
        let mut c = ClientRegions::EMPTY;
        // MAX_REGIONS slots: fill every one, then the next mmap fails ENOMEM.
        for _ in 0..MAX_REGIONS {
            c.mmap(0x1000).unwrap();
        }
        assert_eq!(c.mmap(0x1000), Err(ENOMEM));
    }

    /// A heap grows to whatever the client asks for, so — exactly like the mmap
    /// arena — the `HEAP_BASE < REGION_LIMIT` const assert bounds only where it
    /// starts. Check both sides of the boundary, and that a refused brk is inert.
    #[test]
    fn set_brk_stops_at_the_device_window() {
        let mut c = ClientRegions::EMPTY;
        // Right up to the bound is fine: the region is half-open, so a heap ending
        // exactly at REGION_LIMIT does not include the window's first byte.
        assert_eq!(c.set_brk(REGION_LIMIT), Ok(REGION_LIMIT));
        assert!(!c.contains(REGION_LIMIT));
        assert!(c.contains(REGION_LIMIT - 1));

        // One byte past is ENOMEM, not a silently clipped heap.
        assert_eq!(c.set_brk(REGION_LIMIT + 1), Err(ENOMEM));
        // ...and the refusal left the previous break in place.
        let heap = c.regions.iter().find(|r| r.kind == Kind::Heap).unwrap();
        assert_eq!(heap.end, REGION_LIMIT, "a refused brk must change nothing");

        // The page-align-up must not be able to carry a break over the bound.
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.set_brk(REGION_LIMIT - PAGE_SIZE + 1), Ok(REGION_LIMIT));
        assert_eq!(
            c.set_brk(REGION_LIMIT - PAGE_SIZE + 1 + PAGE_SIZE),
            Err(ENOMEM)
        );

        // A fresh client's *first* brk is capped too — not just a grow.
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.set_brk(0x8000_0000), Err(ENOMEM));
        assert!(c.regions.iter().all(|r| r.kind == Kind::Unused));
    }

    #[test]
    fn both_region_origins_sit_below_the_bound() {
        assert_eq!(REGION_LIMIT, USER_DEVICE_WINDOW_BASE);
        // `min` rather than `<`: an all-constant `assert!` trips clippy's
        // `assertions_on_constants`.
        assert_eq!(HEAP_BASE.min(REGION_LIMIT), HEAP_BASE);
        assert_eq!(MMAP_BASE.min(REGION_LIMIT), MMAP_BASE);
    }

    /// The arena is bump-only, so the only thing standing between a long-running
    /// mmap loop and the kernel's device window is [`REGION_LIMIT`]. Drive the
    /// cursor right up to it and check both sides of the boundary.
    #[test]
    fn mmap_stops_at_the_device_window() {
        let mut c = ClientRegions::EMPTY;
        // Park the cursor one page short of the limit: that page must succeed…
        c.mmap_next = REGION_LIMIT - PAGE_SIZE;
        let last = c.mmap(PAGE_SIZE).unwrap();
        assert_eq!(last, REGION_LIMIT - PAGE_SIZE);
        assert_eq!(c.mmap_next, REGION_LIMIT);
        // …and the next one must not, even though region slots remain.
        assert_eq!(c.mmap(PAGE_SIZE), Err(ENOMEM));
        // A request that would *straddle* the limit is refused whole, not clipped.
        c.mmap_next = REGION_LIMIT - PAGE_SIZE;
        assert_eq!(c.mmap(2 * PAGE_SIZE), Err(ENOMEM));
        assert_eq!(
            c.mmap_next,
            REGION_LIMIT - PAGE_SIZE,
            "cursor moved on failure"
        );
    }

    #[test]
    fn the_arena_starts_below_the_device_window() {
        assert_eq!(REGION_LIMIT, USER_DEVICE_WINDOW_BASE);
        // `min` rather than `<`: an all-constant `assert!` trips clippy's
        // `assertions_on_constants`. The module-level `const _` proves the same
        // thing at compile time; this records it where a reader looks.
        assert_eq!(MMAP_BASE.min(REGION_LIMIT), MMAP_BASE);
    }

    #[test]
    fn munmap_removes_region_and_returns_range() {
        let mut c = ClientRegions::EMPTY;
        let a = c.mmap(0x2000).unwrap();
        let (start, end) = c.munmap(a, 0x2000).unwrap();
        assert_eq!((start, end), (a, a + 0x2000));
        assert!(!c.contains(a));
        let mmaps = c.regions.iter().filter(|r| r.kind == Kind::Mmap).count();
        assert_eq!(mmaps, 0);
    }

    #[test]
    fn munmap_unknown_addr_is_einval() {
        let mut c = ClientRegions::EMPTY;
        assert_eq!(c.munmap(MMAP_BASE, 0x1000), Err(EINVAL)); // nothing mapped
        let a = c.mmap(0x1000).unwrap();
        assert_eq!(c.munmap(a + 0x1000, 0x1000), Err(EINVAL)); // wrong base
    }

    #[test]
    fn munmap_does_not_touch_heap_region() {
        let mut c = ClientRegions::EMPTY;
        c.set_brk(HEAP_BASE + 0x1000).unwrap();
        let a = c.mmap(0x1000).unwrap();
        c.munmap(a, 0x1000).unwrap();
        // The heap region survives untouched.
        assert!(c.contains(HEAP_BASE));
        assert_eq!(c.regions.iter().filter(|r| r.kind == Kind::Heap).count(), 1);
    }

    #[test]
    fn munmap_zero_len_is_einval_and_keeps_region() {
        let mut c = ClientRegions::EMPTY;
        let a = c.mmap(0x1000).unwrap();
        // len == 0 must not drop the region (which would orphan its mapped
        // frames); the mapping stays tracked, symmetric with mmap(0) == EINVAL.
        assert_eq!(c.munmap(a, 0), Err(EINVAL));
        assert!(c.contains(a));
        assert_eq!(c.regions.iter().filter(|r| r.kind == Kind::Mmap).count(), 1);
    }

    #[test]
    fn munmap_caps_sweep_to_region_end() {
        let mut c = ClientRegions::EMPTY;
        let a = c.mmap(0x1000).unwrap();
        // Caller overstates len; the sweep must not exceed the region's own end.
        let (start, end) = c.munmap(a, 0x4000).unwrap();
        assert_eq!((start, end), (a, a + 0x1000));
    }

    // The `fork` free function operates on the global TABLE, so each test below
    // uses its own dedicated proc-number slots (distinct from every other test's)
    // to stay independent under parallel test execution.

    #[test]
    fn fork_clones_parent_regions_into_child() {
        // Seed parent slot 20 with a heap and an mmap region, then fork into 21.
        set_brk(20, HEAP_BASE + 0x4000).unwrap();
        let mmap_base = mmap(20, 0x2000).unwrap();

        fork(20, 21).unwrap();

        // Child inherited both regions.
        assert!(contains(21, HEAP_BASE));
        assert!(contains(21, HEAP_BASE + 0x3FFF));
        assert!(contains(21, mmap_base));
        assert!(!contains(21, HEAP_BASE + 0x4000)); // half-open, as parent
    }

    #[test]
    fn fork_from_untracked_parent_gives_empty_child() {
        // Parent slot 28 was never touched → empty clone; child 29 has no regions.
        fork(28, 29).unwrap();
        assert!(!contains(29, HEAP_BASE));
    }

    #[test]
    fn fork_out_of_range_is_einval() {
        // Kernel-task parent or past-the-cap child are both untrackable.
        assert_eq!(fork(-1, 5), Err(EINVAL));
        assert_eq!(fork(30, MAX_CLIENTS as i32), Err(EINVAL));
    }
}

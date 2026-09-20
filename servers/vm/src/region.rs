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
use minixrs_kernel_shared::uspace::{USER_REGION_LIMIT, USER_STACK_BASE, USER_STACK_TOP};

/// **Legacy** heap origin: the origin used by a process that has never been
/// through `VM_EXEC` — stub D and the boot servers. VM and stub D agree on this
/// VA by a convention baked into `user_stub.S`'s blob: `brk` grows the heap as
/// `[HEAP_BASE, new_break)` and stub D writes inside that range.
///
/// An exec'd image gets an *image-relative* origin instead — the loader's
/// page-aligned image end, carried to VM by `VM_EXEC` and recorded per process.
/// This constant survives rather than moving because nothing ever tells VM that
/// a boot server or a stub exists, so there is no moment at which their origin
/// could be recorded.
pub const HEAP_BASE: u64 = 0x0100_0000;

/// **Legacy** origin of the anonymous-mmap arena, paired with [`HEAP_BASE`] and
/// belonging to the same processes: the ones that never go through `VM_EXEC`. VM
/// bump-allocates mmap addresses upward from here, so a mapping never collides
/// with stub D's code (`0x0043_0000`), stack (`0x0083_0000`), or heap
/// (`[0x0100_0000, 0x0100_8000)` today). `0x0200_0000` sits a clean [`MMAP_GAP`]
/// above `HEAP_BASE`, leaving the heap room to grow before it could reach the
/// arena — and an exec'd process gets that same gap above its own
/// image-relative origin.
///
/// The arena is bump-only — munmap never returns addresses to it (reuse waits for
/// a real per-process VM layout). Since slice 5.3 it is **capped** at
/// [`REGION_LIMIT`] rather than growing without bound: an unbounded mmap loop
/// would otherwise eventually hand a client an address the kernel has already
/// promised to something else. Past the cap, `mmap` returns `ENOMEM`.
pub const MMAP_BASE: u64 = 0x0200_0000;

/// Exclusive upper bound of **every** tracked region.
///
/// A re-export of [`USER_REGION_LIMIT`], which is where the user VA map is
/// defined. It used to be defined here as the base of the lowest kernel-owned
/// window; the stack moving to the top of process VA made that wrong — the first
/// thing above a growing region is now the stack's guard page, not a window.
///
/// The definition moved to `kernel-shared` rather than merely changing value,
/// because `kernel-shared::uspace` documents the whole map and could not name
/// the bound the map is really about: a shared crate cannot reference a server.
///
/// A `const _` on the region *bases* would not be enough — it proves only where
/// each region starts, and both grow on request: the mmap arena's bump cursor
/// advances, and `set_brk` raises the heap's `end` to whatever the client asks
/// for. So both carry a runtime check against this bound, returning `ENOMEM`.
pub const REGION_LIMIT: u64 = USER_REGION_LIMIT;

/// Distance from a process's heap origin to its mmap arena origin.
///
/// Extracted from the legacy pair rather than invented: `MMAP_BASE - HEAP_BASE`
/// has been 16 MiB since slice 3.6, and applying the same gap to a per-process
/// origin keeps the heap exactly as much room to grow as it has always had.
pub const MMAP_GAP: u64 = 0x0100_0000;

// The gap describes the legacy pair too, or it would be a number that happens to
// work for new processes and silently disagrees with stub D's layout.
const _: () = assert!(MMAP_BASE == HEAP_BASE + MMAP_GAP);

// The legacy origins must still sit below the bound, which is now *lower* than
// it was — so these asserts do more work than before, not less.
const _: () = assert!(MMAP_BASE < REGION_LIMIT);
const _: () = assert!(HEAP_BASE < REGION_LIMIT);

// The ceiling must clear the stack and its guard page. This is the assert that
// replaces the old `REGION_LIMIT <= RAMDISK_WINDOW_BASE`: the windows are no
// longer what bounds a region, the stack is.
const _: () = assert!(REGION_LIMIT < USER_STACK_BASE);

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
    /// The initial stack the *kernel* mapped at image load.
    ///
    /// VM records it but never resolves a fault against it: every stack page is
    /// already mapped when the record is made, so the region is pure
    /// bookkeeping. What it buys is `fork` cloning a region set that describes
    /// the child's whole address space, and a fault in the guard page landing
    /// just outside a known neighbour instead of nowhere at all.
    Stack,
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
    /// Origin of this process's heap. The legacy [`HEAP_BASE`] until a
    /// `VM_EXEC` records the image end.
    heap_origin: u64,
    /// Next free VA for an anonymous mmap. Bump-only: munmap never returns
    /// addresses here (matches a trivial mmap allocator; reuse waits for a real
    /// per-process VM layout).
    mmap_next: u64,
}

impl ClientRegions {
    const EMPTY: Self = Self {
        regions: [Region::EMPTY; MAX_REGIONS],
        heap_origin: HEAP_BASE,
        mmap_next: MMAP_BASE,
    };

    /// True if `addr` falls inside one of this client's regions.
    fn contains(&self, addr: u64) -> bool {
        self.regions.iter().any(|r| r.contains(addr))
    }

    /// Set the program break to `new_break`, growing or creating the heap
    /// region as `[heap_origin, page_align_up(new_break))`. Returns the
    /// resulting break. Errors: `EINVAL` if `new_break` is below this process's
    /// heap origin, the page-aligned break would overflow `u64`, or no region
    /// slot is free for a new heap; `ENOMEM` if the break would carry the heap
    /// past [`REGION_LIMIT`] into the stack's guard page.
    fn set_brk(&mut self, new_break: u64) -> Result<u64, i32> {
        // The origin is read once, up front: the free-slot loop below borrows
        // `self.regions` mutably, so `self.heap_origin` cannot be read inside it.
        let origin = self.heap_origin;
        if new_break < origin {
            return Err(EINVAL);
        }
        // page_align_up, guarding the round-up add against wraparound near
        // `u64::MAX` (a silent wrap would yield a tiny `end` in `--release`).
        let end = new_break
            .checked_add(PAGE_SIZE - 1)
            .map(|v| v & !(PAGE_SIZE - 1))
            .ok_or(EINVAL)?;
        // Same cap the mmap arena carries, and for the same reason: a heap grows to
        // whatever the client asks for, so a const assert on the origin bounds only
        // where it starts. Checked *after* the page-align, since rounding up can
        // carry a break that was just under the bound over it, and *before* any
        // region is mutated, so a refused brk changes nothing.
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
                    start: origin,
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
    /// bump would carry the arena past [`REGION_LIMIT`] into the stack's guard
    /// page.
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
        // would eventually hand out an address inside the stack's guard page and
        // then the stack itself, and VM would try to resolve faults there.
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

    /// Reset this process's bookkeeping around a freshly exec'd image.
    ///
    /// Drops every region the *previous* image accumulated and records
    /// `image_end` as the heap origin, with the mmap arena [`MMAP_GAP`] above
    /// it. Returns how many regions were dropped — non-zero only for a proc that
    /// had touched memory before exec'ing, which is what makes the stale-region
    /// gap observable in a boot log.
    ///
    /// A full reset, not a merge: the old regions describe an address space the
    /// kernel tore down in `SYS_EXEC`, so every one of them is wrong.
    fn exec(&mut self, image_end: u64) -> usize {
        let dropped = self
            .regions
            .iter()
            .filter(|r| r.kind != Kind::Unused)
            .count();
        self.regions = [Region::EMPTY; MAX_REGIONS];
        self.heap_origin = image_end;
        // Saturating rather than checked: an image_end near u64::MAX cannot come
        // out of the loader (every segment passed `check_va`), and an arena
        // origin above REGION_LIMIT simply makes every mmap answer ENOMEM, which
        // is the correct outcome for a process with no address space left.
        self.mmap_next = image_end.saturating_add(MMAP_GAP);
        dropped
    }

    /// Record the initial stack the kernel mapped, as a [`Kind::Stack`] region.
    ///
    /// Idempotent: a second call replaces the existing stack region rather than
    /// consuming a second slot, so a re-exec cannot leak slots.
    fn record_stack(&mut self) {
        for r in self.regions.iter_mut() {
            if r.kind == Kind::Stack {
                *r = Region {
                    start: USER_STACK_BASE,
                    end: USER_STACK_TOP,
                    kind: Kind::Stack,
                };
                return;
            }
        }
        for r in self.regions.iter_mut() {
            if r.kind == Kind::Unused {
                *r = Region {
                    start: USER_STACK_BASE,
                    end: USER_STACK_TOP,
                    kind: Kind::Stack,
                };
                return;
            }
        }
        // No free slot: MAX_REGIONS is 16 and a proc uses heap + stack + mmaps,
        // so this is unreachable in practice. Dropping the record silently is
        // the right failure — the stack is mapped either way, and refusing the
        // exec over a bookkeeping slot would be worse than not recording it.
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
/// heap region as `[heap_origin, page_align_up(new_break))`. Returns the
/// resulting break on success, or `EINVAL` if `nr` is untrackable or
/// `new_break` is below that process's heap origin — the legacy [`HEAP_BASE`]
/// until a `VM_EXEC` records an image-relative one.
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

/// Reset process `nr`'s regions around a freshly exec'd image whose page-aligned
/// end is `image_end` (the `VM_EXEC` path). Returns the number of stale regions
/// dropped, or `EINVAL` if `nr` is untrackable.
// Forward declaration: `main.rs`'s `VM_EXEC` handler is the sole caller and lands
// in the next task of this slice. Drop the allow when it does.
#[allow(dead_code)]
pub fn exec(nr: i32, image_end: u64) -> Result<usize, i32> {
    Ok(client_mut(nr).ok_or(EINVAL)?.exec(image_end))
}

/// Record the kernel-mapped initial stack as a region of process `nr`.
/// `EINVAL` if `nr` is untrackable.
// Forward declaration, like `exec` above: called from `main.rs`'s `VM_EXEC`
// handler and from `vm_init` (for the boot procs) in the next task of this slice.
#[allow(dead_code)]
pub fn record_stack(nr: i32) -> Result<(), i32> {
    client_mut(nr).ok_or(EINVAL)?.record_stack();
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
    fn set_brk_stops_at_the_region_ceiling() {
        let mut c = ClientRegions::EMPTY;
        // Right up to the bound is fine: the region is half-open, so a heap ending
        // exactly at REGION_LIMIT does not include the guard page's first byte.
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
        // The legacy origins, which a process that never goes through VM_EXEC
        // still uses, must clear a bound that is now *lower* than it was.
        assert_eq!(REGION_LIMIT, USER_REGION_LIMIT);
        // `min` rather than `<`: an all-constant `assert!` trips clippy's
        // `assertions_on_constants`.
        assert_eq!(HEAP_BASE.min(REGION_LIMIT), HEAP_BASE);
        assert_eq!(MMAP_BASE.min(REGION_LIMIT), MMAP_BASE);
    }

    /// The arena is bump-only, so the only thing standing between a long-running
    /// mmap loop and the stack's guard page is [`REGION_LIMIT`]. Drive the
    /// cursor right up to it and check both sides of the boundary.
    #[test]
    fn mmap_stops_at_the_region_ceiling() {
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
    fn the_arena_starts_below_the_region_ceiling() {
        assert_eq!(REGION_LIMIT, USER_REGION_LIMIT);
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

    #[test]
    fn a_fresh_client_still_uses_the_legacy_origins() {
        // Stub D and the boot servers never get a VM_EXEC, so the fixed origins
        // stay their default. This is the arm that keeps user_stub.S working.
        let mut c = ClientRegions::EMPTY;
        let brk = c.set_brk(HEAP_BASE + 0x1000).unwrap();
        assert_eq!(brk, HEAP_BASE + 0x1000);
        assert!(c.contains(HEAP_BASE));
    }

    #[test]
    fn exec_moves_the_heap_origin_to_the_image_end() {
        let mut c = ClientRegions::EMPTY;
        let image_end = 0x0030_2000;
        c.exec(image_end);
        // The first brk creates the heap at the recorded origin, not HEAP_BASE.
        let brk = c.set_brk(image_end + 0x1000).unwrap();
        assert_eq!(brk, image_end + 0x1000);
        assert!(c.contains(image_end));
        assert!(!c.contains(HEAP_BASE));
    }

    #[test]
    fn a_brk_below_the_recorded_origin_is_einval() {
        let mut c = ClientRegions::EMPTY;
        // Deliberately *above* the legacy `HEAP_BASE`, so the second assertion
        // tests what it claims to: an image whose end outruns the fixed origin
        // is the whole reason the origin had to become per-process.
        let image_end = HEAP_BASE + 0x0010_0000;
        c.exec(image_end);
        assert_eq!(c.set_brk(image_end - 1), Err(EINVAL));
        // ...including an address that would have been valid under the legacy
        // origin, which is the regression this guards.
        assert_eq!(c.set_brk(HEAP_BASE + 0x1000), Err(EINVAL));
    }

    #[test]
    fn exec_drops_the_previous_images_regions() {
        let mut c = ClientRegions::EMPTY;
        c.set_brk(HEAP_BASE + 0x4000).unwrap();
        let old_mmap = c.mmap(0x2000).unwrap();
        assert!(c.contains(old_mmap));

        let dropped = c.exec(0x0030_2000);
        assert_eq!(dropped, 2, "heap + one mmap should have been dropped");
        assert!(!c.contains(old_mmap));
        assert!(!c.contains(HEAP_BASE));
    }

    #[test]
    fn the_mmap_arena_bumps_from_the_recorded_origin_plus_the_gap() {
        let mut c = ClientRegions::EMPTY;
        let image_end = 0x0030_2000;
        c.exec(image_end);
        let base = c.mmap(0x1000).unwrap();
        assert_eq!(base, image_end + MMAP_GAP);
        // The legacy pair still differ by exactly the same gap, so the constant
        // describes both worlds rather than only the new one.
        assert_eq!(MMAP_BASE, HEAP_BASE + MMAP_GAP);
    }

    #[test]
    fn a_recorded_stack_is_contained_but_is_not_a_heap_or_an_mmap() {
        let mut c = ClientRegions::EMPTY;
        c.record_stack();
        assert!(c.contains(USER_STACK_BASE));
        assert!(c.contains(USER_STACK_TOP - 1));
        assert!(!c.contains(USER_STACK_TOP), "the range is half-open");
        // The guard page below the stack must NOT be covered, or an overflow
        // would be silently resolved instead of faulting.
        assert!(!c.contains(USER_STACK_BASE - 1));
        // brk must not mistake it for a heap, and munmap must refuse it.
        assert_eq!(c.munmap(USER_STACK_BASE, 0x1000), Err(EINVAL));
    }

    #[test]
    fn the_region_ceiling_excludes_the_stack_and_its_guard_page() {
        let mut c = ClientRegions::EMPTY;
        // A brk that would reach the guard page is refused.
        assert_eq!(c.set_brk(REGION_LIMIT + 1), Err(ENOMEM));
        assert_eq!(REGION_LIMIT, USER_REGION_LIMIT);
        // Exactly one guard page between the ceiling and the lowest stack VA --
        // stronger than `REGION_LIMIT < USER_STACK_BASE`, and written through a
        // local because an all-constant `assert!` trips clippy's
        // `assertions_on_constants`.
        let guard = USER_STACK_BASE - REGION_LIMIT;
        assert_eq!(guard, PAGE_SIZE, "the guard page is not one page wide");
    }
}

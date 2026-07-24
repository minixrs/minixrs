# Memory Management

minix.rs splits memory management the way MINIX 3 does — *mechanism* in the
kernel, *policy* in a user-space server — but moves physical-frame ownership
into the kernel. The kernel owns the frame allocator and performs every page
table write; the user-space **VM server** decides *what* should be mapped where
and drives the kernel through a single privileged kernel call (`SYS_VMCTL`).

This chapter describes the subsystem as it stands at the end of Phase 3 — a
physical frame allocator, per-process address spaces with hardware-tagged TLBs,
a page-fault path that delegates to VM, and the VM server's region tracking for
`brk`, `mmap`, and `munmap` — plus the region-cloning that PM-driven `fork` added
in Phase 4.

## Memory map (aarch64 QEMU `virt`)

The QEMU `virt` machine has a fixed device layout. With `-m 256M`, RAM sits at
`0x4000_0000`:

| Physical range | Device |
|----------------|--------|
| `0x0800_0000`–`0x0800_FFFF` | GICv3 distributor |
| `0x080A_0000`–`0x080B_FFFF` | GICv3 redistributor |
| `0x0900_0000`–`0x0900_0FFF` | PL011 UART |
| `0x0A00_0000`+ | VirtIO MMIO devices |
| `0x4000_0000`+ | RAM (256 MB default) |

The virtual address space splits at the canonical hole. `TTBR1_EL1` (the upper
half) holds the kernel image and Limine's direct map; `TTBR0_EL1` (the lower
half) holds the current process:

```text
0xFFFF_FFFF_........   kernel image (.text / .rodata / .data / .bss, embedded boot image)
0xFFFF_8000_0000_0000   HHDM base — all physical memory mapped at phys + hhdm_offset
        ... non-canonical hole ...
0x0000_7FFF_FFFF_FFFF   user ceiling (stack region, grows down)
0x0000_0000_0200_0000   MMAP_BASE — anonymous mmap arena (bump-allocated up)
0x0000_0000_0100_0000   HEAP_BASE — heap region (brk grows up)
0x0000_0000_0010_0000   user load base — text/rodata/data/bss (servers, init, worker)
0x0000_0000_0000_0000
```

### Translation tables

minix.rs uses a 4 KB granule with 4-level translation:

| Level | VA bits | Each entry maps |
|-------|---------|-----------------|
| L0 | [47:39] | 512 GB |
| L1 | [38:30] | 1 GB |
| L2 | [29:21] | 2 MB |
| L3 | [20:12] | 4 KB |

`TTBR0_EL1` holds the current process's tables and is swapped (tagged with the
process's ASID) on every context switch; `TTBR1_EL1` holds the shared kernel
mapping and never changes. Limine maps all physical memory contiguously at
`hhdm_offset`, so the kernel reaches any physical address as `phys + hhdm_offset`
with no temporary mapping windows — every free frame, page table, and message
buffer the kernel touches is addressed this way.

## Physical frame allocator

`kernel/src/mm/` is the kernel-side physical allocator. At boot it walks
Limine's `MEMMAP_USABLE` entries and seeds a per-region bump pointer for each
(`MAX_REGIONS = 16` regions — QEMU virt and Apple-Silicon QEMU both fit
comfortably). Freed frames go onto an intrusive free-list threaded through the
frames themselves, reached via Limine's higher-half direct map (HHDM).

Two invariants keep callers honest:

- `alloc_frame` **zeroes** every frame before handing it out, so a caller never
  observes residual state.
- `free_frame` pushes the frame back through the HHDM.

Frames inside the kernel image, the embedded boot image, and the static EL0
test-stub pages live in Limine's `EXECUTABLE_AND_MODULES` region and are never
visible to the allocator, so no explicit reservation logic is needed.

## Per-process address spaces

Each user process has its own translation table tree, rooted at a physical
address recorded in `Proc::ttbr0_pa`, and an 8-bit address-space identifier in
`Proc::asid`. `kernel/src/arch/aarch64/addrspace.rs` is the page-table API:

- `AddrSpace::new()` allocates an L0 root frame.
- `map_page(va, pa, prot)` walks the tree, allocating L1/L2/L3 tables on demand
  from the frame allocator, and writes the leaf PTE through the HHDM.
- `walk_pt(va)` resolves a VA to its PTE (or `None`).
- `destroy()` recursively frees the intermediate tables and the L0 root (leaf
  frames are caller-owned and freed elsewhere).

The free functions `map_page_in(ttbr0_pa, …)` and `unmap_page_in(ttbr0_pa, …)`
do the same work keyed by a root PA, so the kernel can mutate a process's tree
without holding an `AddrSpace` value — this is how `SYS_VMCTL` edits a *target*
process's address space.

### Context switch and ASIDs

On every switch into a user process, `proc::sched::schedule_next`:

1. parks the next process's register frame for the trap return,
2. calls `switch_ttbr0_with_asid(ttbr0_pa, asid)` — which writes
   `TTBR0_EL1 = ttbr0_pa | ((asid as u64) << 48)` and issues an ASID-tagged
   `tlbi` — **before**
3. flushing any pending IPC message into the user buffer.

The order matters: the message flush writes through the *active* TTBR0, so the
incoming process's address space must be live first. ASIDs let the hardware keep
TLB entries for multiple address spaces without a full flush on each switch; the
allocator (`asid.rs`) hands them out from `FIRST_ASID = 1` (0 means
"uninitialized") and panics on 8-bit wrap — real rollover is deferred until
process churn in Phase 4 makes it reachable.

## The page-fault path

When an EL0 process touches an unmapped page, the CPU traps to EL1 and
`do_page_fault(esr, elr, far)` runs. It classifies the abort (instruction vs
data, fault status code, write-vs-read), records the coordinates in the
faulting process's `PageFaultState`, blocks the process on the `RTS_PAGEFAULT`
run-time state, and sends the VM server a `VM_PAGEFAULT` message carrying the
faulting endpoint and the fault address. Permission faults (a write to a
read-only page) are not resolvable by mapping and halt loudly.

The kernel originates that send with `mini_pf_send`, which models the faulting
process as a blocked sender on VM's caller queue — the lingering `RTS_PAGEFAULT`
keeps it blocked even after the `RTS_SENDING` half clears, until VM explicitly
clears the fault. Because VM resolves the fault while running under its own
TTBR0 and the message is delivered after the switch, no cross-address-space copy
machinery is needed at this stage.

## `SYS_VMCTL`: kernel mechanism, VM policy

`SYS_VMCTL` (`kernel/src/system/do_vmctl.rs`) is the one privileged call through
which VM drives the kernel's paging mechanism. It is gated solely by
`Priv::k_call_mask` granting `SYS_VMCTL`; VM is its sole intended holder and is
trusted to target only processes it legitimately manages (the MINIX 3 trust
model). Each subcall names a *target* process by endpoint (`SELF` allowed):

| Subcall | Effect |
|---------|--------|
| `VMCTL_PT_MAP` | Allocate a fresh zeroed frame and map it at `vaddr` with the requested protection; reply with the chosen PA. |
| `VMCTL_PT_UNMAP` | Clear the PTE at `vaddr` and free its backing frame. **Returns `EINVAL` if nothing is mapped there** (a harmless no-op for callers sweeping a range). |
| `VMCTL_CLEAR_PAGEFAULT` | Clear the target's recorded fault and make it runnable again. |
| `VMCTL_GET_PAGEFAULT` | Read the target's recorded fault coordinates. |
| `VMCTL_VMINHIBIT_SET` / `_CLEAR` | Gate scheduling of the target while VM mutates its address space. |

Every PTE change is followed by an ASID-tagged TLB invalidation. The kernel
allocates frames (unlike MINIX 3, where VM owns physical memory) and VM supplies
only the virtual address and protection.

## VM server region tracking

The VM server (`servers/vm/`) is the first real user-space process. It runs a
`RECEIVE(ANY)` loop and dispatches on message type. It owns no heap allocator —
the kernel owns frames — so it tracks memory with a static per-process region
table (`servers/vm/src/region.rs`): `[ClientRegions; 16]`, keyed by process
number, each holding up to `MAX_REGIONS = 4` regions. A region is a half-open
virtual range `[start, end)` tagged with a `Kind`:

- `Heap` — grown by `brk`, based at the fixed `HEAP_BASE` (`0x0100_0000`).
- `Mmap` — anonymous mappings, bump-allocated from `MMAP_BASE` (`0x0200_0000`).
- `Unused` — a free slot.

A page fault is satisfied **only** if its address lies inside one of the
faulting process's regions: VM consults the table, and on a hit issues
`SYS_VMCTL(VMCTL_PT_MAP)` then `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`. A fault
outside every region is a segmentation fault: VM raises
`SYS_KILL(faulter, SIGSEGV)`, and the faulter stays blocked on `RTS_PAGEFAULT`
until PM terminates it (the minimal-signals path that made this a real kill
landed in Phase 4; before that VM could only leave the process wedged). VM runs
at EL0 with no console, so it is silent from the server's side — the kill surfaces
in the kernel signal trace.

### `brk`

`VM_BRK(new_break)` sets the caller's program break. VM page-aligns the request
and grows (or first creates) the caller's `Heap` region to
`[HEAP_BASE, new_break)`, replying with the resulting break. No frames are
mapped eagerly — pages fault in lazily on first touch and are then satisfied by
the region check above.

### `mmap` / `munmap`

`VM_MMAP(len)` is an anonymous mapping in the style of `mmap(NULL, len, …)`: VM
page-aligns the length, bump-allocates a base address from the caller's mmap
arena, records an `Mmap` region, and replies with the chosen base. As with the
heap, frames are not mapped until first touch.

`VM_MUNMAP(addr, len)` drops the `Mmap` region based at `addr` and unmaps each
backing page with `SYS_VMCTL(VMCTL_PT_UNMAP)`. Pages that never faulted in were
never mapped, so the kernel returns a harmless `EINVAL` for them, which VM
ignores. The match is keyed on the region's base address and the unmap sweep is
capped at the region's own end, so an over-stated length can never reach into a
neighboring region or the heap.

The arena is bump-only for now — `munmap` does not return addresses to it.
Address reuse within the arena remains future work. PM-driven `fork` (Phase 4)
already clones a parent's entire region set into the child: after the kernel
copies the child's page tables via `SYS_FORK`, PM issues `VM_FORK`, and VM copies
its own `ClientRegions` bookkeeping so the child's later `brk` / `mmap` / fault
lookups see the inherited heap and mmap regions.

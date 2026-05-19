# Memory Layout

## aarch64 (QEMU virt machine)

### Physical Memory Map (QEMU virt)

QEMU's `virt` machine has a fixed device layout:

| Address Range | Size | Device |
|---------------|------|--------|
| `0x0000_0000` - `0x07FF_FFFF` | 128 MB | Flash (firmware) |
| `0x0800_0000` - `0x0800_FFFF` | 64 KB | GICv3 Distributor |
| `0x080A_0000` - `0x080B_FFFF` | 128 KB | GICv3 Redistributor |
| `0x0900_0000` - `0x0900_0FFF` | 4 KB | PL011 UART |
| `0x0A00_0000` - `0x0A00_0FFF` | 4 KB+ | VirtIO MMIO devices |
| `0x4000_0000` - ... | configurable | RAM (256 MB default) |

With `-m 256M`, RAM is at `0x4000_0000` to `0x4FFF_FFFF`.

### Virtual Address Space

```
0xFFFF_FFFF_FFFF_FFFF  +------------------------+
                       |                        |
                       |  Kernel high mapping   |
0xFFFF_FFFF_8020_0000  +------------------------+  _kern_vir_base
                       |  .text                 |
                       |  .rodata               |
                       |  .data, .bss           |
                       |  .boot_image           |
                       +------------------------+

0xFFFF_8000_0000_0000  +------------------------+  HHDM base (from Limine)
                       |  Higher Half Direct    |
                       |  Map of all physical   |
                       |  memory                |
                       |  phys_to_virt(p) =     |
                       |    p + hhdm_offset     |
  + physical mem size  +------------------------+

                       ... (non-canonical hole) ...

0x0000_7FFF_FFFF_FFFF  +------------------------+  User space ceiling
                       |                        |
                       |  User stack            |
                       |  (grows down)          |
                       +------------------------+
                       |                        |
                       |  mmap region           |
                       |                        |
                       +------------------------+
                       |                        |
                       |  Heap (grows up)       |
                       |  (brk/sbrk)            |
                       +------------------------+
                       |  .bss                  |
                       |  .data                 |
                       +------------------------+
                       |  .text (code)          |
0x0000_0000_0040_0000  +------------------------+  User base (~4 MB)
                       |  (unmapped guard)      |
0x0000_0000_0000_0000  +------------------------+
```

### Translation Tables (aarch64)

MINIX 4 uses 4KB granule with 4-level translation:

| Level | Bits | Entries | Maps |
|-------|------|---------|------|
| L0 (PGD) | [47:39] | 512 | 512 GB each |
| L1 (PUD) | [38:30] | 512 | 1 GB each |
| L2 (PMD) | [29:21] | 512 | 2 MB each |
| L3 (PTE) | [20:12] | 512 | 4 KB each |

- `TTBR0_EL1` -- User-space page tables (lower half addresses)
- `TTBR1_EL1` -- Kernel page tables (upper half addresses, shared across all processes)

Each process has its own `TTBR0` value. Context switch updates `TTBR0_EL1`.
`TTBR1_EL1` stays the same (kernel mapping is shared).

### HHDM (Higher Half Direct Map)

Limine maps all physical memory contiguously starting at `hhdm_offset` (typically
`0xFFFF_8000_0000_0000`). The kernel accesses any physical address as:

```rust
fn phys_to_virt(phys: PhysAddr) -> *mut u8 {
    (phys + HHDM_OFFSET) as *mut u8
}

fn virt_to_phys(virt: *const u8) -> PhysAddr {
    virt as usize - HHDM_OFFSET
}
```

This eliminates the need for MINIX 3's `createpde()`/`freepdes[]` mechanism (which
mapped 4MB windows on i386 to access physical memory).

## x86_64

### Virtual Address Space

```
0xFFFF_FFFF_FFFF_FFFF  +------------------------+
                       |                        |
0xFFFF_FFFF_8020_0000  +------------------------+  Kernel virtual base
                       |  Kernel image          |
                       +------------------------+

0xFFFF_8000_0000_0000  +------------------------+  HHDM base
                       |  Direct map            |
                       +------------------------+

                       ... (non-canonical hole) ...

0x0000_7FFF_FFFF_FFFF  +------------------------+  User ceiling
                       |  User space            |
0x0000_0000_0000_0000  +------------------------+
```

### Page Tables (x86_64)

4-level paging (same depth as aarch64):

| Level | Name | Bits | Maps |
|-------|------|------|------|
| L4 | PML4 | [47:39] | 512 GB each |
| L3 | PDPT | [38:30] | 1 GB each |
| L2 | PD | [29:21] | 2 MB each |
| L1 | PT | [20:12] | 4 KB each |

CR3 holds the physical address of the PML4 table. Context switch updates CR3.

## Per-Process Memory

Each user process has its own address space (separate page tables). The kernel
maps itself into the high virtual addresses of every process's page tables, so
the kernel is accessible during system calls without a page table switch.

**Process memory regions:**

| Region | Description |
|--------|-------------|
| Text | Read-only executable code (loaded from ELF) |
| Data | Initialized read-write data |
| BSS | Zero-initialized data |
| Heap | Grows upward via brk()/sbrk() |
| Stack | Grows downward from near the user ceiling |
| mmap | Dynamically mapped regions (files, shared memory, anon) |

VM server manages these regions and handles page faults. When a page fault occurs:
1. Kernel sends VM_PAGEFAULT message to VM server
2. VM determines the faulting region, allocates a physical frame, updates page tables
3. VM calls `sys_vmctl()` to install the page table entry
4. Kernel retries the faulting instruction

## Kernel Memory

The kernel itself has a small heap for dynamic allocations (IRQ hooks, async message
tables). This uses a simple slab allocator initialized after the boot image is parsed.

Most kernel data structures are statically allocated:
- Process table: `[Proc; NR_TASKS + NR_PROCS]` (static array)
- Privilege table: `[Priv; NR_SYS_PROCS]` (static array)
- Run queues: `[Option<ProcNr>; NR_SCHED_QUEUES]` (static array of queue heads)

## Physical Memory Management

Physical frames are tracked by the VM server (not the kernel). VM maintains:
- A free frame list (or bitmap) of 4KB physical pages
- Allocation for process page tables, data pages, and kernel requests
- CoW (copy-on-write) tracking for fork'd pages

The kernel only manipulates physical memory for:
- Its own page tables (kernel portion of address space)
- Safe copy operations (copying between process address spaces)

## MINIX 3 Reference

| Aspect | MINIX 3 |
|--------|---------|
| i386 memory layout | Kernel at `0xF040_0000`, user at `0x0000_0000` - `0xE000_0000` |
| Physical memory access | `createpde()` / `freepdes[]` (4MB windows) |
| Page table setup | `kernel/arch/i386/pg_utils.c` |
| VM server | `servers/vm/` (region.c, pagetable.c, pagefaults.c) |
| Process address space | `servers/vm/region.c` |

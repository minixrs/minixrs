# Boot Sequence

## Bootloader: Limine

minix.rs uses the [Limine](https://github.com/limine-bootloader/limine) bootloader (BSD-licensed).
Limine handles all the complexity of firmware initialization, CPU mode transitions, and initial
page table setup, allowing the kernel to start in a clean 64-bit environment.

**Why Limine:**
- BSD-licensed (no GPL)
- Supports both aarch64 (UEFI) and x86_64 (BIOS + UEFI)
- Handles the entire firmware-to-kernel transition
- Sets up 4-level page tables with Higher Half Direct Map (HHDM)
- Simple request/response protocol (magic structs embedded in kernel ELF)
- Widely used in the OS-dev community

### Limine Protocol (Revision 3)

The kernel communicates with Limine via request structs placed in a `.limine_requests` ELF section.
Limine scans the kernel binary for these structs (identified by magic IDs), fills in response
pointers, and jumps to the kernel entry point.

**Key requests:**

| Request | Purpose |
|---------|---------|
| `limine_memmap_request` | Physical memory map (usable, reserved, etc.) |
| `limine_hhdm_request` | Higher Half Direct Map base offset |
| `limine_paging_mode_request` | 4-level or 5-level paging selection |
| `limine_stack_size_request` | Initial kernel stack size |

## Boot Flow

### aarch64 (Primary -- QEMU virt machine)

```
UEFI firmware (QEMU virt built-in)
  |
  v
Limine bootloader
  |  Reads limine.conf from FAT32 EFI System Partition
  |  Loads kernel ELF64 (aarch64)
  |  Sets up translation tables (4KB granule, 4-level)
  |  Creates Higher Half Direct Map
  |  Fills in Limine response structs
  v
_start (kernel/src/arch/aarch64/entry.S)
  |  CPU at EL1, MMU on, interrupts masked
  |  SP points to Limine-allocated stack
  |  Set frame pointer to 0 (stack unwinding sentinel)
  |  Branch to kmain()
  v
kmain() (kernel/src/main.rs)
  |  1. Parse Limine responses
  |     - Memory map -> KernelInfo.memmap[]
  |     - HHDM offset -> KernelInfo.hhdm_offset
  |  2. Initialize PL011 UART (QEMU virt: 0x0900_0000)
  |     - Print "minix.rs booting on aarch64"
  |  3. Initialize kernel heap (bump allocator -> slab)
  |  4. Unpack embedded boot image -> module_list[]
  |  5. arch_init():
  |     - Configure exception vectors (VBAR_EL1)
  |     - Initialize GICv3 (distributor + redistributor)
  |     - Configure ARM generic timer (CNTV_CTL_EL0)
  |  6. proc_init() -- clear process table
  |  7. For each boot module:
  |     - Allocate process slot
  |     - Load ELF into new translation tables
  |     - Set privilege structure from boot_image[] config
  |     - Mark as RTS_PROC_STOP
  |  8. system_init() -- register kernel call handlers
  |  9. Enable timer interrupt, unmask IRQs
  | 10. Clear RTS_PROC_STOP for all boot processes
  | 11. switch_to_user() -- pick first runnable process
  v
Boot processes start (in dependency order):
  DS -> RS -> PM -> SCHED -> VFS -> memory -> tty -> VM -> PFS -> MFS -> init
```

### x86_64 (Secondary)

Same flow, but with x86_64-specific arch_init:
- GDT/TSS setup
- IDT with exception and IRQ handlers
- APIC initialization (local APIC + I/O APIC)
- SYSCALL/SYSRET MSR configuration
- Serial port (COM1, 0x3F8) for early output

### CPU State at Kernel Entry

**aarch64 (Limine UEFI):**
- Exception Level: EL1
- MMU: enabled, translation tables set up by Limine
- Interrupts: masked (DAIF all set)
- SP: valid stack allocated by Limine
- X0: 0 (all GPRs zeroed except SP)
- HHDM: all physical memory mapped at `hhdm_offset + phys_addr`

**x86_64 (Limine):**
- Mode: 64-bit long mode, paging enabled
- Interrupts: IF cleared
- RSP: valid stack (>= 64 KiB)
- A20: opened
- CR0: PE, PG set
- HHDM: same concept

## Embedded Boot Image

All boot modules (12 server/driver ELF binaries) are packed into a single archive and
embedded in the kernel ELF binary as a `.boot_image` section. This avoids needing the
bootloader to locate and load modules individually from the filesystem.

### Boot Image Format

```
Offset  Size    Field
------  ----    -----
0       4       magic           "MXBI" (0x4942584D)
4       4       version         1
8       4       entry_count     Number of modules
12      4       total_size      Total archive size in bytes

For each entry (64 bytes each):
0       4       offset          Byte offset from archive start
4       4       size            Module size in bytes
8       56      name            Null-terminated ASCII name (e.g., "ds", "pm")
```

### Build Process

1. Cross-compile all boot server/driver ELFs
2. `tools/mkbootimage` packs them into the archive format
3. The archive is linked into the kernel ELF as a binary blob:
   ```
   .boot_image : {
       _boot_image_start = .;
       KEEP(*(.boot_image))
       _boot_image_end = .;
   }
   ```
4. At boot, `unpack_boot_image()` reads the header and populates `module_list[]`

### Boot Process Table

The kernel's boot image table defines the boot processes and their order:

| Slot | Name | Type | Process |
|------|------|------|---------|
| -5 | asyncm | Kernel task | Async message handler |
| -4 | idle | Kernel task | Idle loop |
| -3 | clock | Kernel task | Timer interrupt handler |
| -2 | system | Kernel task | Kernel call dispatcher |
| -1 | kernel | Kernel task | Hardware interrupt routing |
| 0 | ds | Boot module | Data Store |
| 1 | rs | Boot module | Reincarnation Server |
| 2 | pm | Boot module | Process Manager |
| 3 | sched | Boot module | Scheduler |
| 4 | vfs | Boot module | Virtual File System |
| 5 | memory | Boot module | Memory driver |
| 6 | tty | Boot module | Terminal driver |
| 7 | mib | Boot module | Management Information Base |
| 8 | vm | Boot module | Virtual Memory |
| 9 | pfs | Boot module | Pipe File System |
| 10 | mfs | Boot module | MINIX File System |
| 11 | init | Boot module | Init process (PID 1) |

Kernel tasks (negative slots) are internal to the kernel and don't have separate ELF
binaries. Boot modules (slots 0-11) are loaded from the embedded boot image.

## Disk Image Layout

```
+------------------------+------------------------+
| Partition 1            | Partition 2            |
| FAT32 (ESP)            | MinixFS (root)         |
| ~32 MB                 | ~128 MB                |
|                        |                        |
| /EFI/BOOT/BOOTAA64.EFI| /bin, /sbin, /etc      |
| /limine.conf           | /dev, /tmp, /usr       |
| /boot/kernel           | /home                  |
+------------------------+------------------------+
```

### limine.conf

```
timeout: 3

/minix.rs
    protocol: limine
    kernel_path: boot():/boot/kernel
    kernel_cmdline: rootdevname=c0d0p1
```

The kernel is the only file Limine needs to load. Boot modules are embedded in the kernel.

## QEMU Launch

### aarch64 (primary)

```sh
qemu-system-aarch64 \
    -M virt -cpu cortex-a72 -m 256M \
    -bios /path/to/OVMF_AARCH64.fd \     # UEFI firmware
    -drive file=minixrs.img,format=raw,if=virtio \
    -device virtio-net-device \
    -serial stdio \
    -no-reboot
```

### x86_64

```sh
qemu-system-x86_64 \
    -m 256M \
    -drive file=minixrs.img,format=raw,if=virtio \
    -device virtio-net-pci \
    -serial stdio \
    -no-reboot
```

## MINIX 3 Reference

| Aspect | MINIX 3 File |
|--------|-------------|
| Boot process table | `kernel/table.c` |
| Kernel init | `kernel/main.c` (kmain, bsp_finish_booting) |
| i386 entry | `kernel/arch/i386/head.S` |
| i386 pre-init | `kernel/arch/i386/pre_init.c` |
| Limine design | `BOOT.md` (in MINIX 3 source tree root) |

# Boot

This chapter follows minix.rs from firmware to the first user process, as it
boots today on aarch64 under QEMU. The x86_64 path is design intent, part of the
planned port (see [Roadmap](../roadmap.md)).

## Bootloader: Limine

minix.rs boots via the [Limine](https://github.com/limine-bootloader/limine)
boot protocol (BSD-licensed, so it fits the project's BSD-3-Clause-only rule).
Limine handles firmware initialization and the mode transition, and hands the
kernel a clean 64-bit environment: 4-level translation tables already set up, all
physical memory mapped through a Higher-Half Direct Map (HHDM), and a valid stack.

The kernel communicates with Limine through request structs placed in a
`.limine_requests` ELF section. Limine scans the kernel binary for these
(identified by magic IDs), fills in response pointers, and jumps to the entry
point. The kernel consumes, among others, the **memory-map** request (usable /
reserved regions, which seed the frame allocator) and the **HHDM** request (the
direct-map base offset). Limine support lives in
`kernel/src/arch/aarch64/limine.rs`.

## Boot flow (aarch64)

```text
edk2 / UEFI firmware  (QEMU virt)
  │
  ▼
Limine  (external/limine/dist/BOOTAA64.EFI)
  │  reads tools/limine.conf from the FAT EFI System Partition
  │  loads the kernel ELF, sets up 4-level tables + HHDM, fills Limine responses
  ▼
_start  (kernel/src/arch/aarch64/entry.S)
  │  CPU at EL1, MMU on, interrupts masked, SP = Limine-allocated stack
  │  zero the frame pointer, branch to kmain()
  ▼
kmain()  (kernel/src/main.rs)
  │  1. resolve the PL011 UART base through the HHDM; arch::init()
  │  2. print "minix.rs booting on aarch64" and the HHDM offset
  │  3. proc::init() — clear the process/privilege tables; dump them
  │  4. mm::set_hhdm_offset() + mm::init_from_limine_memmap() — seed the frame allocator
  │  5. gic::init() + enable the virtual-timer PPI + timer::init(100 Hz)
  │  6. arch::userland_bootstrap() — load the boot image, install the demo stubs
  │  7. proc::sched::run() — first ERET into EL0
  ▼
User processes run  (scheduler picks the highest-priority runnable proc)
```

There is no hand-ordered "start DS, then RS, then PM, …" sequence:
`userland_bootstrap` loads every boot module and marks it runnable, and the
scheduler takes over from there. Servers rendezvous at run time — each publishes
its endpoint to DS at startup and looks the others up by name — so boot order does
not matter (see [Servers](../servers/overview.md)).

### CPU state at kernel entry

When Limine branches to `_start` on aarch64:

- Exception level **EL1**, MMU **enabled** with Limine's translation tables.
- Interrupts **masked** (all `DAIF` bits set).
- `SP` points to a valid Limine-allocated stack.
- All physical memory is reachable at `phys + hhdm_offset`.

## The embedded boot image (MXBI)

minix.rs does not ask the bootloader to locate server binaries on disk. Instead,
`kernel/build.rs` compiles every boot server for the EL0 user target, packs the
resulting ELFs into a single **MXBI archive**, and the kernel embeds that archive
in its own `.rodata` via `include_bytes!` (`kernel/src/boot_image/mod.rs`). Limine
reports the kernel image — archive included — under `EXECUTABLE_AND_MODULES`, so
those bytes are never visible to the frame allocator.

### Archive format

All multi-byte fields are little-endian
(`kernel/build.rs::pack_mxbi` ↔ `boot_image/mod.rs`):

```text
16-byte header:   magic "MXBI" (u32 = 0x4942_584D), version (u32 = 1),
                  entry_count (u32), total_size (u32)
entry_count × 32-byte records:  { proc_nr:i32, offset:u32, len:u32, name:[u8;20] }
then the ELF payloads back-to-back, each at its recorded offset
```

`BootImage` (`boot_image/mod.rs`) is a zero-copy view: `BootImage::iter` drives
the loader (one `load_boot_server` call per module), and `BootImage::module_by_name`
lets `SYS_EXEC` resolve a target binary — the mechanism `worker` rides on.

### Boot modules

The archive holds eight modules (`kernel/build.rs`'s `servers` array). Seven have
non-negative proc numbers and are loaded into a process slot at boot; `worker` is
tagged with the sentinel `EXEC_ONLY_PROC_NR = -1`, so the loader skips it — it has
no boot slot and exists only to be resolved by name at `exec` time.

| Module | Proc nr | Loaded at boot? |
|--------|--------:|-----------------|
| `pm` | 0 | yes |
| `vfs` | 1 | yes (skeletal) |
| `rs` | 2 | yes |
| `ds` | 5 | yes |
| `vm` | 7 | yes |
| `sched` | 9 | yes |
| `init` | 10 | yes — becomes PID 1 |
| `worker` | −1 | no — exec-only target |

Proc numbers 3 (`MEM`), 4 (`TTY`), 6 (`MFS`), and 8 (`PFS`) are reserved in
`kernel-shared/src/com.rs` for servers that do not yet exist. Kernel tasks
(`ASYNCM`, `IDLE`, `CLOCK`, `SYSTEM`, `HARDWARE`) occupy the negative proc numbers
and are internal to the kernel — they have no ELF binary.

Alongside the boot modules, `userland_bootstrap` hand-installs four demo stubs
(A–D) at proc numbers 11–14. They are not part of the archive; they are small EL0
programs kept as a live regression battery for the IPC primitives, SCHED
delegation, and the page-fault / SIGSEGV paths that `init` and `worker` do not
exercise. So the boot trace shows eleven address-space (`[as]`) lines — six
servers, `init`, and the four stubs — but not `worker`, which is never loaded
until an `exec` names it.

## Disk image and QEMU

There is no partitioned root disk yet. The cargo runner (`tools/qemu-run.sh`)
stages a small ESP *directory* and hands it to QEMU's directory-as-FAT helper,
which is enough to land the boot banner without `parted` / `mtools`:

```text
target/esp/
  EFI/BOOT/BOOTAA64.EFI   ← Limine (external/limine/dist/BOOTAA64.EFI)
  limine.conf             ← tools/limine.conf
  boot/kernel             ← the freshly built kernel ELF
```

`tools/limine.conf` names the kernel and nothing else — the servers are embedded:

```text
timeout: 0
serial: yes

/minix.rs
    protocol: limine
    kernel_path: boot():/boot/kernel
```

The runner then launches QEMU (firmware auto-located, or set
`QEMU_EFI_AARCH64`):

```sh
qemu-system-aarch64 \
    -M virt,gic-version=3 -cpu cortex-a72 -m 256M \
    -bios <edk2-aarch64-code.fd> \
    -drive file=fat:rw:fat-type=32:target/esp,format=raw,if=virtio \
    -display none -serial stdio -no-reboot
```

Because the kernel runs indefinitely once EL0 starts, always wrap a boot run in
`timeout`. See [Build & Toolchain](../build.md) for the full run recipe and the
trace-forensics rules for reading a serial log.

## MINIX 3 reference

| Aspect | MINIX 3 file |
|--------|--------------|
| Boot process table | `kernel/table.c` |
| Kernel init | `kernel/main.c` (`kmain`, `bsp_finish_booting`) |
| i386 entry / pre-init | `kernel/arch/i386/head.S`, `pre_init.c` |

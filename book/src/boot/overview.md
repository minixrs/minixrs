# Boot

This chapter follows minix.rs from firmware to the first user process, as it boots today on aarch64
under QEMU. The x86_64 path is design intent, part of the planned port (see
[Roadmap](../roadmap.md)).

## Bootloader: Limine

minix.rs boots via the [Limine](https://github.com/limine-bootloader/limine) boot protocol
(BSD-licensed, so it fits the project's BSD-3-Clause-only rule). Limine handles firmware
initialization and the mode transition, and hands the kernel a clean 64-bit environment: 4-level
translation tables already set up, all physical memory mapped through a Higher-Half Direct Map
(HHDM), and a valid stack.

The kernel communicates with Limine through request structs placed in a `.limine_requests` ELF
section. Limine scans the kernel binary for these (identified by magic IDs), fills in response
pointers, and jumps to the entry point. The kernel consumes, among others, the **memory-map**
request (usable / reserved regions, which seed the frame allocator) and the **HHDM** request (the
direct-map base offset). Limine support lives in `kernel/src/arch/aarch64/limine.rs`.

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
  │  6. arch::userland_bootstrap() — probe virtio-mmio, load the boot image, install the demo stubs
  │  7. proc::sched::run() — first ERET into EL0
  ▼
User processes run  (scheduler picks the highest-priority runnable proc)
```

There is no hand-ordered "start DS, then RS, then PM, …" sequence: `userland_bootstrap` loads every
boot module and marks it runnable, and the scheduler takes over from there. Servers rendezvous at
run time — each publishes its endpoint to DS at startup and looks the others up by name — so boot
order does not matter (see [Servers](../servers/overview.md)).

### CPU state at kernel entry

When Limine branches to `_start` on aarch64:

- Exception level **EL1**, MMU **enabled** with Limine's translation tables.
- Interrupts **masked** (all `DAIF` bits set).
- `SP` points to a valid Limine-allocated stack.
- All physical memory is reachable at `phys + hhdm_offset`.

## The embedded boot image (MXBI)

minix.rs does not ask the bootloader to locate server binaries on disk. Instead, `kernel/build.rs`
compiles every boot server for the EL0 user target, packs the resulting ELFs into a single **MXBI
archive**, and the kernel embeds that archive in its own `.rodata` via `include_bytes!`
(`kernel/src/boot_image/mod.rs`). Limine reports the kernel image — archive included — under
`EXECUTABLE_AND_MODULES`, so those bytes are never visible to the frame allocator.

### Archive format

All multi-byte fields are little-endian (`kernel/build.rs::pack_mxbi` ↔ `boot_image/mod.rs`):

```text
16-byte header:   magic "MXBI" (u32 = 0x4942_584D), version (u32 = 1),
                  entry_count (u32), total_size (u32)
entry_count × 32-byte records:  { proc_nr:i32, offset:u32, len:u32, name:[u8;20] }
then the ELF payloads back-to-back, each at its recorded offset
```

`BootImage` (`boot_image/mod.rs`) is a zero-copy view: `BootImage::iter` drives the loader (one
`load_boot_server` call per module), and `BootImage::module_by_name` lets `SYS_EXEC` resolve a
target binary — the mechanism `worker` rides on.

### Boot modules

The archive holds thirteen modules (`kernel/build.rs`'s `servers` array). Eleven have non-negative
proc numbers and are loaded into a process slot at boot; `worker` and `bigprog` (a fixture for the
user VA map) are tagged with the sentinel `EXEC_ONLY_PROC_NR = -1`, so the loader skips them — they
have no boot slot and exist only to be resolved by name at `exec` time. The array's order is the
order the boot log's `[as]` lines appear in, and it is load-bearing: DS first so every later publish
lands, then each server before its first client's lookup (`ds < tty < memory < mfs < vfs < pm`).
`vblk` sits between `memory` and `mfs`, where slice 6.3's DS chain will need it.

| Module    | Proc nr | Loaded at boot?       |
| --------- | ------: | --------------------- |
| `vm`      |       7 | yes                   |
| `ds`      |       5 | yes                   |
| `tty`     |       4 | yes                   |
| `memory`  |       3 | yes                   |
| `vblk`    |      11 | yes — the virtio disk |
| `mfs`     |       6 | yes                   |
| `vfs`     |       1 | yes                   |
| `sched`   |       9 | yes                   |
| `rs`      |       2 | yes                   |
| `pm`      |       0 | yes                   |
| `init`    |      10 | yes — becomes PID 1   |
| `worker`  |      −1 | no — exec-only target |
| `bigprog` |      −1 | no — exec-only target |

Proc number 8 (`PFS`) is reserved in `kernel-shared/src/com.rs` for a server that does not yet
exist, and so are the three virtio driver slots that have no ELF: 12 (`vcon`), 13 (`vinput`) and 14
(`vnet`). A reserved slot is fully wired — a `BootEntry` row, a privilege slot, `ipc_to` edges — and
stays blocked on `RTS_NO_PRIV` until its module arrives. Kernel tasks (`ASYNCM`, `IDLE`, `CLOCK`,
`SYSTEM`, `HARDWARE`) occupy the negative proc numbers and are internal to the kernel — they have no
ELF binary. `NR_BOOT_PROCS` is 15: slots `0..=14`.

Alongside the boot modules, `userland_bootstrap` hand-installs four demo stubs (A–D) at proc numbers
15–18, just past the boot image; PM's fork pool starts at 19. They are not part of the archive; they
are small EL0 programs kept as a live regression battery for the IPC primitives, SCHED delegation,
and the page-fault / SIGSEGV paths that `init` and `worker` do not exercise. So the boot trace shows
fifteen address-space (`[as]`) lines — ten servers and drivers, `init`, and the four stubs — but
neither exec-only module, which is never loaded until an `exec` names it.

### The virtio probe

Before `userland_bootstrap` loads the first module it probes QEMU's 32 virtio-mmio transports and
reserves each assigned driver's DMA run, because the run must be carved out before the first frame
is ever freed (`device_teardown_selftest`, a few lines later, is the first caller of `free_frame`).
What the probe reads and decides is described in
[the drivers chapter](../drivers/overview.md#the-kernel-finds-the-device), and the run in
[the memory chapter](../memory/overview.md#the-dma-window).

## Disk image and QEMU

There is no partitioned root disk yet — the virtio disk below is blank and nothing reads it until
slice 6.3. The cargo runner (`tools/qemu-run.sh`) stages a small ESP *directory* and hands it to
QEMU's directory-as-FAT helper, which is enough to land the boot banner without `parted` / `mtools`:

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

The runner then launches QEMU (firmware auto-located, or set `QEMU_EFI_AARCH64`):

```sh
qemu-system-aarch64 \
    -M virt,gic-version=3 -cpu cortex-a72 -m 256M \
    -bios <edk2-aarch64-code.fd> \
    -drive file=fat:rw:fat-type=32:target/esp,format=raw,if=virtio \
    -global virtio-mmio.force-legacy=false \
    -drive file=target/vblk-blank.img,format=raw,if=none,id=vblk0 \
    -device virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.0 \
    -display none -serial stdio -no-reboot
```

The first drive is a virtio-blk-**pci** device that only the firmware reads. The second is the one
minix.rs drives: a blank 2560-sector image (`dd`-created by the runner when absent) on a
virtio-blk-**mmio** device pinned to transport slot 0, which the kernel's
[boot probe](#the-virtio-probe) finds and hands to `virtio-blk`. `force-legacy=false` makes every
mmio transport modern (version 2), the only version the drivers accept; a later `-global` on the
command line overrides it, which is how the version-refusal boot is produced.

Because the kernel runs indefinitely once EL0 starts, always wrap a boot run in `timeout`. See
[Build & Toolchain](../build.md) for the full run recipe and the trace-forensics rules for reading a
serial log.

## MINIX 3 reference

| Aspect                | MINIX 3 file                                    |
| --------------------- | ----------------------------------------------- |
| Boot process table    | `kernel/table.c`                                |
| Kernel init           | `kernel/main.c` (`kmain`, `bsp_finish_booting`) |
| i386 entry / pre-init | `kernel/arch/i386/head.S`, `pre_init.c`         |

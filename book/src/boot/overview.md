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

There is no hand-ordered "start DS, then RS, then PM, …" sequence in the kernel:
`userland_bootstrap` loads every boot module and marks it runnable, and the scheduler takes over
from there. Servers rendezvous at run time — each publishes its endpoint to DS at startup and looks
the others up by name (see [Servers](../servers/overview.md)). That does not make the order
irrelevant: nothing guarantees a publish precedes the lookup that needs it, and it holds only
because of the order the modules are packed in, which is therefore load-bearing — see
[Boot modules](#boot-modules).

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

The archive holds fourteen modules: thirteen ELFs (`kernel/build.rs`'s `servers` array) and one that
is not a program at all, the `rootfs` filesystem image. Eleven of the ELFs have non-negative proc
numbers and are loaded into a process slot at boot; `worker` and `bigprog` (a fixture for the user
VA map) are tagged with the sentinel `EXEC_ONLY_PROC_NR = -1`, so the loader skips them — they have
no boot slot and exist only to be resolved by name at `exec` time. `rootfs` carries `-1` too and is
never loaded as a process: the kernel copies it into the ramdisk. The array's order is the order the
boot log's `[as]` lines appear in, and it is load-bearing: DS first so every later publish lands,
then each server before its first client's lookup (`ds < tty < memory < vblk < mfs < vfs < pm`).
`vblk` sits before `mfs` because MFS asks DS for it first when it
[chooses its root device](../servers/overview.md#choosing-the-root-device), and a `vblk` that had
not yet published would read as "no virtio-mmio disk attached".

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
| `rootfs`  |      −1 | no — not an ELF       |

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

The machine boots from **one disk**. A host tool, `tools/mkimage`, writes a GPT image holding a
FAT32 EFI System Partition (Limine, its configuration and the kernel) and the MinixFS root
partition; the cargo runner (`tools/qemu-run.sh`) builds that image on every run and attaches it as
a single virtio-blk device. The firmware reads the kernel from it, and minix.rs's
[`virtio-blk` driver](../drivers/overview.md#virtio-blk) then brings the same device up and serves
the root partition from it. The runner attaches nothing else, and nothing else on the machine holds
a kernel, so a kernel banner in the log is itself the proof that edk2 and Limine read the image.

### The disk image

Sectors are 512 bytes. Every number below is derived in `tools/mkimage/src/layout.rs` from a handful
of inputs — the sector size, the shape of the GPT entry array, a 1 MiB alignment, the ESP's size and
`rootfs::ROOTFS_IMAGE_BYTES` — and a host test pins the literals, because the boot-marker files
carry two of them (`lba=71680` and `capacity=75776`).

| LBA         | Sectors | Holds                                                                      |
| ----------- | ------: | -------------------------------------------------------------------------- |
| 0           |       1 | protective MBR: one record of type `0xEE` covering the disk                |
| 1           |       1 | primary GPT header                                                         |
| 2–33        |      32 | primary partition entry array: 128 entries of 128 bytes                    |
| 2048–71679  |  69 632 | partition 1: the ESP, FAT32, 34 MiB                                        |
| 71680–73727 |   2 048 | partition 2: the root, exactly the 1 MiB root image (absent when rootless) |
| 75743–75774 |      32 | backup partition entry array                                               |
| 75775       |       1 | backup GPT header                                                          |

The disk is 75 776 sectors — 37 MiB, 38 797 312 bytes — rounded up to a whole mebibyte, and it is
the same size **with or without** the root partition: a rootless image leaves entry 2 unused and the
root's range zero, so the driver's `capacity=` marker reads the same on both. Both partitions start
on a 1 MiB boundary, which makes each start on a 4096-byte block boundary — a requirement for
anything the driver [serves as a minor](../drivers/overview.md#minors). Const assertions in
`layout.rs` hold the alignment, the ordering and the sizes, so a layout that broke one of them does
not compile.

**The root partition is found by its type GUID**, `10FDAC43-2E55-4E0E-8D18-2D46479E1B44`, generated
for minix.rs and defined once, as its 16 on-disk bytes, in `kernel-shared::rootfs`
(`ROOT_PARTITION_TYPE_GUID`). The tool writes it and the driver selects by it; neither depends on
the other. GPT stores a GUID mixed-endian — the first three fields little-endian, the rest as
written — and a test pins the bytes to the text form.

**The image is reproducible.** The disk GUID and both partitions' unique GUIDs are constants in the
tool, FAT timestamps are zero and the volume serial is fixed, so the same inputs give the same bytes
and two images can be compared across runs.

The root partition's bytes are not produced by the tool. `kernel/build.rs` writes the MinixFS image
it packs into the boot archive to `rootfs.img` beside the kernel ELF, and `mkimage` copies that file
into the partition byte for byte — refusing one that is not exactly `ROOTFS_IMAGE_BYTES` long or
does not begin with the image label. The disk's root and the ramdisk's are therefore one image by
construction.

#### The ESP

The ESP is a FAT32 volume written whole and never modified, from Microsoft's FAT specification.
Three properties matter:

- **One sector per cluster, and at least 65 525 clusters.** A FAT volume's type is decided by its
  cluster count and by nothing else: below 65 525 clusters a conforming driver reads it as FAT16
  whatever the boot sector says. At one sector per cluster that bound is what sizes the ESP — 34 MiB
  gives 68 520 clusters (two FATs of 540 sectors, the first data sector at 1112) — and a const
  assertion holds it, so shrinking the ESP until it fails is a compile error rather than a volume
  the firmware misreads.
- **Long-name entries.** `limine.conf` does not fit an 8.3 name, so the tool writes VFAT long-name
  entries carrying each name's exact spelling in front of a generated short alias: the name
  upper-cased when that loses nothing (`kernel` → `KERNEL`), otherwise a basis with a numeric tail
  (`limine.conf` → `LIMINE~1.CON`). A name that is already upper-case 8.3 (`BOOTAA64.EFI`) gets a
  short entry alone. Paths are ASCII, and two paths that differ only in case are refused.
- **Contiguous allocation.** The root directory, then every other directory, then every file, each a
  run of consecutive clusters whose FAT entries name the next. The boot record (boot sector, FSInfo
  and a third signature sector) is duplicated at sectors 6–8, as a formatter does.

The runner puts three files on it:

```text
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

#### `tools/mkimage`

`minixrs-mkimage` is a workspace host crate — a library and a thin binary, with `kernel-shared` as
its only dependency and no third-party crate:

```text
mkimage <out.img> [--root <rootfs.img>] <esp-path>=<host-file>...
```

Without `--root` the image has no root partition. At least one ESP file is required. The image is
built in memory (`build_image`) and written sparsely (`write_sparse` seeks over runs of zeros), so
the 37 MiB file occupies only a few MiB on a filesystem with sparse files.

**The writer and the driver's parser share no code.** `tools/mkimage/src/gpt.rs` and
`drivers/driver-rt/src/gpt.rs` each take their field offsets from the UEFI specification, and each
has its own CRC-32 (table-driven in the tool, bit-at-a-time in the driver). The tool's integration
test, `tools/mkimage/tests/image.rs`, can reach only the crate's public API and checks a built image
with readers that are not the writer: a FAT32 reader of its own that parses the boot sector, walks
the FAT chains and reassembles long names; `driver-rt`'s parser, which must find the root by type
and whose bytes must equal the input; and fields read straight from the offsets the specification
gives. edk2, which reads the same GPT and FAT32 to find Limine on every boot, is a third independent
reader.

### The QEMU command line

The runner launches QEMU (firmware auto-located, or set `QEMU_EFI_AARCH64`), and prints the exact
command line on stderr as one `qemu-run: …` line:

```sh
qemu-system-aarch64 \
    -M virt,gic-version=3 -cpu cortex-a72 -m 256M \
    -bios <edk2-aarch64-code.fd> \
    -global virtio-mmio.force-legacy=false \
    -drive file=target/disk.img,format=raw,if=none,id=vblk0 \
    -device virtio-blk-device,drive=vblk0,bus=virtio-mmio-bus.0 \
    -display none -serial stdio -no-reboot
```

The one drive is a virtio-blk-**mmio** device pinned to transport slot 0, which the kernel's
[boot probe](#the-virtio-probe) finds and hands to `virtio-blk`. `force-legacy=false` makes every
mmio transport modern (version 2), the only version the drivers accept, and edk2 boots from a modern
one; a later `-global` on the command line overrides it, which is how the version-refusal boot is
produced. There is no directory-as-FAT drive, and on this default command line the runner attaches
no PCI device of its own (the `nodisk` backend below is the one case where it does). The machine is
not PCI-free for all that: `-M virt` still creates its default devices, a `virtio-net-pci` network
card among them, because the runner does not pass `-nodefaults`. Nothing in minix.rs sees it.

### The root backend

The root filesystem can come from either of two block drivers, and **what is on the attached disk
selects it**. The environment variable `MINIXRS_ROOT` chooses what the runner builds and how it
attaches it:

| `MINIXRS_ROOT`   | What the runner attaches                                                | Where MFS mounts its root from                                |
| ---------------- | ----------------------------------------------------------------------- | ------------------------------------------------------------- |
| `disk` (default) | `target/disk.img`: ESP + root partition, on virtio-mmio                 | `virtio-blk`, minor 0 — the root partition                    |
| `ramdisk`        | `target/disk-rootless.img`: the same image, no root partition           | `memory`, after `virtio-blk` declined the disk — the fallback |
| `nodisk`         | the rootless image as virtio-blk-**PCI**, which only the firmware reads | `memory`; `virtio-blk` has no device at all                   |

A machine with no disk has nothing to boot from, so the `ramdisk` boot still attaches one. The
driver brings it up, finds no partition of the root type and prints `root FAIL no root partition`;
MFS, having found `vblk` in DS, is answered `ENXIO`, prints `bdev.root vblk rc=-6 fallback=memory`
and mounts the ramdisk (see
[Choosing the root device](../servers/overview.md#choosing-the-root-device)). That is the boot CI
runs as its second backend. `nodisk` is the only way to reach the state where the driver has no
device — minix.rs has no PCI — and is run by hand: CI does not boot it and it has no marker pair.
Any other value is an error.

The image is **rebuilt on every run**, by a `cargo run -q --release -p minixrs-mkimage` nested
inside the `cargo run` that started the runner. The boot writes to its root and the driver flushes
every write, so a reused image would make the second run a different test from the first. The two
flavours go to different files, so a `ramdisk` boot does not destroy the image a `disk` boot left
behind. The ramdisk is still built and mapped on a `disk` boot — the `rootfs` module is in the
archive either way — it is simply not the device MFS selects.

Two further variables apply to a `disk` boot only (the other two values reject them):

- `MINIXRS_VBLK_IMAGE=<file>` attaches that file **as-is**: no build, no copy, and the boot's writes
  land in it. The machine then boots *entirely* from that file — firmware, Limine and the kernel all
  come off its ESP, and the kernel ELF cargo just built is not used. A file with no ESP, such as the
  raw `rootfs.img`, gives no kernel at all, only the firmware's boot-failure output.
- `MINIXRS_VBLK_DRIVE_OPTS=<opts>` appends options to the `-drive` argument, such as `readonly=on`,
  which makes the driver print `root FAIL readonly` and MFS fall back.

### Booting the same image twice

A write to the disk root survives the machine, which the ramdisk could never show. The root image
ships `/etc/boots` holding the digit `0`; `init` prints what it reads there as `minix.rs init:
persist gen=<n>` and stores the next digit
([the persistence probe](../servers/overview.md#the-persistence-probe)). A fresh image prints
`gen=0`. Booting the image a `disk` boot just wrote, with no rebuild, prints `gen=1`:

```sh
timeout 450 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > disk.log 2>&1
MINIXRS_VBLK_IMAGE=target/disk.img timeout 60 \
    cargo run -p minixrs-kernel --target aarch64-unknown-none --release > reboot.log 2>&1
tools/check-boot-log.sh reboot reboot.log
```

The second command must run before anything rebuilds `target/disk.img`, which every plain `disk`
boot does. Run it once per disk boot: every boot of the image stores the next digit, so a second
reboot of the same image prints `gen=2` and fails the reboot set by design; take a fresh disk boot
first. The reboot is checked against its own marker pair and nothing else, because `init`'s write
battery was designed against a fresh image; it needs only the probe's line, so it is a short boot.
What it shows is that writes **reach the device**. It shows nothing about the flush: QEMU serves a
raw image through the host's page cache, so the first boot's writes are in the file whether or not
the guest flushed them.

### The firmware writes to the disk too

On a **writable** disk edk2 repairs a damaged primary GPT header or primary entry array from the
backup copy — before the kernel runs (observed with Homebrew's edk2 under QEMU 11.0.3). An image
with one bit flipped in either therefore boots normally, the driver accepts it, and afterwards the
image's primary GPT sectors have been rewritten by the firmware: a write to the boot disk that no
minix.rs code made. Two consequences. The driver consults only the primary table, but its refusals
of a damaged one are reachable only on a disk the firmware cannot repair — one attached
`readonly=on`. And "the image is byte-identical afterwards" is evidence about minix.rs only where
the firmware had nothing to fix.

Because the kernel runs indefinitely once EL0 starts, always wrap a boot run in `timeout`. See
[Build & Toolchain](../build.md) for the full run recipe and the trace-forensics rules for reading a
serial log.

## MINIX 3 reference

| Aspect                | MINIX 3 file                                    |
| --------------------- | ----------------------------------------------- |
| Boot process table    | `kernel/table.c`                                |
| Kernel init           | `kernel/main.c` (`kmain`, `bsp_finish_booting`) |
| i386 entry / pre-init | `kernel/arch/i386/head.S`, `pre_init.c`         |

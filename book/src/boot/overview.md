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
request (usable / reserved regions, which seed the frame allocator), the **HHDM** request (the
direct-map base offset) and, since slice 6.7, the **framebuffer** request (the screen, if the
machine has one — see [The boot framebuffer](#the-boot-framebuffer)). Limine support lives in
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
  │  4. framebuffer::init() — read and validate Limine's framebuffer; allocates nothing
  │  5. mm::set_hhdm_offset() + mm::init_from_limine_memmap(hole) — seed the frame allocator
  │     around the framebuffer; then print the one [fb] line
  │  6. gic::init() + enable the virtual-timer PPI + timer::init(100 Hz)
  │  7. arch::userland_bootstrap() — probe virtio-mmio, load the boot image, install the demo stubs
  │  8. proc::sched::run() — first ERET into EL0
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

## The boot framebuffer

Since slice 6.7 the kernel asks Limine for a framebuffer (the framebuffer request, revision 0) and,
if it likes the answer, gives the screen to the TTY driver. The kernel never draws on it. It decides
once, at boot, and three later steps act on the one record of that decision
(`kernel/src/arch/aarch64/framebuffer.rs`): the frame allocator's seed, which keeps the
framebuffer's pages out of the allocator; the boot loader's TTY arm, which maps them into TTY; and
`SYS_GETINFO(GET_FRAMEBUFFER)`, which tells TTY the geometry. Because all three read one copy, the
range kept out, the range mapped and the geometry reported cannot disagree.

No resolution is requested, in the kernel or in `limine.conf`: the kernel takes whatever mode the
firmware set. Under QEMU that is `ramfb`'s default with edk2 — 800×600, 32 bits per pixel — as
measured on QEMU 11.0.3. Only the **first** framebuffer Limine lists is looked at.

### What the kernel accepts

`limine::framebuffer()` copies the fields out of Limine's response and hands them to a pure
function, `kernel-shared::framebuffer::validate`, which is host-tested because the kernel crate
cannot be. The conditions are checked in this order and the first that fails names the refusal:

| Token       | Condition                                                                                    |
| ----------- | -------------------------------------------------------------------------------------------- |
| `bpp`       | 32 bits per pixel                                                                            |
| `model`     | the RGB memory model                                                                         |
| `mask-size` | each of the three colour masks is 8 bits wide                                                |
| `shift`     | each mask's shift is 0, 8, 16 or 24, and the three are distinct                              |
| `size`      | width and height are non-zero                                                                |
| `pitch`     | the pitch is at least `width * 4` and a multiple of 4                                        |
| `address`   | the address lies in the higher-half direct map, and its page-rounded end does not pass 2^48  |
| `align`     | the physical address is page-aligned                                                         |
| `window`    | `pitch * height` fits [the framebuffer window](../memory/overview.md#the-framebuffer-window) |

Every product and sum is checked for overflow. The 2^48 bound (`PA_LIMIT`) is the reach of a page
descriptor: it holds 48 bits of physical address and the kernel's mapper masks the rest away, so a
framebuffer page above that would be mapped at a different address with no assert firing. Two more
tokens belong to the reading rather than the judging: `pointer`, when a non-empty response carries a
null array or a null first entry, and `address` again when there is no HHDM response to tell a
direct-map address by. One pixel format is accepted because one is all the renderer has: 24 or 16
bits per pixel would need unaligned or merged stores, which a device mapping does not tolerate.

The kernel prints exactly one line:

```text
[fb] ok w=800 h=600 pitch=3200 r=16 g=8 b=0 usable=0 pa=0x...
[fb] none
[fb] FAIL <token>
```

`r=`, `g=` and `b=` are the channel shifts (16, 8, 0 is XRGB8888). `usable=` is the number of pages
that had to be
[carved out of the frame allocator](../memory/overview.md#carving-the-framebuffer-out-of-the-allocator)
— zero wherever the firmware's memory map reports the framebuffer as its own kind, as QEMU's does.
`pa=` is last because it follows the firmware's memory map, so the boot-marker file can assert
everything before it. `[fb] none` is Limine answering with no framebuffer, or not answering; `[fb]
FAIL` is one it reported and the kernel refused. After either there is no screen: nothing is mapped,
`GET_FRAMEBUFFER` answers `ENOENT`, and TTY runs
[serial-only](../drivers/overview.md#asking-for-it). The runner attaches a display to every boot, so
both lines are forbidden markers.

### Why it comes before the allocator

The framebuffer is read and validated **before** the frame allocator is seeded, and nothing between
the two allocates — the read only follows Limine's pointers and records the result. The allocator is
then seeded *around* the accepted range. Done in the other order, a frame already handed out could
turn out to be part of the screen. The `[fb]` line is printed after the seed rather than at the
decision, because it reports how many pages the seed removed.

The mapping itself happens later, when `userland_bootstrap` loads TTY, and prints its own line:

```text
[devmap] tty fb va=0x100000000 pages=469 pa=0x...
```

`pages=` is `ceil(pitch * height / 4096)`: 1 920 000 bytes is 468.75 pages.

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

The archive holds fifteen modules: fourteen ELFs (`kernel/build.rs`'s `servers` array) and one that
is not a program at all, the `rootfs` filesystem image. Twelve of the ELFs have non-negative proc
numbers and are loaded into a process slot at boot; `worker` and `bigprog` (a fixture for the user
VA map) are tagged with the sentinel `EXEC_ONLY_PROC_NR = -1`, so the loader skips them — they have
no boot slot and exist only to be resolved by name at `exec` time. `rootfs` carries `-1` too and is
never loaded as a process: the kernel copies it into the ramdisk. The array's order is the order the
boot log's `[as]` lines appear in, and it is load-bearing: DS first so every later publish lands,
then each server before its first client's lookup (`ds < tty < memory < vblk < mfs < vfs < pm`, and
`vcon < vfs`). `vblk` sits before `mfs` because MFS asks DS for it first when it
[chooses its root device](../servers/overview.md#choosing-the-root-device), and a `vblk` that had
not yet published would read as "no virtio-mmio disk attached". `vcon` sits directly after `vblk`;
all its place has to satisfy is that it comes before `vfs`, which looks it up at start-up and, on a
miss, falls back to the boot endpoint and says so.

| Module    | Proc nr | Loaded at boot?          |
| --------- | ------: | ------------------------ |
| `vm`      |       7 | yes                      |
| `ds`      |       5 | yes                      |
| `tty`     |       4 | yes                      |
| `memory`  |       3 | yes                      |
| `vblk`    |      11 | yes — the virtio disk    |
| `vcon`    |      12 | yes — the virtio console |
| `mfs`     |       6 | yes                      |
| `vfs`     |       1 | yes                      |
| `sched`   |       9 | yes                      |
| `rs`      |       2 | yes                      |
| `pm`      |       0 | yes                      |
| `init`    |      10 | yes — becomes PID 1      |
| `worker`  |      −1 | no — exec-only target    |
| `bigprog` |      −1 | no — exec-only target    |
| `rootfs`  |      −1 | no — not an ELF          |

Proc number 8 (`PFS`) is reserved in `kernel-shared/src/com.rs` for a server that does not yet
exist, and so are the two virtio driver slots that have no ELF: 13 (`vinput`) and 14 (`vnet`). A
reserved slot is fully wired — a `BootEntry` row, a privilege slot, `ipc_to` edges — and stays
blocked on `RTS_NO_PRIV` until its module arrives. Kernel tasks (`ASYNCM`, `IDLE`, `CLOCK`,
`SYSTEM`, `HARDWARE`) occupy the negative proc numbers and are internal to the kernel — they have no
ELF binary. `NR_BOOT_PROCS` is 15: slots `0..=14`.

Alongside the boot modules, `userland_bootstrap` hand-installs four demo stubs (A–D) at proc numbers
15–18, just past the boot image; PM's fork pool starts at 19. They are not part of the archive; they
are small EL0 programs kept as a live regression battery for the IPC primitives, SCHED delegation,
and the page-fault / SIGSEGV paths that `init` and `worker` do not exercise. So the boot trace shows
sixteen address-space (`[as]`) lines — eleven servers and drivers, `init`, and the four stubs — but
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
the root partition from it. Nothing else on the machine holds a kernel, so a kernel banner in the
log is itself the proof that edk2 and Limine read the image. The runner attaches two other devices,
on every boot: a virtio console, which is [a second character device](#the-second-console) and has
nothing to boot from, and [a display](#the-display).

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
    -chardev pipe,id=hvc0,path=target/hvc0 \
    -device virtio-serial-device,bus=virtio-mmio-bus.8 \
    -device virtconsole,chardev=hvc0 \
    -device ramfb \
    -qmp unix:target/qmp.sock,server=on,wait=off \
    -display none -serial stdio -no-reboot
```

The one drive is a virtio-blk-**mmio** device pinned to transport slot 0, which the kernel's
[boot probe](#the-virtio-probe) finds and hands to `virtio-blk`. `force-legacy=false` makes every
mmio transport modern (version 2), the only version the drivers accept, and edk2 boots from a modern
one; a later `-global` on the command line overrides it, which is how the version-refusal boot is
produced. The `hvc0` chardev and the two device lines after it are
[the second console](#the-second-console); `ramfb`, the QMP socket and `-display none` are
[the display](#the-display). There is no directory-as-FAT drive, and on this default command line
the runner attaches no PCI device of its own (the `nodisk` backend below is the one case where it
does). The machine is not PCI-free for all that: `-M virt` still creates its default devices, a
`virtio-net-pci` network card among them, because the runner does not pass `-nodefaults`. Nothing in
minix.rs sees it.

### The second console

Since slice 6.6 every boot — whatever its root backend, the reboot and the hand-run `nodisk` boot
included — also carries a virtio console: a `virtio-serial-device` pinned to `virtio-mmio-bus.8`,
transport slot 8, the first slot of the second register page, with a single `virtconsole` port.
`virtconsole` rather than `virtserialport` because QEMU reserves port 0 for a console, and port 0 is
the only port a driver that does not negotiate the multiport feature can see. The kernel's probe
assigns it to the [`virtio-console` driver](../drivers/overview.md#virtio-console), which serves it
as `/dev/hvc0`. Attaching it always means there is one machine shape, not two, and the reboot proves
the same driver path as the boot before it.

Its host side is a `pipe` chardev over two FIFOs under `target/`:

| File              | Direction    | Carries                                |
| ----------------- | ------------ | -------------------------------------- |
| `target/hvc0.out` | guest → host | what is written to `/dev/hvc0`         |
| `target/hvc0.in`  | host → guest | what a read of `/dev/hvc0` will return |

QEMU appends `.in` and `.out` to the chardev's path itself, and the runner creates both with
`mkfifo` when they are missing. `MINIXRS_HVC0=<base>` moves the pair to `<base>.in` and
`<base>.out`, so that two boots run at once do not share one. A regular file in a FIFO's place is
refused (exit 73) rather than replaced. The serial log carries none of the second console's bytes.

Nobody has to be on either end. QEMU opens both FIFOs read-write, so it starts with no peer, and a
guest write with no reader completes: the bytes sit in the pipe. Two things about that pipe are
worth knowing before reading it. The firmware uses the port too — edk2 and Limine put about 800
bytes of boot text on it before the kernel starts — so a reader must look for its line among other
output. And the pipe is not reliably emptied when QEMU exits: on macOS the bytes of an unread boot
were still in `target/hvc0.out` afterwards with no process holding it, which is why
[the feeder](../build.md#feeding-the-console) discards whatever it finds there before it starts
looking, and why unread boots with no fed boot between them accumulate.

### The display

Since slice 6.7 every boot also has a display: `-device ramfb`, a plain framebuffer the firmware
sets a mode on, Limine [reports to the kernel](#the-boot-framebuffer) and TTY draws the console on.
It is not a virtio device and sits on no virtio-mmio bus. It is attached to **every** boot for the
second console's reason — one machine shape, not two — so that CI and a boot somebody watches run
the same code: a TTY with a screen. Dropping the device by hand is the way to see the other path,
where the kernel prints `[fb] none` and TTY is serial-only.

Whether anything *shows* the display is separate, and is chosen by `MINIXRS_DISPLAY`:

| `MINIXRS_DISPLAY` | What the runner does                                                           |
| ----------------- | ------------------------------------------------------------------------------ |
| `none` (default)  | passes `-display none`: no window. The framebuffer is still there and drawn on |
| `window`          | leaves `-display none` off, so QEMU opens its default UI (cocoa on macOS)      |

Any other value is refused (exit 64): a misspelt `window` would otherwise run as a perfectly good
headless boot and look like a display that does not work. The serial console stays on stdio either
way.

A display nobody can see still has to be inspectable, so every boot also gets a **QMP socket**:
`-qmp unix:<socket>,server=on,wait=off`. QEMU listens without waiting for a client, so a boot nobody
connects to runs exactly as before. Through it a script can stop the guest, ask for a `screendump`
and let it run again, which is what [the screen check](../build.md#the-screen-check) is built on.

The socket is `target/qmp.sock`; `MINIXRS_QMP=<socket>` moves it, so that two boots run at once do
not share one. The runner refuses a path of 104 bytes or more (exit 64): that is all a unix socket's
address holds on macOS, the default lives under the checkout — whose depth is not the runner's to
choose — and QEMU's own error does not name the variable to change. The limit is applied on Linux
too, so that a path which works in CI works on a laptop. A socket left behind by an earlier boot is
removed just before QEMU starts, because QEMU does not unlink it when it is killed and will not bind
over one; anything else at that path is refused (exit 73) rather than replaced. All three checks run
before the disk image is built, so a refused boot costs nothing.

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
timeout 600 cargo run -p minixrs-kernel --target aarch64-unknown-none --release > disk.log 2>&1
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

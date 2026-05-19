# Driver Model

## Overview

In MINIX 4, device drivers are user-space processes. They communicate with the kernel
(for interrupts and I/O access) and with VFS (for device protocols) via IPC messages.
This isolation means a buggy driver cannot corrupt kernel memory or crash the system --
at worst, its own process dies and RS restarts it.

## Driver Types

### Block Drivers (BDEV protocol)

Block drivers handle storage devices (disks, ramdisks). They respond to messages from VFS:

| Message | Description |
|---------|-------------|
| `BDEV_OPEN` | Open device |
| `BDEV_CLOSE` | Close device |
| `BDEV_READ` | Read sectors |
| `BDEV_WRITE` | Write sectors |
| `BDEV_GATHER` | Vectored read (scatter-gather) |
| `BDEV_SCATTER` | Vectored write |
| `BDEV_IOCTL` | Device-specific control |

Data transfer uses grant-based memory access: VFS creates a grant for the user's buffer,
passes the grant ID to the driver, and the driver uses `sys_safecopyfrom/to` to transfer
data through the kernel without direct memory access.

### Character Drivers (CDEV protocol)

Character drivers handle byte-stream devices (terminals, serial ports, /dev/null):

| Message | Description |
|---------|-------------|
| `CDEV_OPEN` | Open device |
| `CDEV_CLOSE` | Close device |
| `CDEV_READ` | Read bytes |
| `CDEV_WRITE` | Write bytes |
| `CDEV_IOCTL` | Device-specific control |
| `CDEV_SELECT` | Poll for readiness |

## Driver Lifecycle

1. **Boot or RS_UP:** Driver process starts (either as a boot module or started by RS)
2. **SEF init:** Driver calls `sef_startup()`, receives init message from RS
3. **Announce:** Driver sends announcement to VFS: "I handle device major X"
4. **Message loop:** Driver enters `sef_receive()` loop, handling BDEV/CDEV messages
5. **Interrupts:** Hardware interrupts arrive as NOTIFY messages from HARDWARE endpoint
6. **Crash recovery:** If the driver dies, RS detects missing heartbeat, restarts it

### Interrupt Handling

Drivers register for interrupts via `sys_irqctl()` kernel call:

```rust
// Register for IRQ
sys_irqctl(IRQ_SETPOLICY, irq_number, hook_id, IRQ_REENABLE)?;
sys_irqctl(IRQ_ENABLE, irq_number, hook_id, 0)?;

// In message loop:
match msg.m_source {
    HARDWARE => {
        // Interrupt fired -- handle it
        handle_interrupt();
        // Re-enable IRQ
        sys_irqctl(IRQ_REENABLE, irq_number, hook_id, 0)?;
    }
    // ... other messages
}
```

The kernel masks the IRQ, then sends a NOTIFY to the driver. The driver processes the
interrupt and re-enables it. This prevents interrupt storms from crashing the system.

## VirtIO Drivers

MINIX 4 targets QEMU, so the primary drivers use VirtIO:

### VirtIO Transport

- **aarch64 (QEMU virt):** VirtIO MMIO -- devices are memory-mapped at fixed addresses
  in the QEMU virt device tree. The driver accesses them via memory-mapped I/O using
  grants from the kernel (`sys_privctl` to add memory ranges).
- **x86_64:** VirtIO PCI -- devices appear on the PCI bus. The driver discovers them via
  PCI configuration space.

### Virtqueues

VirtIO devices use virtqueues for data transfer. Each virtqueue is a ring buffer in
shared memory:

```
Descriptor Table:  array of { addr, len, flags, next }
Available Ring:    producer (driver) writes descriptors here
Used Ring:         consumer (device) writes completed descriptors here
```

The driver allocates virtqueue memory, tells the device its physical address via MMIO/PCI
registers, then communicates by adding descriptors to the available ring and checking the
used ring for completions.

### virtio-blk (Block Storage)

- BDEV protocol handler
- Single virtqueue for all I/O requests
- Supports read, write, flush
- Device type: `VIRTIO_DEVICE_ID_BLOCK (2)`

### virtio-net (Network)

- Two virtqueues: RX (receive) and TX (transmit)
- Packet send/receive via descriptor chains
- MAC address from device config
- Device type: `VIRTIO_DEVICE_ID_NET (1)`

### virtio-console (Terminal)

- CDEV protocol handler
- Two virtqueues: input and output
- Replaces traditional serial/VGA TTY
- Device type: `VIRTIO_DEVICE_ID_CONSOLE (3)`

### memory (Memory Driver)

- Implements /dev/null, /dev/zero, /dev/mem, and ramdisk
- No hardware -- pure software driver
- /dev/null: discards writes, returns EOF on read
- /dev/zero: returns zero bytes on read
- Ramdisk: in-memory block device for initramfs

## Driver Runtime Library (driver-rt)

The `driver-rt` crate provides reusable infrastructure for writing drivers:

```rust
// Block driver trait
pub trait BlockDriver {
    fn open(&mut self, minor: u32) -> Result<(), i32>;
    fn close(&mut self, minor: u32) -> Result<(), i32>;
    fn transfer(&mut self, minor: u32, position: u64,
                grant: CpGrantId, size: usize,
                write: bool) -> Result<usize, i32>;
    fn ioctl(&mut self, minor: u32, request: u32,
             grant: CpGrantId) -> Result<i32, i32>;
}

// Character driver trait
pub trait CharDriver {
    fn open(&mut self, minor: u32) -> Result<(), i32>;
    fn close(&mut self, minor: u32) -> Result<(), i32>;
    fn read(&mut self, minor: u32, grant: CpGrantId,
            size: usize) -> Result<usize, i32>;
    fn write(&mut self, minor: u32, grant: CpGrantId,
             size: usize) -> Result<usize, i32>;
    fn ioctl(&mut self, minor: u32, request: u32,
             grant: CpGrantId) -> Result<i32, i32>;
    fn select(&mut self, minor: u32, ops: u32) -> Result<u32, i32>;
}

// VirtIO transport
pub mod virtio {
    pub struct Virtqueue { /* ring buffers */ }
    pub trait VirtioTransport {
        fn read_config(&self, offset: u32) -> u32;
        fn write_config(&mut self, offset: u32, val: u32);
        fn notify_queue(&mut self, queue: u16);
    }
    pub struct MmioTransport { /* for aarch64 */ }
    pub struct PciTransport { /* for x86_64 */ }
}
```

## Device Registration

When a driver starts, it announces itself to VFS:

```
Driver -> VFS: MAPDRIVER message { major_number, label }
```

VFS updates its device map (`dmap[]`): "major number X is handled by driver with
endpoint Y". When a user opens `/dev/vda` (which maps to major X, minor 0), VFS
forwards the BDEV/CDEV messages to that driver's endpoint.

## MINIX 3 Reference

| Aspect | MINIX 3 File |
|--------|-------------|
| Block driver framework | `lib/libblockdriver/driver.c` |
| Char driver framework | `lib/libchardriver/chardriver.c` |
| Memory driver | `drivers/storage/memory/memory.c` |
| VirtIO library | `drivers/storage/virtio_blk/` |
| Device mapping | `servers/vfs/dmap.c` |
| IRQ handling | `kernel/interrupt.c` |

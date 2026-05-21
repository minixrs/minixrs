//! Per-privilege-slot limits enforced by the kernel.
//!
//! These bound the I/O ranges, IRQ lines, and memory ranges a single
//! privileged process may hold. MINIX 3's caps are larger
//! (`NR_IO_RANGE = 64`, `NR_IRQ = 16`, `NR_MEM_RANGE = 20`); MINIX 4 starts
//! smaller because no slice yet exercises them, and the smaller caps keep
//! the `Priv` struct compact while the privilege table is statically
//! allocated. Resize as servers and drivers come online.

/// Maximum number of I/O port ranges a single privileged process may hold.
pub const NR_IO_RANGE: usize = 16;

/// Maximum number of IRQ lines a single privileged process may register.
pub const NR_IRQ: usize = 8;

/// Maximum number of memory ranges a single privileged process may hold.
pub const NR_MEM_RANGE: usize = 8;

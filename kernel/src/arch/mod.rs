#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

#[cfg(not(target_arch = "aarch64"))]
compile_error!("MINIX 4 currently supports only aarch64; x86_64 arrives in Phase 8");

// Assembles the per-arch entry path and exception vectors into the kernel.
//
// .S files go through clang (cross-targeted at the kernel triple) and the
// resulting .o files are passed straight to the linker via cargo:rustc-link-arg.
// We deliberately *avoid* the cc crate's static-library packaging because the
// linker's archive member resolution wouldn't pull in `_start` (it's only
// referenced via the linker-script `ENTRY` directive, not from any Rust
// symbol). Direct .o linkage sidesteps that, and it also dodges the
// macOS-ar/ELF-object mismatch entirely.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH unset");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR unset"));

    match arch.as_str() {
        "aarch64" => {
            let sources = [
                "src/arch/aarch64/entry.S",
                "src/arch/aarch64/vectors.S",
                "src/arch/aarch64/trap.S",
                "src/arch/aarch64/interrupt.S",
                "src/arch/aarch64/user_stub.S",
            ];
            for src in &sources {
                println!("cargo:rerun-if-changed={src}");
                let stem = std::path::Path::new(src)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy();
                let obj = out_dir.join(format!("{stem}.o"));
                let status = Command::new("clang")
                    .args([
                        "--target=aarch64-unknown-none",
                        "-ffreestanding",
                        "-c",
                        src,
                        "-o",
                    ])
                    .arg(&obj)
                    .status()
                    .expect("failed to spawn clang");
                if !status.success() {
                    panic!("clang failed assembling {src}");
                }
                println!("cargo:rustc-link-arg={}", obj.display());
            }
            println!("cargo:rerun-if-changed=src/arch/aarch64/linker.ld");
        }
        "x86_64" => {
            // Phase 8 territory -- nothing to assemble yet.
        }
        other => panic!("unsupported target arch: {other}"),
    }
}

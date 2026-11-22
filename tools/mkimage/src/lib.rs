// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `mkimage` — the minix.rs boot disk: one GPT image holding a FAT32 EFI System
//! Partition and, optionally, the root filesystem (slice 6.4, decisions P1/P2).
//!
//! ```text
//! protective MBR | GPT | ESP (FAT32: Limine, limine.conf, the kernel) | root | backup GPT
//! ```
//!
//! The firmware reads the GPT to find the ESP and the FAT32 volume to find the
//! boot loader; Limine reads the same volume for its configuration and the
//! kernel; `virtio-blk` reads the GPT to find the root partition by its type
//! GUID. This crate writes all of it, by hand and with no third-party crate —
//! the whole output format is a few hundred lines of fixed-layout bytes.
//!
//! - [`layout`] — where everything sits, derived from a handful of constants;
//! - [`gpt`] — the protective MBR and both GPT copies;
//! - [`fat32`] — the ESP's filesystem;
//! - [`crc`] — the CRC-32 GPT uses.
//!
//! The root image is taken as **bytes**, the `rootfs.img` `kernel/build.rs`
//! already wrote, and copied into its partition unchanged: the bytes on the
//! disk are then the bytes in the boot archive by construction.
//!
//! The image is **reproducible**: fixed GUIDs, zero timestamps, a fixed volume
//! serial. The same [`Spec`] gives the same bytes.

pub mod crc;
pub mod fat32;
pub mod gpt;
pub mod layout;

use std::fmt;
use std::io::{self, Seek, SeekFrom, Write};

use minixrs_kernel_shared::rootfs::{
    HDR_LABEL_OFF, IMAGE_LABEL, IMAGE_LABEL_LEN, ROOTFS_IMAGE_BYTES,
};

/// What goes into an image.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    /// The ESP's files: `(path, contents)`, the path `/`-separated and relative
    /// to the ESP's root, e.g. `EFI/BOOT/BOOTAA64.EFI`. Directories are created
    /// as needed.
    pub esp_files: Vec<(String, Vec<u8>)>,
    /// The root filesystem image, or `None` for a disk with no root partition.
    /// The disk is the same size either way.
    pub root: Option<Vec<u8>>,
}

/// Why an image was not built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The root image is not exactly `ROOTFS_IMAGE_BYTES` long.
    RootLength {
        /// Its length.
        got: usize,
    },
    /// The root image does not begin with `IMAGE_LABEL`.
    RootLabel,
    /// An ESP path cannot be stored on a FAT volume.
    InvalidPath {
        /// The path as given.
        path: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// Two ESP files have the same path. FAT compares names without regard to
    /// case, so `boot/kernel` and `BOOT/Kernel` are the same path.
    DuplicatePath {
        /// The second of the two.
        path: String,
    },
    /// One ESP path needs a directory where another has a file.
    PathConflict {
        /// The path that could not be added.
        path: String,
    },
    /// An ESP file is 4 GiB or larger, more than a FAT size field holds.
    FileTooLarge {
        /// Its path.
        path: String,
    },
    /// A directory needs more entries than FAT allows one to have.
    DirectoryTooLarge {
        /// Its path; empty for the root.
        path: String,
    },
    /// The ESP's files and directories do not fit in the ESP.
    EspFull {
        /// Clusters they need.
        needed_clusters: u64,
        /// Clusters the ESP has.
        available_clusters: u32,
    },
    /// The volume handed to [`fat32::write_volume`] cannot be a FAT32 volume.
    /// Never returned by [`build_image`], whose ESP size is fixed and asserted.
    VolumeSize {
        /// Its length.
        bytes: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootLength { got } => write!(
                f,
                "the root image is {got} bytes; it must be exactly {ROOTFS_IMAGE_BYTES}"
            ),
            Self::RootLabel => write!(
                f,
                "the root image does not start with the label {:?}; it is not a minix.rs root \
                 image",
                String::from_utf8_lossy(&IMAGE_LABEL)
            ),
            Self::InvalidPath { path, why } => write!(f, "ESP path {path:?} is not valid: {why}"),
            Self::DuplicatePath { path } => {
                write!(
                    f,
                    "ESP path {path:?} is given twice (FAT names ignore case)"
                )
            }
            Self::PathConflict { path } => write!(
                f,
                "ESP path {path:?} needs the same name to be both a file and a directory"
            ),
            Self::FileTooLarge { path } => {
                write!(
                    f,
                    "ESP file {path:?} is 4 GiB or larger; FAT cannot hold it"
                )
            }
            Self::DirectoryTooLarge { path } => {
                write!(f, "ESP directory {path:?} has more entries than FAT allows")
            }
            Self::EspFull {
                needed_clusters,
                available_clusters,
            } => write!(
                f,
                "the files do not fit in the ESP: they need {needed_clusters} clusters of {} \
                 bytes and it has {available_clusters}",
                fat32::CLUSTER_BYTES
            ),
            Self::VolumeSize { bytes } => {
                write!(f, "a volume of {bytes} bytes cannot be formatted as FAT32")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Build the whole disk image, [`layout::DISK_BYTES`] long, in memory.
///
/// # Errors
///
/// - [`Error::RootLength`] / [`Error::RootLabel`] if `spec.root` is not a
///   minix.rs root image;
/// - whatever [`fat32::write_volume`] returns for the ESP's files.
pub fn build_image(spec: &Spec) -> Result<Vec<u8>, Error> {
    if let Some(root) = &spec.root {
        if root.len() != ROOTFS_IMAGE_BYTES {
            return Err(Error::RootLength { got: root.len() });
        }
        // The length check above makes this slice in range.
        if root[HDR_LABEL_OFF..HDR_LABEL_OFF + IMAGE_LABEL_LEN] != IMAGE_LABEL {
            return Err(Error::RootLabel);
        }
    }

    let mut image = vec![0u8; layout::DISK_BYTES];

    fat32::write_volume(
        &mut image[layout::ESP_OFFSET..layout::ESP_OFFSET + layout::ESP_BYTES],
        // Asserted in `layout` to fit the BPB's 32-bit field.
        layout::ESP_FIRST_LBA as u32,
        &spec.esp_files,
    )?;

    // Without a root the partition's slot stays unused and its range zero; the
    // tables otherwise do not change, so the disk is the same size either way.
    match &spec.root {
        Some(root) => {
            image[layout::ROOT_OFFSET..layout::ROOT_OFFSET + ROOTFS_IMAGE_BYTES]
                .copy_from_slice(root);
            gpt::write_tables(&mut image, &[gpt::ESP, gpt::ROOT]);
        }
        None => gpt::write_tables(&mut image, &[gpt::ESP]),
    }
    Ok(image)
}

/// Write `image` to `out` without writing its zeros: runs of all-zero chunks
/// are seeked over, so on a filesystem with sparse files a mostly empty image
/// costs only the blocks that hold something. The result is `image.len()` bytes
/// long and reads back as `image` — provided `out` was empty to begin with.
///
/// # Errors
///
/// Whatever `out` returns.
pub fn write_sparse<W: Write + Seek>(out: &mut W, image: &[u8]) -> io::Result<()> {
    /// The granularity holes are found at: a filesystem block.
    const CHUNK: usize = 4096;

    let mut offset = 0;
    let mut end_of_data = 0;
    while offset < image.len() {
        // The next run of chunks that hold something...
        let run_end = |from: usize, zero: bool| {
            let mut at = from;
            while at < image.len() {
                let chunk = &image[at..image.len().min(at + CHUNK)];
                if chunk.iter().all(|&b| b == 0) != zero {
                    break;
                }
                at += chunk.len();
            }
            at
        };
        let data_start = run_end(offset, true);
        let data_end = run_end(data_start, false);
        if data_start < data_end {
            out.seek(SeekFrom::Start(data_start as u64))?;
            out.write_all(&image[data_start..data_end])?;
            end_of_data = data_end;
        }
        offset = data_end;
    }
    // ...and, if the image ends in zeros, one of them, written: that is what
    // gives the file its length.
    if end_of_data < image.len() {
        out.seek(SeekFrom::Start(image.len() as u64 - 1))?;
        out.write_all(&[0])?;
    }
    out.flush()
}

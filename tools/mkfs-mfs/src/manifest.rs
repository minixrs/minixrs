// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! What goes into the image: a list of absolute paths and their contents.
//!
//! Exactly **one directory level** is supported — `/bin/hello`, `/etc/motd` — and
//! that is asserted rather than assumed. Slices 5.7–5.9 need no deeper tree, and a
//! recursive builder would be code with no caller and no test that reflected a
//! real image. [`split_path`] is where the rule lives, so widening it later is one
//! function and its tests, not a search through the writer.

use crate::image::MkfsError;

/// One file in the image.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Absolute path, exactly `/<dir>/<name>`.
    pub path: String,
    /// File contents, verbatim — the **whole** file, hole included.
    pub bytes: Vec<u8>,
    /// Leading bytes to leave **unallocated**: a hole.
    ///
    /// `0` for every ordinary file. When non-zero it must be a whole multiple of
    /// the block size and the corresponding prefix of `bytes` must already be
    /// zero — the image would otherwise claim content it does not store. Both are
    /// checked when the image is built, not here, so a caller assembling a
    /// manifest hears about every problem at once.
    ///
    /// **One variant with one caller**, and deliberately so: it exists for
    /// `/etc/holey`, whose whole job is making a zone assignment that does not
    /// move the file's size reachable. A general sparse-file description would be
    /// code with no second user.
    pub hole: usize,
}

/// The files to write, in the order they were added. Directories are implied by
/// the paths and created in first-seen order.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub entries: Vec<Entry>,
}

impl Manifest {
    /// An empty manifest.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file. Paths are validated when the image is built, not here, so a
    /// caller assembling a manifest from argv reports every problem at once.
    pub fn add(&mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> &mut Self {
        self.entries.push(Entry {
            path: path.into(),
            bytes: bytes.into(),
            hole: 0,
        });
        self
    }

    /// Add a file with a leading hole. See [`Entry::hole`].
    pub fn add_sparse(
        &mut self,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        hole: usize,
    ) -> &mut Self {
        self.entries.push(Entry {
            path: path.into(),
            bytes: bytes.into(),
            hole,
        });
        self
    }

    /// Total bytes of file content. Used by the block-budget precheck.
    pub fn total_bytes(&self) -> usize {
        self.entries.iter().map(|e| e.bytes.len()).sum()
    }
}

/// Split `/<dir>/<name>` into its two components.
///
/// Rejects anything else: a bare `/name`, a deeper `/a/b/c`, a relative path, a
/// trailing slash, an empty component, or a `.`/`..` component (which would
/// collide with the entries every directory already carries).
pub fn split_path(path: &str) -> Result<(&str, &str), MkfsError> {
    let bad = || MkfsError::BadPath(path.to_string());
    let rest = path.strip_prefix('/').ok_or_else(bad)?;
    let mut parts = rest.split('/');
    let dir = parts.next().ok_or_else(bad)?;
    let name = parts.next().ok_or_else(bad)?;
    if parts.next().is_some() {
        return Err(bad());
    }
    for c in [dir, name] {
        if c.is_empty() || c == "." || c == ".." || c.len() > minixrs_mfs::dirent::NAME_MAX {
            return Err(bad());
        }
    }
    Ok((dir, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_component_path_splits() {
        assert_eq!(split_path("/bin/hello"), Ok(("bin", "hello")));
        assert_eq!(split_path("/etc/motd"), Ok(("etc", "motd")));
    }

    #[test]
    fn every_other_shape_is_a_bad_path() {
        // Each of these would otherwise be silently reinterpreted as something
        // the writer could half-handle.
        for p in [
            "",            // empty
            "bin/hello",   // relative
            "/hello",      // no directory level
            "/a/b/c",      // too deep
            "/bin/",       // empty name
            "//hello",     // empty directory
            "/bin/hello/", // trailing slash reads as a third, empty component
            "/./x",        // would collide with the "." entry
            "/x/..",       // would collide with the ".." entry
        ] {
            assert_eq!(
                split_path(p),
                Err(MkfsError::BadPath(p.to_string())),
                "{p:?}"
            );
        }
    }

    #[test]
    fn a_component_longer_than_the_name_field_is_refused() {
        let long = "x".repeat(minixrs_mfs::dirent::NAME_MAX + 1);
        let path = format!("/bin/{long}");
        assert_eq!(split_path(&path), Err(MkfsError::BadPath(path.clone())));

        // ...and exactly NAME_MAX is fine, because the name field is NUL-padded
        // rather than NUL-terminated.
        let ok = "x".repeat(minixrs_mfs::dirent::NAME_MAX);
        let path = format!("/bin/{ok}");
        assert_eq!(split_path(&path), Ok(("bin", ok.as_str())));
    }

    #[test]
    fn a_manifest_keeps_insertion_order_and_sums_its_bytes() {
        let mut m = Manifest::new();
        m.add("/bin/hello", vec![1u8; 10])
            .add("/etc/motd", vec![2u8; 5]);
        assert_eq!(
            m.entries
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            ["/bin/hello", "/etc/motd"]
        );
        assert_eq!(m.total_bytes(), 15);
    }
}

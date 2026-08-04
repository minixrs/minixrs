// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! What PM decides about a `PM_EXEC` target before it asks anyone else (slice
//! 5.9).
//!
//! Two questions, both pure, both answered here so they carry unit tests:
//! **which form is this** (a filesystem path or a boot-image module name), and
//! **what is `argv[0]`**. `main.rs` is Sonar-coverage-excluded and a sibling
//! module is not — the `servers/vfs` `rw.rs` / `open.rs` split, and the reason
//! `mproc.rs` already exists beside it.
//!
//! ## The discriminator is the leading `/`
//!
//! One field rather than a path plus a form flag, because two fields can
//! disagree — and a message whose two halves disagree is a message some layer has
//! to pick a winner for. It is also already the rule everywhere below this point:
//! `walk::parse_path` answers `EINVAL` to a relative path because minix.rs has no
//! working directory, so `/`-or-not is the distinction the filesystem itself
//! draws.
//!
//! The consequence worth stating: **module names and paths are disjoint
//! namespaces**. Only the name form reaches `module_by_name`, so nothing that
//! resolves a *path* can name the `rootfs` blob — which is the warning
//! `com::ROOTFS_MODULE_NAME` has been carrying since slice 5.7.

use minixrs_kernel_shared::callnr::EXEC_NAME_LEN;
use minixrs_kernel_shared::error::{EINVAL, ENAMETOOLONG};

/// What a `PM_EXEC` payload's target field names.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Target<'a> {
    /// An absolute filesystem path, to be staged through VFS. Carries the path
    /// and its basename, which becomes `argv[0]` and the exec'd proc's name.
    Path { path: &'a str, argv0: &'a str },
    /// A boot-image module name, forwarded to `SYS_EXEC` as it has been since
    /// slice 4.7.
    Module(&'a str),
}

/// Classify a `PM_EXEC` target field, or say why it is not one.
///
/// `field` is the payload's whole fixed-width, NUL-**padded** target field —
/// raw bytes, not what `rd_name` would hand back. That distinction is
/// load-bearing: `rd_name` reports "no NUL anywhere" and "a 64-byte name"
/// identically, and those are `ENAMETOOLONG` and `EINVAL` respectively, so the
/// terminator rule has to be decided here where the field's *width* is visible.
///
/// The checks, in order — each is the first thing that can be wrong given the
/// ones before it:
///
/// 1. **No NUL anywhere** → `ENAMETOOLONG`. Never a silent truncation, which for
///    an executable is the difference between running one program and another.
/// 2. **Empty** → `EINVAL`; it names nothing.
/// 3. Non-UTF-8 → `EINVAL`; the target travels onward as text.
/// 4. **Not starting with `/`** → a module name, refused if it will not fit
///    [`EXEC_NAME_LEN`] (the kernel's field; truncating would silently exec a
///    different module).
/// 5. Otherwise a path, whose **basename** must be non-empty — a path ending in
///    `/` names a directory and therefore no program — and must fit
///    `EXEC_NAME_LEN`, because it becomes `argv[0]`. Slice 5.9 deliberately
///    passes the basename rather than the whole path, which is what leaves
///    `EXEC_NAME_LEN`, `PROC_NAME_LEN`, and the initial stack's geometry alone.
pub fn parse(field: &[u8]) -> Result<Target<'_>, i32> {
    let Some(end) = field.iter().position(|&b| b == 0) else {
        return Err(ENAMETOOLONG);
    };
    let field = core::str::from_utf8(&field[..end]).map_err(|_| EINVAL)?;
    if field.is_empty() {
        return Err(EINVAL);
    }

    if !field.starts_with('/') {
        if field.len() > EXEC_NAME_LEN {
            return Err(EINVAL);
        }
        return Ok(Target::Module(field));
    }
    let argv0 = basename(field).ok_or(EINVAL)?;
    if argv0.len() > EXEC_NAME_LEN {
        return Err(EINVAL);
    }
    Ok(Target::Path { path: field, argv0 })
}

/// The tail of a path after its last `/`, or `None` when there is none.
///
/// Total over strings, so a path ending in `/` (a directory, and therefore no
/// program) and the empty string both answer `None` rather than something the
/// caller has to re-check.
fn basename(path: &str) -> Option<&str> {
    let name = match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    };
    if name.is_empty() { None } else { Some(name) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::PM_EXEC_PATH_MAX;

    /// The payload's fixed-width field, NUL-padded — exactly what `handle_exec`
    /// hands over.
    fn field(bytes: &[u8]) -> [u8; PM_EXEC_PATH_MAX] {
        let mut f = [0u8; PM_EXEC_PATH_MAX];
        f[..bytes.len()].copy_from_slice(bytes);
        f
    }

    #[test]
    fn an_absolute_path_is_a_path_with_its_basename() {
        // The pairing is the point: `argv[0]` is the basename, never the path,
        // which is what keeps `EXEC_NAME_LEN` at 16 through exec-from-FS.
        assert_eq!(
            parse(&field(b"/bin/hello")),
            Ok(Target::Path {
                path: "/bin/hello",
                argv0: "hello"
            })
        );
        assert_eq!(
            parse(&field(b"/hello")),
            Ok(Target::Path {
                path: "/hello",
                argv0: "hello"
            })
        );
    }

    #[test]
    fn a_bare_name_is_a_module() {
        // The slice-4.7 form, still live: `worker` is boot-embedded and has no
        // path. Nothing here reaches the filesystem.
        assert_eq!(parse(&field(b"worker")), Ok(Target::Module("worker")));
        assert_eq!(parse(&field(b"hello")), Ok(Target::Module("hello")));
    }

    #[test]
    fn the_leading_slash_is_the_only_discriminator() {
        // No second field, so nothing can disagree with this. A name that
        // *looks* like a path but is not absolute is a module name — and will
        // simply not resolve, which is the honest answer and what init's
        // `relative` probe asserts (ENOENT, not EINVAL).
        assert_eq!(parse(&field(b"etc/motd")), Ok(Target::Module("etc/motd")));
    }

    #[test]
    fn a_field_with_no_nul_is_enametoolong() {
        // The reason this takes raw bytes rather than a `&str` from `rd_name`:
        // that helper reports "no NUL" and "a full-width name" identically, and
        // they are different errnos. Never a truncation — for an executable, a
        // silently shortened target runs a different program.
        assert_eq!(parse(&[b'a'; PM_EXEC_PATH_MAX]), Err(ENAMETOOLONG));
        assert_eq!(parse(&[b'/'; PM_EXEC_PATH_MAX]), Err(ENAMETOOLONG));
    }

    #[test]
    fn an_all_nul_field_is_einval() {
        // A client that sent nothing.
        assert_eq!(parse(&field(b"")), Err(EINVAL));
    }

    #[test]
    fn a_non_utf8_target_is_einval() {
        assert_eq!(parse(&field(b"/\xff\xfe")), Err(EINVAL));
    }

    #[test]
    fn a_path_ending_in_a_slash_is_einval() {
        // It names a directory, so there is no program and no `argv[0]`.
        assert_eq!(parse(&field(b"/bin/")), Err(EINVAL));
        assert_eq!(parse(&field(b"/")), Err(EINVAL));
    }

    #[test]
    fn a_module_name_past_the_kernel_field_is_einval() {
        // Refused rather than truncated: a silently shortened module name would
        // exec a *different* module, or none.
        let long = [b'm'; EXEC_NAME_LEN + 1];
        assert_eq!(parse(&field(&long)), Err(EINVAL));
        let fits = [b'm'; EXEC_NAME_LEN];
        let f = field(&fits);
        assert!(matches!(parse(&f), Ok(Target::Module(_))));
    }

    #[test]
    fn a_basename_past_the_kernel_field_is_einval() {
        // The path itself may run to `PM_EXEC_PATH_MAX`, but `argv[0]` has to fit
        // `EXEC_NAME_LEN` — so it is the *basename* that is capped, and a long
        // directory prefix costs nothing.
        let mut long = [b'n'; EXEC_NAME_LEN + 6];
        long[..5].copy_from_slice(b"/bin/");
        assert_eq!(parse(&field(&long)), Err(EINVAL));

        let mut deep = [b'n'; 17 + EXEC_NAME_LEN];
        deep[..17].copy_from_slice(b"/a/b/c/d/e/f/g/h/");
        let f = field(&deep);
        assert!(matches!(parse(&f), Ok(Target::Path { .. })));
    }

    #[test]
    fn the_longest_path_that_fits_the_field_is_accepted() {
        // NUL-padded, so the longest target leaves room for the terminator.
        let mut p = [b'n'; PM_EXEC_PATH_MAX - 1];
        p[..5].copy_from_slice(b"/bin/");
        let f = field(&p);
        assert!(matches!(parse(&f), Err(EINVAL)), "basename is far too long");

        let mut q = [b'/'; PM_EXEC_PATH_MAX - 1];
        let tail = q.len() - 5;
        q[tail..].copy_from_slice(b"hello");
        let f = field(&q);
        assert_eq!(
            parse(&f),
            Ok(Target::Path {
                path: core::str::from_utf8(&q).unwrap(),
                argv0: "hello"
            })
        );
    }
}

//! Static assets — `static "/assets" from "public";` (routing.md §10).
//!
//! This module is the part both backends share: how a URL becomes a
//! relative path, what may never be one, the content type, and the ETag.
//! Only the *lookup* differs — `jwc serve` reads the directory, a native
//! binary reads a table `include_bytes!`d into it at build time — and
//! keeping the decision here is what makes the two answer the same bytes.
//!
//! ## Why the rules are refusals rather than repairs
//!
//! The usual traversal defence normalises `a/../b` to `b`. That is a
//! *repair*, and a repair has to be exactly as clever as the attacker: it
//! must agree with the operating system about every encoding, separator and
//! case fold, or the normalised path and the opened path are different
//! strings. Here nothing is repaired. A segment that is `..`, that begins
//! with `.`, or that carries a separator of any kind through an encoding is
//! **refused**, and the request answers 404 without a syscall.
//!
//! `hardening.rs::the_filesystem_is_out_of_reach_of_a_request` still holds:
//! a *route* may not read a path the caller chose (`E0230`). A `static`
//! mount is not that. Its root is written in the source, fixed at compile
//! time, and the only thing the caller supplies is a path *inside* it that
//! this module has already refused if it is not an ordinary file name.

use std::path::{Path, PathBuf};

/// A mount, as the runtime sees it.
#[derive(Clone, Debug)]
pub struct Mount {
    /// The URL prefix, normalised: no trailing slash, except the bare `/`.
    pub prefix: String,
    /// The directory, resolved against the project root at load time.
    pub root: PathBuf,
    /// `cache <n>` — seconds for `Cache-Control: max-age`. 0 means the
    /// asset is revalidated every time, which the ETag makes cheap.
    pub max_age: u32,
}

// The decisions both backends make — `under`, `safe_relative`,
// `content_type`, `cache_control`, `none_match` — live in a file the native
// backend pastes into the crate it generates, so the two run the same text
// rather than two readings of the same paragraph (routing.md §10.6).
include!("assets_core.rs.in");

/// The file a cleaned relative path names inside `root`, or `None`.
///
/// The containment check is against the **canonical** root and the
/// canonical target, so a symlink pointing out of the tree is caught even
/// though every segment of the URL was an ordinary name.
pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let mut p = root.clone();
    if !rel.is_empty() {
        p.push(rel);
    }
    let p = p.canonicalize().ok()?;
    if !p.starts_with(&root) {
        return None;
    }
    if p.is_dir() {
        let index = p.join("index.html");
        let index = index.canonicalize().ok()?;
        if !index.starts_with(&root) || !index.is_file() {
            return None;
        }
        return Some(index);
    }
    p.is_file().then_some(p)
}

/// Every file a mount publishes, as `(url path, file on disk)`, sorted.
///
/// The same rules the request path applies, applied once at build time: a
/// dotfile is skipped and a symlink that leaves the root is skipped, so
/// `jwc build` cannot embed into the binary something `jwc serve` would
/// refuse to send. Sorted because the emitted table is part of the
/// generated source, and a build that reorders it is a build that is not
/// reproducible.
pub fn walk(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(canon_root) = root.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), canon_root.clone())];
    while let Some((prefix, dir)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            // Canonicalise before deciding: a symlink is whatever it points
            // at, and what it points at may be outside the tree.
            let Ok(canon) = e.path().canonicalize() else {
                continue;
            };
            if !canon.starts_with(&canon_root) {
                continue;
            }
            if canon.is_dir() {
                stack.push((rel, canon));
            } else if canon.is_file() {
                out.push((rel, canon));
            }
        }
    }
    out.sort();
    out
}

/// A strong ETag: the sha256 of the bytes, quoted.
///
/// Content-derived rather than `(size, mtime)`, because the native build
/// has the bytes and no mtime. Anything else would make the two backends
/// disagree on a header for the same file.
pub fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", crate::hash::sha256_hex_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_reference_is_refused_however_it_is_written() {
        assert_eq!(safe_relative(".."), None);
        assert_eq!(safe_relative("a/../b"), None);
        assert_eq!(safe_relative("%2e%2e/b"), None);
        assert_eq!(safe_relative("%2E%2E%2Fb"), None, "an encoded separator");
        assert_eq!(safe_relative("....//"), None, "the `....//` bypass");
    }

    #[test]
    fn a_separator_that_arrives_encoded_is_refused_not_re_split() {
        // If this were decoded and then split, `%2f` would become a real
        // separator and the segment rules would run on the halves. It is
        // one segment carrying a separator, so it is refused.
        assert_eq!(safe_relative("a%2fb"), None);
        assert_eq!(safe_relative("a%5cb"), None, "a backslash");
        assert_eq!(safe_relative("C%3a"), None, "a drive letter");
        assert_eq!(safe_relative("a%00b"), None, "a NUL");
    }

    #[test]
    fn a_dotfile_is_refused() {
        assert_eq!(safe_relative(".env"), None);
        assert_eq!(safe_relative(".git/config"), None);
        assert_eq!(safe_relative("ok/.htpasswd"), None);
        // Only a leading dot. `app.min.js` is an ordinary name.
        assert_eq!(safe_relative("app.min.js").as_deref(), Some("app.min.js"));
    }

    #[test]
    fn an_invalid_escape_is_refused_rather_than_passed_through() {
        assert_eq!(safe_relative("a%"), None);
        assert_eq!(safe_relative("a%zz"), None);
        assert_eq!(safe_relative("a%2"), None);
    }

    #[test]
    fn empty_segments_collapse_and_the_root_is_the_empty_path() {
        assert_eq!(safe_relative("").as_deref(), Some(""));
        assert_eq!(safe_relative("/").as_deref(), Some(""));
        assert_eq!(safe_relative("a//b").as_deref(), Some("a/b"));
        assert_eq!(safe_relative("./a").as_deref(), Some("a"));
        assert_eq!(safe_relative("a%20b.txt").as_deref(), Some("a b.txt"));
    }

    #[test]
    fn a_mount_covers_its_prefix_and_not_a_neighbour_that_starts_the_same() {
        assert_eq!(under("/assets", "/assets"), Some(""));
        assert_eq!(under("/assets", "/assets/"), Some(""));
        assert_eq!(under("/assets", "/assets/app.js"), Some("app.js"));
        assert_eq!(under("/assets", "/assetsx/app.js"), None);
        assert_eq!(under("/assets", "/other"), None);
        assert_eq!(under("/", "/app.js"), Some("app.js"));
        assert_eq!(under("/", "/"), Some(""));
    }

    #[test]
    fn an_unknown_extension_is_never_guessed_into_html() {
        assert_eq!(content_type("a.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("a.HTML"), "text/html; charset=utf-8");
        assert_eq!(content_type("a.png"), "image/png");
        assert_eq!(content_type("a.weird"), "application/octet-stream");
        assert_eq!(content_type("noextension"), "application/octet-stream");
    }

    #[test]
    fn the_etag_is_the_content_and_nothing_else() {
        assert_eq!(etag(b"a"), etag(b"a"));
        assert_ne!(etag(b"a"), etag(b"b"));
        assert!(etag(b"a").starts_with('"') && etag(b"a").ends_with('"'));
    }

    #[test]
    fn if_none_match_accepts_a_list_a_star_and_a_weak_form() {
        let tag = "\"abc\"";
        assert!(none_match("\"abc\"", tag));
        assert!(none_match("\"x\", \"abc\"", tag));
        assert!(none_match("*", tag));
        assert!(none_match("W/\"abc\"", tag));
        assert!(!none_match("\"x\"", tag));
        assert!(!none_match("", tag));
    }

    #[test]
    fn a_symlink_out_of_the_tree_is_refused_even_with_ordinary_names() {
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(outside.path().join("secret.txt"), b"s").expect("write");
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("ok.txt"), b"o").expect("write");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                root.path().join("link.txt"),
            )
            .expect("symlink");
            assert!(
                resolve(root.path(), "link.txt").is_none(),
                "a symlink leaving the root is not inside it"
            );
        }
        assert!(resolve(root.path(), "ok.txt").is_some());
        assert!(resolve(root.path(), "missing.txt").is_none());
    }

    #[test]
    fn a_directory_answers_its_index_and_nothing_else() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join("sub")).expect("mkdir");
        assert!(
            resolve(root.path(), "sub").is_none(),
            "a directory with no index is not a file"
        );
        std::fs::write(root.path().join("sub/index.html"), b"<i>").expect("write");
        let hit = resolve(root.path(), "sub").expect("index");
        assert!(hit.ends_with("index.html"));
        // The root of the mount is the same rule.
        std::fs::write(root.path().join("index.html"), b"<r>").expect("write");
        assert!(resolve(root.path(), "").expect("root index").ends_with("index.html"));
    }
}

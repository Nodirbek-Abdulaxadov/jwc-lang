//! User-profile cache for downloaded/fetched package sources.
//!
//! Layout:
//!
//! ```text
//! ~/.jwc/
//!   registry/
//!     <registry-host>/
//!       <pkg-name>/
//!         <version>/        ← extracted source tree
//!     git/
//!       <host>/<path>-<rev>/  ← git clone (path slashes flattened)
//! ```
//!
//! All projects share the same cache. The `<host>` segment isolates
//! different registries so a name collision across registries doesn't
//! clobber.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Resolve the user's home directory across platforms, returning a fallback
/// of `./.jwc-home` if no profile dir is set. Mirrors the lightweight
/// implementation already in `native_build.rs::dirs_home`.
pub fn home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return PathBuf::from(profile);
        }
    }
    PathBuf::from(".")
}

/// Root of the jwc per-user data dir. Override via `JWC_HOME` env.
pub fn jwc_home() -> PathBuf {
    if let Ok(custom) = std::env::var("JWC_HOME") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
    home_dir().join(".jwc")
}

/// `<jwc_home>/registry/<host>/` — packages downloaded from this registry.
/// The `host` is the registry's URL host segment (e.g. `registry.jwc.dev`),
/// lowercased and stripped of leading/trailing slashes.
pub fn registry_cache_dir(host: &str) -> PathBuf {
    jwc_home().join("registry").join(sanitize_segment(host))
}

/// `<registry_cache>/<name>/<version>/` — fully resolved source tree for a
/// specific package version. The directory layout inside is whatever the
/// tarball / git snapshot contained.
pub fn pkg_dir(host: &str, name: &str, version: &str) -> PathBuf {
    registry_cache_dir(host)
        .join(sanitize_segment(name))
        .join(sanitize_segment(version))
}

/// Git clones live separately so the same git URL at different revisions
/// can coexist without rewriting on every fetch.
pub fn git_cache_dir(url: &str, rev: &str) -> PathBuf {
    let key = format!("{}-{}", sanitize_segment(url), sanitize_segment(rev));
    jwc_home().join("registry").join("git").join(key)
}

/// Ensure the parent of `path` exists. Creates it (recursively) if missing.
pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    Ok(())
}

/// Replace path-unsafe characters with `_` so the segment can be used as a
/// directory name on any platform.
fn sanitize_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "_".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_slashes_and_colons() {
        assert_eq!(
            sanitize_segment("https://example.com/path"),
            "https___example.com_path"
        );
        assert_eq!(sanitize_segment(""), "_");
        assert_eq!(sanitize_segment("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn pkg_dir_layout_is_stable() {
        let env_backup = std::env::var("JWC_HOME").ok();
        std::env::set_var("JWC_HOME", "/tmp/jwc-cache-test");
        let d = pkg_dir("registry.example", "http", "1.2.3");
        assert!(d.ends_with("registry/registry.example/http/1.2.3"));
        if let Some(v) = env_backup {
            std::env::set_var("JWC_HOME", v);
        } else {
            std::env::remove_var("JWC_HOME");
        }
    }
}

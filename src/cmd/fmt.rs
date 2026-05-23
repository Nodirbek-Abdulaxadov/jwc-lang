//! `jwc fmt` command implementation split out from main.rs.
//!
//! Walks `path` (file or directory), runs the v1 line-based formatter
//! on every `.jwc` file, and either rewrites them in place (default) or
//! exits non-zero when any file would change (`--check`, CI mode).
//! Skips build-cache directories — see `fmt::collect_jwc_files`.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::fmt as jwc_fmt;

/// Run `jwc fmt` against `path` with the given `check` mode.
///
/// Returns `Ok(())` after a successful walk. In `check` mode, exits the
/// process with code 1 (not via `Result`) when at least one file would
/// be rewritten — matches the original behaviour from main.rs so CI
/// callers don't have to translate errors.
pub fn run(path: Option<PathBuf>, check: bool) -> Result<()> {
    let target = path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let files = jwc_fmt::collect_jwc_files(&target)
        .with_context(|| format!("Failed to enumerate .jwc files under {}", target.display()))?;
    if files.is_empty() {
        eprintln!("No .jwc files found under {}", target.display());
        return Ok(());
    }

    let mut changed: Vec<PathBuf> = Vec::new();
    for file in &files {
        let outcome = jwc_fmt::format_file(file, check)
            .with_context(|| format!("Failed to format {}", file.display()))?;
        if matches!(outcome, jwc_fmt::FormatOutcome::Changed) {
            changed.push(file.clone());
        }
    }

    if check {
        if !changed.is_empty() {
            eprintln!(
                "jwc fmt --check: {} file(s) would be rewritten:",
                changed.len()
            );
            for f in &changed {
                eprintln!("  {}", f.display());
            }
            std::process::exit(1);
        }
        println!("jwc fmt --check: {} file(s) already formatted", files.len());
    } else {
        println!("jwc fmt: rewrote {}/{} file(s)", changed.len(), files.len());
    }
    Ok(())
}

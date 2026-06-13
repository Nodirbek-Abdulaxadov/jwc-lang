//! `jwc fmt` command implementation split out from main.rs.
//!
//! Walks every entry in `paths` (file or directory), runs the formatter
//! on every `.jwc` file, and either rewrites them in place (default),
//! prints the formatted result to stdout (`--stdout`), or exits non-zero
//! when any file would change (`--check`, CI mode). Skips build-cache
//! directories — see `fmt::collect_jwc_files`.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::fmt as jwc_fmt;

/// Run `jwc fmt` against every entry in `paths` with the given `check` and
/// `stdout` modes. When `paths` is empty, formats the current working
/// directory.
///
/// Mode precedence:
/// - `check=true` overrides `stdout`. We never touch files in either mode
///   when `check` is set, and exit code 1 signals "would rewrite".
/// - Otherwise `stdout=true` prints to stdout and leaves files alone.
/// - Default (`check=false`, `stdout=false`) rewrites files in place.
pub fn run(paths: Vec<PathBuf>, check: bool, stdout: bool) -> Result<()> {
    let targets: Vec<PathBuf> = if paths.is_empty() {
        vec![std::env::current_dir().unwrap_or_else(|_| ".".into())]
    } else {
        paths
    };

    // Enumerate every `.jwc` file across all targets.
    let mut files: Vec<PathBuf> = Vec::new();
    for target in &targets {
        let mut found = jwc_fmt::collect_jwc_files(target).with_context(|| {
            format!("Failed to enumerate .jwc files under {}", target.display())
        })?;
        files.append(&mut found);
    }
    // De-dup in case the user passed overlapping targets (e.g. `examples`
    // and `examples/testapp`).
    files.sort();
    files.dedup();

    if files.is_empty() {
        eprintln!("No .jwc files found under the given path(s)");
        return Ok(());
    }

    // --stdout mode: just print, no rewrites, no change tracking.
    if stdout && !check {
        for file in &files {
            let src = std::fs::read_to_string(file)
                .with_context(|| format!("read: {}", file.display()))?;
            let out = jwc_fmt::format_source(&src);
            print!("{}", out);
        }
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

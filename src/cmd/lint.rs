//! `jwc lint` command implementation split out from main.rs for testability.
//!
//! Three modes:
//! - `--explain CODE`: pure catalog lookup, prints one description line.
//! - `--list-codes`: dumps the entire W/E catalog as JSON, no project load.
//! - default: walks the current project, runs the lint pass, prints
//!   warnings (or one JSON array under `--json`).

use std::path::Path;

use anyhow::{bail, Result};

use crate::{error_codes, lint, project};

/// Pure catalog lookup. Returns early — does NOT touch the project.
pub fn explain(code: &str) -> Result<()> {
    let code_upper = code.to_uppercase();
    match error_codes::lookup_warning(&code_upper)
        .or_else(|| error_codes::lookup_error(&code_upper))
    {
        Some(desc) => {
            println!("{code_upper}: {desc}");
            Ok(())
        }
        None => bail!(
            "Unknown diagnostic code `{code_upper}`. Use `--list-codes` to see the full catalog."
        ),
    }
}

/// Dump the entire W + E diagnostic catalog as a JSON array. No project
/// load required — runs anywhere, useful for editor / tooling integration.
pub fn list_codes() -> Result<()> {
    let mut all: Vec<serde_json::Value> = Vec::new();
    for d in error_codes::LINT_WARNINGS {
        all.push(serde_json::json!({
            "code": d.code,
            "description": d.description,
            "severity": "warning",
        }));
    }
    for d in error_codes::VALIDATOR_ERRORS {
        all.push(serde_json::json!({
            "code": d.code,
            "description": d.description,
            "severity": "error",
        }));
    }
    println!("{}", serde_json::Value::Array(all));
    Ok(())
}

/// Default lint mode: load the current project, run the lint pass, print
/// warnings. With `json: true` emits a single JSON array on stdout; the
/// human-readable mode prints one `warning[CODE]: message` per line
/// followed by a summary count.
pub fn run(json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = project::find_project_root(&cwd)?;
    let loaded = project::load_project_from_root(&root)?;
    let warnings = lint::lint_program(&loaded.program);

    if json {
        let payload: Vec<serde_json::Value> = warnings
            .iter()
            .map(|w| serde_json::json!({ "code": w.code, "message": w.message }))
            .collect();
        println!("{}", serde_json::Value::Array(payload));
        return Ok(());
    }

    if warnings.is_empty() {
        print_clean(&loaded);
    } else {
        print_warnings(&warnings);
    }
    Ok(())
}

fn print_clean(loaded: &project::LoadedProject) {
    println!(
        "No lint warnings for project '{}' ({} source files).",
        loaded.manifest.name,
        loaded.source_files.len()
    );
}

fn print_warnings(warnings: &[lint::LintWarning]) {
    for w in warnings {
        println!(
            "warning[{code}]: {message}",
            code = w.code,
            message = w.message
        );
    }
    println!();
    println!("{} warning(s) found.", warnings.len());
}

/// Helper for callers that want the project root without re-reading
/// the manifest. Currently unused outside `run`, but keeping it here
/// gives the eventual `jwc lint --watch` / `jwc lint --fix` follow-ups
/// somewhere to live.
#[allow(dead_code)]
pub fn project_root(cwd: &Path) -> Result<std::path::PathBuf> {
    project::find_project_root(cwd)
}

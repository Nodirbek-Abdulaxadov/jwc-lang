//! `jwc lint` command implementation split out from main.rs for testability.
//!
//! Three modes:
//! - `--explain CODE`: pure catalog lookup, prints one description line.
//! - `--list-codes`: dumps the entire W/E catalog as JSON, no project load.
//! - default: walks the current project, runs the lint pass, prints
//!   warnings (or one JSON array under `--json`).
//!
//! [`report_warnings`] is the shared helper that other subcommands
//! (`jwc build`, `jwc test`) call before / instead of running the program
//! so high-signal warnings (unused middleware, unused function) surface
//! in the default build path — closing the dogfooding gap where a
//! declared-but-unused middleware shipped to production silently. Warnings
//! go to stderr; the build's success line stays on stdout so scripts can
//! still pipe / grep it without seeing lint chatter.

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

/// Run the lint pass over an already-loaded project and surface any
/// warnings on stderr. Returns `Err` when `deny_warnings` is set and at
/// least one warning fires — CI-friendly gate. Otherwise returns `Ok(())`
/// regardless of the warning count: warnings are advisory by default to
/// keep the dev loop unblocked.
pub fn report_warnings(loaded: &project::LoadedProject, deny_warnings: bool) -> Result<()> {
    let warnings = lint::lint_program(&loaded.program);
    if warnings.is_empty() {
        return Ok(());
    }
    for w in &warnings {
        eprintln!(
            "warning[{code}]: {message}",
            code = w.code,
            message = w.message
        );
    }
    eprintln!("{} warning(s) found.", warnings.len());
    if deny_warnings {
        bail!(
            "{} lint warning(s) treated as errors (--deny-warnings).",
            warnings.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Dogfooding regression: jwc-shortener shipped with a declared-but-
    //! unused `RateLimit` middleware, which left a write endpoint
    //! unthrottled in production. Before this lint enforcement, the
    //! build path said nothing because `jwc lint` was a separate opt-in
    //! command. These tests pin the new behaviour:
    //!   * `jwc build` / `jwc test` invoke the lint pass by default.
    //!   * A clean project produces no warnings and no error.
    //!   * Without `--deny-warnings`, warnings are advisory (Ok).
    //!   * With `--deny-warnings`, the same warnings produce Err.
    use super::*;
    use crate::parser;
    use std::collections::BTreeMap;

    fn synthesize(src: &str) -> project::LoadedProject {
        let program = parser::parse_program(src).expect("test fixture must parse");
        parser::validate_program(&program).expect("test fixture must validate");
        project::LoadedProject {
            manifest: project::JwcProject {
                name: "lint_fixture".to_string(),
                kind: project::ProjectKind::App,
                language_version: String::new(),
                version: String::new(),
                pkg_version: None,
                dependencies: BTreeMap::new(),
                registry: None,
            },
            source_files: Vec::new(),
            program,
        }
    }

    const UNUSED_MIDDLEWARE_SRC: &str = r#"
        middleware RateLimit {
            return null;
        }

        route GET "ping" {
            return text("pong");
        }
    "#;

    const CLEAN_SRC: &str = r#"
        route GET "ping" {
            return text("pong");
        }
    "#;

    #[test]
    fn clean_project_passes_with_and_without_deny() {
        let loaded = synthesize(CLEAN_SRC);
        report_warnings(&loaded, false).unwrap();
        report_warnings(&loaded, true).unwrap();
    }

    #[test]
    fn unused_middleware_is_advisory_by_default() {
        let loaded = synthesize(UNUSED_MIDDLEWARE_SRC);
        // No --deny-warnings: should NOT fail the build.
        report_warnings(&loaded, false).expect("warnings advisory by default");
    }

    #[test]
    fn deny_warnings_promotes_unused_middleware_to_error() {
        let loaded = synthesize(UNUSED_MIDDLEWARE_SRC);
        let err = report_warnings(&loaded, true).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("treated as errors"),
            "expected --deny-warnings error message, got: {msg}"
        );
    }
}

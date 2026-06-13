//! Small "read-only" CLI subcommands split out from main.rs:
//! `jwc new`, `jwc check`, `jwc gen-sql`, `jwc test`.
//!
//! None of these mutate project state beyond `jwc new` creating the
//! initial scaffold; they all sit on top of parser / validator / sql
//! generator and stay synchronous, no tokio runtime needed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{cmd, parser, project, sql, templates};

/// Create a new JWC project scaffold rooted at `target`.
///
/// `name` is the project name as the user typed it (used by templated
/// content/path substitution). `kind` picks which embedded template tree
/// to materialise; `Empty` falls back to [`project::create_new_project`],
/// which is the legacy behaviour we don't want to disturb.
pub fn new_project(target: &Path, name: &str, kind: templates::TemplateKind) -> Result<()> {
    match kind {
        templates::TemplateKind::Empty => {
            project::create_new_project(target)?;
        }
        other => {
            templates::create_from_template(name, other, target)?;
        }
    }
    println!(
        "Created project: {} (template: {})",
        target.display(),
        kind.as_str()
    );
    println!("Try:");
    println!("  cd {}", target.display());
    println!("  jwc test");
    if !matches!(kind, templates::TemplateKind::Empty) {
        println!("  jwc run");
    } else {
        println!("  jwc build");
    }
    Ok(())
}

/// Parse + validate a single `.jwc` file. Prints `OK` on success.
///
/// When `typecheck` is true (the default — only the CLI's `--no-typecheck`
/// escape hatch flips it to false), the gradual static type checker
/// also runs after `validate_program`.
pub fn check(file: &Path, typecheck: bool) -> Result<()> {
    let source = read_source(file)?;
    let program = parser::parse_program(&source)
        .with_context(|| format!("Failed to parse {}", file.display()))?;
    parser::validate_program(&program)
        .with_context(|| format!("Validation failed for {}", file.display()))?;
    if typecheck {
        crate::typecheck::typecheck_program(&program)
            .with_context(|| format!("Type check failed for {}", file.display()))?;
    }
    println!("OK");
    Ok(())
}

/// Generate the Postgres `CREATE TABLE` SQL for a `.jwc` schema file and
/// print it to stdout.
pub fn gen_sql(file: &Path) -> Result<()> {
    let source = read_source(file)?;
    let program = parser::parse_program(&source)
        .with_context(|| format!("Failed to parse {}", file.display()))?;
    parser::validate_program(&program)
        .with_context(|| format!("Validation failed for {}", file.display()))?;
    let schema_sql = sql::generate_postgres_schema_sql(&program)?;
    print!("{schema_sql}");
    Ok(())
}

/// Load the current project, report the source file count. Also runs the
/// lint pass so high-signal warnings (unused middleware, unused function)
/// surface here — `jwc test` is the natural place a developer checks
/// "is this project healthy?" before pushing.
pub fn test(deny_warnings: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = project::find_project_root(&cwd)?;
    let loaded = project::load_project_from_root(&root)?;
    cmd::lint::report_warnings(&loaded, deny_warnings)?;
    println!(
        "OK: project '{}' ({} source files)",
        loaded.manifest.name,
        loaded.source_files.len()
    );
    Ok(())
}

fn read_source(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

#[allow(dead_code)]
fn target_to_path_buf(s: &str) -> PathBuf {
    PathBuf::from(s)
}

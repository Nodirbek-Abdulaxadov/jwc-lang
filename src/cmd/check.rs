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

/// Parse + validate a `.jwc` file. Prints `OK` on success.
///
/// When the file belongs to a project (a `jwcproj.json` exists at or above
/// it), validation runs against the **merged project program**, not the file
/// alone. A JWC project is one flat namespace: entities live in
/// `Data/AppDbContext.jwc`, middleware in `Infrastructure/`, and the routes
/// that use them in a third file. Validating a file in isolation reports
/// every one of those cross-file references as unknown, so a project that
/// builds and runs cleanly still reports errors like
/// `Unknown entity 'Wallet' used in select expression` on nearly every file.
///
/// The file's own syntax is still parsed first, so a syntax error is reported
/// against the file the user actually named, with its line and column.
///
/// When `typecheck` is true (the default — only the CLI's `--no-typecheck`
/// escape hatch flips it to false), the gradual static type checker
/// also runs after `validate_program`.
pub fn check(file: &Path, typecheck: bool) -> Result<()> {
    let source = read_source(file)?;
    let program = parser::parse_program(&source)
        .with_context(|| format!("Failed to parse {}", file.display()))?;

    if let Ok(root) = project::find_project_root(file) {
        project::load_project_from_root_with(&root, project::LoadOpts { typecheck }).with_context(
            || {
                format!(
                    "Validation failed for the project containing {}",
                    file.display()
                )
            },
        )?;
        println!("OK");
        return Ok(());
    }

    // Standalone file — no manifest above it, so the file is all there is to
    // check. Cross-file references can't be resolved and aren't expected to.
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

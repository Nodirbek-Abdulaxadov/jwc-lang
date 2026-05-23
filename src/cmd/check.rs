//! Small "read-only" CLI subcommands split out from main.rs:
//! `jwc new`, `jwc check`, `jwc gen-sql`, `jwc test`.
//!
//! None of these mutate project state beyond `jwc new` creating the
//! initial scaffold; they all sit on top of parser / validator / sql
//! generator and stay synchronous, no tokio runtime needed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{parser, project, sql};

/// Create a new JWC project scaffold rooted at `target`.
pub fn new_project(target: &Path) -> Result<()> {
    project::create_new_project(target)?;
    println!("Created project: {}", target.display());
    println!("Try:");
    println!("  cd {}", target.display());
    println!("  jwc test");
    println!("  jwc build");
    Ok(())
}

/// Parse + validate a single `.jwc` file. Prints `OK` on success.
pub fn check(file: &Path) -> Result<()> {
    let source = read_source(file)?;
    let program = parser::parse_program(&source)
        .with_context(|| format!("Failed to parse {}", file.display()))?;
    parser::validate_program(&program)
        .with_context(|| format!("Validation failed for {}", file.display()))?;
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

/// Load the current project, report the source file count. Same shape
/// as the old `Command::Test` handler.
pub fn test() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = project::find_project_root(&cwd)?;
    let loaded = project::load_project_from_root(&root)?;
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

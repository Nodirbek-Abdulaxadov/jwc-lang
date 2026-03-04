mod ast;
mod diag;
mod lexer;
mod migrate;
mod parser;
mod project;
mod runner;
mod server;
mod sql;

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "jwc", version, about = "JWC MVP CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new JWC project folder with jwcproj.json and main.jwc
    New { name: String },
    /// Parse and validate a .jwc schema file
    Check { file: PathBuf },
    /// Generate PostgreSQL CREATE TABLE SQL from entities
    GenSql { file: PathBuf },
    /// Run a JWC program from a .jwc file or project directory (defaults to current project)
    Run { path: Option<PathBuf> },
    /// Validate current project sources (searches jwcproj.json upward)
    Test,
    /// Build current project into bin/debug or bin/release
    Build {
        #[arg(long)]
        release: bool,
    },
    /// Manage SQL migrations for Postgres
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Start a real HTTP server for a JWC project
    Serve {
        /// Project directory or jwcproj.json (defaults to current dir)
        path: Option<PathBuf>,
        /// Port to listen on (default: 8080)
        #[arg(long, short, default_value_t = 8080)]
        port: u16,
    },
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Create new migration files
    New { name: String },
    /// Apply pending migrations to Postgres
    #[command(alias = "apply")]
    Up {
        #[arg(long)]
        database_url: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::New { name } => {
            let target = PathBuf::from(name);
            project::create_new_project(&target)?;
            println!("Created project: {}", target.display());
            println!("Try:");
            println!("  cd {}", target.display());
            println!("  jwc test");
            println!("  jwc build");
        }
        Command::Check { file } => {
            let source = read_source(&file)?;
            let program = parser::parse_program(&source)
                .with_context(|| format!("Failed to parse {}", file.display()))?;
            parser::validate_program(&program)
                .with_context(|| format!("Validation failed for {}", file.display()))?;
            println!("OK");
        }
        Command::GenSql { file } => {
            let source = read_source(&file)?;
            let program = parser::parse_program(&source)
                .with_context(|| format!("Failed to parse {}", file.display()))?;
            parser::validate_program(&program)
                .with_context(|| format!("Validation failed for {}", file.display()))?;
            let schema_sql = sql::generate_postgres_schema_sql(&program)?;
            print!("{}", schema_sql);
        }
        Command::Run { path } => {
            let target = path.unwrap_or(std::env::current_dir()?);

            if target.is_dir() {
                let root = project::find_project_root(&target)?;
                project::load_dotenv(&root);
                let loaded = project::load_project_from_root(&root)?;
                let result = runner::run_main(&loaded.program)?;
                if !result.output.is_empty() { print!("{}", result.output); }
                if let Some(port) = result.serve_port {
                    server::serve(&loaded.program, port)?;
                }
            } else if target
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case(project::PROJECT_FILE))
                .unwrap_or(false)
            {
                let root = target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Invalid project file path"))?
                    .to_path_buf();
                project::load_dotenv(&root);
                let loaded = project::load_project_from_root(&root)?;
                let result = runner::run_main(&loaded.program)?;
                if !result.output.is_empty() { print!("{}", result.output); }
                if let Some(port) = result.serve_port {
                    server::serve(&loaded.program, port)?;
                }
            } else {
                let source = read_source(&target)?;
                let program = parser::parse_program(&source)
                    .with_context(|| format!("Failed to parse {}", target.display()))?;
                parser::validate_program(&program)
                    .with_context(|| format!("Validation failed for {}", target.display()))?;
                let result = runner::run_main(&program)?;
                if !result.output.is_empty() { print!("{}", result.output); }
                if let Some(port) = result.serve_port {
                    server::serve(&program, port)?;
                }
            }
        }
        Command::Test => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            let loaded = project::load_project_from_root(&root)?;
            println!(
                "OK: project '{}' ({} source files)",
                loaded.manifest.name,
                loaded.source_files.len()
            );
        }
        Command::Build { release } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            let loaded = project::load_project_from_root(&root)?;

            let profile = if release { "release" } else { "debug" };
            let bin_dir = root.join("bin").join(profile);
            std::fs::create_dir_all(&bin_dir)?;

            let app_name = sanitize_app_name(&loaded.manifest.name);
            let out_path = bin_dir.join(&app_name);
            let script = build_launcher_script(&app_name);
            std::fs::write(&out_path, script)?;

            let runtime_src = std::env::current_exe()?;
            let runtime_dst = bin_dir.join("jwc-runtime");
            std::fs::copy(&runtime_src, &runtime_dst).with_context(|| {
                format!(
                    "Failed to copy runtime from {} to {}",
                    runtime_src.display(),
                    runtime_dst.display()
                )
            })?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&out_path)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&out_path, perms)?;

                let mut runtime_perms = std::fs::metadata(&runtime_dst)?.permissions();
                runtime_perms.set_mode(0o755);
                std::fs::set_permissions(&runtime_dst, runtime_perms)?;
            }

            println!("Build OK ({profile})");
            println!("Project: {}", loaded.manifest.name);
            println!("Executable: {}", out_path.display());
        }
        Command::Migrate { command } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            project::load_dotenv(&root);

            match command {
                MigrateCommand::New { name } => {
                    let created = migrate::create_migration(&root, &name)?;
                    println!("Migration created:");
                    println!("  {}", created.up_path.display());
                    println!("  {}", created.down_path.display());
                }
                MigrateCommand::Up { database_url } => {
                    let report = migrate::apply_pending_migrations(&root, database_url)?;
                    println!("Migrations applied: {}", report.applied);
                    println!("Already applied: {}", report.skipped);
                    println!("Total found: {}", report.total);
                }
            }
        }
        Command::Serve { path, port } => {
            let target = path.unwrap_or(std::env::current_dir()?);
            let root = if target.is_dir() {
                project::find_project_root(&target)?
            } else {
                target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Invalid project path"))?
                    .to_path_buf()
            };
            project::load_dotenv(&root);
            let loaded = project::load_project_from_root(&root)?;
            server::serve(&loaded.program, port)?;
        }
    }

    Ok(())
}

fn read_source(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

fn sanitize_app_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        }
    }
    if out.is_empty() {
        "app".to_string()
    } else {
        out
    }
}

fn build_launcher_script(_app_name: &str) -> String {
    r#"#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SELF_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "$SELF_DIR/jwc-runtime" run "$ROOT_DIR" "$@"
"#
    .to_string()
}

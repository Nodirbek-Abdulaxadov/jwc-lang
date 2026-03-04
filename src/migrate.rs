use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::project;
use crate::sql;

pub struct CreatedMigration {
    pub up_path: PathBuf,
    pub down_path: PathBuf,
}

pub struct ApplyReport {
    pub total: usize,
    pub applied: usize,
    pub skipped: usize,
}

pub fn create_migration(root: &Path, name: &str) -> Result<CreatedMigration> {
    let loaded = project::load_project_from_root(root)?;
    let schema_sql = sql::generate_postgres_schema_sql(&loaded.program)?;

    let migrations_dir = root.join("migrations");
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("Failed to create {}", migrations_dir.display()))?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("System clock is before UNIX_EPOCH"))?
        .as_secs();

    let slug = slugify(name);
    if slug.is_empty() {
        bail!("Migration name cannot be empty");
    }

    let base = format!("{}_{}", timestamp, slug);
    let up_path = migrations_dir.join(format!("{}.up.sql", base));
    let down_path = migrations_dir.join(format!("{}.down.sql", base));

    let up_content = if schema_sql.trim().is_empty() {
        "-- empty migration\n".to_string()
    } else {
        schema_sql
    };

    let down_content = "-- Write rollback SQL here\n".to_string();

    std::fs::write(&up_path, up_content)
        .with_context(|| format!("Failed to write {}", up_path.display()))?;
    std::fs::write(&down_path, down_content)
        .with_context(|| format!("Failed to write {}", down_path.display()))?;

    Ok(CreatedMigration { up_path, down_path })
}

pub fn apply_pending_migrations(root: &Path, database_url: Option<String>) -> Result<ApplyReport> {
    ensure_psql_installed()?;

    let url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .ok_or_else(|| {
            anyhow!("database url is required: pass --database-url or set DATABASE_URL")
        })?;

    let migrations_dir = root.join("migrations");
    if !migrations_dir.is_dir() {
        bail!(
            "migrations directory not found: {} (run 'jwc migrate new init' first)",
            migrations_dir.display()
        );
    }

    ensure_migration_table(&url)?;

    let mut migration_files: Vec<PathBuf> = std::fs::read_dir(&migrations_dir)
        .with_context(|| format!("Failed to read {}", migrations_dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".up.sql"))
                .unwrap_or(false)
        })
        .collect();

    migration_files.sort();

    let applied = read_applied_migrations(&url)?;
    let mut applied_now = 0usize;
    let mut skipped = 0usize;

    for file in &migration_files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("Invalid migration file name: {}", file.display()))?
            .to_string();

        if applied.contains(&name) {
            skipped += 1;
            continue;
        }

        run_psql_file(&url, file)?;
        mark_migration_applied(&url, &name)?;
        applied_now += 1;
    }

    Ok(ApplyReport {
        total: migration_files.len(),
        applied: applied_now,
        skipped,
    })
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;

    for ch in name.trim().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };

        if normalized == '-' {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(normalized);
            prev_dash = false;
        }
    }

    out.trim_matches('-').to_string()
}

fn ensure_psql_installed() -> Result<()> {
    let status = Command::new("psql")
        .arg("--version")
        .status()
        .context("Failed to execute 'psql --version'")?;

    if !status.success() {
        bail!("psql is required to run migrations")
    }

    Ok(())
}

fn ensure_migration_table(database_url: &str) -> Result<()> {
    let sql = r#"
CREATE TABLE IF NOT EXISTS _jwc_migrations (
    name text PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT now()
);
"#;
    run_psql_sql(database_url, sql)
}

fn read_applied_migrations(database_url: &str) -> Result<HashSet<String>> {
    let output = Command::new("psql")
        .arg(database_url)
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-At")
        .arg("-c")
        .arg("SELECT name FROM _jwc_migrations ORDER BY name;")
        .output()
        .context("Failed to execute psql for migration history")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to read applied migrations: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let set = stdout
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    Ok(set)
}

fn mark_migration_applied(database_url: &str, name: &str) -> Result<()> {
    let escaped = name.replace('\'', "''");
    let sql = format!(
        "INSERT INTO _jwc_migrations(name) VALUES ('{}') ON CONFLICT (name) DO NOTHING;",
        escaped
    );
    run_psql_sql(database_url, &sql)
}

fn run_psql_file(database_url: &str, file: &Path) -> Result<()> {
    let output = Command::new("psql")
        .arg(database_url)
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-f")
        .arg(file)
        .output()
        .with_context(|| format!("Failed to execute psql for {}", file.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Migration failed for {}: {}", file.display(), stderr.trim());
    }

    Ok(())
}

fn run_psql_sql(database_url: &str, sql: &str) -> Result<()> {
    let output = Command::new("psql")
        .arg(database_url)
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-c")
        .arg(sql)
        .output()
        .context("Failed to execute psql SQL command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("psql command failed: {}", stderr.trim());
    }

    Ok(())
}

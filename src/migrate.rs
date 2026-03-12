use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use postgres::{Client, NoTls};
use url::Url;

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
    let url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("JWC_DATABASE_URL").ok())
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

    ensure_database_exists(&url)?;

    let mut client = Client::connect(&url, NoTls)
        .with_context(|| "Failed to connect to database for migrations")?;

    ensure_migration_table(&mut client)?;

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

    let applied = read_applied_migrations(&mut client)?;
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

        run_migration_file(&mut client, file, &name)?;
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

fn ensure_database_exists(url: &str) -> Result<()> {
    let parsed = Url::parse(url).with_context(|| "Invalid DATABASE_URL")?;
    let dbname = parsed
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();

    if dbname.is_empty() {
        bail!("DATABASE_URL must include a database name");
    }

    let admin_db = std::env::var("JWC_ADMIN_DB").unwrap_or_else(|_| "postgres".to_string());
    if dbname == admin_db {
        return Ok(());
    }

    let mut admin_url = parsed;
    admin_url.set_path(&format!("/{}", admin_db));

    let mut admin_client = Client::connect(admin_url.as_str(), NoTls)
        .with_context(|| "Failed to connect to admin database to ensure target database exists")?;

    let exists = admin_client
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1;", &[&dbname])
        .with_context(|| "Failed to query pg_database")?
        .is_some();

    if !exists {
        let create_sql = format!("CREATE DATABASE {}", quote_identifier(&dbname));
        admin_client
            .batch_execute(&create_sql)
            .with_context(|| format!("Failed to create database '{}'", dbname))?;
    }

    Ok(())
}

fn quote_identifier(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    format!("\"{}\"", escaped)
}

fn ensure_migration_table(client: &mut Client) -> Result<()> {
    let sql = r#"
CREATE TABLE IF NOT EXISTS _jwc_migrations (
    name text PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT now()
);
"#;
    client
        .batch_execute(sql)
        .with_context(|| "Failed to ensure _jwc_migrations table")
}

fn read_applied_migrations(client: &mut Client) -> Result<HashSet<String>> {
    let rows = client
        .query("SELECT name FROM _jwc_migrations ORDER BY name;", &[])
        .with_context(|| "Failed to read applied migrations")?;

    let set = rows
        .into_iter()
        .map(|row| row.get::<usize, String>(0))
        .collect::<HashSet<_>>();

    Ok(set)
}

fn run_migration_file(client: &mut Client, file: &Path, name: &str) -> Result<()> {
    let sql = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read migration file {}", file.display()))?;

    let mut tx = client
        .transaction()
        .with_context(|| "Failed to start migration transaction")?;

    tx.batch_execute(&sql)
        .with_context(|| format!("Migration failed for {}", file.display()))?;

    tx.execute(
        "INSERT INTO _jwc_migrations(name) VALUES ($1) ON CONFLICT (name) DO NOTHING;",
        &[&name],
    )
    .with_context(|| "Failed to record applied migration")?;

    tx.commit()
        .with_context(|| "Failed to commit migration transaction")?;

    Ok(())
}

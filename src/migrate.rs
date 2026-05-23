use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use tokio_postgres::Client;
use url::Url;

use crate::engine;
use crate::project;
use crate::schema_diff;

/// Session-level advisory lock key used to serialise concurrent migration
/// runs. The constant is the byte sequence `b"jwc-mig"` interpreted as i64
/// — stable across processes, version-agnostic, and unlikely to collide with
/// application-level advisory locks.
const MIGRATION_LOCK_KEY: i64 = 0x6a77632d6d6967; // "jwc-mig" ASCII

pub struct CreatedMigration {
    pub up_path: PathBuf,
    pub down_path: PathBuf,
}

pub struct ApplyReport {
    pub total: usize,
    pub applied: usize,
    pub skipped: usize,
}

#[derive(Debug)]
pub struct RollbackReport {
    pub total_applied: usize,
    pub rolled_back: usize,
}

pub fn create_migration(root: &Path, name: &str) -> Result<CreatedMigration> {
    let loaded = project::load_project_from_root(root)?;

    let migrations_dir = root.join("migrations");
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("Failed to create {}", migrations_dir.display()))?;

    // Schema diff: reconstruct the previously-applied state from the
    // `migrations/` directory and compare it against the current entity
    // definitions. This keeps each migration scoped to what actually
    // changed, instead of re-emitting the full schema every time.
    let old_snapshots = schema_diff::read_latest_snapshot(&migrations_dir)?;
    let new_snapshots = schema_diff::program_to_snapshots(&loaded.program)?;
    let diff = schema_diff::compute_diff(&old_snapshots, &new_snapshots);

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

    let up_content = if diff.is_empty() {
        // No schema changes detected — leave a placeholder so the
        // migration file still exists for the user to fill in manually
        // (e.g. data migrations) without re-emitting the whole schema.
        "-- no schema changes\n".to_string()
    } else {
        schema_diff::diff_to_sql(&diff)
    };

    let down_content = "-- Write rollback SQL here\n".to_string();

    std::fs::write(&up_path, up_content)
        .with_context(|| format!("Failed to write {}", up_path.display()))?;
    std::fs::write(&down_path, down_content)
        .with_context(|| format!("Failed to write {}", down_path.display()))?;

    Ok(CreatedMigration { up_path, down_path })
}

pub async fn apply_pending_migrations(
    root: &Path,
    database_url: Option<String>,
) -> Result<ApplyReport> {
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

    ensure_database_exists(&url).await?;

    let client = engine::connect_for_migrations(&url).await?;

    let _lock = MigrationLock::acquire(&client).await?;

    ensure_migration_table(&client).await?;

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

    let applied = read_applied_migrations(&client).await?;
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

        run_migration_file(&client, file, &name).await?;
        applied_now += 1;
    }

    Ok(ApplyReport {
        total: migration_files.len(),
        applied: applied_now,
        skipped,
    })
}

pub async fn rollback_migrations(
    root: &Path,
    database_url: Option<String>,
    steps: usize,
) -> Result<RollbackReport> {
    if steps == 0 {
        return Ok(RollbackReport {
            total_applied: 0,
            rolled_back: 0,
        });
    }

    let url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("JWC_DATABASE_URL").ok())
        .ok_or_else(|| {
            anyhow!("database url is required: pass --database-url or set DATABASE_URL")
        })?;

    let migrations_dir = root.join("migrations");
    if !migrations_dir.is_dir() {
        bail!(
            "migrations directory not found: {}",
            migrations_dir.display()
        );
    }

    ensure_database_exists(&url).await?;

    let client = engine::connect_for_migrations(&url).await?;

    let _lock = MigrationLock::acquire(&client).await?;

    ensure_migration_table(&client).await?;

    let applied_names = read_applied_migrations_ordered_desc(&client).await?;
    let total_applied = applied_names.len();

    if total_applied == 0 {
        bail!("no migrations have been applied yet");
    }

    let to_rollback: Vec<String> = applied_names.into_iter().take(steps).collect();
    let mut rolled_back = 0usize;

    for name in &to_rollback {
        let down_filename = name
            .strip_suffix(".up.sql")
            .map(|base| format!("{}.down.sql", base))
            .unwrap_or_else(|| format!("{}.down.sql", name));

        let down_path = migrations_dir.join(&down_filename);
        if !down_path.is_file() {
            bail!("no rollback SQL for migration {}", name);
        }

        let sql = std::fs::read_to_string(&down_path)
            .with_context(|| format!("Failed to read {}", down_path.display()))?;

        let sql_trimmed = sql
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");

        if sql_trimmed.is_empty() {
            bail!("no rollback SQL for migration {}", name);
        }

        client
            .batch_execute("BEGIN;")
            .await
            .with_context(|| "Failed to start rollback transaction")?;

        let inner = async {
            client
                .batch_execute(&sql)
                .await
                .with_context(|| format!("Rollback failed for {}", down_path.display()))?;

            client
                .execute("DELETE FROM _jwc_migrations WHERE name = $1;", &[name])
                .await
                .with_context(|| "Failed to remove rolled-back migration record")?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match inner {
            Ok(()) => {
                client
                    .batch_execute("COMMIT;")
                    .await
                    .with_context(|| "Failed to commit rollback transaction")?;
            }
            Err(e) => {
                let _ = client.batch_execute("ROLLBACK;").await;
                return Err(e);
            }
        }

        rolled_back += 1;
    }

    Ok(RollbackReport {
        total_applied,
        rolled_back,
    })
}

async fn read_applied_migrations_ordered_desc(client: &Client) -> Result<Vec<String>> {
    let rows = client
        .query("SELECT name FROM _jwc_migrations ORDER BY name DESC;", &[])
        .await
        .with_context(|| "Failed to read applied migrations")?;

    Ok(rows
        .into_iter()
        .map(|row| row.get::<usize, String>(0))
        .collect())
}

/// Session advisory lock guard. Acquired before any migration work runs, so
/// two concurrent `jwc migrate up` / `down` invocations can't race on the
/// `_jwc_migrations` table. Released when the client connection drops (the
/// lock is session-scoped), and we also try an explicit unlock for tidiness.
///
/// Acquisition uses `pg_try_advisory_lock` so that a busy migration job
/// fails fast with a clear error instead of blocking indefinitely.
struct MigrationLock;

impl MigrationLock {
    async fn acquire(client: &Client) -> Result<MigrationLock> {
        let row = client
            .query_one("SELECT pg_try_advisory_lock($1);", &[&MIGRATION_LOCK_KEY])
            .await
            .with_context(|| "Failed to request migration advisory lock")?;
        let got: bool = row.try_get(0)?;
        if !got {
            bail!(
                "another migration run is in progress (advisory lock {} held)",
                MIGRATION_LOCK_KEY
            );
        }
        Ok(MigrationLock)
    }
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

async fn ensure_database_exists(url: &str) -> Result<()> {
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

    let admin_client = engine::connect_for_migrations(admin_url.as_str())
        .await
        .with_context(|| "Failed to connect to admin database to ensure target database exists")?;

    let exists = admin_client
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1;", &[&dbname])
        .await
        .with_context(|| "Failed to query pg_database")?
        .is_some();

    if !exists {
        let create_sql = format!("CREATE DATABASE {}", quote_identifier(&dbname));
        admin_client
            .batch_execute(&create_sql)
            .await
            .with_context(|| format!("Failed to create database '{}'", dbname))?;
    }

    Ok(())
}

fn quote_identifier(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    format!("\"{}\"", escaped)
}

async fn ensure_migration_table(client: &Client) -> Result<()> {
    let sql = r#"
CREATE TABLE IF NOT EXISTS _jwc_migrations (
    name text PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT now()
);
"#;
    client
        .batch_execute(sql)
        .await
        .with_context(|| "Failed to ensure _jwc_migrations table")
}

async fn read_applied_migrations(client: &Client) -> Result<HashSet<String>> {
    let rows = client
        .query("SELECT name FROM _jwc_migrations ORDER BY name;", &[])
        .await
        .with_context(|| "Failed to read applied migrations")?;

    let set = rows
        .into_iter()
        .map(|row| row.get::<usize, String>(0))
        .collect::<HashSet<_>>();

    Ok(set)
}

async fn run_migration_file(client: &Client, file: &Path, name: &str) -> Result<()> {
    let sql = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read migration file {}", file.display()))?;

    // `transaction()` requires &mut Client. Use SAVEPOINT-style explicit BEGIN/COMMIT instead.
    client
        .batch_execute("BEGIN;")
        .await
        .with_context(|| "Failed to start migration transaction")?;

    let res = async {
        client
            .batch_execute(&sql)
            .await
            .with_context(|| format!("Migration failed for {}", file.display()))?;

        client
            .execute(
                "INSERT INTO _jwc_migrations(name) VALUES ($1) ON CONFLICT (name) DO NOTHING;",
                &[&name],
            )
            .await
            .with_context(|| "Failed to record applied migration")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    match res {
        Ok(()) => {
            client
                .batch_execute("COMMIT;")
                .await
                .with_context(|| "Failed to commit migration transaction")?;
            Ok(())
        }
        Err(e) => {
            let _ = client.batch_execute("ROLLBACK;").await;
            Err(e)
        }
    }
}

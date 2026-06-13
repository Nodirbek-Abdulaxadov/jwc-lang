//! `jwc migrate new / up / down / status` command implementations split
//! out from main.rs for testability and to keep the CLI shell thin.

use std::path::Path;

use anyhow::Result;

use crate::migrate;

/// Create a new migration timestamped pair under `migrations/`.
pub fn new(root: &Path, name: &str) -> Result<()> {
    let created = migrate::create_migration(root, name)?;
    println!("Migration created:");
    println!("  {}", created.up_path.display());
    println!("  {}", created.down_path.display());
    Ok(())
}

/// Apply every pending migration in order, advisory-locked. With
/// `dry_run=true`, print the SQL each migration would execute without
/// touching the database (still verifies stored checksums against the
/// on-disk files).
pub async fn up(root: &Path, database_url: Option<String>, dry_run: bool) -> Result<()> {
    let report = migrate::apply_pending_migrations(root, database_url, dry_run).await?;
    if dry_run {
        println!("(dry-run) Migrations that would apply: {}", report.applied);
    } else {
        println!("Migrations applied: {}", report.applied);
    }
    println!("Already applied: {}", report.skipped);
    println!("Total found: {}", report.total);
    Ok(())
}

/// List every migration file in `migrations/` (offline — no DB query).
pub fn list(root: &Path) -> Result<()> {
    let files = migrate::list_migrations(root)?;
    if files.is_empty() {
        println!(
            "No migrations found under {}",
            root.join("migrations").display()
        );
        return Ok(());
    }
    println!("Migrations ({}):", files.len());
    for file in &files {
        if let Some(name) = file.file_name().and_then(|n| n.to_str()) {
            println!("  {name}");
        }
    }
    Ok(())
}

/// Roll back the most recent `steps` applied migrations. `steps == 0` is
/// a no-op (matches the CLI's behaviour before extraction). `dry_run=true`
/// prints the rollback SQL without executing it.
pub async fn down(
    root: &Path,
    database_url: Option<String>,
    steps: usize,
    dry_run: bool,
) -> Result<()> {
    if steps == 0 {
        println!("No-op (steps=0)");
        return Ok(());
    }
    let report = migrate::rollback_migrations(root, database_url, steps, dry_run).await?;
    if dry_run {
        println!("(dry-run) Would roll back: {}", report.rolled_back);
    } else {
        println!("Rolled back: {}", report.rolled_back);
    }
    println!("Previously applied: {}", report.total_applied);
    Ok(())
}

/// `jwc migrate status` — applied / pending / sha-mismatch / orphan
/// matrix against the database referenced by `--database-url` (or
/// `DATABASE_URL` / `JWC_DATABASE_URL`).
pub async fn status(root: &Path, database_url: Option<String>) -> Result<()> {
    let rows = migrate::migration_status(root, database_url).await?;
    if rows.is_empty() {
        println!("No migrations found.");
        return Ok(());
    }
    println!(
        "{:60} {:>9}  {:24}  {}",
        "version", "applied?", "applied_at", "sha"
    );
    for r in &rows {
        let applied = if r.applied { "yes" } else { "no" };
        let when = r.applied_at.clone().unwrap_or_else(|| "—".to_string());
        println!(
            "{:60} {:>9}  {:24}  {}",
            r.name,
            applied,
            when,
            r.sha_state.label(),
        );
    }
    Ok(())
}

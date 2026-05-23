//! `jwc migrate new / up / down` command implementations split out from
//! main.rs for testability and to keep the CLI shell thin.

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

/// Apply every pending migration in order, advisory-locked.
pub async fn up(root: &Path, database_url: Option<String>) -> Result<()> {
    let report = migrate::apply_pending_migrations(root, database_url).await?;
    println!("Migrations applied: {}", report.applied);
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
/// a no-op (matches the CLI's behaviour before extraction).
pub async fn down(root: &Path, database_url: Option<String>, steps: usize) -> Result<()> {
    if steps == 0 {
        println!("No-op (steps=0)");
        return Ok(());
    }
    let report = migrate::rollback_migrations(root, database_url, steps).await?;
    println!("Rolled back: {}", report.rolled_back);
    println!("Previously applied: {}", report.total_applied);
    Ok(())
}

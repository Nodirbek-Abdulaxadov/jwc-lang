use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::JwcConfig;
use crate::schema_snapshot::{load_snapshot, save_snapshot, SchemaSnapshot};

pub struct MigrationPaths {
    pub dir: PathBuf,
    pub snapshot_path: PathBuf,
}

pub fn migration_paths(base_dir: &Path, cfg: &JwcConfig) -> MigrationPaths {
    let dir = base_dir.join(cfg.migrations_dir());
    let snapshot_path = dir.join("schema.snapshot.json");
    MigrationPaths { dir, snapshot_path }
}

pub fn sanitize_migration_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch == '-' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() {
        "migration".to_string()
    } else {
        out
    }
}

pub fn new_migration_id(name: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}_{}", ts, sanitize_migration_name(name))
}

pub fn create_migration(
    base_dir: &Path,
    cfg: &JwcConfig,
    migration_name: &str,
    new_program: &crate::ast::Program,
) -> Result<(String, PathBuf)> {
    let paths = migration_paths(base_dir, cfg);
    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("Failed to create migrations dir {}", paths.dir.display()))?;

    let prev = load_snapshot(&paths.snapshot_path)?;
    let old_program = match prev {
        Some(s) => s.to_program(),
        None => crate::ast::Program::new(),
    };

    let sql = crate::migrate::generate_postgres_migration_sql(&old_program, new_program)?;
    let migration_id = new_migration_id(migration_name);

    let sql_path = paths.dir.join(format!("{}.sql", migration_id));
    if sql_path.exists() {
        bail!("Migration already exists: {}", sql_path.display());
    }

    std::fs::write(&sql_path, &sql)
        .with_context(|| format!("Failed to write migration {}", sql_path.display()))?;

    let snap = SchemaSnapshot::from_program(new_program);
    save_snapshot(&paths.snapshot_path, &snap)?;

    Ok((migration_id, sql_path))
}

pub fn list_local_migration_sql_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read migrations dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sql"))
        .collect();

    files.sort();
    Ok(files)
}

pub fn migration_name_from_sql_path(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Invalid migration filename: {}", path.display()))?;
    Ok(stem.to_string())
}

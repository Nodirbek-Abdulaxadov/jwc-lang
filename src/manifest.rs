use std::path::{Path, PathBuf};
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct JwcProjectManifest {
    #[serde(rename = "Name", alias = "name")]
    pub name: Option<String>,

    #[serde(rename = "Version", alias = "version")]
    pub version: Option<String>,

    #[serde(rename = "Files", alias = "files")]
    pub files: Option<Vec<String>>,

    #[serde(rename = "Dirs", alias = "dirs")]
    pub dirs: Option<Vec<String>>,

    #[serde(rename = "Config", alias = "config")]
    pub config: Option<String>,

    #[serde(rename = "MigrationsDir", alias = "migrationsDir")]
    pub migrations_dir: Option<String>,

    // Inline config (lets jwcproj.json be the only config file)
    #[serde(
        rename = "ConnectionStrings",
        alias = "connectionStrings",
        alias = "connection_strings"
    )]
    pub connection_strings: Option<BTreeMap<String, String>>,

    #[serde(
        rename = "DatabaseUrl",
        alias = "databaseUrl",
        alias = "DefaultConnection",
        alias = "defaultConnection"
    )]
    pub database_url: Option<String>,

    #[serde(
        rename = "ConnectionString",
        alias = "connectionString",
        alias = "connection_string"
    )]
    pub connection_string: Option<String>,

    #[serde(rename = "DbContext", alias = "dbContext")]
    pub dbcontext: Option<String>,
}

impl JwcProjectManifest {
    pub fn default_connection_string(&self) -> Option<String> {
        if let Some(v) = &self.connection_string {
            if !v.trim().is_empty() {
                return Some(v.clone());
            }
        }

        let map = self.connection_strings.as_ref()?;
        for k in ["DefaultConnection", "defaultConnection", "default_connection"] {
            if let Some(v) = map.get(k) {
                if !v.trim().is_empty() {
                    return Some(v.clone());
                }
            }
        }
        // If there's exactly one entry (or user used custom names like postgres1), use the first.
        map.values().next().cloned().filter(|v| !v.trim().is_empty())
    }
}

impl JwcProjectManifest {
    pub fn load(path: &Path) -> Result<JwcProjectManifest> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read project manifest {}", path.display()))?;
        let manifest: JwcProjectManifest = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse project manifest {}", path.display()))?;
        Ok(manifest)
    }
}

pub fn is_manifest_path(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if file_name == "jwcproj.json" {
        return true;
    }
    if file_name.ends_with(".jwcproj.json") {
        return true;
    }
    if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| e.eq_ignore_ascii_case("jwcproj"))
        .unwrap_or(false)
    {
        return true;
    }
    false
}

pub fn resolve_relative(base_dir: &Path, maybe_rel: &str) -> PathBuf {
    let p = PathBuf::from(maybe_rel);
    if p.is_absolute() {
        p
    } else {
        base_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_connection_string_field_as_default() {
        let raw = r#"
        {
            "name": "t",
            "connection_string": "Server=localhost;Database=todo_db1;User Id=postgres;Password=1234;"
        }
        "#;

        let m: JwcProjectManifest = serde_json::from_str(raw).unwrap();
        let cs = m.default_connection_string().unwrap();
        assert!(cs.contains("Database=todo_db1"));
    }
}

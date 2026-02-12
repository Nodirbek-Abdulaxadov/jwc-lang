use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JwcConfig {
    pub database_url: Option<String>,
    pub dbcontext: Option<String>,
    pub migrations_dir: Option<String>,
}

impl JwcConfig {
    pub fn migrations_dir(&self) -> String {
        self.migrations_dir
            .clone()
            .unwrap_or_else(|| "migrations".to_string())
    }
}

pub fn load_config(explicit_path: Option<PathBuf>) -> Result<JwcConfig> {
    load_config_in_dir(&PathBuf::from("."), explicit_path)
}

pub fn load_config_in_dir(base_dir: &Path, explicit_path: Option<PathBuf>) -> Result<JwcConfig> {
    let path = if let Some(p) = explicit_path {
        if p.is_absolute() {
            p
        } else {
            base_dir.join(p)
        }
    } else if base_dir.join("config.json").exists() {
        base_dir.join("config.json")
    } else {
        return Ok(JwcConfig::default());
    };

    if !Path::new(&path).exists() {
        return Ok(JwcConfig::default());
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config {}", path.display()))?;

    // JSON (v1-style)
    let json: JsonConfig = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse config {}", path.display()))?;

    Ok(JwcConfig {
        database_url: json
            .connection_strings
            .and_then(|c| c.default_connection)
            .or(json.database_url),
        dbcontext: json.dbcontext,
        migrations_dir: json.migrations_dir,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct JsonConfig {
    #[serde(rename = "ConnectionStrings", alias = "connectionStrings")]
    connection_strings: Option<ConnectionStrings>,

    #[serde(rename = "DefaultConnection", alias = "defaultConnection")]
    database_url: Option<String>,

    #[serde(rename = "DbContext", alias = "dbContext")]
    dbcontext: Option<String>,

    #[serde(rename = "MigrationsDir", alias = "migrationsDir")]
    migrations_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConnectionStrings {
    #[serde(rename = "DefaultConnection", alias = "defaultConnection")]
    default_connection: Option<String>,
}

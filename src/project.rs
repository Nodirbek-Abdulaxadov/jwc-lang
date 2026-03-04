use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::ast::Program;

pub const PROJECT_FILE: &str = "jwcproj.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwcProject {
    pub name: String,
    /// Supports both "languageVersion" (old) and "version" (new) field names
    #[serde(rename = "languageVersion", default, skip_serializing_if = "String::is_empty")]
    pub language_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

impl JwcProject {
    pub fn effective_version(&self) -> &str {
        if !self.version.is_empty() {
            &self.version
        } else if !self.language_version.is_empty() {
            &self.language_version
        } else {
            "0.1"
        }
    }
}

pub struct LoadedProject {
    pub manifest: JwcProject,
    pub source_files: Vec<PathBuf>,
    pub program: Program,
}

pub fn create_new_project(target_dir: &Path) -> Result<()> {
    if target_dir.exists() {
        if !target_dir.is_dir() {
            bail!("Target path is not a directory: {}", target_dir.display());
        }
        if target_dir.read_dir()?.next().is_some() {
            bail!("Target directory is not empty: {}", target_dir.display());
        }
    } else {
        std::fs::create_dir_all(target_dir)
            .with_context(|| format!("Failed to create {}", target_dir.display()))?;
    }

    let name = target_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Invalid project folder name"))?
        .to_string();

    let manifest = JwcProject {
        name,
        language_version: String::new(),
        version: "1.0.0".to_string(),
        dependencies: Vec::new(),
    };

    let proj_filename = format!("{}.jwcproj", manifest.name);
    let manifest_path = target_dir.join(&proj_filename);
    let main_path = target_dir.join("main.jwc");

    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, manifest_json)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;

    let main_content = "function main() {\n    print(\"Hello from JWC\");\n}\n";
    std::fs::write(&main_path, main_content)
        .with_context(|| format!("Failed to write {}", main_path.display()))?;
    Ok(())
}

/// Find the `.jwcproj` or `jwcproj.json` file in `dir`, returns its path if found.
fn find_manifest_in_dir(dir: &Path) -> Option<PathBuf> {
    // Prefer *.jwcproj
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("jwcproj"))
                .unwrap_or(false)
                && path.is_file()
            {
                return Some(path);
            }
        }
    }
    // Fallback to jwcproj.json
    let legacy = dir.join(PROJECT_FILE);
    if legacy.is_file() {
        return Some(legacy);
    }
    None
}

pub fn find_project_root(start: &Path) -> Result<PathBuf> {
    let start_dir = if start.is_file() {
        start
            .parent()
            .ok_or_else(|| anyhow!("Invalid start file path"))?
            .to_path_buf()
    } else {
        start.to_path_buf()
    };

    let mut current = start_dir.as_path();
    loop {
        if find_manifest_in_dir(current).is_some() {
            return Ok(current.to_path_buf());
        }

        current = match current.parent() {
            Some(parent) => parent,
            None => break,
        };
    }

    bail!("jwc project not found")
}

pub fn load_project_from_root(root: &Path) -> Result<LoadedProject> {
    let manifest_path = find_manifest_in_dir(root)
        .ok_or_else(|| anyhow!("jwc project not found"))?;

    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: JwcProject = serde_json::from_str(&manifest_raw)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    let source_files = collect_jwc_files(root)?;
    if source_files.is_empty() {
        bail!("No .jwc source files found in project root");
    }

    let has_main = source_files.iter().any(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("main.jwc"))
            .unwrap_or(false)
    });
    if !has_main {
        bail!("Project main.jwc not found");
    }

    let mut source_text = String::new();
    for path in &source_files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        source_text.push_str(&format!("// file: {rel}\n"));
        source_text.push_str(&content);
        if !source_text.ends_with('\n') {
            source_text.push('\n');
        }
        source_text.push('\n');
    }

    let program = crate::parser::parse_program(&source_text)?;
    crate::parser::validate_program(&program)?;

    Ok(LoadedProject {
        manifest,
        source_files,
        program,
    })
}

fn collect_jwc_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| {
        let a_main = a
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("main.jwc"))
            .unwrap_or(false);
        let b_main = b
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("main.jwc"))
            .unwrap_or(false);

        match (a_main, b_main) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        }
    });
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("target") {
                continue;
            }
            walk(root, &path, out)?;
            continue;
        }

        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jwc"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }

    let _ = root;
    Ok(())
}

/// Load a `.env` file from `dir` (if it exists) into the process environment.
/// Lines are parsed as `KEY=VALUE`. Comments (`#`) and blank lines are skipped.
pub fn load_dotenv(dir: &Path) {
    let env_path = dir.join(".env");
    let Ok(content) = std::fs::read_to_string(&env_path) else { return };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            // Don't override vars already set in the environment
            if std::env::var(key).is_err() {
                std::env::set_var(key, val);
            }
        }
    }

    // Auto-build DATABASE_URL from PG_* vars if not already set
    if std::env::var("DATABASE_URL").is_err() {
        if let (Ok(user), Ok(password), Ok(host), Ok(port), Ok(db)) = (
            std::env::var("PG_USER"),
            std::env::var("PG_PASSWORD"),
            std::env::var("PG_HOST"),
            std::env::var("PG_PORT"),
            std::env::var("PG_DATABASE"),
        ) {
            let url = format!("postgresql://{}:{}@{}:{}/{}", user, password, host, port, db);
            std::env::set_var("DATABASE_URL", url);
        }
    }
}

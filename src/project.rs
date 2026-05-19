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
    let gitignore_path = target_dir.join(".gitignore");
    let env_example_path = target_dir.join(".env.example");

    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, manifest_json)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;

    let main_content = "function main() {\n    print(\"Hello from JWC\");\n}\n";
    std::fs::write(&main_path, main_content)
        .with_context(|| format!("Failed to write {}", main_path.display()))?;

    // Sensible defaults: keep secrets, bundled binaries, IDE/OS cruft out
    // of the repo. Migrations stay tracked — they're the schema history.
    let gitignore = "\
# Local secrets / env
.env
.env.local

# jwc bundle output
bin/

# Rust toolchain output (only relevant if you check the compiler in)
target/

# Editors
.vscode/
.idea/
*.swp
*.swo

# OS junk
.DS_Store
Thumbs.db
";
    std::fs::write(&gitignore_path, gitignore)
        .with_context(|| format!("Failed to write {}", gitignore_path.display()))?;

    let env_example = "\
# Postgres connection details. JWC auto-assembles DATABASE_URL from these
# the first time the runtime loads .env, so a `setConnectionString(...)`
# call in main() is NOT required.
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=\
";
    let env_example = format!("{}{}\n", env_example, manifest.name);
    std::fs::write(&env_example_path, env_example)
        .with_context(|| format!("Failed to write {}", env_example_path.display()))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("jwc-newproj-{}-{}-{}", label, std::process::id(), nanos))
    }

    #[test]
    fn create_new_project_lays_down_starter_files() {
        let dir = unique_tmp_dir("starter");
        create_new_project(&dir).expect("create project");

        // The manifest file is named after the directory's basename.
        let project_name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .expect("temp dir name")
            .to_string();
        assert!(
            dir.join(format!("{}.jwcproj", project_name)).is_file(),
            ".jwcproj not created"
        );
        assert!(dir.join("main.jwc").is_file());
        assert!(dir.join(".gitignore").is_file(), ".gitignore not created");
        assert!(dir.join(".env.example").is_file(), ".env.example not created");

        let gi = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gi.contains(".env"));
        assert!(gi.contains("bin/"));
        assert!(gi.contains("target/"));

        let env_ex = std::fs::read_to_string(dir.join(".env.example")).unwrap();
        assert!(env_ex.contains("PG_HOST"));
        assert!(env_ex.contains("PG_DATABASE="));

        // Be tidy.
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::ast::Program;

pub const PROJECT_FILE: &str = "jwcproj.json";
pub const LOCK_FILE: &str = "jwcproj.lock";

/// One dependency declaration. The JSON form is flexible:
///
/// ```json
/// "http": "^1.2",                           // version range, registry source
/// "auth": { "version": "^1", "registry": "https://registry.example/" },
/// "shared": { "path": "../shared" },
/// "ext": { "git": "https://github.com/x/y", "rev": "abc123" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum DepSpec {
    /// `"http": "^1.2"` — shorthand for a registry source with this version
    /// requirement and the project's default registry URL.
    Version(String),
    /// Detailed form. Exactly one of `path`, `git`, or `version` is required.
    Detailed(DetailedDep),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DetailedDep {
    /// Semver requirement (e.g. `^1.2`, `>=0.3, <0.5`). Required for registry
    /// or git sources where the resolver needs a version range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Local filesystem source. Relative to the project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Git source URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    /// Specific git revision (commit SHA or tag). Optional with `git`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Override registry URL. Defaults to the project's `registry.url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}

/// Registry server configuration. Read from manifest, then overridden by
/// `JWC_REGISTRY_URL` env at runtime if set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RegistryConfig {
    /// Base URL for the index/download API (Cargo-shaped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What this project IS. `App` is runnable (`jwc run/serve/build`),
/// `Pkg` is a library — depend-on-only. Default is `App` for compatibility
/// with projects that haven't adopted the `type` field yet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    #[default]
    App,
    Pkg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwcProject {
    pub name: String,
    /// `"app"` (default) — runnable. `"pkg"` — library, depend-on-only.
    #[serde(default, rename = "type")]
    pub kind: ProjectKind,
    /// Supports both "languageVersion" (old) and "version" (new) field names.
    /// Note: this is the *project / language* version (used by `effective_version`),
    /// not the package's published version — that lives in `pkg_version`.
    #[serde(
        rename = "languageVersion",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub language_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Semver version of THIS package, used when other projects depend on it
    /// via a path/git/registry source. Distinct from `version` (above) to
    /// avoid breaking legacy projects that store the language version there.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "pkgVersion"
    )]
    pub pkg_version: Option<String>,
    /// Structured dependency map. The legacy `Vec<String>` form is no longer
    /// accepted — use either a version string or a detailed object per name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DepSpec>,
    /// Optional registry configuration. `JWC_REGISTRY_URL` env wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<RegistryConfig>,
}

impl JwcProject {
    /// Fail if this project is a `pkg` — used by `jwc run/serve/build` so
    /// libraries don't try to behave like runnable apps.
    pub fn ensure_runnable(&self) -> Result<()> {
        if matches!(self.kind, ProjectKind::Pkg) {
            bail!(
                "Project '{}' is declared as a package (type: \"pkg\") and cannot be run directly. \
                Depend on it from an app project, or change its type to \"app\".",
                self.name
            );
        }
        Ok(())
    }
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
        kind: ProjectKind::App,
        language_version: String::new(),
        version: "1.0.0".to_string(),
        pkg_version: Some("0.1.0".to_string()),
        dependencies: BTreeMap::new(),
        registry: None,
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

/// Strip JSONC-style `//` line comments and `/* ... */` block comments from a
/// raw manifest string, preserving content inside double-quoted strings.
/// Trailing commas before `}` or `]` are also tolerated (a common pitfall when
/// the user removes a field and forgets to clean up the preceding comma).
pub fn strip_jsonc_comments(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c as char);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push(c as char);
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                // line comment — skip until newline (keep the newline)
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                // block comment — skip until closing */
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
        }
        out.push(c as char);
        i += 1;
    }

    // Cheap trailing-comma fix: `,<whitespace>}` → `<whitespace>}` and same for `]`.
    // Won't touch commas inside strings because we already stripped/preserved above.
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut j = 0;
    let mut in_str = false;
    while j < chars.len() {
        let ch = chars[j];
        if in_str {
            cleaned.push(ch);
            if ch == '\\' && j + 1 < chars.len() {
                cleaned.push(chars[j + 1]);
                j += 2;
                continue;
            }
            if ch == '"' {
                in_str = false;
            }
            j += 1;
            continue;
        }
        if ch == '"' {
            in_str = true;
            cleaned.push(ch);
            j += 1;
            continue;
        }
        if ch == ',' {
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if k < chars.len() && (chars[k] == '}' || chars[k] == ']') {
                // skip the comma; whitespace and the close bracket flow through
                j += 1;
                continue;
            }
        }
        cleaned.push(ch);
        j += 1;
    }
    cleaned
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

/// Per-load knobs for [`load_project_from_root_with`]. Today only the
/// gradual type checker is opt-out; future flags (lint pre-pass, schema
/// snapshot refresh) can hang off the same struct without breaking the
/// existing call sites that use the default [`load_project_from_root`].
#[derive(Debug, Clone, Copy)]
pub struct LoadOpts {
    /// When `false`, skip [`crate::typecheck::typecheck_program`]. The
    /// `--no-typecheck` CLI flag on `jwc run` / `jwc check` /
    /// `jwc build` flips this to support the transition release per
    /// `PRODUCTION_READINESS_PLAN.md` Phase 3 exit criteria.
    pub typecheck: bool,
}

impl Default for LoadOpts {
    fn default() -> Self {
        Self { typecheck: true }
    }
}

pub fn load_project_from_root(root: &Path) -> Result<LoadedProject> {
    load_project_from_root_with(root, LoadOpts::default())
}

pub fn load_project_from_root_with(root: &Path, opts: LoadOpts) -> Result<LoadedProject> {
    let manifest_path =
        find_manifest_in_dir(root).ok_or_else(|| anyhow!("jwc project not found"))?;

    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest_json = strip_jsonc_comments(&manifest_raw);
    let manifest: JwcProject = serde_json::from_str(&manifest_json)
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

    let mut program = Program::default();
    for path in &source_files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let file_prog = crate::parser::parse_program_with_label(&content, &rel)
            .with_context(|| format!("Failed to parse {}", rel))?;
        merge_program(&mut program, file_prog).with_context(|| format!("While merging {}", rel))?;
    }

    // Resolve and merge dependency packages, if any. The resolver caches
    // sources under `~/.jwc/registry/` so subsequent loads are fast; for
    // path sources nothing is downloaded.
    if !manifest.dependencies.is_empty() {
        let graph = crate::resolver::ensure_resolved(&manifest, root)
            .with_context(|| "Failed to resolve dependencies")?;
        for pkg in graph.iter() {
            merge_dep_package(&mut program, pkg)
                .with_context(|| format!("Failed to merge dependency '{}'", pkg.name))?;
        }
    }

    crate::parser::validate_program(&program)?;
    if opts.typecheck {
        crate::typecheck::typecheck_program(&program)?;
    }

    Ok(LoadedProject {
        manifest,
        source_files,
        program,
    })
}

/// Walk a resolved dependency's source directory, parse every `.jwc` file
/// it contains, and merge the result into `program`. Decls with no
/// explicit `namespace` declaration get the package name as their default
/// namespace so two deps with a clashing simple name don't collide.
fn merge_dep_package(program: &mut Program, pkg: &crate::resolver::ResolvedPackage) -> Result<()> {
    let pkg_files = collect_jwc_files(&pkg.source_path).unwrap_or_default();
    for path in &pkg_files {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut file_prog = crate::parser::parse_program(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        apply_default_namespace(&mut file_prog, &pkg.name);
        // Dependency packages' error handlers are ignored — only the root
        // project's errorHandler is honoured.
        file_prog.error_handler = None;
        // Mounts are activation decisions that belong to the consumer; a
        // library declaring its own mounts shouldn't auto-activate them in
        // the host project.
        file_prog.mounts.clear();
        merge_program(program, file_prog)?;
    }
    Ok(())
}

/// If a decl has no namespace tag, assign one based on the package name.
/// Decls that already declared their own `namespace` keep it.
fn apply_default_namespace(prog: &mut Program, default_ns: &str) {
    let default = vec![default_ns.to_string()];
    for f in &mut prog.functions {
        if f.namespace.is_empty() {
            f.namespace = default.clone();
        }
    }
    for m in &mut prog.models {
        if m.namespace.is_empty() {
            m.namespace = default.clone();
        }
    }
    for r in &mut prog.routes {
        if r.namespace.is_empty() {
            r.namespace = default.clone();
        }
    }
    for mw in &mut prog.middlewares {
        if mw.namespace.is_empty() {
            mw.namespace = default.clone();
        }
    }
    for c in &mut prog.dbcontexts {
        if c.namespace.is_empty() {
            c.namespace = default.clone();
        }
    }
}

/// Merge `incoming` (a per-file Program) into `combined`. Each file's
/// declarations already carry the file's namespace tag; the merge
/// concatenates the lists, enforces the single-errorHandler invariant,
/// and shifts every incoming decl's `file_idx` so it still points at the
/// right entry in `combined.sources` after the source vec grows.
pub fn merge_program(combined: &mut Program, mut incoming: Program) -> Result<()> {
    let base = combined.sources.len();
    if base > 0 {
        // Shift every per-decl file_idx so it indexes into the post-merge
        // `combined.sources` instead of `incoming.sources`. Without this,
        // a validator error on (say) the second file's third route would
        // render against the first file's source text — pointing at the
        // wrong line.
        for d in &mut incoming.dbcontexts {
            d.file_idx += base;
        }
        for d in &mut incoming.models {
            d.file_idx += base;
        }
        for d in &mut incoming.routes {
            d.file_idx += base;
        }
        for d in &mut incoming.functions {
            d.file_idx += base;
        }
        for d in &mut incoming.middlewares {
            d.file_idx += base;
        }
        for d in &mut incoming.consts {
            d.file_idx += base;
        }
    }
    combined.sources.extend(incoming.sources);
    combined.dbcontexts.extend(incoming.dbcontexts);
    combined.models.extend(incoming.models);
    combined.routes.extend(incoming.routes);
    combined.functions.extend(incoming.functions);
    combined.middlewares.extend(incoming.middlewares);
    combined.imports.extend(incoming.imports);
    combined.mounts.extend(incoming.mounts);
    combined.consts.extend(incoming.consts);
    if let Some(eh) = incoming.error_handler {
        if combined.error_handler.is_some() {
            bail!("only one errorHandler is allowed across all project files");
        }
        combined.error_handler = Some(eh);
    }
    Ok(())
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
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))?
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
    let Ok(content) = std::fs::read_to_string(&env_path) else {
        return;
    };
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
            let url = format!(
                "postgresql://{}:{}@{}:{}/{}",
                user, password, host, port, db
            );
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
        std::env::temp_dir().join(format!(
            "jwc-newproj-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ))
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
        assert!(
            dir.join(".env.example").is_file(),
            ".env.example not created"
        );

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

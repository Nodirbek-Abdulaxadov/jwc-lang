//! Project-template scaffolding for `jwc new --template <kind>`.
//!
//! The template tree lives at `<repo-root>/templates/{api,auth,jobs}/` and
//! is baked into the binary via `include_str!`, so `jwc new` works on a
//! user box that doesn't have the source repo checked out.
//!
//! ## File-path placeholder convention
//!
//! Both file *contents* and file *paths* run through a tiny
//! `{{name}} → <project-name>` substitution. Filenames can't literally
//! contain `{{name}}` on every OS (Windows treats `{}` oddly in some shells
//! and embedded build pipelines), so the path side uses a distinct sentinel:
//! `__name__`. Anywhere it appears in a relative path it's replaced with the
//! project name.
//!
//! Example: the on-disk file `templates/api/__name__.jwcproj` lands at
//! `<project>/<project-name>.jwcproj`.
//!
//! ## Adding a template
//!
//! 1. Drop the tree under `templates/<kind>/`.
//! 2. Add a `TemplateKind::Kind` enum variant and a `match` arm in
//!    [`template_files`] that returns the embedded `(path, contents)` pairs.
//!    Each entry is one `include_str!` call referencing the on-disk file.
//! 3. Update the `--template` value-enum in `src/main.rs` so the CLI
//!    accepts the new kind.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// Which starter scaffold the user asked for. `Empty` preserves the
/// original `jwc new <name>` behaviour (a minimal `main.jwc` + manifest),
/// the other variants materialise a full multi-file project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    Empty,
    Api,
    Auth,
    Jobs,
}

impl TemplateKind {
    /// Human-readable name for log messages and the CLI value-enum.
    pub fn as_str(self) -> &'static str {
        match self {
            TemplateKind::Empty => "empty",
            TemplateKind::Api => "api",
            TemplateKind::Auth => "auth",
            TemplateKind::Jobs => "jobs",
        }
    }
}

/// One file in a template tree: the relative path under the new project
/// root, and the file's raw text. Both go through `{{name}}` / `__name__`
/// substitution before being written.
struct TemplateFile {
    /// Relative path (forward slashes) from the project root. May contain
    /// the `__name__` sentinel — see the module docs.
    path: &'static str,
    /// File contents. May contain `{{name}}` placeholders.
    contents: &'static str,
}

/// Embedded copy of every template tree. Each `include_str!` baked at
/// compile time means the binary works without the source repo on disk.
fn template_files(kind: TemplateKind) -> &'static [TemplateFile] {
    match kind {
        TemplateKind::Empty => &[],
        TemplateKind::Api => API_FILES,
        TemplateKind::Auth => AUTH_FILES,
        TemplateKind::Jobs => JOBS_FILES,
    }
}

const API_FILES: &[TemplateFile] = &[
    TemplateFile {
        path: "__name__.jwcproj",
        contents: include_str!("../templates/api/__name__.jwcproj"),
    },
    TemplateFile {
        path: "main.jwc",
        contents: include_str!("../templates/api/main.jwc"),
    },
    TemplateFile {
        path: "src/AppDbContext.jwc",
        contents: include_str!("../templates/api/src/AppDbContext.jwc"),
    },
    TemplateFile {
        path: "src/Item.jwc",
        contents: include_str!("../templates/api/src/Item.jwc"),
    },
    TemplateFile {
        path: "migrations/0000000001_init.up.sql",
        contents: include_str!("../templates/api/migrations/0000000001_init.up.sql"),
    },
    TemplateFile {
        path: "migrations/0000000001_init.down.sql",
        contents: include_str!("../templates/api/migrations/0000000001_init.down.sql"),
    },
    TemplateFile {
        path: ".env.example",
        contents: include_str!("../templates/api/.env.example"),
    },
    TemplateFile {
        path: ".gitignore",
        contents: include_str!("../templates/api/.gitignore"),
    },
    TemplateFile {
        path: "README.md",
        contents: include_str!("../templates/api/README.md"),
    },
];

const AUTH_FILES: &[TemplateFile] = &[
    TemplateFile {
        path: "__name__.jwcproj",
        contents: include_str!("../templates/auth/__name__.jwcproj"),
    },
    TemplateFile {
        path: "main.jwc",
        contents: include_str!("../templates/auth/main.jwc"),
    },
    TemplateFile {
        path: "src/AppDbContext.jwc",
        contents: include_str!("../templates/auth/src/AppDbContext.jwc"),
    },
    TemplateFile {
        path: "src/User.jwc",
        contents: include_str!("../templates/auth/src/User.jwc"),
    },
    TemplateFile {
        path: "src/AuthMiddleware.jwc",
        contents: include_str!("../templates/auth/src/AuthMiddleware.jwc"),
    },
    TemplateFile {
        path: "migrations/0000000001_init.up.sql",
        contents: include_str!("../templates/auth/migrations/0000000001_init.up.sql"),
    },
    TemplateFile {
        path: "migrations/0000000001_init.down.sql",
        contents: include_str!("../templates/auth/migrations/0000000001_init.down.sql"),
    },
    TemplateFile {
        path: ".env.example",
        contents: include_str!("../templates/auth/.env.example"),
    },
    TemplateFile {
        path: ".gitignore",
        contents: include_str!("../templates/auth/.gitignore"),
    },
    TemplateFile {
        path: "README.md",
        contents: include_str!("../templates/auth/README.md"),
    },
];

const JOBS_FILES: &[TemplateFile] = &[
    TemplateFile {
        path: "__name__.jwcproj",
        contents: include_str!("../templates/jobs/__name__.jwcproj"),
    },
    TemplateFile {
        path: "main.jwc",
        contents: include_str!("../templates/jobs/main.jwc"),
    },
    TemplateFile {
        path: "src/EmailJob.jwc",
        contents: include_str!("../templates/jobs/src/EmailJob.jwc"),
    },
    TemplateFile {
        path: ".env.example",
        contents: include_str!("../templates/jobs/.env.example"),
    },
    TemplateFile {
        path: ".gitignore",
        contents: include_str!("../templates/jobs/.gitignore"),
    },
    TemplateFile {
        path: "README.md",
        contents: include_str!("../templates/jobs/README.md"),
    },
];

/// Materialise the `kind` template into `root`. The directory must not
/// exist or must be empty; we won't clobber an in-flight project.
///
/// `name` becomes both the manifest `name` field (via `{{name}}`
/// substitution in contents) and the filename of the `.jwcproj`
/// (via `__name__` substitution in paths).
///
/// Note: `TemplateKind::Empty` is rejected here — the caller should route
/// to [`crate::project::create_new_project`] for that branch, which has
/// the legacy quick-scaffold behaviour we want to preserve byte-for-byte.
pub fn create_from_template(name: &str, kind: TemplateKind, root: &Path) -> Result<()> {
    if matches!(kind, TemplateKind::Empty) {
        bail!("create_from_template should not be called for TemplateKind::Empty");
    }
    if name.is_empty() {
        bail!("project name is empty");
    }

    prepare_root(root)?;

    let files = template_files(kind);
    if files.is_empty() {
        bail!("template '{}' has no files registered", kind.as_str());
    }

    for tf in files {
        let rel = substitute_path(tf.path, name);
        let target = root.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let contents = tf.contents.replace("{{name}}", name);
        std::fs::write(&target, contents)
            .with_context(|| format!("Failed to write {}", target.display()))?;
    }

    Ok(())
}

/// Ensure `root` exists, is a directory, and is empty (matches the
/// pre-existing `create_new_project` contract). Creates it if missing.
fn prepare_root(root: &Path) -> Result<()> {
    if root.exists() {
        if !root.is_dir() {
            bail!("Target path is not a directory: {}", root.display());
        }
        if root
            .read_dir()
            .with_context(|| format!("Failed to read {}", root.display()))?
            .next()
            .is_some()
        {
            bail!("Target directory is not empty: {}", root.display());
        }
        Ok(())
    } else {
        std::fs::create_dir_all(root)
            .with_context(|| format!("Failed to create {}", root.display()))?;
        Ok(())
    }
}

/// Replace the `__name__` sentinel in a relative path with the project
/// name. The replacement is whole-segment-agnostic — we just do a string
/// swap, which is enough for the placeholders we use today.
fn substitute_path(path: &str, name: &str) -> PathBuf {
    let swapped = path.replace("__name__", name);
    // Normalise to native separators so Windows doesn't end up with mixed
    // `/` and `\` in error messages further down the line.
    PathBuf::from(swapped.replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// Parse a CLI-friendly template name into a [`TemplateKind`]. The match
/// list mirrors the `clap::ValueEnum` derive on `main.rs`'s
/// `TemplateKindArg`, so they stay in sync at the only place a user ever
/// types a template name.
pub fn parse_template_kind(s: &str) -> Result<TemplateKind> {
    match s.to_ascii_lowercase().as_str() {
        "empty" => Ok(TemplateKind::Empty),
        "api" => Ok(TemplateKind::Api),
        "auth" => Ok(TemplateKind::Auth),
        "jobs" => Ok(TemplateKind::Jobs),
        other => Err(anyhow!(
            "Unknown template '{}', expected one of: empty, api, auth, jobs",
            other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_tmp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "jwc-tpl-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ))
    }

    #[test]
    fn create_from_template_substitutes_name() {
        let dir = unique_tmp_dir("api-name");
        create_from_template("myapp", TemplateKind::Api, &dir).expect("materialise api");

        // Manifest filename uses the project name.
        let proj = dir.join("myapp.jwcproj");
        assert!(proj.is_file(), "expected myapp.jwcproj at {}", proj.display());

        // Contents-side substitution: the JSON should carry "name": "myapp".
        let manifest = std::fs::read_to_string(&proj).unwrap();
        assert!(
            manifest.contains("\"name\": \"myapp\""),
            "manifest didn't substitute name: {}",
            manifest
        );
        // The placeholder must not survive the swap anywhere in the tree.
        let env_ex = std::fs::read_to_string(dir.join(".env.example")).unwrap();
        assert!(!env_ex.contains("{{name}}"), "{{name}} leaked into .env.example");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_from_template_api_passes_jwc_check() {
        let dir = unique_tmp_dir("api-check");
        create_from_template("checkme", TemplateKind::Api, &dir).expect("materialise api");

        // Load the materialised project the same way `jwc test` does. This
        // walks the source files, merges them, and runs the validator —
        // any syntax problem in a template file blows up here.
        let loaded = crate::project::load_project_from_root(&dir).expect("load project");
        assert!(
            loaded.source_files.len() >= 2,
            "expected at least main.jwc + src/*.jwc, got {}",
            loaded.source_files.len()
        );
        assert!(!loaded.program.routes.is_empty(), "no routes parsed");
        assert!(!loaded.program.models.is_empty(), "no models parsed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_from_template_auth_passes_jwc_check() {
        let dir = unique_tmp_dir("auth-check");
        create_from_template("authcheck", TemplateKind::Auth, &dir).expect("materialise auth");
        let loaded = crate::project::load_project_from_root(&dir).expect("load project");
        // The auth template defines an `Auth` middleware — verify it.
        assert!(
            loaded
                .program
                .middlewares
                .iter()
                .any(|m| m.name.eq_ignore_ascii_case("Auth")),
            "Auth middleware not found"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_from_template_jobs_passes_jwc_check() {
        let dir = unique_tmp_dir("jobs-check");
        create_from_template("jobcheck", TemplateKind::Jobs, &dir).expect("materialise jobs");
        let loaded = crate::project::load_project_from_root(&dir).expect("load project");
        assert!(
            loaded
                .program
                .functions
                .iter()
                .any(|f| f.name == "process_email"),
            "process_email handler not found"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_kind_empty_falls_back_to_existing_behaviour() {
        // Regression guard: calling `create_from_template(Empty, ...)` must
        // refuse — the caller has to route Empty back to the legacy
        // `project::create_new_project` path, which we don't want to
        // accidentally rewrite in terms of templates.
        let dir = unique_tmp_dir("empty");
        let err = create_from_template("does-not-matter", TemplateKind::Empty, &dir)
            .expect_err("Empty should be rejected here");
        let msg = format!("{err}");
        assert!(
            msg.contains("Empty"),
            "expected error to mention Empty, got: {msg}"
        );

        // And confirm the legacy path still works untouched: empty target,
        // expected files appear.
        let legacy_dir = unique_tmp_dir("legacy");
        crate::project::create_new_project(&legacy_dir).expect("legacy create");
        let basename = legacy_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_string();
        assert!(legacy_dir.join(format!("{}.jwcproj", basename)).is_file());
        assert!(legacy_dir.join("main.jwc").is_file());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
    }

    #[test]
    fn parse_template_kind_accepts_known_values() {
        assert_eq!(parse_template_kind("empty").unwrap(), TemplateKind::Empty);
        assert_eq!(parse_template_kind("api").unwrap(), TemplateKind::Api);
        assert_eq!(parse_template_kind("AUTH").unwrap(), TemplateKind::Auth);
        assert_eq!(parse_template_kind("jobs").unwrap(), TemplateKind::Jobs);
        assert!(parse_template_kind("unknown").is_err());
    }
}

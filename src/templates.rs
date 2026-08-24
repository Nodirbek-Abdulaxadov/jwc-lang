//! `jwc new [--template <kind>]` — project scaffolding.
//!
//! The template trees live under `<repo>/templates/{empty,api,auth}/` and
//! are baked into the binary with `include_str!`, so `jwc new` works on a
//! machine that has no checkout of this repository.
//!
//! ## What "restored" means here
//!
//! The 0.9 trees survived the cutover on disk but the module that read
//! them did not, so `jwc new` was gone while the files sat unreferenced —
//! written in a grammar the compiler had stopped accepting. Reinstating
//! the command meant rewriting every template in the 1.0 vocabulary; the
//! old contents could not be reused, only the shape of this module.
//!
//! ## Placeholders
//!
//! `{{name}}` in a file's **contents** becomes the project name.
//! `__name__` in a **path** does the same, because a literal `{{name}}` in
//! a filename is awkward on Windows shells. Nothing in the 1.0 trees uses
//! the path form yet — the manifest is `jwcproj.json` under every project
//! — but the substitution stays because a template that wants it should
//! not have to reintroduce it.
//!
//! ## Adding a template
//!
//! 1. Put the tree under `templates/<kind>/`.
//! 2. Add a [`TemplateKind`] variant, its `as_str`, and a `match` arm in
//!    [`template_files`] with one `include_str!` per file.
//! 3. Extend the `--template` value-enum in `src/main.rs`.
//! 4. `tests/templates.rs` will then scaffold it and run the real checker
//!    over it, so a template that does not compile fails the build.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// Which starter tree to lay down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// One route, one schema, no tables — the smallest thing that runs.
    Empty,
    /// CRUD over one table: DTOs, a service, five routes, keyset paging.
    Api,
    /// `Empty` plus accounts, Argon2id passwords and JWT sessions.
    Auth,
    /// A background `job`, its dispatch site, and the durable queue.
    Jobs,
}

impl TemplateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TemplateKind::Empty => "empty",
            TemplateKind::Api => "api",
            TemplateKind::Auth => "auth",
            TemplateKind::Jobs => "jobs",
        }
    }

    /// Every kind, for the CLI's error message and the test sweep.
    pub const ALL: &'static [TemplateKind] = &[
        TemplateKind::Empty,
        TemplateKind::Api,
        TemplateKind::Auth,
        TemplateKind::Jobs,
    ];
}

/// One file in a template tree.
struct TemplateFile {
    /// Relative path from the project root, forward slashes. May contain
    /// `__name__`.
    path: &'static str,
    /// Raw text. May contain `{{name}}`.
    contents: &'static str,
}

macro_rules! tfile {
    ($kind:literal, $path:literal) => {
        TemplateFile {
            path: $path,
            contents: include_str!(concat!("../templates/", $kind, "/", $path)),
        }
    };
}

fn template_files(kind: TemplateKind) -> &'static [TemplateFile] {
    match kind {
        TemplateKind::Empty => EMPTY_FILES,
        TemplateKind::Api => API_FILES,
        TemplateKind::Auth => AUTH_FILES,
        TemplateKind::Jobs => JOBS_FILES,
    }
}

const EMPTY_FILES: &[TemplateFile] = &[
    tfile!("empty", "jwcproj.json"),
    tfile!("empty", "src/app.jwc"),
    tfile!("empty", ".env.example"),
    tfile!("empty", ".gitignore"),
    tfile!("empty", "README.md"),
];

const API_FILES: &[TemplateFile] = &[
    tfile!("api", "jwcproj.json"),
    tfile!("api", "src/app.jwc"),
    tfile!("api", "src/db/notes.jwc"),
    tfile!("api", "src/dto/notes.jwc"),
    tfile!("api", "src/services/notes.jwc"),
    tfile!("api", "src/routes/notes.jwc"),
    tfile!("api", ".env.example"),
    tfile!("api", ".gitignore"),
    tfile!("api", "README.md"),
];

const AUTH_FILES: &[TemplateFile] = &[
    tfile!("auth", "jwcproj.json"),
    tfile!("auth", "src/app.jwc"),
    tfile!("auth", "src/db/auth.jwc"),
    tfile!("auth", "src/dto/auth.jwc"),
    tfile!("auth", "src/middleware/auth.jwc"),
    tfile!("auth", "src/services/auth.jwc"),
    tfile!("auth", "src/routes/auth.jwc"),
    tfile!("auth", ".env.example"),
    tfile!("auth", ".gitignore"),
    tfile!("auth", "README.md"),
];

const JOBS_FILES: &[TemplateFile] = &[
    tfile!("jobs", "jwcproj.json"),
    tfile!("jobs", "src/app.jwc"),
    tfile!("jobs", "src/db/work.jwc"),
    tfile!("jobs", "src/dto/work.jwc"),
    tfile!("jobs", "src/jobs/deliver.jwc"),
    tfile!("jobs", "src/routes/work.jwc"),
    tfile!("jobs", ".env.example"),
    tfile!("jobs", ".gitignore"),
    tfile!("jobs", "README.md"),
];

/// Write `kind`'s tree into `root`, substituting `name`.
pub fn create(name: &str, kind: TemplateKind, root: &Path) -> Result<()> {
    if name.trim().is_empty() {
        bail!("project name is empty");
    }
    // The name lands in `jwcproj.json`, which `import` resolves against,
    // and in a `DATABASE_URL`. Checking it here means the scaffold cannot
    // produce a project that `jwc check` immediately rejects.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!(
            "`{name}` is not a usable project name: letters, digits, `_` and `-` only \
             (it becomes the manifest's `name`)"
        );
    }

    prepare_root(root)?;

    for tf in template_files(kind) {
        let target = root.join(substitute_path(tf.path, name));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        std::fs::write(&target, tf.contents.replace("{{name}}", name))
            .with_context(|| format!("could not write {}", target.display()))?;
    }
    Ok(())
}

/// `root` must be missing or empty. Scaffolding over an existing project
/// would overwrite files the user wrote.
fn prepare_root(root: &Path) -> Result<()> {
    if !root.exists() {
        return std::fs::create_dir_all(root)
            .with_context(|| format!("could not create {}", root.display()));
    }
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    if root
        .read_dir()
        .with_context(|| format!("could not read {}", root.display()))?
        .next()
        .is_some()
    {
        bail!("{} is not empty", root.display());
    }
    Ok(())
}

fn substitute_path(path: &str, name: &str) -> PathBuf {
    PathBuf::from(
        path.replace("__name__", name)
            .replace('/', std::path::MAIN_SEPARATOR_STR),
    )
}

pub fn parse_kind(s: &str) -> Result<TemplateKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "empty" => Ok(TemplateKind::Empty),
        "api" => Ok(TemplateKind::Api),
        "auth" => Ok(TemplateKind::Auth),
        "jobs" => Ok(TemplateKind::Jobs),
        other => Err(anyhow!(
            "unknown template `{other}`. One of: {}",
            TemplateKind::ALL
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// `jwc new <name> [--template <kind>] [--path <dir>]`.
pub fn new_project(name: String, kind: TemplateKind, path: Option<PathBuf>) -> Result<()> {
    let root = path.unwrap_or_else(|| PathBuf::from(&name));
    create(&name, kind, &root)?;
    println!(
        "created `{name}` from the `{}` template in {}",
        kind.as_str(),
        root.display()
    );
    println!();
    println!("  cd {}", root.display());
    println!("  cp .env.example .env      # point DATABASE_URL at a database");
    println!("  jwc check                 # offline: types, schema, routes");
    if kind != TemplateKind::Empty {
        println!("  jwc migrate new init && jwc migrate up");
    }
    println!("  jwc serve");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_manifest_and_an_entry_point() {
        for kind in TemplateKind::ALL {
            let files = template_files(*kind);
            assert!(
                files.iter().any(|f| f.path == "jwcproj.json"),
                "{} has no manifest",
                kind.as_str()
            );
            assert!(
                files.iter().any(|f| f.path == "src/app.jwc"),
                "{} has no src/app.jwc",
                kind.as_str()
            );
        }
    }

    #[test]
    fn parse_kind_round_trips_and_names_the_alternatives() {
        for kind in TemplateKind::ALL {
            assert_eq!(
                parse_kind(kind.as_str()).map(|k| k.as_str()).unwrap_or(""),
                kind.as_str()
            );
        }
        let Err(e) = parse_kind("nosuch") else {
            panic!("`nosuch` is not a template");
        };
        assert!(e.to_string().contains("empty, api, auth, jobs"), "{e}");
    }

    #[test]
    fn a_name_that_would_not_survive_the_manifest_is_refused() {
        let dir = std::env::temp_dir().join("jwc-new-badname");
        let _ = std::fs::remove_dir_all(&dir);
        for bad in ["", "  ", "my project", "../escape", "naïve"] {
            assert!(
                create(bad, TemplateKind::Empty, &dir).is_err(),
                "`{bad}` should be refused"
            );
        }
    }

    #[test]
    fn scaffolding_refuses_a_non_empty_directory() {
        let dir = std::env::temp_dir().join("jwc-new-nonempty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("keep.txt"), "mine").expect("write");

        let Err(e) = create("demo", TemplateKind::Empty, &dir) else {
            panic!("scaffolding over a non-empty directory would overwrite files");
        };
        assert!(e.to_string().contains("is not empty"), "{e}");
        assert_eq!(
            std::fs::read_to_string(dir.join("keep.txt")).unwrap_or_default(),
            "mine"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_name_reaches_the_manifest() {
        let dir = std::env::temp_dir().join("jwc-new-subst");
        let _ = std::fs::remove_dir_all(&dir);
        create("shop", TemplateKind::Api, &dir).expect("scaffold");

        let manifest = std::fs::read_to_string(dir.join("jwcproj.json")).expect("manifest");
        assert!(manifest.contains("\"name\": \"shop\""), "{manifest}");
        assert!(
            !manifest.contains("{{name}}"),
            "a placeholder survived: {manifest}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

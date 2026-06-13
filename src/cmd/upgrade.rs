//! `jwc upgrade` — codemod scaffold for deprecation migrations.
//!
//! The rule registry is INTENTIONALLY empty at v0.4.7 because nothing has
//! been removed yet (every documented surface is still supported). The
//! CLI lands now so we can ship rules in v0.5 / v0.6 without breaking the
//! command shape — `DEPRECATION.md` already commits to flipping
//! `--no-typecheck` from "supported escape hatch" to "deprecated → removed"
//! by v0.6.0, and that flip will be the first rule.
//!
//! Rule shape:
//!
//! ```ignore
//! pub trait UpgradeRule {
//!     fn id(&self) -> &'static str;        // e.g. "no-typecheck-removed"
//!     fn description(&self) -> &'static str;
//!     fn apply_to_file(&self, path: &Path, src: &str) -> Option<String>;
//! }
//! ```
//!
//! `apply_to_file` returns `Some(new_text)` when the rule rewrote the
//! file or `None` when nothing applied. The CLI walks every `.jwc` source
//! under the target paths (defaulting to the current project root),
//! threads each through every registered rule in order, and either
//! reports the diff (`--dry-run`) or writes the result back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// One codemod transformation. Stateless; the registry holds boxed
/// instances behind `&'static dyn UpgradeRule`.
pub trait UpgradeRule: Send + Sync {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn apply_to_file(&self, path: &Path, src: &str) -> Option<String>;
}

/// The global rule registry. Empty at v0.4.7; each rule listed here will
/// run in order against every input file.
pub fn rules() -> Vec<Box<dyn UpgradeRule>> {
    Vec::new()
}

/// `jwc upgrade [paths...] [--dry-run]` entry point. Walks the input
/// paths, finds every `.jwc` file (recursively), threads each through the
/// rule registry, and writes back changes (or prints a diff under
/// `--dry-run`).
///
/// `paths` empty → defaults to the current project root (walks upward
/// looking for `*.jwcproj`).
pub fn upgrade(paths: Vec<PathBuf>, dry_run: bool) -> Result<()> {
    let rules = rules();
    if rules.is_empty() {
        println!("jwc upgrade: no rules registered at this JWC version.");
        println!("Nothing to migrate. See DEPRECATION.md for the upcoming rule schedule.");
        return Ok(());
    }

    let inputs = if paths.is_empty() {
        let cwd = std::env::current_dir()?;
        let root = crate::project::find_project_root(&cwd)
            .with_context(|| "jwc upgrade: no jwcproj found; pass explicit paths")?;
        vec![root]
    } else {
        paths
    };

    let mut total_files = 0usize;
    let mut rewritten = 0usize;
    for input in inputs {
        for path in walk_jwc_files(&input)? {
            total_files += 1;
            let src = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let mut current = src.clone();
            let mut any_rule_applied = false;
            for rule in &rules {
                if let Some(updated) = rule.apply_to_file(&path, &current) {
                    if updated != current {
                        any_rule_applied = true;
                        println!("  [{}] {}", rule.id(), path.display(),);
                        current = updated;
                    }
                }
            }
            if any_rule_applied {
                rewritten += 1;
                if dry_run {
                    println!("    (dry-run: not written)");
                } else {
                    std::fs::write(&path, current)
                        .with_context(|| format!("write {}", path.display()))?;
                }
            }
        }
    }

    if dry_run {
        println!("jwc upgrade --dry-run: {rewritten}/{total_files} file(s) would change.",);
    } else {
        println!("jwc upgrade: {rewritten}/{total_files} file(s) rewritten.",);
    }
    Ok(())
}

fn walk_jwc_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            // Skip build / dependency caches.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(
                name,
                ".jwc-build" | "target" | "node_modules" | ".git" | "bin" | "obj"
            ) {
                continue;
            }
            for entry in
                std::fs::read_dir(&path).with_context(|| format!("read_dir {}", path.display()))?
            {
                let entry = entry?;
                stack.push(entry.path());
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("jwc") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is empty at v0.4.7. Smoke-test: `rules()` returns a
    /// Vec, and the trait object size is sensible. This is a placeholder
    /// guarding against accidentally removing the `UpgradeRule` trait
    /// before any rule lands.
    #[test]
    fn rules_registry_is_empty_at_v047() {
        let rs = rules();
        assert!(
            rs.is_empty(),
            "rule registry should still be empty at v0.4.7; \
             update this test when adding the first rule",
        );
    }

    #[test]
    fn walk_jwc_files_filters_to_jwc_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.jwc"), "function main() {}").unwrap();
        std::fs::write(root.join("readme.md"), "hello").unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("Item.jwc"), "entity Item {}").unwrap();
        // .jwc-build should be skipped wholesale.
        std::fs::create_dir(root.join(".jwc-build")).unwrap();
        std::fs::write(root.join(".jwc-build").join("generated.jwc"), "x").unwrap();

        let found = walk_jwc_files(root).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"main.jwc".to_string()));
        assert!(names.contains(&"Item.jwc".to_string()));
        assert!(
            !names.contains(&"generated.jwc".to_string()),
            ".jwc-build/ should be skipped",
        );
        assert!(!names.contains(&"readme.md".to_string()));
    }
}

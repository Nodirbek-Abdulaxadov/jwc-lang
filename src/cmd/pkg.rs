//! `jwc add / install / update / remove / tree` command implementations.
//!
//! Every command mutates the project manifest (when needed), re-runs the
//! resolver, and writes back the lockfile. The resolver's side effect of
//! materialising sources under `~/.jwc/registry/` keeps the cache populated
//! as a free bonus.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::project::{DepSpec, DetailedDep, JwcProject, PROJECT_FILE};
use crate::resolver;

/// Add (or replace) a dependency in the manifest and re-resolve.
///
/// Exactly one source flag is honoured. If multiple are passed, `path`
/// wins, then `git`, then version (registry). This matches Cargo's
/// precedence and avoids the user having to think about exclusivity.
pub fn add(
    project_root: &Path,
    name: &str,
    version: Option<&str>,
    path: Option<&Path>,
    git: Option<&str>,
    rev: Option<&str>,
) -> Result<()> {
    let mut manifest = read_manifest(project_root)?;

    let spec = if let Some(p) = path {
        DepSpec::Detailed(DetailedDep {
            path: Some(p.to_path_buf()),
            ..Default::default()
        })
    } else if let Some(url) = git {
        DepSpec::Detailed(DetailedDep {
            git: Some(url.to_string()),
            rev: rev.map(|s| s.to_string()),
            version: version.map(|s| s.to_string()),
            ..Default::default()
        })
    } else if let Some(v) = version {
        DepSpec::Version(v.to_string())
    } else {
        bail!(
            "`jwc add {}` needs at least one source: --path, --git, or a version requirement",
            name
        );
    };

    manifest.dependencies.insert(name.to_string(), spec);
    write_manifest(project_root, &manifest)?;

    let _graph = resolver::ensure_resolved(&manifest, project_root)?;
    println!("Added '{}' and refreshed jwcproj.lock", name);
    Ok(())
}

/// Re-fetch all locked deps without changing the manifest. Useful after a
/// fresh clone.
pub fn install(project_root: &Path) -> Result<()> {
    let manifest = read_manifest(project_root)?;
    let graph = resolver::ensure_resolved(&manifest, project_root)?;
    println!("Installed {} package(s)", graph.packages.len());
    Ok(())
}

/// Internal silent version used by the project loader: install on demand
/// when the lockfile exists but cache is missing. Errors are surfaced; this
/// is only "silent" in the sense of not printing on success.
pub fn install_silent(project_root: &Path) -> Result<()> {
    let manifest = read_manifest(project_root)?;
    let _ = resolver::ensure_resolved(&manifest, project_root)?;
    Ok(())
}

/// Update one package (or all if `name` is None) within its semver range.
pub fn update(project_root: &Path, name: Option<&str>) -> Result<()> {
    let manifest = read_manifest(project_root)?;

    // To force re-resolution of a specific package, we briefly drop its
    // entry from the existing lockfile and re-run. Other packages stay
    // pinned at their current versions.
    if let Some(pkg) = name {
        if let Some(mut lock) = crate::lockfile::JwcLockfile::load(project_root)? {
            lock.package.retain(|p| p.name != pkg);
            lock.save(project_root)?;
        }
        println!("Updating '{}'...", pkg);
    } else {
        // Update everything: wipe the lockfile.
        crate::lockfile::JwcLockfile::new().save(project_root)?;
        println!("Updating all dependencies...");
    }

    let graph = resolver::ensure_resolved(&manifest, project_root)?;
    println!("Resolved {} package(s)", graph.packages.len());
    Ok(())
}

/// Remove a dependency from the manifest and re-resolve. Transitive deps
/// no longer required will be dropped from the lockfile.
pub fn remove(project_root: &Path, name: &str) -> Result<()> {
    let mut manifest = read_manifest(project_root)?;
    if manifest.dependencies.remove(name).is_none() {
        bail!("'{}' is not a direct dependency in {}", name, PROJECT_FILE);
    }
    write_manifest(project_root, &manifest)?;
    // Re-resolve from scratch — the simplest way to drop now-orphaned
    // transitives.
    crate::lockfile::JwcLockfile::new().save(project_root)?;
    let _graph = resolver::ensure_resolved(&manifest, project_root)?;
    println!("Removed '{}'", name);
    Ok(())
}

/// Print the dep tree to stdout.
pub fn tree(project_root: &Path) -> Result<()> {
    let manifest = read_manifest(project_root)?;
    let graph = resolver::ensure_resolved(&manifest, project_root)?;
    println!("{}", graph.render_tree());
    Ok(())
}

// ---------- manifest read/write ----------

fn read_manifest(project_root: &Path) -> Result<JwcProject> {
    let path = manifest_path(project_root)?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let json = crate::project::strip_jsonc_comments(&raw);
    let proj: JwcProject = serde_json::from_str(&json)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(proj)
}

fn write_manifest(project_root: &Path, manifest: &JwcProject) -> Result<()> {
    let path = manifest_path(project_root)?;
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(&path, json + "\n")
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

fn manifest_path(project_root: &Path) -> Result<PathBuf> {
    // Prefer `<name>.jwcproj`; fall back to `jwcproj.json`.
    if let Ok(entries) = std::fs::read_dir(project_root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("jwcproj"))
                .unwrap_or(false)
                && p.is_file()
            {
                return Ok(p);
            }
        }
    }
    let legacy = project_root.join(PROJECT_FILE);
    if legacy.is_file() {
        return Ok(legacy);
    }
    Err(anyhow!(
        "No project manifest found in {} (expected *.jwcproj or {})",
        project_root.display(),
        PROJECT_FILE
    ))
}

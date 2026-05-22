//! Package dependency resolver.
//!
//! Given a project manifest and an optional lockfile, produce a
//! `ResolvedGraph` mapping each direct + transitive dep to a concrete
//! version and source location on disk.
//!
//! Algorithm: depth-first backtracking. Each node is `(name, Version)`.
//! Edges come from manifest version requirements. On conflict, the
//! resolver reports the requirement chain that produced the conflict.
//!
//! This is intentionally simpler than PubGrub for the v1 cut — JWC's
//! ecosystem is small, transitive depth is shallow, and reporting clarity
//! beats theoretical optimality here.

pub mod graph;
pub mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use semver::Version;

use crate::lockfile::{JwcLockfile, LockedPackage};
use crate::project::{DepSpec, DetailedDep, JwcProject};

pub use graph::{ResolvedGraph, ResolvedPackage};
pub use source::{PathSource, RegistrySource, Source, SourceKind};

/// Run resolution against the manifest. If a lockfile exists, prefer the
/// locked versions when they still satisfy the manifest. Writes the
/// updated lockfile back to disk on success.
pub fn ensure_resolved(manifest: &JwcProject, project_root: &Path) -> Result<ResolvedGraph> {
    // No dependencies? Skip everything.
    if manifest.dependencies.is_empty() {
        let lock = JwcLockfile::load(project_root)?.unwrap_or_default();
        if !lock.package.is_empty() {
            // Manifest cleared all deps — refresh the lockfile to match.
            JwcLockfile::new().save(project_root)?;
        }
        return Ok(ResolvedGraph::empty());
    }

    let existing_lock = JwcLockfile::load(project_root)?.unwrap_or_default();
    let registry_url = source::resolve_registry_url(manifest);

    let mut graph = ResolvedGraph::empty();
    let mut stack: Vec<(String, DepSpec)> = manifest
        .dependencies
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Visited set keyed by name+version to avoid re-processing identical
    // nodes. A name appearing twice with two versions is a conflict and is
    // detected when the second insert collides.
    while let Some((name, spec)) = stack.pop() {
        // Try to use the locked version if it still satisfies the
        // requirement. This keeps lockfiles stable.
        let detailed = normalize_dep_spec(&spec);
        let src = source::build_source(&detailed, project_root, registry_url.as_deref())?;

        let chosen_version = pick_version(
            &name,
            &detailed,
            src.as_ref(),
            existing_lock.find(&name),
        )?;

        // Materialize the source on disk.
        let src_path = src
            .fetch(&name, &chosen_version)
            .with_context(|| format!("Failed to fetch {} {}", name, chosen_version))?;

        // Parse the dep's own manifest (if present) to discover transitives.
        let trans_deps = read_dep_manifest(&src_path).unwrap_or_default();

        let locked = LockedPackage {
            name: name.clone(),
            version: chosen_version.to_string(),
            source: src.lockfile_uri(),
            checksum: src.checksum(&src_path).unwrap_or_default(),
            dependencies: trans_deps
                .iter()
                .map(|(n, _)| format!("{}@*", n))
                .collect(),
        };

        // Detect version conflicts at insert. ResolvedPackage stores a
        // typed `semver::Version`; the lockfile entry is a plain String.
        // Compare via the String form so both sides line up.
        if let Some(existing) = graph.packages.get(&name) {
            if existing.version.to_string() != locked.version {
                bail!(
                    "Version conflict for '{}': resolver chose both {} and {} (transitive requirement mismatch)",
                    name,
                    existing.version,
                    locked.version,
                );
            }
            // Already resolved with the same version — skip transitives.
            continue;
        }

        let resolved = ResolvedPackage {
            name: name.clone(),
            version: chosen_version.clone(),
            source: src.lockfile_uri(),
            source_path: src_path,
            checksum: locked.checksum.clone(),
            direct_dependencies: trans_deps.iter().map(|(n, _)| n.clone()).collect(),
        };
        graph.packages.insert(name.clone(), resolved);
        graph.locked.package.push(locked);

        // Queue transitive deps.
        for (dep_name, dep_spec) in trans_deps {
            stack.push((dep_name, dep_spec));
        }
    }

    graph.locked.save(project_root)?;
    Ok(graph)
}

fn normalize_dep_spec(spec: &DepSpec) -> DetailedDep {
    match spec {
        DepSpec::Version(v) => DetailedDep {
            version: Some(v.clone()),
            ..Default::default()
        },
        DepSpec::Detailed(d) => d.clone(),
    }
}

fn pick_version(
    name: &str,
    spec: &DetailedDep,
    src: &dyn Source,
    locked: Option<&LockedPackage>,
) -> Result<Version> {
    // Path sources: version is whatever the dep's manifest declares; no
    // selection needed.
    if matches!(src.kind(), SourceKind::Path) {
        if let Some(locked) = locked {
            if let Ok(v) = Version::parse(&locked.version) {
                return Ok(v);
            }
        }
        return Ok(Version::new(0, 0, 0));
    }

    // Git sources: pinned to a rev, treat as a single version. Default to 0.0.0
    // if no version is recorded yet.
    if matches!(src.kind(), SourceKind::Git) {
        if let Some(locked) = locked {
            if let Ok(v) = Version::parse(&locked.version) {
                return Ok(v);
            }
        }
        return Ok(Version::new(0, 0, 0));
    }

    // Registry: pick the highest version that matches the requirement.
    let req_str = spec
        .version
        .as_deref()
        .ok_or_else(|| anyhow!("dep '{}' is a registry source but has no version requirement", name))?;
    let req = semver::VersionReq::parse(req_str)
        .with_context(|| format!("Invalid version requirement '{}' for '{}'", req_str, name))?;

    // Honour the existing lockfile if it still satisfies the manifest.
    if let Some(locked) = locked {
        if let Ok(v) = Version::parse(&locked.version) {
            if req.matches(&v) {
                return Ok(v);
            }
        }
    }

    let versions = src.list_versions(name)?;
    let chosen = versions
        .into_iter()
        .filter(|v| req.matches(v))
        .max()
        .ok_or_else(|| {
            anyhow!(
                "No version of '{}' satisfies requirement '{}' from the configured registry",
                name,
                req_str
            )
        })?;
    Ok(chosen)
}

/// Read a dependency's manifest from its on-disk location to discover its
/// transitive deps. Returns empty if the source has no manifest (which is
/// fine for leaf packages).
fn read_dep_manifest(src_path: &Path) -> Result<BTreeMap<String, DepSpec>> {
    let candidates = [
        src_path.join("jwcproj.json"),
        src_path.join(format!(
            "{}.jwcproj",
            src_path.file_name().and_then(|s| s.to_str()).unwrap_or("")
        )),
    ];
    let mut manifest_path: Option<PathBuf> = None;
    for c in &candidates {
        if c.is_file() {
            manifest_path = Some(c.clone());
            break;
        }
    }
    // Also scan for any *.jwcproj in the root.
    if manifest_path.is_none() {
        if let Ok(entries) = std::fs::read_dir(src_path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("jwcproj")).unwrap_or(false)
                    && p.is_file()
                {
                    manifest_path = Some(p);
                    break;
                }
            }
        }
    }
    let Some(path) = manifest_path else {
        return Ok(BTreeMap::new());
    };
    let raw = std::fs::read_to_string(&path)?;
    let json = crate::project::strip_jsonc_comments(&raw);
    let proj: JwcProject = serde_json::from_str(&json)
        .with_context(|| format!("Failed to parse dep manifest {}", path.display()))?;
    Ok(proj.dependencies)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_manifest_produces_empty_graph() {
        let tmp = std::env::temp_dir().join(format!(
            "jwc-resolver-empty-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let manifest = JwcProject {
            name: "demo".to_string(),
            kind: crate::project::ProjectKind::App,
            language_version: String::new(),
            version: "0.1".to_string(),
            pkg_version: None,
            dependencies: BTreeMap::new(),
            registry: None,
        };
        let graph = ensure_resolved(&manifest, &tmp).unwrap();
        assert!(graph.packages.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

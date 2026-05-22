//! Resolved dependency graph — the resolver's output.
//!
//! Each `ResolvedPackage` carries a concrete on-disk location that the
//! project loader can read source files from. The graph also holds the
//! companion `JwcLockfile` so callers can persist it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use semver::Version;

use crate::lockfile::JwcLockfile;

#[derive(Debug, Clone)]
pub struct ResolvedGraph {
    /// Resolved packages keyed by name (each name appears at most once).
    pub packages: BTreeMap<String, ResolvedPackage>,
    /// The lockfile representation, ready to be persisted via `JwcLockfile::save`.
    pub locked: JwcLockfile,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: Version,
    /// URI form as recorded in the lockfile.
    pub source: String,
    /// On-disk path containing the package source (e.g. cache or local path).
    pub source_path: PathBuf,
    pub checksum: String,
    pub direct_dependencies: Vec<String>,
}

impl ResolvedGraph {
    pub fn empty() -> Self {
        Self {
            packages: BTreeMap::new(),
            locked: JwcLockfile::default(),
        }
    }

    /// Iterate packages in deterministic order (alphabetical by name).
    pub fn iter(&self) -> impl Iterator<Item = &ResolvedPackage> {
        self.packages.values()
    }

    /// Render the graph as a human-readable tree, used by `jwc tree`.
    pub fn render_tree(&self) -> String {
        let mut out = String::new();
        for pkg in self.iter() {
            out.push_str(&format!("{}@{}\n", pkg.name, pkg.version));
            for dep in &pkg.direct_dependencies {
                out.push_str(&format!("  └── {}\n", dep));
            }
        }
        out
    }
}

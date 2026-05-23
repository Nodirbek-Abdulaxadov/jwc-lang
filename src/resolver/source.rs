//! Package sources — where to fetch a dep from.
//!
//! Three flavours, in increasing order of complexity:
//! - `PathSource`  — a relative path in the project workspace. Cheapest.
//! - `GitSource`   — `git clone` then check out a rev. Shell out to `git`.
//! - `RegistrySource` — HTTP download + sha256 + tar+gzip extract.
//!
//! The resolver only knows about the `Source` trait; the concrete
//! implementations are picked up via `build_source`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use semver::Version;

use crate::pkg_cache;
use crate::project::{DetailedDep, JwcProject};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Path,
    Git,
    Registry,
}

pub trait Source {
    fn kind(&self) -> SourceKind;
    /// List available versions for this package name. Only meaningful for
    /// registry sources; path/git return a single placeholder version.
    fn list_versions(&self, name: &str) -> Result<Vec<Version>>;
    /// Resolve the package source to a local directory on disk. May download,
    /// extract, or git-clone as needed.
    fn fetch(&self, name: &str, version: &Version) -> Result<PathBuf>;
    /// URI form used in the lockfile.
    fn lockfile_uri(&self) -> String;
    /// Compute a sha256 hex over the source contents. Path sources skip this
    /// (path source is resolved fresh each run anyway).
    fn checksum(&self, _path: &Path) -> Result<String> {
        Ok(String::new())
    }
}

/// Resolve the configured registry URL from env > manifest > default.
pub fn resolve_registry_url(manifest: &JwcProject) -> Option<String> {
    if let Ok(env_url) = std::env::var("JWC_REGISTRY_URL") {
        if !env_url.is_empty() {
            return Some(env_url);
        }
    }
    if let Some(reg) = &manifest.registry {
        if let Some(u) = &reg.url {
            if !u.is_empty() {
                return Some(u.clone());
            }
        }
    }
    Some("https://registry-jwc.1kb.uz/".to_string())
}

/// Build a Source for a single dep spec.
pub fn build_source(
    spec: &DetailedDep,
    project_root: &Path,
    default_registry: Option<&str>,
) -> Result<Box<dyn Source>> {
    if let Some(path) = &spec.path {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            project_root.join(path)
        };
        if !abs.is_dir() {
            bail!(
                "Path dependency does not exist or is not a directory: {}",
                abs.display()
            );
        }
        return Ok(Box::new(PathSource { path: abs }));
    }
    if let Some(url) = &spec.git {
        let rev = spec.rev.clone().unwrap_or_else(|| "HEAD".to_string());
        return Ok(Box::new(GitSource {
            url: url.clone(),
            rev,
        }));
    }
    // Registry source.
    let url = spec
        .registry
        .as_deref()
        .or(default_registry)
        .ok_or_else(|| {
            anyhow!(
                "No registry URL configured (set 'registry.url' in jwcproj or JWC_REGISTRY_URL)"
            )
        })?;
    Ok(Box::new(RegistrySource {
        base_url: url.to_string(),
    }))
}

// ---------- PathSource ----------

pub struct PathSource {
    pub path: PathBuf,
}

impl Source for PathSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Path
    }
    fn list_versions(&self, _name: &str) -> Result<Vec<Version>> {
        Ok(vec![Version::new(0, 0, 0)])
    }
    fn fetch(&self, _name: &str, _version: &Version) -> Result<PathBuf> {
        Ok(self.path.clone())
    }
    fn lockfile_uri(&self) -> String {
        format!("path+{}", self.path.display())
    }
}

// ---------- GitSource ----------

pub struct GitSource {
    pub url: String,
    pub rev: String,
}

impl Source for GitSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Git
    }
    fn list_versions(&self, _name: &str) -> Result<Vec<Version>> {
        // Git sources are pinned to a rev; the lockfile records 0.0.0 unless
        // a manifest at that rev declares otherwise.
        Ok(vec![Version::new(0, 0, 0)])
    }
    fn fetch(&self, _name: &str, _version: &Version) -> Result<PathBuf> {
        let target = pkg_cache::git_cache_dir(&self.url, &self.rev);
        if target.is_dir() {
            return Ok(target);
        }
        pkg_cache::ensure_parent(&target)?;

        let status = std::process::Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                &self.url,
                target.to_string_lossy().as_ref(),
            ])
            .status()
            .with_context(|| format!("Failed to run `git clone {}`", self.url))?;
        if !status.success() {
            bail!("git clone of {} failed (exit {})", self.url, status);
        }

        if self.rev != "HEAD" {
            let status = std::process::Command::new("git")
                .args([
                    "-C",
                    target.to_string_lossy().as_ref(),
                    "checkout",
                    &self.rev,
                ])
                .status()
                .with_context(|| {
                    format!("Failed to checkout {} in {}", self.rev, target.display())
                })?;
            if !status.success() {
                bail!("git checkout {} failed (exit {})", self.rev, status);
            }
        }

        Ok(target)
    }
    fn lockfile_uri(&self) -> String {
        format!("git+{}?rev={}", self.url, self.rev)
    }
    fn checksum(&self, _path: &Path) -> Result<String> {
        Ok(self.rev.clone())
    }
}

// ---------- RegistrySource ----------

pub struct RegistrySource {
    pub base_url: String,
}

impl RegistrySource {
    pub fn host(&self) -> String {
        url::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "default".to_string())
    }
}

impl Source for RegistrySource {
    fn kind(&self) -> SourceKind {
        SourceKind::Registry
    }
    fn list_versions(&self, name: &str) -> Result<Vec<Version>> {
        crate::registry::client::list_versions(&self.base_url, name)
    }
    fn fetch(&self, name: &str, version: &Version) -> Result<PathBuf> {
        let host = self.host();
        let target = pkg_cache::pkg_dir(&host, name, &version.to_string());
        if target.is_dir() {
            return Ok(target);
        }
        pkg_cache::ensure_parent(&target)?;
        crate::registry::client::download_and_extract(
            &self.base_url,
            name,
            &version.to_string(),
            &target,
        )?;
        Ok(target)
    }
    fn lockfile_uri(&self) -> String {
        format!("registry+{}", self.base_url)
    }
    fn checksum(&self, path: &Path) -> Result<String> {
        // Hash the entire extracted tree for a stable lockfile checksum.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hash_dir(path, &mut hasher)?;
        let result = hasher.finalize();
        Ok(format!("sha256:{}", hex_encode(&result)))
    }
}

fn hash_dir(dir: &Path, hasher: &mut sha2::Sha256) -> Result<()> {
    use sha2::Digest;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            hash_dir(&path, hasher)?;
        } else {
            let bytes = std::fs::read(&path)?;
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(&bytes);
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

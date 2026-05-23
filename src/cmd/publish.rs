//! `jwc login` + `jwc publish` — registry credential storage and upload.
//!
//! Storage: `~/.jwc/credentials.json` (on Windows: `%USERPROFILE%\.jwc\credentials.json`).
//! Shape:
//!     {
//!       "registry": "https://registry-jwc.1kb.uz/",
//!       "token":    "jwc_<48-hex>"
//!     }
//!
//! `jwc login --token jwc_...` writes that file. `jwc publish` reads
//! it, packs the current project's directory into a `tar.gz`, and
//! POSTs to `<registry>/api/v1/pkg/<name>/<version>` with an
//! `Authorization: Bearer <token>` header.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use crate::project;

/// On-disk credential blob. `registry` is kept here so a power user
/// can talk to a self-hosted registry without setting `JWC_REGISTRY_URL`
/// for every command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub registry: String,
    pub token: String,
}

/// `jwc login --token <jwc_...>` — write `~/.jwc/credentials.json`.
/// Optionally override the registry URL; default = `registry-jwc.1kb.uz`.
pub fn login(token: &str, registry: Option<&str>) -> Result<()> {
    if !token.starts_with("jwc_") {
        bail!(
            "expected an API key starting with `jwc_` — \
             generate one at https://registry-jwc.1kb.uz/#/keys"
        );
    }
    let reg = registry
        .map(|s| s.to_string())
        .unwrap_or_else(default_registry);
    let cfg_path = credentials_path()?;
    let parent = cfg_path
        .parent()
        .ok_or_else(|| anyhow!("credentials path has no parent dir"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let creds = Credentials {
        registry: reg.clone(),
        token: token.to_string(),
    };
    let body = serde_json::to_string_pretty(&creds)? + "\n";
    std::fs::write(&cfg_path, body).with_context(|| format!("write {}", cfg_path.display()))?;
    // Best-effort chmod 600 on Unix; Windows ACLs already default to user-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600));
    }
    println!("Saved token for {reg}");
    println!("Credentials: {}", cfg_path.display());
    Ok(())
}

/// `jwc publish` — packs the current project + POSTs to the registry.
/// Reads the credential file; bails clearly if missing.
pub async fn publish() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = project::find_project_root(&cwd)
        .with_context(|| "jwc publish must run inside a JWC project")?;
    let loaded = project::load_project_from_root(&root)?;
    let name = loaded.manifest.name.clone();
    // `pkgVersion` is what other projects depend on; fall back to the
    // free-form `version` field for legacy manifests that never split.
    let version = loaded
        .manifest
        .pkg_version
        .clone()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            let v = &loaded.manifest.version;
            if v.is_empty() {
                None
            } else {
                Some(v.clone())
            }
        })
        .ok_or_else(|| {
            anyhow!(
                "manifest is missing a publish version — add \
                 `\"pkgVersion\": \"0.1.0\"` to {}.jwcproj",
                name
            )
        })?;

    if loaded.manifest.kind != project::ProjectKind::Pkg {
        bail!(
            "this manifest has type=\"app\" — only `type: \"pkg\"` projects \
             are publishable. Edit {}.jwcproj.",
            name
        );
    }

    let creds = load_credentials()?;

    // Pack into an in-memory tar.gz so we can both POST and report the size
    // without writing a temp file.
    let tarball = pack_directory(&root)?;
    println!(
        "Packing  {name}@{version} ({:.1} KB)",
        tarball.len() as f64 / 1024.0
    );

    let url = format!(
        "{}/api/v1/pkg/{}/{}",
        creds.registry.trim_end_matches('/'),
        urlencode(&name),
        urlencode(&version),
    );

    let part = reqwest::multipart::Part::bytes(tarball)
        .file_name(format!("{name}-{version}.tar.gz"))
        .mime_str("application/x-gzip")?;
    let form = reqwest::multipart::Form::new().part("file", part);

    println!("Uploading → {url}");
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&creds.token)
        .multipart(form)
        .send()
        .await
        .with_context(|| "registry upload failed (network)")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("registry returned {status}: {body}");
    }
    println!("OK     {body}");
    Ok(())
}

/// Pack every file under `root` (skipping build / IDE caches) into a
/// `tar.gz` byte vector. Paths in the archive are relative to `root` so
/// the registry can extract straight into a target directory.
fn pack_directory(root: &Path) -> Result<Vec<u8>> {
    let buf: Vec<u8> = Vec::new();
    let enc = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(enc);

    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    for path in &files {
        let rel = path
            .strip_prefix(root)
            .map_err(|_| anyhow!("path escapes root: {}", path.display()))?;
        let mut f =
            std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        tar.append_file(rel, &mut f)
            .with_context(|| format!("tar append {}", path.display()))?;
    }

    let enc = tar.into_inner()?;
    let bytes = enc.finish()?;
    Ok(bytes)
}

/// Recursively walk `dir`, ignoring directories that are clearly not
/// part of the source distribution. Mirrors the `.gitignore`/cargo skip
/// list — adding a new pattern only takes one entry here.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(
                name,
                ".git"
                    | ".jwc-build"
                    | "target"
                    | "node_modules"
                    | "bin"
                    | ".vscode"
                    | ".idea"
                    | ".DS_Store"
            ) {
                continue;
            }
            collect_files(root, &p, out)?;
        } else if p.is_file() {
            // Top-level lockfiles shouldn't ship with the source tarball.
            if dir == root && matches!(name, "jwcproj.lock" | ".env" | ".env.local") {
                continue;
            }
            out.push(p);
        }
    }
    Ok(())
}

fn load_credentials() -> Result<Credentials> {
    // Env override wins, then the on-disk file. Both errors point at
    // `jwc login` so the fix is obvious.
    if let (Ok(token), reg) = (
        std::env::var("JWC_REGISTRY_TOKEN"),
        std::env::var("JWC_REGISTRY_URL").ok(),
    ) {
        return Ok(Credentials {
            registry: reg.unwrap_or_else(default_registry),
            token,
        });
    }
    let path = credentials_path()?;
    let body = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no credentials at {} — run `jwc login --token <key>` first",
            path.display()
        )
    })?;
    serde_json::from_str::<Credentials>(&body).with_context(|| format!("parse {}", path.display()))
}

fn credentials_path() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    };
    let home = base.ok_or_else(|| anyhow!("HOME / USERPROFILE not set"))?;
    Ok(home.join(".jwc").join("credentials.json"))
}

fn default_registry() -> String {
    std::env::var("JWC_REGISTRY_URL").unwrap_or_else(|_| "https://registry-jwc.1kb.uz/".to_string())
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

//! `jwc login` / `publish` / `add` — the registry client.
//!
//! The registry is `jwc-registry`, a sister service. Its surface is three
//! endpoints:
//!
//! | Method | Path | Purpose |
//! |---|---|---|
//! | `POST` | `/api/v1/pkg/{name}/{version}` | upload a `.tar.gz`, Bearer auth |
//! | `GET` | `/api/v1/pkg/{name}` | versions, each with its `sha256` |
//! | `GET` | `/api/v1/pkg/{name}/{version}/download` | the bytes |
//!
//! ## The checksum comes from the metadata, not the download
//!
//! The download response carries no checksum header, so a client that
//! verified "the header against the body" would be verifying a value the
//! same response supplied — no integrity at all. `jwc add` therefore reads
//! the `sha256` from `GET /api/v1/pkg/{name}`, which is a **separate**
//! request against the registry's database, and checks the downloaded bytes
//! against it. That is worth having; the header would not be.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_REGISTRY: &str = "https://registry.jwc.1kb.uz";

/// Where a package's sources land. Checked in or not is the project's
/// choice; `jwc add` writes it and nothing else does.
pub const VENDOR_DIR: &str = "jwc_packages";

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Credentials {
    /// `registry url -> token`. Keyed by URL so a private registry and the
    /// public one can both be logged in at once.
    #[serde(default)]
    pub tokens: std::collections::BTreeMap<String, String>,
}

pub fn credentials_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("no HOME")?;
    Ok(PathBuf::from(home).join(".jwc").join("credentials.json"))
}

pub fn load_credentials() -> Credentials {
    credentials_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn registry_url() -> String {
    std::env::var("JWC_REGISTRY").unwrap_or_else(|_| DEFAULT_REGISTRY.to_string())
}

/// `jwc login --token jwc_… [--registry url]`.
pub fn login(token: String, registry: String) -> Result<()> {
    if !token.starts_with("jwc_") {
        bail!("an API key starts with `jwc_` — this looks like something else");
    }
    let mut creds = load_credentials();
    creds.tokens.insert(registry.clone(), token);
    let path = credentials_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&creds)?),
    )?;
    // The file holds a bearer token. 0600 on anything that has modes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("logged in to {registry}");
    Ok(())
}

fn token_for(registry: &str) -> Result<String> {
    load_credentials()
        .tokens
        .get(registry)
        .cloned()
        .with_context(|| format!("not logged in to {registry} — run `jwc login --token jwc_…`"))
}

// ── publish ────────────────────────────────────────────────────────────

/// Every file a package ships: its manifest and its `.jwc` sources.
///
/// Nothing else. A package is source, and shipping whatever happens to sit
/// in the directory is how a `.env` reaches a registry.
fn packable(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![root.join("jwcproj.json")];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let p = entry?.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == VENDOR_DIR {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("jwc") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The archive, and its sha256. Deterministic: sorted paths, and no
/// timestamps or ownership from the filesystem.
pub fn pack(root: &Path) -> Result<(Vec<u8>, String)> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let files = packable(root)?;
    let mut tar = tar::Builder::new(Vec::new());
    for path in &files {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let data = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        tar.append_data(&mut header, rel, data.as_slice())?;
    }
    let tarball = tar.into_inner()?;
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(&mut gz, &tarball)?;
    let bytes = gz.finish()?;
    let sha = crate::hash::sha256_hex_bytes(&bytes);
    Ok((bytes, sha))
}

pub fn publish(path: PathBuf, registry: String, dry_run: bool) -> Result<()> {
    let ws = crate::workspace::Workspace::load(&path)?;
    let Some(manifest) = &ws.manifest else {
        bail!("no jwcproj.json under {}", path.display());
    };
    if manifest.kind != crate::workspace::Kind::Package {
        bail!(
            "{} is an application (`\"type\": \"app\"`). Only a package is published.",
            manifest.path.display()
        );
    }
    // packages.md §1.2 — the name goes into `import <name>;`, so a
    // hyphenated name is publishable and unusable. Refusing here is the
    // only place that can still be fixed cheaply: a registry name is
    // permanent.
    if !is_identifier(&manifest.name) {
        bail!(
            "`{}` cannot be written as `import {};` — a package name must also be an \
             identifier (letters, digits and `_`, not starting with a digit). \
             A registry name is permanent, so this is refused before it is taken.",
            manifest.name,
            manifest.name
        );
    }
    if manifest.version.is_empty() {
        bail!("{} has no `version`", manifest.path.display());
    }

    let root = manifest
        .path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(path.clone());
    let (bytes, sha) = pack(&root)?;

    println!(
        "{} {} — {} bytes, sha256 {sha}",
        manifest.name,
        manifest.version,
        bytes.len()
    );
    if dry_run {
        for f in packable(&root)? {
            println!("  {}", f.strip_prefix(&root).unwrap_or(&f).display());
        }
        return Ok(());
    }

    let token = token_for(&registry)?;
    let url = format!(
        "{}/api/v1/pkg/{}/{}",
        registry.trim_end_matches('/'),
        manifest.name,
        manifest.version
    );
    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(bytes)
            .file_name(format!("{}-{}.tar.gz", manifest.name, manifest.version)),
    );
    let res = reqwest::blocking::Client::new()
        .post(&url)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = res.status();
    let body = res.text().unwrap_or_default();
    if !status.is_success() {
        bail!("{status}: {body}");
    }
    println!("published to {registry}");
    Ok(())
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ── add ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PackageView {
    #[allow(dead_code)]
    name: String,
    versions: Vec<VersionView>,
}

#[derive(Deserialize)]
struct VersionView {
    version: String,
    sha256: String,
}

pub fn add(spec: String, path: PathBuf, registry: String) -> Result<()> {
    let (name, wanted) = match spec.split_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (spec.clone(), None),
    };
    if !is_identifier(&name) {
        bail!("`{name}` is not a name a program can `import`");
    }

    let base = registry.trim_end_matches('/');
    let client = reqwest::blocking::Client::new();
    let meta: PackageView = client
        .get(format!("{base}/api/v1/pkg/{name}"))
        .send()
        .with_context(|| format!("GET {base}/api/v1/pkg/{name}"))?
        .error_for_status()
        .with_context(|| format!("no package `{name}` on {registry}"))?
        .json()
        .context("the registry's answer was not the expected JSON")?;

    // The registry returns versions newest first.
    let picked = match &wanted {
        Some(v) => meta
            .versions
            .iter()
            .find(|x| &x.version == v)
            .with_context(|| format!("`{name}` has no version {v}"))?,
        None => meta
            .versions
            .first()
            .with_context(|| format!("`{name}` has no versions"))?,
    };

    let bytes = client
        .get(format!(
            "{base}/api/v1/pkg/{name}/{}/download",
            picked.version
        ))
        .send()
        .context("download")?
        .error_for_status()
        .context("download")?
        .bytes()
        .context("download")?;

    // Against the sha256 from the *metadata* request, not from this
    // response — see the module note.
    let got = crate::hash::sha256_hex_bytes(&bytes);
    if got != picked.sha256 {
        bail!(
            "checksum mismatch for {name} {}\n  want: {}\n  got:  {got}",
            picked.version,
            picked.sha256
        );
    }

    let root = crate::workspace::Workspace::load(&path)
        .ok()
        .and_then(|ws| ws.manifest.map(|m| m.path))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or(path.clone());
    let dest = root.join(VENDOR_DIR).join(&name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.to_path_buf();
        // A `..` or an absolute path in an archive is how an unpack writes
        // outside its directory. The registry does not produce one; a
        // registry is not the only thing that can serve a `.tar.gz`.
        if rel.is_absolute() || rel.components().any(|c| c.as_os_str() == "..") {
            bail!("the archive contains an unsafe path: {}", rel.display());
        }
        entry.unpack(dest.join(&rel))?;
    }

    record_dependency(&root, &name, &picked.version)?;
    println!(
        "added {name} {} to {}",
        picked.version,
        dest.strip_prefix(&root).unwrap_or(&dest).display()
    );
    Ok(())
}

/// Add the dependency to `jwcproj.json`, preserving everything else in the
/// file — it is the author's, not ours.
fn record_dependency(root: &Path, name: &str, version: &str) -> Result<()> {
    let path = root.join("jwcproj.json");
    let mut json: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).context("jwcproj.json is not valid JSON")?,
        Err(_) => serde_json::json!({ "name": "app", "version": "0.1.0", "type": "app" }),
    };
    let deps = json
        .as_object_mut()
        .context("jwcproj.json is not an object")?
        .entry("dependencies")
        .or_insert_with(|| serde_json::json!({}));
    deps.as_object_mut()
        .context("`dependencies` is not an object")?
        .insert(
            name.to_string(),
            serde_json::Value::String(format!("^{version}")),
        );
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&json)?))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_name_must_also_be_an_identifier() {
        // packages.md §1.2 — the registry's own rule allows a hyphen, and
        // `import jwc-redis;` does not parse. Both are true, which is why
        // publishing refuses the intersection rather than the union.
        assert!(is_identifier("redis"));
        assert!(is_identifier("jwc_redis"));
        assert!(!is_identifier("jwc-redis"));
        assert!(!is_identifier("2fast"));
        assert!(!is_identifier(""));
    }

    #[test]
    fn packing_is_deterministic_and_carries_only_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("jwcproj.json"), "{\"name\":\"x\"}").expect("w");
        std::fs::write(root.join("main.jwc"), "namespace x;\n").expect("w");
        std::fs::create_dir_all(root.join("src")).expect("d");
        std::fs::write(root.join("src/a.jwc"), "namespace x.a;\n").expect("w");
        // Not source, and not something to put on a registry.
        std::fs::write(root.join(".env"), "SECRET=1").expect("w");
        std::fs::write(root.join("notes.md"), "hello").expect("w");

        let files = packable(root).expect("packable");
        let names: Vec<String> = files
            .iter()
            .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
            .collect();
        assert_eq!(names, vec!["jwcproj.json", "main.jwc", "src/a.jwc"]);

        let (_, a) = pack(root).expect("pack");
        let (_, b) = pack(root).expect("pack");
        assert_eq!(a, b, "two packs of the same tree differ");
    }
}

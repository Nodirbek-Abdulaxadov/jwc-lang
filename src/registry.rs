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

/// The project root: the directory holding `jwcproj.json`, found from
/// `path` or an ancestor of it.
pub fn project_root(path: &Path) -> PathBuf {
    crate::workspace::Workspace::load(path)
        .ok()
        .and_then(|ws| ws.manifest.map(|m| m.path))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| path.to_path_buf())
}

/// `GET /api/v1/pkg/{name}` — the versions and their checksums.
fn versions_of(client: &reqwest::blocking::Client, base: &str, name: &str) -> Result<PackageView> {
    client
        .get(format!("{base}/api/v1/pkg/{name}"))
        .send()
        .with_context(|| format!("GET {base}/api/v1/pkg/{name}"))?
        .error_for_status()
        .with_context(|| format!("no package `{name}` on {base}"))?
        .json()
        .context("the registry's answer was not the expected JSON")
}

/// Download, verify and unpack one version into `<root>/jwc_packages/<name>`.
///
/// The checksum is `picked.sha256`, which came from the metadata request —
/// a *separate* round trip. See the module note: verifying a download
/// against a value the same response supplied is not integrity.
fn vendor(
    client: &reqwest::blocking::Client,
    base: &str,
    name: &str,
    picked: &VersionView,
    root: &Path,
) -> Result<PathBuf> {
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

    let got = crate::hash::sha256_hex_bytes(&bytes);
    if got != picked.sha256 {
        bail!(
            "checksum mismatch for {name} {}\n  want: {}\n  got:  {got}",
            picked.version,
            picked.sha256
        );
    }

    let dest = root.join(VENDOR_DIR).join(name);
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
    Ok(dest)
}

/// The newest version satisfying `req`, from a newest-first list.
///
/// `req` is what `jwcproj.json` records: `^1.2.3`, or a bare `1.2.3`
/// meaning exactly that. An unparseable requirement is an error rather
/// than a silent "take the newest" — a typo in a version range must not
/// quietly become "whatever shipped today".
fn pick<'a>(name: &str, versions: &'a [VersionView], req: &str) -> Result<&'a VersionView> {
    let req = req.trim();
    if req.is_empty() || req == "*" {
        return versions
            .first()
            .with_context(|| format!("`{name}` has no versions"));
    }
    let range = semver::VersionReq::parse(req)
        .with_context(|| format!("`{name}`'s requirement `{req}` is not a semver range"))?;
    versions
        .iter()
        .find(|v| {
            semver::Version::parse(&v.version)
                .map(|parsed| range.matches(&parsed))
                .unwrap_or(false)
        })
        .with_context(|| format!("no version of `{name}` satisfies `{req}`"))
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
    let meta = versions_of(&client, base, &name)?;

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

    let root = project_root(&path);
    let dest = vendor(&client, base, &name, picked, &root)?;
    record_dependency(&root, &name, &picked.version)?;
    println!(
        "added {name} {} to {}",
        picked.version,
        dest.strip_prefix(&root).unwrap_or(&dest).display()
    );
    Ok(())
}

/// `jwc install` — materialise every declared dependency that is missing.
///
/// This is the command a fresh clone needs. `jwc_packages/` is a build
/// artefact for most projects (the template's `.gitignore` says so), so a
/// checkout has the manifest and none of the sources, and `jwc check`
/// fails on imports it cannot resolve. `jwc add` per dependency would do
/// it and would also rewrite every version in the manifest.
///
/// Transitive: a vendored package's own `jwcproj.json` may declare
/// dependencies, and its `import`s resolve against them.
pub fn install(path: PathBuf, registry: String, force: bool) -> Result<()> {
    let root = project_root(&path);
    let base = registry.trim_end_matches('/');
    let client = reqwest::blocking::Client::new();

    let mut pending: Vec<(String, String)> = declared_dependencies(&root.join("jwcproj.json"))?;
    let mut done: std::collections::BTreeSet<String> = Default::default();
    let mut installed = 0usize;
    let mut kept = 0usize;

    while let Some((name, req)) = pending.pop() {
        if !done.insert(name.clone()) {
            continue;
        }
        if !is_identifier(&name) {
            bail!("`{name}` in jwcproj.json is not a name a program can `import`");
        }
        let dest = root.join(VENDOR_DIR).join(&name);
        if dest.is_dir() && !force {
            kept += 1;
        } else {
            let meta = versions_of(&client, base, &name)?;
            let picked = pick(&name, &meta.versions, &req)?;
            vendor(&client, base, &name, picked, &root)?;
            println!("  {name} {}", picked.version);
            installed += 1;
        }
        pending.extend(declared_dependencies(&dest.join("jwcproj.json"))?);
    }

    match (installed, kept) {
        (0, 0) => println!("no dependencies declared"),
        (0, k) => println!("{k} package{} already present", plural(k)),
        (i, 0) => println!("installed {i} package{}", plural(i)),
        (i, k) => println!("installed {i} package{}, {k} already present", plural(i)),
    }
    Ok(())
}

/// `jwc update [name]` — move within the recorded range.
///
/// Without a name, every direct dependency. The manifest's requirement is
/// the bound: `^0.2.1` moves to the newest `0.2.x`, never to `0.3.0`. To
/// cross a major, `jwc add name@version` — which is the request to change
/// the requirement, and says so.
pub fn update(name: Option<String>, path: PathBuf, registry: String) -> Result<()> {
    let root = project_root(&path);
    let base = registry.trim_end_matches('/');
    let client = reqwest::blocking::Client::new();

    let declared = declared_dependencies(&root.join("jwcproj.json"))?;
    if declared.is_empty() {
        println!("no dependencies declared");
        return Ok(());
    }
    let targets: Vec<(String, String)> = match &name {
        Some(n) => {
            let found = declared
                .iter()
                .find(|(d, _)| d == n)
                .with_context(|| format!("`{n}` is not a dependency in jwcproj.json"))?;
            vec![found.clone()]
        }
        None => declared,
    };

    let mut moved = 0usize;
    for (name, req) in targets {
        let meta = versions_of(&client, base, &name)?;
        let picked = pick(&name, &meta.versions, &req)?;
        let before = vendored_version(&root, &name);
        if before.as_deref() == Some(picked.version.as_str()) {
            println!("  {name} {} (unchanged)", picked.version);
            continue;
        }
        vendor(&client, base, &name, picked, &root)?;
        // The requirement is re-recorded from the version now on disk, so
        // `^0.2.1` becomes `^0.2.4` — the floor rises with what is
        // actually installed, which is what makes the manifest a record of
        // the build rather than of one past intention.
        record_dependency(&root, &name, &picked.version)?;
        match before {
            Some(b) => println!("  {name} {b} -> {}", picked.version),
            None => println!("  {name} {}", picked.version),
        }
        moved += 1;
    }
    println!("{moved} package{} updated", plural(moved));
    Ok(())
}

/// `jwc remove <name>` — drop it from the manifest and from disk.
///
/// Offline: nothing here needs a registry.
pub fn remove(name: String, path: PathBuf) -> Result<()> {
    let root = project_root(&path);
    let manifest = root.join("jwcproj.json");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("no jwcproj.json at {}", root.display()))?;
    let mut json: serde_json::Value =
        serde_json::from_str(&text).context("jwcproj.json is not valid JSON")?;

    let removed = json
        .get_mut("dependencies")
        .and_then(|d| d.as_object_mut())
        .and_then(|o| o.remove(&name))
        .is_some();
    if !removed {
        bail!("`{name}` is not a dependency in jwcproj.json");
    }
    std::fs::write(
        &manifest,
        format!("{}\n", serde_json::to_string_pretty(&json)?),
    )?;

    let dest = root.join(VENDOR_DIR).join(&name);
    if dest.is_dir() {
        std::fs::remove_dir_all(&dest)?;
    }
    println!("removed {name}");
    // Not automatic: a transitive dependency this one pulled in may now be
    // unreferenced, and deleting sources the user might still import is
    // not something to do without being asked.
    println!("`jwc tree` shows what is still vendored");
    Ok(())
}

/// `jwc tree` — what is declared, what is vendored, and at which version.
///
/// Offline, and it reads disk rather than the registry on purpose: the
/// question it answers is "what will actually compile", and that is the
/// vendored tree, not the intention in the manifest.
pub fn tree(path: PathBuf) -> Result<()> {
    let root = project_root(&path);
    let manifest = root.join("jwcproj.json");
    let name = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|j| j.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .unwrap_or_else(|| "(this project)".into());

    println!("{name}");
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    print_tree(&root, &manifest, "", &mut seen)
}

fn print_tree(
    root: &Path,
    manifest: &Path,
    indent: &str,
    seen: &mut std::collections::BTreeSet<String>,
) -> Result<()> {
    let deps = declared_dependencies(manifest)?;
    for (i, (name, req)) in deps.iter().enumerate() {
        let last = i + 1 == deps.len();
        let branch = if last { "└── " } else { "├── " };
        let dest = root.join(VENDOR_DIR).join(name);
        let state = match vendored_version(root, name) {
            Some(v) if v == *req || format!("^{v}") == *req => v.to_string(),
            Some(v) => format!("{v} (declared {req})"),
            // The state `jwc install` exists to fix, and the one that
            // makes `jwc check` fail on an import that looks correct.
            None => format!("{req} — not installed"),
        };
        println!("{indent}{branch}{name} {state}");
        if seen.insert(name.clone()) {
            let next = format!("{indent}{}", if last { "    " } else { "│   " });
            print_tree(root, &dest.join("jwcproj.json"), &next, seen)?;
        }
    }
    Ok(())
}

/// `dependencies` from a manifest, as `(name, requirement)`, sorted.
/// A missing or unreadable manifest has none — a package without one
/// declares no dependencies, which is the truthful reading.
///
/// A requirement must be a **string**. Anything else used to be read as
/// `"*"`, which turned `{"path": "../redis"}` — a form the documentation
/// advertised and nothing ever implemented — into a wildcard fetch from
/// the registry, failing with a 404 about a version rather than a word
/// about the manifest.
fn declared_dependencies(manifest: &Path) -> Result<Vec<(String, String)>> {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return Ok(Vec::new());
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(Vec::new());
    };
    let Some(obj) = json.get("dependencies").and_then(|d| d.as_object()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (name, req) in obj {
        let Some(req) = req.as_str() else {
            bail!(
                "{}: dependency `{name}` is {}, and a requirement has to be a version \
                 string like \"^1.2.0\".\n\
                 \n\
                 Path and git dependencies do not exist: every dependency is fetched \
                 from a registry and vendored under `{VENDOR_DIR}/`. To work on a \
                 package beside its consumer, point `JWC_REGISTRY` at a local one, or \
                 copy the package into `{VENDOR_DIR}/{name}/` yourself — `jwc check` \
                 reads what is vendored and does not re-fetch it.",
                manifest.display(),
                match req {
                    serde_json::Value::Object(_) => "an object",
                    serde_json::Value::Array(_) => "an array",
                    serde_json::Value::Null => "null",
                    _ => "not a string",
                }
            );
        };
        out.push((name.clone(), req.to_string()));
    }
    Ok(out)
}

/// The `version` in a vendored package's own manifest.
fn vendored_version(root: &Path, name: &str) -> Option<String> {
    let text =
        std::fs::read_to_string(root.join(VENDOR_DIR).join(name).join("jwcproj.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
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

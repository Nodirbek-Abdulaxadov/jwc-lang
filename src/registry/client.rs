//! HTTP client for the package registry.
//!
//! Uses the existing `reqwest` (rustls) client to keep the binary lean. The
//! tarball download path streams through `flate2` + `tar` to produce the
//! extracted source tree directly under the cache directory.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use semver::Version;
use serde::Deserialize;

use crate::registry::config::Credentials;

#[derive(Debug, Deserialize)]
struct RegistryVersionsResponse {
    #[serde(default)]
    versions: Vec<RegistryVersion>,
}

#[derive(Debug, Deserialize)]
struct RegistryVersion {
    #[serde(rename = "num")]
    version: String,
    #[serde(default)]
    yanked: bool,
}

fn http() -> &'static reqwest::blocking::Client {
    use std::sync::OnceLock;
    static C: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("jwc-pkg/0.1")
            .build()
            .expect("failed to build reqwest blocking client")
    })
}

fn host_of(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string())
}

fn auth_token(base_url: &str) -> Option<String> {
    let creds = Credentials::load().ok()?;
    creds
        .token_for_host(&host_of(base_url))
        .map(|s| s.to_string())
}

pub fn list_versions(base_url: &str, name: &str) -> Result<Vec<Version>> {
    let url = format!("{}/api/v1/crates/{}", base_url.trim_end_matches('/'), name);
    let mut req = http().get(&url);
    if let Some(tok) = auth_token(base_url) {
        req = req.header("Authorization", format!("Bearer {}", tok));
    }
    let resp = req.send().with_context(|| format!("GET {} failed", url))?;
    if !resp.status().is_success() {
        bail!("Registry returned HTTP {} for {}", resp.status(), url);
    }
    let body: RegistryVersionsResponse = resp
        .json()
        .with_context(|| format!("Failed to parse registry response from {}", url))?;
    let mut versions: Vec<Version> = body
        .versions
        .into_iter()
        .filter(|v| !v.yanked)
        .filter_map(|v| Version::parse(&v.version).ok())
        .collect();
    versions.sort();
    Ok(versions)
}

/// Download the tarball for `name@version`, verify its checksum (if the
/// registry returns one in a `X-Checksum-Sha256` header), and extract it
/// into `dest`. The directory is created if missing.
pub fn download_and_extract(base_url: &str, name: &str, version: &str, dest: &Path) -> Result<()> {
    let url = format!(
        "{}/api/v1/crates/{}/{}/download",
        base_url.trim_end_matches('/'),
        name,
        version
    );
    let mut req = http().get(&url);
    if let Some(tok) = auth_token(base_url) {
        req = req.header("Authorization", format!("Bearer {}", tok));
    }
    let mut resp = req.send().with_context(|| format!("GET {} failed", url))?;
    if !resp.status().is_success() {
        bail!("Registry returned HTTP {} for {}", resp.status(), url);
    }

    let expected_checksum = resp
        .headers()
        .get("X-Checksum-Sha256")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let mut bytes = Vec::new();
    resp.copy_to(&mut bytes)
        .with_context(|| format!("Failed to read response body from {}", url))?;

    if let Some(expected) = expected_checksum {
        let actual = sha256_hex(&bytes);
        if actual.eq_ignore_ascii_case(&expected) {
            // ok
        } else {
            bail!(
                "Checksum mismatch for {}@{}: registry header X-Checksum-Sha256={}, computed sha256={}",
                name,
                version,
                expected,
                actual
            );
        }
    }

    std::fs::create_dir_all(dest)
        .with_context(|| format!("Failed to create {}", dest.display()))?;

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&bytes[..]));
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest).with_context(|| {
        format!(
            "Failed to extract {}@{} tarball into {}",
            name,
            version,
            dest.display()
        )
    })?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    out.iter()
        .fold(String::with_capacity(out.len() * 2), |mut s, b| {
            s.push_str(&format!("{:02x}", b));
            s
        })
}

#[allow(dead_code)]
pub fn ping(base_url: &str) -> Result<()> {
    let resp = http().get(base_url).send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("Registry ping failed: HTTP {}", resp.status()));
    }
    Ok(())
}

//! `http.*` — outbound HTTP (builtins.md §7c).
//!
//! Restored in 0.9.921. The v0.25.0 cutover deleted `http_get`,
//! `http_post` and `fetch_json` with the rest of the 0.9 front-end, and
//! nothing replaced them, so a JWC service could not call another service
//! at all — no payment provider, no OAuth exchange, no webhook. That is a
//! large hole in a language whose whole subject is HTTP backends.
//!
//! `JWC_HTTP_ALLOWLIST` and `JWC_HTTP_BLOCK_PRIVATE` survived in
//! `config::REGISTRY` the whole time, documented against builtins that no
//! longer existed. They mean what they say again.
//!
//! ## The guard runs before the request
//!
//! An SSRF check that runs after the socket is open has already leaked
//! whether the host exists. Both gates below are evaluated on the URL and
//! refuse before anything is dispatched.

use anyhow::{anyhow, bail, Result};
use std::sync::OnceLock;
use std::time::Duration;

/// One client for the process: connection reuse is the point, and a fresh
/// client per call also re-resolves DNS every time.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(timeout())
            // A redirect is how an allowlisted host walks you to one that
            // is not, so the allowlist has to see every hop. `none` plus
            // an explicit re-check would be the thorough form; refusing to
            // follow at all is the one that cannot be got round.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("jwc/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

fn timeout() -> Duration {
    let secs = std::env::var("JWC_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(10);
    Duration::from_secs(secs)
}

/// Both gates, in the order that keeps a blocked URL off the network.
pub fn check_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("`{url}` is not a URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => bail!("`{other}:` is not a scheme `http.*` will request; use http or https"),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("`{url}` has no host"))?
        .to_ascii_lowercase();

    check_allowlisted(&host)?;
    check_not_private(&host)
}

/// `JWC_HTTP_ALLOWLIST`, comma-separated hosts. Empty means no
/// restriction, which is the default a program that talks to one API
/// should not stay on.
fn check_allowlisted(host: &str) -> Result<()> {
    let list = allowlist();
    if list.is_empty() || list.iter().any(|h| h == host) {
        return Ok(());
    }
    bail!(
        "`{host}` is not in JWC_HTTP_ALLOWLIST (allowed: {})",
        list.join(", ")
    )
}

fn allowlist() -> &'static Vec<String> {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("JWC_HTTP_ALLOWLIST")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// `JWC_HTTP_BLOCK_PRIVATE`. Off by default because it breaks talking to
/// a sibling container by name, which is an ordinary thing to do; on, it
/// is what stops a user-supplied URL reaching the cloud metadata endpoint.
///
/// This resolves the host and checks every address it answers with: a name
/// that resolves to `169.254.169.254` is the whole attack, and checking
/// the literal text would miss it.
fn check_not_private(host: &str) -> Result<()> {
    if !block_private() {
        return Ok(());
    }
    use std::net::{IpAddr, ToSocketAddrs};

    let addrs: Vec<IpAddr> = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| anyhow!("`{host}` did not resolve: {e}"))?
        .map(|s| s.ip())
        .collect();

    if addrs.is_empty() {
        bail!("`{host}` resolved to no address");
    }
    for ip in &addrs {
        if is_private(ip) {
            bail!(
                "`{host}` resolves to {ip}, which JWC_HTTP_BLOCK_PRIVATE refuses \
                 (loopback, private, link-local or unspecified)"
            );
        }
    }
    Ok(())
}

fn block_private() -> bool {
    static BLOCK: OnceLock<bool> = OnceLock::new();
    *BLOCK.get_or_init(|| {
        matches!(
            std::env::var("JWC_HTTP_BLOCK_PRIVATE")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn is_private(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        // `169.254.0.0/16` covers the cloud metadata address, which is the
        // one this setting exists for.
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local and fe80::/10 link-local, neither
                // of which has a stable accessor on stable Rust.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// What every `http.*` call answers with.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

pub async fn request(method: &str, url: &str, body: Option<String>) -> Result<Reply> {
    check_url(url)?;
    let mut req = match method {
        "GET" => client().get(url),
        "POST" => client().post(url),
        other => bail!("`{other}` is not a method `http.*` sends"),
    };
    if let Some(b) = body {
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(b);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| anyhow!("{method} {url} failed: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| anyhow!("{method} {url}: could not read the body: {e}"))?;
    Ok(Reply { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_http_scheme_is_refused_before_anything_resolves() {
        // `file:` is the one that turns "fetch this URL" into "read this
        // file", and it costs nothing to refuse by name.
        for url in ["file:///etc/passwd", "ftp://example.com/x", "gopher://x/"] {
            assert!(check_url(url).is_err(), "{url} should be refused");
        }
    }

    #[test]
    fn a_url_without_a_host_is_refused() {
        assert!(check_url("http://").is_err());
        assert!(check_url("notaurl").is_err());
    }

    #[test]
    fn the_private_ranges_are_the_ones_named() {
        use std::net::IpAddr;
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            // The cloud metadata endpoint, which is the reason the setting
            // exists at all.
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            let parsed: IpAddr = ip.parse().expect(ip);
            assert!(is_private(&parsed), "{ip} should count as private");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "2606:4700::1111"] {
            let parsed: IpAddr = ip.parse().expect(ip);
            assert!(!is_private(&parsed), "{ip} is public");
        }
    }
}

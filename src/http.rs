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
///
/// **What it does not stop: DNS rebinding.** The name is resolved here and
/// resolved again by the HTTP client when it connects, and between the two
/// a short-TTL record can change from a public address to a private one.
/// Closing it means resolving once and dialling the address that was
/// checked, which `reqwest` cannot be told to do without a custom
/// resolver. It is a real gap and is written down as one in
/// `security.md §9.4` rather than left for someone to find.
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

/// Whether an address is one `JWC_HTTP_BLOCK_PRIVATE` refuses.
///
/// The list is longer than "the RFC 1918 three" because the addresses that
/// matter here are the ones a cloud puts a credential endpoint on, and
/// they are not all in the obvious ranges:
///
/// * `169.254.0.0/16` — AWS, GCP and Azure metadata at `169.254.169.254`.
/// * `100.64.0.0/10` — carrier-grade NAT, and **Alibaba Cloud metadata at
///   `100.100.100.200`**. Measured before 0.9.943: not refused. It is not
///   a private range by RFC 1918, which is exactly why a check written
///   from memory misses it.
/// * `0.0.0.0/8` — `0.0.0.1` and friends route to the local host on Linux,
///   and `is_unspecified()` matches only `0.0.0.0` itself.
/// * `192.0.0.0/24`, `198.18.0.0/15`, `240.0.0.0/4`, `255.255.255.255` —
///   IETF protocol assignments, benchmarking and reserved space. Nothing
///   an outbound call should reach, and free to refuse.
/// * An IPv4-mapped IPv6 address (`::ffff:127.0.0.1`) is the **v4**
///   address wearing a v6 spelling: `Ipv6Addr::is_loopback` is false for
///   it. Unwrapped and re-checked rather than listed.
///
/// The spellings a caller can use for these — `0x7f000001`, `2130706433`,
/// `0177.0.0.1`, `127.1` — are normalised to dotted-quad by `url::Url`
/// before they reach here, which is checked by a test rather than assumed.
fn is_private(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                // "this network" — `0.0.0.0/8`.
                || o[0] == 0
                // Carrier-grade NAT, `100.64.0.0/10`.
                || (o[0] == 100 && (64..128).contains(&o[1]))
                // IETF protocol assignments, `192.0.0.0/24`.
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // Benchmarking, `198.18.0.0/15`.
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                // Reserved, `240.0.0.0/4`.
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address is a v4 address, so it gets the v4
            // rules rather than a second, shorter list.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
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

    /// Every spelling of `127.0.0.1` a caller can reach for.
    ///
    /// This is the half of the check that is *not* in `is_private`: the
    /// guard reads `Url::host_str()`, and if that returned `0x7f000001`
    /// verbatim the address check would never see a loopback address at
    /// all. WHATWG URL parsing normalises hex, decimal, octal and short
    /// forms to dotted-quad, and `url` implements it — but a guard that
    /// depends on someone else's normalisation should say so out loud
    /// rather than leave it to be rediscovered.
    #[test]
    fn the_alternative_spellings_of_an_address_normalise_before_the_check() {
        for spelling in [
            "http://0x7f000001/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://127.1/",
            "http://127.0.0.1/",
        ] {
            let host = url::Url::parse(spelling)
                .expect(spelling)
                .host_str()
                .expect(spelling)
                .to_string();
            assert_eq!(host, "127.0.0.1", "{spelling} must normalise");
        }
        // And the userinfo trick, where the allowlisted name is a
        // *username* and the real host follows the `@`.
        let host = url::Url::parse("http://api.example.com@evil.example/x")
            .expect("parses")
            .host_str()
            .expect("host")
            .to_string();
        assert_eq!(host, "evil.example");
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
            // Alibaba Cloud puts its metadata endpoint here, inside
            // carrier-grade NAT rather than any RFC 1918 range. Not
            // refused until 0.9.943 — measured.
            "100.100.100.200",
            "100.64.0.1",
            // `0.0.0.0/8` routes to the local host on Linux, and
            // `is_unspecified()` matches only `0.0.0.0` itself.
            "0.0.0.1",
            "0.1.2.3",
            "255.255.255.255",
            "192.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            // The same addresses wearing a v6 spelling. `is_loopback` on
            // an `Ipv6Addr` is false for every one of them.
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ] {
            let parsed: IpAddr = ip.parse().expect(ip);
            assert!(is_private(&parsed), "{ip} should count as private");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700::1111",
            // The edges of the ranges above, on the public side. A guard
            // that swallowed these would break ordinary outbound calls.
            "100.63.255.255",
            "100.128.0.1",
            "192.0.1.1",
            "198.20.0.1",
            "::ffff:8.8.8.8",
        ] {
            let parsed: IpAddr = ip.parse().expect(ip);
            assert!(!is_private(&parsed), "{ip} is public");
        }
    }
}

//! JWKS (JSON Web Key Set) fetching and caching for RS256 verification.
//!
//! An OIDC provider publishes its signing keys at a `jwks_uri` taken from
//! `/.well-known/openid-configuration`. A resource server verifying an
//! access token has to fetch that document, pick the key whose `kid`
//! matches the token header, and cache the result — refetching per
//! request would put an HTTP round-trip in front of every authenticated
//! call and hammer the identity provider.
//!
//! ## The refetch-storm hazard
//!
//! Providers rotate keys without warning, so "unknown `kid` → refetch"
//! is the standard recovery. Implemented naively it is a denial-of-service
//! amplifier: `kid` comes from an *unauthenticated* token header, so an
//! attacker sends tokens carrying random `kid`s and every one of them
//! triggers an outbound fetch. A few hundred requests per second against
//! your public API become a few hundred requests per second against your
//! identity provider — which then rate-limits or falls over, taking real
//! logins down with it. The signature never even gets checked, because
//! key lookup happens first.
//!
//! Two things contain it:
//!
//! 1. **A refetch cooldown** ([`min_refetch_interval`]). After a forced
//!    refetch, further unknown-`kid` misses reuse the cached set until
//!    the cooldown expires. A genuine rotation costs one fetch and
//!    resolves within the cooldown; a flood costs one fetch per cooldown
//!    window no matter how fast it arrives.
//! 2. **Single-flight.** The cache sits behind a `tokio::sync::Mutex`
//!    held across the fetch, so N concurrent misses produce one request
//!    and N cache hits rather than N requests.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::Value as JsonValue;
use tokio::sync::Mutex;

/// Default lifetime of a cached key set. Long enough that steady-state
/// traffic never fetches, short enough that a planned rotation heals on
/// its own without anyone restarting the process.
const DEFAULT_TTL_SECS: u64 = 300;

/// Default floor between two forced refetches of the same URL. See the
/// module docs — this is the number that turns an unbounded amplifier
/// into "at most one extra fetch per minute per JWKS URL".
const DEFAULT_MIN_REFETCH_SECS: u64 = 60;

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn cache_ttl() -> Duration {
    static TTL: OnceLock<Duration> = OnceLock::new();
    *TTL.get_or_init(|| Duration::from_secs(env_secs("JWC_JWT_JWKS_TTL_SECS", DEFAULT_TTL_SECS)))
}

/// Minimum wall-clock gap between forced refetches of one JWKS URL.
fn min_refetch_interval() -> Duration {
    static MIN: OnceLock<Duration> = OnceLock::new();
    *MIN.get_or_init(|| {
        Duration::from_secs(env_secs(
            "JWC_JWT_JWKS_MIN_REFETCH_SECS",
            DEFAULT_MIN_REFETCH_SECS,
        ))
    })
}

/// An RSA signing key from a JWKS document, kept in its wire form: the
/// base64url `n` / `e` components are exactly what `ring` wants, so
/// there is nothing to decode until a verification actually happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsaJwk {
    pub kid: Option<String>,
    pub n: String,
    pub e: String,
}

/// Parse a JWKS document, keeping only the keys usable for RS256
/// signature verification.
///
/// Skipped without complaint: non-RSA key types, keys marked
/// `"use": "enc"` (encryption keys are published in the same document and
/// must never verify a signature), and keys pinned to a different `alg`.
/// A key with no `alg` is kept — plenty of providers omit it.
pub fn parse_jwks(doc: &JsonValue) -> Result<Vec<RsaJwk>> {
    let keys = doc
        .get("keys")
        .and_then(|k| k.as_array())
        .ok_or_else(|| anyhow!("jwt_verify: JWKS document has no 'keys' array"))?;

    let mut out = Vec::new();
    for key in keys {
        if key.get("kty").and_then(|v| v.as_str()) != Some("RSA") {
            continue;
        }
        if let Some(use_) = key.get("use").and_then(|v| v.as_str()) {
            if use_ != "sig" {
                continue;
            }
        }
        if let Some(alg) = key.get("alg").and_then(|v| v.as_str()) {
            if alg != "RS256" {
                continue;
            }
        }
        let (Some(n), Some(e)) = (
            key.get("n").and_then(|v| v.as_str()),
            key.get("e").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        out.push(RsaJwk {
            kid: key.get("kid").and_then(|v| v.as_str()).map(str::to_string),
            n: n.to_string(),
            e: e.to_string(),
        });
    }
    Ok(out)
}

/// Pick the key a token should be verified against.
///
/// With a `kid`, only an exact match is acceptable. Without one, a
/// single-key set is unambiguous and is used; a multi-key set is not,
/// and guessing there would mean trying keys until one verifies — which
/// is how signature-confusion bugs start.
pub fn select_key<'a>(keys: &'a [RsaJwk], kid: Option<&str>) -> Option<&'a RsaJwk> {
    match kid {
        Some(kid) => keys.iter().find(|k| k.kid.as_deref() == Some(kid)),
        None => match keys {
            [only] => Some(only),
            _ => None,
        },
    }
}

struct CacheEntry {
    keys: Vec<RsaJwk>,
    fetched_at: Instant,
    /// When a forced (unknown-`kid`) refetch last happened, so the
    /// cooldown can be enforced independently of the TTL.
    last_forced: Option<Instant>,
}

impl CacheEntry {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < cache_ttl()
    }

    fn may_force_refetch(&self) -> bool {
        match self.last_forced {
            None => true,
            Some(at) => at.elapsed() >= min_refetch_interval(),
        }
    }
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn fetch_jwks(url: &str) -> Result<Vec<RsaJwk>> {
    let resp = crate::runner::http_client()
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("jwt_verify: JWKS fetch failed for '{url}': {e}"))?;
    if !resp.status().is_success() {
        bail!(
            "jwt_verify: JWKS fetch returned HTTP {} for '{url}'",
            resp.status().as_u16()
        );
    }
    let doc: JsonValue = resp
        .json()
        .await
        .map_err(|e| anyhow!("jwt_verify: JWKS response is not JSON for '{url}': {e}"))?;
    let keys = parse_jwks(&doc)?;
    if keys.is_empty() {
        bail!("jwt_verify: JWKS at '{url}' contains no usable RS256 keys");
    }
    Ok(keys)
}

/// Resolve the RSA key for a token's `kid`, fetching and caching the
/// JWKS document as needed.
///
/// The whole lookup happens under one lock so concurrent misses collapse
/// into a single outbound request (see the module docs).
pub async fn rsa_key_for(jwks_url: &str, kid: Option<&str>) -> Result<RsaJwk> {
    let mut guard = cache().lock().await;

    // 1. Populate or refresh on TTL.
    let need_initial_fetch = match guard.get(jwks_url) {
        None => true,
        Some(entry) => !entry.is_fresh(),
    };
    if need_initial_fetch {
        let keys = fetch_jwks(jwks_url).await?;
        guard.insert(
            jwks_url.to_string(),
            CacheEntry {
                keys,
                fetched_at: Instant::now(),
                last_forced: None,
            },
        );
    }

    // 2. Try the cached set.
    if let Some(entry) = guard.get(jwks_url) {
        if let Some(key) = select_key(&entry.keys, kid) {
            return Ok(key.clone());
        }
    }

    // 3. Unknown `kid` — the provider may have rotated. Refetch once,
    //    rate-limited, so a stream of bogus `kid`s cannot turn into a
    //    stream of outbound requests.
    let may_force = guard
        .get(jwks_url)
        .map(|e| e.may_force_refetch())
        .unwrap_or(true);

    if may_force {
        let keys = fetch_jwks(jwks_url).await?;
        let entry = CacheEntry {
            keys,
            fetched_at: Instant::now(),
            last_forced: Some(Instant::now()),
        };
        let found = select_key(&entry.keys, kid).cloned();
        guard.insert(jwks_url.to_string(), entry);
        if let Some(key) = found {
            return Ok(key);
        }
    }

    match kid {
        Some(kid) => bail!("jwt_verify: no JWKS key matches kid '{kid}' at '{jwks_url}'"),
        None => bail!(
            "jwt_verify: token has no 'kid' and the JWKS at '{jwks_url}' \
             publishes more than one key, so the signing key is ambiguous"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jwk(kid: &str) -> JsonValue {
        json!({"kty":"RSA","use":"sig","alg":"RS256","kid":kid,"n":"AQAB-n","e":"AQAB"})
    }

    #[test]
    fn parse_jwks_keeps_rs256_signing_keys() {
        let doc = json!({"keys":[jwk("a"), jwk("b")]});
        let keys = parse_jwks(&doc).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].kid.as_deref(), Some("a"));
        assert_eq!(keys[0].e, "AQAB");
    }

    #[test]
    fn parse_jwks_skips_encryption_keys() {
        // An encryption key living in the same document must never be
        // offered up as a signature-verification key.
        let doc = json!({"keys":[
            {"kty":"RSA","use":"enc","kid":"enc","n":"x","e":"AQAB"},
            jwk("sig"),
        ]});
        let keys = parse_jwks(&doc).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].kid.as_deref(), Some("sig"));
    }

    #[test]
    fn parse_jwks_skips_non_rsa_and_other_algs() {
        let doc = json!({"keys":[
            {"kty":"EC","use":"sig","kid":"ec","x":"a","y":"b","crv":"P-256"},
            {"kty":"RSA","use":"sig","alg":"RS512","kid":"rs512","n":"x","e":"AQAB"},
            jwk("ok"),
        ]});
        let keys = parse_jwks(&doc).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].kid.as_deref(), Some("ok"));
    }

    #[test]
    fn parse_jwks_keeps_keys_without_alg() {
        // OpenIddict and others omit `alg`; dropping those would leave
        // the set empty and break verification outright.
        let doc = json!({"keys":[{"kty":"RSA","use":"sig","kid":"noalg","n":"x","e":"AQAB"}]});
        let keys = parse_jwks(&doc).unwrap();
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn parse_jwks_rejects_document_without_keys_array() {
        let err = parse_jwks(&json!({"nope": true})).unwrap_err().to_string();
        assert!(err.contains("no 'keys' array"), "got: {err}");
    }

    #[test]
    fn parse_jwks_skips_entries_missing_components() {
        let doc = json!({"keys":[{"kty":"RSA","use":"sig","kid":"partial","e":"AQAB"}]});
        assert!(parse_jwks(&doc).unwrap().is_empty());
    }

    #[test]
    fn select_key_matches_on_kid() {
        let keys = parse_jwks(&json!({"keys":[jwk("a"), jwk("b")]})).unwrap();
        assert_eq!(
            select_key(&keys, Some("b")).unwrap().kid.as_deref(),
            Some("b")
        );
        assert!(select_key(&keys, Some("missing")).is_none());
    }

    #[test]
    fn select_key_without_kid_only_when_unambiguous() {
        let one = parse_jwks(&json!({"keys":[jwk("a")]})).unwrap();
        assert!(select_key(&one, None).is_some());

        // Two keys and no `kid` is ambiguous — refuse rather than guess.
        let two = parse_jwks(&json!({"keys":[jwk("a"), jwk("b")]})).unwrap();
        assert!(select_key(&two, None).is_none());
    }

    #[test]
    fn refetch_cooldown_blocks_a_second_forced_fetch() {
        // The DoS guard: once a forced refetch has happened, the next
        // unknown-`kid` miss must reuse the cache instead of going out
        // again. Uses a far-future cooldown so the test is not timing
        // dependent.
        let entry = CacheEntry {
            keys: Vec::new(),
            fetched_at: Instant::now(),
            last_forced: Some(Instant::now()),
        };
        assert!(
            !entry.may_force_refetch(),
            "a refetch moments after the last one must be suppressed"
        );

        let never_forced = CacheEntry {
            keys: Vec::new(),
            fetched_at: Instant::now(),
            last_forced: None,
        };
        assert!(
            never_forced.may_force_refetch(),
            "the first forced refetch must be allowed"
        );
    }

    #[test]
    fn cache_entry_expires_after_ttl() {
        let fresh = CacheEntry {
            keys: Vec::new(),
            fetched_at: Instant::now(),
            last_forced: None,
        };
        assert!(fresh.is_fresh());

        let stale = CacheEntry {
            keys: Vec::new(),
            fetched_at: Instant::now() - cache_ttl() - Duration::from_secs(1),
            last_forced: None,
        };
        assert!(!stale.is_fresh());
    }
}

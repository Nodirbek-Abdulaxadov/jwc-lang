use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value as JsonValue;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Returns the current Unix time in seconds, defaulting to 0 if the
/// system clock is impossibly set before the epoch. The `0` default
/// means a clock skew never accidentally accepts an expired token —
/// the only risk would be rejecting a valid one, which is the
/// fail-closed direction.
fn unix_now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Sign a JSON payload string with HS256 and return a compact JWT
/// (`header.payload.signature`). The header is a fixed `{"alg":"HS256","typ":"JWT"}`.
pub fn sign_hs256(payload_json: &str, secret: &str) -> Result<String> {
    // Validate payload is JSON so the resulting token is decodable.
    let _: JsonValue = serde_json::from_str(payload_json)
        .map_err(|_| anyhow!("jwt_sign: payload must be valid JSON"))?;

    let header_b64 = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);
    let signing_input = format!("{header_b64}.{payload_b64}");

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| anyhow!("jwt_sign: invalid secret: {e}"))?;
    mac.update(signing_input.as_bytes());
    let signature = mac.finalize().into_bytes();
    let signature_b64 = URL_SAFE_NO_PAD.encode(signature);

    Ok(format!("{signing_input}.{signature_b64}"))
}

/// Strip an optional, case-insensitive `Bearer ` scheme prefix (with any
/// surrounding whitespace) from an `Authorization` header value, returning the
/// bare token. A raw token without the prefix passes through unchanged.
///
/// The slice is taken with `get(..7)` rather than `[..7]`: an
/// `Authorization` header is attacker-controlled, and a value whose 7th
/// byte lands inside a multibyte UTF-8 sequence (`"Кириллица..."`) made
/// the old byte-index panic — a remote crash on an unauthenticated path.
/// A non-char-boundary now simply yields `None` and falls through to the
/// pass-through branch.
pub fn strip_bearer_prefix(value: &str) -> &str {
    let trimmed = value.trim();
    match trimmed.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("bearer ") => trimmed[7..].trim_start(),
        _ => trimmed,
    }
}

/// Optional, opt-in claim checks layered on top of signature verification.
///
/// Every field is inert at its default, so a plain 2-arg
/// `jwt_verify(token, secret)` keeps its historical behaviour: the
/// signature and `exp` are checked, nothing else. Operators turn the
/// extra checks on per deployment through the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyOptions {
    /// Clock-skew tolerance in seconds, widening the window accepted for
    /// both `exp` and `nbf`. Issuer and verifier clocks are rarely in
    /// perfect agreement; without leeway a token minted with
    /// `nbf = now` on a machine a second ahead is rejected on arrival.
    pub leeway_secs: i64,
    /// When set, the token's `iss` claim must be present and equal.
    pub expected_iss: Option<String>,
    /// When set, the token's `aud` claim must be present and must either
    /// equal this value (string form) or contain it (array form).
    pub expected_aud: Option<String>,
}

impl VerifyOptions {
    /// Read the options out of the environment.
    ///
    /// A malformed `JWC_JWT_LEEWAY_SECS` degrades to `0` rather than
    /// failing the verify: `config::validate_or_bail` is the boot fence
    /// that reports the typo, and fail-closed (no leeway) is the safe
    /// direction to land in if one slips past it. Empty strings are
    /// treated as unset so `JWC_JWT_EXPECTED_ISS=` in a compose file
    /// doesn't demand an empty issuer.
    pub fn from_env() -> Self {
        fn non_empty(name: &str) -> Option<String> {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        }
        VerifyOptions {
            leeway_secs: non_empty("JWC_JWT_LEEWAY_SECS")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|n| *n >= 0)
                .unwrap_or(0),
            expected_iss: non_empty("JWC_JWT_EXPECTED_ISS"),
            expected_aud: non_empty("JWC_JWT_EXPECTED_AUD"),
        }
    }
}

/// Process-wide cached copy of [`VerifyOptions::from_env`].
///
/// `jwt_verify` sits on the hot path of every authenticated request, so
/// the env vars are read once and cached — the same treatment
/// `runner::util::check_url_allowlisted` gives `JWC_HTTP_ALLOWLIST`.
/// Tests drive [`verify_hs256_with`] directly instead of mutating the
/// environment, which keeps them independent of execution order.
fn env_verify_options() -> &'static VerifyOptions {
    static OPTS: OnceLock<VerifyOptions> = OnceLock::new();
    OPTS.get_or_init(VerifyOptions::from_env)
}

/// Read a numeric claim (`exp` / `nbf` / `iat`), accepting both integer
/// and float encodings. `Ok(None)` means the claim is absent.
fn numeric_claim(payload: &JsonValue, name: &str) -> Result<Option<i64>> {
    match payload.get(name) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .or_else(|| value.as_f64().map(|f| f as i64))
            .map(Some)
            .ok_or_else(|| anyhow!("jwt_verify: '{name}' claim must be a number")),
    }
}

/// Check the `aud` claim against an expected value. JWTs carry `aud`
/// either as a bare string or as an array of strings (RFC 7519 §4.1.3),
/// and both shapes have to match.
fn audience_matches(aud: &JsonValue, expected: &str) -> bool {
    match aud {
        JsonValue::String(s) => s == expected,
        JsonValue::Array(items) => items
            .iter()
            .any(|item| item.as_str().is_some_and(|s| s == expected)),
        _ => false,
    }
}

/// Verify an HS256 JWT against the given secret. On success returns the decoded
/// payload JSON string. Rejects unsupported algorithms.
///
/// Claim checks beyond `exp` are configured through the environment; see
/// [`VerifyOptions`]. Use [`verify_hs256_with`] to pass them explicitly.
pub fn verify_hs256(token: &str, secret: &str) -> Result<String> {
    verify_hs256_with(token, secret, env_verify_options())
}

/// [`verify_hs256`] with the claim policy supplied by the caller rather
/// than read from the environment.
pub fn verify_hs256_with(token: &str, secret: &str, opts: &VerifyOptions) -> Result<String> {
    // Tolerate a full `Authorization` header value — strip an optional
    // `Bearer ` scheme prefix so callers can pass the header straight through.
    let token = strip_bearer_prefix(token);
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        bail!("jwt_verify: token must have 3 segments");
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| anyhow!("jwt_verify: invalid base64 in header"))?;
    let header: JsonValue = serde_json::from_slice(&header_bytes)
        .map_err(|_| anyhow!("jwt_verify: header is not JSON"))?;

    let alg = header.get("alg").and_then(|v| v.as_str()).unwrap_or("none");
    if alg != "HS256" {
        bail!("jwt_verify: only HS256 is supported, got '{alg}'");
    }

    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let signature = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| anyhow!("jwt_verify: invalid base64 in signature"))?;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| anyhow!("jwt_verify: invalid secret: {e}"))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| anyhow!("jwt_verify: signature mismatch"))?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| anyhow!("jwt_verify: invalid base64 in payload"))?;
    let payload_str = String::from_utf8(payload_bytes)
        .map_err(|_| anyhow!("jwt_verify: payload is not valid UTF-8"))?;
    // Round-trip to normalize JSON formatting.
    let parsed: JsonValue = serde_json::from_str(&payload_str)
        .map_err(|_| anyhow!("jwt_verify: payload is not valid JSON"))?;

    let now = unix_now_secs();
    let leeway = opts.leeway_secs.max(0);

    // Enforce `exp` if present. Absent `exp` is intentionally accepted
    // so non-expiring tokens (long-lived API keys, machine-to-machine)
    // keep working. Present-but-past `exp` surfaces as a classifiable
    // `JwtError.Expired` (see `runner::JWC_ERROR_KINDS`).
    if let Some(exp_secs) = numeric_claim(&parsed, "exp")? {
        if exp_secs.saturating_add(leeway) <= now {
            bail!("jwt_verify: token expired (exp={exp_secs}, now={now})");
        }
    }

    // `nbf` ("not before") went unchecked until now, so a token minted
    // for a future activation window was accepted the moment it was
    // issued. Same shape as `exp`: absent is fine, present is enforced.
    if let Some(nbf_secs) = numeric_claim(&parsed, "nbf")? {
        if nbf_secs.saturating_sub(leeway) > now {
            bail!("jwt_verify: token not yet valid (nbf={nbf_secs}, now={now})");
        }
    }

    // `iat` is parsed so a malformed one is still rejected, but never
    // enforced: a token issued slightly in the future is a clock-skew
    // artefact, not an attack, and `nbf` is the claim that exists to
    // express an activation time.
    let _ = numeric_claim(&parsed, "iat")?;

    if let Some(expected) = opts.expected_iss.as_deref() {
        match parsed.get("iss").and_then(|v| v.as_str()) {
            Some(iss) if iss == expected => {}
            Some(iss) => bail!("jwt_verify: issuer mismatch (expected '{expected}', got '{iss}')"),
            None => bail!("jwt_verify: missing 'iss' claim (expected '{expected}')"),
        }
    }

    if let Some(expected) = opts.expected_aud.as_deref() {
        match parsed.get("aud") {
            Some(aud) if audience_matches(aud, expected) => {}
            Some(_) => bail!("jwt_verify: audience mismatch (expected '{expected}')"),
            None => bail!("jwt_verify: missing 'aud' claim (expected '{expected}')"),
        }
    }

    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrip() {
        let payload = r#"{"sub":"user-1","exp":9999999999}"#;
        let secret = "topsecret";
        let token = sign_hs256(payload, secret).unwrap();
        let decoded = verify_hs256(&token, secret).unwrap();
        let decoded_json: JsonValue = serde_json::from_str(&decoded).unwrap();
        assert_eq!(decoded_json["sub"], "user-1");
    }

    #[test]
    fn verify_rejects_tampered_token() {
        let token = sign_hs256(r#"{"a":1}"#, "k").unwrap();
        let mut bytes = token.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'a' { b'b' } else { b'a' };
        let tampered = String::from_utf8(bytes).unwrap();
        let err = verify_hs256(&tampered, "k").unwrap_err().to_string();
        assert!(err.contains("signature mismatch") || err.contains("invalid base64"));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let token = sign_hs256(r#"{"a":1}"#, "right").unwrap();
        let err = verify_hs256(&token, "wrong").unwrap_err().to_string();
        assert!(err.contains("signature mismatch"));
    }

    #[test]
    fn verify_rejects_non_hs256_algorithm() {
        // Hand-craft a token with alg=none
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(r#"{"a":1}"#);
        let bad = format!("{header}.{payload}.");
        let err = verify_hs256(&bad, "k").unwrap_err().to_string();
        assert!(err.contains("HS256"));
    }

    #[test]
    fn jwt_verify_accepts_token_without_exp() {
        // No `exp` claim — long-lived API key shape. Must verify clean.
        let token = sign_hs256(r#"{"sub":"svc"}"#, "k").unwrap();
        let decoded = verify_hs256(&token, "k").unwrap();
        let decoded_json: JsonValue = serde_json::from_str(&decoded).unwrap();
        assert_eq!(decoded_json["sub"], "svc");
    }

    #[test]
    fn jwt_verify_rejects_expired_token() {
        // `exp` in the past — must reject with a classifiable message.
        let token = sign_hs256(r#"{"sub":"u","exp":1}"#, "k").unwrap();
        let err = verify_hs256(&token, "k").unwrap_err().to_string();
        assert!(
            err.contains("expired"),
            "expected 'expired' in error message, got: {err}"
        );
    }

    #[test]
    fn jwt_verify_accepts_future_exp() {
        // `exp` far in the future — must verify clean.
        let token = sign_hs256(r#"{"sub":"u","exp":9999999999}"#, "k").unwrap();
        let decoded = verify_hs256(&token, "k").unwrap();
        let decoded_json: JsonValue = serde_json::from_str(&decoded).unwrap();
        assert_eq!(decoded_json["sub"], "u");
    }

    #[test]
    fn strip_bearer_prefix_handles_variants() {
        assert_eq!(strip_bearer_prefix("Bearer abc.def.ghi"), "abc.def.ghi");
        assert_eq!(strip_bearer_prefix("bearer abc.def.ghi"), "abc.def.ghi");
        assert_eq!(strip_bearer_prefix("  Bearer   abc  "), "abc");
        // A raw token (no scheme) passes through untouched.
        assert_eq!(strip_bearer_prefix("abc.def.ghi"), "abc.def.ghi");
    }

    #[test]
    fn jwt_verify_accepts_authorization_header_with_bearer() {
        // The canonical middleware passes `header("authorization")` straight in;
        // verify must tolerate the `Bearer ` scheme prefix.
        let token = sign_hs256(r#"{"sub":"u"}"#, "k").unwrap();
        let decoded = verify_hs256(&format!("Bearer {token}"), "k").unwrap();
        let decoded_json: JsonValue = serde_json::from_str(&decoded).unwrap();
        assert_eq!(decoded_json["sub"], "u");
    }

    /// Regression: `strip_bearer_prefix` used to slice `trimmed[..7]`, which
    /// panics when byte 7 lands mid-codepoint. The `Authorization` header is
    /// attacker-controlled, so that was a remote panic on an unauthenticated
    /// path. Every one of these must return rather than unwind.
    #[test]
    fn strip_bearer_prefix_survives_multibyte_headers() {
        for header in [
            "Кириллица",      // 2-byte codepoints — boundary lands mid-char
            "日本語テキスト", // 3-byte codepoints
            "🔐🔐",           // 4-byte codepoints
            "Bearer🔐token",  // ASCII prefix, multibyte at the split
            "Bearе r abc",    // Cyrillic 'е' inside the scheme word
            "abcdé",          // shorter than 7 bytes once trimmed
            "",
        ] {
            let out = strip_bearer_prefix(header);
            assert!(
                !out.is_empty() || header.trim().is_empty(),
                "unexpected empty result for {header:?}"
            );
        }
    }

    #[test]
    fn jwt_verify_does_not_panic_on_multibyte_authorization_header() {
        // End-to-end shape of the regression: the value reaches `verify_hs256`
        // exactly as the middleware read it off the wire.
        let err = verify_hs256("Кириллица.payload.sig", "k")
            .unwrap_err()
            .to_string();
        assert!(err.contains("jwt_verify"), "got: {err}");
    }

    #[test]
    fn jwt_verify_rejects_token_before_nbf() {
        let token = sign_hs256(r#"{"sub":"u","nbf":9999999999}"#, "k").unwrap();
        let err = verify_hs256(&token, "k").unwrap_err().to_string();
        assert!(err.contains("not yet valid"), "got: {err}");
    }

    #[test]
    fn jwt_verify_accepts_token_after_nbf() {
        let token = sign_hs256(r#"{"sub":"u","nbf":1}"#, "k").unwrap();
        assert!(verify_hs256(&token, "k").is_ok());
    }

    #[test]
    fn jwt_verify_leeway_widens_nbf_window() {
        // `nbf` 30s in the future: rejected with no leeway, accepted with 60s.
        let nbf = unix_now_secs() + 30;
        let token = sign_hs256(&format!(r#"{{"sub":"u","nbf":{nbf}}}"#), "k").unwrap();

        let strict = VerifyOptions::default();
        assert!(verify_hs256_with(&token, "k", &strict).is_err());

        let lenient = VerifyOptions {
            leeway_secs: 60,
            ..VerifyOptions::default()
        };
        assert!(verify_hs256_with(&token, "k", &lenient).is_ok());
    }

    #[test]
    fn jwt_verify_leeway_widens_exp_window() {
        // `exp` 30s in the past: expired strictly, still inside a 60s leeway.
        let exp = unix_now_secs() - 30;
        let token = sign_hs256(&format!(r#"{{"sub":"u","exp":{exp}}}"#), "k").unwrap();

        let strict = VerifyOptions::default();
        let err = verify_hs256_with(&token, "k", &strict)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expired"), "got: {err}");

        let lenient = VerifyOptions {
            leeway_secs: 60,
            ..VerifyOptions::default()
        };
        assert!(verify_hs256_with(&token, "k", &lenient).is_ok());
    }

    #[test]
    fn jwt_verify_rejects_non_numeric_nbf() {
        let token = sign_hs256(r#"{"sub":"u","nbf":"soon"}"#, "k").unwrap();
        let err = verify_hs256(&token, "k").unwrap_err().to_string();
        assert!(err.contains("'nbf' claim must be a number"), "got: {err}");
    }

    #[test]
    fn jwt_verify_rejects_non_numeric_iat() {
        // `iat` is never enforced, but a malformed one is still a malformed token.
        let token = sign_hs256(r#"{"sub":"u","iat":"yesterday"}"#, "k").unwrap();
        let err = verify_hs256(&token, "k").unwrap_err().to_string();
        assert!(err.contains("'iat' claim must be a number"), "got: {err}");
    }

    #[test]
    fn jwt_verify_ignores_iat_in_the_future() {
        let iat = unix_now_secs() + 3600;
        let token = sign_hs256(&format!(r#"{{"sub":"u","iat":{iat}}}"#), "k").unwrap();
        assert!(verify_hs256(&token, "k").is_ok());
    }

    fn iss_opts(iss: &str) -> VerifyOptions {
        VerifyOptions {
            expected_iss: Some(iss.to_string()),
            ..VerifyOptions::default()
        }
    }

    fn aud_opts(aud: &str) -> VerifyOptions {
        VerifyOptions {
            expected_aud: Some(aud.to_string()),
            ..VerifyOptions::default()
        }
    }

    #[test]
    fn jwt_verify_accepts_matching_issuer() {
        let token = sign_hs256(r#"{"sub":"u","iss":"https://auth.example"}"#, "k").unwrap();
        assert!(verify_hs256_with(&token, "k", &iss_opts("https://auth.example")).is_ok());
    }

    #[test]
    fn jwt_verify_rejects_wrong_issuer() {
        let token = sign_hs256(r#"{"sub":"u","iss":"https://evil.example"}"#, "k").unwrap();
        let err = verify_hs256_with(&token, "k", &iss_opts("https://auth.example"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("issuer mismatch"), "got: {err}");
    }

    #[test]
    fn jwt_verify_rejects_missing_issuer_when_expected() {
        let token = sign_hs256(r#"{"sub":"u"}"#, "k").unwrap();
        let err = verify_hs256_with(&token, "k", &iss_opts("https://auth.example"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing 'iss'"), "got: {err}");
    }

    #[test]
    fn jwt_verify_accepts_string_audience() {
        let token = sign_hs256(r#"{"sub":"u","aud":"api"}"#, "k").unwrap();
        assert!(verify_hs256_with(&token, "k", &aud_opts("api")).is_ok());
    }

    #[test]
    fn jwt_verify_accepts_audience_array_containing_expected() {
        // RFC 7519 §4.1.3 allows `aud` to be an array; both shapes must match.
        let token = sign_hs256(r#"{"sub":"u","aud":["web","api","admin"]}"#, "k").unwrap();
        assert!(verify_hs256_with(&token, "k", &aud_opts("api")).is_ok());
    }

    #[test]
    fn jwt_verify_rejects_audience_array_without_expected() {
        let token = sign_hs256(r#"{"sub":"u","aud":["web","admin"]}"#, "k").unwrap();
        let err = verify_hs256_with(&token, "k", &aud_opts("api"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("audience mismatch"), "got: {err}");
    }

    #[test]
    fn jwt_verify_rejects_missing_audience_when_expected() {
        let token = sign_hs256(r#"{"sub":"u"}"#, "k").unwrap();
        let err = verify_hs256_with(&token, "k", &aud_opts("api"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing 'aud'"), "got: {err}");
    }

    #[test]
    fn jwt_verify_ignores_iss_and_aud_when_unconfigured() {
        // The default policy is fully inert — a 2-arg `jwt_verify` behaves
        // exactly as it did before any of this landed.
        let token = sign_hs256(r#"{"sub":"u","iss":"whoever","aud":["anything"]}"#, "k").unwrap();
        assert!(verify_hs256_with(&token, "k", &VerifyOptions::default()).is_ok());
    }

    #[test]
    fn audience_matches_rejects_non_string_shapes() {
        assert!(!audience_matches(&serde_json::json!(42), "api"));
        assert!(!audience_matches(&serde_json::json!({"a": "api"}), "api"));
        assert!(!audience_matches(&serde_json::json!([1, 2]), "api"));
        assert!(audience_matches(&serde_json::json!(["api"]), "api"));
    }
}

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
pub fn strip_bearer_prefix(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
        trimmed[7..].trim_start()
    } else {
        trimmed
    }
}

/// Verify an HS256 JWT against the given secret. On success returns the decoded
/// payload JSON string. Rejects unsupported algorithms.
pub fn verify_hs256(token: &str, secret: &str) -> Result<String> {
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

    // Enforce `exp` if present. Absent `exp` is intentionally accepted
    // so non-expiring tokens (long-lived API keys, machine-to-machine)
    // keep working. Present-but-past `exp` surfaces as a classifiable
    // `JwtError.Expired` (see `runner::JWC_ERROR_KINDS`).
    if let Some(exp_value) = parsed.get("exp") {
        let exp_secs = exp_value
            .as_i64()
            .or_else(|| exp_value.as_f64().map(|f| f as i64))
            .ok_or_else(|| anyhow!("jwt_verify: 'exp' claim must be a number"))?;
        let now = unix_now_secs();
        if exp_secs <= now {
            bail!("jwt_verify: token expired (exp={exp_secs}, now={now})");
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
}

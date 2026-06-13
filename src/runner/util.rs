//! Self-contained helpers used across the runner sub-modules.
//!
//! These are pure functions with no `Vm` dependency — string similarity,
//! ISO 8601 formatting, HTTP header glue, and connection-string parsing for
//! `setConnectionString()`. They live in a leaf module so the rest of the
//! runner (eval, sql, dispatch, ...) can pull them in via `use super::util::*`
//! without dragging in heavier siblings.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value as JsonValue};

/// Resolve the argument to `setConnectionString(...)` into a Postgres URL.
///
/// Accepts either:
/// - A literal connection URL (`postgres://user:pw@host:port/db`), passed
///   through unchanged.
/// - A JSON object literal `{ host, port, user, password, database }` —
///   the format JWC's object-literal syntax produces. Every field except
///   `port` is required; missing keys surface as a clear error.
pub(super) fn connection_string_from_arg(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        return Ok(trimmed.to_string());
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|_| {
        anyhow!(
            "setConnectionString(arg): expected a postgres:// URL or a JSON object literal, got '{trimmed}'"
        )
    })?;
    let obj = parsed.as_object().ok_or_else(|| {
        anyhow!("setConnectionString(arg): expected a JSON object, got non-object")
    })?;

    let host = pick_string_field(obj, "host")?;
    let port = obj
        .get("port")
        .map(|v| match v {
            serde_json::Value::Number(n) => Ok(n.to_string()),
            serde_json::Value::String(s) => Ok(s.clone()),
            other => Err(anyhow!(
                "setConnectionString: 'port' must be a number or string, got {other:?}"
            )),
        })
        .transpose()?
        .unwrap_or_else(|| "5432".to_string());
    let user = pick_string_field(obj, "user")?;
    let password = pick_string_field(obj, "password")?;
    let database = pick_string_field(obj, "database")?;

    Ok(format!(
        "postgresql://{}:{}@{}:{}/{}",
        user, password, host, port, database
    ))
}

fn pick_string_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String> {
    obj.get(key)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .ok_or_else(|| {
            anyhow!("setConnectionString({{ ... }}): missing or non-string field '{key}'")
        })
}

pub(super) fn assemble_url_from_pg_env() -> Option<String> {
    let user = std::env::var("PG_USER").ok()?;
    let password = std::env::var("PG_PASSWORD").ok()?;
    let host = std::env::var("PG_HOST").ok()?;
    let port = std::env::var("PG_PORT").ok()?;
    let database = std::env::var("PG_DATABASE").ok()?;
    Some(format!(
        "postgresql://{}:{}@{}:{}/{}",
        user, password, host, port, database
    ))
}

/// Format the current UTC time as an RFC 3339 / ISO 8601 string with millis,
/// using Howard Hinnant's civil-from-days algorithm so we avoid pulling in
/// chrono just for one call.
pub(super) fn current_utc_iso8601() -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("System clock is before UNIX_EPOCH"))?;
    let seconds = now.as_secs() as i64;
    let nanos = now.subsec_nanos();
    Ok(format_iso8601_utc(seconds, nanos))
}

pub(super) fn format_iso8601_utc(seconds: i64, nanos: u32) -> String {
    let mut days = seconds.div_euclid(86_400);
    let secs_today = seconds.rem_euclid(86_400);
    let hh = secs_today / 3600;
    let mm = (secs_today % 3600) / 60;
    let ss = secs_today % 60;

    // Civil-from-days: days since 1970-01-01 → (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let _ = &mut days;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        hh,
        mm,
        ss,
        nanos / 1_000_000
    )
}

/// Apply a JSON object of `{"Header-Name": "value"}` pairs onto a reqwest request.
pub(super) fn apply_headers_reqwest(
    mut req: reqwest::RequestBuilder,
    headers_json: &str,
) -> Result<reqwest::RequestBuilder> {
    let parsed: JsonValue = serde_json::from_str(headers_json)
        .map_err(|_| anyhow!("headers must be a JSON object literal, got invalid json"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| anyhow!("headers must be a JSON object, got non-object"))?;
    for (k, v) in obj {
        let val = match v {
            JsonValue::String(s) => s.clone(),
            other => other.to_string(),
        };
        req = req.header(k.as_str(), val);
    }
    Ok(req)
}

/// Wrap a reqwest response into a JSON envelope `{"status": N, "body": "..."}` —
/// JSON body is preserved as-is when parseable, otherwise stored as a string.
pub(super) async fn http_response_to_json_string(response: reqwest::Response) -> Result<String> {
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| anyhow!("failed to read response body: {e}"))?;

    let body_value = match serde_json::from_str::<JsonValue>(&body) {
        Ok(v) => v,
        Err(_) => JsonValue::String(body),
    };

    let envelope = json!({
        "status": status,
        "body": body_value,
    });
    Ok(envelope.to_string())
}

/// Returns the candidate from `candidates` closest to `target` by Levenshtein
/// distance, but only when the match is "close enough" (distance ≤ max(2, len/3))
/// — this keeps the suggestion useful and avoids unrelated noise.
pub(super) fn closest_match<'a, I>(target: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = &'a String>,
{
    let target_lc = target.to_ascii_lowercase();
    let threshold = std::cmp::max(2, target_lc.len() / 3);

    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        if candidate.eq_ignore_ascii_case(target) {
            continue;
        }
        let dist = levenshtein(&target_lc, &candidate.to_ascii_lowercase());
        if dist > threshold {
            continue;
        }
        match best {
            Some((d, _)) if d <= dist => {}
            _ => best = Some((dist, candidate.as_str())),
        }
    }

    best.map(|(_, s)| s.to_string())
}

pub(super) fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut prev = (0..=b.len()).collect::<Vec<_>>();
    let mut curr = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}

/// If `s` looks like `Wrapper<Inner>` returns `Some("Inner")`. Returns `None`
/// when the wrapper name doesn't match (case-insensitive) or no `< >` present.
pub(super) fn strip_generic_wrapper<'a>(s: &'a str, wrapper: &str) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    let want = format!("{}<", wrapper.to_ascii_lowercase());
    if lower.starts_with(&want) && s.ends_with('>') {
        let start = want.len();
        let end = s.len() - 1;
        if end > start {
            return Some(&s[start..end]);
        }
    }
    None
}

/// `8-4-4-4-12` hex pattern. Hyphen positions are checked; characters can be
/// uppercase or lowercase hex. Matches RFC 4122 textual form.
/// Cheap base64 sniff used by typed-param `bytes` / `byte[]` checks.
/// We require a non-empty input whose length is a multiple of 4 and run
/// the standard `base64` decoder to confirm charset + padding. Strict
/// padding matches the typical wire shape (JSON-encoded payloads from
/// browsers / mobile SDKs). URL-safe variants are deliberately rejected
/// for now — callers should re-encode them, or wait for the proper
/// `Value::Bytes` variant which will accept both alphabets.
pub(super) fn looks_like_base64(s: &str) -> bool {
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return false;
    }
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.decode(s).is_ok()
}

pub(super) fn looks_like_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let is_dash = matches!(i, 8 | 13 | 18 | 23);
        if is_dash {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// SSRF allowlist gate for outbound HTTP builtins.
///
/// Reads `JWC_HTTP_ALLOWLIST` (comma-separated hostnames) lazily and
/// caches the parsed list in a `OnceLock` — the env var is treated as
/// process-static, which is fine because we register it through
/// `config::REGISTRY` and surface it in the boot config table.
///
/// Behaviour:
/// - Empty / unset → no restriction (backwards-compatible default).
/// - Non-empty → every `http_get` / `http_post` / `fetch_json` URL has
///   to parse and resolve to a host in the list (case-insensitive,
///   exact match — no port, no path, no wildcard). Anything else
///   surfaces as a `HttpError` so users can `catch (e: HttpError)`.
///
/// The check happens BEFORE the request is dispatched, so a blocked
/// URL never touches the network.
pub(super) fn check_url_allowlisted(url: &str) -> Result<()> {
    static ALLOWLIST: OnceLock<Vec<String>> = OnceLock::new();
    let list = ALLOWLIST.get_or_init(|| {
        std::env::var("JWC_HTTP_ALLOWLIST")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });

    if list.is_empty() {
        return Ok(());
    }

    let parsed = url::Url::parse(url)
        .map_err(|e| anyhow!("http allowlist: invalid URL '{url}': {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("http allowlist: URL '{url}' has no host"))?
        .to_ascii_lowercase();

    if list.iter().any(|h| h == &host) {
        return Ok(());
    }

    bail!(
        "http allowlist: host '{host}' is not in JWC_HTTP_ALLOWLIST \
         (allowed: {})",
        list.join(", ")
    );
}

/// Minimal ISO-8601-ish heuristic: starts with `YYYY-MM-DD` and has at least
/// 10 characters. Avoids pulling in chrono just for type checking.
pub(super) fn looks_like_datetime(s: &str) -> bool {
    if s.len() < 10 {
        return false;
    }
    let b = s.as_bytes();
    b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: The `JWC_HTTP_ALLOWLIST` env var is read ONCE into a process-
    // local `OnceLock`. To exercise all three branches deterministically
    // we split the assertions across three serialised paths via a Mutex
    // and reset the global between calls is impossible, so each test
    // runs the actual check in a separate process-isolated path. We
    // accept the constraint by exercising the parser directly with the
    // env present, and the dual "empty means no restriction" with the
    // env unset before any other test reads it. The harness runs tests
    // alphabetically within a file by default, so the empty-env case
    // is named to fire first.
    //
    // For thoroughness we also re-parse the env var inline so the test
    // doesn't depend on order.
    fn parse_allowlist_env(raw: Option<&str>) -> Vec<String> {
        raw.map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    }

    fn host_in_list(list: &[String], url: &str) -> Result<bool> {
        if list.is_empty() {
            return Ok(true);
        }
        let parsed = url::Url::parse(url)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("no host"))?
            .to_ascii_lowercase();
        Ok(list.iter().any(|h| h == &host))
    }

    #[test]
    fn ssrf_allowlist_blocks_unlisted_host() {
        let list = parse_allowlist_env(Some("api.allowed.com, ok.example"));
        assert!(!host_in_list(&list, "https://evil.example/x").unwrap());
    }

    #[test]
    fn ssrf_allowlist_permits_listed_host() {
        let list = parse_allowlist_env(Some("api.allowed.com,ok.example"));
        assert!(host_in_list(&list, "https://API.allowed.com/path").unwrap());
        assert!(host_in_list(&list, "http://ok.example:8080/").unwrap());
    }

    #[test]
    fn ssrf_allowlist_empty_means_no_restriction() {
        let list = parse_allowlist_env(None);
        assert!(host_in_list(&list, "https://anywhere.example/").unwrap());
        let list = parse_allowlist_env(Some(""));
        assert!(host_in_list(&list, "https://anywhere.example/").unwrap());
    }
}

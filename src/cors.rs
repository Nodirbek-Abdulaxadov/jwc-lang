//! CORS for the HTTP runtime.
//!
//! There was none: an `OPTIONS` preflight fell through to the route table and
//! came back `404`, and no response ever carried an `Access-Control-*` header.
//! That makes a browser frontend on a different origin impossible without
//! putting a reverse proxy in front of the server — in development *and* in
//! production.
//!
//! Off by default. A server that starts answering cross-origin requests
//! because it was upgraded would be a security regression, so CORS turns on
//! only when `JWC_CORS_ORIGINS` is set.
//!
//! ```text
//! JWC_CORS_ORIGINS=http://localhost:4200,https://app.example.com
//! JWC_CORS_ORIGINS=*                     # any origin (no credentials)
//! JWC_CORS_CREDENTIALS=1                 # allow cookies / Authorization
//! JWC_CORS_METHODS=GET,POST,PUT,DELETE   # default covers the usual verbs
//! JWC_CORS_HEADERS=content-type,authorization
//! JWC_CORS_EXPOSE_HEADERS=x-request-id
//! JWC_CORS_MAX_AGE=86400                 # preflight cache, seconds
//! ```

use std::sync::Arc;

use anyhow::{bail, Result};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

const DEFAULT_METHODS: &str = "GET,POST,PUT,PATCH,DELETE,OPTIONS";
const DEFAULT_HEADERS: &str = "content-type,authorization";
const DEFAULT_EXPOSE: &str = "x-request-id";
const DEFAULT_MAX_AGE: &str = "86400";

#[derive(Debug, Clone, Default)]
pub struct CorsConfig {
    /// `JWC_CORS_ORIGINS` was set to something non-empty.
    pub enabled: bool,
    /// `JWC_CORS_ORIGINS=*` — reflect any origin.
    pub any_origin: bool,
    /// Exact origins to match, compared case-insensitively.
    pub origins: Vec<String>,
    pub methods: String,
    pub headers: String,
    pub expose: String,
    pub credentials: bool,
    pub max_age: String,
}

impl CorsConfig {
    pub fn from_env() -> Self {
        let raw = env_or("JWC_CORS_ORIGINS", "");
        let raw = raw.trim();
        if raw.is_empty() {
            return Self::default();
        }
        let any_origin = raw == "*";
        let origins = if any_origin {
            Vec::new()
        } else {
            raw.split(',')
                .map(|o| o.trim().trim_end_matches('/').to_ascii_lowercase())
                .filter(|o| !o.is_empty())
                .collect()
        };

        Self {
            enabled: true,
            any_origin,
            origins,
            methods: env_or("JWC_CORS_METHODS", DEFAULT_METHODS),
            headers: env_or("JWC_CORS_HEADERS", DEFAULT_HEADERS),
            expose: env_or("JWC_CORS_EXPOSE_HEADERS", DEFAULT_EXPOSE),
            credentials: env_bool("JWC_CORS_CREDENTIALS"),
            max_age: env_or("JWC_CORS_MAX_AGE", DEFAULT_MAX_AGE),
        }
    }

    /// Reject configurations the browser would silently ignore, at boot,
    /// rather than leaving the user to debug a preflight that "does nothing".
    pub fn validate(&self) -> Result<()> {
        if self.enabled && self.any_origin && self.credentials {
            bail!(
                "JWC_CORS_ORIGINS=* cannot be combined with JWC_CORS_CREDENTIALS=1 — \
                 browsers reject `Access-Control-Allow-Origin: *` on a credentialed \
                 request. List the exact origins instead."
            );
        }
        Ok(())
    }

    /// The value to echo in `Access-Control-Allow-Origin`, or `None` when this
    /// origin isn't allowed (the response then carries no CORS headers and the
    /// browser blocks it, which is the correct outcome).
    fn allow_origin(&self, origin: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        if self.any_origin {
            // With credentials off, `*` is the simplest correct answer.
            return Some("*".to_string());
        }
        // Normalise both sides here rather than only on the way in from env,
        // so the match rule holds however the config was constructed.
        let probe = normalize_origin(origin);
        self.origins
            .iter()
            .any(|o| normalize_origin(o) == probe)
            .then(|| origin.to_string())
    }
}

/// Origins compare case-insensitively, and a trailing slash — which browsers
/// never send but users do write in config — is ignored.
fn normalize_origin(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

fn env_bool(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn set(resp: &mut Response, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        resp.headers_mut().insert(HeaderName::from_static(name), v);
    }
}

/// Axum middleware. Answers preflights itself and decorates every other
/// response.
pub async fn middleware(cfg: Arc<CorsConfig>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // A request with no `Origin` isn't cross-origin; leave it untouched.
    let Some(origin) = origin else {
        return next.run(req).await;
    };
    let allow = cfg.allow_origin(&origin);

    // Preflight: answer here, because the route table has no OPTIONS entry
    // and would otherwise 404 the browser's check.
    let is_preflight = req.method() == Method::OPTIONS
        && req.headers().contains_key("access-control-request-method");
    if is_preflight {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::NO_CONTENT;
        if let Some(allow) = &allow {
            decorate(&mut resp, &cfg, allow);
            set(&mut resp, "access-control-allow-methods", &cfg.methods);
            set(&mut resp, "access-control-allow-headers", &cfg.headers);
            set(&mut resp, "access-control-max-age", &cfg.max_age);
        }
        return resp;
    }

    let mut resp = next.run(req).await;
    if let Some(allow) = &allow {
        decorate(&mut resp, &cfg, allow);
        if !cfg.expose.is_empty() {
            set(&mut resp, "access-control-expose-headers", &cfg.expose);
        }
    }
    resp
}

fn decorate(resp: &mut Response, cfg: &CorsConfig, allow_origin: &str) {
    set(resp, "access-control-allow-origin", allow_origin);
    if cfg.credentials {
        set(resp, "access-control-allow-credentials", "true");
    }
    if allow_origin != "*" {
        // The response body varies by Origin, so caches must key on it.
        set(resp, "vary", "Origin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(origins: &[&str], any: bool, creds: bool) -> CorsConfig {
        CorsConfig {
            enabled: true,
            any_origin: any,
            origins: origins.iter().map(|o| o.to_string()).collect(),
            methods: DEFAULT_METHODS.into(),
            headers: DEFAULT_HEADERS.into(),
            expose: DEFAULT_EXPOSE.into(),
            credentials: creds,
            max_age: DEFAULT_MAX_AGE.into(),
        }
    }

    #[test]
    fn disabled_by_default_so_an_upgrade_cannot_open_a_server_up() {
        let c = CorsConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.allow_origin("http://localhost:4200"), None);
    }

    #[test]
    fn listed_origin_is_echoed_back() {
        let c = cfg(&["http://localhost:4200"], false, false);
        assert_eq!(
            c.allow_origin("http://localhost:4200").as_deref(),
            Some("http://localhost:4200")
        );
    }

    #[test]
    fn unlisted_origin_gets_no_header() {
        let c = cfg(&["http://localhost:4200"], false, false);
        assert_eq!(c.allow_origin("https://evil.example"), None);
    }

    /// Origins are compared case-insensitively and a trailing slash — which
    /// browsers never send but users do write in config — is tolerated.
    #[test]
    fn origin_match_is_case_and_trailing_slash_insensitive() {
        let c = cfg(&["http://localhost:4200"], false, false);
        assert!(c.allow_origin("http://LOCALHOST:4200").is_some());
        let c = cfg(&["http://localhost:4200/"], false, false);
        assert!(c.allow_origin("http://localhost:4200").is_some());
    }

    #[test]
    fn wildcard_allows_anything() {
        let c = cfg(&[], true, false);
        assert_eq!(
            c.allow_origin("https://anywhere.example").as_deref(),
            Some("*")
        );
    }

    /// The browser ignores `*` on a credentialed request, so a config that
    /// asks for both is a silent no-op. Fail at boot instead.
    #[test]
    fn wildcard_with_credentials_is_rejected_at_boot() {
        assert!(cfg(&[], true, true).validate().is_err());
        assert!(cfg(&["http://a.example"], false, true).validate().is_ok());
        assert!(cfg(&[], true, false).validate().is_ok());
    }
}

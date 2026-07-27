//! Sprint 5A — boot-time env var catalog.
//!
//! Single registry of every `JWC_*` env var the runtime reads, with its
//! parser, default, and one-line doc. The server boot path walks the
//! registry, prints a rendered table, and fails fast if a known numeric
//! var was set to a non-numeric string. Lets an operator confirm at a
//! glance exactly what configuration the process is about to run with —
//! and which values came from the environment vs. baked-in defaults.
//!
//! Stdlib-only. No new deps.
//!
//! Redaction policy (see [`render`]): a row's value is replaced with
//! `*** (redacted)` when the var name contains any of `PASSWORD`,
//! `SECRET`, `TOKEN`, `KEY`, `JWT`, or `DATABASE_URL`. Match is on the
//! *name* (case-insensitive substring) — the parsed value is preserved
//! in [`RenderedEnvVar::parsed_raw`] so callers that legitimately need
//! the value (e.g. building a connection string) can still read it; only
//! the rendered string is masked.
//!
//! The registry intentionally does NOT include `DATABASE_URL` (no
//! `JWC_` prefix) — that one is documented separately and never echoed
//! to logs.

use anyhow::{anyhow, Result};
use std::fmt::Write as _;

/// What kind of value a registered env var holds. Drives parsing and
/// the canonical rendered form (e.g. `"30 seconds"` for a duration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseKind {
    /// Free-form string; no parse step beyond the read.
    Str,
    /// Truthy set: `1`, `true`, `yes`, `on` (case-insensitive).
    Bool,
    /// `u16` numeric. Used for ports.
    U16,
    /// `u32` numeric.
    U32,
    /// `u64` numeric. Generic counter / size in bytes.
    U64,
    /// `usize` numeric.
    Usize,
    /// `u64` seconds — rendered as `"<n> seconds"`.
    DurationSecs,
    /// `u64` milliseconds — rendered as `"<n> ms"`.
    DurationMs,
    /// Comma-separated list, trimmed, empties dropped.
    CsvList,
    /// Free-form enum (e.g. `text` | `json`). No structural check here;
    /// the consuming module owns the value set.
    Enum,
}

/// One row in the registry. Constant — describes the variable, not the
/// runtime value.
#[derive(Debug, Clone, Copy)]
pub struct EnvVar {
    pub name: &'static str,
    pub parse_kind: ParseKind,
    /// Canonical default shown in the table when the var is unset.
    /// `""` means "no default — empty / disabled".
    pub default: &'static str,
    pub doc: &'static str,
}

/// Where the runtime got the value for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Value came from `std::env` at snapshot time.
    Env,
    /// Var was unset; the printed value is the baked-in default.
    Default,
}

impl Source {
    fn as_str(&self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Default => "default",
        }
    }
}

/// Runtime snapshot of one registered env var. `parsed` holds the
/// canonical printable form; `parsed_raw` holds the unmasked value so
/// the redaction in [`render`] is purely a display concern.
#[derive(Debug, Clone)]
pub struct RenderedEnvVar {
    pub name: &'static str,
    pub source: Source,
    pub raw: String,
    pub parsed: String,
    /// Same as `parsed`, but never masked. Internal — used so tests can
    /// assert the value survived the redaction pass.
    pub parsed_raw: String,
    pub error: Option<String>,
}

/// The single source of truth for every `JWC_*` env var the runtime
/// reads. Adding a new one? Add it here AND in the matching module —
/// `validate_or_bail` is the boot fence that catches typos in numeric
/// values, so registering it here is what enables that protection.
pub const REGISTRY: &[EnvVar] = &[
    // --- Database / engine -------------------------------------------------
    EnvVar {
        name: "JWC_DATABASE_URL",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Postgres connection string (overrides DATABASE_URL).",
    },
    EnvVar {
        name: "JWC_DB_POOL_SIZE",
        parse_kind: ParseKind::Usize,
        default: "64",
        doc: "Max connections in the deadpool-postgres pool.",
    },
    EnvVar {
        name: "JWC_DB_TLS",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "Connect to Postgres over TLS via tokio-postgres-rustls.",
    },
    EnvVar {
        name: "JWC_DB_TLS_INSECURE_SKIP_VERIFY",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "Skip cert verification (dev only — never set in prod).",
    },
    EnvVar {
        name: "JWC_QUERY_CACHE_TTL_SECS",
        parse_kind: ParseKind::DurationSecs,
        default: "0",
        doc: "Result-cache TTL; 0 disables caching.",
    },
    EnvVar {
        name: "JWC_DB_RETRY_MAX_ATTEMPTS",
        parse_kind: ParseKind::U32,
        default: "3",
        doc: "Transient-error retry ceiling (outside transactions).",
    },
    EnvVar {
        name: "JWC_DB_RETRY_BACKOFF_MS",
        parse_kind: ParseKind::U32,
        default: "100",
        doc: "Base retry backoff (ms); doubles each attempt.",
    },
    EnvVar {
        name: "JWC_ADMIN_DB",
        parse_kind: ParseKind::Str,
        default: "postgres",
        doc: "Admin DB used by `migrate` to create the target DB.",
    },
    // --- Server ------------------------------------------------------------
    EnvVar {
        name: "JWC_SERVER_WORKERS",
        parse_kind: ParseKind::Usize,
        default: "0",
        doc: "Tokio worker threads; 0 = available_parallelism().",
    },
    EnvVar {
        name: "JWC_SERVER_METRICS",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "Periodically log in-flight / completed / failed counters.",
    },
    EnvVar {
        name: "JWC_SERVER_METRICS_INTERVAL_SECS",
        parse_kind: ParseKind::DurationSecs,
        default: "10",
        doc: "Metrics log cadence.",
    },
    EnvVar {
        name: "JWC_LOG_FORMAT",
        parse_kind: ParseKind::Enum,
        default: "text",
        doc: "Access-log shape: `text` or `json`.",
    },
    EnvVar {
        name: "JWC_REQUEST_TIMEOUT",
        parse_kind: ParseKind::DurationSecs,
        default: "30",
        doc: "Per-request watchdog; 0 disables the cap.",
    },
    EnvVar {
        name: "JWC_MAX_BODY_BYTES",
        parse_kind: ParseKind::Usize,
        default: "2097152",
        doc: "Request body cap (bytes); 0 disables.",
    },
    EnvVar {
        name: "JWC_SHUTDOWN_TIMEOUT",
        parse_kind: ParseKind::DurationSecs,
        default: "5",
        doc: "Graceful shutdown budget before force-exit.",
    },
    EnvVar {
        name: "JWC_CORS_ORIGINS",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Comma-separated allowed origins, or `*`. Empty disables CORS.",
    },
    EnvVar {
        name: "JWC_CORS_METHODS",
        parse_kind: ParseKind::Str,
        default: "GET,POST,PUT,PATCH,DELETE,OPTIONS",
        doc: "Methods echoed in the preflight response.",
    },
    EnvVar {
        name: "JWC_CORS_HEADERS",
        parse_kind: ParseKind::Str,
        default: "content-type,authorization",
        doc: "Request headers the browser may send cross-origin.",
    },
    EnvVar {
        name: "JWC_CORS_EXPOSE_HEADERS",
        parse_kind: ParseKind::Str,
        default: "x-request-id",
        doc: "Response headers readable by cross-origin JS.",
    },
    EnvVar {
        name: "JWC_CORS_CREDENTIALS",
        parse_kind: ParseKind::Bool,
        default: "0",
        doc: "Allow cookies / Authorization cross-origin. Incompatible with `*`.",
    },
    EnvVar {
        name: "JWC_CORS_MAX_AGE",
        parse_kind: ParseKind::U64,
        default: "86400",
        doc: "Seconds a browser may cache the preflight result.",
    },
    EnvVar {
        name: "JWC_REAL_IP_HEADER",
        parse_kind: ParseKind::Str,
        default: "x-forwarded-for",
        doc: "Header name parsed by the request_ip() builtin.",
    },
    EnvVar {
        name: "JWC_TRUSTED_PROXIES",
        parse_kind: ParseKind::CsvList,
        default: "",
        doc: "Comma-separated IPs/prefixes peeled off X-F-F.",
    },
    EnvVar {
        name: "JWC_PRINT_CONFIG",
        parse_kind: ParseKind::Bool,
        default: "true",
        doc: "Print this config table at server boot; set off to suppress.",
    },
    // --- Queue -------------------------------------------------------------
    EnvVar {
        name: "JWC_QUEUE_WORKERS",
        parse_kind: ParseKind::Usize,
        default: "0",
        doc: "Background queue worker count; 0 = auto.",
    },
    EnvVar {
        name: "JWC_QUEUE_MAX_ATTEMPTS",
        parse_kind: ParseKind::U32,
        default: "3",
        doc: "Per-job retry ceiling; 0 = single attempt.",
    },
    EnvVar {
        name: "JWC_QUEUE_BACKOFF_MS",
        parse_kind: ParseKind::DurationMs,
        default: "1000",
        doc: "Base retry backoff in milliseconds.",
    },
    EnvVar {
        name: "JWC_QUEUE_DLQ_MAX",
        parse_kind: ParseKind::Usize,
        default: "1024",
        doc: "Dead-letter queue cap; 0 disables eviction.",
    },
    // --- Email -------------------------------------------------------------
    EnvVar {
        name: "JWC_SMTP_HOST",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "SMTP server hostname.",
    },
    EnvVar {
        name: "JWC_SMTP_PORT",
        parse_kind: ParseKind::U16,
        default: "587",
        doc: "SMTP server port.",
    },
    EnvVar {
        name: "JWC_SMTP_USER",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "SMTP auth username.",
    },
    EnvVar {
        name: "JWC_SMTP_PASSWORD",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "SMTP auth password / app token.",
    },
    EnvVar {
        name: "JWC_SMTP_FROM",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Default From: header for outbound mail.",
    },
    EnvVar {
        name: "JWC_SMTP_TLS",
        parse_kind: ParseKind::Enum,
        default: "starttls",
        doc: "TLS mode: starttls | tls | none.",
    },
    // --- Registry / packaging ---------------------------------------------
    EnvVar {
        name: "JWC_REGISTRY_URL",
        parse_kind: ParseKind::Str,
        default: "https://registry-jwc.1kb.uz/",
        doc: "Package registry endpoint.",
    },
    EnvVar {
        name: "JWC_REGISTRY_TOKEN",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Bearer token sent when publishing.",
    },
    EnvVar {
        name: "JWC_HOME",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Override the per-user data dir (default platform-specific).",
    },
    // --- Outbound HTTP / SSRF ----------------------------------------------
    EnvVar {
        name: "JWC_HTTP_ALLOWLIST",
        parse_kind: ParseKind::CsvList,
        default: "",
        doc: "Comma-separated host allowlist for http_get/http_post/fetch_json; empty = no restriction.",
    },
];

/// Names whose rendered value must be masked.
const REDACT_NEEDLES: &[&str] = &["PASSWORD", "SECRET", "TOKEN", "KEY", "JWT", "DATABASE_URL"];

fn name_is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    REDACT_NEEDLES.iter().any(|needle| upper.contains(needle))
}

fn parse_one(var: &EnvVar, raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim();
    match var.parse_kind {
        ParseKind::Str | ParseKind::Enum => Ok(trimmed.to_string()),
        ParseKind::Bool => {
            // Same truthy set as engine::parse_bool_flag — keep in sync.
            let lower = trimmed.to_ascii_lowercase();
            match lower.as_str() {
                "1" | "true" | "yes" | "on" => Ok("true".to_string()),
                "0" | "false" | "no" | "off" | "" => Ok("false".to_string()),
                other => Err(format!("invalid bool '{other}'")),
            }
        }
        ParseKind::U16 => trimmed
            .parse::<u16>()
            .map(|n| n.to_string())
            .map_err(|e| format!("invalid u16 '{trimmed}': {e}")),
        ParseKind::U32 => trimmed
            .parse::<u32>()
            .map(|n| n.to_string())
            .map_err(|e| format!("invalid u32 '{trimmed}': {e}")),
        ParseKind::U64 => trimmed
            .parse::<u64>()
            .map(|n| n.to_string())
            .map_err(|e| format!("invalid u64 '{trimmed}': {e}")),
        ParseKind::Usize => trimmed
            .parse::<usize>()
            .map(|n| n.to_string())
            .map_err(|e| format!("invalid usize '{trimmed}': {e}")),
        ParseKind::DurationSecs => trimmed
            .parse::<u64>()
            .map(|n| format!("{n} seconds"))
            .map_err(|e| format!("invalid u64 seconds '{trimmed}': {e}")),
        ParseKind::DurationMs => trimmed
            .parse::<u64>()
            .map(|n| format!("{n} ms"))
            .map_err(|e| format!("invalid u64 ms '{trimmed}': {e}")),
        ParseKind::CsvList => {
            let parts: Vec<&str> = trimmed
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            Ok(parts.join(", "))
        }
    }
}

/// Format a default value into the same canonical shape `parse_one`
/// produces, so the rendered table doesn't print a raw `"30"` for a
/// duration when the row above it shows `"30 seconds"`.
fn render_default(var: &EnvVar) -> String {
    if var.default.is_empty() {
        return String::new();
    }
    // Reuse the parser when it produces a nicer canonical form; for
    // strings it's a no-op.
    match parse_one(var, var.default) {
        Ok(s) => s,
        Err(_) => var.default.to_string(),
    }
}

/// Walk every row of [`REGISTRY`], read `std::env::var`, run the parser,
/// and return one [`RenderedEnvVar`] per row.
pub fn snapshot() -> Vec<RenderedEnvVar> {
    REGISTRY
        .iter()
        .map(|var| match std::env::var(var.name) {
            Ok(raw) if !raw.is_empty() => match parse_one(var, &raw) {
                Ok(parsed) => RenderedEnvVar {
                    name: var.name,
                    source: Source::Env,
                    raw: raw.clone(),
                    parsed: parsed.clone(),
                    parsed_raw: parsed,
                    error: None,
                },
                Err(msg) => RenderedEnvVar {
                    name: var.name,
                    source: Source::Env,
                    raw,
                    parsed: String::new(),
                    parsed_raw: String::new(),
                    error: Some(msg),
                },
            },
            _ => {
                let parsed = render_default(var);
                RenderedEnvVar {
                    name: var.name,
                    source: Source::Default,
                    raw: var.default.to_string(),
                    parsed: parsed.clone(),
                    parsed_raw: parsed,
                    error: None,
                }
            }
        })
        .collect()
}

/// Fixed-width ASCII table of [`snapshot`]'s output. Values whose name
/// matches the redaction policy at the top of this module are replaced
/// with `*** (redacted)` before formatting — the unmasked value stays
/// in `RenderedEnvVar::parsed_raw` so the runtime can still use it.
pub fn render(rows: &[RenderedEnvVar]) -> String {
    // Column widths — tuned for the longest registered name
    // (`JWC_DB_TLS_INSECURE_SKIP_VERIFY` = 31 chars).
    const NAME_W: usize = 32;
    const SOURCE_W: usize = 8;
    const VALUE_W: usize = 32;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<name_w$} {:<source_w$} {:<value_w$} ERROR",
        "ENV VAR",
        "SOURCE",
        "VALUE",
        name_w = NAME_W,
        source_w = SOURCE_W,
        value_w = VALUE_W,
    );

    for row in rows {
        let displayed_value = if name_is_secret(row.name) && !row.parsed.is_empty() {
            "*** (redacted)".to_string()
        } else if row.parsed.is_empty() && row.error.is_none() {
            "(unset)".to_string()
        } else {
            row.parsed.clone()
        };
        let err = row.error.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "{:<name_w$} {:<source_w$} {:<value_w$} {}",
            row.name,
            row.source.as_str(),
            truncate(&displayed_value, VALUE_W),
            err,
            name_w = NAME_W,
            source_w = SOURCE_W,
            value_w = VALUE_W,
        );
    }
    out
}

/// Clip overlong values so the value column doesn't bleed into ERROR.
/// Trailing `…` would be non-ASCII, so use `...` to keep `render`'s
/// output ASCII-only (a test pins that property).
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let mut t: String = s.chars().take(max - 3).collect();
    t.push_str("...");
    t
}

/// Boot fence — returns `Err` listing every parse failure from
/// [`snapshot`]. Wire this into the server boot path BEFORE the
/// listening line so a typo in `JWC_REQUEST_TIMEOUT=thirty` fails fast
/// instead of being swallowed by an `unwrap_or(30)` deeper in the call
/// graph.
pub fn validate_or_bail() -> Result<()> {
    let rows = snapshot();
    let errors: Vec<String> = rows
        .iter()
        .filter_map(|r| r.error.as_ref().map(|e| format!("  {}: {}", r.name, e)))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "config: {} env var(s) failed to parse:\n{}",
            errors.len(),
            errors.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Snapshot/validate tests mutate process-global env vars and so
    /// must serialise — any test that touches `std::env::set_var` /
    /// `remove_var` takes this lock for the duration of its body.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        name: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prev = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, prev }
        }
        fn unset(name: &'static str) -> Self {
            let prev = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn registry_has_at_least_ten_known_names() {
        assert!(
            REGISTRY.len() >= 10,
            "registry too small: {}",
            REGISTRY.len()
        );
        // Spot-check the names that downstream modules actually read.
        let names: Vec<&str> = REGISTRY.iter().map(|v| v.name).collect();
        for must in [
            "JWC_DATABASE_URL",
            "JWC_DB_POOL_SIZE",
            "JWC_REQUEST_TIMEOUT",
            "JWC_LOG_FORMAT",
            "JWC_SMTP_PASSWORD",
            "JWC_TRUSTED_PROXIES",
        ] {
            assert!(names.contains(&must), "registry missing {must}");
        }
    }

    #[test]
    fn snapshot_missing_var_is_default() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::unset("JWC_REQUEST_TIMEOUT");
        let rows = snapshot();
        let row = rows
            .iter()
            .find(|r| r.name == "JWC_REQUEST_TIMEOUT")
            .expect("row");
        assert_eq!(row.source, Source::Default);
        assert_eq!(row.parsed, "30 seconds");
        assert!(row.error.is_none());
    }

    #[test]
    fn smtp_password_is_redacted_in_render_but_not_lost() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("JWC_SMTP_PASSWORD", "hunter2");
        let rows = snapshot();
        let row = rows
            .iter()
            .find(|r| r.name == "JWC_SMTP_PASSWORD")
            .expect("row");
        // The raw / parsed_raw values are preserved so future code that
        // needs the password can still read it.
        assert_eq!(row.parsed_raw, "hunter2");
        assert_eq!(row.raw, "hunter2");

        let table = render(&rows);
        assert!(
            table.contains("*** (redacted)"),
            "redaction marker missing:\n{table}"
        );
        assert!(
            !table.contains("hunter2"),
            "password leaked into render output:\n{table}"
        );
    }

    #[test]
    fn validate_or_bail_errors_on_bad_numeric() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("JWC_REQUEST_TIMEOUT", "thirty");
        let err = validate_or_bail().expect_err("should fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("JWC_REQUEST_TIMEOUT"),
            "expected var name in error, got: {msg}"
        );
    }

    #[test]
    fn render_is_ascii_only() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("JWC_SMTP_PASSWORD", "hunter2");
        let rows = snapshot();
        let table = render(&rows);
        for (i, ch) in table.char_indices() {
            assert!(
                ch.is_ascii(),
                "non-ASCII char {ch:?} at byte {i} in render output"
            );
        }
    }

    #[test]
    fn csv_list_is_normalised() {
        let _l = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::set("JWC_TRUSTED_PROXIES", " 10.0.0.0/8 , 127.0.0.1 ,, ");
        let rows = snapshot();
        let row = rows
            .iter()
            .find(|r| r.name == "JWC_TRUSTED_PROXIES")
            .expect("row");
        assert_eq!(row.parsed, "10.0.0.0/8, 127.0.0.1");
    }

    #[test]
    fn redaction_covers_token_key_jwt_and_database_url() {
        assert!(name_is_secret("JWC_REGISTRY_TOKEN"));
        assert!(name_is_secret("JWC_DATABASE_URL"));
        assert!(name_is_secret("JWC_API_KEY"));
        assert!(name_is_secret("JWC_JWT_SIGNING"));
        assert!(!name_is_secret("JWC_PORT"));
        assert!(!name_is_secret("JWC_LOG_FORMAT"));
    }
}

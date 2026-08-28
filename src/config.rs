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
        doc: "NOT IMPLEMENTED — Admin DB used by `migrate` to create the target DB.",
    },
    // --- Redis -------------------------------------------------------------
    EnvVar {
        name: "JWC_REDIS_URL",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Redis connection string; empty disables the redis_* built-ins. \
              Use rediss:// for TLS.",
    },
    EnvVar {
        name: "JWC_REDIS_POOL_SIZE",
        parse_kind: ParseKind::Usize,
        default: "64",
        doc: "Max connections in the deadpool-redis pool.",
    },
    EnvVar {
        name: "JWC_REDIS_RETRY_MAX_ATTEMPTS",
        parse_kind: ParseKind::U32,
        default: "3",
        doc: "Transient-error retry ceiling for Redis commands.",
    },
    EnvVar {
        name: "JWC_REDIS_RETRY_BACKOFF_MS",
        parse_kind: ParseKind::U32,
        default: "100",
        doc: "Base Redis retry backoff (ms); doubles each attempt.",
    },
    // --- Buffered log writer -----------------------------------------------
    EnvVar {
        name: "JWC_LOG_QUEUE",
        parse_kind: ParseKind::Usize,
        default: "10000",
        doc: "Channel capacity for log_insert; rows are dropped once full.",
    },
    EnvVar {
        name: "JWC_LOG_BATCH",
        parse_kind: ParseKind::Usize,
        default: "2000",
        doc: "Rows per batched INSERT from the log writer.",
    },
    EnvVar {
        name: "JWC_LOG_FLUSH_MS",
        parse_kind: ParseKind::DurationMs,
        default: "200",
        doc: "Longest a log_insert row waits before being written (ms).",
    },
    EnvVar {
        name: "JWC_LOG_CONCURRENCY",
        parse_kind: ParseKind::Usize,
        default: "4",
        doc: "Batch INSERTs the log writer keeps in flight at once.",
    },
    // --- Server ------------------------------------------------------------
    EnvVar {
        name: "JWC_SERVER_WORKERS",
        parse_kind: ParseKind::Usize,
        default: "0",
        doc: "NOT IMPLEMENTED — Tokio worker threads; 0 = available_parallelism().",
    },
    EnvVar {
        name: "JWC_SERVER_METRICS",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "NOT IMPLEMENTED — Periodically log in-flight / completed / failed counters.",
    },
    EnvVar {
        name: "JWC_SERVER_METRICS_INTERVAL_SECS",
        parse_kind: ParseKind::DurationSecs,
        default: "10",
        doc: "NOT IMPLEMENTED — Metrics log cadence.",
    },
    EnvVar {
        name: "JWC_REQUEST_LOG",
        parse_kind: ParseKind::Bool,
        default: "0",
        doc: "One access line per answered request, on stderr. \
              `jwc serve --request-logging` sets it; a native binary has \
              no flags, so this is how `jwc build` output is turned on.",
    },
    EnvVar {
        name: "JWC_LOG_FORMAT",
        parse_kind: ParseKind::Enum,
        default: "text",
        doc: "Access-log shape: `text` or `json`. Read only when \
              JWC_REQUEST_LOG is on.",
    },
    EnvVar {
        name: "JWC_REQUEST_TIMEOUT",
        parse_kind: ParseKind::DurationSecs,
        default: "30",
        doc: "NOT IMPLEMENTED — Per-request watchdog; 0 disables the cap.",
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
        name: "JWC_DEBUG_ERRORS",
        parse_kind: ParseKind::Bool,
        default: "0",
        doc: "Return the full error text on a 500 instead of a generic message. Local debugging only.",
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
        doc: "NOT IMPLEMENTED — Print this config table at server boot; set off to suppress.",
    },
    // --- Read by the code, and until 0.9.927 in no registry, so absent
    // from the boot table and from config.md. Found by
    // `every_env_var_the_code_reads_is_registered_and_the_other_way_round`,
    // not by anyone reading the code.
    EnvVar {
        name: "JWC_BIND_HOST",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Native builds only: override the listen address (`server { bind }` in the source).",
    },
    EnvVar {
        name: "JWC_DEV",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "Development mode: `debug.dump` prints. Never in production — it prints request data.",
    },
    EnvVar {
        name: "JWC_HTTP_TIMEOUT_SECS",
        parse_kind: ParseKind::U64,
        default: "10",
        doc: "Whole-request ceiling for outbound `http.*` calls.",
    },
    EnvVar {
        name: "JWC_LOG_SQL",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "Print every SQL statement the program issues, with its parameters.",
    },
    EnvVar {
        name: "JWC_OTLP_ENDPOINT",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "OTLP collector URL; empty disables tracing export.",
    },
    EnvVar {
        name: "JWC_SERVICE_NAME",
        parse_kind: ParseKind::Str,
        default: "jwc",
        doc: "`service.name` on exported traces.",
    },
    EnvVar {
        name: "JWC_REGISTRY",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Package registry base URL; empty uses the default registry.",
    },
    EnvVar {
        name: "JWC_REQUEST_BODY",
        parse_kind: ParseKind::Str,
        default: "null",
        doc: "Native builds only: what `request.body()` answers outside a request.",
    },

    // --- Queue -------------------------------------------------------------
    // `JWC_JOB_WORKERS`, not `JWC_QUEUE_WORKERS`: the registry carried the
    // second name and `jobs.rs` read the first, so the documented knob did
    // nothing and the working knob was documented nowhere. The names now
    // come from one place, and `every_env_var_the_code_reads_is_registered`
    // keeps them there.
    EnvVar {
        name: "JWC_JOB_WORKERS",
        parse_kind: ParseKind::Usize,
        default: "2",
        doc: "Worker tasks polling the job queue. 0 = none in this \
              process; another deployment of the same sources drains it.",
    },
    EnvVar {
        name: "JWC_JOB_POLL_MS",
        parse_kind: ParseKind::U64,
        default: "1000",
        doc: "How often a worker polls an empty queue, in milliseconds.",
    },
    EnvVar {
        name: "JWC_QUEUE_MAX_ATTEMPTS",
        parse_kind: ParseKind::U32,
        default: "3",
        doc: "NOT IMPLEMENTED — Per-job retry ceiling; 0 = single attempt.",
    },
    EnvVar {
        name: "JWC_QUEUE_BACKOFF_MS",
        parse_kind: ParseKind::DurationMs,
        default: "1000",
        doc: "NOT IMPLEMENTED — Base retry backoff in milliseconds.",
    },
    EnvVar {
        name: "JWC_QUEUE_DLQ_MAX",
        parse_kind: ParseKind::Usize,
        default: "1024",
        doc: "NOT IMPLEMENTED — Dead-letter queue cap; 0 disables eviction.",
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
    EnvVar {
        name: "JWC_CACHE_MAX_ENTRIES",
        parse_kind: ParseKind::U64,
        default: "10000",
        doc: "Entry ceiling for the process-local `cache.*` store.",
    },
    // --- Registry / packaging ---------------------------------------------
    EnvVar {
        name: "JWC_REGISTRY_URL",
        parse_kind: ParseKind::Str,
        default: "https://registry-jwc.1kb.uz/",
        doc: "NOT IMPLEMENTED — Package registry endpoint.",
    },
    EnvVar {
        name: "JWC_REGISTRY_TOKEN",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "NOT IMPLEMENTED — Bearer token sent when publishing.",
    },
    EnvVar {
        name: "JWC_HOME",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "NOT IMPLEMENTED — Override the per-user data dir (default platform-specific).",
    },
    // --- Outbound HTTP / SSRF ----------------------------------------------
    EnvVar {
        name: "JWC_HTTP_ALLOWLIST",
        parse_kind: ParseKind::CsvList,
        default: "",
        doc: "Comma-separated host allowlist for http_get/http_post/fetch_json; empty = no restriction.",
    },
    EnvVar {
        name: "JWC_HTTP_BLOCK_PRIVATE",
        parse_kind: ParseKind::Bool,
        default: "false",
        doc: "Block loopback/private/link-local outbound hosts (incl. cloud metadata).",
    },
    // --- JWT verification --------------------------------------------------
    EnvVar {
        name: "JWC_JWT_LEEWAY_SECS",
        parse_kind: ParseKind::DurationSecs,
        default: "0",
        doc: "Clock-skew tolerance applied to jwt_verify's exp/nbf checks.",
    },
    EnvVar {
        name: "JWC_JWT_EXPECTED_ISS",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Require this 'iss' claim in jwt_verify; empty = not checked.",
    },
    EnvVar {
        name: "JWC_JWT_EXPECTED_AUD",
        parse_kind: ParseKind::Str,
        default: "",
        doc: "Require this value in jwt_verify's 'aud' claim; empty = not checked.",
    },
    EnvVar {
        name: "JWC_JWT_JWKS_TTL_SECS",
        parse_kind: ParseKind::DurationSecs,
        default: "300",
        doc: "How long a fetched JWKS key set stays cached.",
    },
    EnvVar {
        name: "JWC_JWT_JWKS_MIN_REFETCH_SECS",
        parse_kind: ParseKind::DurationSecs,
        default: "60",
        doc: "Floor between forced JWKS refetches on an unknown 'kid' (DoS guard).",
    },
];

/// Names whose rendered value must be masked.
// `REDIS_URL` earns its place for the same reason as `DATABASE_URL`: the
// value is a connection string whose userinfo carries a password
// (`redis://:hunter2@host:6379`), and on Redis the bare-password form with
// no username is the common one.
const REDACT_NEEDLES: &[&str] = &[
    "PASSWORD",
    "SECRET",
    "TOKEN",
    "KEY",
    "JWT",
    "DATABASE_URL",
    "REDIS_URL",
];

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
        // `redis://:hunter2@host` puts a password in the value.
        assert!(name_is_secret("JWC_REDIS_URL"));
        // ...but the other Redis knobs are plain numbers — redacting them
        // would hide useful info from `jwc config` for no benefit.
        assert!(!name_is_secret("JWC_REDIS_POOL_SIZE"));
        assert!(!name_is_secret("JWC_PORT"));
        assert!(!name_is_secret("JWC_LOG_FORMAT"));
    }
}

// ---------------------------------------------------------------- dotenv

// The `.env` rules, shared with the crate `jwc build` generates so a
// native binary reads the same file the same way (config.md §5.1).
include!("dotenv_core.rs.in");

#[cfg(test)]
mod dotenv_tests {
    use super::*;

    fn in_dir(body: &str) -> (tempfile::TempDir, DotenvReport) {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join(".env"), body).expect("write");
        let r = load_dotenv(d.path());
        (d, r)
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let d = tempfile::tempdir().expect("tempdir");
        let r = load_dotenv(d.path());
        assert!(r.path.is_none() && r.set.is_empty() && r.malformed.is_empty());
    }

    #[test]
    fn the_ordinary_shapes_all_read() {
        let (_d, r) = in_dir(
            "# a comment\n\
             \n\
             DOTENV_T_PLAIN=one\n\
             export DOTENV_T_EXPORTED=two\n\
             DOTENV_T_DQ=\"three four\"\n\
             DOTENV_T_SQ='five'\n\
             DOTENV_T_SPACED = six \n",
        );
        assert!(r.malformed.is_empty(), "{:?}", r.malformed);
        assert_eq!(std::env::var("DOTENV_T_PLAIN").as_deref(), Ok("one"));
        assert_eq!(std::env::var("DOTENV_T_EXPORTED").as_deref(), Ok("two"));
        assert_eq!(std::env::var("DOTENV_T_DQ").as_deref(), Ok("three four"));
        assert_eq!(std::env::var("DOTENV_T_SQ").as_deref(), Ok("five"));
        assert_eq!(std::env::var("DOTENV_T_SPACED").as_deref(), Ok("six"));
    }

    #[test]
    fn nothing_inside_a_value_is_interpreted() {
        let (_d, r) = in_dir("DOTENV_T_PW=p$ss\\w0rd#not-a-comment\n");
        assert!(r.malformed.is_empty());
        // A password is a password. No expansion, no escape, and the `#`
        // is not a trailing comment — treating it as one silently changes
        // the secret.
        assert_eq!(
            std::env::var("DOTENV_T_PW").as_deref(),
            Ok("p$ss\\w0rd#not-a-comment")
        );
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        // SAFETY: single-threaded test setup.
        unsafe { std::env::set_var("DOTENV_T_WINS", "from-env") };
        let (_d, r) = in_dir("DOTENV_T_WINS=from-file\n");
        assert_eq!(std::env::var("DOTENV_T_WINS").as_deref(), Ok("from-env"));
        assert_eq!(r.kept_from_env, vec!["DOTENV_T_WINS".to_string()]);
        assert!(r.set.is_empty());
    }

    #[test]
    fn a_line_that_is_not_a_setting_is_reported_not_swallowed() {
        let (_d, r) = in_dir("DOTENV_T_OK=1\nthis is not a setting\nBAD-NAME=2\n");
        assert_eq!(r.set, vec!["DOTENV_T_OK".to_string()]);
        assert_eq!(r.malformed.len(), 2, "{:?}", r.malformed);
        assert_eq!(r.malformed[0].0, 2);
        assert_eq!(r.malformed[1].0, 3);
    }
}

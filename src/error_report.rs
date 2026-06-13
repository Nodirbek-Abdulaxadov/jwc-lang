use std::backtrace::BacktraceStatus;

use anyhow::Error;

/// Last-pass secret scrubber for any string that lands in user-visible
/// error output (CLI banners, structured JSON logs, response bodies
/// after `to_single_line`). Removes two patterns:
///
/// - `scheme://user:password@host` → `scheme://user:***@host`
///   (matches `crate::engine::scrub_database_url` but works on a
///   sub-string so it scrubs URLs embedded inside a larger message).
/// - `password=...` (non-greedy, stops at `&`, whitespace or end of
///   string) → `password=***`. Catches SMTP auth strings, libpq-style
///   keyword/value connection strings, leaked `?password=` query
///   params, and the occasional `anyhow!("smtp failed: password=...")`
///   message.
///
/// Both substitutions are case-insensitive for the literal prefix.
pub fn scrub_secrets(s: &str) -> String {
    let mut out = scrub_scheme_userinfo(s);
    out = scrub_kv_password(&out);
    out
}

/// Walk `s` once, copying bytes verbatim except for `scheme://user:password@`
/// runs, which collapse the `:password` portion to `:***`. Single-pass so we
/// don't allocate when nothing matches.
fn scrub_scheme_userinfo(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        // Look for `://` starting at `i`.
        if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"://" {
            // Find `@` before the next `/`, whitespace, or end.
            let userinfo_start = i + 3;
            let mut at_idx: Option<usize> = None;
            let mut term_idx = bytes.len();
            for j in userinfo_start..bytes.len() {
                let b = bytes[j];
                if b == b'@' {
                    at_idx = Some(j);
                    break;
                }
                if b == b'/' || b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'"' {
                    term_idx = j;
                    break;
                }
            }
            if let Some(at) = at_idx {
                // Only rewrite if the userinfo contains a `:` (password).
                let userinfo = &s[userinfo_start..at];
                if let Some(colon) = userinfo.find(':') {
                    out.push_str("://");
                    out.push_str(&userinfo[..colon]);
                    out.push_str(":***");
                    out.push('@');
                    i = at + 1;
                    continue;
                }
                // No password — passthrough.
                out.push_str("://");
                out.push_str(userinfo);
                out.push('@');
                i = at + 1;
                continue;
            }
            // No `@` before terminator — passthrough the `://` and keep walking.
            // Advance past the chunk we already inspected so we don't redo
            // O(n) work on each `://` occurrence.
            out.push_str("://");
            for j in userinfo_start..term_idx {
                out.push(bytes[j] as char);
            }
            i = term_idx;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Mask `password=VALUE` (case-insensitive prefix). VALUE runs up to the
/// first `&`, whitespace byte, quote, or end of string.
fn scrub_kv_password(s: &str) -> String {
    const PREFIX: &str = "password=";
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut start = 0;
    while let Some(rel) = lower[start..].find(PREFIX) {
        let pwd_start = start + rel + PREFIX.len();
        out.push_str(&s[start..start + rel + PREFIX.len()]);
        // Walk forward until the value ends.
        let bytes = s.as_bytes();
        let mut end = pwd_start;
        while end < bytes.len() {
            let b = bytes[end];
            if matches!(b, b'&' | b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'') {
                break;
            }
            end += 1;
        }
        if end > pwd_start {
            out.push_str("***");
        }
        start = end;
    }
    out.push_str(&s[start..]);
    out
}

/// `true` when the user has opted into structured JSON log output via
/// `JWC_LOG_FORMAT=json`. Aggregators like Loki / Datadog / CloudWatch
/// parse line-delimited JSON natively; the legacy `[JWC-ERROR]` shape
/// stays the default so existing log scrapers and interactive `jwc run`
/// output don't break.
fn log_format_is_json() -> bool {
    std::env::var("JWC_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

/// JSON-escape a string with just enough coverage for log payloads
/// (quotes, backslashes, control bytes). Keeping this local avoids
/// pulling `serde_json` into every error-path code site.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

pub fn print_cli_error(err: &Error) {
    eprintln!("\nUnhandled JWC error:");
    eprintln!("  Message: {}", scrub_secrets(&err.to_string()));

    let mut causes = err.chain();
    let _ = causes.next();
    for (idx, cause) in causes.enumerate() {
        eprintln!("  Caused by[{idx}]: {}", scrub_secrets(&cause.to_string()));
    }

    let bt = err.backtrace();
    if bt.status() == BacktraceStatus::Captured {
        eprintln!("\nBacktrace:\n{bt}");
    } else {
        eprintln!("  Tip: set RUST_BACKTRACE=1 to include backtrace details.");
    }
}

pub fn log_runtime_error(context: &str, err: &Error) {
    if log_format_is_json() {
        log_runtime_error_json(context, err);
    } else {
        log_runtime_error_text(context, err);
    }
}

fn log_runtime_error_text(context: &str, err: &Error) {
    eprintln!("[JWC-ERROR] {}", scrub_secrets(context));
    eprintln!("[JWC-ERROR] Message: {}", scrub_secrets(&err.to_string()));

    let mut causes = err.chain();
    let _ = causes.next();
    for (idx, cause) in causes.enumerate() {
        eprintln!(
            "[JWC-ERROR] Caused by[{idx}]: {}",
            scrub_secrets(&cause.to_string())
        );
    }
}

/// One log line per error in newline-delimited JSON shape:
///
/// `{"level":"error","context":"...","message":"...","causes":["...", ...]}`
///
/// `level` is a top-level field so a Loki / Datadog / CloudWatch query
/// can filter on it without re-parsing the message string. The causes
/// array preserves the anyhow error chain in arrival order, matching the
/// text formatter's `Caused by[0]/[1]/...` numbering.
fn log_runtime_error_json(context: &str, err: &Error) {
    let mut causes_iter = err.chain();
    let head = causes_iter
        .next()
        .map(|e| scrub_secrets(&e.to_string()))
        .unwrap_or_default();
    let mut causes: Vec<String> = causes_iter
        .map(|c| escape_json(&scrub_secrets(&c.to_string())))
        .collect();
    let causes_json = {
        let mut s = String::from("[");
        for (i, c) in causes.drain(..).enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('"');
            s.push_str(&c);
            s.push('"');
        }
        s.push(']');
        s
    };
    eprintln!(
        "{{\"level\":\"error\",\"context\":\"{ctx}\",\"message\":\"{msg}\",\"causes\":{causes}}}",
        ctx = escape_json(&scrub_secrets(context)),
        msg = escape_json(&head),
        causes = causes_json,
    );
}

pub fn to_single_line(err: &Error) -> String {
    let mut parts = Vec::new();
    for cause in err.chain() {
        parts.push(cause.to_string());
    }
    scrub_secrets(&parts.join(" | caused by: "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_json_handles_quotes_and_controls() {
        assert_eq!(escape_json("a\"b"), "a\\\"b");
        assert_eq!(escape_json("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_json("\x01"), "\\u0001");
    }

    #[test]
    fn database_url_with_password_redacted_in_connection_error() {
        // Synthesise an anyhow error chain that embeds a postgres URL with
        // creds and pipe it through `to_single_line` (the last-pass
        // scrubber the HTTP layer uses to ship a body back to the client).
        let inner =
            anyhow::anyhow!("connect failed for postgres://alice:hunter2@db.internal:5432/app");
        let outer: anyhow::Error = inner.context("DB ping (SELECT 1) failed");
        let line = to_single_line(&outer);
        assert!(line.contains("alice:***@db.internal"), "got: {line}");
        assert!(!line.contains("hunter2"), "leaked password in: {line}");
    }

    #[test]
    fn smtp_password_not_leaked_in_error_chain() {
        // `password=...` is the SMTP / libpq KV shape. `to_single_line`
        // must mask it before the message lands in the response body.
        let err = anyhow::anyhow!("smtp send failed: password=hunter2 host=mail.example.com");
        let line = to_single_line(&err);
        assert!(line.contains("password=***"), "got: {line}");
        assert!(!line.contains("hunter2"), "leaked password in: {line}");
    }

    #[test]
    fn scrub_secrets_leaves_clean_strings_alone() {
        // Sanity: anything without a secret pattern is unchanged.
        let s = "DB ping (SELECT 1) failed | caused by: pool exhausted";
        assert_eq!(scrub_secrets(s), s);
    }

    #[test]
    fn log_format_is_json_respects_env() {
        let prev = std::env::var("JWC_LOG_FORMAT").ok();
        std::env::remove_var("JWC_LOG_FORMAT");
        assert!(!log_format_is_json());
        std::env::set_var("JWC_LOG_FORMAT", "json");
        assert!(log_format_is_json());
        std::env::set_var("JWC_LOG_FORMAT", "JSON");
        assert!(log_format_is_json(), "case-insensitive match");
        std::env::set_var("JWC_LOG_FORMAT", "text");
        assert!(!log_format_is_json());
        match prev {
            Some(v) => std::env::set_var("JWC_LOG_FORMAT", v),
            None => std::env::remove_var("JWC_LOG_FORMAT"),
        }
    }
}

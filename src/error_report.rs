use std::backtrace::BacktraceStatus;

use anyhow::Error;

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
    eprintln!("  Message: {}", err);

    let mut causes = err.chain();
    let _ = causes.next();
    for (idx, cause) in causes.enumerate() {
        eprintln!("  Caused by[{idx}]: {cause}");
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
    eprintln!("[JWC-ERROR] {context}");
    eprintln!("[JWC-ERROR] Message: {err}");

    let mut causes = err.chain();
    let _ = causes.next();
    for (idx, cause) in causes.enumerate() {
        eprintln!("[JWC-ERROR] Caused by[{idx}]: {cause}");
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
        .map(|e| e.to_string())
        .unwrap_or_default();
    let mut causes: Vec<String> = causes_iter.map(|c| escape_json(&c.to_string())).collect();
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
        ctx = escape_json(context),
        msg = escape_json(&head),
        causes = causes_json,
    );
}

pub fn to_single_line(err: &Error) -> String {
    let mut parts = Vec::new();
    for cause in err.chain() {
        parts.push(cause.to_string());
    }
    parts.join(" | caused by: ")
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

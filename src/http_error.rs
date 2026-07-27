//! One JSON shape for every error the runtime returns.
//!
//! There used to be three, and a client had to handle all of them:
//!
//! ```text
//! {"errors":{"email":"pattern(...)","password":"minLength(8)"}}   // validate
//! {"status":404,"error":"Not Found","method":"GET","path":"/x"}   // router
//! {"error":"category has transactions; delete them first"}        // handler
//! ```
//!
//! Only the middle one carried a status, only the first carried field
//! detail, and the key holding the message changed between them — so every
//! frontend wrote three parsers. They now share:
//!
//! ```text
//! {
//!   "error":   "human-readable message",   // always
//!   "status":  400,                        // always
//!   "code":    "validation_failed",        // always, stable, machine-readable
//!   "details": { ... }                     // only when there is structured detail
//! }
//! ```
//!
//! The request id stays in the `x-request-id` response header rather than
//! being duplicated into the body.

use serde_json::{json, Map, Value};

/// Stable, machine-readable error kinds. The string is part of the HTTP
/// contract — clients branch on it, so these names don't change without a
/// release note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ValidationFailed,
    NotFound,
    MethodNotAllowed,
    Timeout,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ValidationFailed => "validation_failed",
            ErrorCode::NotFound => "not_found",
            ErrorCode::MethodNotAllowed => "method_not_allowed",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Internal => "internal_error",
        }
    }
}

/// Build the envelope. `details` is attached only when non-empty, so a
/// client can treat its presence as "there is per-field information here".
pub fn envelope(status: u16, code: ErrorCode, message: &str, details: Option<Value>) -> String {
    let mut doc = Map::new();
    doc.insert("error".into(), json!(message));
    doc.insert("status".into(), json!(status));
    doc.insert("code".into(), json!(code.as_str()));
    if let Some(d) = details {
        let empty = match &d {
            Value::Object(m) => m.is_empty(),
            Value::Array(a) => a.is_empty(),
            Value::Null => true,
            _ => false,
        };
        if !empty {
            doc.insert("details".into(), d);
        }
    }
    Value::Object(doc).to_string()
}

/// 404 — no route matched this path under any method.
pub fn not_found(method: &str, path: &str) -> String {
    envelope(
        404,
        ErrorCode::NotFound,
        &format!("No route for {method} {path}"),
        Some(json!({ "method": method, "path": path })),
    )
}

/// 405 — the path exists, the verb doesn't. `allow` is also sent as the
/// `Allow` header, per RFC 9110.
pub fn method_not_allowed(method: &str, path: &str, allow: &str) -> String {
    envelope(
        405,
        ErrorCode::MethodNotAllowed,
        &format!("{method} is not allowed on {path}"),
        Some(json!({ "method": method, "path": path, "allow": allow })),
    )
}

/// 500 — an uncaught error from a handler.
///
/// The message is deliberately generic. The previous shape passed the raw
/// error straight through, which put internal Rust type names and SQL text
/// in front of anyone who could make a request:
///
/// ```text
/// {"error":"Failed to execute SQL query | caused by: ... cannot convert
///  between the Rust type `alloc::string::String` and the Postgres type ..."}
/// ```
///
/// The full error is still logged server-side against the request id, and
/// `JWC_DEBUG_ERRORS=1` puts it back in the response for local debugging.
pub fn internal(detail: &str) -> String {
    if debug_errors_enabled() {
        return envelope(500, ErrorCode::Internal, detail, None);
    }
    envelope(
        500,
        ErrorCode::Internal,
        "Internal server error. Check the server log for the matching x-request-id.",
        None,
    )
}

/// 504 — the per-request watchdog fired.
pub fn timeout(seconds: u64) -> String {
    envelope(
        504,
        ErrorCode::Timeout,
        &format!("Request timed out after {seconds}s"),
        None,
    )
}

/// 400 — `validate body { ... }` rejected the request. `fields` maps a field
/// name to the rule it failed.
pub fn validation_failed(fields: Value) -> String {
    envelope(
        400,
        ErrorCode::ValidationFailed,
        "Request body failed validation",
        Some(fields),
    )
}

fn debug_errors_enabled() -> bool {
    std::env::var("JWC_DEBUG_ERRORS")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("envelope is valid JSON")
    }

    /// The point of the change: one shape, so a client reads `.error`,
    /// `.status` and `.code` the same way whatever went wrong.
    #[test]
    fn every_kind_carries_error_status_and_code() {
        let cases = [
            not_found("GET", "/x"),
            method_not_allowed("DELETE", "/items", "GET, POST"),
            timeout(30),
            validation_failed(json!({ "email": "pattern(...)" })),
        ];
        for raw in cases {
            let v = parse(&raw);
            assert!(v.get("error").and_then(|e| e.as_str()).is_some(), "{raw}");
            assert!(v.get("status").and_then(|s| s.as_u64()).is_some(), "{raw}");
            assert!(v.get("code").and_then(|c| c.as_str()).is_some(), "{raw}");
        }
    }

    #[test]
    fn validation_detail_lands_under_details() {
        let v = parse(&validation_failed(json!({ "email": "pattern(^.+$)" })));
        assert_eq!(v["status"], 400);
        assert_eq!(v["code"], "validation_failed");
        assert_eq!(v["details"]["email"], "pattern(^.+$)");
    }

    #[test]
    fn method_not_allowed_reports_the_allowed_verbs() {
        let v = parse(&method_not_allowed("DELETE", "/items", "GET, POST"));
        assert_eq!(v["status"], 405);
        assert_eq!(v["details"]["allow"], "GET, POST");
    }

    #[test]
    fn empty_details_are_omitted_rather_than_sent_as_an_empty_object() {
        let v = parse(&envelope(500, ErrorCode::Internal, "boom", Some(json!({}))));
        assert!(v.get("details").is_none(), "got: {v}");
    }

    /// A 500 must not hand internal type names and SQL to the caller.
    #[test]
    fn internal_errors_do_not_leak_server_internals_by_default() {
        let leak = "Failed to execute SQL query | caused by: cannot convert between \
                    the Rust type `alloc::string::String` and the Postgres type `numeric`";
        let v = parse(&internal(leak));
        let msg = v["error"].as_str().unwrap();
        assert!(!msg.contains("Rust type"), "leaked internals: {msg}");
        assert!(!msg.contains("numeric"), "leaked internals: {msg}");
        assert!(
            msg.contains("x-request-id"),
            "should point at the correlating log line: {msg}"
        );
    }
}

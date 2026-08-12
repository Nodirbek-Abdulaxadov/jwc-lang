//! `statusCode(3xx, { Location: url })` must emit a real redirect: the
//! object is a HEADER map, not a body.
//!
//! The interpreter used to accept only a `Value::Str` of JSON there, which
//! stopped matching once object literals started evaluating to the typed
//! `Value::Record`. The call then fell through to the JSON-body arm and
//! answered `302` with `{"Location":"..."}` as the *body* and no `Location`
//! header, so no client ever followed it. The native backend already
//! handled Record; these tests pin the two paths to the same behaviour.

use std::collections::HashMap;

use jwc::parser::{parse_program, validate_program};
use jwc::runner;

fn parse(src: &str) -> jwc::ast::Program {
    let program = parse_program(src).expect("program should parse");
    validate_program(&program).expect("program should validate");
    program
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn object_literal_becomes_a_location_header() {
    let src = r#"
        route GET "/go" {
            return statusCode(302, { Location: "https://example.com/target" });
        }
    "#;
    let program = parse(src);
    let (status, body, _content_type, headers) =
        runner::run_request_with_headers(&program, "GET", "/go", None, HashMap::new())
            .await
            .expect("request should succeed");

    assert_eq!(status, 302);
    assert_eq!(
        header(&headers, "Location"),
        Some("https://example.com/target"),
        "a 3xx object arg must become response headers, got headers {headers:?}",
    );
    assert!(
        body.is_empty(),
        "redirect body must stay empty, got `{body}`",
    );
}

#[tokio::test]
async fn location_built_from_a_variable_still_redirects() {
    // The shortener's shape: the URL is looked up first, then handed to
    // `statusCode`. Guards the same path when the field value is not a
    // literal.
    let src = r#"
        function target(): string { return "https://example.com/from-fn"; }
        route GET "/go" {
            let url = target();
            return statusCode(302, { Location: url });
        }
    "#;
    let program = parse(src);
    let (status, _body, _content_type, headers) =
        runner::run_request_with_headers(&program, "GET", "/go", None, HashMap::new())
            .await
            .expect("request should succeed");

    assert_eq!(status, 302);
    assert_eq!(
        header(&headers, "Location"),
        Some("https://example.com/from-fn"),
    );
}

#[tokio::test]
async fn non_redirect_status_keeps_the_object_as_a_body() {
    // The header interpretation is scoped to 3xx: `statusCode(429, {...})`
    // must still answer with the object as the response body.
    let src = r#"
        route GET "/limited" {
            return statusCode(429, { error: "rate limited" });
        }
    "#;
    let program = parse(src);
    let (status, body, _content_type, headers) =
        runner::run_request_with_headers(&program, "GET", "/limited", None, HashMap::new())
            .await
            .expect("request should succeed");

    assert_eq!(status, 429);
    assert!(
        body.contains("rate limited"),
        "non-3xx object must stay a body, got `{body}`",
    );
    assert!(
        header(&headers, "error").is_none(),
        "non-3xx must not promote body fields to headers, got {headers:?}",
    );
}

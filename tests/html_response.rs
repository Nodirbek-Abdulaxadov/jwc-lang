//! `html(body)` HTTP response helper — the runtime should ship the body
//! verbatim and surface `text/html; charset=utf-8` as the response
//! content-type so browsers actually render the markup. Before v0.3.6 the
//! HTTP transport hardcoded `application/json` on every string response, so
//! returning an HTML string would dump tag soup into the user's tab.

use std::collections::HashMap;

use jwc::parser::{parse_program, validate_program};
use jwc::runner;

const HTML_CT: &str = "text/html; charset=utf-8";

fn parse(src: &str) -> jwc::ast::Program {
    let program = parse_program(src).expect("program should parse");
    validate_program(&program).expect("program should validate");
    program
}

#[tokio::test]
async fn html_builtin_returns_body_with_html_content_type() {
    let src = r#"
        route GET "/" {
            return html("<h1>hi</h1>");
        }
    "#;
    let program = parse(src);
    let (status, body, content_type, _headers) =
        runner::run_request_with_headers(&program, "GET", "/", None, HashMap::new())
            .await
            .expect("request should succeed");

    assert_eq!(status, 200);
    assert_eq!(
        body, "<h1>hi</h1>",
        "body must be the raw HTML, not JSON-wrapped"
    );
    assert_eq!(
        content_type.as_deref(),
        Some(HTML_CT),
        "html(...) must declare text/html so axum doesn't fall back to application/json",
    );
}

#[tokio::test]
async fn html_builtin_accepts_string_function_result() {
    // Mirrors the shortener / 1kb-landing call pattern:
    // `return html(landing_html());` — the body comes from another function,
    // not a string literal.
    let src = r#"
        function page(): string { return "<!doctype html><body>ok</body>"; }
        route GET "/page" { return html(page()); }
    "#;
    let program = parse(src);
    let (status, body, content_type, _headers) =
        runner::run_request_with_headers(&program, "GET", "/page", None, HashMap::new())
            .await
            .expect("request should succeed");

    assert_eq!(status, 200);
    assert_eq!(body, "<!doctype html><body>ok</body>");
    assert_eq!(content_type.as_deref(), Some(HTML_CT));
}

#[tokio::test]
async fn json_builtin_still_omits_content_type_so_server_defaults_to_json() {
    // Regression guard: only the new HTML/text helpers set an explicit
    // content-type. The legacy `json(...)` path keeps emitting `None` so
    // `/healthz` and every other JSON route continues to be served as
    // `application/json` by the axum layer's default — no change in
    // behaviour for existing handlers.
    //
    // Use a non-`status` payload field because `dispatch_route` strips the
    // top-level `status` key (it's the historical "tell me the HTTP code"
    // channel for response builders like `notFound()` and `unauthorized()`).
    let src = r#"
        route GET "/healthz" {
            return json({ ok: true });
        }
    "#;
    let program = parse(src);
    let (status, body, content_type, _headers) =
        runner::run_request_with_headers(&program, "GET", "/healthz", None, HashMap::new())
            .await
            .expect("request should succeed");

    assert_eq!(status, 200);
    assert!(
        body.contains("\"ok\":true"),
        "json body should still be a JSON document, got `{body}`",
    );
    assert!(
        content_type.is_none(),
        "json(...) must not declare a content-type — the server defaults to application/json",
    );
}

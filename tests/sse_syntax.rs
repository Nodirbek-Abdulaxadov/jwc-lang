//! Parse + validate smoke tests for Sprint 10.4 v1 (`route SSE "..."`).
//!
//! v1 only ships the syntax surface — the chunked `text/event-stream`
//! transport, `sse_send` / `sse_close` / `sse_broadcast` builtins, and the
//! per-topic subscriber registry are deferred to v2. The point of v1 is
//! that user code declaring `route SSE` parses cleanly and the AST stores
//! the protocol marker, so a v2 PR can wire the runtime without touching
//! the parser again.

use jwc::ast::RouteProtocol;
use jwc::parser::{parse_program, validate_program};

#[test]
fn route_sse_parses_with_protocol_marker() {
    let src = r#"
        route SSE "/feed" {
            return json({ ok: true });
        }
    "#;
    let program = parse_program(src).expect("SSE route must parse");
    validate_program(&program).expect("SSE route must validate");
    assert_eq!(program.routes.len(), 1);
    assert_eq!(program.routes[0].protocol, RouteProtocol::Sse);
    assert_eq!(program.routes[0].method, "SSE");
    assert_eq!(program.routes[0].path, "/feed");
}

#[test]
fn route_sse_lowercase_method_normalises_to_uppercase() {
    let src = r#"
        route sse "/events" {
            return json({ ok: true });
        }
    "#;
    let program = parse_program(src).expect("lowercase sse must parse");
    validate_program(&program).expect("lowercase sse must validate");
    assert_eq!(program.routes[0].protocol, RouteProtocol::Sse);
    assert_eq!(
        program.routes[0].method, "SSE",
        "method string is always uppercased for SSE just like WS"
    );
}

#[test]
fn route_sse_alongside_http_and_ws_routes() {
    let src = r#"
        route GET "/users" { return json({ ok: true }); }
        route WS  "/chat"  { return json({ ok: true }); }
        route SSE "/feed"  { return json({ ok: true }); }
    "#;
    let program = parse_program(src).expect("mixed protocol parse");
    validate_program(&program).expect("mixed protocol validate");
    let protocols: Vec<_> = program.routes.iter().map(|r| r.protocol).collect();
    assert_eq!(
        protocols,
        vec![RouteProtocol::Http, RouteProtocol::Ws, RouteProtocol::Sse]
    );
}

//! The `server { }` limits, through the real pipeline (config.md §3).
//!
//! No database: every case here is about what happens *before* a handler
//! runs, and the whole point of the body limit is that nothing downstream
//! of it does.

use jwc::serve::{self, Incoming};
use jwc::workspace::Workspace;
use std::collections::HashMap;
use std::sync::Arc;

fn program(source: &str) -> Arc<jwc::exec::Program> {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.jwc"), source).expect("write");
    let ws = Workspace::load(dir.path()).expect("load");
    Arc::new(serve::load(&ws).unwrap_or_else(|e| panic!("{e}")))
}

async fn call(
    program: Arc<jwc::exec::Program>,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> jwc::exec::Response {
    let headers: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.to_string()))
        .collect();
    serve::handle(
        program,
        Incoming {
            method: method.to_string(),
            path: path.to_string(),
            query: Vec::new(),
            headers,
            body: body.as_bytes().to_vec(),
            peer_ip: "203.0.113.7".into(),
        },
    )
    .await
}

/// A middleware whose only effect is to answer. If it runs, the status says
/// so — which is how "the limit is checked before the chain" becomes an
/// observation rather than a claim.
const DENYING: &str = "namespace h;\n\
                       server { max_body_bytes = 16; }\n\
                       middleware Deny {\n\
                       \x20   throw Forbidden(\"the chain ran\");\n\
                       }\n\
                       routes \"/x\" use Deny {\n\
                       \x20   route POST \"\" { return json({ ok: true }); }\n\
                       }\n";

#[tokio::test]
async fn an_oversized_body_never_reaches_the_chain() {
    let p = program(DENYING);

    // Under the limit: the middleware runs and answers 403. That is the
    // control — without it, a 413 could mean the route simply does not
    // exist.
    let small = call(p.clone(), "POST", "/x", &[], "{}").await;
    assert_eq!(small.status, 403, "{}", small.body);
    assert!(small.body.contains("the chain ran"), "{}", small.body);

    // Over it: 413, and the middleware did not run. Anything the chain does
    // — a rate-limit bucket, a signature check, an audit row — on a body
    // the server was always going to refuse is work an attacker chose.
    let big = call(p, "POST", "/x", &[], &"x".repeat(64)).await;
    assert_eq!(big.status, 413, "{}", big.body);
    assert!(!big.body.contains("the chain ran"), "{}", big.body);
    assert!(big.body.contains("too large"), "{}", big.body);
}

const CORS: &str = "namespace h;\n\
                    server {\n\
                    \x20   cors {\n\
                    \x20       origins     = [\"https://app.example.com\"];\n\
                    \x20       methods     = [\"GET\", \"POST\"];\n\
                    \x20       headers     = [\"authorization\"];\n\
                    \x20       credentials = true;\n\
                    \x20       max_age     = \"600s\";\n\
                    \x20   }\n\
                    }\n\
                    routes \"/x\" {\n\
                    \x20   route GET \"\" { return json({ ok: true }); }\n\
                    }\n";

fn header<'a>(r: &'a jwc::exec::Response, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn a_preflight_is_answered_and_an_unlisted_origin_is_not() {
    let p = program(CORS);

    // `OPTIONS` reaches no handler — the browser is asking about the route,
    // not calling it (config.md §3.4).
    let pre = call(
        p.clone(),
        "OPTIONS",
        "/x",
        &[("Origin", "https://app.example.com")],
        "",
    )
    .await;
    assert_eq!(pre.status, 204);
    assert_eq!(
        header(&pre, "access-control-allow-origin"),
        Some("https://app.example.com")
    );
    assert_eq!(header(&pre, "access-control-allow-credentials"), Some("true"));
    assert_eq!(header(&pre, "access-control-allow-methods"), Some("GET, POST"));
    assert_eq!(header(&pre, "access-control-max-age"), Some("600"));
    // Any cache in front of this has to key on the origin, or one caller's
    // answer is served to another's.
    assert_eq!(header(&pre, "vary"), Some("Origin"));

    // A real call carries the headers too.
    let ok = call(
        p.clone(),
        "GET",
        "/x",
        &[("Origin", "https://app.example.com")],
        "",
    )
    .await;
    assert_eq!(ok.status, 200);
    assert!(header(&ok, "access-control-allow-origin").is_some());

    // An origin nobody listed gets no header, and the browser refuses on
    // its own. The request still runs: CORS is the browser's rule, not the
    // server's authorisation.
    let other = call(p, "GET", "/x", &[("Origin", "https://evil.example")], "").await;
    assert_eq!(other.status, 200);
    assert_eq!(header(&other, "access-control-allow-origin"), None);
}

#[tokio::test]
async fn with_no_cors_block_nothing_is_emitted() {
    // config.md §3.4 — absent means absent. A header emitted "just in case"
    // is a policy nobody wrote.
    let p = program(
        "namespace h;\nroutes \"/x\" {\n\
         \x20   route GET \"\" { return json({ ok: true }); }\n}\n",
    );
    let r = call(p, "GET", "/x", &[("Origin", "https://app.example.com")], "").await;
    assert_eq!(r.status, 200);
    assert_eq!(header(&r, "access-control-allow-origin"), None);
    assert_eq!(header(&r, "vary"), None);
}

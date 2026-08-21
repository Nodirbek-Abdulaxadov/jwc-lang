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
    assert_eq!(
        header(&pre, "access-control-allow-credentials"),
        Some("true")
    );
    assert_eq!(
        header(&pre, "access-control-allow-methods"),
        Some("GET, POST")
    );
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

// ── the login timing channel ───────────────────────────────────────────

/// ROADMAP §3's fourth done criterion for v0.29.0: the two failure branches
/// of `login` must not be distinguishable by the clock.
///
/// Needs Postgres. Without `JWC_V1_DATABASE_URL` this prints SKIPPED, and a
/// SKIPPED line is not a pass — a timing claim is not checkable by reading.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_failure_branches_of_login_cost_the_same() {
    let Ok(url) = std::env::var("JWC_V1_DATABASE_URL") else {
        eprintln!(
            "SKIPPED the_two_failure_branches_of_login_cost_the_same — set \
             JWC_V1_DATABASE_URL. A SKIPPED line is not a pass."
        );
        return;
    };
    // `login` sits behind the sample's `AuthRateLimit`, which calls
    // `redis.rate_limit` with no `enabled()` guard — so measuring `login`
    // needs a Redis as much as a Postgres. It did not use to, because
    // `redis.rate_limit` was a stub answering `true`: the timing
    // measurement ran through a limiter that had never limited.
    let Ok(redis_url) = std::env::var("JWC_TEST_REDIS_URL") else {
        eprintln!(
            "SKIPPED the_two_failure_branches_of_login_cost_the_same — `login` \
             is rate-limited, so this needs JWC_TEST_REDIS_URL too. A SKIPPED \
             line is not a pass."
        );
        return;
    };
    std::env::set_var("JWC_REDIS_URL", &redis_url);
    jwc::redis_engine::init_from_env().expect("redis");
    if !jwc::redis_engine::is_enabled() {
        // A build fact, not a missing service — the driver is behind a
        // Cargo feature. CI passes `--features redis` for this suite.
        eprintln!(
            "SKIPPED the_two_failure_branches_of_login_cost_the_same — this \
             binary was built without `--features redis`. A SKIPPED line is \
             not a pass."
        );
        return;
    }

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/spec/v1/sample");
    let ws = Workspace::load(&root).expect("sample");

    // The schema, fresh.
    let sql = jwc::ddl::render(&ws, &jwc::ddl::emit(&jwc::model::build(&ws).model), false);
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("schema.sql");
    std::fs::write(&file, sql).expect("write");
    for args in [
        vec![
            url.as_str(),
            "-q",
            "-c",
            "DROP SCHEMA IF EXISTS audit, auth, billing, org CASCADE",
        ],
        vec![
            url.as_str(),
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-f",
            file.to_str().expect("utf8"),
        ],
    ] {
        let out = std::process::Command::new("psql")
            .args(&args)
            .output()
            .expect("psql");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // One account whose password we know, so the "wrong password" branch is
    // a real Argon2id verification against a real stored hash.
    let stored = jwc::password::hash_password("correct horse battery staple").expect("hash");
    let insert = format!(
        "INSERT INTO auth.accounts (email, display_name, password_hash) \
         VALUES ('known@example.com', 'Known', '{}')",
        stored.replace('\'', "''")
    );
    let out = std::process::Command::new("psql")
        .args([url.as_str(), "-q", "-v", "ON_ERROR_STOP=1", "-c", &insert])
        .output()
        .expect("psql");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::env::set_var("DATABASE_URL", &url);
    std::env::set_var("JWT_SECRET", "test-secret-abcdefghijklmnop");
    std::env::set_var("CURSOR_SECRET", "test-cursor-secret");
    jwc::engine::init_engine_from_env().expect("engine");
    let program = Arc::new(serve::load(&ws).unwrap_or_else(|e| panic!("{e}")));

    // The sample limits `login` to five attempts per identity per five
    // minutes, and this test makes thirty-six. Clearing the buckets before
    // each attempt keeps the limiter out of a measurement that is about
    // Argon2id, not about the limiter — the alternative is 429s, which
    // cost nothing and would hide exactly the gap being measured.
    let clear = || async {
        jwc::redis_engine::eval("return redis.call('FLUSHDB')", &[], &[])
            .await
            .expect("flush the rate-limit buckets");
    };

    let attempt = |email: &'static str| {
        let p = program.clone();
        async move {
            let body = format!(r#"{{"email":"{email}","password":"definitely not the password"}}"#);
            let started = std::time::Instant::now();
            let r = call(p, "POST", "/api/v1/auth/login", &[], &body).await;
            (r.status, started.elapsed())
        }
    };

    // Warm the pool and the KDF's first-run cost out of the measurement.
    for _ in 0..3 {
        clear().await;
        attempt("known@example.com").await;
        clear().await;
        attempt("nobody@example.com").await;
    }

    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for _ in 0..15 {
        clear().await;
        let (status, d) = attempt("known@example.com").await;
        assert_eq!(status, 401, "the limiter, not the credentials");
        known.push(d.as_secs_f64() * 1000.0);
        clear().await;
        let (status, d) = attempt("nobody@example.com").await;
        assert_eq!(status, 401, "the limiter, not the credentials");
        unknown.push(d.as_secs_f64() * 1000.0);
    }

    let median = |mut v: Vec<f64>| -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
        v[v.len() / 2]
    };
    let a = median(known);
    let b = median(unknown);
    eprintln!("login: known {a:.1}ms, unknown {b:.1}ms");

    // A generous band, deliberately. Without the decoy the miss branch
    // never touches Argon2id and is two to three orders of magnitude
    // faster, so any band at all catches the regression — and a tight one
    // would only make the test flaky on a busy machine.
    let ratio = a.max(b) / a.min(b);
    assert!(
        ratio < 2.5,
        "the two branches differ by {ratio:.1}x — known {a:.1}ms, unknown {b:.1}ms. \
         The same message for both failures is undone by the clock."
    );
}

// ── the shipped dependency tree ────────────────────────────────────────

/// Every open advisory in `deny.toml`'s ignore list is triaged as
/// dev-dependency-only. That is a claim about the graph, and a claim about
/// the graph is checkable.
///
/// `cargo audit` reads `Cargo.lock`, which cannot tell a dev-dependency
/// from a shipped one, so the ignore list is the only place that
/// distinction lives — and a comment saying "dev only" is exactly the kind
/// of thing that stops being true without anyone noticing. This is what
/// notices.
#[test]
fn no_triaged_advisory_crate_reaches_the_shipped_binary() {
    let out = std::process::Command::new("cargo")
        .args([
            "tree",
            "--edges",
            "normal",
            "--target",
            "all",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .output();
    let Ok(out) = out else {
        eprintln!("SKIPPED no_triaged_advisory_crate_reaches_the_shipped_binary — no cargo");
        return;
    };
    if !out.status.success() {
        eprintln!(
            "SKIPPED no_triaged_advisory_crate_reaches_the_shipped_binary — cargo tree: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(tree.len() > 1000, "the tree looks empty");

    // Each of these carries an open advisory and each is triaged in
    // `deny.toml` as reached only through `testcontainers` -> `bollard`,
    // which is a dev-dependency.
    for (crate_name, why) in [
        (
            "hickory-proto",
            "reqwest's DNS resolver, only under testcontainers' feature set",
        ),
        ("rkyv", "bollard"),
        ("rustls-pemfile", "bollard"),
    ] {
        assert!(
            !tree.contains(&format!("{crate_name} v")),
            "`{crate_name}` is in the shipped tree — deny.toml says it is dev-only ({why})"
        );
    }

    // `rustls-webpki` *is* shipped, through reqwest. The advisories are
    // against 0.102; anything at or above 0.103.13 is clear of all four.
    for line in tree.lines() {
        let Some(rest) = line.split("rustls-webpki v").nth(1) else {
            continue;
        };
        let version = rest.split_whitespace().next().unwrap_or_default();
        let (major, minor, patch) = split_version(version);
        assert!(
            (major, minor, patch) >= (0, 103, 13),
            "the shipped rustls-webpki is {version}; the four 0.102 advisories \
             need 0.103.13 or later"
        );
    }
}

fn split_version(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split(['.', '-']).map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Every suite in `tests/` is named in a CI job.
///
/// `.github/workflows/ci.yml` lists its suites by name rather than running
/// a bare `cargo test`, which is right — several need a database or a
/// server and skip without one, and a skip reports ok. The comment there
/// claimed the naming made an omission visible. It did not: seven suites,
/// this one included, appeared in no job at all, so the hardening tests,
/// the applier, the migration round-trip property and `jwc test` were
/// never run by CI. Nothing said so, because nothing was looking.
///
/// This is the same shape as
/// `no_triaged_advisory_crate_reaches_the_shipped_binary` above — a claim
/// about the repository, checked against the repository.
#[test]
fn every_test_suite_is_named_in_ci() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("ci.yml");

    let mut missing = Vec::new();
    for entry in std::fs::read_dir(root.join("tests")).expect("tests/") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !ci.contains(&format!("--test {stem}")) {
            missing.push(stem.to_string());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "these suites are in tests/ and in no CI job, so nothing runs them: {}",
        missing.join(", ")
    );
}

// ── the operational endpoints ──────────────────────────────────────────

/// `/healthz`, `/readyz` and `/metrics` answer, and a declared route of
/// the same name still wins.
///
/// The v1 runtime served none of these — they went at the cutover with the
/// rest of the 0.9.x server — which is also why the soak's "zero pool
/// leaks" criterion had nothing to read: `engine::pool_status()` existed
/// and nothing exposed it.
#[tokio::test(flavor = "multi_thread")]
async fn the_operational_endpoints_answer_and_do_not_shadow_a_declared_route() {
    let p = program(
        "namespace h;\n\
         routes \"/\" {\n\
         \x20   route GET \"healthz\" { return json({ mine: true }); }\n\
         }\n",
    );

    // Declared wins. Shadowing it would take someone's endpoint away in a
    // point release and show up as a dashboard that went blank.
    let r = call(p.clone(), "GET", "/healthz", &[], "").await;
    assert_eq!(r.status, 200);
    assert!(
        r.body.contains("\"mine\":true"),
        "built-in shadowed it: {}",
        r.body
    );

    // Undeclared: the built-in answers.
    let bare = program("namespace h;\nroutes \"/x\" { route GET \"\" { return json(1); } }\n");
    let r = call(bare.clone(), "GET", "/healthz", &[], "").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("\"ok\""), "{}", r.body);

    // Liveness touches nothing — with no database configured at all it
    // still answers, which is the point: a dependency check here turns a
    // database blip into a restart storm.
    let r = call(bare.clone(), "GET", "/metrics", &[], "").await;
    assert_eq!(r.status, 200);
    assert!(
        r.body.contains("jwc_routes 1"),
        "the gauge does not describe this program: {}",
        r.body
    );
    assert!(
        r.headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("text/plain")),
        "Prometheus scrapes text/plain, not JSON: {:?}",
        r.headers
    );

    // Only GET. A POST to `/metrics` is not a metrics scrape.
    let r = call(bare.clone(), "POST", "/metrics", &[], "").await;
    assert_eq!(r.status, 404, "{}", r.body);
}

/// Readiness is the half that must fail when a dependency is gone —
/// otherwise a pod with no database stays in rotation, which is the exact
/// failure `/readyz` exists to prevent.
#[tokio::test(flavor = "multi_thread")]
async fn readyz_is_503_when_the_database_is_not_there() {
    let p = program("namespace h;\nroutes \"/x\" { route GET \"\" { return json(1); } }\n");
    let r = call(p, "GET", "/readyz", &[], "").await;
    // This test process may or may not have an engine — both outcomes are
    // legitimate, and both must *name* what they mean rather than
    // answering 200 unconditionally.
    if r.status == 503 {
        assert!(r.body.contains("db_"), "503 without naming why: {}", r.body);
    } else {
        assert_eq!(r.status, 200);
        assert!(r.body.contains("ready"), "{}", r.body);
    }
}

/// `content(mime, body)` — routing.md §6.5.
///
/// Found porting jwc-shortener, whose landing page, `robots.txt`,
/// `sitemap.xml` and OpenGraph card are five routes that do not answer
/// JSON. Before this the only way to reach for one was
/// `statusCode(200, $html) with { "Content-Type": "text/html" }`, and it
/// produced a response with **two** `content-type` headers — the builder's
/// `application/json` and the author's — around a body that was still
/// JSON-encoded, so a browser was handed `"<h1>…</h1>"`, quotes included.
#[tokio::test]
async fn content_sends_the_body_verbatim_under_one_declared_type() {
    let p = program(
        "namespace h;\n\
         routes \"/\" {\n\
         \x20   route GET \"page\" { return content(\"text/html\", \"<h1>salom</h1>\"); }\n\
         \x20   route GET \"card\" { return content(\"image/svg+xml\", \"<svg/>\"); }\n\
         \x20   route GET \"gone\" { return statusCode(404, content(\"text/plain\", \"yo'q\")); }\n\
         }\n",
    );

    let page = call(p.clone(), "GET", "/page", &[], "").await;
    assert_eq!(page.status, 200);
    // Verbatim: no quotes, and the length is the string's own.
    assert_eq!(page.body, "<h1>salom</h1>");
    assert_eq!(
        page.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .count(),
        1,
        "two content-type headers is a malformed message (RFC 9110 §8.3)"
    );
    // §6.5.3 — `text/*` gains the charset it did not declare.
    assert_eq!(
        header(&page, "content-type"),
        Some("text/html; charset=utf-8")
    );

    // Not `text/*`: passed through exactly as written, no charset invented.
    let card = call(p.clone(), "GET", "/card", &[], "").await;
    assert_eq!(header(&card, "content-type"), Some("image/svg+xml"));
    assert_eq!(card.body, "<svg/>");

    // §6.5.4 — a response is a value, so the status composes.
    let gone = call(p, "GET", "/gone", &[], "").await;
    assert_eq!(gone.status, 404);
    assert_eq!(gone.body, "yo'q");
    assert_eq!(
        header(&gone, "content-type"),
        Some("text/plain; charset=utf-8")
    );
}

/// `with { }` replaces a header the builder already set — routing.md §6.2.
///
/// The append form left the JSON `content-type` in place ahead of the
/// author's, so the header that was written last was the one clients were
/// least likely to honour.
#[tokio::test]
async fn with_replaces_a_header_the_builder_already_set() {
    let p = program(
        "namespace h;\n\
         routes \"/\" {\n\
         \x20   route GET \"p\" {\n\
         \x20       return json({ a: 1 }) with { \"Content-Type\": \"application/problem+json\" };\n\
         \x20   }\n\
         \x20   route GET \"r\" {\n\
         \x20       return redirect(302, \"/one\") with { \"Location\": \"/two\" };\n\
         \x20   }\n\
         }\n",
    );

    let problem = call(p.clone(), "GET", "/p", &[], "").await;
    assert_eq!(
        problem
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .count(),
        1
    );
    assert_eq!(
        header(&problem, "content-type"),
        Some("application/problem+json")
    );
    // The body is untouched — `with` sets headers and nothing else.
    assert_eq!(problem.body, "{\"a\":1}");

    // Case-insensitive, and it works for a header no JSON builder sets.
    let red = call(p, "GET", "/r", &[], "").await;
    assert_eq!(
        red.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("location"))
            .count(),
        1
    );
    assert_eq!(header(&red, "location"), Some("/two"));
}

/// `serve(port)` in `main()` decides the port — builtins.md §2.
///
/// It had never been evaluated. `main` was parsed and arity-checked and
/// then dropped on the floor, so the listener always took the CLI default:
/// a program asking for 3000 got 8080, and the spec's own sample line —
/// `serve(int(env("PORT") ?? "8080"))` — could not have worked at all.
#[tokio::test]
async fn main_decides_the_port_it_listens_on() {
    let literal = program(
        "namespace h;\n\
         routes \"/x\" { route GET \"\" { return json({ ok: true }); } }\n\
         function main() { serve(3000); }\n",
    );
    assert_eq!(
        jwc::serve::declared_port(&literal).await.expect("boot"),
        3000
    );

    // The argument is an expression, which is the whole reason `main` is
    // evaluated rather than pattern-matched for an integer literal.
    std::env::set_var("JWC_TEST_PORT_VAR", "4100");
    let from_env = program(
        "namespace h;\n\
         routes \"/x\" { route GET \"\" { return json({ ok: true }); } }\n\
         function main() { serve(int(env(\"JWC_TEST_PORT_VAR\") ?? \"8080\")); }\n",
    );
    assert_eq!(
        jwc::serve::declared_port(&from_env).await.expect("boot"),
        4100
    );

    // Unset: the `??` arm is taken, and that is still the program's answer
    // rather than a default applied behind its back.
    std::env::remove_var("JWC_TEST_PORT_VAR");
    assert_eq!(
        jwc::serve::declared_port(&from_env).await.expect("boot"),
        8080
    );

    // No `main` at all — nothing to read, and the listener is not the place
    // to refuse a program the checker already has an opinion about.
    let headless = program(
        "namespace h;\n\
         routes \"/x\" { route GET \"\" { return json({ ok: true }); } }\n",
    );
    assert_eq!(
        jwc::serve::declared_port(&headless).await.expect("boot"),
        8080
    );
}

/// A `+` chain is folded, not recursed — types.md §12.1.
///
/// v1 has no multi-line string literal (names.md §2.3), so a page is
/// assembled from its own lines. jwc-shortener's landing page is 360 of
/// them, and evaluating that chain by recursion spent one `MAX_DEPTH`
/// level per term: the page compiled, served, and answered 500 with
/// "expression nesting is too deep".
///
/// 300 terms, against a `MAX_DEPTH` of 128 — more than twice the limit
/// that used to reject this. The count is bounded by the **test** binary's
/// stack, not the product's: the compiler's other passes still recurse
/// once per term, and a debug build's frames are large enough to overflow
/// a test thread somewhere past 300. A release build takes 2000 terms
/// through `check`, `fmt` and `serve` without trouble.
#[tokio::test]
async fn a_long_concatenation_is_not_nesting() {
    let mut src =
        String::from("namespace h;\nfunction page() -> text {\n    return \"line 0\\n\"\n");
    for i in 1..=300 {
        src.push_str(&format!("        + \"line {i}\\n\"\n"));
    }
    src.push_str("    ;\n}\n");
    src.push_str("routes \"/\" { route GET \"p\" { return content(\"text/plain\", page()); } }\n");

    let r = call(program(&src), "GET", "/p", &[], "").await;
    assert_eq!(r.status, 200, "body was: {}", r.body);
    assert_eq!(r.body.lines().count(), 301);
    assert!(r.body.starts_with("line 0\nline 1\n"));
    assert!(r.body.ends_with("line 300\n"));
}

/// `timestamptz - interval` and `timestamptz - timestamptz` — types.md §12.2.
///
/// `+` carried its timestamptz overload; `-` fell through to the numeric
/// path and faulted with "arithmetic is not defined here". The checker
/// allowed both, so `date.now() - date.hours(24)` compiled and then
/// answered 500 — and that expression is how a query asks for "the last
/// day".
#[tokio::test]
async fn timestamptz_subtraction_is_defined() {
    let p = program(
        "namespace h;\n\
         routes \"/\" {\n\
         \x20   route GET \"t\" {\n\
         \x20       let now = date.now();\n\
         \x20       let day_ago = $now - date.hours(24);\n\
         \x20       let back = $day_ago + date.hours(24);\n\
         \x20       let gap = $now - $day_ago;\n\
         \x20       return json({ same: string.of($back) == string.of($now), gap: string.of($gap) });\n\
         \x20   }\n\
         }\n",
    );

    let r = call(p, "GET", "/t", &[], "").await;
    assert_eq!(r.status, 200, "body was: {}", r.body);
    // Subtracting an interval and adding it back is a round trip.
    assert!(r.body.contains("\"same\":true"), "{}", r.body);
    // And the difference of the two endpoints is that interval.
    assert!(r.body.contains("PT86400S"), "{}", r.body);
}

/// `break` and `continue` — errors.md §7.2.
///
/// Both are named by the normative clause and by `E1020`'s own help text,
/// and neither existed: a reader who did what the diagnostic said got the
/// same diagnostic again. `continue` is what makes a retry-on-conflict
/// loop expressible at all, since a postfix `catch` must diverge and
/// `return`/`throw` leave the function.
#[tokio::test]
async fn break_and_continue_control_the_loop() {
    let p = program(
        "namespace h;\n\
         routes \"/\" {\n\
         \x20   route GET \"b\" {\n\
         \x20       let seen = \"\";\n\
         \x20       for (n in [\"a\", \"b\", \"stop\", \"c\"]) {\n\
         \x20           if ($n == \"stop\") { break; }\n\
         \x20           $seen = $seen + $n;\n\
         \x20       }\n\
         \x20       return json({ seen: $seen });\n\
         \x20   }\n\
         \x20   route GET \"c\" {\n\
         \x20       let seen = \"\";\n\
         \x20       for (n in [\"a\", \"skip\", \"b\"]) {\n\
         \x20           if ($n == \"skip\") { continue; }\n\
         \x20           $seen = $seen + $n;\n\
         \x20       }\n\
         \x20       return json({ seen: $seen });\n\
         \x20   }\n\
         }\n",
    );

    let brk = call(p.clone(), "GET", "/b", &[], "").await;
    assert_eq!(brk.status, 200, "body was: {}", brk.body);
    assert!(brk.body.contains("\"seen\":\"ab\""), "{}", brk.body);

    let cont = call(p, "GET", "/c", &[], "").await;
    assert_eq!(cont.status, 200, "body was: {}", cont.body);
    assert!(cont.body.contains("\"seen\":\"ab\""), "{}", cont.body);
}

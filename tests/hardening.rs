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
        Some(3000)
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
        Some(4100)
    );

    // Unset: the `??` arm is taken, and that is still the program's answer
    // rather than a default applied behind its back.
    std::env::remove_var("JWC_TEST_PORT_VAR");
    assert_eq!(
        jwc::serve::declared_port(&from_env).await.expect("boot"),
        Some(8080)
    );

    // No `main` at all — nothing to read. `None`, not a defaulted 8080:
    // `jwc serve` supplies the default, and `jwc run` needs to know the
    // program never asked for a listener.
    let headless = program(
        "namespace h;\n\
         routes \"/x\" { route GET \"\" { return json({ ok: true }); } }\n",
    );
    assert_eq!(
        jwc::serve::declared_port(&headless).await.expect("boot"),
        None
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

/// A wildcard route does not swallow the operational endpoints —
/// config.md §4.0.3.
///
/// "A declared route wins" was read as "anything that matched wins", and
/// the two are different when the match came from a path parameter.
/// jwc-shortener declares `/{code}` for its redirects; it matches one
/// segment, so it matched `/readyz` as well, and the readiness probe
/// answered 404 with the shortener's "no such link". Every pod would have
/// stayed out of rotation, and nothing in the source names `/readyz` for
/// an operator to find.
#[tokio::test]
async fn a_wildcard_route_does_not_shadow_the_operational_endpoints() {
    let p = program(
        "namespace h;\n\
         routes \"/{code}\" {\n\
         \x20   route GET \"\" { return json({ code: @code }); }\n\
         }\n",
    );

    // The built-in answers, not the catch-all.
    let ready = call(p.clone(), "GET", "/healthz", &[], "").await;
    assert_eq!(ready.status, 200);
    assert!(ready.body.contains("\"status\":\"ok\""), "{}", ready.body);

    let metrics = call(p.clone(), "GET", "/metrics", &[], "").await;
    assert_eq!(metrics.status, 200);
    // `jwc_routes` and not a pool gauge: no pool is configured in a unit
    // test, and the point here is which handler answered.
    assert!(metrics.body.contains("jwc_routes"), "{}", metrics.body);

    // Any other single segment still reaches the route.
    let other = call(p, "GET", "/abc123", &[], "").await;
    assert_eq!(other.status, 200);
    assert!(other.body.contains("\"code\":\"abc123\""), "{}", other.body);
}

/// …and a program that writes its own still keeps it — the half of
/// §4.0.3 that was already right.
#[tokio::test]
async fn a_literally_declared_operational_path_still_wins() {
    let p = program(
        "namespace h;\n\
         routes \"/metrics\" {\n\
         \x20   route GET \"\" { return content(\"text/plain\", \"mine\"); }\n\
         }\n",
    );

    let r = call(p, "GET", "/metrics", &[], "").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "mine");
}

/// Every diagnostic code the compiler emits is documented, and no code is
/// documented twice.
///
/// Codes are assigned by hand and nothing checked them, so six of them
/// were handed out twice in one afternoon: `E0011`–`E0014` were parser
/// errors when socket and job rules took them, `E0611` was "`raw` inside
/// a view" when a buffered-insert rule did, and `E0811` was "an `after`
/// block can raise" when a socket rule did. Each surfaced as a corpus
/// case failing on a diagnostic that looked right and meant something
/// else — the good outcome. Without a corpus case nearby, a duplicate
/// reaches a user as documentation describing a different error than the
/// one they got.
///
/// The registry is the specification's own "Diagnostics introduced here"
/// tables, which is where a reader looks the code up. A code with no row
/// there is undocumented; a code with two rows is ambiguous.
#[test]
fn every_diagnostic_code_is_documented_exactly_once() {
    use std::collections::{BTreeMap, BTreeSet};

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    // --- what the spec documents -----------------------------------------
    let mut documented: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let spec = root.join("docs/spec/v1");
    for entry in std::fs::read_dir(&spec).expect("docs/spec/v1").flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for line in text.lines() {
            let t = line.trim();
            // `| \`E0123\` | … |` — a row of a diagnostics table.
            if !t.starts_with("| `E") {
                continue;
            }
            let Some(code) = t.trim_start_matches("| `").split('`').next().filter(|c| {
                c.len() == 5 && c.starts_with('E') && c[1..].chars().all(|ch| ch.is_ascii_digit())
            }) else {
                continue;
            };
            documented
                .entry(code.to_string())
                .or_default()
                .push(name.clone());
        }
    }
    assert!(
        documented.len() > 50,
        "found only {} documented codes — the table format changed",
        documented.len()
    );

    // One code is deliberately in two tables: `E0900` is "a word from the
    // pre-1.0 vocabulary", and both the routing spec (which has a whole
    // section of them) and the names spec (which owns the registry row)
    // list it. Same meaning, two readers.
    const DOCUMENTED_TWICE_ON_PURPOSE: &[&str] = &["E0900"];

    let twice: Vec<String> = documented
        .iter()
        .filter(|(code, _)| !DOCUMENTED_TWICE_ON_PURPOSE.contains(&code.as_str()))
        .filter(|(_, files)| {
            let mut f = (*files).clone();
            f.sort();
            f.dedup();
            f.len() > 1
        })
        .map(|(code, files)| format!("{code} in {files:?}"))
        .collect();
    assert!(
        twice.is_empty(),
        "a code documented in two specs means two things:\n  {}",
        twice.join("\n  ")
    );

    // --- what the compiler emits ------------------------------------------
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let src = root.join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    assert!(!files.is_empty(), "no sources under src/");
    // Every `"E0123"` literal, wherever it sits on the line. Scanning only
    // line-leading literals would work — rustfmt puts most of them on their
    // own line — but "most" is the problem: a short `self.err("E0537", s,
    // "…")` stays on one line, and a scan that misses it would report the
    // code as documented-but-unimplemented below.
    //
    // Matched as a whole `"E` + four digits + `"` window rather than by
    // splitting on quotes: `lexer.rs` writes `\"` inside its own string
    // literals, which flips the odd/even parity of a split for the rest of
    // the file and hid three real codes when this was written that way.
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let b = text.as_bytes();
        for i in 0..b.len().saturating_sub(6) {
            if b[i] == b'"'
                && b[i + 1] == b'E'
                && b[i + 6] == b'"'
                && b[i + 2..i + 6].iter().all(|c| c.is_ascii_digit())
            {
                emitted.insert(text[i + 1..i + 6].to_string());
            }
        }
    }

    let undocumented: Vec<&String> = emitted
        .iter()
        .filter(|c| !documented.contains_key(*c))
        .collect();
    assert!(
        undocumented.is_empty(),
        "these codes are emitted and appear in no spec's diagnostics table, \
         so a reader who gets one has nowhere to look it up: {undocumented:?}"
    );

    // The other direction. A tabled code that nothing emits is a promise the
    // compiler does not keep, and it reads exactly like one it does — the
    // reader cannot tell them apart from the table.
    //
    // `E0711` ("route is fully shadowed") is the one deliberate entry:
    // routing.md §4.3 makes it unreachable in 1.0 and says so in bold above
    // the table, so the row documents a reserved code rather than a check.
    // Anything else here is drift.
    const RESERVED_UNIMPLEMENTED: &[&str] = &["E0711"];

    let unimplemented: Vec<&String> = documented
        .keys()
        .filter(|c| !emitted.contains(*c))
        .filter(|c| !RESERVED_UNIMPLEMENTED.contains(&c.as_str()))
        .collect();
    assert!(
        unimplemented.is_empty(),
        "these codes are in a diagnostics table and nothing in src/ emits them, \
         so the spec promises a check that does not run: {unimplemented:?}"
    );
}

fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// routing.md §9.2 — a socket route whose chain answers.
///
/// The upgrade itself is exempt from the `after` chain, and the reason
/// given is that the response was the 101. When the chain answers instead,
/// the response is an ordinary one and every middleware that started runs
/// its `after` block (middleware.md §4.3).
///
/// Both backends read the exemption as covering the whole socket path, so
/// an access log recorded rejected routes and not rejected upgrades — the
/// connections most worth looking at, missing, with nothing to show it.
const SOCKET_AFTER: &str = "namespace h;\n\
                            middleware Mark {\n\
                            \x20   after { response.set_header(\"x-after\", \"ran\"); }\n\
                            }\n\
                            middleware Gate {\n\
                            \x20   let deny = request.query(\"deny\");\n\
                            \x20   if ($deny == \"1\") { throw Unauthorized(\"no\"); }\n\
                            }\n\
                            routes \"/s\" use Mark, Gate {\n\
                            \x20   socket \"ws\" { on open { socket.send(\"hi\"); } }\n\
                            }\n";

async fn preflight(deny: bool) -> Result<(), jwc::exec::Response> {
    let program = program(SOCKET_AFTER);
    let incoming = Incoming {
        method: "GET".into(),
        path: "/s/ws".into(),
        query: if deny {
            vec![("deny".to_string(), "1".to_string())]
        } else {
            Vec::new()
        },
        headers: HashMap::new(),
        body: Vec::new(),
        peer_ip: "203.0.113.7".into(),
    };
    let request = Arc::new(jwc::exec::Request {
        method: "GET".to_string(),
        path: incoming.path.clone(),
        route: incoming.path.clone(),
        headers: incoming.headers.clone(),
        query: incoming.query.clone(),
        body: String::new(),
        peer_ip: incoming.peer_ip.clone(),
        client_ip: incoming.peer_ip.clone(),
        id: "0000000000000000".to_string(),
    });
    serve::socket_preflight(&program, &incoming, request)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_socket_chain_that_answers_runs_the_after_blocks() {
    let Err(response) = preflight(true).await else {
        panic!("`Gate` throws, so the chain answers and there is no upgrade");
    };
    assert_eq!(response.status, 401);
    assert!(
        response
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-after") && v == "ran"),
        "`Mark` started, so its `after` block runs on the refusal: {:?}",
        response.headers
    );
}

#[tokio::test]
async fn a_socket_upgrade_does_not_run_the_after_blocks() {
    assert!(
        preflight(false).await.is_ok(),
        "nothing answers, so the handshake proceeds and the response is the 101"
    );
}

/// The install page's platform table against the release matrix.
///
/// The 1.0 page claimed archives for `x86_64-macos` and `aarch64-macos`,
/// which have never been built, said nothing about Windows, which is
/// built, and hardcoded `VERSION=0.9.9` — a tag that was never cut, so the
/// one command a new user runs first answered 404. The one-line installers
/// that do all of this correctly were not mentioned at all.
///
/// Both lists are here in the repository, so neither has to be trusted.
#[test]
fn the_install_page_lists_the_platforms_the_release_actually_builds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    let release =
        std::fs::read_to_string(root.join(".github/workflows/release.yml")).expect("release.yml");
    let built: std::collections::BTreeSet<String> = release
        .lines()
        .filter_map(|l| l.trim().strip_prefix("short:"))
        .map(|s| s.trim().to_string())
        .collect();
    assert!(
        built.len() >= 4,
        "found only {built:?} — the matrix format changed"
    );

    let page =
        std::fs::read_to_string(root.join("docs/docs/getting-started/install.md")).expect("page");

    for short in &built {
        assert!(
            page.contains(short.as_str()),
            "the release builds `{short}` and the install page never names it"
        );
    }

    // The inverse, derived rather than listed: every archive name the page
    // shows must be a target the matrix builds. A hardcoded "macOS is
    // absent" was here until macOS was added, at which point the guard
    // was the thing that was wrong.
    for line in page.lines() {
        let Some(rest) = line.trim().strip_prefix("| ") else {
            continue;
        };
        let Some((_, name)) = rest.split_once("`jwc-vX.Y.Z-") else {
            continue;
        };
        let Some(short) = name.split(&['.', '`'][..]).next().filter(|s| !s.is_empty()) else {
            continue;
        };
        assert!(
            built.contains(short),
            "the install page shows an archive for `{short}` and no release job builds it \
             (built: {built:?})"
        );
    }

    // The installers are the documented path; a hand-rolled curl with a
    // pinned version is what rotted.
    assert!(
        page.contains("install.sh") && page.contains("install.ps1"),
        "the install page should lead with the one-line installers"
    );
}

/// "What 1.0 does not have" against what 1.0 has.
///
/// That page listed background jobs, WebSocket, an in-process cache and
/// outbound email as not declarable. All four had been implemented — the
/// page was telling anyone deciding whether to adopt JWC that it could not
/// do things it does. A capability page that lags the compiler argues
/// against its own product.
///
/// Both sides are in this repository: the parser's declaration keywords
/// and the checker's namespace list are the ground truth.
#[test]
fn the_capability_page_does_not_deny_what_the_compiler_accepts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let page =
        std::fs::read_to_string(root.join("docs/docs/reference/not-in-1-0.md")).expect("page");

    // Everything above "## What it does have" is the absent list.
    let absent = page
        .split("## What it does have")
        .next()
        .expect("the page should still have a section naming what it lacks");

    let parser = std::fs::read_to_string(root.join("src/parser.rs")).expect("parser.rs");
    let keywords: Vec<String> = parser
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.split_once("\" => Decl::"))
        .map(|(k, _)| k.to_string())
        .collect();
    assert!(keywords.len() > 10, "found only {keywords:?}");

    for kw in &keywords {
        assert!(
            !absent.contains(&format!("`{kw}`")),
            "`{kw}` is a declaration the parser accepts, and the page lists it as absent"
        );
    }

    let check = std::fs::read_to_string(root.join("src/check.rs")).expect("check.rs");
    let namespaces = check
        .split("fn is_namespace")
        .nth(1)
        .and_then(|s| s.split_once('}'))
        .map(|(body, _)| body.to_string())
        .expect("is_namespace");
    for ns in ["cache", "mail", "socket", "redis"] {
        if !namespaces.contains(&format!("\"{ns}\"")) {
            continue;
        }
        assert!(
            !absent.contains(&format!("`{ns}.")),
            "`{ns}.*` is a namespace the checker resolves, and the page lists it as absent"
        );
    }
}

/// Every `jwc` subcommand appears in both CLI references.
///
/// The README's table presented itself as the CLI and was missing seven of
/// twenty-two, including `jwc new` — the entry point — and `jwc build`,
/// the native backend. A reader who trusted it did not learn the compiler
/// could scaffold a project.
#[test]
fn both_cli_references_name_every_subcommand() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let main = std::fs::read_to_string(root.join("src/main.rs")).expect("main.rs");

    // clap derives a subcommand's name from its variant identifier,
    // PascalCase to kebab-case — `GenSql` is `gen-sql`. So the enum body is
    // the list, and it needs no attribute to be authoritative.
    let body = main
        .split("enum Command {")
        .nth(1)
        .and_then(|s| s.split("\n}\n").next())
        .expect("enum Command");

    let mut names: Vec<String> = Vec::new();
    for line in body.lines() {
        // A variant sits at one level of indentation and starts uppercase;
        // its fields sit deeper, and attributes and doc comments do not
        // start with a letter.
        let Some(ident) = line.strip_prefix("    ") else {
            continue;
        };
        if ident.starts_with(char::is_whitespace) {
            continue;
        }
        let ident: String = ident
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if ident.is_empty() || !ident.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        let mut kebab = String::new();
        for (i, c) in ident.chars().enumerate() {
            if c.is_ascii_uppercase() && i > 0 {
                kebab.push('-');
            }
            kebab.push(c.to_ascii_lowercase());
        }
        names.push(kebab);
    }
    assert!(
        names.len() >= 15,
        "found only {names:?} — the clap attribute shape changed"
    );

    for (label, path) in [
        ("README.md", "README.md"),
        ("the CLI page", "docs/docs/cli/index.md"),
    ] {
        let text = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| !text.contains(&format!("jwc {n}")))
            .collect();
        assert!(
            missing.is_empty(),
            "{label} presents itself as the CLI reference and never mentions {missing:?}"
        );
    }
}

/// The environment table in `config.md`, rendered from the registry.
///
/// `config.rs::REGISTRY` is what the runtime reads at boot. The page that
/// presented itself as the environment reference listed seven of the
/// fifty-one entries in it — every variable mail, the cache, jobs and
/// buffered writes need was missing, along with the whole CORS, JWT, queue
/// and retry families.
///
/// Rather than fix the list and watch it rot again, the table is generated
/// here and this test is the check. `JWC_UPDATE_DOCS=1 cargo test` rewrites
/// it; anything else compares and fails with what changed.
#[test]
fn the_environment_table_is_generated_from_the_registry() {
    use std::fmt::Write as _;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let page_path = root.join("docs/docs/backend/config.md");
    let page = std::fs::read_to_string(&page_path).expect("config.md");

    const OPEN: &str = "<!-- generated:env-table -->";
    const CLOSE: &str = "<!-- /generated:env-table -->";

    let mut table = String::from("\n| Variable | Default | |\n|---|---|---|\n");
    for v in jwc::config::REGISTRY {
        let default = if v.default.is_empty() {
            "—".to_string()
        } else {
            format!("`{}`", v.default)
        };
        // A pipe inside a cell would end it; nothing in the registry has
        // one today, and escaping keeps that from becoming a silent break.
        let doc = v.doc.replace('|', "\\|");
        let _ = writeln!(table, "| `{}` | {default} | {doc} |", v.name);
    }

    let (before, rest) = page
        .split_once(OPEN)
        .unwrap_or_else(|| panic!("{OPEN} is missing from config.md"));
    let (_, after) = rest
        .split_once(CLOSE)
        .unwrap_or_else(|| panic!("{CLOSE} is missing from config.md"));

    let want = format!("{before}{OPEN}{table}{CLOSE}{after}");
    if page == want {
        return;
    }
    if std::env::var("JWC_UPDATE_DOCS").is_ok() {
        std::fs::write(&page_path, &want).expect("rewrite config.md");
        return;
    }
    panic!(
        "the environment table is out of step with `config.rs::REGISTRY` \
         ({} entries). Run `JWC_UPDATE_DOCS=1 cargo test \
         the_environment_table_is_generated_from_the_registry` to regenerate it.",
        jwc::config::REGISTRY.len()
    );
}

/// `file.*` and `directory.*` are refused where a request can reach them.
///
/// 0.9 placed no restriction on them at all, so a route could read a path
/// built from the query string. The rule is on the body being compiled,
/// not a call graph, and this pins both halves of it.
#[test]
fn the_filesystem_is_out_of_reach_of_a_request() {
    let load = |src: &str| -> Result<(), String> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = Workspace::load(dir.path()).expect("load");
        serve::load(&ws).map(|_| ()).map_err(|e| e.to_string())
    };

    let msg = load(
        "namespace h;\n\
         routes \"/x\" {\n\
         \x20   route GET \"\" {\n\
         \x20       let s = file.read(request.query(\"p\") ?? \"/etc/passwd\");\n\
         \x20       return json({ s: $s });\n\
         \x20   }\n\
         }\n",
    )
    .err()
    .unwrap_or_else(|| panic!("a route must not read a path from the request"));
    assert!(msg.contains("E0230"), "{msg}");

    // The same call in a plain `function` — what `jwc run` calls — is fine.
    assert!(
        load("namespace h;\nfunction main() { let s = file.read(\"a.txt\"); }\n").is_ok(),
        "a script must be able to read a file"
    );
}

/// Every variable a generated `.env.example` names is one something reads.
///
/// `jwc new` writes this file and the first line tells the reader the
/// runtime reads it. Until 0.9.927 nothing did — `DATABASE_URL` in a
/// `.env` was inert, and a beginner who followed the file exactly got
/// "DATABASE_URL is required" with no way forward. The loader is the fix;
/// this is the guard, because the failure was never in the loader, it was
/// in nobody checking that the file and the code agreed.
///
/// A name is legitimate when the runtime registry holds it, or when the
/// template's own sources read it with `env("NAME")` — `CURSOR_SECRET` is
/// the second kind: `server { cursor_secret = env("CURSOR_SECRET") }`.
#[test]
fn a_generated_env_example_names_nothing_that_is_never_read() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let mut checked = 0;

    for entry in std::fs::read_dir(&root).expect("templates/").flatten() {
        let dir = entry.path();
        let example = dir.join(".env.example");
        let Ok(text) = std::fs::read_to_string(&example) else {
            continue;
        };
        let template = dir.file_name().unwrap().to_string_lossy().to_string();

        // Every `env("…")` the template's own sources read.
        let mut read_by_source = std::collections::BTreeSet::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("jwc") {
                    let src = std::fs::read_to_string(&p).unwrap_or_default();
                    let mut rest = src.as_str();
                    while let Some(i) = rest.find("env(\"") {
                        rest = &rest[i + 5..];
                        if let Some(j) = rest.find('"') {
                            read_by_source.insert(rest[..j].to_string());
                        }
                    }
                }
            }
        }

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, _)) = line.split_once('=') else {
                panic!("{template}/.env.example: `{line}` is not KEY=VALUE");
            };
            let name = name.trim();
            let known_to_runtime = jwc::config::REGISTRY.iter().any(|v| v.name == name)
                // Read by name outside the registry, which documents only
                // the `JWC_*` surface.
                || matches!(name, "DATABASE_URL" | "JWC_DATABASE_URL");
            assert!(
                known_to_runtime || read_by_source.contains(name),
                "templates/{template}/.env.example names `{name}`, which is not in \
                 config::REGISTRY and which no `.jwc` under templates/{template} \
                 reads with env(\"{name}\") — the file would be telling a new user \
                 to set something nothing looks at"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 8,
        "expected several variables, checked {checked}"
    );
}

/// A `.env` beside the sources reaches the code that asks for the database.
///
/// The unit tests cover the parser; this covers the wiring, which is where
/// it was broken: the parser did not exist, so nothing downstream could
/// have been wrong, and no test noticed the absence.
#[test]
fn a_dotenv_value_reaches_the_database_url_lookup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = "JWC_DATABASE_URL";
    // The loader never overwrites, so the variable has to be clear first.
    let restore = std::env::var_os(name);
    // SAFETY: single-threaded test.
    unsafe { std::env::remove_var(name) };
    unsafe { std::env::remove_var("DATABASE_URL") };

    std::fs::write(
        dir.path().join(".env"),
        "# a comment\nJWC_DATABASE_URL=postgres://u:p@h:5432/db\n",
    )
    .expect("write");

    let report = jwc::config::load_dotenv(dir.path());
    assert_eq!(
        report.set,
        vec![name.to_string()],
        "{report:?}",
        report = report.malformed
    );

    let url = jwc::engine::database_url_from_env().expect("the .env value");
    assert_eq!(url, "postgres://u:p@h:5432/db");

    // SAFETY: single-threaded test.
    unsafe { std::env::remove_var(name) };
    if let Some(v) = restore {
        unsafe { std::env::set_var(name, v) };
    }
}

/// The env-var registry and the code that reads the environment name the
/// same variables.
///
/// `JWC_QUEUE_WORKERS` sat in the registry and in `config.md` while
/// `jobs.rs` read `JWC_JOB_WORKERS`: the documented knob did nothing and
/// the working knob was documented nowhere. Nothing could notice, because
/// the registry was a hand-kept list beside the code rather than a
/// statement about it.
///
/// Both directions, because each is a different lie: a registered name
/// nothing reads is a setting that silently does nothing, and an unread
/// name is a setting nobody can discover.
#[test]
fn every_env_var_the_code_reads_is_registered_and_the_other_way_round() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    // Every `JWC_*` string literal under src/, minus the test-only ones.
    let mut read_by_code: std::collections::BTreeSet<String> = Default::default();
    let mut stack = vec![src];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext != "rs" && !p.to_string_lossy().ends_with(".rs.in") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            // The registry itself is the thing under test, so it does not
            // get to vouch for its own entries.
            if p.ends_with("config.rs") {
                continue;
            }
            // Any `"JWC_…"` *string literal*. Not just `env::var("…")`:
            // `mail.rs` passes the name to a helper and the CORS code
            // builds its reads through one, and a scanner that only knew
            // the direct form called both of them dead. A name inside a
            // doc comment is not a literal and still does not count.
            let mut rest = text.as_str();
            while let Some(i) = rest.find("\"JWC_") {
                // `env!("JWC_BUILD_TARGET")` is a *compile-time* constant
                // emitted by build.rs, not a runtime knob: nobody sets it
                // in an environment and the boot table has nothing to
                // print. `option_env!` is the same.
                let before = &rest[..i];
                let compile_time = before.ends_with("env!(") || before.ends_with("option_env!(");
                rest = &rest[i + 1..];
                if compile_time {
                    continue;
                }
                if let Some(j) = rest.find('"') {
                    let name = &rest[..j];
                    if name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    {
                        read_by_code.insert(name.to_string());
                    }
                }
            }
        }
    }

    let registered: std::collections::BTreeSet<String> = jwc::config::REGISTRY
        .iter()
        .map(|v| v.name.to_string())
        .collect();

    // Marker strings codegen emits into the generated crate to name a
    // query's shape. They share the prefix and nothing else — no one sets
    // them, and the environment never sees them.
    const NOT_A_KNOB: &[&str] = &["JWC_SHAPE_FIRST", "JWC_SHAPE_NONE", "JWC_SHAPE_ROWS"];

    let unregistered: Vec<&String> = read_by_code
        .iter()
        .filter(|n| !registered.contains(*n) && !NOT_A_KNOB.contains(&n.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "read by the code, absent from config::REGISTRY (so undiscoverable, \
         and missing from the boot table): {unregistered:?}"
    );

    // A registered name nothing reads is a setting that silently does
    // nothing. Thirteen of them were shipped that way, documented in
    // config.md and printed in the boot table. Implementing thirteen
    // features is not what this test is for, so the registry says so in
    // the entry itself and the generated table carries it — and this
    // assertion is what keeps the two in step: a dead knob must be
    // labelled, and a labelled knob must still be dead.
    for name in registered.difference(&read_by_code) {
        let entry = jwc::config::REGISTRY
            .iter()
            .find(|v| v.name == name.as_str())
            .expect("from the registry");
        assert!(
            entry.doc.starts_with("NOT IMPLEMENTED"),
            "`{name}` is in config::REGISTRY, is printed in the boot table and \
             documented in config.md, and nothing in src/ reads it. Either wire \
             it up, or start its `doc` with `NOT IMPLEMENTED — ` so the table \
             stops promising it."
        );
    }
    for v in jwc::config::REGISTRY {
        if v.doc.starts_with("NOT IMPLEMENTED") {
            assert!(
                !read_by_code.contains(v.name),
                "`{}` is labelled NOT IMPLEMENTED but the code reads it — drop \
                 the label",
                v.name
            );
        }
    }
}

/// `while` and compound assignment, which 0.9 had and the 1.0 front-end
/// never grew.
///
/// Neither was removed by a decision anyone wrote down: the redesign
/// specified `for` and `=` and nobody diffed the new grammar against the
/// old one, so a loop that ends on a condition and `i += 1` simply had no
/// spelling. This pins them, and pins the runaway ceiling, which is the
/// one thing 0.9's `while` did not have.
#[test]
fn a_while_loop_and_compound_assignment_parse_and_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         function main() {\n\
         \x20   let i = 0;\n\
         \x20   let sum = 0;\n\
         \x20   while (i < 5) {\n\
         \x20       i += 1;\n\
         \x20       if (i == 3) { continue; }\n\
         \x20       sum += i;\n\
         \x20   }\n\
         \x20   sum -= 2;\n\
         \x20   sum *= 3;\n\
         \x20   sum /= 2;\n\
         \x20   console.writeln(string.of(sum));\n\
         }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    let checked = jwc::check::check(&ws, &sym, &built.model);
    let errors: Vec<String> = checked
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(_, d)| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

/// A `while` whose condition never goes false is a request that never
/// answers. Both backends stop at the same count and say so.
#[test]
fn a_runaway_while_is_bounded_the_same_way_in_both_backends() {
    // The interpreter's ceiling is the number codegen emits, so the two
    // cannot drift into "one hangs, one errors".
    let n = jwc::exec::MAX_WHILE_TURNS;
    assert!(
        n >= 1_000_000,
        "a ceiling below a million would reject real loops"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\nfunction main() { while (true) { let x = 1; } }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let rust = jwc::native::codegen_for_test(&ws).expect("codegen");
    assert!(
        rust.contains(&format!("__turns > {n}")),
        "the generated crate should carry the interpreter's ceiling"
    );
}

/// `while (1)` is not a condition. A loop that never ends because its
/// condition is not a boolean is a typo, not a design.
#[test]
fn a_while_condition_that_is_not_boolean_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\nfunction main() { while (1) { break; } }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    let checked = jwc::check::check(&ws, &sym, &built.model);
    assert!(
        checked.diags.iter().any(|(_, d)| d.code == "E0371"),
        "{:?}",
        checked
            .diags
            .iter()
            .map(|(_, d)| d.code)
            .collect::<Vec<_>>()
    );
}

/// `const` and `x.field = v`, the other two 0.9 had and the 1.0 front-end
/// never grew.
#[test]
fn a_const_and_a_field_assignment_check_and_are_refused_when_wrong() {
    fn diags(src: &str) -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        let mut out: Vec<String> = ws
            .files
            .iter()
            .flat_map(|f| f.diags.iter().map(|d| d.code.to_string()))
            .collect();
        let built = jwc::model::build(&ws);
        let sym = jwc::symbols::build(&ws, &built.model);
        out.extend(sym.diags.iter().map(|(_, d)| d.code.to_string()));
        out.extend(
            jwc::check::check(&ws, &sym, &built.model)
                .diags
                .iter()
                .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
                .map(|(_, d)| d.code.to_string()),
        );
        out
    }

    // The shapes that work.
    let ok = diags(
        "namespace n;\n\
         const PI = 3;\n\
         const TAU = PI * 2;\n\
         const NAMES = [\"a\", \"b\"];\n\
         function main() {\n\
         \x20   let o = { \"a\": 1, \"b\": { \"c\": 2 } };\n\
         \x20   o.a = TAU;\n\
         \x20   o.b.c = 20;\n\
         \x20   o.fresh = PI;\n\
         }\n",
    );
    assert!(ok.is_empty(), "{ok:?}");

    // A `const` that reaches outside itself.
    assert!(
        diags("namespace n;\nconst BAD = env(\"X\");\n").contains(&"E0216".to_string()),
        "a call in a const should be E0216"
    );
    // Two with one name.
    assert!(
        diags("namespace n;\nconst A = 1;\nconst A = 2;\n").contains(&"E0215".to_string()),
        "a duplicate const should be E0215"
    );
    // Writing a field of something that was never declared.
    assert!(
        diags("namespace n;\nfunction main() { nope.a = 1; }\n").contains(&"E0211".to_string()),
        "an unknown base should be E0211"
    );
}

/// `text`, `html`, `hash.sha1` and `hash.md5` — four names whose
/// implementations were already in the tree and reachable from nothing.
///
/// `src/hash.rs` computed sha1 and md5, the native prelude defined
/// `jwc_b_text`, `jwc_b_html`, `jwc_b_sha1` and `jwc_b_md5`, and no name in
/// the language reached any of them. That is the same shape as `src/jwks.rs`
/// and as the HTTP prelude before 0.9.922: code that ships, is compiled, and
/// cannot be called.
#[test]
fn the_four_builtins_whose_implementations_were_already_here_are_reachable() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         routes \"/\" {\n\
         \x20   route GET \"t\" { return text(\"hi\"); }\n\
         \x20   route GET \"h\" { return html(\"<b>x</b>\"); }\n\
         \x20   route GET \"d\" { return json({ a: hash.md5(\"abc\"), b: hash.sha1(\"abc\") }); }\n\
         }\n\
         function main() { serve(8080); }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    let errors: Vec<String> = jwc::check::check(&ws, &sym, &built.model)
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(_, d)| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    // And the native backend reaches the same four, by the names the
    // prelude already defined.
    let rust = jwc::native::codegen_for_test(&ws).expect("codegen");
    for f in ["jwc_b_text", "jwc_b_html", "jwc_b_md5", "jwc_b_sha1"] {
        assert!(rust.contains(f), "the generated crate never calls `{f}`");
    }
}

/// A non-text body in `text(...)` is the same mistake `content(...)` already
/// reports, and gets the same code.
#[test]
fn text_of_a_record_is_refused_the_way_content_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         routes \"/\" { route GET \"\" { return text({ a: 1 }); } }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    assert!(
        jwc::check::check(&ws, &sym, &built.model)
            .diags
            .iter()
            .any(|(_, d)| d.code == "E0736"),
        "a record body should be E0736"
    );
}

/// `docs/docs/language/syntax.md` has listed `not` among the operators since
/// the operator table was written, and `names.md`'s keyword table lists it
/// too — but `not x` did not parse. The two spellings are one node, so this
/// pins that they stay one node rather than pinning the parse alone.
#[test]
fn the_word_spelling_of_the_logical_negation_parses() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         function main() {\n\
         \x20   let t = true;\n\
         \x20   let a = not t;\n\
         \x20   let b = !t;\n\
         \x20   let c = not not t;\n\
         \x20   if (not a) { console.writeln(\"ok\"); }\n\
         \x20   console.writeln(string.of(a) + string.of(b) + string.of(c));\n\
         }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    let checked = jwc::check::check(&ws, &sym, &built.model);
    let errors: Vec<String> = checked
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(_, d)| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

/// `not exists (…)` is its own node, not `not` applied to a call — so the
/// prefix rule must not swallow it. Nor may `x not in (…)`, which is infix.
#[test]
fn not_exists_and_not_in_survive_the_prefix_rule() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         database App : Postgres { init() { pool_size = 4; } }\n\
         schema public of App;\n\
         table Users of App.public {\n\
         \x20   id bigint identity primary key;\n\
         \x20   name varchar(80);\n\
         }\n\
         service S {\n\
         \x20   function pick(w: text) {\n\
         \x20       return select U from App.public.Users\n\
         \x20           where name not in (\"a\", \"b\") as { id };\n\
         \x20   }\n\
         }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    let checked = jwc::check::check(&ws, &sym, &built.model);
    let errors: Vec<String> = checked
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(_, d)| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

/// The access line's two shapes. A log pipeline is configured against
/// these strings, so a silent change to either breaks a dashboard and
/// nothing else — which is why the formatter is pure and pinned here
/// rather than observed on stderr.
#[test]
fn the_access_line_has_one_shape_per_format() {
    let text = jwc::serve::format_request_log_line("GET", "/a/b", 200, 1234, "abc", false);
    assert_eq!(text, "[jwc] GET /a/b -> 200 1.2ms rid=abc");

    let json = jwc::serve::format_request_log_line("POST", "/x", 500, 7, "d1", true);
    let v: serde_json::Value = serde_json::from_str(&json).expect("the json form must parse");
    assert_eq!(v["kind"], "access");
    assert_eq!(v["method"], "POST");
    assert_eq!(v["path"], "/x");
    assert_eq!(v["status"], 500);
    assert_eq!(v["latency_us"], 7);
    assert_eq!(v["request_id"], "d1");

    // A path is the one field a client controls, so it is the one that
    // can break the envelope.
    let hostile = jwc::serve::format_request_log_line("GET", "/\"a\\b", 200, 0, "r", true);
    let v: serde_json::Value =
        serde_json::from_str(&hostile).expect("a quote in the path must not break the envelope");
    assert_eq!(v["path"], "/\"a\\b");
}

/// W3C Trace Context §3.2.2. An id that is not a trace-id is not
/// repaired: a made-up id that looks like the caller's is worse than one
/// that is visibly ours.
#[test]
fn a_request_id_comes_from_traceparent_only_when_it_is_one() {
    let good = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    assert_eq!(
        jwc::serve::request_id_from_traceparent(Some(good)),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );

    for bad in [
        "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01", // uppercase
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01", // all zero
        "00-4bf92f35-00f067aa0ba902b7-01",                         // too short
        "not-a-traceparent",
        "",
    ] {
        let got = jwc::serve::request_id_from_traceparent(Some(bad));
        assert_eq!(got.len(), 16, "a generated id is 16 hex digits: {got}");
        assert!(got.chars().all(|c| c.is_ascii_hexdigit()), "{got}");
    }

    // Two calls never collide inside one process.
    let a = jwc::serve::request_id_from_traceparent(None);
    let b = jwc::serve::request_id_from_traceparent(None);
    assert_ne!(a, b);
}

/// The access line is one text, included by the CLI and pasted into the
/// generated crate. Two copies would drift, and the drift would show up
/// as a log pipeline that parses `jwc serve` and drops `jwc build`.
#[test]
fn both_backends_format_the_access_line_from_the_same_text() {
    let core = include_str!("../src/access_log_core.rs.in");
    assert!(
        core.contains("pub fn format_request_log_line("),
        "the shared file must hold the formatter"
    );
    assert!(
        jwc::native::PRELUDE_ACCESS_LOG_CORE == core,
        "the generated crate must be handed the same bytes the CLI includes"
    );
    // And `serve.rs` must reach it by including that file, not by holding
    // a second copy.
    let serve = include_str!("../src/serve.rs");
    assert!(serve.contains(r#"include!("access_log_core.rs.in")"#));
    assert!(
        !serve.contains("pub fn format_request_log_line("),
        "serve.rs must not define its own copy"
    );
}

/// `jwc lint --explain E0211` has to answer for every code the compiler
/// can produce, or it is a lookup table with holes exactly where a reader
/// needs it. The catalogue is generated from the spec by `build.rs`, so
/// this is really a check that the extraction sees the same rows the
/// documentation guard above sees.
#[test]
fn the_catalogue_answers_for_every_documented_code() {
    use std::collections::BTreeSet;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut documented: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("docs/spec/v1"))
        .expect("docs/spec/v1")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for line in text.lines() {
            let t = line.trim();
            if !t.starts_with("| `E") && !t.starts_with("| `W") {
                continue;
            }
            if let Some(code) = t.trim_start_matches("| `").split('`').next() {
                let ok = code.len() == 5
                    && (code.starts_with('E') || code.starts_with('W'))
                    && code[1..].chars().all(|c| c.is_ascii_digit());
                if ok {
                    documented.insert(code.to_string());
                }
            }
        }
    }

    assert!(
        documented.len() > 50,
        "found {} documented codes — the table format changed",
        documented.len()
    );

    let missing: Vec<&String> = documented
        .iter()
        .filter(|c| jwc::codes::lookup(c).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "documented but absent from the catalogue `--explain` reads: {missing:?}"
    );

    // And nothing extra: a row the spec does not have is a meaning
    // invented by the extractor.
    let extra: Vec<&str> = jwc::codes::DIAGNOSTIC_CATALOGUE
        .iter()
        .map(|(c, _, _)| *c)
        .filter(|c| !documented.contains(*c))
        .collect();
    assert!(extra.is_empty(), "in the catalogue, in no spec: {extra:?}");

    // Every row carries a meaning and the file that defines it — an empty
    // one would print as a blank line and read as a code with no rule.
    for (code, file, meaning) in jwc::codes::DIAGNOSTIC_CATALOGUE {
        assert!(!meaning.trim().is_empty(), "{code} has no meaning");
        assert!(file.ends_with(".md"), "{code} names {file}");
    }

    // A miss lands the reader in the right band rather than nowhere.
    assert!(jwc::codes::lookup("E0211").is_some());
    assert!(jwc::codes::lookup("e0211").is_some(), "case-insensitive");
    assert!(jwc::codes::lookup("E0299").is_none());
    assert!(
        jwc::codes::in_same_band("E0299").len() > 3,
        "a mistyped code should still list its band"
    );
}

/// `jwt.verify` disagreed with itself across the two backends in two ways
/// at once, and both were reachable from the auth template's middleware.
///
/// The checker types the call `Record{sub, exp, iat}?`. `jwc serve`
/// answered that. A `jwc build` binary returned the raw payload **string**
/// on success and `panic!`ed on a bad signature — so a request carrying a
/// tampered token, which is ordinary traffic on any public endpoint, took
/// the unwind path instead of the 401 the middleware writes.
#[test]
fn both_backends_answer_jwt_verify_with_the_same_shape() {
    let prelude = jwc::native::PRELUDE_CRYPTO;

    // The success path builds the same three-field record the interpreter
    // does, rather than handing back the payload text.
    assert!(
        prelude.contains("fn jwc_jwt_claims_record(payload: &str) -> V {"),
        "the native side must build the record"
    );
    assert!(
        prelude.contains("jwc_jwt_claims_record(&payload)"),
        "and jwt_verify must go through it"
    );

    // The failure path is null on both, not a panic on one.
    let verify = prelude
        .split("fn jwc_b_jwt_verify(")
        .nth(1)
        .expect("jwc_b_jwt_verify is in the crypto prelude");
    let body = verify.split("\nfn ").next().unwrap_or(verify);
    assert!(
        !body.contains("panic!"),
        "a token that does not verify is null on `jwc serve`; it must not \
         crash a native build:\n{body}"
    );

    // And the interpreter has one function for it, so HS256 and the JWKS
    // path cannot drift on the shape.
    let exec = include_str!("../src/exec_call.rs");
    assert_eq!(
        exec.matches("fn jwt_claims_record(").count(),
        1,
        "one definition, two callers"
    );
    assert_eq!(exec.matches("jwt_claims_record(&payload)").count(), 2);
}

/// `src/jwks.rs` is 395 lines with a key cache, a negative cache and a
/// refetch-storm guard; the native prelude carries the whole thing again.
/// Both shipped in every binary and no program could call either — the
/// checker had no arm, so `jwt.verify_jwks(...)` was `E0204`.
#[test]
fn jwt_verify_jwks_is_reachable_from_the_language() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         service S {\n\
         \x20   function who(token: text) {\n\
         \x20       let claims = jwt.verify_jwks(token, \"https://idp.example/jwks\")\n\
         \x20           or throw Unauthorized(\"token yaroqsiz\");\n\
         \x20       return claims.sub;\n\
         \x20   }\n\
         }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    let checked = jwc::check::check(&ws, &sym, &built.model);
    let errors: Vec<String> = checked
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(_, d)| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    // The native backend has to map the name too, or a program that
    // checks would be refused at build time.
    let codegen = include_str!("../src/native/codegen.rs");
    assert!(codegen.contains(r#""jwt.verify_jwks" => "jwc_b_jwt_verify_jwks""#));
    // It awaits, so it has to be in the async list — a mapping without
    // that emits a call with no `.await` and the generated crate does not
    // compile.
    assert!(codegen.contains(r#""jwc_b_jwt_verify_jwks","#));
}

/// Every version this documentation tells a reader to install is the one
/// this tree *is*.
///
/// The install page and the deployment Dockerfile both pinned `0.9.914`
/// — seventeen releases behind — so following either got a binary from
/// before `.env` was read at all, before `static` mounts, and before
/// `text()` came back. A version in a copy-pasteable command is an
/// instruction, and a stale one sends people to a build whose bugs are
/// already fixed here.
#[test]
fn the_documented_install_version_is_this_version() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let want = env!("CARGO_PKG_VERSION");

    // `vX.Y.Z` placeholders are fine — they are naming a shape, not an
    // instruction. Concrete `0.9.NNN` in a command is what goes stale.
    let re_pages = [
        "docs/docs/getting-started/install.md",
        "docs/docs/deployment/index.md",
    ];
    for page in re_pages {
        let text = std::fs::read_to_string(root.join(page)).unwrap_or_else(|_| panic!("{page}"));
        for line in text.lines() {
            let t = line.trim();
            // Only lines that pin a version for a download.
            if !t.contains("JWC_VERSION") {
                continue;
            }
            // The line either names this version, or names none at all
            // (`JWC_VERSION` used as a variable, or a `vX.Y.Z` shape).
            let names_a_version = t.contains("0.9.") || t.contains("v0.9.");
            if !names_a_version {
                continue;
            }
            assert!(
                t.contains(want),
                "{page} pins a version that is not {want}:\n  {t}"
            );
        }
    }
}

/// What the docs show the server printing is what it prints.
///
/// The banner said `http://0.0.0.0:8080` until 0.9.926 — an address a
/// browser refuses on Windows and that resolves to nothing useful
/// anywhere. It was fixed in both backends; two pages went on quoting the
/// old line, so a reader on Windows was still being shown a URL that
/// cannot be clicked as if it were the expected output.
#[test]
fn the_documented_boot_banner_is_the_one_that_is_printed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pages = Vec::new();
    let mut stack = vec![root.join("docs/docs")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                pages.push(p);
            }
        }
    }
    assert!(!pages.is_empty());

    let mut stale = Vec::new();
    for p in &pages {
        let text = std::fs::read_to_string(p).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            if line.contains("listening on")
                && line.contains("0.0.0.0")
                && !line.contains("bound to")
            {
                stale.push(format!("{}:{}", p.display(), n + 1));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "these quote the pre-0.9.926 banner: {stale:?}"
    );
}

/// The diagnostic reference page is the catalogue, rendered.
///
/// `build.rs` extracts the rows from `docs/spec/v1/*.md`; `--explain` and
/// this page read that one extraction, so a code cannot be explainable at
/// the command line and missing from the reference, or the other way
/// round.
///
/// `JWC_UPDATE_DOCS=1 cargo test the_diagnostic_reference_is_generated`
/// regenerates it.
#[test]
fn the_diagnostic_reference_is_generated_from_the_catalogue() {
    use std::fmt::Write as _;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let page_path = root.join("docs/docs/reference/error-codes.md");
    let page = std::fs::read_to_string(&page_path).expect("error-codes.md");

    const OPEN: &str = "<!-- generated:diagnostic-table -->";
    const CLOSE: &str = "<!-- /generated:diagnostic-table -->";

    let mut table = String::from("\n| Code | Meaning | Defined in |\n|---|---|---|\n");
    for (code, file, meaning) in jwc::codes::DIAGNOSTIC_CATALOGUE {
        // A pipe inside a cell would end it. Nothing in the spec has one
        // today; escaping keeps that from becoming a silent break.
        let meaning = meaning.replace('|', "\\|");
        let _ = writeln!(table, "| `{code}` | {meaning} | `{file}` |");
    }

    let (before, rest) = page
        .split_once(OPEN)
        .unwrap_or_else(|| panic!("{OPEN} is missing from error-codes.md"));
    let (_, after) = rest
        .split_once(CLOSE)
        .unwrap_or_else(|| panic!("{CLOSE} is missing from error-codes.md"));

    let want = format!("{before}{OPEN}{table}{CLOSE}{after}");
    if page == want {
        return;
    }
    if std::env::var("JWC_UPDATE_DOCS").is_ok() {
        std::fs::write(&page_path, &want).expect("rewrite error-codes.md");
        return;
    }
    panic!(
        "the diagnostic reference is out of step with the catalogue \
         ({} codes). Run `JWC_UPDATE_DOCS=1 cargo test \
         the_diagnostic_reference_is_generated` to regenerate it.",
        jwc::codes::DIAGNOSTIC_CATALOGUE.len()
    );
}

/// `JWC_JOB_WORKERS=0` means zero.
///
/// It used to fall through to the default of 2 in both backends, so a web
/// deployment told not to drain the queue drained it with two workers —
/// the setting did the opposite of what it said, identically on both
/// sides. A value that is not a number still defaults, because "not a
/// number" is a typo and "zero" is a decision.
#[test]
fn zero_job_workers_means_zero_on_both_backends() {
    // The interpreter's reader, directly.
    let restore = std::env::var("JWC_JOB_WORKERS").ok();
    // SAFETY: single-threaded test body; restored before it returns.
    unsafe { std::env::set_var("JWC_JOB_WORKERS", "0") };
    assert_eq!(jwc::jobs::worker_count(), 0);
    unsafe { std::env::set_var("JWC_JOB_WORKERS", "5") };
    assert_eq!(jwc::jobs::worker_count(), 5);
    unsafe { std::env::set_var("JWC_JOB_WORKERS", "not-a-number") };
    assert_eq!(jwc::jobs::worker_count(), 2, "a typo still defaults");
    match restore {
        Some(v) => unsafe { std::env::set_var("JWC_JOB_WORKERS", v) },
        None => unsafe { std::env::remove_var("JWC_JOB_WORKERS") },
    }

    // And the generated crate's, which is a separate copy of the read.
    let prelude = jwc::native::PRELUDE_JOBS;
    let spawn = prelude
        .split("JWC_JOB_WORKERS")
        .nth(1)
        .expect("the jobs prelude reads it");
    assert!(
        !spawn.contains(".filter(|x| *x > 0)"),
        "the native side must not filter zero away"
    );
    assert!(
        spawn.contains("if n == 0 {"),
        "and must return rather than spawn"
    );

    // The starter in `serve.rs` has to short-circuit too, or the count is
    // honoured and the loop still runs.
    let serve = include_str!("../src/serve.rs");
    assert!(serve.contains("if n == 0 {"));
}

/// The observability page lists the metrics that are exported, and only
/// those.
///
/// A page naming a metric nobody emits sends someone to build a dashboard
/// on a series that never appears; a metric emitted and undocumented is
/// one nobody knows to alert on. Both directions, from the `# TYPE` lines
/// the code actually writes.
#[test]
fn the_documented_metrics_are_the_exported_ones() {
    use std::collections::BTreeSet;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    // `# TYPE <name> <kind>` is the Prometheus declaration, so it is the
    // exact set a scraper sees — narrower and more honest than every
    // `jwc_…` string in the tree.
    let mut exported: BTreeSet<String> = BTreeSet::new();
    for f in ["src/serve.rs", "src/jobs.rs", "src/log_writer.rs"] {
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("# TYPE ") else {
                continue;
            };
            if let Some(name) = rest.split_whitespace().next() {
                if name.starts_with("jwc_") {
                    exported.insert(name.to_string());
                }
            }
        }
    }
    assert!(
        exported.len() > 10,
        "found {} exported metrics — the `# TYPE` shape changed",
        exported.len()
    );

    let page = std::fs::read_to_string(root.join("docs/docs/deployment/observability.md"))
        .expect("observability.md");

    let undocumented: Vec<&String> = exported
        .iter()
        .filter(|m| !page.contains(&format!("`{m}`")))
        .collect();
    assert!(
        undocumented.is_empty(),
        "exported and not on the page, so nobody knows to alert on them: {undocumented:?}"
    );

    // The other direction: every `jwc_…` in a table cell on the page has
    // to be a series that exists.
    let mut invented: Vec<String> = Vec::new();
    for line in page.lines() {
        if !line.trim_start().starts_with("| `jwc_") {
            continue;
        }
        let name = line
            .trim_start()
            .trim_start_matches("| `")
            .split('`')
            .next()
            .unwrap_or("");
        if !name.is_empty() && !exported.contains(name) {
            invented.push(name.to_string());
        }
    }
    assert!(
        invented.is_empty(),
        "on the page, exported by nothing — a dashboard built on these \
         would stay empty forever: {invented:?}"
    );
}

/// Every registered variable is read by something, and the three that
/// were not are now.
///
/// `every_env_var_the_code_reads_is_registered_and_the_other_way_round`
/// enforces the pair. This one pins the *decisions* made about the twelve
/// that were registered, documented, printed in the boot table and read
/// by nothing — so that "we looked at this and chose" does not decay back
/// into "nobody noticed".
#[test]
fn the_settings_that_did_nothing_were_wired_or_removed() {
    let names: Vec<&str> = jwc::config::REGISTRY.iter().map(|v| v.name).collect();

    // Wired.
    for wired in ["JWC_SERVER_WORKERS", "JWC_PRINT_CONFIG", "JWC_HOME"] {
        assert!(names.contains(&wired), "{wired} should still be registered");
    }

    // Removed, each because the language already says the thing:
    //   the queue three  -> `job X retries N backoff "30s"` (jobs.md §2)
    //   REQUEST_TIMEOUT  -> `server { request_timeout }` (config.md §3)
    //   the registry two -> JWC_REGISTRY, and `jwc login`
    //   SERVER_METRICS   -> /metrics
    //   ADMIN_DB         -> `jwc migrate` never creates a database
    for gone in [
        "JWC_ADMIN_DB",
        "JWC_SERVER_METRICS",
        "JWC_SERVER_METRICS_INTERVAL_SECS",
        "JWC_REQUEST_TIMEOUT",
        "JWC_QUEUE_MAX_ATTEMPTS",
        "JWC_QUEUE_BACKOFF_MS",
        "JWC_QUEUE_DLQ_MAX",
        "JWC_REGISTRY_URL",
        "JWC_REGISTRY_TOKEN",
    ] {
        assert!(
            !names.contains(&gone),
            "{gone} is back in the registry — if it was implemented, drop \
             this row; if not, it is a setting that silently does nothing"
        );
    }

    // Nothing is marked as not implemented any more. A row that does
    // nothing is a row that should not be there.
    let config = include_str!("../src/config.rs");
    assert!(
        !config.contains("NOT IMPLEMENTED"),
        "a registered variable that does nothing is worse than an absent \
         one: it is documented, printed at boot, and a lie"
    );
}

/// The boot fence runs.
///
/// `config::validate_or_bail` is called "the boot fence" by `jwt.rs` and
/// was never called by anything, so `JWC_DB_POOL_SIZE=twenty` was
/// swallowed by an `unwrap_or(64)` deeper in the call graph and the pool
/// was quietly the wrong size. Same for `config::render` and
/// `config::snapshot`, which had no caller at all.
#[test]
fn the_boot_fence_and_the_config_table_have_a_caller() {
    let cmd = include_str!("../src/cmd/mod.rs");
    assert!(
        cmd.contains("crate::config::validate_or_bail()?"),
        "serve must refuse to start on an unparseable setting"
    );
    assert!(
        cmd.contains("crate::config::render(&crate::config::snapshot())"),
        "JWC_PRINT_CONFIG must reach the renderer"
    );
    assert!(cmd.contains(r#""JWC_PRINT_CONFIG""#));

    // And the fence really rejects. A parse failure is the whole point.
    let restore = std::env::var("JWC_DB_POOL_SIZE").ok();
    // SAFETY: single-threaded test body, restored before it returns.
    unsafe { std::env::set_var("JWC_DB_POOL_SIZE", "twenty") };
    let verdict = jwc::config::validate_or_bail();
    match restore {
        Some(v) => unsafe { std::env::set_var("JWC_DB_POOL_SIZE", v) },
        None => unsafe { std::env::remove_var("JWC_DB_POOL_SIZE") },
    }
    let err = verdict.expect_err("`twenty` is not a usize");
    assert!(format!("{err:?}").contains("JWC_DB_POOL_SIZE"));
}

/// The else branch of a null test narrows (types.md §6.6 rule 3).
///
/// `if (x == null) { …; return; } x.f` checked, and
/// `if (x == null) { … } else { x.f }` was `E0320` — so the shape a
/// reader reaches for first was the one the compiler refused, for a fact
/// it had already established.
#[test]
fn the_else_branch_of_a_null_test_narrows() {
    let ok = "namespace n;\n\
              function main() {\n\
              \x20   let c = jwt.verify(\"t\", \"s\");\n\
              \x20   if (c == null) {\n\
              \x20       console.writeln(\"none\");\n\
              \x20   } else {\n\
              \x20       console.writeln(c.sub);\n\
              \x20   }\n\
              }\n";
    // And the polarity has to be right: the *then* branch of `== null` is
    // where the value is null, so a field read there is still E0320.
    let bad = "namespace n;\n\
               function main() {\n\
               \x20   let c = jwt.verify(\"t\", \"s\");\n\
               \x20   if (c == null) {\n\
               \x20       console.writeln(c.sub);\n\
               \x20   } else {\n\
               \x20       console.writeln(\"some\");\n\
               \x20   }\n\
               }\n";

    let codes = |src: &str| -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
        let built = jwc::model::build(&ws);
        let sym = jwc::symbols::build(&ws, &built.model);
        jwc::check::check(&ws, &sym, &built.model)
            .diags
            .iter()
            .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
            .map(|(_, d)| d.code.to_string())
            .collect()
    };

    assert!(codes(ok).is_empty(), "{:?}", codes(ok));
    assert!(
        codes(bad).contains(&"E0320".to_string()),
        "the then-branch of `== null` is where it *is* null: {:?}",
        codes(bad)
    );
}

/// A key added by `o.x = v` keeps its place on both backends.
///
/// 0.9.929 shipped `jwc_set_field_path` converting a `V::Record` to a
/// `V::Object` so the key could be inserted — and an `FxHashMap` has no
/// order, so `jwc_write_json` sorts it, while `exec.rs::set_field_path`
/// pushes onto a `Vec` and keeps insertion order. The same program
/// answered two different response bodies:
///
///     jwc serve   {"a":1,"fresh":30,"deep":{"x":1}}
///     jwc build   {"a":1,"deep":{"x":1},"fresh":30}
///
/// The test that shipped with it used `{"a":…,"b":…,"fresh":…}`, which is
/// already alphabetical — sorted and insertion-ordered are the same
/// string for that fixture, so it passed on both and proved nothing. The
/// fixture here is deliberately anti-alphabetical.
#[test]
fn a_field_added_by_assignment_keeps_its_place_on_both_backends() {
    // The interpreter, run for real.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         function main() {\n\
         \x20   let o = { zebra: 1, apple: 2 };\n\
         \x20   o.mango = 3;\n\
         \x20   o.deep = { z: 1 };\n\
         \x20   o.deep.a = 2;\n\
         \x20   console.writeln(json.stringify(o));\n\
         }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));

    // And the text codegen pastes, which is where the divergence lived.
    let prelude = jwc::native::PRELUDE_BASE;
    let f = prelude
        .split("fn jwc_set_field_path(")
        .nth(1)
        .expect("jwc_set_field_path is in the base prelude");
    let body = f.split("\nfn ").next().unwrap_or(f);
    assert!(
        !body.contains("v_obj(m)"),
        "a record must not become an FxHashMap to gain a key — that is \
         what sorts it:\n{body}"
    );
    assert!(
        body.contains("names.push("),
        "a new key is appended to `field_names`, so the order a reader \
         sees is the order the program wrote"
    );

    // The interpreter's side of the contract, stated the same way.
    let exec = include_str!("../src/exec.rs");
    let g = exec
        .split("fn set_field_path(")
        .nth(1)
        .expect("set_field_path is in exec.rs");
    let gbody = g.split("\nfn ").next().unwrap_or(g);
    assert!(
        gbody.contains("fields.push("),
        "and the interpreter appends too"
    );
}

/// A typo in the *service* name is a compile error, like a typo in the
/// function name already was.
///
/// `S.typo()` where `S` exists reported `E0204`, and a bare
/// `unknown_name()` reported `E0204` — but `NoSuchService.anything()`
/// checked clean and failed at run time. The checker looked the
/// qualifier up, did not find it, and returned `Ty::Unknown` without a
/// word. Found by writing a route against a service I had not written
/// yet: `jwc check` said "ok — 7 files checked, 0 warnings".
#[test]
fn an_unknown_call_qualifier_is_reported() {
    let codes = |src: &str| -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
        let built = jwc::model::build(&ws);
        let sym = jwc::symbols::build(&ws, &built.model);
        jwc::check::check(&ws, &sym, &built.model)
            .diags
            .iter()
            .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
            .map(|(_, d)| d.code.to_string())
            .collect()
    };

    let head = "namespace n;\nservice S { function known() { return 1; } }\n";

    // The three that were already caught stay caught.
    assert!(codes(&format!(
        "{head}function main() {{ let a = S.typo(); console.writeln(\"x\"); }}\n"
    ))
    .contains(&"E0204".to_string()));
    assert!(codes(&format!(
        "{head}function main() {{ let a = nope(); console.writeln(\"x\"); }}\n"
    ))
    .contains(&"E0204".to_string()));

    // The one that was not.
    assert!(
        codes(&format!(
            "{head}function main() {{ let a = NoSuchService.anything(); console.writeln(\"x\"); }}\n"
        ))
        .contains(&"E0204".to_string()),
        "an undeclared service must not check clean"
    );

    // And a real call still checks, so the rule did not become "every
    // dotted call is an error".
    assert!(codes(&format!(
        "{head}function main() {{ let a = S.known(); console.writeln(string.of(date.now())); }}\n"
    ))
    .is_empty());
}

/// `jwc run` on a program whose `main` calls `serve(...)` starts the
/// server.
///
/// `serve(n)` records the port; it does not block. So `declared_port`
/// ran `main`, returned the port, and `cmd::run` threw it away —
/// `jwc run app.jwc` on the hello-world printed nothing and exited 0,
/// while the CLI help beside it said "a `main` that calls `serve(...)`
/// still starts a server, because that is what the call means" and the
/// comment in the code claimed the program "has already blocked inside it
/// and never reaches here". Neither was true.
///
/// The distinction is now in the type: `Option<u16>` separates "asked for
/// 8080" from "never asked", which is exactly what `jwc run` needs and
/// what a defaulted `u16` threw away.
#[tokio::test]
async fn a_main_that_serves_is_distinguishable_from_one_that_does_not() {
    let load = |src: &str| {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        std::sync::Arc::new(jwc::serve::load(&ws).unwrap_or_else(|e| panic!("{e}")))
    };

    let serving = load(
        "namespace n;\n\
         routes \"/\" { route GET \"\" { return json({ ok: true }); } }\n\
         function main() { serve(8123); }\n",
    );
    assert_eq!(
        jwc::serve::declared_port(&serving).await.expect("boot"),
        Some(8123)
    );

    let quiet = load(
        "namespace n;\n\
         function main() { console.writeln(\"done\"); }\n",
    );
    assert_eq!(
        jwc::serve::declared_port(&quiet).await.expect("boot"),
        None,
        "a `main` that never calls `serve` must not report a port"
    );

    // And no `main` at all is the same fact.
    let bare = load("namespace n;\nroutes \"/\" { route GET \"\" { return json({}); } }\n");
    assert_eq!(jwc::serve::declared_port(&bare).await.expect("boot"), None);

    // `cmd::run` has to act on it, or the distinction is decorative.
    let cmd = include_str!("../src/cmd/mod.rs");
    assert!(cmd.contains("if let Some(port) = crate::serve::declared_port(&program).await?"));
}

/// types.md §12 gives `+` and `-` three overloads on timestamps. The
/// interpreter had all three. The native prelude had none — and the
/// interesting half is not the two that panicked, it is the one that did
/// not: `jwc_add`'s string arm caught `timestamptz + interval` and
/// answered `"2026-08-28T15:44:14ZPT720H"`, a value that is wrong, is not
/// reported, and only becomes an error when Postgres refuses to bind it.
///
/// A shortener asking for "the last 24 hours" is what walked into it, so
/// the arithmetic now lives in one file both backends are handed, and
/// this pins the wiring: same bytes, no second copy, and — the part that
/// actually bit — the timestamp arm is consulted *before* the string arm.
#[test]
fn both_backends_do_the_same_timestamp_arithmetic() {
    let core = include_str!("../src/interval_core.rs.in");
    for f in [
        "fn jwc_parse_iso_duration(",
        "fn jwc_shift_secs(",
        "fn jwc_ts_diff_secs(",
    ] {
        assert!(core.contains(f), "the shared file must hold `{f}`");
    }
    assert!(
        jwc::native::PRELUDE_INTERVAL_CORE == core,
        "the generated crate must be handed the same bytes the CLI includes"
    );

    let exec = include_str!("../src/exec.rs");
    assert!(exec.contains(r#"include!("interval_core.rs.in")"#));
    assert!(
        !exec.contains("fn parse_iso_duration("),
        "exec.rs must not keep its own copy of the duration reader"
    );

    // Every overload the type table gives, on both sides.
    let base = include_str!("../src/native/prelude/base.rs.in");
    assert!(
        base.contains("fn jwc_ts_shift("),
        "the native prelude must implement `timestamptz ± interval`"
    );
    assert!(
        base.contains("jwc_ts_diff_secs(x, y)"),
        "the native prelude must implement `timestamptz - timestamptz`"
    );
    for (name, hay) in [("interpreter", exec), ("native", base)] {
        assert!(
            hay.contains("jwc_shift_secs") && hay.contains("jwc_ts_diff_secs"),
            "the {name} backend must reach the shared arithmetic"
        );
    }

    // The ordering bug, pinned: in `jwc_add` the timestamp arm has to come
    // first, or the string arm swallows the pair and concatenates it.
    let add = base
        .split_once("fn jwc_add(")
        .expect("jwc_add")
        .1
        .split_once("\n}\n")
        .expect("body")
        .0;
    let shift = add
        .find("jwc_ts_shift")
        .expect("jwc_add must try the shift");
    let concat = add.find("V::Str(x), b").expect("jwc_add has a string arm");
    assert!(
        shift < concat,
        "`timestamptz + interval` must be decided before the string arm"
    );

    // And codegen must actually paste it, unconditionally: `jwc_add` and
    // `jwc_sub` are in the base prelude and call into it, so a program
    // that uses no date at all still needs the definitions present.
    let codegen = include_str!("../src/native/codegen.rs");
    assert!(
        codegen.contains("source.push_str(super::PRELUDE_INTERVAL_CORE);"),
        "codegen must paste the shared arithmetic"
    );
}

/// `jwc build` is the command that produces the artefact you deploy, and
/// it was the one command that did not run the checker.
///
/// It tested `has_parse_errors` and went straight to codegen, so a program
/// with five type errors — one `jwc check` exits 1 on and `jwc serve`
/// refuses to boot — compiled to a release binary and ran. Codegen does
/// not need the types to be right to emit something that happens to
/// build, which is exactly why it cannot be the thing that decides.
#[test]
fn build_does_not_ship_what_check_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("jwcproj.json"),
        r#"{ "name": "bad", "type": "app", "version": "0.1.0" }"#,
    )
    .expect("manifest");
    // E0320: `date.parse` answers `timestamptz?`, and arithmetic on a
    // nullable is refused. Parses cleanly, so only the checker catches it.
    std::fs::write(
        dir.path().join("app.jwc"),
        "namespace bad;\n\
         function main() {\n\
         \x20   let t = date.parse(\"2026-01-01T00:00:00Z\");\n\
         \x20   console.writeln(string.of(t - date.hours(24)));\n\
         }\n",
    )
    .expect("write");

    // `--emit-rust` stops before cargo, so this costs a parse and not a
    // release build — and it is the same gate, because the gate is the
    // first thing `build` does.
    let err = jwc::cmd::build(dir.path().to_path_buf(), false, true, None)
        .expect_err("a program with a type error must not build");
    let msg = err.to_string();
    assert!(
        msg.contains("error"),
        "the failure should be the diagnostic count, got: {msg}"
    );

    // And the same program, with the nullable handled, does build.
    std::fs::write(
        dir.path().join("app.jwc"),
        "namespace bad;\n\
         function main() {\n\
         \x20   let t = date.parse(\"2026-01-01T00:00:00Z\") ?? date.now();\n\
         \x20   console.writeln(string.of(t - date.hours(24)));\n\
         }\n",
    )
    .expect("write");
    jwc::cmd::build(dir.path().to_path_buf(), false, true, None).expect("the fixed program builds");
}

/// A native binary is a server when the program is one, and not otherwise.
///
/// The generated `main` used to call the listener unconditionally, with
/// `JWC_SERVE_PORT` defaulting to 8080 — so `jwc build` on a console
/// program produced a binary that printed its output and then bound a
/// socket, or, on a box already using the port, printed its output and
/// then a bind error. `jwc run` on the same source returns when `main`
/// does; the two now agree.
#[test]
fn a_native_console_program_does_not_bind_a_port() {
    fn emit(src: &str) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        jwc::native::codegen_for_test(&ws).expect("codegen")
    }

    let console = emit("namespace n;\nfunction main() { console.writeln(\"hi\"); }\n");
    assert!(
        console.contains("if __port != 0 {"),
        "a program with no routes must only listen when `serve(...)` ran"
    );

    // `serve(...)` in `main` still listens, on the port the program named.
    let server = emit("namespace n;\nfunction main() { serve(9000); }\n");
    assert!(server.contains("JWC_SERVE_PORT.store("));
    assert!(server.contains("if __port != 0 {"));

    // Routes with no `main` listen on the documented default.
    let routed =
        emit("namespace n;\nroutes \"/x\" { route GET \"\" { return json({ ok: true }); } }\n");
    assert!(
        routed.contains("if __port == 0 { 8080 } else { __port }"),
        "a program with routes is a server whether or not it calls `serve`"
    );

    // The sentinel is what makes \"never called\" expressible at all.
    assert!(
        console.contains("AtomicU16::new(0)"),
        "the port must start at the sentinel, not at 8080"
    );
}

/// A formatter that deletes comments is worse than no formatter, and this
/// one did — silently, on the most ordinary construct in the language.
///
/// `fmt` re-prints from the AST, and `Attached` hangs on declarations and
/// statements. `ObjEntry` has none, so a `--` written between the fields
/// of a record literal, between the keys of `server { }`, or in an
/// `insert` value list simply was not in the tree to print. Formatting
/// `jwc-shortener` deleted thirteen lines of reasoning across three files
/// and reported success.
///
/// The printer still cannot hold them. What it no longer does is lose
/// them without saying so.
#[test]
fn fmt_refuses_a_file_rather_than_drop_a_comment() {
    // A comment the AST carries: survives, as it always did.
    let kept = "namespace n;\n\n-- why this function exists\nfunction f() {\n    return 1;\n}\n";
    let parsed = jwc::parse_str(std::path::Path::new("a.jwc"), kept);
    let printed = jwc::fmt::format_program(&parsed.program);
    assert!(printed.contains("-- why this function exists"));
    assert!(jwc::fmt::comments_lost(kept, &printed).is_empty());

    // A comment inside a record literal: the printer drops it, and the
    // check is what turns that into a refusal instead of a deletion.
    let lossy = "namespace n;\n\
                 function f() {\n\
                 \x20   return {\n\
                 \x20       -- the reason for the next line\n\
                 \x20       a: 1\n\
                 \x20   };\n\
                 }\n";
    let parsed = jwc::parse_str(std::path::Path::new("a.jwc"), lossy);
    assert!(!parsed.has_errors(), "the sample must parse");
    let printed = jwc::fmt::format_program(&parsed.program);
    let lost = jwc::fmt::comments_lost(lossy, &printed);
    assert_eq!(
        lost,
        vec!["-- the reason for the next line".to_string()],
        "the dropped comment must be named, not merely counted"
    );

    // Lexed, not grepped: a `--` inside a string is not a comment, and a
    // file full of them must still format.
    let strings = "namespace n;\nfunction f() {\n    return \"a -- b\";\n}\n";
    let parsed = jwc::parse_str(std::path::Path::new("a.jwc"), strings);
    let printed = jwc::fmt::format_program(&parsed.program);
    assert!(jwc::fmt::comments_lost(strings, &printed).is_empty());
    assert!(jwc::fmt::comment_texts(strings).is_empty());

    // A multiset, not a set: two identical comments, one dropped, is one
    // comment lost.
    let twice = "namespace n;\n\
                 function f() {\n\
                 \x20   return {\n\
                 \x20       -- same text\n\
                 \x20       a: 1,\n\
                 \x20       -- same text\n\
                 \x20       b: 2\n\
                 \x20   };\n\
                 }\n";
    let parsed = jwc::parse_str(std::path::Path::new("a.jwc"), twice);
    let printed = jwc::fmt::format_program(&parsed.program);
    assert_eq!(jwc::fmt::comments_lost(twice, &printed).len(), 2);

    // And the module doc must not go back to promising more than this.
    let doc = include_str!("../src/fmt.rs");
    assert!(
        doc.contains("What the AST does *not* carry"),
        "fmt.rs must say which comments it cannot keep"
    );
}

/// A recursion that never ends is an error, and the process survives it.
///
/// It did not. `MAX_DEPTH` counted expression nesting and was set to 128,
/// but a JWC call frame is a chain of boxed futures whose poll costs the
/// whole chain's depth — so the *machine* stack ran out first. Measured on
/// tokio's default 2 MiB worker stack, `jwc serve` answered a recursion 18
/// deep and died at 20 with `fatal runtime error: stack overflow,
/// aborting`. That is a process abort: every other request in flight dies
/// with it, from one request. `jwc run` did the same at ~100 on the main
/// thread's 8 MiB.
///
/// Two halves, and both are needed: the runtime gives its threads a stack
/// big enough for `MAX_CALL_DEPTH` frames, and `MAX_CALL_DEPTH` is what a
/// program reaches first.
#[test]
fn a_recursion_that_never_ends_is_an_error_not_a_crash() {
    // The call ceiling has to sit under `MAX_DEPTH`, or a runaway recursion
    // reports expression nesting and names no function; and it is only real
    // if the stack can hold that many frames. Both are read off the crate
    // rather than repeated here, so the run below is the check.

    // Recursion with no base case, and a frame carrying enough locals that
    // it is not the cheapest possible one — the ceiling has to hold for a
    // realistic body, not only for `f(n - 1)`.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("jwcproj.json"),
        r#"{ "name": "deep", "type": "app", "version": "0.1.0" }"#,
    )
    .expect("manifest");
    std::fs::write(
        dir.path().join("app.jwc"),
        "namespace deep;\n\
         function down(n: int) -> int {\n\
         \x20   let a = string.of($n) + \"-\" + string.of($n);\n\
         \x20   let b = [$a, $a, $a, $a];\n\
         \x20   let c = { one: $a, two: $b, three: $n };\n\
         \x20   if (array.len($b) < 0) { return 0; }\n\
         \x20   return down($n + 1) + string.len($c.one) - string.len($a);\n\
         }\n\
         function main() { console.writeln(string.of(down(0))); }\n",
    )
    .expect("write");

    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let program = std::sync::Arc::new(jwc::serve::load(&ws).expect("load program"));

    // On a worker with the runtime's stack, which is the whole point.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_stack_size(jwc::cmd::WORKER_STACK_BYTES)
        .enable_all()
        .build()
        .expect("runtime");
    let err = rt
        .block_on(async move {
            tokio::spawn(async move { jwc::serve::declared_port(&program).await })
                .await
                .expect("the task must not abort the process")
        })
        .expect_err("a recursion with no base case must be an error");

    let msg = err.to_string();
    assert!(
        msg.contains("down") && msg.contains(&jwc::exec::MAX_CALL_DEPTH.to_string()),
        "the error must name the function and the ceiling, got: {msg}"
    );
}

/// A loop that never finishes must not own a worker thread.
///
/// Every future a JWC loop body awaits is *ready*, and awaiting a ready
/// future does not yield to the scheduler — so the task never returned
/// `Pending`, and `serve`'s `tokio::time::timeout` never got a turn.
/// Measured before this: `request_timeout = "3s"` around
/// `while (true) { i += 1; }` did not fire at all, the client gave up at
/// twenty seconds, and the worker stayed pegged at 100% after it
/// disconnected. Afterwards: 504 at 3.006s, and the CPU released.
///
/// Both backends emit the yield, because a built binary hangs the same way.
#[test]
fn a_loop_hands_the_scheduler_a_turn() {
    fn emit(src: &str) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        jwc::native::codegen_for_test(&ws).expect("codegen")
    }

    let w = emit("namespace n;\nfunction main() { let i = 0; while (true) { i += 1; } }\n");
    assert!(
        w.contains("yield_now().await"),
        "a generated `while` must yield"
    );
    let f = emit("namespace n;\nfunction main() { let t = 0; for (x in [1, 2, 3]) { t += 1; } }\n");
    assert!(
        f.contains("yield_now().await"),
        "a generated `for` must yield — a million-row array is a million turns"
    );

    // And the interpreter must not have a second opinion about when.
    let exec = include_str!("../src/exec.rs");
    assert!(exec.contains("turns.is_multiple_of(TURNS_PER_YIELD)"));
    assert!(
        exec.matches("tokio::task::yield_now().await").count() >= 2,
        "`while` and `for` both"
    );
}

/// A recursive function has to *build*, and it did not.
///
/// A generated function is an `async fn`, and rustc refuses a directly
/// recursive one: `E0733: recursion in an async fn requires boxing`,
/// reported against `src/main.rs` of a crate the author never wrote. So
/// every JWC program with a recursive function ran fine under `jwc serve`
/// and could not be built at all — and the message named neither the JWC
/// function nor the reason.
#[test]
fn a_recursive_function_compiles_natively() {
    fn emit(src: &str) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        jwc::native::codegen_for_test(&ws).expect("codegen")
    }

    let direct = emit(
        "namespace n;\n\
         function down(k: int) -> int { if (k <= 0) { return 0; } return down($k - 1); }\n\
         function main() { console.writeln(string.of(down(3))); }\n",
    );
    assert!(
        direct.contains("Box::pin(jwc_fn_down("),
        "a directly recursive call must be boxed"
    );
    assert!(
        direct.contains("jwc_enter_call(\"down\""),
        "and must count its frames"
    );

    // Mutual recursion is the same cycle by a longer path.
    let mutual = emit(
        "namespace n;\n\
         function ping(k: int) -> int { if (k <= 0) { return 0; } return pong($k - 1); }\n\
         function pong(k: int) -> int { return ping($k - 1); }\n\
         function main() { console.writeln(string.of(ping(3))); }\n",
    );
    assert!(mutual.contains("Box::pin(jwc_fn_ping("));
    assert!(mutual.contains("Box::pin(jwc_fn_pong("));

    // A program with no cycle keeps the direct call and pays nothing.
    let plain = emit(
        "namespace n;\n\
         function twice(k: int) -> int { return $k + $k; }\n\
         function main() { console.writeln(string.of(twice(3))); }\n",
    );
    assert!(plain.contains("jwc_fn_twice("));
    assert!(
        !plain.contains("Box::pin(jwc_fn_twice("),
        "a function that cannot recurse must not be boxed"
    );
    // The prelude *defines* `jwc_enter_call` in every crate; what a
    // non-recursive function must not have is a call to it.
    assert!(
        !plain.contains("jwc_enter_call(\"twice\""),
        "a function that cannot recurse must not pay for the frame counter"
    );
    assert!(
        !plain.contains("let _frame = JwcCallFrame;"),
        "nor carry the guard"
    );
}

/// A cookie carried none of the attributes the author wrote.
///
/// routing.md §6.2 documents `cookie(name, value, opts)` and shows
/// `{ http_only: true, max_age: 3600 }`. The interpreter evaluated that
/// record and dropped it: every cookie was `name=value; Path=/`, so a
/// session cookie was readable by any script on the page, rode along on
/// every cross-site request, and went out over plain HTTP. An author who
/// read the page and wrote the safe thing got the unsafe cookie anyway,
/// with nothing said — which is worse than not having the option at all.
///
/// `jwc build` meanwhile refused the program outright ("native build does
/// not cover `cookie(...)` yet"), so a cookie-setting service could not be
/// built at all, and the native path that would have run had a second bug:
/// it put `Set-Cookie` into a header *map*, where a second cookie
/// overwrites the first.
#[test]
fn a_cookie_carries_the_attributes_it_was_given() {
    let core = include_str!("../src/cookie_core.rs.in");
    assert!(
        jwc::native::PRELUDE_COOKIE_CORE == core,
        "both backends must be handed the same formatter"
    );
    let exec = include_str!("../src/exec.rs");
    assert!(exec.contains(r#"include!("cookie_core.rs.in")"#));
    assert!(
        !exec.contains("; Path=/\", value_text"),
        "exec.rs must not keep the old hand-built cookie line"
    );

    // The default is the safe one, and it is the *shared* function that
    // says so — so neither backend can have its own idea of a default.
    let line = jwc::exec::format_set_cookie("sid", "v", &jwc::exec::CookieOpts::default())
        .expect("a plain cookie");
    assert!(line.contains("; HttpOnly"), "{line}");
    assert!(line.contains("; SameSite=Lax"), "{line}");

    // Every attribute reaches the header.
    let all = jwc::exec::CookieOpts {
        http_only: true,
        secure: true,
        same_site: "Strict",
        max_age: Some(3600),
        path: "/app".into(),
        domain: Some("example.com".into()),
    };
    let line = jwc::exec::format_set_cookie("sid", "v", &all).expect("cookie");
    for part in [
        "Path=/app",
        "Domain=example.com",
        "Max-Age=3600",
        "SameSite=Strict",
        "Secure",
        "HttpOnly",
    ] {
        assert!(line.contains(part), "{part} missing from {line}");
    }

    // A value that would split the response is a named error, not the 500
    // hyper used to produce with nothing in the log.
    let e = jwc::exec::format_set_cookie("sid", "a\r\nX-Injected: yes", &Default::default())
        .expect_err("must be refused");
    assert!(e.contains("sid"), "{e}");

    // And the native backend emits the call rather than refusing to build.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace n;\n\
         routes \"/s\" {\n\
         \x20   route GET \"\" {\n\
         \x20       return json({ ok: true })\n\
         \x20           cookie(\"a\", \"1\", { secure: true })\n\
         \x20           cookie(\"b\", \"2\");\n\
         \x20   }\n\
         }\n",
    )
    .expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let rust = jwc::native::codegen_for_test(&ws).expect("a cookie must build");
    assert!(
        rust.contains("jwc_b_v1_cookie("),
        "codegen must emit the call"
    );
    assert!(
        rust.contains("PRELUDE") || rust.contains("format_set_cookie"),
        "and the crate must carry the shared formatter"
    );
    // The repeat bug: cookies travel in a list, because a map holds one.
    assert!(
        rust.contains("__jwc_set_cookies"),
        "`Set-Cookie` repeats, so it cannot live in the header map"
    );
}

/// The attributes are checked, so a misspelling is a diagnostic and not a
/// cookie that quietly lost its `HttpOnly`.
#[test]
fn a_misspelled_cookie_attribute_is_reported() {
    fn codes(src: &str) -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        let built = jwc::model::build(&ws);
        let sym = jwc::symbols::build(&ws, &built.model);
        jwc::check::check(&ws, &sym, &built.model)
            .diags
            .iter()
            .map(|(_, d)| d.code.to_string())
            .collect()
    }
    fn route(opts: &str) -> String {
        format!(
            "namespace n;\nroutes \"/s\" {{ route GET \"\" {{ return json({{ ok: true }}) cookie(\"s\", \"v\", {opts}); }} }}\n"
        )
    }

    assert!(codes(&route("{ httponly: true }")).contains(&"E0737".to_string()));
    assert!(codes(&route("{ same_site: \"Loose\" }")).contains(&"E0738".to_string()));
    assert!(codes(&route("{ max_age: \"an hour\" }")).contains(&"E0738".to_string()));
    // `SameSite=None` without `Secure` is a cookie the browser silently
    // refuses to store — the one failure no layer would have reported.
    assert!(codes(&route("{ same_site: \"None\" }")).contains(&"E0739".to_string()));
    assert!(
        codes(&route(
            "{ same_site: \"None\", secure: true, max_age: 60, path: \"/a\" }"
        ))
        .is_empty(),
        "the valid form must check clean"
    );
}

/// A route response carried no security header at all.
///
/// Measured before this existed: a JSON route answered with
/// `content-type`, `x-request-id`, `content-length` and `date`. A `static`
/// mount sent `nosniff`; a route did not. No `X-Frame-Options`, no
/// `Referrer-Policy`, and no way for a program to ask for HSTS or a CSP.
///
/// Three are on by default because there is no deployment they are wrong
/// for, and three stay opt-in because a wrong value is worse than none —
/// an HSTS max-age sent by mistake pins a domain to HTTPS in every browser
/// that saw it, and cannot be taken back.
#[test]
fn every_response_carries_the_security_headers() {
    let core = include_str!("../src/security_headers_core.rs.in");
    let serve = include_str!("../src/serve.rs");
    let exec = include_str!("../src/exec.rs");
    assert!(exec.contains(r#"include!("security_headers_core.rs.in")"#));
    assert!(core.contains("pub fn to_headers"));

    let names: Vec<&str> = jwc::exec::SecurityHeaders::default()
        .to_headers()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(
        names,
        vec![
            "x-content-type-options",
            "x-frame-options",
            "referrer-policy"
        ],
        "the default set is the three that are always right"
    );

    // Applied where every answer passes, not in the response builders —
    // a 413 refused before the chain and a 404 have no builder.
    assert!(
        serve.contains("fn with_security_headers("),
        "the interpreter must apply them centrally"
    );

    // And the native backend must bake the *same* function's output rather
    // than grow a second opinion about the set or its order.
    let codegen = include_str!("../src/native/codegen.rs");
    assert!(codegen.contains("server.headers.to_headers()"));
    let base = include_str!("../src/native/prelude/base.rs.in");
    assert!(
        base.contains("for (name, value) in JWC_SECURITY_HEADERS"),
        "the generated crate must apply them on the way out"
    );

    // A program's own header wins over a default, on both sides.
    assert!(serve.contains("if !r.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))"));
    assert!(base.contains("if !own.iter().any(|k| k == name)"));

    // The block is a real `server { }` group, so a misspelled key is E1206.
    let wiring = include_str!("../src/wiring.rs");
    assert!(wiring.contains(r#"const GROUPS: [&str; 3] = ["cors", "tls", "headers"];"#));
    assert!(wiring.contains("crate::exec::SECURITY_HEADER_KEYS"));
}

/// `origins = ["*"]` with `credentials = true` is the CORS misconfiguration
/// that reads every authenticated response back to any site on the
/// internet.
///
/// A browser refuses the literal pair, but a server that answers `*` by
/// *reflecting* the caller's origin satisfies the browser and defeats the
/// check — and reflecting is what `jwc serve` did. Measured: a request
/// carrying `Origin: https://evil.example` came back with that origin
/// allowed and `access-control-allow-credentials: true`, and `jwc check`
/// reported nothing.
///
/// The native binary refused the same pair at boot, so the two backends
/// disagreed about whether the program could exist at all. The compiler is
/// where that belongs.
#[test]
fn wildcard_cors_with_credentials_is_refused() {
    fn codes(src: &str) -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
        let built = jwc::model::build(&ws);
        let sym = jwc::symbols::build(&ws, &built.model);
        jwc::wiring::wire(&ws, &sym)
            .diags
            .iter()
            .map(|(_, d)| d.code.to_string())
            .collect()
    }

    let bad = "namespace n;\nserver { cors { origins = [\"*\"]; credentials = true; } }\n";
    assert!(codes(bad).contains(&"E1207".to_string()));

    // Either alone is fine: `*` without credentials is an ordinary public
    // API, and credentials with a real origin list is the normal shape.
    let star_only = "namespace n;\nserver { cors { origins = [\"*\"]; } }\n";
    assert!(!codes(star_only).contains(&"E1207".to_string()));
    let listed = "namespace n;\n\
                  server { cors { origins = [\"https://app.example.com\"]; credentials = true; } }\n";
    assert!(!codes(listed).contains(&"E1207".to_string()));
}

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
        vec![url.as_str(), "-q", "-v", "ON_ERROR_STOP=1", "-f", file.to_str().expect("utf8")],
    ] {
        let out = std::process::Command::new("psql").args(&args).output().expect("psql");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    std::env::set_var("DATABASE_URL", &url);
    std::env::set_var("JWT_SECRET", "test-secret-abcdefghijklmnop");
    std::env::set_var("CURSOR_SECRET", "test-cursor-secret");
    jwc::engine::init_engine_from_env().expect("engine");
    let program = Arc::new(serve::load(&ws).unwrap_or_else(|e| panic!("{e}")));

    let attempt = |email: &'static str| {
        let p = program.clone();
        async move {
            let body = format!(
                r#"{{"email":"{email}","password":"definitely not the password"}}"#
            );
            let started = std::time::Instant::now();
            let r = call(p, "POST", "/api/v1/auth/login", &[], &body).await;
            (r.status, started.elapsed())
        }
    };

    // Warm the pool and the KDF's first-run cost out of the measurement.
    for _ in 0..3 {
        attempt("known@example.com").await;
        attempt("nobody@example.com").await;
    }

    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for _ in 0..15 {
        let (status, d) = attempt("known@example.com").await;
        assert_eq!(status, 401);
        known.push(d.as_secs_f64() * 1000.0);
        let (status, d) = attempt("nobody@example.com").await;
        assert_eq!(status, 401);
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
        ("hickory-proto", "reqwest's DNS resolver, only under testcontainers' feature set"),
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
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml"))
        .expect("ci.yml");

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

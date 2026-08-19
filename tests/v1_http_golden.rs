//! v0.24.0 acceptance: request/response pairs against the real pipeline.
//!
//! These drive `serve::handle` — the same function the server calls — so
//! nothing about ordering, middleware, the error model or SQL is stubbed.
//! The only thing not exercised is the socket.
//!
//! Requires Postgres. Set `JWC_V1_DATABASE_URL` to a connection string for
//! a database the test may **drop and recreate schemas in**. Without it the
//! suite prints SKIPPED and returns — and, as everywhere else in this
//! repo, **a SKIPPED line is not a pass**.

use jwc::v1::exec::{Program, Response};
use jwc::v1::serve::{self, Incoming};
use jwc::v1::workspace::Workspace;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Case {
    name: &'static str,
    method: &'static str,
    path: &'static str,
    query: Vec<(String, String)>,
    headers: Vec<(&'static str, String)>,
    body: String,
    want_status: u16,
    /// Substrings the body must contain. Kept as substrings on purpose:
    /// pinning whole bodies would freeze generated ids and timestamps.
    want_body: Vec<String>,
    /// Substrings the body must NOT contain — this is how the `private`
    /// rule is asserted end to end.
    deny_body: Vec<String>,
    want_headers: Vec<(&'static str, String)>,
}

fn case(name: &'static str, method: &'static str, path: &'static str, status: u16) -> Case {
    Case {
        name,
        method,
        path,
        query: Vec::new(),
        headers: Vec::new(),
        body: String::new(),
        want_status: status,
        want_body: Vec::new(),
        deny_body: Vec::new(),
        want_headers: Vec::new(),
    }
}

impl Case {
    fn json(mut self, body: &str) -> Self {
        self.body = body.to_string();
        self.headers.push(("content-type", "application/json".into()));
        self
    }
    fn header(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.headers.push((k, v.into()));
        self
    }
    fn query(mut self, k: &str, v: &str) -> Self {
        self.query.push((k.to_string(), v.to_string()));
        self
    }
    fn contains(mut self, s: &str) -> Self {
        self.want_body.push(s.to_string());
        self
    }
    fn not_contains(mut self, s: &str) -> Self {
        self.deny_body.push(s.to_string());
        self
    }
    fn has_header(mut self, k: &'static str, v: &str) -> Self {
        self.want_headers.push((k, v.to_string()));
        self
    }
}

async fn run(program: &Arc<Program>, c: &Case) -> Response {
    let mut headers: HashMap<String, String> = HashMap::new();
    for (k, v) in &c.headers {
        headers.insert(k.to_lowercase(), v.clone());
    }
    serve::handle(
        program.clone(),
        Incoming {
            method: c.method.to_string(),
            path: c.path.to_string(),
            query: c.query.clone(),
            headers,
            body: c.body.clone().into_bytes(),
            peer_ip: "203.0.113.7".into(),
        },
    )
    .await
}

fn setup_database(url: &str) -> Result<(), String> {
    // Apply the sample's own generated DDL — the artefact tests/v1_ddl_golden
    // pins — so the runtime runs against exactly what `gen-sql` emits.
    let sql = std::fs::read_to_string(repo_root().join("tests/ddl_golden/sample.sql"))
        .map_err(|e| e.to_string())?;
    let mut reset = String::from(
        "DROP SCHEMA IF EXISTS auth CASCADE; DROP SCHEMA IF EXISTS org CASCADE; \
         DROP SCHEMA IF EXISTS billing CASCADE; DROP SCHEMA IF EXISTS audit CASCADE;\n",
    );
    reset.push_str(&sql);
    reset.push_str(
        "\nINSERT INTO billing.counters (name, value) VALUES ('invoice', 0);\n\
         INSERT INTO billing.plans (code, name, price, currency, interval) \
         VALUES ('pro', 'Pro', 25.00, 'USD', 'monthly'), \
                ('free', 'Free', 0.00, 'USD', 'monthly');\n\
         UPDATE billing.plans SET active = false WHERE code = 'free';\n\
         INSERT INTO org.orgs (slug, name) VALUES ('acme', 'Acme');\n\
         INSERT INTO billing.invoices (org_id, number, amount, status, issued_at, due_at) \
         VALUES (1, 'INV-00000001', 10.00, 'open', now(), now() + interval '14 days');\n",
    );

    let out = std::process::Command::new("psql")
        .arg(url)
        .arg("-q")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-c")
        .arg(&reset)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn http_golden() {
    let Ok(url) = std::env::var("JWC_V1_DATABASE_URL") else {
        eprintln!(
            "SKIPPED http_golden — set JWC_V1_DATABASE_URL to a Postgres \
             connection string. A SKIPPED line is not a pass."
        );
        return;
    };
    if let Err(e) = setup_database(&url) {
        panic!("could not prepare the database: {e}");
    }

    std::env::set_var("DATABASE_URL", &url);
    std::env::set_var("JWT_SECRET", "test-secret-abcdefghijklmnop");
    std::env::set_var("JWT_TTL_MINUTES", "60");
    std::env::set_var("STRIPE_WEBHOOK_SECRET", "hook-secret");
    std::env::set_var("APP_URL", "https://app.test");
    jwc::engine::init_engine(&url).expect("engine");

    let ws = Workspace::load(repo_root().join("docs/spec/v1/sample")).expect("load sample");
    let program = Arc::new(serve::load(&ws).expect("the sample must compile"));

    let mut failures = Vec::new();
    let mut ran = 0usize;

    // A registration first, so later cases have an account and a token.
    let reg = run(
        &program,
        &case("bootstrap register", "POST", "/api/v1/auth/register", 201).json(
            r#"{"email":"a@example.com","display_name":"Ann","password":"correct horse battery"}"#,
        ),
    )
    .await;
    assert_eq!(reg.status, 201, "bootstrap register: {}", reg.body);

    let login = run(
        &program,
        &case("bootstrap login", "POST", "/api/v1/auth/login", 200)
            .json(r#"{"email":"a@example.com","password":"correct horse battery"}"#),
    )
    .await;
    assert_eq!(login.status, 200, "bootstrap login: {}", login.body);
    let token: String = serde_json::from_str::<serde_json::Value>(&login.body)
        .ok()
        .and_then(|j| j.get("token").and_then(|t| t.as_str().map(String::from)))
        .expect("login returns a token");
    let bearer = format!("Bearer {token}");

    let hook = |body: &str| jwc::hash::hmac_sha256_hex("hook-secret", body);
    const PAY_OK: &str =
        r#"{"provider_ref":"pi_1","invoice_id":"1","amount":"10.00","status":"succeeded"}"#;
    const PAY_MISSING_INVOICE: &str =
        r#"{"provider_ref":"pi_2","invoice_id":"999","amount":"10.00","status":"succeeded"}"#;
    const PAY_BAD_AMOUNT: &str =
        r#"{"provider_ref":"pi_3","invoice_id":"1","amount":"-1.00","status":"succeeded"}"#;

    let cases: Vec<Case> = vec![
        // ---- validation, the fixed 400 contract (types.md §11.3)
        case("register: missing every required field", "POST", "/api/v1/auth/register", 400)
            .json("{}")
            .contains("validation_failed")
            .contains("\"path\":\"email\"")
            .contains("\"rule\":\"required\""),
        case("register: short password", "POST", "/api/v1/auth/register", 400)
            .json(r#"{"email":"b@example.com","display_name":"Bee","password":"short"}"#)
            .contains("\"rule\":\"minLength\"")
            .contains("\"limit\":10"),
        case("register: bad email pattern", "POST", "/api/v1/auth/register", 400)
            .json(r#"{"email":"nope","display_name":"Bee","password":"correct horse battery"}"#)
            .contains("\"rule\":\"pattern\""),
        case("register: all failures are collected", "POST", "/api/v1/auth/register", 400)
            .json(r#"{"email":"nope","display_name":"x","password":"short"}"#)
            .contains("\"path\":\"email\"")
            .contains("\"path\":\"display_name\"")
            .contains("\"path\":\"password\""),
        case("register: body is not JSON", "POST", "/api/v1/auth/register", 400)
            .json("not json at all")
            .contains("validation_failed"),
        case("register: unknown keys are dropped", "POST", "/api/v1/auth/register", 201)
            .json(r#"{"email":"c@example.com","display_name":"Cee","password":"correct horse battery","admin":true}"#)
            .contains("c@example.com")
            .not_contains("admin"),

        // ---- the private rule, end to end (schema.md §3.1)
        case("register: never returns the hash", "POST", "/api/v1/auth/register", 201)
            .json(r#"{"email":"d@example.com","display_name":"Dee","password":"correct horse battery"}"#)
            .contains("d@example.com")
            .not_contains("password_hash")
            .not_contains("$argon2"),

        // ---- constraint promotion (errors.md §6.1)
        case("register: duplicate email is a 409", "POST", "/api/v1/auth/register", 409)
            .json(r#"{"email":"a@example.com","display_name":"Ann","password":"correct horse battery"}"#)
            .contains("ro'yxatdan o'tgan"),

        // ---- login (401, not 403 — the defect gaps.md found)
        case("login: wrong password is 401", "POST", "/api/v1/auth/login", 401)
            .json(r#"{"email":"a@example.com","password":"wrong"}"#)
            .contains("email yoki parol xato"),
        case("login: unknown email is 401 with the same message", "POST", "/api/v1/auth/login", 401)
            .json(r#"{"email":"nobody@example.com","password":"whatever"}"#)
            .contains("email yoki parol xato"),
        case("login: succeeds", "POST", "/api/v1/auth/login", 200)
            .json(r#"{"email":"a@example.com","password":"correct horse battery"}"#)
            .contains("\"token\"")
            .contains("\"expires_in\":3600"),

        // ---- auth middleware (errors.md §2.2, E14)
        case("me: no token is 401", "GET", "/api/v1/me", 401).contains("token kerak"),
        case("me: junk token is 401", "GET", "/api/v1/me", 401)
            .header("authorization", "Bearer not-a-token")
            .contains("yaroqsiz"),
        case("me: valid token", "GET", "/api/v1/me", 200)
            .header("authorization", bearer.clone())
            .contains("a@example.com")
            .not_contains("password_hash"),
        case("me: patch display name", "PATCH", "/api/v1/me", 200)
            .header("authorization", bearer.clone())
            .json(r#"{"display_name":"Annabel"}"#)
            .contains("Annabel"),
        case("me: patch with an empty body keeps the row", "PATCH", "/api/v1/me", 200)
            .header("authorization", bearer.clone())
            .json("{}")
            .contains("Annabel"),
        case("me: patch below minLength is 400", "PATCH", "/api/v1/me", 400)
            .header("authorization", bearer.clone())
            .json(r#"{"display_name":"x"}"#)
            .contains("\"rule\":\"minLength\""),

        // ---- plans: raw path, wire forms (types.md §2.3)
        case("plans: lists active plans only", "GET", "/api/v1/plans", 200)
            .contains("\"code\":\"pro\"")
            .not_contains("\"code\":\"free\""),
        case("plans: money is an exact string", "GET", "/api/v1/plans", 200)
            .contains("\"price\":\"25.00\""),
        case("plans: bigint ids are strings", "GET", "/api/v1/plans", 200)
            .contains("\"id\":\"1\""),
        case("plans: cache-control from `with { }`", "GET", "/api/v1/plans", 200)
            .has_header("cache-control", "public, max-age=60"),

        // ---- routing (routing.md §3.2, §4)
        case("unknown path is 404", "GET", "/api/v1/nope", 404),
        case("unknown method on a known path is 404", "PUT", "/api/v1/plans", 404),
        case("path parameter that does not parse is 400 before middleware", "GET", "/api/v1/orgs/not-a-number", 400)
            .header("authorization", bearer.clone())
            .contains("bad_path_parameter")
            .contains("\"parameter\":\"org_id\"")
            .contains("\"expected\":\"bigint\""),
        case("a bad path parameter is 400 even without a token", "GET", "/api/v1/orgs/abc", 400)
            .contains("bad_path_parameter"),

        // ---- webhooks: signature, then idempotency (writes.md §2.3)
        case("webhook: no signature is 401", "POST", "/api/v1/webhooks/payments", 401)
            .json(r#"{"provider_ref":"pi_1","invoice_id":"1","amount":"10.00","status":"succeeded"}"#)
            .contains("imzo yo'q"),
        case("webhook: bad signature is 401", "POST", "/api/v1/webhooks/payments", 401)
            .header("x-signature", "deadbeef")
            .json(r#"{"provider_ref":"pi_1","invoice_id":"1","amount":"10.00","status":"succeeded"}"#)
            .contains("imzo yaroqsiz"),
        case("webhook: a missing invoice is a 400, not a 500", "POST", "/api/v1/webhooks/payments", 400)
            .header("x-signature", hook(PAY_MISSING_INVOICE))
            .json(PAY_MISSING_INVOICE)
            .contains("referenced row does not exist"),
        // A decimal below the bound: `"-1.00".parse::<i64>()` fails, so an
        // integer-only comparison would have let it through.
        case("webhook: a negative amount is 400", "POST", "/api/v1/webhooks/payments", 400)
            .header("x-signature", hook(PAY_BAD_AMOUNT))
            .json(PAY_BAD_AMOUNT)
            .contains("\"rule\":\"min\""),
        case("webhook: records a payment", "POST", "/api/v1/webhooks/payments", 200)
            .header("x-signature", hook(PAY_OK))
            .json(PAY_OK)
            .contains("\"status\":\"ok\""),
        // writes.md §2.3 — this is the TOCTOU gaps.md found. Redelivering
        // the same event must not manufacture a 4xx that Stripe reads as
        // "malformed, resend".
        case("webhook: redelivery is a duplicate, not an error", "POST", "/api/v1/webhooks/payments", 200)
            .header("x-signature", hook(PAY_OK))
            .json(PAY_OK)
            .contains("\"status\":\"duplicate\""),
        case("webhook: a third delivery is still a duplicate", "POST", "/api/v1/webhooks/payments", 200)
            .header("x-signature", hook(PAY_OK))
            .json(PAY_OK)
            .contains("\"status\":\"duplicate\""),

        // ---- responses (routing.md §6, §7.1)
        case("every JSON response declares its charset", "GET", "/api/v1/plans", 200)
            .has_header("content-type", "application/json; charset=utf-8"),
        case("plans is a JSON array", "GET", "/api/v1/plans", 200).contains("[{"),
        // The projection order is the JSON key order: `jsonb` would sort
        // these alphabetically (queries.md §7.2).
        case("keys come back in projection order", "GET", "/api/v1/plans", 200)
            .contains(r#"{"id":"1","code":"pro","name":"Pro","price":"25.00""#),
        case("an error body is always {\"error\": ...}", "GET", "/api/v1/me", 401)
            .contains("{\"error\":"),

        // ---- query parameters (routing.md §5.3)
        case("an unknown query parameter is ignored", "GET", "/api/v1/plans", 200)
            .query("nope", "1")
            .contains("\"code\":\"pro\""),

        // ---- body buffer (routing.md §5.1)
        case("an oversized body is 413 before middleware", "POST", "/api/v1/webhooks/payments", 413)
            .json(&"x".repeat(300_000))
            .contains("too large"),

        // ---- login edge cases
        case("login: missing password is 400", "POST", "/api/v1/auth/login", 400)
            .json(r#"{"email":"a@example.com"}"#)
            .contains("\"path\":\"password\""),
        case("login: null password is 400", "POST", "/api/v1/auth/login", 400)
            .json(r#"{"email":"a@example.com","password":null}"#)
            .contains("\"rule\":\"required\""),
        case("me: a token signed with another secret is 401", "GET", "/api/v1/me", 401)
            .header(
                "authorization",
                format!(
                    "Bearer {}",
                    jwc::jwt::sign_hs256(r#"{"sub":"1","exp":9999999999}"#, "wrong-secret")
                        .expect("sign")
                ),
            )
            .contains("yaroqsiz"),
        case("me: a token without the Bearer prefix is still read", "GET", "/api/v1/me", 200)
            .header("authorization", token.clone())
            .contains("a@example.com"),
    ];

    for c in &cases {
        ran += 1;
        let got = run(&program, c).await;
        let mut problems = Vec::new();
        if got.status != c.want_status {
            problems.push(format!("status {} (want {})", got.status, c.want_status));
        }
        for want in &c.want_body {
            if !got.body.contains(want) {
                problems.push(format!("body is missing {want:?}"));
            }
        }
        for deny in &c.deny_body {
            if got.body.contains(deny) {
                problems.push(format!("body must not contain {deny:?}"));
            }
        }
        for (k, v) in &c.want_headers {
            let found = got
                .headers
                .iter()
                .any(|(hk, hv)| hk.eq_ignore_ascii_case(k) && hv == v);
            if !found {
                problems.push(format!("header {k}: {v:?} missing"));
            }
        }
        if !problems.is_empty() {
            failures.push(format!(
                "=== {} ===\n  {}\n  body: {}",
                c.name,
                problems.join("\n  "),
                got.body
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(ran >= 25, "expected the golden set, ran {ran}");
}

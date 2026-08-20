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

use jwc::exec::{Program, Response};
use jwc::serve::{self, Incoming};
use jwc::workspace::Workspace;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Case {
    name: String,
    method: &'static str,
    /// Owned, because the ids the sample hands out are the ones the test
    /// then asks about — pinning them as literals made the suite depend on
    /// how many rows an earlier run happened to insert.
    path: String,
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

fn case(name: &str, method: &'static str, path: &str, status: u16) -> Case {
    Case {
        name: name.to_string(),
        method,
        path: path.to_string(),
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
        // The seeded invoice is INV-00000001, so the counter stands at 1:
        // the next number the app hands out has to be the next one.
        "\nINSERT INTO billing.counters (name, value) VALUES ('invoice', 1);\n\
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

    // An org of our own. Its id is whatever the sequence hands out, so the
    // cases that read it back are built from the response rather than from
    // a number written down here.
    let created = run(
        &program,
        &case("bootstrap org", "POST", "/api/v1/orgs", 201)
            .json(r#"{"slug":"beta","name":"Beta"}"#)
            .header("authorization", &bearer),
    )
    .await;
    assert_eq!(created.status, 201, "bootstrap org: {}", created.body);
    let org: String = serde_json::from_str::<serde_json::Value>(&created.body)
        .ok()
        .and_then(|j| j.get("id").and_then(|t| t.as_str().map(String::from)))
        .expect("create returns the org id");
    let org_path = format!("/api/v1/orgs/{org}");

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
        // ---- views (queries.md §8) — every one of these reads a
        // `CREATE VIEW`, which is what v0.25.d made a real object. The
        // seeded `acme` (org 1) has no members, so it doubles as the
        // membership gate's negative case.
        case("orgs: an existing slug is a 409", "POST", "/api/v1/orgs", 409)
            .json(r#"{"slug":"acme","name":"Acme"}"#)
            .header("authorization", &bearer),
        case("orgs: create", "POST", "/api/v1/orgs", 201)
            .json(r#"{"slug":"gamma","name":"Gamma"}"#)
            .header("authorization", &bearer)
            .contains("\"slug\":\"gamma\""),
        case("membership gate: a non-member is 403", "GET", "/api/v1/orgs/1", 403)
            .header("authorization", &bearer),
        case("org detail: reads OrgWithMembers", "GET", &org_path, 200)
            .header("authorization", &bearer)
            .contains("\"slug\":\"beta\"")
            // The collection is present and non-empty: the creator is a
            // member, and it arrives through the view's lateral.
            .contains("\"members\":[{")
            .contains("\"email\":\"a@example.com\""),
        case("members: the nested account comes from a join", "GET", &format!("{org_path}/members"), 200)
            .header("authorization", &bearer)
            .contains("\"account\":{")
            .contains("\"role\":\"owner\"")
            .not_contains("password_hash"),
        case("billing summary: aggregates over a bare join", "GET", &format!("{org_path}/billing/summary"), 200)
            .header("authorization", &bearer)
            .contains("\"invoice_count\":0")
            // `sum` over an empty group is null, `count` is 0 — and the
            // flattened columns the view carries for `orderby` stay out of
            // the response.
            .contains("\"paid_total\":null")
            .not_contains("org__"),
        case("subscription: none yet is a 404", "GET", &format!("{org_path}/subscription"), 404)
            .header("authorization", &bearer),
        case("subscribe: creates one", "POST", &format!("{org_path}/subscription"), 201)
            .json(r#"{"plan_code":"pro"}"#)
            .header("authorization", &bearer),
        case("subscription: reads SubscriptionDetail", "GET", &format!("{org_path}/subscription"), 200)
            .header("authorization", &bearer)
            .contains("\"plan\":{")
            .contains("\"code\":\"pro\"")
            .contains("\"org\":{")
            .not_contains("plan__"),

        // ---- keyset pagination (queries.md §9)
        case("invoices: an empty page still has the envelope", "GET", &format!("{org_path}/invoices"), 200)
            .header("authorization", &bearer)
            .contains("\"items\":[]")
            .contains("\"has_more\":false")
            .contains("\"next\":null"),
        case("invoices: a tampered cursor is a 400, not a 500", "GET", &format!("{org_path}/invoices"), 400)
            .header("authorization", &bearer)
            .query("cursor", "v1.eyJhIjoxfQ.deadbeef")
            .contains("kursor yaroqsiz"),
        case("invoices: junk in the cursor is a 400", "GET", &format!("{org_path}/invoices"), 400)
            .header("authorization", &bearer)
            .query("cursor", "not-a-cursor"),
        case("invoices: a bad size is 400, not 500", "GET", &format!("{org_path}/invoices"), 400)
            .header("authorization", &bearer)
            .query("size", "many"),

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

    // ---- a real page walk (queries.md §9). The envelope is only half the
    // contract; the other half is that following `next` visits every row
    // exactly once, which an empty page cannot show.
    for i in 1..=5 {
        let made = run(
            &program,
            &case("seed invoice", "POST", &format!("{org_path}/invoices"), 201)
                .json(&format!(
                    r#"{{"lines":[{{"description":"line {i}","quantity":1,"unit_price":"1.00"}}]}}"#
                ))
                .header("authorization", &bearer),
        )
        .await;
        assert_eq!(made.status, 201, "seed invoice {i}: {}", made.body);
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for round in 0..5 {
        let mut c = case("page", "GET", &format!("{org_path}/invoices"), 200)
            .header("authorization", &bearer)
            .query("size", "2");
        if let Some(cur) = &cursor {
            c = c.query("cursor", cur);
        }
        let page = run(&program, &c).await;
        assert_eq!(page.status, 200, "page {round}: {}", page.body);
        let j: serde_json::Value = serde_json::from_str(&page.body).expect("json");
        let items = j["items"].as_array().expect("items");
        assert!(items.len() <= 2, "size 2 was not honoured: {}", page.body);
        for it in items {
            seen.push(it["id"].as_str().expect("id").to_string());
        }
        if !j["has_more"].as_bool().unwrap_or(false) {
            assert!(j["next"].is_null(), "the last page carries no cursor: {}", page.body);
            break;
        }
        cursor = Some(j["next"].as_str().expect("next").to_string());
    }

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), unique.len(), "a row was visited twice: {seen:?}");
    assert_eq!(seen.len(), 5, "the walk did not see every row: {seen:?}");
    // Newest first, and the ids the sample hands out ascend, so the walk
    // runs down them.
    let mut descending = seen.clone();
    descending.sort_by_key(|id| std::cmp::Reverse(id.parse::<i64>().unwrap()));
    assert_eq!(seen, descending, "the order did not survive paging: {seen:?}");

    // ---- every route answers (ROADMAP v0.25.0's done criterion).
    //
    // Not that every route answers *correctly* — the cases above do that
    // for the ones worth pinning. This rules out the failure the query
    // compiler could still produce: a route that reaches the database and
    // faults, which is a 500 and looks like nothing else.
    let ids: HashMap<&str, &str> = [
        ("org_id", org.as_str()),
        ("invoice_id", "1"),
        ("invite_id", "1"),
        ("account_id", "1"),
    ]
    .into_iter()
    .collect();
    let bodies: HashMap<&str, &str> = [
        ("POST /api/v1/orgs", r#"{"slug":"smoke","name":"Smoke"}"#),
        ("PATCH /api/v1/orgs/{org_id}", r#"{"name":"Renamed"}"#),
        (
            "POST /api/v1/orgs/{org_id}/invites",
            r#"{"email":"i@example.com","role":"member"}"#,
        ),
        (
            "PATCH /api/v1/orgs/{org_id}/members/{account_id}",
            r#"{"role":"admin"}"#,
        ),
        (
            "POST /api/v1/orgs/{org_id}/invoices",
            r#"{"lines":[{"description":"smoke","quantity":1,"unit_price":"1.00"}]}"#,
        ),
        (
            "POST /api/v1/orgs/{org_id}/subscription",
            r#"{"plan_code":"pro"}"#,
        ),
    ]
    .into_iter()
    .collect();

    let mut faulted = Vec::new();
    let mut answered = 0usize;
    for route in &program.routes {
        let path: String = route
            .segments
            .iter()
            .map(|seg| match seg {
                jwc::wiring::Segment::Literal(l) => format!("/{l}"),
                jwc::wiring::Segment::Param { name, .. } => {
                    format!("/{}", ids.get(name.as_str()).copied().unwrap_or("1"))
                }
            })
            .collect();
        let key = format!("{} {}", route.method, route.pattern);
        let mut c = case(&key, method_of(&route.method), &path, 200)
            .header("authorization", &bearer);
        if let Some(b) = bodies.get(key.as_str()) {
            c = c.json(b);
        }
        if route.pattern.contains("/webhooks/") {
            c = c.json(PAY_OK).header("x-signature", hook(PAY_OK));
        }
        let r = run(&program, &c).await;
        answered += 1;
        if r.status >= 500 {
            faulted.push(format!("{key} -> {} {}", r.status, r.body));
        }
    }
    assert_eq!(answered, 25, "the sample has 25 routes");
    assert!(
        faulted.is_empty(),
        "routes that faulted:\n{}",
        faulted.join("\n")
    );

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(ran >= 25, "expected the golden set, ran {ran}");
}

/// `Case::method` is `&'static str`; the route table's is owned.
fn method_of(m: &str) -> &'static str {
    match m {
        "GET" => "GET",
        "POST" => "POST",
        "PATCH" => "PATCH",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        other => panic!("unknown method {other}"),
    }
}

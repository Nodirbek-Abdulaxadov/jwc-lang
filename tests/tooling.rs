//! `jwc explain`, `jwc lint`, `jwc openapi` — the commands that make the
//! compiler's output readable without deploying it (tooling.md).
//!
//! These drive the real binary rather than the library, because what is
//! being pinned is the command-line contract: which flag selects what, and
//! what a wrong name prints.

use std::path::PathBuf;
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sample() -> PathBuf {
    repo_root().join("docs/spec/v1/sample")
}

/// The `W1302` fixture.
///
/// These two tests used to run against the specification's sample, which
/// made them depend on it staying sloppy: the moment its constraints got
/// the messages `W1302` asks for, the assertions that needed a
/// message-less one broke. A normative sample should be exemplary, so the
/// untidy shapes moved here.
fn lint_fixture() -> PathBuf {
    repo_root().join("tests/lint_constraints")
}

fn jwc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(args)
        .output()
        .expect("run jwc")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn explain_route_selects_only_what_that_route_reaches() {
    let path = sample();
    let path = path.to_str().expect("utf8");
    let all = jwc(&["explain", path, "--sql"]);
    assert!(
        all.status.success(),
        "{}",
        String::from_utf8_lossy(&all.stderr)
    );

    let one = jwc(&[
        "explain",
        path,
        "--sql",
        "--route",
        "GET /api/v1/orgs/{org_id}/invoices",
    ]);
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );

    let all_n = count(&stdout(&all));
    let one_n = count(&stdout(&one));
    assert!(
        one_n >= 2,
        "the route's own query and the view it reads: {one_n}"
    );
    assert!(
        one_n < all_n,
        "--route did not narrow anything: {one_n} of {all_n}"
    );

    // The route's query and the view it selects from. The view is part of
    // the answer — its body runs as part of the statement (tooling.md §1.3).
    let text = stdout(&one);
    assert!(text.contains("BillingService.invoices"), "{text}");
    assert!(text.contains("view InvoiceDetail"), "{text}");
    // Nothing from an unrelated service.
    assert!(!text.contains("AuthService.login"), "{text}");
}

#[test]
fn explain_function_follows_the_call_graph() {
    let path = sample();
    let path = path.to_str().expect("utf8");
    let out = jwc(&["explain", path, "--sql", "--function", "AuthService.login"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains("AuthService.login"), "{text}");
    assert!(text.contains("auth.accounts"), "{text}");
    assert_eq!(count(&text), 1, "{text}");
}

#[test]
fn a_name_that_does_not_exist_lists_what_does() {
    let path = sample();
    let path = path.to_str().expect("utf8");

    let out = jwc(&["explain", path, "--route", "GET /nope"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    // Never an empty success: the point of the message is that the caller
    // mistyped a pattern, and the patterns are right there.
    assert!(err.contains("no route"), "{err}");
    assert!(err.contains("POST /api/v1/auth/login"), "{err}");

    let out = jwc(&["explain", path, "--function", "Nope.nope"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no function"), "{err}");
    assert!(err.contains("AuthService.login"), "{err}");
}

#[test]
fn a_route_pattern_is_not_two_strings_glued_together() {
    // A suffix carries no leading slash and a parameter still carries its
    // type, so `"/api/v1/auth" + "register"` is `/api/v1/authregister`.
    // This is the normalisation `request.route()` and `--route` share
    // (routing.md §5.4).
    assert_eq!(
        jwc::wiring::route_pattern("/api/v1/auth", "register"),
        "/api/v1/auth/register"
    );
    assert_eq!(
        jwc::wiring::route_pattern("/api/v1/orgs", "{org_id: bigint}/invoices"),
        "/api/v1/orgs/{org_id}/invoices"
    );
    assert_eq!(jwc::wiring::route_pattern("/api/v1/me", ""), "/api/v1/me");
}

#[test]
fn deny_warnings_is_the_ci_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace w;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         table T of App.s { id bigint primary key identity; }\n\
         service S {\n\
         \x20   function peek() {\n\
         \x20       let rows = select R from App.s.T;\n\
         \x20       return debug.dump($rows);\n\
         \x20   }\n\
         }\n",
    )
    .expect("write");
    let path = dir.path().to_str().expect("utf8");

    let ok = jwc(&["check", path]);
    assert!(ok.status.success(), "a warning alone is not an error");
    assert!(
        String::from_utf8_lossy(&ok.stderr).contains("W1301"),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let denied = jwc(&["check", path, "--deny-warnings"]);
    assert!(!denied.status.success(), "--deny-warnings did not deny");
}

#[test]
fn lint_constraints_reports_the_status_each_violation_produces() {
    let path = lint_fixture();
    let path = path.to_str().expect("utf8");
    let out = jwc(&["lint", path, "--constraints"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);

    // errors.md §6.1 — a unique carrying a message is a Conflict, a check
    // is a BadRequest, and a message-less one is a fault.
    assert!(
        text.contains("uq_owners__slug") && text.contains("409  \"bu slug band\""),
        "{text}"
    );
    assert!(text.contains("500  (no message — a fault)"), "{text}");
    // errors.md §6.3 — an FK is always 400, with a fixed message.
    assert!(
        text.contains("400  referenced row does not exist"),
        "{text}"
    );

    // A `delete` can violate nothing on the row it removes. What it can
    // trip is a foreign key pointing *at* that row, and only where the
    // reference is not cascaded — `Notes` is not, `Items` is.
    let deleting_an_owner = section(&text, "DELETE /owners/{id}");
    assert!(
        deleting_an_owner.contains("fk_notes__owner_id"),
        "deleting an owner with notes is a 400: {deleting_an_owner}"
    );
    assert!(
        !deleting_an_owner.contains("fk_items__owner_id"),
        "a cascaded reference cannot be violated: {deleting_an_owner}"
    );
    assert!(
        !deleting_an_owner.contains("uq_owners__slug"),
        "a delete cannot violate a unique: {deleting_an_owner}"
    );

    // A read-only route reaches nothing.
    assert!(
        section(&text, "GET /items").contains("writes nothing"),
        "{text}"
    );
}

/// The specification's sample is the counter-example: every constraint a
/// route can reach carries a message, so it lints clean. If this fails,
/// the sample grew a 500 nobody meant to ship.
#[test]
fn the_sample_has_no_message_less_constraint_left() {
    let path = sample();
    let out = jwc(&["lint", path.to_str().expect("utf8"), "--deny-warnings"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_message_less_constraint_a_route_can_reach_is_a_warning() {
    let path = lint_fixture();
    let path = path.to_str().expect("utf8");
    let out = jwc(&["lint", path]);
    let err = String::from_utf8_lossy(&out.stderr);

    // Reported once per constraint, at the schema line, with the routes in
    // the note (tooling.md §4.3.1) — not once per route, at a handler that
    // did nothing wrong.
    assert!(err.contains("W1302"), "{err}");
    assert!(
        err.contains("uq_items__sku"),
        "the fixture's message-less unique: {err}"
    );
    assert_eq!(
        err.matches("uq_items__sku` carries no message").count(),
        1,
        "reported per route instead of per constraint"
    );
    // A *column rule* without a message, not just a whole constraint. The
    // warning used to advise `add ": …"` here and the grammar refused it.
    assert!(
        err.contains("ck_items__name__minlength"),
        "a message-less column rule warns too: {err}"
    );
    assert!(err.contains("reached from:"), "{err}");

    // Foreign keys are deliberately not warned about: errors §6.3 gives them
    // a fixed 400 and no per-constraint message exists to add (DEFERRED-4).
    assert!(!err.contains("W1302]: `fk_"), "{err}");

    assert!(!jwc(&["lint", path, "--deny-warnings"]).status.success());
}

#[test]
fn explain_and_the_sql_golden_are_the_same_compiler() {
    // v0.27.0's done criterion (ROADMAP §3): what `jwc explain` prints for
    // the sample is what `tests/sql_golden/sample.sql` froze. They go
    // through the same `sites()` and the same `Compiler`, so this asserts a
    // property rather than discovering one — which is the point: the day
    // one of them grows a private path, this fails.
    let path = sample();
    let out = jwc(&["explain", path.to_str().expect("utf8"), "--sql"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = stdout(&out);

    let golden = std::fs::read_to_string(repo_root().join("tests/sql_golden/sample.sql"))
        .expect("tests/sql_golden/sample.sql");

    // Every non-comment line of the golden appears in what was printed.
    // Line by line rather than statement by statement, because the two
    // wrap and indent for different readers.
    let mut missing: Vec<&str> = Vec::new();
    let mut checked = 0usize;
    for line in golden.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        checked += 1;
        if !printed.contains(t) {
            missing.push(t);
        }
    }
    assert!(checked > 100, "the golden looks empty: {checked} lines");
    assert!(
        missing.is_empty(),
        "{} golden line(s) `jwc explain` does not print:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}

#[test]
fn openapi_describes_every_route_and_validates() {
    let path = sample();
    let path = path.to_str().expect("utf8");
    let out = jwc(&["openapi", path]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid JSON");

    assert_eq!(doc["openapi"], "3.1.0");
    let paths = doc["paths"].as_object().expect("paths");
    // 26 endpoints over 19 distinct patterns.
    assert_eq!(paths.len(), 19, "{:?}", paths.keys().collect::<Vec<_>>());
    let operations: usize = paths
        .values()
        .map(|p| p.as_object().map_or(0, |o| o.len()))
        .sum();
    assert_eq!(operations, 26);

    let invoices = &paths["/api/v1/orgs/{org_id}/invoices"]["get"];
    // routing.md §3.1 — a typed path parameter, in its wire form: `bigint`
    // is a JSON string because JavaScript loses digits above 2^53
    // (types.md §2.3).
    assert_eq!(invoices["parameters"][0]["name"], "org_id");
    assert_eq!(invoices["parameters"][0]["schema"]["type"], "string");
    assert_eq!(invoices["parameters"][0]["schema"]["format"], "int64");
    // errors.md §4.3 — a declared error's default status is the answer
    // whether or not an `errorHandler` arm names it, so the non-2xx set is
    // the raise set.
    assert!(invoices["responses"]["401"].is_object(), "{invoices}");
    assert!(invoices["responses"]["403"].is_object(), "{invoices}");
    // The middleware chain is not an OpenAPI concept, but it is what decides
    // whether a call needs a token.
    assert_eq!(invoices["x-jwc-middleware"][0], "RequireAuth");

    // A 200 whose shape the compiler knows is documented, nested types and
    // enum members included — even though the service function carries no
    // return annotation, because types.md §10.2 only demands one when two
    // returns disagree.
    let org = &paths["/api/v1/orgs/{org_id}"]["get"]["responses"]["200"];
    let schema = &org["content"]["application/json"]["schema"];
    assert_eq!(schema["properties"]["id"]["format"], "int64");
    assert_eq!(schema["properties"]["members"]["type"], "array");
    let role = &schema["properties"]["members"]["items"]["properties"]["role"];
    assert_eq!(role["enum"][0], "owner", "{role}");

    // A `class` a route validates its body against becomes a schema.
    let login = &paths["/api/v1/auth/login"]["post"];
    assert_eq!(
        login["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Login"
    );
    // Class rules have exact JSON Schema spellings, so the document rejects
    // what the server would reject.
    let invite = &doc["components"]["schemas"]["InviteCreate"]["properties"]["email"];
    assert!(invite["pattern"].is_string(), "{invite}");

    // Every `$ref` resolves.
    let schemas = doc["components"]["schemas"].as_object().expect("schemas");
    for name in refs(&doc) {
        assert!(schemas.contains_key(&name), "dangling $ref to `{name}`");
    }
}

#[test]
fn openapi_passes_a_real_validator() {
    // `pip install openapi-spec-validator`. Without it this prints SKIPPED —
    // and a SKIPPED line is not a pass: the structural test above checks
    // what this repository knows to check, and only a real validator checks
    // the rest of the 3.1 specification.
    let probe = Command::new("python3")
        .args(["-c", "import openapi_spec_validator"])
        .output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!(
            "SKIPPED openapi_passes_a_real_validator — pip install \
             openapi-spec-validator. A SKIPPED line is not a pass."
        );
        return;
    }
    let path = sample();
    let out = jwc(&["openapi", path.to_str().expect("utf8")]);
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("openapi.json");
    std::fs::write(&doc, stdout(&out)).expect("write");

    let v = Command::new("python3")
        .args([
            "-c",
            "import sys,json; from openapi_spec_validator import validate; \
             validate(json.load(open(sys.argv[1])))",
            doc.to_str().expect("utf8"),
        ])
        .output()
        .expect("python3");
    assert!(v.status.success(), "{}", String::from_utf8_lossy(&v.stderr));
}

/// Every `$ref` target name in a document.
fn refs(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                if k == "$ref" {
                    if let Some(s) = x.as_str().and_then(|s| s.rsplit('/').next()) {
                        out.push(s.to_string());
                    }
                }
                out.extend(refs(x));
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                out.extend(refs(x));
            }
        }
        _ => {}
    }
    out
}

/// The lines under one route heading in `--constraints` output.
fn section<'a>(text: &'a str, route: &str) -> &'a str {
    let start = match text.find(&format!("{route}\u{1b}[0m")) {
        Some(i) => i,
        None => return "",
    };
    let rest = &text[start..];
    let end = rest[1..]
        .find("\u{1b}[1m")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// `jwc explain` ends with `N queries`.
fn count(text: &str) -> usize {
    text.lines()
        .find_map(|l| {
            l.strip_suffix(" queries")
                .or_else(|| l.strip_suffix(" query"))
        })
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

/// `created(json($row))` is the idiomatic 201, and it used to produce two
/// wrong responses in the document: a `200` carrying the object (a status
/// the route cannot answer) and a `201` carrying nothing (the status it
/// does answer, with the body dropped). The inner `json` recorded its own
/// 200 and the outer `created` recorded the *type of a response*, which
/// has no schema.
///
/// A client generator reading that produced the wrong type for every
/// created resource in the sample.
#[test]
fn a_nested_response_builder_documents_one_status_with_the_body() {
    let path = sample();
    let doc = jwc(&["openapi", path.to_str().expect("utf8")]);
    assert!(
        doc.status.success(),
        "{}",
        String::from_utf8_lossy(&doc.stderr)
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout(&doc)).expect("json");

    let paths = doc["paths"].as_object().expect("paths");
    let mut checked = 0;
    for (route, item) in paths {
        let Some(post) = item.get("post") else {
            continue;
        };
        let responses = post["responses"].as_object().expect("responses");
        // A POST that creates something answers 201, not 200.
        if let Some(created) = responses.get("201") {
            checked += 1;
            assert!(
                !responses.contains_key("200"),
                "POST {route} documents both 200 and 201:\n{responses:#?}"
            );
            assert!(
                created
                    .pointer("/content/application~1json/schema")
                    .is_some(),
                "POST {route} answers 201 with no documented body:\n{created:#?}"
            );
        }
    }
    assert!(checked > 0, "the sample has no `created(...)` route left");
}

/// `jwc run` executes `main()` and exits, with no database and no listener.
///
/// Restored in 0.9.920. The v0.25.0 cutover deleted `jwc run` and nothing
/// replaced it, so the smallest program anyone writes — print a line —
/// had no way to be run: `jwc serve` was the only executor, it started a
/// listener, and it demanded `DATABASE_URL` from a program with no tables.
/// Someone hit exactly that on a fresh install.
#[test]
fn run_executes_main_and_exits() {
    let dir = std::env::temp_dir().join("jwc-run-console");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("app.jwc"),
        "function main() {\n\
         \x20   console.write(\"Ismingiz: \");\n\
         \x20   let who = console.read();\n\
         \x20   console.writeln(\"Salom, \" + ($who ?? \"notanish\"));\n\
         \x20   console.writeln(42);\n\
         }\n",
    )
    .expect("write");

    let out = Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(["run", dir.join("app.jwc").to_str().expect("utf8")])
        .env_remove("DATABASE_URL")
        .env_remove("JWC_DATABASE_URL")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(b"Nodirbek\n")?;
            child.wait_with_output()
        })
        .expect("run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // No trailing newline after `write`, one after each `writeln`, and a
    // non-text value renders as itself rather than as JSON.
    assert_eq!(stdout, "Ismingiz: Salom, Nodirbek\n42\n", "{stdout}");
}

/// A program with no `main` is not something `run` can run, and the
/// message says which command it wants instead.
#[test]
fn run_without_a_main_says_what_to_do() {
    let dir = std::env::temp_dir().join("jwc-run-no-main");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("app.jwc"),
        "routes \"/x\" {\n    route GET \"\" { return json({ ok: true }); }\n}\n",
    )
    .expect("write");

    let out = Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(["run", dir.join("app.jwc").to_str().expect("utf8")])
        .output()
        .expect("run");

    assert!(!out.status.success(), "a program with no `main` cannot run");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("declares no `main`"), "{err}");
    assert!(
        err.contains("jwc serve"),
        "it should name the alternative: {err}"
    );
}

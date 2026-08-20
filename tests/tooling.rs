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
    assert!(all.status.success(), "{}", String::from_utf8_lossy(&all.stderr));

    let one = jwc(&[
        "explain",
        path,
        "--sql",
        "--route",
        "GET /api/v1/orgs/{org_id}/invoices",
    ]);
    assert!(one.status.success(), "{}", String::from_utf8_lossy(&one.stderr));

    let all_n = count(&stdout(&all));
    let one_n = count(&stdout(&one));
    assert!(one_n >= 2, "the route's own query and the view it reads: {one_n}");
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
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
    let path = sample();
    let path = path.to_str().expect("utf8");
    let out = jwc(&["lint", path, "--constraints"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = stdout(&out);

    // errors.md §6.1 — a unique carrying a message is a Conflict, a check
    // is a BadRequest, and a message-less one is a fault.
    assert!(
        text.contains("uq_accounts__email               409  \"bu email allaqachon ro'yxatdan o'tgan\""),
        "{text}"
    );
    assert!(text.contains("500  (no message — a fault)"), "{text}");
    // errors.md §6.3 — an FK is always 400, with a fixed message.
    assert!(text.contains("400  referenced row does not exist"), "{text}");

    // A `delete` can violate nothing on the row it removes. What it can
    // trip is a foreign key pointing *at* that row, and only where the
    // reference is not cascaded.
    let deleting_an_org = section(&text, "DELETE /api/v1/orgs/{org_id}");
    assert!(
        deleting_an_org.contains("fk_invoices__org_id"),
        "deleting an org with invoices is a 400: {deleting_an_org}"
    );
    assert!(
        !deleting_an_org.contains("uq_orgs__slug"),
        "a delete cannot violate a unique: {deleting_an_org}"
    );

    // A read-only route reaches nothing.
    assert!(
        section(&text, "GET /api/v1/plans").contains("writes nothing"),
        "{text}"
    );
}

#[test]
fn a_message_less_constraint_a_route_can_reach_is_a_warning() {
    let path = sample();
    let path = path.to_str().expect("utf8");
    let out = jwc(&["lint", path]);
    let err = String::from_utf8_lossy(&out.stderr);

    // Reported once per constraint, at the schema line, with the routes in
    // the note (tooling.md §4.3.1) — not once per route, at a handler that
    // did nothing wrong.
    assert!(err.contains("W1302"), "{err}");
    assert!(
        err.contains("uq_invites__token_hash"),
        "the sample's one reachable message-less unique: {err}"
    );
    assert_eq!(
        err.matches("uq_invites__token_hash` carries no message").count(),
        1,
        "reported per route instead of per constraint"
    );
    assert!(err.contains("reached from:"), "{err}");

    // Foreign keys are deliberately not warned about: errors §6.3 gives them
    // a fixed 400 and no per-constraint message exists to add (DEFERRED-4).
    assert!(!err.contains("W1302]: `fk_"), "{err}");

    assert!(!jwc(&["lint", path, "--deny-warnings"]).status.success());
}

/// The lines under one route heading in `--constraints` output.
fn section<'a>(text: &'a str, route: &str) -> &'a str {
    let start = match text.find(&format!("{route}\u{1b}[0m")) {
        Some(i) => i,
        None => return "",
    };
    let rest = &text[start..];
    let end = rest[1..].find("\u{1b}[1m").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// `jwc explain` ends with `N queries`.
fn count(text: &str) -> usize {
    text.lines()
        .find_map(|l| l.strip_suffix(" queries").or_else(|| l.strip_suffix(" query")))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

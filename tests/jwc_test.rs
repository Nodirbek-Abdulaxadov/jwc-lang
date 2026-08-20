//! `jwc test` — the test framework, tested (testing.md).
//!
//! Needs Postgres: set `JWC_V1_DATABASE_URL` to a database this may drop
//! and recreate schemas in. Without it every test here prints SKIPPED, and
//! **a SKIPPED line is not a pass** — isolation and `assert fails` are
//! claims about what a database does, and nothing else can check them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn url() -> Option<String> {
    std::env::var("JWC_V1_DATABASE_URL").ok()
}

/// One database, one test at a time.
///
/// `install_schema` drops and recreates the sample's four schemas, so two
/// tests running at once against one `JWC_V1_DATABASE_URL` are not
/// isolated — they are racing to drop each other's types mid-apply. Three
/// of the six failed that way the first time a database was configured,
/// with errors (`duplicate key value ... pg_type_typname_nsp_index`) that
/// name the collision rather than the cause.
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The url and the exclusion guard. Bind both — `let (url, _) = ...` drops
/// the guard immediately and restores the race.
macro_rules! db {
    ($name:literal) => {
        match url() {
            Some(u) => (u, TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())),
            None => {
                eprintln!(
                    "SKIPPED {} — set JWC_V1_DATABASE_URL. A SKIPPED line is not a pass.",
                    $name
                );
                return;
            }
        }
    };
}

fn jwc(args: &[&str], url: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(args)
        .env("DATABASE_URL", url)
        .env("CURSOR_SECRET", "test-secret")
        .output()
        .expect("run jwc")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A fresh schema built from `gen-sql`, so a run starts from nothing.
fn install_schema(project: &Path, url: &str) {
    let out = jwc(&["gen-sql", project.to_str().expect("utf8")], url);
    assert!(out.status.success(), "{}", text(&out));
    let dir = tempfile::tempdir().expect("tempdir");
    let sql = dir.path().join("schema.sql");
    std::fs::write(&sql, String::from_utf8_lossy(&out.stdout).as_ref()).expect("write");

    let drop = Command::new("psql")
        .args([
            url,
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            "DROP SCHEMA IF EXISTS audit, auth, billing, org CASCADE",
        ])
        .output()
        .expect("psql");
    assert!(
        drop.status.success(),
        "{}",
        String::from_utf8_lossy(&drop.stderr)
    );

    let apply = Command::new("psql")
        .args([
            url,
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-f",
            sql.to_str().expect("utf8"),
        ])
        .output()
        .expect("psql");
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
}

fn rows(url: &str, sql: &str) -> i64 {
    let out = Command::new("psql")
        .args([url, "-tAc", sql])
        .output()
        .expect("psql");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(-1)
}

#[test]
fn the_samples_tests_pass_and_leave_nothing_behind() {
    let (url, _guard) = db!("the_samples_tests_pass_and_leave_nothing_behind");
    let sample = repo_root().join("docs/spec/v1/sample");
    install_schema(&sample, &url);

    // Three runs. Every fixture uses a fixed slug on a `unique` column, so
    // if a single row survived a test the next run would fail on its own
    // first insert — which is exactly the N9 failure the isolation model
    // exists for (testing.md §2.2).
    for run in 1..=3 {
        let out = jwc(&["test", sample.to_str().expect("utf8")], &url);
        assert!(out.status.success(), "run {run}:\n{}", text(&out));
        assert!(
            text(&out).contains("4 tests, 0 failed"),
            "run {run}: {}",
            text(&out)
        );
    }
    assert_eq!(rows(&url, "select count(*) from org.orgs"), 0);
    assert_eq!(rows(&url, "select count(*) from billing.subscriptions"), 0);
    assert_eq!(rows(&url, "select count(*) from billing.invoices"), 0);
}

#[test]
fn the_order_does_not_matter() {
    let (url, _guard) = db!("the_order_does_not_matter");
    let sample = repo_root().join("docs/spec/v1/sample");
    install_schema(&sample, &url);

    // The suite runs in declaration order, so shuffling means reordering
    // the declarations. Reversed is the harshest permutation of four: every
    // test moves.
    let dir = tempfile::tempdir().expect("tempdir");
    copy_tree(&sample, dir.path());
    let f = dir.path().join("sample/tests/billing_test.jwc");
    let src = std::fs::read_to_string(&f).expect("read");
    std::fs::write(&f, reverse_tests(&src)).expect("write");

    let project = dir.path().join("sample");
    let out = jwc(&["test", project.to_str().expect("utf8")], &url);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("4 tests, 0 failed"), "{}", text(&out));
}

#[test]
fn a_wrong_message_fails_and_prints_both() {
    let (url, _guard) = db!("a_wrong_message_fails_and_prints_both");
    let sample = repo_root().join("docs/spec/v1/sample");
    install_schema(&sample, &url);

    let dir = tempfile::tempdir().expect("tempdir");
    copy_tree(&sample, dir.path());
    let f = dir.path().join("sample/tests/billing_test.jwc");
    let src = std::fs::read_to_string(&f).expect("read");
    // The message is compared exactly. This is the negative test the
    // release is measured by: without it, a test passes when the sentence a
    // caller sees changes underneath it (#28).
    std::fs::write(
        &f,
        src.replace(
            r#"} with "bu tashkilotda faol obuna allaqachon bor";"#,
            r#"} with "bu tashkilotda faol obuna bor";"#,
        ),
    )
    .expect("write");

    let project = dir.path().join("sample");
    let out = jwc(&["test", project.to_str().expect("utf8")], &url);
    assert!(!out.status.success(), "a wrong message passed");
    let t = text(&out);
    assert!(t.contains("1 failed"), "{t}");
    assert!(t.contains("want: bu tashkilotda faol obuna bor"), "{t}");
    assert!(
        t.contains("got:  bu tashkilotda faol obuna allaqachon bor"),
        "{t}"
    );
}

#[test]
fn a_wrong_error_type_fails() {
    let (url, _guard) = db!("a_wrong_error_type_fails");
    let sample = repo_root().join("docs/spec/v1/sample");
    install_schema(&sample, &url);

    let dir = tempfile::tempdir().expect("tempdir");
    copy_tree(&sample, dir.path());
    let f = dir.path().join("sample/tests/billing_test.jwc");
    let src = std::fs::read_to_string(&f).expect("read");
    // A unique with a message is a `Conflict`, never a `BadRequest`
    // (errors.md §6.1). Before v0.28.0 `assert fails` ignored the name
    // entirely, so this passed.
    std::fs::write(
        &f,
        src.replace("assert fails Conflict {", "assert fails BadRequest {"),
    )
    .expect("write");

    let project = dir.path().join("sample");
    let out = jwc(&["test", project.to_str().expect("utf8")], &url);
    assert!(!out.status.success(), "the wrong error type passed");
    assert!(
        text(&out).contains("expected `BadRequest`, got `Conflict`"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_failed_assertion_does_not_poison_the_rest_of_the_test() {
    let (url, _guard) = db!("a_failed_assertion_does_not_poison_the_rest_of_the_test");
    let sample = repo_root().join("docs/spec/v1/sample");
    install_schema(&sample, &url);

    // testing.md §4.4 — the `assert fails` block runs in a savepoint.
    // Postgres refuses every statement in a transaction that has seen an
    // error (25P02), so without one the *next* statement in the test would
    // fail for a reason that has nothing to do with it. The sample's second
    // and third tests both keep working after an `assert fails`, and the
    // fourth writes after one — this asserts the suite as a whole.
    let out = jwc(&["test", sample.to_str().expect("utf8")], &url);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !text(&out).contains("25P02") && !text(&out).contains("current transaction is aborted"),
        "{}",
        text(&out)
    );
}

#[test]
fn an_untyped_assert_fails_is_a_compile_error() {
    // testing.md §4.1. No database needed: this one is the checker's.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("a.jwc"),
        "namespace t;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         table T of App.s { id bigint primary key identity; }\n\
         test \"untyped\" {\n\
         \x20   assert fails {\n\
         \x20       insert into App.s.T { id = 1 };\n\
         \x20   };\n\
         }\n",
    )
    .expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(["check", dir.path().to_str().expect("utf8")])
        .output()
        .expect("run jwc");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E1401"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Move every `test "…" { … }` block to the opposite end of the file,
/// keeping the helper functions where they are.
fn reverse_tests(src: &str) -> String {
    let mut head = String::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut depth = 0i32;
    for line in src.lines() {
        if current.is_none() && line.starts_with("test ") {
            current = Some(String::new());
            depth = 0;
        }
        match &mut current {
            None => {
                head.push_str(line);
                head.push('\n');
            }
            Some(buf) => {
                buf.push_str(line);
                buf.push('\n');
                depth += line.matches('{').count() as i32;
                depth -= line.matches('}').count() as i32;
                if depth == 0 {
                    blocks.push(std::mem::take(buf));
                    current = None;
                }
            }
        }
    }
    assert_eq!(blocks.len(), 4, "the sample's four tests");
    blocks.reverse();
    format!("{head}{}", blocks.join("\n"))
}

fn copy_tree(from: &Path, into: &Path) {
    let target = into.join(from.file_name().expect("name"));
    if from.is_dir() {
        std::fs::create_dir_all(&target).expect("mkdir");
        for e in std::fs::read_dir(from).expect("read_dir") {
            copy_tree(&e.expect("entry").path(), &target);
        }
    } else {
        std::fs::copy(from, &target).expect("copy");
    }
}

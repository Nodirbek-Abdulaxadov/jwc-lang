//! `jwc new` acceptance: every template a user can scaffold must pass the
//! toolchain the same user will run on it five seconds later.
//!
//! This is the whole reason the command is worth restoring rather than
//! shipping a `git clone` instruction. The trees that survived the v0.25.0
//! cutover on disk were written in the pre-1.0 grammar and no longer
//! parsed; nothing noticed, because no test ever fed them to the compiler.
//! So: scaffold each one and run `check --deny-warnings`, `lint
//! --deny-warnings`, `fmt --check` and `routes` over it. A template that
//! starts a project with a warning fails here.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn jwc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(args)
        .output()
        .expect("run jwc")
}

fn ok(args: &[&str]) -> String {
    let out = jwc(args);
    assert!(
        out.status.success(),
        "`jwc {}` failed:\n{}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A directory that is removed when the test ends, pass or fail.
struct Scratch(PathBuf);

impl Scratch {
    /// The suffix matters: `cargo test` runs these concurrently in one
    /// process, and two tests that scaffolded the same template shared a
    /// directory — one deleted what the other had just written.
    fn new(label: &str) -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jwc-template-{label}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn str(&self) -> &str {
        self.0.to_str().expect("utf8 scratch path")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scaffold(kind: &str) -> Scratch {
    let scratch = Scratch::new(kind);
    ok(&["new", "demo", "--template", kind, "--path", scratch.str()]);
    scratch
}

#[test]
fn every_template_checks_lints_and_is_formatted() {
    for kind in ["empty", "api", "auth"] {
        let scratch = scaffold(kind);
        let path = scratch.str();

        ok(&["check", path, "--deny-warnings"]);
        ok(&["lint", path, "--deny-warnings"]);
        // Not just "it parses": the checked-in text is what `jwc fmt`
        // would write, so a fresh project is not one `fmt` away from a
        // diff on its first commit.
        ok(&["fmt", path, "--check"]);
        ok(&["routes", path]);
        // Derived from the types, so this exercises the whole front end.
        ok(&["openapi", path]);
    }
}

#[test]
fn no_placeholder_survives_scaffolding() {
    for kind in ["empty", "api", "auth"] {
        let scratch = scaffold(kind);
        let mut files = Vec::new();
        collect(scratch.path(), &mut files);
        assert!(!files.is_empty(), "{kind} scaffolded nothing");
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            assert!(
                !text.contains("{{name}}"),
                "{} still holds a `{{{{name}}}}` placeholder",
                f.display()
            );
            assert!(
                !f.to_string_lossy().contains("__name__"),
                "{} still holds a `__name__` placeholder",
                f.display()
            );
        }
    }
}

#[test]
fn the_api_template_declares_the_five_crud_routes() {
    let scratch = scaffold("api");
    let routes = ok(&["routes", scratch.str()]);
    for expected in [
        "GET     /api/v1/notes",
        "POST    /api/v1/notes",
        "GET     /api/v1/notes/{id}",
        "PATCH   /api/v1/notes/{id}",
        "DELETE  /api/v1/notes/{id}",
    ] {
        assert!(routes.contains(expected), "missing `{expected}`:\n{routes}");
    }
}

/// The auth template's point is the middleware contract, so pin it: the
/// two `/me` routes carry `RequireAuth` and the two `/auth` ones do not.
#[test]
fn the_auth_template_puts_require_auth_only_where_it_belongs() {
    let scratch = scaffold("auth");
    let routes = ok(&["routes", scratch.str()]);
    for line in routes.lines() {
        if line.contains("/api/v1/me") {
            assert!(line.contains("RequireAuth"), "unguarded: {line}");
        }
        if line.contains("/api/v1/auth/") {
            assert!(
                !line.contains("RequireAuth"),
                "register/login cannot require a session: {line}"
            );
        }
    }
}

/// `migrate new` runs offline, so a template's schema can be turned into
/// DDL without a database. A template whose tables do not diff is one the
/// user cannot deploy.
#[test]
fn a_template_with_tables_produces_a_first_migration() {
    for kind in ["api", "auth"] {
        let scratch = scaffold(kind);
        ok(&["migrate", "new", "init", scratch.str()]);
        let dir = scratch.path().join("migrations");
        let mut written = Vec::new();
        collect(&dir, &mut written);
        let names: Vec<String> = written
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into())
            .collect();
        assert!(
            names.iter().any(|n| n.ends_with(".up.sql")),
            "{kind}: no up migration in {names:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with(".down.sql")),
            "{kind}: no down migration in {names:?}"
        );
        let up = written
            .iter()
            .find(|p| p.to_string_lossy().ends_with(".up.sql"))
            .map(|p| std::fs::read_to_string(p).unwrap_or_default())
            .unwrap_or_default();
        assert!(
            up.to_uppercase().contains("CREATE TABLE"),
            "{kind}: the first migration creates nothing:\n{up}"
        );
    }
}

/// `jwc new` must not scaffold on top of somebody's work.
#[test]
fn new_refuses_a_non_empty_directory() {
    let scratch = Scratch::new("occupied");
    std::fs::create_dir_all(scratch.path()).expect("mkdir");
    std::fs::write(scratch.path().join("notes.txt"), "mine").expect("write");

    let out = jwc(&["new", "demo", "--path", scratch.str()]);
    assert!(!out.status.success(), "should have refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(scratch.path().join("notes.txt")).unwrap_or_default(),
        "mine",
        "the existing file was clobbered"
    );
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

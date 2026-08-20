//! Emitted migrations, byte for byte — and that they apply.
//!
//! Every case in `tests/diff_corpus/cases/` is run through `migrate::plan`
//! and the whole result is frozen in `tests/migrate_golden/<case>.txt`: one
//! reviewed file per case, holding the up file, the down file and the
//! snapshot, or the reason there is nothing to write.
//!
//! Regenerate with:
//!
//! ```bash
//! JWC_BLESS=1 cargo test --test migrate_golden
//! ```
//!
//! The apply half is opt-in and needs a psql connection string. **A SKIPPED
//! line is not a pass** — a migration that reads correctly and does not run
//! is not a migration:
//!
//! ```bash
//! JWC_V1_PG='-h 127.0.0.1 -p 5432 -U postgres' cargo test --test migrate_golden
//! ```

use jwc::{ddl, migrate, model, snapshot, workspace::Workspace};
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cases() -> Vec<String> {
    let dir = repo_root().join("tests/diff_corpus/cases");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .expect("tests/diff_corpus/cases")
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".after.jwc")
                .map(|s| s.to_string())
        })
        .collect();
    out.sort();
    out
}

fn model_of(path: &Path) -> model::SchemaModel {
    let ws = Workspace::load(path).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    model::build(&ws).model
}

fn before(case: &str) -> Option<model::SchemaModel> {
    let p = repo_root().join(format!("tests/diff_corpus/cases/{case}.before.jwc"));
    p.exists().then(|| model_of(&p))
}

fn after(case: &str) -> model::SchemaModel {
    model_of(&repo_root().join(format!("tests/diff_corpus/cases/{case}.after.jwc")))
}

/// The whole plan as one reviewable document.
fn render(case: &str) -> String {
    let prev = before(case).map(|m| snapshot::of(&m)).unwrap_or_default();
    let current = after(case);
    let plan = migrate::plan(&prev, &current, 1, case);

    if plan.has_errors() {
        let codes: Vec<&str> = plan.diags.iter().map(|(_, d)| d.code).collect();
        return format!("refused: {}\n", codes.join(", "));
    }
    if plan.is_empty() {
        return "no schema changes\n".to_string();
    }
    let mut out = String::new();
    for f in &plan.files {
        out.push_str(&format!("══ {}.up.sql\n{}", f.stem, f.up));
        out.push_str(&format!("\n══ {}.down.sql\n{}", f.stem, f.down));
    }
    out
}

#[test]
fn emitted_migrations_match_byte_for_byte() {
    let root = repo_root();
    let bless = std::env::var("JWC_BLESS").is_ok();
    let mut wrong: Vec<String> = Vec::new();
    for case in cases() {
        let text = render(&case);
        let golden = root.join(format!("tests/migrate_golden/{case}.txt"));
        if bless {
            std::fs::write(&golden, &text).expect("bless");
            continue;
        }
        let want = std::fs::read_to_string(&golden).unwrap_or_default();
        if text != want {
            wrong.push(format!("── {case}\nwant:\n{want}\ngot:\n{text}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} migration(s) changed. Review, then re-bless with \
         JWC_BLESS=1 cargo test --test migrate_golden\n\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn generation_is_deterministic() {
    // migrations.md §10.1 — two runs on the same source and snapshot
    // produce byte-identical output.
    for case in cases() {
        assert_eq!(render(&case), render(&case), "{case}");
    }
}

#[test]
fn a_no_transaction_file_holds_nothing_else() {
    // The generator never writes one, but the files are checked in and
    // editable. E1101 is what stands between a hand-added statement there
    // and its being applied with no transaction to roll it back.
    let ok = format!(
        "{}\nALTER TYPE billing.invoice_status ADD VALUE IF NOT EXISTS 'refunded';\n",
        migrate::NO_TRANSACTION
    );
    assert!(migrate::check_no_transaction(&ok).is_ok());

    let bad = format!(
        "{}\nALTER TYPE b.s ADD VALUE IF NOT EXISTS 'x';\nUPDATE b.t SET s = 'x';\n",
        migrate::NO_TRANSACTION
    );
    let err = migrate::check_no_transaction(&bad).expect_err("should refuse");
    assert!(err.contains("UPDATE"), "{err}");

    // A file without the marker is an ordinary migration and unconstrained.
    assert!(migrate::check_no_transaction("UPDATE b.t SET s = 'x';").is_ok());
}

// ── the apply half ─────────────────────────────────────────────────────

fn psql(args: &[&str], conn: &str, db: Option<&str>) -> (bool, String) {
    let mut cmd = Command::new("psql");
    // `-d` after a URI is a whole new connection target, not a database
    // name — see `common::psql_target`.
    match db {
        Some(d) => {
            for part in common::psql_target(conn, d) {
                cmd.arg(part);
            }
        }
        None => {
            cmd.args(conn.split_whitespace());
        }
    }
    cmd.args(["-q", "-v", "ON_ERROR_STOP=1"]);
    cmd.args(args);
    let out = cmd.output().expect("psql");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn script(model: &model::SchemaModel) -> String {
    let ws = Workspace {
        root: PathBuf::new(),
        files: Vec::new(),
        packages: Default::default(),
        manifest: None,
    };
    ddl::render(&ws, &ddl::emit(model), false)
}

#[test]
fn every_migration_applies_and_reverses() {
    let Ok(conn) = std::env::var("JWC_V1_PG") else {
        eprintln!(
            "SKIPPED every_migration_applies_and_reverses — set JWC_V1_PG to a psql \
             connection string. A SKIPPED line is not a pass."
        );
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = "jwc_migrate_golden";

    for case in cases() {
        let prev_model = before(&case);
        let prev = prev_model.as_ref().map(snapshot::of).unwrap_or_default();
        let current = after(&case);
        let plan = migrate::plan(&prev, &current, 1, &case);
        if plan.has_errors() || plan.is_empty() {
            continue;
        }

        // Explicitly through `postgres`, the maintenance database. With
        // `None` psql picks the default, which for the flag form of
        // `JWC_V1_PG` is the *username* — so this worked only because CI's
        // user happens to be `postgres` and that database happens to
        // exist. Any other user and the step fails on `database "…" does
        // not exist` while naming nothing that is actually wrong.
        let (ok, log) = psql(
            &[
                "-c",
                &format!("drop database if exists {db}"),
                "-c",
                &format!("create database {db}"),
            ],
            &conn,
            Some("postgres"),
        );
        assert!(ok, "{case}: could not create the test database\n{log}");

        // Start from what the previous schema would have created.
        if let Some(m) = &prev_model {
            let p = dir.path().join("base.sql");
            std::fs::write(&p, script(m)).expect("write");
            let (ok, log) = psql(&["-f", p.to_str().expect("utf8")], &conn, Some(db));
            assert!(ok, "{case}: the base schema did not apply\n{log}");
        }

        for f in &plan.files {
            let p = dir.path().join(format!("{}.up.sql", f.stem));
            std::fs::write(&p, &f.up).expect("write");
            let (ok, log) = psql(&["-f", p.to_str().expect("utf8")], &conn, Some(db));
            assert!(ok, "{case}: {}.up.sql did not apply\n{log}", f.stem);
        }

        // §9.2 — a file carrying an irreversible marker is not run at all.
        for f in plan.files.iter().rev() {
            if f.down.contains(migrate::IRREVERSIBLE) {
                continue;
            }
            let p = dir.path().join(format!("{}.down.sql", f.stem));
            std::fs::write(&p, &f.down).expect("write");
            let (ok, log) = psql(&["-f", p.to_str().expect("utf8")], &conn, Some(db));
            assert!(ok, "{case}: {}.down.sql did not apply\n{log}", f.stem);
        }
    }

    let (_, _) = psql(
        &["-c", &format!("drop database if exists {db}")],
        &conn,
        None,
    );
}

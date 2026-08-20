//! The diff, case by case: two programs in, a list of operations out.
//!
//! A case is a pair of files under `tests/diff_corpus/cases/`:
//!
//! * `<name>.before.jwc` — the schema the previous snapshot recorded.
//!   Absent means an empty database.
//! * `<name>.after.jwc` — the schema now, annotated with what the diff
//!   must produce.
//!
//! The annotations sit in ordinary `--` comments at the top of the `after`
//! file, in the order the operations come out:
//!
//! ```text
//! -- ops: 2 add_column org.orgs.region
//! -- ops: 9 drop_column org.orgs.legacy
//! -- diag: E0440
//! ```
//!
//! The number is the phase (migrations.md §4), so the corpus pins the
//! *order* of the migration and not only its contents — which is the half
//! that decides whether the file applies at all.
//!
//! **Exact in both directions.** An operation with no annotation and an
//! annotation with no operation both fail. That is what makes this a
//! specification of the diff rather than a smoke test.
//!
//! Rewrite the annotation blocks after a deliberate change with:
//!
//! ```bash
//! JWC_BLESS=1 cargo test --test diff_corpus
//! ```
//!
//! and then *read the diff* — blessing without reading turns the corpus
//! back into a smoke test.

use jwc::{diff, model, snapshot, workspace::Workspace};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cases() -> Vec<(String, PathBuf)> {
    let dir = repo_root().join("tests/diff_corpus/cases");
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .expect("tests/diff_corpus/cases")
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            let name = p
                .file_name()?
                .to_str()?
                .strip_suffix(".after.jwc")?
                .to_string();
            Some((name, p))
        })
        .collect();
    out.sort();
    out
}

/// Parse, resolve, and fail loudly rather than diffing a broken program.
fn load(path: &Path) -> (Workspace, model::SchemaModel) {
    let ws = Workspace::load(path).expect("load");
    assert!(
        !ws.has_parse_errors(),
        "{} did not parse:\n{}",
        path.display(),
        ws.parse_errors().join("")
    );
    let built = model::build(&ws);
    let errors: Vec<String> = built
        .diags
        .iter()
        .filter(|(_, d)| d.severity == jwc::diag::Severity::Error)
        .map(|(loc, d)| ws.render(*loc, d))
        .collect();
    assert!(
        errors.is_empty(),
        "{} has schema errors:\n{}",
        path.display(),
        errors.join("")
    );
    (ws, built.model)
}

fn annotations(text: &str, tag: &str) -> Vec<String> {
    let prefix = format!("-- {tag}:");
    text.lines()
        .filter_map(|l| l.trim().strip_prefix(&prefix))
        .map(|s| s.trim().to_string())
        .collect()
}

#[test]
fn every_case_produces_exactly_its_annotated_operations() {
    let dir = repo_root().join("tests/diff_corpus/cases");
    let mut failures: Vec<String> = Vec::new();
    let all = cases();
    assert!(all.len() >= 20, "expected the corpus, saw {}", all.len());

    for (name, after_path) in all {
        let after_text = std::fs::read_to_string(&after_path).expect("read after");
        let before_path = dir.join(format!("{name}.before.jwc"));

        let prev = if before_path.exists() {
            let (_, m) = load(&before_path);
            snapshot::of(&m)
        } else {
            snapshot::Snapshot::default()
        };
        let (ws, current) = load(&after_path);
        let next = snapshot::of(&current);
        let d = diff::compute(&prev, &next, &diff::Source::of(&current));

        let got: Vec<String> = d
            .changes
            .iter()
            .map(|c| format!("{} {}", c.op.phase() as u8, c.op.describe()))
            .collect();
        let want = annotations(&after_text, "ops");
        if got != want {
            failures.push(format!(
                "{name}: operations differ\n  want:\n{}\n  got:\n{}",
                want.iter()
                    .map(|l| format!("    -- ops: {l}\n"))
                    .collect::<String>(),
                got.iter()
                    .map(|l| format!("    -- ops: {l}\n"))
                    .collect::<String>(),
            ));
        }

        let got_diags: Vec<String> = d.diags.iter().map(|(_, x)| x.code.to_string()).collect();
        let want_diags = annotations(&after_text, "diag");

        if std::env::var("JWC_BLESS").is_ok() {
            let body: String = after_text
                .lines()
                .skip_while(|l| l.trim().starts_with("-- ops:") || l.trim().starts_with("-- diag:"))
                .collect::<Vec<_>>()
                .join("\n");
            let head: String = got
                .iter()
                .map(|l| format!("-- ops: {l}\n"))
                .chain(got_diags.iter().map(|c| format!("-- diag: {c}\n")))
                .collect();
            let head = if head.is_empty() {
                head
            } else {
                format!("{head}\n")
            };
            std::fs::write(
                &after_path,
                format!("{head}{}\n", body.trim_start_matches('\n')),
            )
            .expect("bless");
            continue;
        }
        if got_diags != want_diags {
            let rendered: String = d.diags.iter().map(|(l, x)| ws.render(*l, x)).collect();
            failures.push(format!(
                "{name}: diagnostics differ\n  want: {want_diags:?}\n  got:  {got_diags:?}\n{rendered}"
            ));
        }

        // migrations.md §10.1 — two runs on the same inputs are identical.
        let again = diff::compute(&prev, &next, &diff::Source::of(&current));
        let a: Vec<String> = again.changes.iter().map(|c| c.op.describe()).collect();
        let b: Vec<String> = d.changes.iter().map(|c| c.op.describe()).collect();
        if a != b {
            failures.push(format!("{name}: two runs disagree"));
        }
    }

    assert!(failures.is_empty(), "\n\n{}", failures.join("\n\n"));
}

/// The effective snapshot is what gets written to disk, so re-diffing
/// against it must be a no-op — otherwise every `migrate new` would emit the
/// same migration again forever.
#[test]
fn applying_a_diff_leaves_nothing_to_do() {
    let dir = repo_root().join("tests/diff_corpus/cases");
    for (name, after_path) in cases() {
        let before_path = dir.join(format!("{name}.before.jwc"));
        let prev = if before_path.exists() {
            let (_, m) = load(&before_path);
            snapshot::of(&m)
        } else {
            snapshot::Snapshot::default()
        };
        let (_, current) = load(&after_path);
        let next = snapshot::of(&current);
        let src = diff::Source::of(&current);
        let first = diff::compute(&prev, &next, &src);

        // The second run sees the state the first one left, and the `was`
        // markers are still in the source — which is exactly the situation
        // §6.4 describes, so a stale-marker warning is expected and the
        // operations are not.
        let second = diff::compute(&first.effective, &next, &src);
        let ops: Vec<String> = second.changes.iter().map(|c| c.op.describe()).collect();
        assert!(
            ops.is_empty(),
            "{name}: re-running the diff against its own result still wants:\n  {}",
            ops.join("\n  ")
        );
    }
}

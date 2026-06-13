//! Idempotency harness for `jwc::fmt::format_source`.
//!
//! Walks every `.jwc` file under the project's fixture roots
//! (`examples/`, `templates/`, and `tests/conformance/cases/`) and asserts
//! that a second formatting pass produces byte-identical output to the
//! first. This is the formatter's load-bearing invariant — without it,
//! `jwc fmt --check` becomes a flapping CI step.
//!
//! Skipping: files whose **first line** is `// FMT: skip` are excluded.
//! Use this for golden artefacts that the formatter shouldn't touch
//! (e.g. malformed parser-regression fixtures kept verbatim).
//!
//! What this test does NOT check:
//! - That `format_source` matches any specific expected output. The shape
//!   of canonical output is governed by `src/fmt.rs` and exercised in its
//!   own unit tests; here we only require the fixed-point property.
//! - That the formatted output is itself valid `.jwc`. The line-based
//!   tier passes through unparseable input unchanged, which is desired.

use std::path::{Path, PathBuf};

use jwc::fmt;

fn fixture_roots() -> Vec<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    vec![
        manifest_dir.join("examples"),
        manifest_dir.join("templates"),
        manifest_dir.join("tests").join("conformance").join("cases"),
    ]
}

fn collect_all(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for r in roots {
        if !r.exists() {
            continue;
        }
        // We reuse the formatter's own walker so the test mirrors what the
        // CLI would see (same skipped directories, same extension filter).
        match fmt::collect_jwc_files(r) {
            Ok(mut files) => out.append(&mut files),
            Err(e) => panic!("walk failed under {}: {e:#}", r.display()),
        }
    }
    out.sort();
    out.dedup();
    out
}

fn is_skipped(path: &Path, src: &str) -> bool {
    let first_line = src.lines().next().unwrap_or("").trim();
    if first_line == "// FMT: skip" {
        eprintln!(
            "fmt_idempotency: skipping {} (// FMT: skip)",
            path.display()
        );
        return true;
    }
    false
}

#[test]
fn every_fixture_jwc_file_is_format_idempotent() {
    let roots = fixture_roots();
    let files = collect_all(&roots);
    assert!(
        !files.is_empty(),
        "no .jwc fixture files found under {:?} — layout drifted?",
        roots
    );

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut skipped = 0usize;

    for file in &files {
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read: {e}", file.display()));
                continue;
            }
        };

        if is_skipped(file, &src) {
            skipped += 1;
            continue;
        }

        let once = fmt::format_source(&src);
        let twice = fmt::format_source(&once);
        if once != twice {
            failures.push(format!(
                "{}: format_source is not idempotent (first pass != second pass)",
                file.display()
            ));
        }
        checked += 1;
    }

    eprintln!(
        "fmt_idempotency: checked {checked} file(s) across {} root(s); skipped {skipped}",
        roots.len()
    );

    assert!(
        failures.is_empty(),
        "{} file(s) failed the idempotency check:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

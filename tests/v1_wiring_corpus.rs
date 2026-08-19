//! v0.24.0 acceptance: the wiring corpus.
//!
//! Each `tests/wiring_corpus/cases/*.jwc` is annotated inline:
//!
//! ```text
//! return $row.body;                       -- expect: E0310
//! ```
//!
//! The annotation names the diagnostic that must be reported **on that
//! line**. The match is exact in both directions: a missing diagnostic and
//! an unannotated one both fail. That is what makes the corpus a
//! specification rather than a smoke test — it pins the absence of a
//! diagnostic as firmly as its presence.
//!
//! Cases are checked against `tests/wiring_corpus/prelude.jwc`, which supplies
//! the schema they share. This corpus covers the passes the type checker
//! does not: the route table, middleware composition, typed `context`, and
//! the error model.

use jwc::v1::{check, diag::Severity, model, symbols, wiring, workspace::Workspace};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cases() -> Vec<PathBuf> {
    let dir = repo_root().join("tests/wiring_corpus/cases");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("tests/wiring_corpus/cases")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jwc"))
        .collect();
    out.sort();
    out
}

/// `-- expect: E0310` on a line means that code is reported on that line.
fn expectations(text: &str) -> BTreeSet<(usize, String)> {
    let mut out = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        if let Some(pos) = line.find("-- expect:") {
            for code in line[pos + "-- expect:".len()..]
                .split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                out.insert((i + 1, code.to_string()));
            }
        }
    }
    out
}

fn observed(case: &Path) -> (BTreeSet<(usize, String)>, String) {
    let dir = std::env::temp_dir().join(format!(
        "jwc_v1_corpus_{}",
        case.file_stem().and_then(|s| s.to_str()).unwrap_or("x")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::copy(
        repo_root().join("tests/wiring_corpus/prelude.jwc"),
        dir.join("prelude.jwc"),
    )
    .expect("prelude");
    std::fs::copy(case, dir.join("case.jwc")).expect("case");

    let ws = Workspace::load(&dir).expect("load");
    assert!(
        !ws.has_parse_errors(),
        "{} must parse:\n{}",
        case.display(),
        ws.parse_errors().join("")
    );
    let built = model::build(&ws);
    let syms = symbols::build(&ws, &built.model);
    let checked = check::check(&ws, &syms, &built.model);
    let wired = wiring::wire(&ws, &syms);

    // The case file is `case.jwc`; the prelude must stay clean, and a
    // diagnostic reported against it is a bug in the fixture.
    let case_index = ws
        .files
        .iter()
        .position(|f| f.source.path.file_name().and_then(|s| s.to_str()) == Some("case.jwc"))
        .expect("case file");

    let mut out = BTreeSet::new();
    let mut rendered = String::new();
    for (loc, d) in built
        .diags
        .iter()
        .chain(&syms.diags)
        .chain(&checked.diags)
        .chain(&wired.diags)
    {
        rendered.push_str(&ws.render(*loc, d));
        if loc.file != case_index {
            // A diagnostic on the prelude means the shared schema is wrong.
            if d.severity == Severity::Error {
                panic!(
                    "prelude must be clean, but {} reported:\n{}",
                    case.display(),
                    ws.render(*loc, d)
                );
            }
            continue;
        }
        let (line, _) = ws.files[loc.file].source.line_col(loc.span.start);
        out.insert((line, d.code.to_string()));
    }
    let _ = std::fs::remove_dir_all(&dir);
    (out, rendered)
}

#[test]
fn corpus_matches_exactly() {
    let mut failures = String::new();
    for case in cases() {
        let text = std::fs::read_to_string(&case).expect("read");
        let want = expectations(&text);
        let (got, rendered) = observed(&case);

        let missing: Vec<_> = want.difference(&got).collect();
        let extra: Vec<_> = got.difference(&want).collect();
        if missing.is_empty() && extra.is_empty() {
            continue;
        }
        failures.push_str(&format!("\n=== {} ===\n", case.display()));
        for (line, code) in missing {
            failures.push_str(&format!("  expected {code} on line {line}, not reported\n"));
        }
        for (line, code) in extra {
            let src = text.lines().nth(line - 1).unwrap_or("").trim();
            failures.push_str(&format!(
                "  unexpected {code} on line {line}: {src}\n"
            ));
        }
        failures.push_str(&format!("--- all diagnostics ---\n{rendered}"));
    }
    assert!(failures.is_empty(), "{failures}");
}

#[test]
fn every_case_asserts_something() {
    // A corpus file with no annotations and no diagnostics proves nothing
    // was checked. Each file must either expect a diagnostic or be a
    // deliberate accept-case with at least one exercised construct.
    for case in cases() {
        let text = std::fs::read_to_string(&case).expect("read");
        assert!(
            text.contains("-- expect:") || text.contains("function"),
            "{} asserts nothing",
            case.display()
        );
    }
}

#[test]
fn the_sample_wires_clean() {
    let ws = Workspace::load(repo_root().join("docs/spec/v1/sample")).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let built = model::build(&ws);
    let syms = symbols::build(&ws, &built.model);
    let wired = wiring::wire(&ws, &syms);
    let reported: Vec<String> = wired
        .diags
        .iter()
        .map(|(loc, d)| ws.render(*loc, d))
        .collect();
    assert!(reported.is_empty(), "{}", reported.join(""));

    // The resolved table is the artefact `jwc v1 routes` prints and the one
    // E0710 / E0803 are read against.
    let patch = wired
        .routes
        .iter()
        .find(|r| r.method == "PATCH" && r.pattern == "/api/v1/orgs/{org_id}")
        .expect("PATCH /api/v1/orgs/{org_id}");
    assert_eq!(
        patch.chain,
        vec!["RequireAuth", "RequireOrgMember", "Audit", "RequireOrgAdmin"],
        "block list in written order, then the route list (middleware.md §4.1)"
    );
    assert_eq!(
        patch.after,
        vec!["Audit"],
        "after blocks run in reverse chain order"
    );
    assert_eq!(
        patch.params,
        vec![("org_id".to_string(), "bigint".to_string())]
    );
}

/// routing.md §4.2 — a literal segment beats a parameter segment, and that
/// is fixed precedence rather than registration order. Two such routes are
/// not a conflict.
#[test]
fn literal_and_parameter_in_one_slot_are_two_routes() {
    let dir = std::env::temp_dir().join("jwc_v1_precedence");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::write(
        dir.join("a.jwc"),
        concat!(
            "database App : Postgres;\n",
            "routes \"/orgs/settings\" { route GET \"\" { return json(1); } }\n",
            "routes \"/orgs/{org_id: bigint}\" { route GET \"\" { return json(1); } }\n"
        ),
    )
    .expect("write");
    let ws = Workspace::load(&dir).expect("load");
    let built = model::build(&ws);
    let syms = symbols::build(&ws, &built.model);
    let wired = wiring::wire(&ws, &syms);
    let errors: Vec<_> = wired
        .diags
        .iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(wired.routes.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

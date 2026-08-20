//! Every ```jwc block in the README and `docs/spec/v1/` must be real JWC.
//!
//! Nothing checked the documentation against the compiler, so it drifted:
//! the README's headline example and three reference pages showed forms the
//! parser had never accepted. A reader copying the first example on the
//! front page got a syntax error.
//!
//! Only parsing is asserted, never checking: documentation deliberately
//! references tables and classes it does not define.
//!
//! `docs/archive-0.9/` is **not** checked. It documents the language that
//! the v0.25.0 cutover removed, and checking it against this compiler would
//! assert that a dead grammar still parses.
//!
//! ## Illustrative blocks
//!
//! Some blocks are prose, not programs — operator tables, `{ ... }`
//! elisions, bare expression lists with trailing comments. Only a **bare**
//! ```` ```jwc ```` fence is compiled, so mark those with
//! ```` ```jwc no-compile ````. The marker sits in the fence's info string,
//! which the docs site ignores, so it costs the reader nothing while
//! keeping the exemption explicit in source.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn markdown_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = vec![root.join("README.md")];
    let mut stack = vec![root.join("docs")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                // Vendored site build output, not authored docs.
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // `archive-0.9` documents the language the cutover
                // removed; checking it here would assert that a dead
                // grammar still parses.
                if matches!(
                    name,
                    "node_modules" | "build" | ".docusaurus" | "archive-0.9"
                ) {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Extract bare ```jwc fenced blocks with the 1-based line each starts on.
fn jwc_blocks(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((i, line)) = lines.next() {
        // Only a bare ```jwc fence. An info string (```jwc no-compile,
        // ```jwc title="x") marks a block that is shown, not compiled.
        if line.trim() != "```jwc" {
            continue;
        }
        let mut body = String::new();
        for (_, l) in lines.by_ref() {
            if l.trim_start().starts_with("```") {
                break;
            }
            body.push_str(l);
            body.push('\n');
        }
        out.push((i + 2, body));
    }
    out
}

/// v1 spec blocks, checked with the v1 front-end. Same excerpt problem:
/// a clause shown on its own is not a program, so try the positions an
/// excerpt can legally occupy.
fn parses_somewhere(body: &str) -> bool {
    const HEADER: &str = concat!(
        "database App : Postgres;\n",
        "schema s of App;\n",
        "table T of App.s { id bigint primary key identity; }\n",
    );
    let contexts = [
        body.to_string(),
        format!("{HEADER}{body}\n"),
        format!("{HEADER}function f() {{\n{body}\n}}\n"),
        format!("{HEADER}function f() {{\nreturn {body};\n}}\n"),
        format!("{HEADER}table U of App.s {{\n{body}\n}}\n"),
        format!("{HEADER}class C {{\n{body}\n}}\n"),
        format!("{HEADER}middleware M {{\n{body}\n}}\n"),
        format!("{HEADER}routes \"/x\" {{\nroute GET \"\" {{\n{body}\n}}\n}}\n"),
        format!("{HEADER}view V of App.s {{\n{body}\n}}\n"),
    ];
    contexts
        .iter()
        .any(|src| !jwc::parse_str("<doc>", src).has_errors())
}

#[test]
fn every_documented_jwc_example_parses() {
    let root = repo_root();
    let mut broken = Vec::new();
    let mut checked = 0usize;

    for file in markdown_files() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, body) in jwc_blocks(&text) {
            if body.trim().is_empty() {
                continue;
            }
            checked += 1;
            let ok = parses_somewhere(&body);
            if !ok {
                let rel: &Path = file.strip_prefix(&root).unwrap_or(&file);
                broken.push(format!("{}:{line}", rel.display()));
            }
        }
    }

    // A floor, not a target: it exists so a broken block-scanner reads as
    // a failure rather than as "nothing to check". It dropped when the
    // 0.9.x docs were archived and the spec became the corpus.
    assert!(
        checked > 20,
        "expected to find the documented examples, saw {checked}"
    );
    assert!(
        broken.is_empty(),
        "{} documented example(s) don't parse — a reader copying these gets a \
         syntax error. Fix the example, or if it is deliberately an excerpt \
         (elisions, operator tables), change its fence to ```jwc no-compile:\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
}

/// `spec-coverage.json` matches the sample it claims to describe.
///
/// ROADMAP §10 lists this file as the mitigation for "the sample stops
/// keeping up with the spec": a construct not tied to a clause is supposed
/// to fail the build. Nothing ran the generator — not CI, not a test — so
/// the file was a snapshot of whenever it was last produced by hand, and
/// it had drifted from the sample it names. A mitigation nothing executes
/// is not one.
#[test]
fn the_spec_coverage_map_is_current() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let map = root.join("docs/spec/v1/spec-coverage.json");
    let before = std::fs::read_to_string(&map).expect("spec-coverage.json");

    let out = std::process::Command::new("python3")
        .arg(root.join("docs/spec/v1/check_sample.py"))
        .output();
    let Ok(out) = out else {
        eprintln!("SKIPPED the_spec_coverage_map_is_current — no python3");
        return;
    };

    // The generator rewrites the file in place, so restore it before
    // asserting: a failing test must not leave the tree dirty.
    let after = std::fs::read_to_string(&map).expect("spec-coverage.json");
    std::fs::write(&map, &before).expect("restore");

    assert!(
        out.status.success(),
        "check_sample.py rejected the sample:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        before == after,
        "spec-coverage.json is stale — run `python3 docs/spec/v1/check_sample.py` \
         and commit the result"
    );
}

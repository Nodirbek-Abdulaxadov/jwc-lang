//! Sprint 8 golden harness: every JWC project under `examples/` must load
//! (parse + cross-file validate) cleanly. Cheaper than the run-vs-build
//! diff (which needs Docker + cargo) and catches the most common
//! regression — a parser or validator edit that breaks a known-good
//! project — on every PR.
//!
//! Each subdirectory of `examples/` that contains a `jwcproj.json` is
//! treated as a project root. Loose `.jwc` files directly inside
//! `examples/` (no manifest) are exercised as single-file inputs against
//! `parse_program` + `validate_program`.

use std::path::PathBuf;

use jwc::parser::{parse_program, validate_program};
use jwc::project;

fn examples_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("examples")
}

fn has_manifest(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("jwcproj")
    })
}

fn collect_project_roots(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Skip generated and non-project directories.
            if matches!(
                name,
                "jmeter" | "bench-cs" | "target" | ".git" | ".jwc-build" | "bin" | "node_modules"
            ) {
                continue;
            }
            if has_manifest(&p) {
                out.push(p);
            } else {
                // Walk one level deeper (e.g. examples/pkg-demo/app/).
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn collect_loose_jwc_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jwc") {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[test]
fn every_example_project_loads_and_validates() {
    let dir = examples_dir();
    let projects = collect_project_roots(&dir);
    assert!(
        !projects.is_empty(),
        "no example projects with jwcproj.json under {} — fixture layout drifted?",
        dir.display()
    );

    let mut failures: Vec<String> = Vec::new();
    for root in &projects {
        match project::load_project_from_root(root) {
            Ok(_loaded) => {
                // load_project_from_root already runs validate_program on
                // the merged Program internally.
            }
            Err(e) => {
                failures.push(format!("{}: load: {e:#}", root.display()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} example project(s) failed to load:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

#[test]
fn every_loose_example_file_parses_and_validates() {
    let dir = examples_dir();
    let files = collect_loose_jwc_files(&dir);
    // Loose files are optional — empty list is allowed, but the moment one
    // exists it has to keep parsing.
    let mut failures: Vec<String> = Vec::new();
    for path in &files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read: {e}", path.display()));
                continue;
            }
        };
        let program = match parse_program(&src) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{}: parse: {e:#}", path.display()));
                continue;
            }
        };
        if let Err(e) = validate_program(&program) {
            failures.push(format!("{}: validate: {e:#}", path.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "{} loose example file(s) failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

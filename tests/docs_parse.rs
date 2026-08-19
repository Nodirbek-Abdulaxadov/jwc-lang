//! Every ```jwc block in the README and `docs/` must be real JWC.
//!
//! Nothing checked the documentation against the compiler, so it drifted: the
//! README's headline example, the getting-started page and the `dbcontext`
//! reference page all showed a `dbcontext AppDb { ... }` block form and
//! colon-separated entity fields that the parser has never accepted. A reader
//! copying the first example on the front page got a syntax error.
//!
//! Only `parse_program` is asserted, never `validate_program`: documentation
//! deliberately references entities and classes it doesn't define.
//!
//! ## Two languages
//!
//! `docs/spec/v1/` describes the redesigned 1.0 language, which is a
//! different grammar (ROADMAP §0). Blocks under that directory are checked
//! with the **v1** front-end (`jwc::v1`); everything else with the 0.9.x
//! parser. Routing by path rather than by fence keeps the marker out of the
//! author's way, and it means the specification's examples are checked
//! against the parser that has to accept them.
//!
//! ## Illustrative blocks
//!
//! Some blocks are prose, not programs — operator tables, `{ ... }` elisions,
//! bare expression lists with trailing comments. Only a **bare** ```` ```jwc ````
//! fence is compiled, so mark those with ```` ```jwc no-compile ````. The
//! marker sits in the fence's info string, which the docs site ignores, so it
//! costs the reader nothing while keeping the exemption explicit in source.

use std::path::{Path, PathBuf};

use jwc::parser::parse_program;

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
                if matches!(name, "node_modules" | "build" | ".docusaurus") {
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
fn parses_somewhere_v1(body: &str) -> bool {
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
        .any(|src| !jwc::v1::parse_str("<doc>", src).has_errors())
}

/// Blocks are written as excerpts, so try the positions an excerpt can
/// legally occupy before calling it broken.
fn parses_somewhere(body: &str) -> bool {
    const HEADER: &str = concat!(
        "dbcontext AppDb: Postgres;\n",
        "entity Item of AppDb { id int pk autoincrement; name varchar(50); }\n",
    );
    let contexts = [
        // `namespace` / `mount` have to precede every other declaration.
        body.to_string(),
        format!("{HEADER}{body}\n"),
        format!("{HEADER}function f() {{\n{body}\n}}\n"),
        format!("{HEADER}route GET \"/x\" {{\n{body}\n}}\n"),
        format!("{HEADER}dome S {{\n  function f() {{\n{body}\n  }}\n}}\n"),
    ];
    contexts.iter().any(|src| parse_program(src).is_ok())
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
            let is_v1_spec = file.starts_with(root.join("docs/spec/v1"));
            let ok = if is_v1_spec {
                parses_somewhere_v1(&body)
            } else {
                parses_somewhere(&body)
            };
            if !ok {
                let rel: &Path = file.strip_prefix(&root).unwrap_or(&file);
                broken.push(format!("{}:{line}", rel.display()));
            }
        }
    }

    assert!(
        checked > 50,
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

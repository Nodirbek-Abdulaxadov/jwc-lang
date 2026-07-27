//! Every snippet the VS Code extension ships must be syntactically valid JWC.
//!
//! A snippet that the parser rejects is worse than no snippet: the editor
//! offers it by name, the user accepts the completion, and the file stops
//! compiling. Two shipped snippets had drifted from the grammar this way —
//! `transaction` proposed `transaction <Name> { ... }` (the statement takes
//! no name), and the "Async Function" snippet expanded to `async function`
//! inside a `dome`, which the parser refused until dome members learned the
//! modifier.
//!
//! Nothing here checks *semantics*: snippet placeholders are deliberately
//! generic (`EntityName`, `db.Table`), so `validate_program` would fail on
//! names that don't exist. Only `parse_program` is asserted.

use std::path::PathBuf;

use jwc::parser::parse_program;

fn snippets_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vscode-extension")
        .join("snippets")
        .join("jwc.code-snippets")
}

/// Expand TextMate placeholders into their default text so the body becomes
/// ordinary source: `${1:name}` → `name`, `${1|a,b|}` → `a`, `$1`/`$0` → a
/// bare identifier / nothing. Runs to a fixed point because placeholders
/// nest (`${2: at "/${3:prefix}"}`).
fn expand_placeholders(body: &str) -> String {
    let choice = regex::Regex::new(r"\$\{\d+\|([^,|]+)[^|]*\|\}").unwrap();
    let named = regex::Regex::new(r"\$\{\d+:([^{}$]*)\}").unwrap();
    let empty = regex::Regex::new(r"\$\{\d+\}").unwrap();
    let bare = regex::Regex::new(r"\$(\d+)").unwrap();

    let mut out = body.to_string();
    for _ in 0..8 {
        let before = out.clone();
        out = choice.replace_all(&out, "$1").to_string();
        out = named.replace_all(&out, "$1").to_string();
        out = empty.replace_all(&out, "placeholder").to_string();
        if out == before {
            break;
        }
    }
    // `$0` is the final cursor stop and carries no text.
    out = out.replace("$0", "");
    out = bare.replace_all(&out, "placeholder").to_string();
    out.replace("\\$", "$")
}

/// A snippet is valid if it parses in *some* legal position. Statement
/// snippets only make sense inside a body; declaration snippets only at the
/// top level.
fn parses_somewhere(body: &str) -> bool {
    const HEADER: &str = concat!(
        "dbcontext AppDb: Postgres;\n",
        "entity Item of AppDb { id int pk autoincrement; name varchar(50); }\n",
        "middleware Auth { return null; }\n",
    );

    let contexts = [
        // `namespace` / `mount` must precede other declarations.
        body.to_string(),
        format!("{HEADER}{body}\n"),
        format!("{HEADER}function f() {{\n{body}\n}}\n"),
        format!("{HEADER}dome S {{\n  function f() {{\n{body}\n  }}\n}}\n"),
        format!("{HEADER}route GET \"/x\" {{\n{body}\n}}\n"),
    ];

    contexts.iter().any(|src| parse_program(src).is_ok())
}

#[test]
fn every_extension_snippet_is_valid_jwc() {
    let path = snippets_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
    let obj = doc
        .as_object()
        .expect("snippets file must be a JSON object");
    assert!(!obj.is_empty(), "no snippets found in {}", path.display());

    let mut broken = Vec::new();
    for (name, entry) in obj {
        let body = match entry.get("body") {
            Some(serde_json::Value::Array(lines)) => lines
                .iter()
                .map(|l| l.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => panic!("snippet {name:?} has no usable body"),
        };
        let expanded = expand_placeholders(&body);
        if !parses_somewhere(&expanded) {
            broken.push(format!("  {name}:\n{expanded}"));
        }
    }

    assert!(
        broken.is_empty(),
        "{} snippet(s) the parser rejects — accepting one of these in the \
         editor breaks the user's file:\n{}",
        broken.len(),
        broken.join("\n")
    );
}

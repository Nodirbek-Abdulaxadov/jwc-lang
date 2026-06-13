//! Sync guard: `docs/docs/reference/builtins.md` must match the generator
//! output. CI runs this test; PRs that touch `src/builtins.rs::BUILTIN_DEFS`
//! without regenerating the doc fail with a clear next-step hint.
//!
//! Regenerate via:
//!   `cargo run --bin gen_builtins_doc > docs/docs/reference/builtins.md`

use std::path::PathBuf;

#[path = "../src/bin/gen_builtins_doc.rs"]
mod gen;

#[test]
fn builtins_doc_matches_generator_output() {
    let expected = gen::render_builtins_doc(jwc::builtins::BUILTIN_DEFS);

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("docs")
        .join("reference")
        .join("builtins.md");
    let actual = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));

    if actual != expected {
        let diff_preview = first_diff_line(&actual, &expected);
        panic!(
            "docs/docs/reference/builtins.md is out of sync with \
             src/builtins.rs::BUILTIN_DEFS.\n\n\
             First diff at line {}: \n  expected: {:?}\n  actual:   {:?}\n\n\
             Regenerate via:\n  \
             cargo run --bin gen_builtins_doc > docs/docs/reference/builtins.md",
            diff_preview.0, diff_preview.2, diff_preview.1,
        );
    }
}

/// Return `(line_number, actual_line, expected_line)` for the first
/// differing line so the panic message points at what to fix instead of
/// dumping the whole diff.
fn first_diff_line<'a>(actual: &'a str, expected: &'a str) -> (usize, &'a str, &'a str) {
    let a_lines: Vec<&str> = actual.lines().collect();
    let e_lines: Vec<&str> = expected.lines().collect();
    for (i, (a, e)) in a_lines.iter().zip(e_lines.iter()).enumerate() {
        if a != e {
            return (i + 1, *a, *e);
        }
    }
    // One side has more lines than the other.
    let i = a_lines.len().min(e_lines.len());
    let a = a_lines.get(i).copied().unwrap_or("<EOF>");
    let e = e_lines.get(i).copied().unwrap_or("<EOF>");
    (i + 1, a, e)
}

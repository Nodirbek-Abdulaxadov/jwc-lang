//! v0.4.0 module-level `const`: visibility, const-references-const, and the
//! compile-time validation errors (cycle, non-const expression).

use jwc::parser::{parse_program, validate_program};
use jwc::runner::run_main;

async fn run(src: &str) -> String {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    validate_program(&program).unwrap_or_else(|e| panic!("validate failed: {e}"));
    run_main(&program)
        .await
        .unwrap_or_else(|e| panic!("run failed: {e}"))
        .output
}

/// Returns the validation error string for a program expected to fail validate.
fn validate_err(src: &str) -> String {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    validate_program(&program)
        .expect_err("expected validate_program to reject this program")
        .to_string()
}

#[tokio::test]
async fn const_visible_and_references_const() {
    let out = run(r#"
        const PI = 3;
        const TAU = PI * 2;
        const LABELS = ["x", "y"];
        function main() {
            print(PI);
            print(TAU);
            print(join(LABELS, ","));
        }
    "#)
    .await;
    assert_eq!(out, "3\n6\nx,y\n");
}

#[test]
fn circular_const_is_rejected() {
    let err = validate_err("const X = X + 1; function main() { print(X); }");
    assert!(
        err.to_lowercase().contains("circular"),
        "expected a circular-reference error, got: {err}"
    );
}

#[test]
fn non_const_expression_is_rejected() {
    let err = validate_err(r#"const Y = sha256("x"); function main() { print(Y); }"#);
    assert!(
        err.to_lowercase().contains("constant expression"),
        "expected a non-const-expression error, got: {err}"
    );
}

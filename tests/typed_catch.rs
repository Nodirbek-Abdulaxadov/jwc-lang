//! Validator smoke tests for Phase 10.5 typed-catch dispatch.
//!
//! The runtime classifier + dispatch lives in `runner.rs`; here we only
//! cover the surface the parser/validator exposes to user code:
//!
//! - `catch (e: DbError)` etc. on the known-kinds list must validate.
//! - Unknown catch types must fail validation with a "did you mean"-style
//!   hint pointing at the closest known kind.

use jwc::parser::{parse_program, validate_program};

fn validate_source(src: &str) -> Result<(), String> {
    let program = parse_program(src).map_err(|e| format!("parse: {e}"))?;
    validate_program(&program).map_err(|e| format!("validate: {e:#}"))
}

const KNOWN_KINDS: &[&str] = &[
    "Error",
    "DbError",
    "HttpError",
    "ValidationError",
    "TimeoutError",
];

#[test]
fn untyped_catch_still_works() {
    let src = r#"
        function main() {
            try { print("hi"); } catch (e) { print(e); }
        }
    "#;
    validate_source(src).expect("untyped catch must validate");
}

#[test]
fn every_known_kind_validates() {
    for kind in KNOWN_KINDS {
        let src = format!(
            r#"
            function main() {{
                try {{ print("hi"); }} catch (e: {kind}) {{ print(e); }}
            }}
            "#
        );
        validate_source(&src).unwrap_or_else(|err| {
            panic!("known kind `{kind}` should validate, got error: {err}")
        });
    }
}

#[test]
fn unknown_catch_type_is_rejected() {
    let src = r#"
        function main() {
            try { print("hi"); } catch (e: BogusError) { print(e); }
        }
    "#;
    let err = validate_source(src).expect_err("unknown kind must be rejected");
    assert!(
        err.contains("unknown catch type"),
        "expected `unknown catch type` in error, got: {err}"
    );
    assert!(
        err.contains("BogusError"),
        "error should mention the bad kind name, got: {err}"
    );
    assert!(
        err.contains("DbError")
            || err.contains("HttpError")
            || err.contains("ValidationError")
            || err.contains("TimeoutError")
            || err.contains("Error"),
        "error should list known kinds, got: {err}"
    );
}

#[test]
fn close_typo_gets_suggestion() {
    // `DbErorr` is one edit away from `DbError` — closest_known_kind
    // should surface a hint.
    let src = r#"
        function main() {
            try { print("hi"); } catch (e: DbErorr) { print(e); }
        }
    "#;
    let err = validate_source(src).expect_err("typo must be rejected");
    assert!(
        err.contains("did you mean"),
        "expected a suggestion hint, got: {err}"
    );
    assert!(
        err.contains("DbError"),
        "expected `DbError` as the suggested kind, got: {err}"
    );
}

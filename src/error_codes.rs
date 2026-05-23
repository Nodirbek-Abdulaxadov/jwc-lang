//! Numbered diagnostic catalog for JWC.
//!
//! This module is the single source of truth for `Wxxx` lint warning
//! codes and `Exxx` validator / parser error codes. Each entry pairs a
//! stable code string with a one-line human description so editors, CI,
//! and `jwc lint --json` can render consistent UI without re-deriving
//! the meaning from the message text.
//!
//! **W-codes** live in `src/lint.rs` (non-fatal warnings, `lint_program`
//! return value). **E-codes** live in `src/parser.rs::validate_program`
//! and `runner.rs` (fatal `bail!` calls). Today the parser/validator
//! emit free-form messages; this catalog seeds the eventual refactor
//! that prefixes each `bail!` with its code so consumers can grep by id.
//!
//! See ROADMAP Phase 3.2 / Sprint 4 for the migration plan.

/// A single catalog entry — code + human description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticCode {
    pub code: &'static str,
    pub description: &'static str,
}

/// Convenience: build a `DiagnosticCode` literal.
const fn d(code: &'static str, description: &'static str) -> DiagnosticCode {
    DiagnosticCode { code, description }
}

/// Lint warnings — non-fatal, emitted by `jwc lint` / `lint_program`.
/// Codes increase monotonically; keep new entries in numeric order so
/// reviewers can scan the table for gaps.
pub const LINT_WARNINGS: &[DiagnosticCode] = &[
    d("W001", "function declared but never called"),
    d("W002", "middleware declared but never attached to a route"),
    d("W003", "function body is empty (returns null silently)"),
    d(
        "W004",
        "single-row select on PK is missing `first` (returns array instead of row)",
    ),
    d("W005", "user-declared function shadows a built-in name"),
    d("W006", "unreachable statement after top-level `return`"),
];

/// Validator + parser errors — fatal, raised via `bail!`. Today many of
/// these are emitted without an explicit code prefix; the catalog seeds
/// the future refactor that prefixes every `bail!` with its E-code so
/// `jwc check` output is grep-friendly.
///
/// Status: catalog only — not yet wired into `bail!` call sites.
pub const VALIDATOR_ERRORS: &[DiagnosticCode] = &[
    d(
        "E001",
        "unknown dbcontext referenced in select / insert / update / delete",
    ),
    d(
        "E002",
        "unknown entity referenced in select / insert / update / delete",
    ),
    d("E003", "entity / dbcontext mismatch on select or mutation"),
    d(
        "E004",
        "unknown column in WHERE / ORDER BY / projection / GROUP BY",
    ),
    d("E005", "duplicate route declaration (same method + path)"),
    d(
        "E006",
        "navigation property references an unknown entity / column",
    ),
    d("E007", "validate body block has no fields"),
    d(
        "E008",
        "unknown catch type — must be one of the known JwcErrorKind names",
    ),
    d("E009", "HAVING used without GROUP BY"),
    d(
        "E010",
        "register_job_handler references a function that doesn't exist",
    ),
];

/// Lookup a warning code → description. Returns `None` for unknown
/// codes; useful for editor integrations that want to render a tooltip
/// or for tests that assert the catalog is complete.
pub fn lookup_warning(code: &str) -> Option<&'static str> {
    LINT_WARNINGS
        .iter()
        .find(|d| d.code == code)
        .map(|d| d.description)
}

/// Lookup a validator error code → description. Symmetric with
/// `lookup_warning`.
pub fn lookup_error(code: &str) -> Option<&'static str> {
    VALIDATOR_ERRORS
        .iter()
        .find(|d| d.code == code)
        .map(|d| d.description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_codes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in LINT_WARNINGS {
            assert!(
                seen.insert(entry.code),
                "duplicate W-code in catalog: {}",
                entry.code
            );
        }
    }

    #[test]
    fn error_codes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in VALIDATOR_ERRORS {
            assert!(
                seen.insert(entry.code),
                "duplicate E-code in catalog: {}",
                entry.code
            );
        }
    }

    #[test]
    fn warning_codes_are_monotonic() {
        // W001, W002, W003, ... in order. Catches accidental gaps when
        // adding a new lint.
        for (i, entry) in LINT_WARNINGS.iter().enumerate() {
            let expected = format!("W{:03}", i + 1);
            assert_eq!(
                entry.code, expected,
                "expected {expected} at position {i}, got {}",
                entry.code
            );
        }
    }

    #[test]
    fn error_codes_are_monotonic() {
        for (i, entry) in VALIDATOR_ERRORS.iter().enumerate() {
            let expected = format!("E{:03}", i + 1);
            assert_eq!(
                entry.code, expected,
                "expected {expected} at position {i}, got {}",
                entry.code
            );
        }
    }

    #[test]
    fn lint_codes_emitted_by_runtime_are_in_catalog() {
        // Every W-code that `lint::lint_program` can emit must be
        // present in the catalog — drift between the two would mean
        // tooling can't render a description.
        let emitted = ["W001", "W002", "W003", "W004", "W005", "W006"];
        for code in emitted {
            assert!(
                lookup_warning(code).is_some(),
                "lint emits {code} but catalog is missing it",
            );
        }
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup_warning("W999").is_none());
        assert!(lookup_error("E999").is_none());
    }
}

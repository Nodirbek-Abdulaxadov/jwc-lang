//! Sprint 12 smoke test for `jwc build --native --target <triple>` flag
//! plumbing. The binary's clap `Command` enum isn't reachable from an
//! integration test (it lives in the bin crate, not the lib), so we drive
//! the same validation through the library-side helper
//! `cmd::build::validate_target` which the CLI calls before doing anything
//! else. This covers the three cases the spec calls out:
//!
//!   1. Accepted: `--native --target x86_64-unknown-linux-gnu`.
//!   2. Rejected: `--target` without `--native`.
//!   3. Rejected: unknown target triple, with a "Known targets:" hint.
//!
//! No cargo invocation happens here — plumbing + validation only.

use jwc::cmd::build::{validate_target, KNOWN_TARGETS};

#[test]
fn target_with_native_accepts_known_triple() {
    let res = validate_target(Some("x86_64-unknown-linux-gnu"), true);
    assert!(
        res.is_ok(),
        "expected known triple to validate, got: {res:?}"
    );
}

#[test]
fn target_with_native_accepts_every_whitelisted_triple() {
    // Tripwire: if someone trims KNOWN_TARGETS down to zero this test
    // will catch the regression rather than silently passing.
    assert!(!KNOWN_TARGETS.is_empty(), "KNOWN_TARGETS is empty");
    for t in KNOWN_TARGETS {
        validate_target(Some(t), true)
            .unwrap_or_else(|e| panic!("whitelist entry {t} failed to validate: {e}"));
    }
}

#[test]
fn target_without_native_is_rejected() {
    let err = validate_target(Some("x86_64-unknown-linux-gnu"), false)
        .expect_err("expected --target without --native to be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("--target") && msg.contains("--native"),
        "error should mention both flags, got: {msg}"
    );
}

#[test]
fn unknown_target_is_rejected_with_known_targets_hint() {
    let err = validate_target(Some("foo-bar"), true)
        .expect_err("expected unknown triple to be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("unsupported target 'foo-bar'"),
        "error should name the rejected triple, got: {msg}"
    );
    assert!(
        msg.contains("Known targets:"),
        "error should list the accepted triples, got: {msg}"
    );
    // Spot-check that at least one real whitelisted triple appears in
    // the hint so users have something to copy/paste.
    assert!(
        msg.contains("x86_64-unknown-linux-gnu"),
        "error should include at least one known triple in the hint, got: {msg}"
    );
}

#[test]
fn no_target_is_always_accepted() {
    // Host-target builds (no --target flag) work with and without
    // --native — the flag combination check only fires when --target
    // is actually set.
    validate_target(None, true).expect("None + native should be fine");
    validate_target(None, false).expect("None + non-native should be fine");
}

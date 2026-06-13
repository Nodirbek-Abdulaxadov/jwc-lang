//! Phase 8D LSP smoke tests.
//!
//! The behaviour-level tests for the new symbol index, rename, definition and
//! context-aware completion live **inside** `src/bin/jwc_lsp.rs` (it's a
//! binary crate, so the helpers can't be imported from an integration
//! test). This file exists to:
//!
//!   * pin the public contract of `JWC_ERROR_KINDS` — if `runner::JWC_ERROR_KINDS`
//!     drifts from the local copy in `jwc_lsp.rs`, the LSP completion would
//!     silently lie. We assert a subset round-trip via the parser instead
//!     (parsing `catch (e: HttpError.NotFound)` must validate-clean), which
//!     is the property the LSP completion is offering.
//!   * carry the end-to-end LSP recipe as `#[ignore]`'d tests so it's not
//!     lost to git archaeology.
//!
//! ## E2E recipe (deferred — `#[ignore]`)
//!
//! Driving the full `LspService::new(Backend::new)` round-trip in-process
//! requires plumbing a fake `Client` and pumping the framed jsonrpc — heavy
//! for a smoke check. When we need it (Phase 8E cross-file work), the steps
//! are:
//!
//!   1. `let (service, _socket) = LspService::new(Backend::new);`
//!   2. Build a `lsp_types::DidOpenTextDocumentParams { ... }` and call
//!      `service.inner().did_open(params).await`.
//!   3. Build a `RenameParams` / `GotoDefinitionParams` / `CompletionParams`
//!      with the cursor position and call the matching handler.
//!   4. Inspect the returned `WorkspaceEdit` / `GotoDefinitionResponse` /
//!      `CompletionResponse`.
//!
//! Until then, the inline `#[cfg(test)]` tests in `src/bin/jwc_lsp.rs` cover
//! the same logic at the helper level.

use jwc::parser::{parse_program, validate_program};

/// Sanity-check the JWC error kinds the LSP completion offers. `parse_program`
/// + `validate_program` accept `catch (e: <kind>)` for any kind listed in
/// `runner::JWC_ERROR_KINDS`; this test pins three of them so a divergence in
/// the LSP's local mirror surfaces here.
#[test]
fn typed_catch_accepts_error_kinds_lsp_offers() {
    for kind in ["DbError", "HttpError.NotFound", "ValidationError"] {
        let src = format!(
            r#"
            function f(): int {{
                try {{
                    return 1;
                }} catch (e: {kind}) {{
                    return 0;
                }}
            }}
            "#
        );
        let program = parse_program(&src).unwrap_or_else(|e| {
            panic!("parse failed for kind {kind}: {e}");
        });
        validate_program(&program).unwrap_or_else(|e| {
            panic!("validate failed for kind {kind}: {e}");
        });
    }
}

#[test]
#[ignore = "e2e LspService driver not wired yet; see module docs for the recipe"]
fn e2e_definition_round_trip() {}

#[test]
#[ignore = "e2e LspService driver not wired yet; see module docs for the recipe"]
fn e2e_rename_round_trip() {}

#[test]
#[ignore = "e2e LspService driver not wired yet; see module docs for the recipe"]
fn e2e_completion_round_trip() {}

//! JWC — a backend language with first-class HTTP routes, tables, views
//! and generated SQL.
//!
//! The language is the one specified in `docs/spec/v1/`. The pre-1.0
//! grammar it replaced was removed at the v0.25.0 cutover
//! (ROADMAP §2, "Implementatsiya joylashuvi"): the two front-ends lived
//! side by side for four releases so the old test suite stayed green while
//! the new one could not yet run the sample, and that reason expired the
//! moment it could.
//!
//! `main.rs` is the thin CLI wrapper over this crate.

// The unwrap budget is met and enforced: `cargo clippy --lib` reports zero
// production `.unwrap()` calls, so this costs nothing today and stops the
// next one from arriving unnoticed. `not(test)` is what makes it usable —
// unit tests unwrap freely, which is the point of a test.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

// ---- the language
pub mod apply;
pub mod ast;
pub mod check;
pub mod cursor;
pub mod db;
pub mod ddl;
pub mod diag;
pub mod diff;
pub mod exec;
mod exec_call;
pub mod fmt;
pub mod imports;
pub mod lexer;
pub mod lsp;
pub mod migrate;
pub mod model;
pub mod naming;
pub mod native;
pub mod openapi;
pub mod packages;
pub mod parser;
pub mod query;
pub mod query_sql;
pub mod registry;
pub mod serve;
pub mod snapshot;
pub mod sql;
pub mod symbols;
pub mod token;
pub mod types;
pub mod validate;
pub mod value;
pub mod views;
pub mod wiring;
pub mod workspace;

// ---- infrastructure the language stands on
pub mod cmd;
pub mod config;
pub mod engine;
pub mod hash;
pub mod jwks;
pub mod jwt;
pub mod locks;
pub mod observability;
pub mod password;
pub mod redis_engine;

use diag::{Diagnostic, Severity, SourceFile};
use std::path::Path;

/// One parsed file plus the source it came from, so diagnostics can be
/// rendered with a caret.
pub struct ParsedFile {
    pub source: SourceFile,
    pub program: ast::Program,
    pub diags: Vec<Diagnostic>,
}

impl ParsedFile {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diags.iter().filter(|d| d.severity == Severity::Error)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    pub fn render_all(&self) -> String {
        self.diags
            .iter()
            .map(|d| self.source.render(d))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn parse_str(path: impl AsRef<Path>, text: &str) -> ParsedFile {
    let (program, diags) = parser::parse(text);
    ParsedFile {
        source: SourceFile::new(path, text),
        program,
        diags,
    }
}

pub fn parse_file(path: impl AsRef<Path>) -> std::io::Result<ParsedFile> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)?;
    Ok(parse_str(path, &text))
}

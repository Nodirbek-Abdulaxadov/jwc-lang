//! The JWC v1 front-end.
//!
//! This tree implements the language specified in `docs/spec/v1/`. It is
//! deliberately separate from the pre-1.0 modules (`crate::lexer`,
//! `crate::parser`, `crate::ast`, `crate::runner`, …), which compile a
//! different language and are removed at the v0.25.0 cutover
//! (ROADMAP §2, "Implementatsiya joylashuvi").
//!
//! Nothing here accepts any construct of the old grammar; the ten removed
//! keywords produce `E0900` naming their replacement (routing.md §10).

pub mod ast;
pub mod check;
pub mod cursor;
pub mod db;
pub mod ddl;
pub mod exec;
mod exec_call;
pub mod diag;
pub mod fmt;
pub mod lexer;
pub mod model;
pub mod naming;
pub mod parser;
pub mod query;
pub mod query_sql;
pub mod serve;
pub mod sql;
pub mod symbols;
pub mod token;
pub mod types;
pub mod validate;
pub mod value;
pub mod views;
pub mod wiring;
pub mod workspace;

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
        self.diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
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

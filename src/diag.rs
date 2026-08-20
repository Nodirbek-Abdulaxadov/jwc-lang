//! v1 diagnostics: a code, a span, a message, and an optional fix-it note.
//!
//! Codes are the ones the specification names (`E0210`, `E0900`, `W0104`, …)
//! so a diagnostic can be grepped straight back to its clause.

use crate::token::Span;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    /// e.g. `E0900`.
    pub code: &'static str,
    pub message: String,
    pub span: Span,
    /// Fix-it text, rendered under the caret.
    pub note: Option<String>,
    /// The clause that defines this rule, e.g. `routing.md §10`.
    pub clause: Option<&'static str>,
}

impl Diagnostic {
    pub fn error(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            message: message.into(),
            span,
            note: None,
            clause: None,
        }
    }

    pub fn warning(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            message: message.into(),
            span,
            note: None,
            clause: None,
        }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn clause(mut self, clause: &'static str) -> Self {
        self.clause = Some(clause);
        self
    }
}

/// Renders diagnostics against the source text they came from.
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(path: impl AsRef<Path>, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            path: path.as_ref().to_path_buf(),
            text,
            line_starts,
        }
    }

    /// 1-based line and column (in characters) for a byte offset.
    ///
    /// The offset is clamped to a character boundary first. A span that
    /// lands inside a multi-byte character is a bug in whoever produced
    /// it, but slicing there panics — and a diagnostic printer that
    /// crashes on the file it is describing turns a one-line error into
    /// a compiler stack trace with the real message scrolled off the top.
    /// It is worth being total here even so.
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let mut offset = (offset as usize).min(self.text.len());
        while offset > 0 && !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line];
        let col = self.text[start..offset].chars().count() + 1;
        (line + 1, col)
    }

    fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn render(&self, d: &Diagnostic) -> String {
        let (line, col) = self.line_col(d.span.start);
        let mut out = format!(
            "{}[{}]: {}\n  --> {}:{}:{}\n",
            d.severity,
            d.code,
            d.message,
            self.path.display(),
            line,
            col
        );
        let src = self.line_text(line);
        let gutter = line.to_string().len();
        out.push_str(&format!("{:width$} |\n", "", width = gutter));
        out.push_str(&format!("{line} | {src}\n"));
        // A span can cover a whole multi-line query; the caret stops at the
        // end of the first line so the rendering stays readable.
        let width = (d.span.end.saturating_sub(d.span.start) as usize)
            .max(1)
            .min(src.chars().count().saturating_sub(col - 1).max(1));
        out.push_str(&format!(
            "{:gw$} | {:pad$}{}\n",
            "",
            "",
            "^".repeat(width),
            gw = gutter,
            pad = col - 1
        ));
        if let Some(note) = &d.note {
            // A multi-line note — E0440's expand/contract recipe, E1102's
            // five-statement enum rebuild — is indented to the same column
            // as the first line. Left flush, the continuation reads as a
            // separate diagnostic rather than as part of this one.
            let mut lines = note.lines();
            if let Some(first) = lines.next() {
                out.push_str(&format!("{:gw$} = help: {first}\n", "", gw = gutter));
                for l in lines {
                    out.push_str(&format!("{:gw$}          {l}\n", "", gw = gutter));
                }
            }
        }
        if let Some(clause) = d.clause {
            out.push_str(&format!("{:gw$} = spec: {clause}\n", "", gw = gutter));
        }
        out
    }
}

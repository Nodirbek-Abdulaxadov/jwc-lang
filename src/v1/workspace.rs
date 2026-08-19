//! A project's v1 sources, parsed together.
//!
//! `project.rs` (the 0.9.x loader) walks upward for `jwcproj.json` and reads
//! the old language. This is the same idea for v1, kept separate until the
//! v0.25.0 cutover.

use super::diag::{Diagnostic, Severity};
use super::token::Span;
use super::ParsedFile;
use std::path::{Path, PathBuf};

/// A location: which file, and where in it. Spans alone are ambiguous once
/// more than one file is in play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub file: usize,
    pub span: Span,
}

pub struct Workspace {
    pub root: PathBuf,
    pub files: Vec<ParsedFile>,
}

impl Workspace {
    /// Parse every `.jwc` file under `root` (or `root` itself when it is a
    /// file). Files are sorted so diagnostics and generated SQL come out in
    /// a stable order — `gen-sql` being byte-reproducible depends on it.
    pub fn load(root: impl AsRef<Path>) -> std::io::Result<Workspace> {
        let root = root.as_ref().to_path_buf();
        let mut paths = Vec::new();
        if root.is_file() {
            paths.push(root.clone());
        } else {
            walk(&root, &mut paths)?;
            paths.sort();
        }
        let mut files = Vec::with_capacity(paths.len());
        for p in paths {
            files.push(super::parse_file(&p)?);
        }
        Ok(Workspace { root, files })
    }

    pub fn parse_errors(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.files {
            for d in f.errors() {
                out.push(f.source.render(d));
            }
        }
        out
    }

    pub fn has_parse_errors(&self) -> bool {
        self.files.iter().any(|f| f.has_errors())
    }

    pub fn render(&self, loc: Loc, d: &Diagnostic) -> String {
        match self.files.get(loc.file) {
            Some(f) => f.source.render(d),
            None => format!("{}[{}]: {}\n", d.severity, d.code, d.message),
        }
    }

    /// `file:line` for a location — what `gen-sql --explain` prints above
    /// each statement (schema.md §9.1).
    pub fn file_line(&self, loc: Loc) -> String {
        let Some(f) = self.files.get(loc.file) else {
            return "<unknown>".into();
        };
        let (line, _) = f.source.line_col(loc.span.start);
        let rel = f
            .source
            .path
            .strip_prefix(&self.root)
            .unwrap_or(&f.source.path);
        format!("{}:{line}", rel.display())
    }

    pub fn count_errors(diags: &[(Loc, Diagnostic)]) -> usize {
        diags
            .iter()
            .filter(|(_, d)| d.severity == Severity::Error)
            .count()
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("jwc") {
            out.push(p);
        }
    }
    Ok(())
}

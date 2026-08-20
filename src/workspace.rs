//! A project's v1 sources, parsed together.
//!
//! `project.rs` (the 0.9.x loader) walks upward for `jwcproj.json` and reads
//! the old language. This is the same idea for v1, kept separate until the
//! v0.25.0 cutover.

use crate::diag::{Diagnostic, Severity};
use crate::token::Span;
use crate::ParsedFile;
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
    /// `jwcproj.json`'s `dependencies` keys. An import resolves to a
    /// namespace or to one of these (names.md §6.2.1).
    pub packages: std::collections::BTreeSet<String>,
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
            files.push(crate::parse_file(&p)?);
        }
        let packages = read_packages(&root);
        Ok(Workspace {
            root,
            files,
            packages,
        })
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

/// `jwcproj.json`'s `dependencies` keys, from the manifest at or above the
/// loaded root.
///
/// Best-effort: a project with no manifest has no package imports, which
/// makes every package import an `E0201` rather than a silent pass. A
/// manifest that does not parse is the same as none — the message the
/// reader needs is about the import, and `jwc check` on a broken manifest
/// has a louder problem than this pass.
fn read_packages(root: &Path) -> std::collections::BTreeSet<String> {
    let mut dir = if root.is_file() { root.parent() } else { Some(root) };
    while let Some(d) = dir {
        let manifest = d.join("jwcproj.json");
        if manifest.is_file() {
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                return Default::default();
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                return Default::default();
            };
            return json
                .get("dependencies")
                .and_then(|d| d.as_object())
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
        }
        dir = d.parent();
    }
    Default::default()
}

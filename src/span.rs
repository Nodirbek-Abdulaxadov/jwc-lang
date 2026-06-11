//! Byte-range source span. Pairs a file (shared `Arc<PathBuf>` so cloning a
//! token is cheap) with a half-open byte range `start..end` into that file's
//! source text. The line/col rendering is computed on demand by
//! [`crate::diag::SourceMap`]; spans themselves stay representation-free so
//! they survive across parse/validate passes without owning the source.
//!
//! Introduced in Phase 2 (production readiness plan, "Spanned diagnostics
//! everywhere"). The existing top-level `diag::Span` keeps its `file:line:col`
//! shape for backward-compat with already-emitted validator messages — this
//! byte-range span is the lower-level primitive carried by `Token` and by the
//! 3 most central AST node types so the parser can render `file:line:col`
//! with a source snippet and a caret.

use std::path::PathBuf;
use std::sync::Arc;

/// Half-open byte range `[start, end)` inside `file`. `file` is shared via
/// `Arc` so every token / AST node cloned from the same source carries the
/// same allocation. `end` is **byte** offset (UTF-8 codepoint boundary), not
/// codepoint count — the parser already deals exclusively in byte offsets
/// (`Lexer::i`, `SourceMap::line_col`).
#[derive(Clone)]
pub struct Span {
    pub file: Arc<PathBuf>,
    /// Byte offset, inclusive.
    pub start: usize,
    /// Byte offset, exclusive.
    pub end: usize,
}

impl Span {
    /// A real span pointing at a known location.
    pub fn new(file: Arc<PathBuf>, start: usize, end: usize) -> Self {
        Self { file, start, end }
    }

    /// Sentinel span for AST nodes that don't have (or don't yet have) a
    /// real source location — e.g. hand-built test fixtures, synthesized
    /// nodes from desugaring. Renders as an empty location so diagnostics
    /// silently degrade rather than printing `synthetic:0:0`.
    pub fn synthetic() -> Self {
        Self {
            file: Arc::new(PathBuf::new()),
            start: 0,
            end: 0,
        }
    }

    /// Alias for [`Span::synthetic`] — placeholder for tests / desugaring
    /// where a real span isn't available yet. Identical behavior; named
    /// `dummy` to match the production-readiness sprint plan.
    pub fn dummy() -> Self {
        Self::synthetic()
    }

    /// Returns true for a synthetic/default span — useful so renderers can
    /// skip emitting a `--> :0:0` line for AST nodes that don't have a
    /// real location.
    pub fn is_synthetic(&self) -> bool {
        self.start == 0 && self.end == 0 && self.file.as_os_str().is_empty()
    }

    /// Cover both spans (`min(start)..max(end)`). Used by the parser to
    /// stamp a span across the consumed token range (`first_tok..last_tok`).
    /// Files must match; mismatched files keep `self`'s file (parser only
    /// joins spans within one source).
    pub fn join(&self, other: &Span) -> Span {
        Span {
            file: self.file.clone(),
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl Default for Span {
    fn default() -> Self {
        Self::synthetic()
    }
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Compact: `Span(path:0..12)`; synthetic spans render as `Span(synthetic)`.
        if self.is_synthetic() {
            return write!(f, "Span(synthetic)");
        }
        write!(
            f,
            "Span({}:{}..{})",
            self.file.display(),
            self.start,
            self.end
        )
    }
}

// Two spans are equal when they cover the same byte range in the same file.
// `Arc<PathBuf>` equality compares the underlying paths (Arc impl forwards
// PartialEq), so two spans built from independently-allocated `Arc::new`s
// over the same path are still equal — what we want.
impl PartialEq for Span {
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && self.end == other.end && self.file == other.file
    }
}

impl Eq for Span {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_span_is_default() {
        let a = Span::default();
        let b = Span::synthetic();
        let c = Span::dummy();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert!(a.is_synthetic());
    }

    #[test]
    fn join_extends_range() {
        let file = Arc::new(PathBuf::from("x.jwc"));
        let a = Span::new(file.clone(), 3, 5);
        let b = Span::new(file.clone(), 8, 12);
        let j = a.join(&b);
        assert_eq!(j.start, 3);
        assert_eq!(j.end, 12);
    }

    #[test]
    fn debug_renders_compactly() {
        let file = Arc::new(PathBuf::from("x.jwc"));
        let s = Span::new(file, 1, 4);
        assert_eq!(format!("{s:?}"), "Span(x.jwc:1..4)");
        assert_eq!(format!("{:?}", Span::synthetic()), "Span(synthetic)");
    }
}

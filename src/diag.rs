/// Byte offset → (line, col) plus snippet rendering for diagnostics.
///
/// Keeps the source text alongside the line-start index so we can render
/// rustc-style snippets — the offending line with a caret pointing at the
/// offset. Cheap: source text is stored once per file; line_starts is a
/// single Vec<usize>.
#[derive(Debug, Clone)]
pub struct SourceMap {
    source: String,
    line_starts: Vec<usize>,
}

impl SourceMap {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            source: source.to_string(),
            line_starts,
        }
    }

    /// Convert a byte offset into 1-based (line, col).
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        // Find greatest line_start <= offset
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        (line_idx + 1, (offset - line_start) + 1)
    }

    /// Extract the line containing `offset` (without the trailing `\n`).
    /// Out-of-range offsets clamp to the last line.
    pub fn line_at(&self, offset: usize) -> &str {
        let (line, _) = self.line_col(offset);
        let line_idx = line - 1;
        let start = self.line_starts[line_idx];
        let end = self
            .line_starts
            .get(line_idx + 1)
            .copied()
            .unwrap_or(self.source.len());
        let raw = &self.source[start..end];
        raw.trim_end_matches('\n').trim_end_matches('\r')
    }

    /// Render a rustc-style snippet for `offset`:
    ///
    /// ```text
    ///   |
    /// 3 |     field foo;
    ///   |     ^ here
    /// ```
    ///
    /// The caret column matches the byte column reported by [`line_col`]
    /// (1-based). The leading line-number gutter is sized to the line
    /// number so multi-digit lines still align. Synthesized in pure
    /// strings — no terminal colour codes, so it survives being captured
    /// into anyhow chains, log files, or LSP diagnostics.
    pub fn snippet(&self, offset: usize) -> String {
        let (line, col) = self.line_col(offset);
        let line_text = self.line_at(offset);
        let gutter_width = line.to_string().len();
        let gutter_pad = " ".repeat(gutter_width);
        let caret_pad = " ".repeat(col.saturating_sub(1));
        format!("\n  {gutter_pad} |\n  {line} | {line_text}\n  {gutter_pad} | {caret_pad}^ here")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_handles_multi_line_input() {
        let sm = SourceMap::new("a\nbc\nxyz");
        assert_eq!(sm.line_col(0), (1, 1));
        assert_eq!(sm.line_col(2), (2, 1));
        assert_eq!(sm.line_col(3), (2, 2));
        assert_eq!(sm.line_col(5), (3, 1));
    }

    #[test]
    fn snippet_points_at_offset() {
        let sm = SourceMap::new("first\nsecond line here\nthird\n");
        let s = sm.snippet(8); // 'c' in "second"
        assert!(s.contains("second line here"), "snippet missing line: {s}");
        assert!(s.contains("^ here"), "snippet missing caret: {s}");
        assert!(s.contains("2 | "), "snippet missing line gutter: {s}");
    }
}

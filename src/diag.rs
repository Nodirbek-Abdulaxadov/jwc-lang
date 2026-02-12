#[derive(Debug, Clone)]
pub struct SourceMap {
    line_starts: Vec<usize>,
}

impl SourceMap {
    pub fn new(source: &str) -> Self {
        let mut line_starts = Vec::new();
        line_starts.push(0);

        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }

        Self { line_starts }
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
}

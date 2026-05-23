//! Source-level formatter for `.jwc` files.
//!
//! v1 is intentionally line-based: it normalises whitespace and blank-line
//! density without parsing the source, so it's safe to run on any `.jwc`
//! file (even one that doesn't parse cleanly) and produces an idempotent
//! output. The AST → source renderer planned for the full Phase 3.3 rewrite
//! will replace this with token-stream-aware logic that preserves comments
//! while reindenting bodies — until then, line normalisation already wins
//! the most common formatting complaints (trailing whitespace, mixed tabs,
//! triple blank lines, missing final newline).
//!
//! Rules applied to every `.jwc` file:
//!
//! 1. Tabs are expanded to four spaces (consistent with the rest of the
//!    codebase, which uses spaces).
//! 2. Trailing whitespace at end of each line is stripped.
//! 3. Three or more consecutive blank lines collapse to two.
//! 4. The file ends with exactly one trailing newline.
//!
//! These four together are *idempotent*: `format(format(src)) == format(src)`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Apply v1 formatting rules to `src` and return the normalised text.
pub fn format_source(src: &str) -> String {
    // Pass 1 — normalise per line: tabs → 4 spaces, strip trailing whitespace.
    let normalised: Vec<String> = src
        .split('\n')
        .map(|line| {
            let expanded = line.replace('\t', "    ");
            expanded.trim_end_matches([' ', '\r']).to_string()
        })
        .collect();

    // Pass 2 — collapse runs of 3+ empty lines down to 2. We walk the
    // sequence and track how many empties we've emitted in the current run.
    let mut out: Vec<String> = Vec::with_capacity(normalised.len());
    let mut empty_run = 0usize;
    for line in normalised {
        if line.is_empty() {
            empty_run += 1;
            if empty_run <= 2 {
                out.push(line);
            }
        } else {
            empty_run = 0;
            out.push(line);
        }
    }

    // Pass 3 — exactly one trailing newline. After Pass 2 the source might
    // end with N empties; trim them all off and reattach a single `\n`.
    while out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
    }
    let mut result = out.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    result
}

/// Return `true` when `src` is already in canonical form.
pub fn is_formatted(src: &str) -> bool {
    format_source(src) == src
}

/// Walk `root` (a file or a directory) and collect every `.jwc` file path
/// reachable from it, breadth-first. Skips the conventional build caches
/// (`.jwc-build/`, `target/`, `node_modules/`) so we don't reformat
/// generated Rust scratch or vendor JS.
pub fn collect_jwc_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::metadata(&path).with_context(|| format!("stat: {}", path.display()))?;
        if meta.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("jwc") {
                out.push(path);
            }
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&path).with_context(|| format!("read_dir: {}", path.display()))? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if matches!(name, ".jwc-build" | "target" | "node_modules" | ".git") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("jwc") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Outcome of running the formatter on a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatOutcome {
    /// Already canonical — no change needed.
    Unchanged,
    /// Was rewritten (write mode) or *would* be rewritten (check mode).
    Changed,
}

/// Format (or in check mode, diff) a single file. On `check=true` the file
/// is left untouched and the return value tells the caller whether it was
/// canonical. On `check=false` the rewritten content is written back.
pub fn format_file(path: &Path, check: bool) -> Result<FormatOutcome> {
    let src = fs::read_to_string(path).with_context(|| format!("read: {}", path.display()))?;
    let formatted = format_source(&src);
    if formatted == src {
        return Ok(FormatOutcome::Unchanged);
    }
    if !check {
        fs::write(path, &formatted).with_context(|| format!("write: {}", path.display()))?;
    }
    Ok(FormatOutcome::Changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_whitespace() {
        let src = "function foo() {  \n    return 1;   \n}\n";
        let want = "function foo() {\n    return 1;\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn expands_tabs_to_four_spaces() {
        let src = "function foo() {\n\treturn 1;\n}\n";
        let want = "function foo() {\n    return 1;\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn collapses_triple_blank_lines() {
        let src = "a\n\n\n\nb\n";
        let want = "a\n\n\nb\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn enforces_single_trailing_newline() {
        assert_eq!(format_source("a\n\n\n"), "a\n");
        assert_eq!(format_source("a"), "a\n");
        assert_eq!(format_source(""), "");
    }

    #[test]
    fn formatter_is_idempotent() {
        let inputs = [
            "function foo() {\n  return 1;\n}\n",
            "a\n\nb\n",
            "a\t\tb\n",
            "  trailing  \n",
            "\n\n\n\nonly blanks above\n",
        ];
        for input in inputs {
            let once = format_source(input);
            let twice = format_source(&once);
            assert_eq!(once, twice, "format() must be idempotent on: {input:?}");
        }
    }

    #[test]
    fn is_formatted_recognises_canonical_input() {
        let canonical = "function foo() {\n    return 1;\n}\n";
        assert!(is_formatted(canonical));
        assert!(!is_formatted("function foo() {\n\treturn 1;\n}\n"));
    }

    #[test]
    fn handles_crlf_line_endings() {
        // Windows-edited files often have CRLF; strip the `\r` along with
        // the trailing-space cleanup.
        let src = "a\r\nb\r\n";
        let want = "a\nb\n";
        assert_eq!(format_source(src), want);
    }
}

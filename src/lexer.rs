//! v1 scanner.
//!
//! Hand-written, like `crate::lexer`. Differences that matter:
//!
//! * keywords are contextual, so every word comes out as [`Tok::Ident`]
//!   (names.md §2.6);
//! * `@name` and `$name` are single tokens — `@ name` does not lex
//!   (names.md §2.5);
//! * `---` doc comments and `--` line comments are kept as [`Trivia`] on the
//!   following token, so `jwc v1 fmt` can round-trip them;
//! * `r"..."` raw strings do no escape processing except `\"`.

use crate::diag::Diagnostic;
use crate::token::{Span, Tok, Token, Trivia};

pub struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    i: usize,
    pending: Vec<Trivia>,
    pub diags: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            src: text.as_bytes(),
            text,
            i: 0,
            pending: Vec::new(),
            diags: Vec::new(),
        }
    }

    /// Tokenise the whole input. Always terminates with `Eof`; lexical errors
    /// are collected rather than aborting, so one bad string does not hide
    /// every later diagnostic.
    pub fn tokenize(mut self) -> (Vec<Token>, Vec<Diagnostic>) {
        let mut out = Vec::new();
        loop {
            self.skip_trivia();
            let start = self.i;
            if self.i >= self.src.len() {
                out.push(Token {
                    tok: Tok::Eof,
                    span: Span::new(start, start),
                    leading: std::mem::take(&mut self.pending),
                });
                break;
            }
            match self.scan_one() {
                Some(tok) => out.push(Token {
                    tok,
                    span: Span::new(start, self.i),
                    leading: std::mem::take(&mut self.pending),
                }),
                None => {
                    // scan_one already recorded a diagnostic and advanced.
                }
            }
        }
        (out, self.diags)
    }

    fn peek(&self) -> u8 {
        *self.src.get(self.i).unwrap_or(&0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        *self.src.get(self.i + n).unwrap_or(&0)
    }

    fn skip_trivia(&mut self) {
        loop {
            let mut newlines = 0usize;
            while matches!(self.peek(), b' ' | b'\t' | b'\r' | b'\n') {
                if self.peek() == b'\n' {
                    newlines += 1;
                }
                self.i += 1;
            }
            if newlines > 1 {
                self.pending.push(Trivia::Blank);
            }
            if self.peek() == b'-' && self.peek_at(1) == b'-' {
                let doc = self.peek_at(2) == b'-';
                self.i += if doc { 3 } else { 2 };
                let start = self.i;
                while self.i < self.src.len() && self.peek() != b'\n' {
                    self.i += 1;
                }
                let body = self.text[start..self.i].trim().to_string();
                self.pending.push(if doc {
                    Trivia::Doc(body)
                } else {
                    Trivia::Line(body)
                });
                continue;
            }
            break;
        }
    }

    fn scan_one(&mut self) -> Option<Tok> {
        let c = self.peek();

        // r"..." raw string, before the identifier rule claims the `r`.
        if c == b'r' && self.peek_at(1) == b'"' {
            self.i += 1;
            return self.scan_string(true);
        }

        if c.is_ascii_alphabetic() {
            let start = self.i;
            while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                self.i += 1;
            }
            return Some(Tok::Ident(self.text[start..self.i].to_string()));
        }

        if c == b'_' {
            let start = self.i;
            while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                self.i += 1;
            }
            self.diags.push(
                Diagnostic::error(
                    "E0105",
                    Span::new(start, self.i),
                    "identifiers may not start with `_`",
                )
                .note("a leading underscore is reserved for compiler temporaries in generated SQL")
                .clause("names.md §2.1"),
            );
            return None;
        }

        if c.is_ascii_digit() {
            let start = self.i;
            while self.peek().is_ascii_digit() {
                self.i += 1;
            }
            if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
                self.i += 1;
                while self.peek().is_ascii_digit() {
                    self.i += 1;
                }
                return Some(Tok::Decimal(self.text[start..self.i].to_string()));
            }
            return Some(Tok::Int(self.text[start..self.i].to_string()));
        }

        if c == b'"' {
            return self.scan_string(false);
        }

        if c == b'@' || c == b'$' {
            let sigil = c;
            let start = self.i;
            self.i += 1;
            if !(self.peek().is_ascii_alphabetic()) {
                self.diags.push(
                    Diagnostic::error(
                        "E0106",
                        Span::new(start, self.i),
                        format!(
                            "`{}` must be followed immediately by a name",
                            sigil as char
                        ),
                    )
                    .clause("names.md §2.5"),
                );
                return None;
            }
            let nstart = self.i;
            while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                self.i += 1;
            }
            let name = self.text[nstart..self.i].to_string();
            return Some(if sigil == b'@' {
                Tok::PathParam(name)
            } else {
                Tok::Local(name)
            });
        }

        // Punctuation, longest match first.
        let two = (c, self.peek_at(1));
        let three = (c, self.peek_at(1), self.peek_at(2));
        let tok = match three {
            (b'=', b'=', b'?') => {
                self.i += 3;
                Tok::EqEqOpt
            }
            (b'.', b'.', b'.') => {
                self.i += 3;
                Tok::DotDotDot
            }
            _ => match two {
                (b'=', b'=') => {
                    self.i += 2;
                    Tok::EqEq
                }
                (b'=', b'?') => {
                    self.i += 2;
                    Tok::EqOpt
                }
                (b'!', b'=') => {
                    self.i += 2;
                    Tok::BangEq
                }
                (b'<', b'=') => {
                    self.i += 2;
                    Tok::LtEq
                }
                (b'>', b'=') => {
                    self.i += 2;
                    Tok::GtEq
                }
                (b'-', b'>') => {
                    self.i += 2;
                    Tok::Arrow
                }
                (b'?', b'?') => {
                    self.i += 2;
                    Tok::Coalesce
                }
                _ => {
                    self.i += 1;
                    match c {
                        b'(' => Tok::LParen,
                        b')' => Tok::RParen,
                        b'{' => Tok::LBrace,
                        b'}' => Tok::RBrace,
                        b'[' => Tok::LBracket,
                        b']' => Tok::RBracket,
                        b',' => Tok::Comma,
                        b';' => Tok::Semi,
                        b':' => Tok::Colon,
                        b'.' => Tok::Dot,
                        b'?' => Tok::Question,
                        b'=' => Tok::Eq,
                        b'!' => Tok::Bang,
                        b'<' => Tok::Lt,
                        b'>' => Tok::Gt,
                        b'+' => Tok::Plus,
                        b'-' => Tok::Minus,
                        b'*' => Tok::Star,
                        b'/' => Tok::Slash,
                        b'%' => Tok::Percent,
                        _ => {
                            // A non-ASCII byte is the *first* byte of a
                            // character, not a character. Reporting
                            // `self.i - 1 .. self.i` would put the span
                            // inside it, and `SourceFile::line_col` then
                            // slices the source there — so the compiler
                            // panicked while rendering the diagnostic it
                            // had just produced. Consume the whole
                            // character and name it.
                            let start = self.i - 1;
                            while self.i < self.src.len()
                                && (self.src[self.i] & 0b1100_0000) == 0b1000_0000
                            {
                                self.i += 1;
                            }
                            let text = String::from_utf8_lossy(&self.src[start..self.i]);
                            self.diags.push(
                                Diagnostic::error(
                                    "E0100",
                                    Span::new(start, self.i),
                                    format!("unexpected character `{text}`"),
                                )
                                .clause("names.md §2"),
                            );
                            return None;
                        }
                    }
                }
            },
        };
        Some(tok)
    }

    fn scan_string(&mut self, raw: bool) -> Option<Tok> {
        let open = self.i;
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            if self.i >= self.src.len() {
                self.diags.push(
                    Diagnostic::error("E0102", Span::new(open, self.i), "unterminated string")
                        .clause("names.md §2.3"),
                );
                return None;
            }
            match self.peek() {
                b'"' => {
                    self.i += 1;
                    break;
                }
                b'\n' => {
                    // The help has to differ by kind. `\n` is the answer in
                    // a `"..."` and is *not* an answer in an `r"..."`,
                    // where a backslash is a backslash — telling someone to
                    // write an escape into a literal that has no escapes
                    // sends them in a circle. A raw string is scoped to
                    // regular expressions (names.md §2.4); a multi-line
                    // document has no literal form in 1.0.
                    let (message, help) = if raw {
                        (
                            "a raw string literal may not span lines",
                            "`r\"...\"` is for regular expressions and ends at the line. \
                             `\\n` inside it is a backslash and an `n`, not a newline — \
                             build multi-line text by concatenating, or keep it out of \
                             the source",
                        )
                    } else {
                        (
                            "a string literal may not contain a literal newline",
                            "write `\\n`",
                        )
                    };
                    self.diags.push(
                        Diagnostic::error("E0103", Span::new(open, self.i), message)
                            .note(help)
                            .clause(if raw { "names.md §2.4" } else { "names.md §2.3" }),
                    );
                    return None;
                }
                b'\\' if raw => {
                    // Raw strings process only \" — everything else is text,
                    // which is what makes `r"^[a-z0-9-]{3,40}$"` and
                    // `r"^[^@]+@[^@]+\.[^@]+$"` mean what they look like.
                    if self.peek_at(1) == b'"' {
                        out.push('"');
                        self.i += 2;
                    } else {
                        out.push('\\');
                        self.i += 1;
                    }
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.peek();
                    self.i += 1;
                    match e {
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'0' => out.push('\0'),
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'u' => {
                            if self.peek() != b'{' {
                                self.diags.push(
                                    Diagnostic::error(
                                        "E0108",
                                        Span::new(self.i - 2, self.i),
                                        "`\\u` must be followed by `{XXXX}`",
                                    )
                                    .clause("names.md §2.3"),
                                );
                                return None;
                            }
                            self.i += 1;
                            let hs = self.i;
                            while self.peek() != b'}' && self.i < self.src.len() {
                                self.i += 1;
                            }
                            let hex = &self.text[hs..self.i];
                            self.i += 1; // closing }
                            match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                Some(ch) => out.push(ch),
                                None => {
                                    self.diags.push(
                                        Diagnostic::error(
                                            "E0108",
                                            Span::new(hs, self.i),
                                            format!("`{hex}` is not a Unicode scalar value"),
                                        )
                                        .clause("names.md §2.3"),
                                    );
                                    return None;
                                }
                            }
                        }
                        other => {
                            self.diags.push(
                                Diagnostic::error(
                                    "E0109",
                                    Span::new(self.i - 2, self.i),
                                    format!("unknown escape `\\{}`", other as char),
                                )
                                .clause("names.md §2.3"),
                            );
                            return None;
                        }
                    }
                }
                _ => {
                    let start = self.i;
                    while self.i < self.src.len()
                        && !matches!(self.peek(), b'"' | b'\\' | b'\n')
                    {
                        self.i += 1;
                    }
                    out.push_str(&self.text[start..self.i]);
                }
            }
        }
        Some(if raw { Tok::RawStr(out) } else { Tok::Str(out) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        let (t, d) = Lexer::new(src).tokenize();
        assert!(d.is_empty(), "unexpected diagnostics: {:?}", d);
        t.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn sigils_are_single_tokens() {
        assert_eq!(
            toks("@org_id $req"),
            vec![
                Tok::PathParam("org_id".into()),
                Tok::Local("req".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn detached_sigil_is_an_error() {
        let (_, d) = Lexer::new("@ org_id").tokenize();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, "E0106");
    }

    #[test]
    fn longest_match_operators() {
        assert_eq!(
            toks("==? == =? = ?? ... . -> !="),
            vec![
                Tok::EqEqOpt,
                Tok::EqEq,
                Tok::EqOpt,
                Tok::Eq,
                Tok::Coalesce,
                Tok::DotDotDot,
                Tok::Dot,
                Tok::Arrow,
                Tok::BangEq,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn raw_strings_keep_backslashes() {
        assert_eq!(
            toks(r###"r"^[^@]+@[^@]+\.[^@]+$""###),
            vec![Tok::RawStr(r"^[^@]+@[^@]+\.[^@]+$".into()), Tok::Eof]
        );
    }

    #[test]
    fn doc_and_line_comments_become_trivia() {
        let (t, d) = Lexer::new("--- doc\n-- plain\ntable").tokenize();
        assert!(d.is_empty());
        assert_eq!(
            t[0].leading,
            vec![Trivia::Doc("doc".into()), Trivia::Line("plain".into())]
        );
        assert_eq!(t[0].tok, Tok::Ident("table".into()));
    }

    #[test]
    fn a_stray_multibyte_character_is_named_whole_and_does_not_split() {
        // The span used to be one byte wide, which put it *inside* the
        // character. `SourceFile::line_col` then sliced the source there
        // and panicked, so the compiler crashed while rendering the error
        // it had just produced — and the message printed a mojibake first
        // byte (`â` for `—`) besides.
        let (_, d) = Lexer::new("table — x").tokenize();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, "E0100");
        assert!(
            d[0].message.contains('—'),
            "the character is not named whole: {}",
            d[0].message
        );
        let span = d[0].span;
        assert_eq!(
            (span.end - span.start) as usize,
            "—".len(),
            "the span must cover the whole character"
        );
        // The renderer is the half that used to panic.
        let file = crate::diag::SourceFile::new("t.jwc", "table — x");
        assert!(file.render(&d[0]).contains("E0100"));
    }

    #[test]
    fn a_newline_in_a_raw_string_is_not_told_to_write_an_escape() {
        // `\n` is the answer for `"..."` and is *not* an answer for
        // `r"..."`, which processes no escapes — a backslash there stays a
        // backslash. Both literals used to get the same help, which sends
        // whoever hit it in a circle.
        // The trailing quote is left dangling, so an `E0102` follows each
        // of these. It is the first diagnostic that gets read.
        let (_, plain) = Lexer::new("\"a\nb\"").tokenize();
        assert_eq!(plain[0].code, "E0103", "{plain:?}");
        assert!(plain[0].note.as_deref().unwrap_or_default().contains(r"\n"));

        let (_, raw) = Lexer::new("r\"a\nb\"").tokenize();
        assert_eq!(raw[0].code, "E0103", "{raw:?}");
        let note = raw[0].note.as_deref().unwrap_or_default();
        assert!(
            note.contains("backslash"),
            "the raw-string help still points at an escape: {note}"
        );
        assert_eq!(raw[0].clause, Some("names.md §2.4"));
    }

    #[test]
    fn blank_lines_are_recorded_once() {
        let (t, _) = Lexer::new("a\n\n\n\nb").tokenize();
        assert_eq!(t[1].leading, vec![Trivia::Blank]);
    }

    #[test]
    fn keywords_are_plain_identifiers() {
        // `route` is a column name in the sample's audit table.
        assert_eq!(
            toks("route varchar"),
            vec![
                Tok::Ident("route".into()),
                Tok::Ident("varchar".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn decimal_literals() {
        assert_eq!(
            toks("10.00 42"),
            vec![Tok::Decimal("10.00".into()), Tok::Int("42".into()), Tok::Eof]
        );
    }

    #[test]
    fn leading_underscore_rejected() {
        let (_, d) = Lexer::new("_tmp").tokenize();
        assert_eq!(d[0].code, "E0105");
    }
}

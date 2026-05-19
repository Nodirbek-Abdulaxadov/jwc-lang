use anyhow::{anyhow, Result};

/// A segment inside a template string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    /// Raw source code of the `${...}` expression hole.
    Hole(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keyword {
    DbContext,
    Entity,
    Class,
    Route,
    Function,
    Dome,
    Let,
    Return,
    Print,
    If,
    Else,
    While,
    Break,
    Continue,
    And,
    Or,
    Namespace,
    Import,
    True,
    False,
    Null,
    Validate,
    Try,
    Catch,
    Middleware,
    Use,
    Async,
    Await,
    Transaction,
    ErrorHandler,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Keyword(Keyword),
    Ident(String),
    Number(String),
    String(String),
    /// Backtick template string: ``\`hello ${expr}\` ``
    TemplateStr(Vec<TemplatePart>),
    Symbol(char),
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub offset: usize,
}

pub struct Lexer<'a> {
    src: &'a str,
    i: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { src, i: 0 }
    }

    pub fn next_token(&mut self) -> Result<Token> {
        self.skip_ws_and_comments();
        let offset = self.i;

        let Some(ch) = self.peek_char() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                offset,
            });
        };

        // Raw string literal: `r"..."`. No escape processing; the closing
        // `"` ends the token. Useful for regex patterns where `\` should
        // reach the regex engine unaltered (`pattern(r"\d+")`).
        if ch == 'r' && self.src[self.i..].starts_with("r\"") {
            self.i += 1; // consume 'r'
            let value = self.consume_raw_string()?;
            return Ok(Token {
                kind: TokenKind::String(value),
                offset,
            });
        }

        if is_ident_start(ch) {
            let ident = self.consume_while(is_ident_continue);
            let kind = match ident.as_str() {
                "dbcontext" => TokenKind::Keyword(Keyword::DbContext),
                "entity" => TokenKind::Keyword(Keyword::Entity),
                "class" => TokenKind::Keyword(Keyword::Class),
                "route" => TokenKind::Keyword(Keyword::Route),
                "function" => TokenKind::Keyword(Keyword::Function),
                "dome" => TokenKind::Keyword(Keyword::Dome),
                "let" => TokenKind::Keyword(Keyword::Let),
                "return" => TokenKind::Keyword(Keyword::Return),
                "print" => TokenKind::Keyword(Keyword::Print),
                "if" => TokenKind::Keyword(Keyword::If),
                "else" => TokenKind::Keyword(Keyword::Else),
                "while" => TokenKind::Keyword(Keyword::While),
                "break" => TokenKind::Keyword(Keyword::Break),
                "continue" => TokenKind::Keyword(Keyword::Continue),
                "and" => TokenKind::Keyword(Keyword::And),
                "or" => TokenKind::Keyword(Keyword::Or),
                "namespace" => TokenKind::Keyword(Keyword::Namespace),
                "import" => TokenKind::Keyword(Keyword::Import),
                "true" => TokenKind::Keyword(Keyword::True),
                "false" => TokenKind::Keyword(Keyword::False),
                "null" => TokenKind::Keyword(Keyword::Null),
                "validate" => TokenKind::Keyword(Keyword::Validate),
                "try" => TokenKind::Keyword(Keyword::Try),
                "catch" => TokenKind::Keyword(Keyword::Catch),
                "middleware" => TokenKind::Keyword(Keyword::Middleware),
                "use" => TokenKind::Keyword(Keyword::Use),
                "async" => TokenKind::Keyword(Keyword::Async),
                "await" => TokenKind::Keyword(Keyword::Await),
                "transaction" => TokenKind::Keyword(Keyword::Transaction),
                "errorHandler" => TokenKind::Keyword(Keyword::ErrorHandler),
                _ => TokenKind::Ident(ident),
            };
            return Ok(Token { kind, offset });
        }

        if ch == '"' {
            let value = self.consume_string()?;
            return Ok(Token {
                kind: TokenKind::String(value),
                offset,
            });
        }

        if ch == '`' {
            let parts = self.consume_template_string()?;
            return Ok(Token {
                kind: TokenKind::TemplateStr(parts),
                offset,
            });
        }

        if ch.is_ascii_digit() {
            let number = self.consume_number()?;
            return Ok(Token {
                kind: TokenKind::Number(number),
                offset,
            });
        }

        self.i += ch.len_utf8();
        Ok(Token {
            kind: TokenKind::Symbol(ch),
            offset,
        })
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while let Some(ch) = self.peek_char() {
                if ch.is_whitespace() {
                    self.i += ch.len_utf8();
                } else {
                    break;
                }
            }

            if self.starts_with("//") {
                while let Some(ch) = self.peek_char() {
                    self.i += ch.len_utf8();
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }

            break;
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.src[self.i..].starts_with(s)
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.i..].chars().next()
    }

    fn consume_while<F>(&mut self, mut keep: F) -> String
    where
        F: FnMut(char) -> bool,
    {
        let start = self.i;
        while let Some(ch) = self.peek_char() {
            if keep(ch) {
                self.i += ch.len_utf8();
            } else {
                break;
            }
        }
        self.src[start..self.i].to_string()
    }

    fn consume_number(&mut self) -> Result<String> {
        let start = self.i;
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_digit() {
                self.i += 1;
            } else {
                break;
            }
        }

        // Optional fractional part: digits '.' digits
        if self.peek_char() == Some('.') {
            let mut it = self.src[self.i..].chars();
            let _dot = it.next();
            let next = it.next();
            if matches!(next, Some(c) if c.is_ascii_digit()) {
                self.i += 1; // consume '.'
                while let Some(ch) = self.peek_char() {
                    if ch.is_ascii_digit() {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
            }
        }

        let token = &self.src[start..self.i];
        if token.parse::<f64>().is_err() {
            return Err(anyhow!("Invalid numeric literal"));
        }

        Ok(token.to_string())
    }

    fn consume_template_string(&mut self) -> Result<Vec<TemplatePart>> {
        self.i += 1; // skip opening `
        let mut parts = Vec::new();
        let mut lit = String::new();

        loop {
            let Some(ch) = self.peek_char() else {
                return Err(anyhow!("Unterminated template string"));
            };

            if ch == '`' {
                self.i += 1;
                if !lit.is_empty() {
                    parts.push(TemplatePart::Literal(lit));
                }
                return Ok(parts);
            }

            // `${...}` hole
            if ch == '$' && self.src[self.i..].starts_with("${") {
                self.i += 2; // skip ${
                if !lit.is_empty() {
                    parts.push(TemplatePart::Literal(std::mem::take(&mut lit)));
                }
                let mut hole = String::new();
                let mut depth: usize = 1;
                loop {
                    let Some(hc) = self.peek_char() else {
                        return Err(anyhow!("Unterminated template expression"));
                    };
                    self.i += hc.len_utf8();
                    match hc {
                        '{' => { depth += 1; hole.push(hc); }
                        '}' => {
                            depth -= 1;
                            if depth == 0 { break; }
                            hole.push(hc);
                        }
                        _ => hole.push(hc),
                    }
                }
                parts.push(TemplatePart::Hole(hole));
                continue;
            }

            if ch == '\\' {
                self.i += 1;
                let Some(esc) = self.peek_char() else {
                    return Err(anyhow!("Unterminated template string"));
                };
                self.i += esc.len_utf8();
                match esc {
                    '`' => lit.push('`'),
                    '\\' => lit.push('\\'),
                    'n' => lit.push('\n'),
                    'r' => lit.push('\r'),
                    't' => lit.push('\t'),
                    '$' => lit.push('$'),
                    _ => { lit.push('\\'); lit.push(esc); }
                }
                continue;
            }

            self.i += ch.len_utf8();
            lit.push(ch);
        }
    }

    /// `r"..."` — no escape processing; closing `"` terminates the literal.
    fn consume_raw_string(&mut self) -> Result<String> {
        self.i += 1; // opening "
        let start = self.i;
        while let Some(ch) = self.peek_char() {
            if ch == '"' {
                let out = self.src[start..self.i].to_string();
                self.i += 1;
                return Ok(out);
            }
            self.i += ch.len_utf8();
        }
        Err(anyhow!("Unterminated raw string literal"))
    }

    fn consume_string(&mut self) -> Result<String> {
        self.i += 1;
        let mut out = String::new();

        while let Some(ch) = self.peek_char() {
            if ch == '"' {
                self.i += 1;
                return Ok(out);
            }

            if ch == '\\' {
                self.i += 1;
                let Some(esc) = self.peek_char() else {
                    return Err(anyhow!("Unterminated string literal"));
                };
                self.i += esc.len_utf8();
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    _ => return Err(anyhow!("Unsupported escape sequence")),
                }
                continue;
            }

            self.i += ch.len_utf8();
            out.push(ch);
        }

        Err(anyhow!("Unterminated string literal"))
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_core_keywords() {
        let mut lexer = Lexer::new("dbcontext App : Postgres; entity User { id uuid; }");

        assert!(matches!(
            lexer.next_token().unwrap().kind,
            TokenKind::Keyword(Keyword::DbContext)
        ));
        assert!(matches!(lexer.next_token().unwrap().kind, TokenKind::Ident(_)));
    }

    #[test]
    fn lexes_raw_string_literal_without_escape_processing() {
        let mut lexer = Lexer::new(r#"r"\d+\.\d+""#);
        let tok = lexer.next_token().unwrap();
        match tok.kind {
            TokenKind::String(s) => assert_eq!(s, "\\d+\\.\\d+"),
            other => panic!("expected raw string, got {other:?}"),
        }
    }

    #[test]
    fn r_prefix_only_lexes_as_raw_when_followed_by_quote() {
        // Plain identifier starting with 'r' must stay an identifier.
        let mut lexer = Lexer::new("ret value");
        let tok = lexer.next_token().unwrap();
        assert!(matches!(tok.kind, TokenKind::Ident(ref v) if v == "ret"));
    }

    #[test]
    fn lexes_decimal_number_literal() {
        let mut lexer = Lexer::new("let x = 0.25;");
        let _ = lexer.next_token().unwrap(); // let
        let _ = lexer.next_token().unwrap(); // x
        let _ = lexer.next_token().unwrap(); // =
        let tok = lexer.next_token().unwrap();
        match tok.kind {
            TokenKind::Number(v) => assert_eq!(v, "0.25"),
            other => panic!("expected Number token, got {other:?}"),
        }
    }
}

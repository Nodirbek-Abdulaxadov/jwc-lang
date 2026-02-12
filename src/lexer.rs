use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keyword {
    Context,
    DbContext,
    Entity,
    Select,
    From,
    Where,
    And,
    Or,
    True,
    False,
    Pk,
    Unique,
    Nullable,
    NotNull,
    Function,
    Fn,
    Print,
    Variable,
    Var,
    Let,
    If,
    Else,
    While,
    Return,
    For,
    In,
    Break,
    Continue,
    Switch,
    Case,
    Default,
    Route,
    Class,
    Controller,
    New,
    This,
    Set,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Keyword(Keyword),
    Ident(String),
    Number(i64),
    Decimal(String),
    String(String),
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
    bytes: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            i: 0,
        }
    }

    pub fn next_token(&mut self) -> Result<Token> {
        self.skip_ws_and_comments();
        let offset = self.i;

        if self.i >= self.bytes.len() {
            return Ok(Token {
                kind: TokenKind::Eof,
                offset,
            });
        }

        let ch = self.peek_char().unwrap();
        if ch == '"' {
            let s = self.lex_string_literal()?;
            return Ok(Token {
                kind: TokenKind::String(s),
                offset,
            });
        }

        if is_ident_start(ch) {
            let ident = self.consume_while(|c| is_ident_continue(c));
            let kind = match ident.as_str() {
                "context" => TokenKind::Keyword(Keyword::Context),
                "dbcontext" => TokenKind::Keyword(Keyword::DbContext),
                "entity" => TokenKind::Keyword(Keyword::Entity),
                "select" => TokenKind::Keyword(Keyword::Select),
                "from" => TokenKind::Keyword(Keyword::From),
                "where" => TokenKind::Keyword(Keyword::Where),
                "and" => TokenKind::Keyword(Keyword::And),
                "or" => TokenKind::Keyword(Keyword::Or),
                "true" => TokenKind::Keyword(Keyword::True),
                "false" => TokenKind::Keyword(Keyword::False),
                "pk" => TokenKind::Keyword(Keyword::Pk),
                "unique" => TokenKind::Keyword(Keyword::Unique),
                "nullable" => TokenKind::Keyword(Keyword::Nullable),
                "notnull" => TokenKind::Keyword(Keyword::NotNull),
                "function" => TokenKind::Keyword(Keyword::Function),
                "fn" => TokenKind::Keyword(Keyword::Fn),
                "print" => TokenKind::Keyword(Keyword::Print),
                "variable" => TokenKind::Keyword(Keyword::Variable),
                "var" => TokenKind::Keyword(Keyword::Var),
                "let" => TokenKind::Keyword(Keyword::Let),
                "if" => TokenKind::Keyword(Keyword::If),
                "else" => TokenKind::Keyword(Keyword::Else),
                "while" => TokenKind::Keyword(Keyword::While),
                "return" => TokenKind::Keyword(Keyword::Return),
                "for" => TokenKind::Keyword(Keyword::For),
                "in" => TokenKind::Keyword(Keyword::In),
                "break" => TokenKind::Keyword(Keyword::Break),
                "continue" => TokenKind::Keyword(Keyword::Continue),
                    "switch" => TokenKind::Keyword(Keyword::Switch),
                    "case" => TokenKind::Keyword(Keyword::Case),
                    "default" => TokenKind::Keyword(Keyword::Default),
                "route" => TokenKind::Keyword(Keyword::Route),
                "class" => TokenKind::Keyword(Keyword::Class),
                "controller" => TokenKind::Keyword(Keyword::Controller),
                "new" => TokenKind::Keyword(Keyword::New),
                "this" => TokenKind::Keyword(Keyword::This),
                "set" => TokenKind::Keyword(Keyword::Set),
                "insert" => TokenKind::Keyword(Keyword::Insert),
                "update" => TokenKind::Keyword(Keyword::Update),
                "delete" => TokenKind::Keyword(Keyword::Delete),
                _ => TokenKind::Ident(ident),
            };
            return Ok(Token { kind, offset });
        }

        if ch.is_ascii_digit() || (ch == '-' && self.peek_next_is_digit()) {
            return self.lex_number_or_decimal(offset);
        }

        // Single-char symbols
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

            // // line comment
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

    fn peek_next_is_digit(&self) -> bool {
        let mut iter = self.src[self.i..].chars();
        let first = iter.next();
        let second = iter.next();
        matches!((first, second), (Some('-'), Some(c)) if c.is_ascii_digit())
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

    fn lex_number_or_decimal(&mut self, offset: usize) -> Result<Token> {
        let start = self.i;

        // Optional '-'
        if self.peek_char() == Some('-') {
            self.i += 1;
        }

        // integer part
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.i += 1;
            } else {
                break;
            }
        }

        // fractional part
        if self.peek_char() == Some('.') {
            // must be followed by at least one digit to be a decimal literal
            let mut iter = self.src[self.i..].chars();
            let _dot = iter.next();
            let next = iter.next();
            if matches!(next, Some(c) if c.is_ascii_digit()) {
                self.i += 1; // '.'
                while let Some(c) = self.peek_char() {
                    if c.is_ascii_digit() {
                        self.i += 1;
                    } else {
                        break;
                    }
                }

                let s = self.src[start..self.i].to_string();
                return Ok(Token {
                    kind: TokenKind::Decimal(s),
                    offset,
                });
            }
        }

        let num_str = self.src[start..self.i].to_string();
        let value: i64 = num_str
            .parse()
            .map_err(|_| anyhow!("Invalid integer literal: {num_str}"))?;
        Ok(Token {
            kind: TokenKind::Number(value),
            offset,
        })
    }

    fn lex_string_literal(&mut self) -> Result<String> {
        // Assumes current char is '"'
        let mut out = String::new();
        self.i += 1; // skip opening quote

        while self.i < self.bytes.len() {
            let ch = self.peek_char().unwrap();
            if ch == '"' {
                self.i += 1;
                return Ok(out);
            }
            if ch == '\\' {
                self.i += 1;
                if self.i >= self.bytes.len() {
                    return Err(anyhow!("Unterminated string literal"));
                }
                let esc = self.peek_char().unwrap();
                self.i += esc.len_utf8();
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    _ => return Err(anyhow!("Unsupported escape: \\{esc}")),
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

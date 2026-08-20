//! v1 token set.
//!
//! Distinct from `crate::lexer` on purpose: the v1 grammar has a different
//! keyword set, two sigils (`@`, `$`), doc comments as real tokens, and it
//! keeps trivia so `jwc v1 fmt` can round-trip comments. The mechanism is
//! the same hand-written scanner shape; the vocabulary is not.

use std::fmt;

/// Half-open byte range into one source file.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn dummy() -> Self {
        Self { start: 0, end: 0 }
    }
}

/// Comment and blank-line trivia attached to the token that follows it.
/// `fmt` reads it; the parser moves `Doc` onto declarations (schema.md §7)
/// and carries `Line` through so a comment does not vanish on reformat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trivia {
    /// `-- text`
    Line(String),
    /// `--- text`
    Doc(String),
    /// One or more blank lines collapsed to a single marker.
    Blank,
}

/// Words the grammar gives meaning to. There are **no reserved words**:
/// every one of these is also a legal identifier, and the parser decides by
/// position (names.md §2.6). That is not laxity — `route`, `server`, `size`,
/// `max`, `check`, `key`, `text`, `date` and `int` all appear as ordinary
/// column names, rule names or builtin namespaces in the specification's own
/// sample, so a reserved-word list would forbid the language's own examples.
///
/// The list exists for `jwc v1 fmt` (which must not re-indent a word it
/// thinks is a keyword when it is a column) and for diagnostics.
pub const KEYWORDS: &[&str] = &[
    "after",
    "and",
    "as",
    "asc",
    "assert",
    "by",
    "cascade",
    "catch",
    "check",
    "class",
    "conflict",
    "cross",
    "database",
    "default",
    "delete",
    "desc",
    "do",
    "else",
    "enum",
    "error",
    "errorHandler",
    "except",
    "exists",
    "false",
    "first",
    "for",
    "foreign",
    "from",
    "full",
    "function",
    "group",
    "having",
    "identity",
    "if",
    "ilike",
    "import",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "join",
    "key",
    "left",
    "let",
    "like",
    "limit",
    "max",
    "middleware",
    "namespace",
    "no",
    "not",
    "nothing",
    "null",
    "nulls",
    "of",
    "on",
    "or",
    "orderby",
    "page",
    "primary",
    "private",
    "provides",
    "raises",
    "references",
    "requires",
    "restrict",
    "return",
    "right",
    "route",
    "routes",
    "schema",
    "select",
    "server",
    "service",
    "set",
    "size",
    "table",
    "test",
    "throw",
    "transaction",
    "transient",
    "true",
    "under",
    "unique",
    "update",
    "use",
    "using",
    "view",
    "was",
    "where",
    "with",
];

pub fn is_keyword(s: &str) -> bool {
    KEYWORDS.binary_search(&s).is_ok()
}

/// Keywords the pre-1.0 language had and 1.0 does not. Recognised only so
/// the parser can emit `E0900` with the replacement instead of a generic
/// "unexpected identifier" (routing.md §10).
pub const REMOVED_KEYWORDS: &[(&str, &str)] = &[
    (
        "entity",
        "'entity' was removed in 1.0 — write 'table Accounts of App.auth { … }'",
    ),
    (
        "dbcontext",
        "'dbcontext' was removed in 1.0 — write 'database App : Postgres' + 'schema auth of App;'",
    ),
    (
        "via",
        "'via' was removed in 1.0 — write the join's 'on' clause",
    ),
    (
        "nav",
        "'nav' was removed in 1.0 — joins are written in the query, never declared on the table",
    ),
    (
        "validate",
        "'validate body' was removed in 1.0 — write 'request.body() as ClassName'",
    ),
    (
        "new",
        "'new X from Y' was removed in 1.0 — write 'insert into App.s.X { ...y }'",
    ),
    (
        "patch",
        "'patch' was removed in 1.0 — write 'update App.s.X set …'",
    ),
    (
        "mount",
        "'mount' was removed in 1.0 — every route declares its full path",
    ),
    ("dome", "'dome' was removed in 1.0 — it has no replacement"),
    (
        "dbset",
        "'dbset' was removed in 1.0 — a table is declared with 'table T of App.s { … }'",
    ),
];

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Tok {
    Ident(String),
    /// `@name` — path parameter (names.md §5.2).
    PathParam(String),
    /// `$name` — local or parameter (names.md §5.3).
    Local(String),
    Int(String),
    Decimal(String),
    Str(String),
    /// `r"..."` — no escape processing.
    RawStr(String),

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Dot,
    DotDotDot,
    Arrow,
    Question,

    Eq,
    /// `=?` — skip-when-absent assignment (writes.md §3.3).
    EqOpt,
    EqEq,
    /// `==?` — optional predicate (queries.md §3.2).
    EqEqOpt,
    Bang,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    /// `??`
    Coalesce,

    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "`{s}`"),
            Tok::PathParam(s) => write!(f, "`@{s}`"),
            Tok::Local(s) => write!(f, "`${s}`"),
            Tok::Int(s) | Tok::Decimal(s) => write!(f, "`{s}`"),
            Tok::Str(_) => write!(f, "a string literal"),
            Tok::RawStr(_) => write!(f, "a raw string literal"),
            Tok::Eof => write!(f, "end of file"),
            other => write!(f, "`{}`", punct_text(other)),
        }
    }
}

pub fn punct_text(t: &Tok) -> &'static str {
    match t {
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::Comma => ",",
        Tok::Semi => ";",
        Tok::Colon => ":",
        Tok::Dot => ".",
        Tok::DotDotDot => "...",
        Tok::Arrow => "->",
        Tok::Question => "?",
        Tok::Eq => "=",
        Tok::EqOpt => "=?",
        Tok::EqEq => "==",
        Tok::EqEqOpt => "==?",
        Tok::Bang => "!",
        Tok::BangEq => "!=",
        Tok::Lt => "<",
        Tok::LtEq => "<=",
        Tok::Gt => ">",
        Tok::GtEq => ">=",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::Slash => "/",
        Tok::Percent => "%",
        Tok::Coalesce => "??",
        _ => "?",
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// Comments and blank lines that preceded this token.
    pub leading: Vec<Trivia>,
}

impl Token {
    pub fn is(&self, t: &Tok) -> bool {
        &self.tok == t
    }

    /// True when this token is the identifier `w`. Keywords are contextual,
    /// so "is this the keyword `table`?" is exactly "is this the identifier
    /// `table`?" asked at a position where `table` means something.
    pub fn is_word(&self, w: &str) -> bool {
        matches!(&self.tok, Tok::Ident(s) if s == w)
    }

    pub fn ident(&self) -> Option<&str> {
        match &self.tok {
            Tok::Ident(s) => Some(s),
            _ => None,
        }
    }
}

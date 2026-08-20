//! v1 recursive-descent parser.
//!
//! Recovery model: a parse error inside a declaration records a diagnostic
//! and skips to the next plausible declaration start, so one broken table
//! does not hide every later error. `parse_program` therefore always returns
//! a `Program` — the caller decides based on `diags`.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::token::{Span, Tok, Token, Trivia, REMOVED_KEYWORDS};

const SCALARS: &[&str] = &[
    "bigint",
    "int",
    "smallint",
    "numeric",
    "varchar",
    "text",
    "boolean",
    "timestamptz",
    "date",
    "time",
    "interval",
    "uuid",
    "jsonb",
    "inet",
    "bytea",
];

/// Words that can begin a top-level declaration. Used both for dispatch and
/// for error recovery.
const DECL_STARTS: &[&str] = &[
    "namespace",
    "import",
    "database",
    "schema",
    "table",
    "view",
    "enum",
    "class",
    "error",
    "service",
    "middleware",
    "routes",
    "errorHandler",
    "server",
    "function",
    "test",
];

pub struct Parser {
    toks: Vec<Token>,
    i: usize,
    /// Non-zero while parsing inside a query. `as` is a clause there, never
    /// the input cast (routing.md §5.2).
    query_depth: u32,
    pub diags: Vec<Diagnostic>,
}

type PResult<T> = Result<T, ()>;

pub fn parse(text: &str) -> (Program, Vec<Diagnostic>) {
    let (toks, mut diags) = crate::lexer::Lexer::new(text).tokenize();
    let mut p = Parser {
        toks,
        i: 0,
        query_depth: 0,
        diags: Vec::new(),
    };
    let program = p.parse_program();
    diags.append(&mut p.diags);
    (program, diags)
}

impl Parser {
    // ------------------------------------------------------------ cursor

    fn peek(&self) -> &Token {
        &self.toks[self.i.min(self.toks.len() - 1)]
    }

    fn peek_at(&self, n: usize) -> &Token {
        &self.toks[(self.i + n).min(self.toks.len() - 1)]
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().tok, Tok::Eof)
    }

    fn at(&self, t: &Tok) -> bool {
        &self.peek().tok == t
    }

    fn at_word(&self, w: &str) -> bool {
        self.peek().is_word(w)
    }

    fn word_at(&self, n: usize, w: &str) -> bool {
        self.peek_at(n).is_word(w)
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.i.min(self.toks.len() - 1)].clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.at_word(w) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn span(&self) -> Span {
        self.peek().span
    }

    fn err(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(code, span, msg));
    }

    fn err_note(
        &mut self,
        code: &'static str,
        span: Span,
        msg: impl Into<String>,
        note: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags
            .push(Diagnostic::error(code, span, msg).note(note).clause(clause));
    }

    fn expect(&mut self, t: Tok) -> PResult<Token> {
        if self.at(&t) {
            return Ok(self.bump());
        }
        // A word from the pre-1.0 vocabulary in *any* unexpected position
        // gets its own diagnostic. Without this, `select … via U` reports
        // "expected `;`", which is true and useless.
        if self.check_removed_keyword() {
            return Err(());
        }
        let found = self.peek().tok.clone();
        let span = self.span();
        self.err("E0001", span, format!("expected {t}, found {found}"));
        Err(())
    }

    fn expect_word(&mut self, w: &'static str) -> PResult<Token> {
        if self.at_word(w) {
            Ok(self.bump())
        } else {
            let found = self.peek().tok.clone();
            let span = self.span();
            self.err("E0001", span, format!("expected `{w}`, found {found}"));
            Err(())
        }
    }

    fn expect_ident(&mut self) -> PResult<Ident> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(Ident::new(name, span))
            }
            other => {
                self.err("E0001", span, format!("expected a name, found {other}"));
                Err(())
            }
        }
    }

    fn expect_string(&mut self) -> PResult<(String, Span)> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Str(s) => {
                self.bump();
                Ok((s, span))
            }
            other => {
                self.err(
                    "E0001",
                    span,
                    format!("expected a string literal, found {other}"),
                );
                Err(())
            }
        }
    }

    fn attached(&self) -> Attached {
        let mut at = Attached::default();
        for t in &self.peek().leading {
            match t {
                Trivia::Doc(s) => at.docs.push(s.clone()),
                Trivia::Line(s) => at.comments.push(s.clone()),
                Trivia::Blank => at.blank_before = true,
            }
        }
        at
    }

    // ------------------------------------------------------------ program

    fn parse_program(&mut self) -> Program {
        let mut decls = Vec::new();
        while !self.at_eof() {
            let before = self.i;
            match self.parse_decl() {
                Ok(Some(d)) => decls.push(d),
                Ok(None) => {}
                Err(()) => self.recover_to_decl(),
            }
            if self.i == before {
                // No progress: force one token so we cannot loop forever.
                self.bump();
            }
        }
        // names.md §3 — a `---` at the end of the file documents nothing.
        // `fmt` has nowhere to put it, so the text is dropped: the author
        // wrote documentation the program will never carry.
        let trailing = self.attached();
        if !trailing.docs.is_empty() {
            let span = self.peek().span;
            self.diags.push(
                Diagnostic::error("E0104", span, "this doc comment documents nothing")
                    .note(
                        "a `---` comment attaches to the declaration below it; there is \
                         none here, so the text would be dropped",
                    )
                    .clause("names.md §3"),
            );
        }
        Program { decls }
    }

    fn recover_to_decl(&mut self) {
        let mut depth = 0i32;
        while !self.at_eof() {
            match &self.peek().tok {
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth <= 0 {
                        self.bump();
                        return;
                    }
                }
                Tok::Ident(w) if depth == 0 && DECL_STARTS.contains(&w.as_str()) => return,
                _ => {}
            }
            self.bump();
        }
    }

    /// Emits `E0900` when the token is a keyword the pre-1.0 language had
    /// (routing.md §10) and returns true.
    fn check_removed_keyword(&mut self) -> bool {
        let (word, span) = match (&self.peek().tok, self.span()) {
            (Tok::Ident(w), s) => (w.clone(), s),
            _ => return false,
        };
        if let Some((_, msg)) = REMOVED_KEYWORDS.iter().find(|(k, _)| *k == word) {
            self.diags.push(
                Diagnostic::error("E0900", span, *msg)
                    .note("the pre-1.0 language has no migration path; it had no users")
                    .clause("routing.md §10"),
            );
            return true;
        }
        false
    }

    fn parse_decl(&mut self) -> PResult<Option<Decl>> {
        if self.check_removed_keyword() {
            return Err(());
        }
        let at = self.attached();
        let start = self.span();

        let word = match &self.peek().tok {
            Tok::Ident(w) => w.clone(),
            other => {
                let other = other.clone();
                self.err(
                    "E0002",
                    start,
                    format!("expected a declaration, found {other}"),
                );
                return Err(());
            }
        };

        let d = match word.as_str() {
            "namespace" => Decl::Namespace(self.parse_namespace(at, start)?),
            "import" => Decl::Import(self.parse_import(at, start)?),
            "database" => Decl::Database(self.parse_database(at, start)?),
            "schema" => Decl::Schema(self.parse_schema(at, start)?),
            "table" => Decl::Table(self.parse_table(at, start)?),
            "view" => Decl::View(self.parse_view(at, start)?),
            "enum" => Decl::Enum(self.parse_enum(at, start)?),
            "class" => Decl::Class(self.parse_class(at, start)?),
            "error" => Decl::Error(self.parse_error_decl(at, start)?),
            "service" => Decl::Service(self.parse_service(at, start)?),
            "middleware" => Decl::Middleware(self.parse_middleware(at, start)?),
            "routes" => Decl::Routes(self.parse_routes(at, start)?),
            "errorHandler" => Decl::ErrorHandler(self.parse_error_handler(at, start)?),
            "server" => Decl::Server(self.parse_server(at, start)?),
            "function" => Decl::Function(self.parse_function(at, start)?),
            "test" => Decl::Test(self.parse_test(at, start)?),
            "route" => {
                self.err_note(
                    "E0003",
                    start,
                    "`route` must be inside a `routes` block",
                    "write `routes \"/prefix\" { route GET \"suffix\" { … } }`",
                    "routing.md §1.1",
                );
                return Err(());
            }
            other => {
                self.err(
                    "E0002",
                    start,
                    format!("expected a declaration, found `{other}`"),
                );
                return Err(());
            }
        };
        Ok(Some(d))
    }

    // ------------------------------------------------------------ modules

    fn parse_namespace(&mut self, at: Attached, start: Span) -> PResult<NamespaceDecl> {
        self.bump();
        let name = self.parse_dotted()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(NamespaceDecl {
            at,
            name,
            span: start.to(end),
        })
    }

    fn parse_import(&mut self, at: Attached, start: Span) -> PResult<ImportDecl> {
        self.bump();
        let name = self.parse_dotted()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(ImportDecl {
            at,
            name,
            span: start.to(end),
        })
    }

    fn parse_dotted(&mut self) -> PResult<DottedName> {
        let first = self.expect_ident()?;
        let mut span = first.span;
        let mut parts = vec![first];
        while self.at(&Tok::Dot) {
            self.bump();
            let p = self.expect_ident()?;
            span = span.to(p.span);
            parts.push(p);
        }
        Ok(DottedName { parts, span })
    }

    // ------------------------------------------------------------ database, schema

    fn parse_database(&mut self, at: Attached, start: Span) -> PResult<DatabaseDecl> {
        self.bump();
        let name = self.expect_ident()?;
        self.expect(Tok::Colon)?;
        let driver = self.expect_ident()?;
        let mut init = Vec::new();
        let mut end = driver.span;
        if self.eat(&Tok::LBrace) {
            while !self.at(&Tok::RBrace) && !self.at_eof() {
                self.expect_word("init")?;
                self.expect(Tok::LParen)?;
                self.expect(Tok::RParen)?;
                self.expect(Tok::LBrace)?;
                while !self.at(&Tok::RBrace) && !self.at_eof() {
                    init.push(self.parse_assignment()?);
                }
                self.expect(Tok::RBrace)?;
            }
            end = self.expect(Tok::RBrace)?.span;
        } else {
            end = self.expect(Tok::Semi).map(|t| t.span).unwrap_or(end);
        }
        Ok(DatabaseDecl {
            at,
            name,
            driver,
            init,
            span: start.to(end),
        })
    }

    fn parse_assignment(&mut self) -> PResult<Assignment> {
        let key = self.expect_ident()?;
        self.expect(Tok::Eq)?;
        let value = self.parse_expr()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(Assignment {
            span: key.span.to(end),
            key,
            value,
        })
    }

    fn parse_schema(&mut self, at: Attached, start: Span) -> PResult<SchemaDecl> {
        self.bump();
        let name = self.expect_ident()?;
        self.expect_word("of")?;
        let database = self.expect_ident()?;
        let physical = self.parse_as_string()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(SchemaDecl {
            at,
            name,
            database,
            physical,
            span: start.to(end),
        })
    }

    /// `as "physical_name"` — names.md §4.2.
    fn parse_as_string(&mut self) -> PResult<Option<String>> {
        if self.at_word("as") && matches!(self.peek_at(1).tok, Tok::Str(_)) {
            self.bump();
            let (s, _) = self.expect_string()?;
            return Ok(Some(s));
        }
        Ok(None)
    }

    fn parse_was_string(&mut self) -> PResult<Option<String>> {
        if self.at_word("was") && matches!(self.peek_at(1).tok, Tok::Str(_)) {
            self.bump();
            let (s, _) = self.expect_string()?;
            return Ok(Some(s));
        }
        Ok(None)
    }

    // ------------------------------------------------------------ table

    fn parse_qualified_schema(&mut self) -> PResult<QualifiedSchema> {
        let database = self.expect_ident()?;
        self.expect(Tok::Dot)?;
        let schema = self.expect_ident()?;
        Ok(QualifiedSchema {
            span: database.span.to(schema.span),
            database,
            schema,
        })
    }

    fn parse_qualified_table(&mut self) -> PResult<QualifiedTable> {
        let database = self.expect_ident()?;
        self.expect(Tok::Dot)?;
        let schema = self.expect_ident()?;
        self.expect(Tok::Dot)?;
        let object = self.expect_ident()?;
        Ok(QualifiedTable {
            span: database.span.to(object.span),
            database,
            schema,
            object,
        })
    }

    fn parse_table(&mut self, at: Attached, start: Span) -> PResult<TableDecl> {
        self.bump();
        let name = self.expect_ident()?;
        self.expect_word("of")?;
        let schema = self.parse_qualified_schema()?;
        let physical = self.parse_as_string()?;
        let was = self.parse_was_string()?;
        self.expect(Tok::LBrace)?;

        let mut columns = Vec::new();
        let mut constraints = Vec::new();
        let mut indexes = Vec::new();

        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            if self.check_removed_keyword() {
                self.skip_to_semi_or_rbrace();
                continue;
            }
            let member_at = self.attached();
            let mspan = self.span();
            let r = if self.at_word("primary") && self.word_at(1, "key") && self.peek_at(2).is(&Tok::LParen) {
                self.parse_pk_constraint(mspan).map(|c| constraints.push(c))
            } else if self.at_word("foreign") {
                self.parse_fk_constraint(mspan).map(|c| constraints.push(c))
            } else if self.at_word("unique") && self.peek_at(1).is(&Tok::LParen) {
                self.parse_uq_constraint(mspan).map(|c| constraints.push(c))
            } else if self.at_word("check") && self.peek_at(1).is(&Tok::LParen) {
                self.parse_check_constraint(mspan).map(|c| constraints.push(c))
            } else if self.at_word("index") && self.word_at(1, "on") {
                self.parse_index(member_at.clone(), mspan)
                    .map(|ix| indexes.push(ix))
            } else {
                self.parse_column(member_at.clone(), mspan)
                    .map(|c| columns.push(c))
            };
            if r.is_err() {
                self.skip_to_semi_or_rbrace();
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(TableDecl {
            at,
            name,
            schema,
            physical,
            was,
            columns,
            constraints,
            indexes,
            span: start.to(end),
        })
    }

    fn skip_to_semi_or_rbrace(&mut self) {
        while !self.at_eof() {
            if self.at(&Tok::Semi) {
                self.bump();
                return;
            }
            if self.at(&Tok::RBrace) {
                return;
            }
            self.bump();
        }
    }

    fn parse_ident_list_parens(&mut self) -> PResult<Vec<Ident>> {
        self.expect(Tok::LParen)?;
        let mut out = vec![self.expect_ident()?];
        while self.eat(&Tok::Comma) {
            out.push(self.expect_ident()?);
        }
        self.expect(Tok::RParen)?;
        Ok(out)
    }

    fn parse_pk_constraint(&mut self, start: Span) -> PResult<TableConstraint> {
        self.bump(); // primary
        self.bump(); // key
        let columns = self.parse_ident_list_parens()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(TableConstraint::PrimaryKey {
            columns,
            span: start.to(end),
        })
    }

    fn parse_fk_constraint(&mut self, start: Span) -> PResult<TableConstraint> {
        self.bump(); // foreign
        self.expect_word("key")?;
        let columns = self.parse_ident_list_parens()?;
        self.expect_word("references")?;
        let target = self.parse_qualified_table()?;
        let target_columns = self.parse_ident_list_parens()?;
        let mut on_delete = None;
        let mut on_update = None;
        while self.at_word("on") {
            self.bump();
            let is_delete = self.at_word("delete");
            if !is_delete && !self.at_word("update") {
                let s = self.span();
                self.err("E0004", s, "expected `delete` or `update` after `on`");
                return Err(());
            }
            self.bump();
            let action = self.parse_ref_action()?;
            if is_delete {
                on_delete = Some(action);
            } else {
                on_update = Some(action);
            }
        }
        let end = self.expect(Tok::Semi)?.span;
        Ok(TableConstraint::ForeignKey {
            columns,
            target,
            target_columns,
            on_delete,
            on_update,
            span: start.to(end),
        })
    }

    fn parse_ref_action(&mut self) -> PResult<RefAction> {
        if self.eat_word("cascade") {
            return Ok(RefAction::Cascade);
        }
        if self.eat_word("restrict") {
            return Ok(RefAction::Restrict);
        }
        if self.at_word("no") {
            self.bump();
            self.expect_word("action")?;
            return Ok(RefAction::NoAction);
        }
        if self.at_word("set") {
            self.bump();
            if self.eat_word("null") {
                return Ok(RefAction::SetNull);
            }
            self.expect_word("default")?;
            return Ok(RefAction::SetDefault);
        }
        let s = self.span();
        self.err_note(
            "E0004",
            s,
            "expected a referential action",
            "one of `cascade`, `restrict`, `no action`, `set null`, `set default`",
            "schema.md §4.2",
        );
        Err(())
    }

    fn parse_uq_constraint(&mut self, start: Span) -> PResult<TableConstraint> {
        self.bump(); // unique
        let columns = self.parse_ident_list_parens()?;
        let predicate = if self.eat_word("where") {
            self.query_depth += 1;
            let e = self.parse_expr();
            self.query_depth -= 1;
            Some(e?)
        } else {
            None
        };
        let message = self.parse_message()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(TableConstraint::Unique {
            columns,
            predicate,
            message,
            span: start.to(end),
        })
    }

    fn parse_check_constraint(&mut self, start: Span) -> PResult<TableConstraint> {
        self.bump(); // check
        self.expect(Tok::LParen)?;
        self.query_depth += 1;
        let expr = self.parse_expr();
        self.query_depth -= 1;
        let expr = expr?;
        self.expect(Tok::RParen)?;
        let message = self.parse_message()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(TableConstraint::Check {
            expr,
            message,
            span: start.to(end),
        })
    }

    /// `: "message"` — the promotion marker (errors.md §6.1).
    fn parse_message(&mut self) -> PResult<Option<String>> {
        if self.at(&Tok::Colon) {
            self.bump();
            let (s, _) = self.expect_string()?;
            return Ok(Some(s));
        }
        Ok(None)
    }

    fn parse_index(&mut self, at: Attached, start: Span) -> PResult<IndexDef> {
        self.bump(); // index
        self.expect_word("on")?;
        self.expect(Tok::LParen)?;
        let mut columns = vec![self.parse_index_column()?];
        while self.eat(&Tok::Comma) {
            columns.push(self.parse_index_column()?);
        }
        self.expect(Tok::RParen)?;
        let predicate = if self.eat_word("where") {
            self.query_depth += 1;
            let e = self.parse_expr();
            self.query_depth -= 1;
            Some(e?)
        } else {
            None
        };
        let method = if self.eat_word("using") {
            Some(self.expect_ident()?)
        } else {
            None
        };
        let end = self.expect(Tok::Semi)?.span;
        Ok(IndexDef {
            at,
            columns,
            predicate,
            method,
            span: start.to(end),
        })
    }

    fn parse_index_column(&mut self) -> PResult<IndexColumn> {
        let name = self.expect_ident()?;
        let mut desc = false;
        if self.eat_word("desc") {
            desc = true;
        } else {
            self.eat_word("asc");
        }
        let nulls = self.parse_nulls_order();
        Ok(IndexColumn { name, desc, nulls })
    }

    fn parse_nulls_order(&mut self) -> Option<NullsOrder> {
        if self.at_word("nulls") {
            self.bump();
            if self.eat_word("first") {
                return Some(NullsOrder::First);
            }
            if self.eat_word("last") {
                return Some(NullsOrder::Last);
            }
        }
        None
    }

    fn parse_column(&mut self, at: Attached, start: Span) -> PResult<ColumnDef> {
        let name = self.expect_ident()?;
        let ty = self.parse_type()?;
        let mut modifiers = Vec::new();
        loop {
            self.eat(&Tok::Comma);
            if self.at(&Tok::Semi) || self.at_eof() {
                break;
            }
            let m = self.parse_column_modifier()?;
            modifiers.push(m);
        }
        let end = self.expect(Tok::Semi)?.span;
        Ok(ColumnDef {
            at,
            name,
            ty,
            modifiers,
            span: start.to(end),
        })
    }

    fn parse_column_modifier(&mut self) -> PResult<ColumnModifier> {
        let start = self.span();
        if self.at_word("primary") {
            self.bump();
            let end = self.expect_word("key")?.span;
            return Ok(ColumnModifier::PrimaryKey(start.to(end)));
        }
        if self.at_word("identity") {
            let s = self.bump().span;
            return Ok(ColumnModifier::Identity(s));
        }
        if self.at_word("private") {
            let s = self.bump().span;
            return Ok(ColumnModifier::Private(s));
        }
        if self.at_word("server") {
            let s = self.bump().span;
            return Ok(ColumnModifier::Server(s));
        }
        if self.at_word("unique") {
            self.bump();
            let message = self.parse_message()?;
            return Ok(ColumnModifier::Unique {
                message,
                span: start,
            });
        }
        if self.at_word("default") {
            self.bump();
            self.query_depth += 1;
            let e = self.parse_expr();
            self.query_depth -= 1;
            let e = e?;
            let span = start.to(e.span);
            return Ok(ColumnModifier::Default(e, span));
        }
        if self.at_word("on") && self.word_at(1, "update") {
            self.bump();
            self.bump();
            self.query_depth += 1;
            let e = self.parse_expr();
            self.query_depth -= 1;
            let e = e?;
            let span = start.to(e.span);
            return Ok(ColumnModifier::OnUpdate(e, span));
        }
        if self.at_word("as") {
            self.bump();
            let (s, sp) = self.expect_string()?;
            return Ok(ColumnModifier::Physical(s, start.to(sp)));
        }
        if self.at_word("was") {
            self.bump();
            let (s, sp) = self.expect_string()?;
            return Ok(ColumnModifier::Was(s, start.to(sp)));
        }
        Ok(ColumnModifier::Rule(self.parse_rule_call()?))
    }

    fn parse_rule_call(&mut self) -> PResult<RuleCall> {
        let name = self.expect_ident()?;
        let mut args = Vec::new();
        let mut span = name.span;
        if self.at(&Tok::LParen) {
            self.bump();
            if !self.at(&Tok::RParen) {
                args.push(self.parse_expr()?);
                while self.eat(&Tok::Comma) {
                    args.push(self.parse_expr()?);
                }
            }
            span = span.to(self.expect(Tok::RParen)?.span);
        }
        Ok(RuleCall { name, args, span })
    }

    // ------------------------------------------------------------ enum, view, class

    fn parse_enum(&mut self, at: Attached, start: Span) -> PResult<EnumDecl> {
        self.bump();
        let name = self.expect_ident()?;
        let schema = if self.eat_word("of") {
            Some(self.parse_qualified_schema()?)
        } else {
            None
        };
        let physical = self.parse_as_string()?;
        self.expect(Tok::LBrace)?;
        let mut members = vec![self.expect_ident()?];
        while self.eat(&Tok::Comma) {
            if self.at(&Tok::RBrace) {
                break;
            }
            members.push(self.expect_ident()?);
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(EnumDecl {
            at,
            name,
            schema,
            physical,
            members,
            span: start.to(end),
        })
    }

    fn parse_view(&mut self, at: Attached, start: Span) -> PResult<ViewDecl> {
        self.bump();
        let name = self.expect_ident()?;
        self.expect_word("of")?;
        let schema = self.parse_qualified_schema()?;
        let physical = self.parse_as_string()?;
        self.expect(Tok::LBrace)?;
        let body = self.parse_select()?;
        self.eat(&Tok::Semi);
        let end = self.expect(Tok::RBrace)?.span;
        Ok(ViewDecl {
            at,
            name,
            schema,
            physical,
            body: Box::new(body),
            span: start.to(end),
        })
    }

    fn parse_class(&mut self, at: Attached, start: Span) -> PResult<ClassDecl> {
        self.bump();
        let name = self.expect_ident()?;
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            let fat = self.attached();
            let fstart = self.span();
            match self.parse_class_field(fat, fstart) {
                Ok(f) => fields.push(f),
                Err(()) => self.skip_to_semi_or_rbrace(),
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(ClassDecl {
            at,
            name,
            fields,
            span: start.to(end),
        })
    }

    fn parse_class_field(&mut self, at: Attached, start: Span) -> PResult<ClassField> {
        let name = self.expect_ident()?;
        let ty = self.parse_type()?;
        let mut rules = Vec::new();
        let mut transient = false;
        loop {
            self.eat(&Tok::Comma);
            if self.at(&Tok::Semi) || self.at_eof() {
                break;
            }
            if self.at_word("transient") {
                self.bump();
                transient = true;
                continue;
            }
            rules.push(self.parse_rule_call()?);
        }
        let end = self.expect(Tok::Semi)?.span;
        Ok(ClassField {
            at,
            name,
            ty,
            rules,
            transient,
            span: start.to(end),
        })
    }

    // ------------------------------------------------------------ error, service, function

    fn parse_error_decl(&mut self, at: Attached, start: Span) -> PResult<ErrorDecl> {
        self.bump();
        let name = self.expect_ident()?;
        let mut params = Vec::new();
        if self.at(&Tok::LParen) {
            params = self.parse_params()?;
        }
        self.expect(Tok::Eq)?;
        let sspan = self.span();
        let status = match self.peek().tok.clone() {
            Tok::Int(n) => {
                self.bump();
                n.parse::<u16>().unwrap_or(0)
            }
            other => {
                self.err(
                    "E0005",
                    sspan,
                    format!("expected an HTTP status code, found {other}"),
                );
                return Err(());
            }
        };
        if !(100..=599).contains(&status) {
            self.err_note(
                "E0005",
                sspan,
                format!("`{status}` is not an HTTP status code"),
                "an error's default status must be in 100..=599",
                "errors.md §1.1",
            );
        }
        let message = self.parse_message()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(ErrorDecl {
            at,
            name,
            params,
            status,
            message,
            span: start.to(end),
        })
    }

    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.expect(Tok::LParen)?;
        let mut out = Vec::new();
        if !self.at(&Tok::RParen) {
            loop {
                let name = self.expect_ident()?;
                let start = name.span;
                self.expect(Tok::Colon)?;
                let ty = self.parse_type()?;
                let default = if self.eat(&Tok::Eq) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                let span = start.to(ty.span);
                out.push(Param {
                    name,
                    ty,
                    default,
                    span,
                });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        self.expect(Tok::RParen)?;
        Ok(out)
    }

    fn parse_service(&mut self, at: Attached, start: Span) -> PResult<ServiceDecl> {
        self.bump();
        let name = self.expect_ident()?;
        self.expect(Tok::LBrace)?;
        let mut functions = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            let fat = self.attached();
            let fstart = self.span();
            if !self.at_word("function") {
                let found = self.peek().tok.clone();
                self.err_note(
                    "E0006",
                    fstart,
                    format!("expected `function`, found {found}"),
                    "a service body contains only functions",
                    "types.md §10",
                );
                self.recover_to_decl();
                break;
            }
            if let Ok(f) = self.parse_function(fat, fstart) {
                functions.push(f);
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(ServiceDecl {
            at,
            name,
            functions,
            span: start.to(end),
        })
    }

    fn parse_function(&mut self, at: Attached, start: Span) -> PResult<FunctionDecl> {
        self.bump(); // function
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let returns = if self.eat(&Tok::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let mut raises = Vec::new();
        if self.eat_word("raises") {
            self.expect(Tok::LParen)?;
            raises.push(self.expect_ident()?);
            while self.eat(&Tok::Comma) {
                raises.push(self.expect_ident()?);
            }
            self.expect(Tok::RParen)?;
        }
        let (body, end) = self.parse_block()?;
        Ok(FunctionDecl {
            at,
            name,
            params,
            returns,
            raises,
            body,
            span: start.to(end),
        })
    }

    // ------------------------------------------------------------ middleware

    fn parse_middleware(&mut self, at: Attached, start: Span) -> PResult<MiddlewareDecl> {
        self.bump();
        let name = self.expect_ident()?;

        let mut binders = Vec::new();
        if self.at(&Tok::LParen) {
            self.bump();
            if !self.at(&Tok::RParen) {
                loop {
                    let bspan = self.span();
                    let bname = match self.peek().tok.clone() {
                        Tok::PathParam(n) => {
                            self.bump();
                            Ident::new(n, bspan)
                        }
                        other => {
                            self.err_note(
                                "E0007",
                                bspan,
                                format!("expected `@name`, found {other}"),
                                "a middleware binder declares a path parameter: `middleware M(@org_id: bigint)`",
                                "middleware.md §2",
                            );
                            return Err(());
                        }
                    };
                    self.expect(Tok::Colon)?;
                    let ty = self.parse_type()?;
                    let span = bspan.to(ty.span);
                    binders.push(Binder {
                        name: bname,
                        ty,
                        span,
                    });
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
            }
            self.expect(Tok::RParen)?;
        }

        let mut requires = Vec::new();
        if self.eat_word("requires") {
            requires.push(self.expect_ident()?);
            while self.eat(&Tok::Comma) {
                requires.push(self.expect_ident()?);
            }
        }

        let mut provides = Vec::new();
        if self.eat_word("provides") {
            loop {
                let pname = self.expect_ident()?;
                self.expect(Tok::Colon)?;
                let ty = self.parse_type()?;
                let span = pname.span.to(ty.span);
                provides.push(CtxDecl {
                    name: pname,
                    ty,
                    span,
                });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }

        self.expect(Tok::LBrace)?;
        let mut body = Vec::new();
        let mut after = None;
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            if self.at_word("after") && self.peek_at(1).is(&Tok::LBrace) {
                self.bump();
                let (blk, _) = self.parse_block()?;
                after = Some(blk);
                continue;
            }
            let before = self.i;
            match self.parse_stmt() {
                Ok(s) => body.push(s),
                Err(()) => self.skip_to_semi_or_rbrace(),
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(MiddlewareDecl {
            at,
            name,
            binders,
            requires,
            provides,
            body,
            after,
            span: start.to(end),
        })
    }

    // ------------------------------------------------------------ routing

    fn parse_use_clause(&mut self) -> PResult<Vec<Ident>> {
        let mut out = Vec::new();
        if self.eat_word("use") {
            out.push(self.expect_ident()?);
            while self.eat(&Tok::Comma) {
                out.push(self.expect_ident()?);
            }
        }
        Ok(out)
    }

    fn parse_routes(&mut self, at: Attached, start: Span) -> PResult<RoutesDecl> {
        self.bump();
        let (prefix, prefix_span) = self.expect_string()?;
        let uses = self.parse_use_clause()?;
        self.expect(Tok::LBrace)?;
        let mut routes = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            let rat = self.attached();
            let rstart = self.span();
            if !self.at_word("route") {
                let found = self.peek().tok.clone();
                self.err_note(
                    "E0008",
                    rstart,
                    format!("expected `route`, found {found}"),
                    "a `routes` block contains only `route` declarations; blocks do not nest",
                    "routing.md §1.1",
                );
                self.recover_to_decl();
                break;
            }
            if let Ok(r) = self.parse_route(rat, rstart) {
                routes.push(r);
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(RoutesDecl {
            at,
            prefix,
            prefix_span,
            uses,
            routes,
            span: start.to(end),
        })
    }

    fn parse_route(&mut self, at: Attached, start: Span) -> PResult<RouteDecl> {
        self.bump(); // route
        let method = self.expect_ident()?;
        const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
        if !METHODS.contains(&method.name.as_str()) {
            self.err_note(
                "E0009",
                method.span,
                format!("`{}` is not an HTTP method", method.name),
                "one of GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
                "routing.md §2",
            );
        }
        let (suffix, suffix_span) = self.expect_string()?;
        let uses = self.parse_use_clause()?;
        let (body, end) = self.parse_block()?;
        Ok(RouteDecl {
            at,
            method,
            suffix,
            suffix_span,
            uses,
            body,
            span: start.to(end),
        })
    }

    fn parse_error_handler(&mut self, at: Attached, start: Span) -> PResult<ErrorHandlerDecl> {
        self.bump();
        self.expect(Tok::LParen)?;
        let binder = self.expect_ident()?;
        self.expect(Tok::RParen)?;
        self.expect(Tok::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            let astart = self.span();
            if !self.at_word("catch") {
                let found = self.peek().tok.clone();
                self.err_note(
                    "E0010",
                    astart,
                    format!("expected `catch`, found {found}"),
                    "an errorHandler body contains only `catch` arms",
                    "errors.md §4.1",
                );
                break;
            }
            self.bump();
            // `catch NotFound (err)` vs the untyped `catch (err)`.
            let error = if self.at(&Tok::LParen) {
                None
            } else {
                Some(self.expect_ident()?)
            };
            self.expect(Tok::LParen)?;
            let abinder = self.expect_ident()?;
            self.expect(Tok::RParen)?;
            let (body, end) = self.parse_block()?;
            arms.push(CatchArm {
                error,
                binder: abinder,
                body,
                span: astart.to(end),
            });
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(ErrorHandlerDecl {
            at,
            binder,
            arms,
            span: start.to(end),
        })
    }

    fn parse_server(&mut self, at: Attached, start: Span) -> PResult<ServerDecl> {
        self.bump();
        self.expect(Tok::LBrace)?;
        let mut entries = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            let gstart = self.span();
            if matches!(self.peek().tok, Tok::Ident(_)) && self.peek_at(1).is(&Tok::LBrace) {
                let name = self.expect_ident()?;
                self.expect(Tok::LBrace)?;
                let mut inner = Vec::new();
                while !self.at(&Tok::RBrace) && !self.at_eof() {
                    inner.push(self.parse_assignment()?);
                }
                let end = self.expect(Tok::RBrace)?.span;
                entries.push(ServerEntry::Group {
                    name,
                    entries: inner,
                    span: gstart.to(end),
                });
            } else {
                match self.parse_assignment() {
                    Ok(a) => entries.push(ServerEntry::Set(a)),
                    Err(()) => self.skip_to_semi_or_rbrace(),
                }
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(ServerDecl {
            at,
            entries,
            span: start.to(end),
        })
    }

    fn parse_test(&mut self, at: Attached, start: Span) -> PResult<TestDecl> {
        self.bump();
        let (name, _) = self.expect_string()?;
        let (body, end) = self.parse_block()?;
        Ok(TestDecl {
            at,
            name,
            body,
            span: start.to(end),
        })
    }

    // ------------------------------------------------------------ types

    fn parse_type(&mut self) -> PResult<TypeRef> {
        let start = self.span();

        let kind = if self.at(&Tok::LBrace) {
            self.bump();
            let mut fields = Vec::new();
            if !self.at(&Tok::RBrace) {
                loop {
                    let name = self.expect_ident()?;
                    self.expect(Tok::Colon)?;
                    let ty = self.parse_type()?;
                    fields.push((name, ty));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
            }
            self.expect(Tok::RBrace)?;
            TypeKind::Record(fields)
        } else {
            let first = self.expect_ident()?;
            if SCALARS.contains(&first.name.as_str()) {
                let mut args = Vec::new();
                if self.at(&Tok::LParen) {
                    self.bump();
                    loop {
                        let s = self.span();
                        match self.peek().tok.clone() {
                            Tok::Int(n) => {
                                self.bump();
                                args.push(n.parse::<u32>().unwrap_or(0));
                            }
                            other => {
                                self.err(
                                    "E0011",
                                    s,
                                    format!("expected a type argument, found {other}"),
                                );
                                return Err(());
                            }
                        }
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RParen)?;
                }
                TypeKind::Scalar {
                    name: first.name.clone(),
                    args,
                }
            } else {
                let mut span = first.span;
                let mut parts = vec![first];
                while self.at(&Tok::Dot) {
                    self.bump();
                    let p = self.expect_ident()?;
                    span = span.to(p.span);
                    parts.push(p);
                }
                TypeKind::Named(DottedName { parts, span })
            }
        };

        let optional = self.eat(&Tok::Question);
        let mut array_depth = 0u8;
        let mut array_optional = Vec::new();
        let mut end = self.toks[self.i.saturating_sub(1)].span;
        while self.at(&Tok::LBracket) && self.peek_at(1).is(&Tok::RBracket) {
            self.bump();
            end = self.bump().span;
            array_depth += 1;
            let opt = self.at(&Tok::Question);
            if opt {
                end = self.bump().span;
            }
            array_optional.push(opt);
        }

        Ok(TypeRef {
            kind,
            array_depth,
            optional,
            array_optional,
            span: start.to(end),
        })
    }

    // ------------------------------------------------------------ statements

    fn parse_block(&mut self) -> PResult<(Block, Span)> {
        self.expect(Tok::LBrace)?;
        let mut out = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let before = self.i;
            match self.parse_stmt() {
                Ok(s) => out.push(s),
                Err(()) => self.skip_to_semi_or_rbrace(),
            }
            if self.i == before {
                self.bump();
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok((out, end))
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        if self.check_removed_keyword() {
            return Err(());
        }
        let at = self.attached();
        let start = self.span();

        if self.at_word("let") {
            self.bump();
            let name = self.expect_ident()?;
            let ty = if self.eat(&Tok::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect(Tok::Eq)?;
            let value = self.parse_expr()?;
            let end = self.expect(Tok::Semi)?.span;
            return Ok(Stmt::Let {
                at,
                name,
                ty,
                value,
                span: start.to(end),
            });
        }

        if self.at_word("if") {
            self.bump();
            self.expect(Tok::LParen)?;
            let cond = self.parse_expr()?;
            self.expect(Tok::RParen)?;
            let (then, mut end) = self.parse_block()?;
            let mut otherwise = None;
            if self.eat_word("else") {
                if self.at_word("if") {
                    let inner = self.parse_stmt()?;
                    end = match &inner {
                        Stmt::If { span, .. } => *span,
                        _ => end,
                    };
                    otherwise = Some(vec![inner]);
                } else {
                    let (blk, e) = self.parse_block()?;
                    end = e;
                    otherwise = Some(blk);
                }
            }
            return Ok(Stmt::If {
                at,
                cond,
                then,
                otherwise,
                span: start.to(end),
            });
        }

        if self.at_word("for") {
            self.bump();
            self.expect(Tok::LParen)?;
            let binder = self.expect_ident()?;
            self.expect_word("in")?;
            let iterable = self.parse_expr()?;
            self.expect(Tok::RParen)?;
            let (body, end) = self.parse_block()?;
            return Ok(Stmt::For {
                at,
                binder,
                iterable,
                body,
                span: start.to(end),
            });
        }

        if self.at_word("return") {
            self.bump();
            let value = if self.at(&Tok::Semi) {
                None
            } else {
                Some(self.parse_expr()?)
            };
            let end = self.expect(Tok::Semi)?.span;
            return Ok(Stmt::Return {
                at,
                value,
                span: start.to(end),
            });
        }

        if self.at_word("throw") {
            self.bump();
            let error = self.expect_ident()?;
            let mut args = Vec::new();
            if self.at(&Tok::LParen) {
                self.bump();
                if !self.at(&Tok::RParen) {
                    args.push(self.parse_expr()?);
                    while self.eat(&Tok::Comma) {
                        args.push(self.parse_expr()?);
                    }
                }
                self.expect(Tok::RParen)?;
            }
            let end = self.expect(Tok::Semi)?.span;
            return Ok(Stmt::Throw {
                at,
                error,
                args,
                span: start.to(end),
            });
        }

        if self.at_word("transaction") && self.peek_at(1).is(&Tok::LBrace) {
            self.bump();
            let (body, end) = self.parse_block()?;
            return Ok(Stmt::Transaction {
                at,
                body,
                span: start.to(end),
            });
        }

        if self.at_word("assert") {
            self.bump();
            if self.at_word("fails") {
                self.bump();
                let error = if self.at(&Tok::LBrace) {
                    None
                } else {
                    Some(self.expect_ident()?)
                };
                let (body, mut end) = self.parse_block()?;
                if self.at(&Tok::Semi) {
                    end = self.bump().span;
                }
                return Ok(Stmt::Assert {
                    at,
                    kind: AssertKind::Fails { error, body },
                    span: start.to(end),
                });
            }
            let e = self.parse_expr()?;
            let end = self.expect(Tok::Semi)?.span;
            return Ok(Stmt::Assert {
                at,
                kind: AssertKind::Expr(e),
                span: start.to(end),
            });
        }

        // `$x = …;` and `context.k = …;`
        if let Tok::Local(name) = self.peek().tok.clone() {
            if self.peek_at(1).is(&Tok::Eq) {
                let nspan = self.bump().span;
                self.bump();
                let value = self.parse_expr()?;
                let end = self.expect(Tok::Semi)?.span;
                return Ok(Stmt::Assign {
                    at,
                    target: AssignTarget::Local(Ident::new(name, nspan)),
                    value,
                    span: start.to(end),
                });
            }
        }
        if self.at_word("context") && self.peek_at(1).is(&Tok::Dot) && self.peek_at(3).is(&Tok::Eq)
        {
            self.bump();
            self.bump();
            let key = self.expect_ident()?;
            self.expect(Tok::Eq)?;
            let value = self.parse_expr()?;
            let end = self.expect(Tok::Semi)?.span;
            return Ok(Stmt::Assign {
                at,
                target: AssignTarget::Context(key),
                value,
                span: start.to(end),
            });
        }

        let expr = self.parse_expr()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(Stmt::Expr {
            at,
            span: start.to(end),
            expr,
        })
    }

    // ------------------------------------------------------------ expressions

    /// The error type is `()` on purpose: every failure has already been
    /// pushed onto `self.diags` with its code, span and fix-it, so the
    /// return value only says "stop parsing this construct".
    #[allow(clippy::result_unit_err)]
    pub fn parse_expr(&mut self) -> PResult<Expr> {
        let mut e = self.parse_coalesce()?;

        // `or throw E(...)` — errors.md §5.
        if self.at_word("or") && self.word_at(1, "throw") {
            self.bump();
            self.bump();
            let error = self.expect_ident()?;
            let mut args = Vec::new();
            if self.at(&Tok::LParen) {
                self.bump();
                if !self.at(&Tok::RParen) {
                    args.push(self.parse_expr()?);
                    while self.eat(&Tok::Comma) {
                        args.push(self.parse_expr()?);
                    }
                }
                self.expect(Tok::RParen)?;
            }
            let span = e.span;
            e = Expr::new(ExprKind::OrThrow { value: e, error, args }, span);
        }

        // `<expr> catch E (err) { … }` — errors.md §7.
        if self.at_word("catch") && matches!(self.peek_at(1).tok, Tok::Ident(_)) {
            self.bump();
            let error = self.expect_ident()?;
            self.expect(Tok::LParen)?;
            let binder = self.expect_ident()?;
            self.expect(Tok::RParen)?;
            let (body, end) = self.parse_block()?;
            let span = e.span.to(end);
            e = Expr::new(
                ExprKind::CatchPostfix {
                    value: e,
                    error,
                    binder,
                    body,
                },
                span,
            );
        }

        // `<response> with { … } cookie(…) cookie(…)` — routing.md §6.2.
        if self.query_depth == 0 && self.at_word("with") && self.peek_at(1).is(&Tok::LBrace) {
            self.bump();
            let (headers, end) = self.parse_object_entries()?;
            let span = e.span.to(end);
            e = Expr::new(ExprKind::WithHeaders { value: e, headers }, span);
        }
        while self.query_depth == 0
            && self.at_word("cookie")
            && self.peek_at(1).is(&Tok::LParen)
        {
            self.bump();
            self.expect(Tok::LParen)?;
            let mut args = Vec::new();
            if !self.at(&Tok::RParen) {
                args.push(self.parse_expr()?);
                while self.eat(&Tok::Comma) {
                    args.push(self.parse_expr()?);
                }
            }
            let end = self.expect(Tok::RParen)?.span;
            let span = e.span.to(end);
            e = Expr::new(ExprKind::Cookie { value: e, args }, span);
        }

        Ok(e)
    }

    fn parse_coalesce(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_ternary()?;
        while self.at(&Tok::Coalesce) {
            self.bump();
            let rhs = self.parse_ternary()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(ExprKind::Coalesce { lhs, rhs }, span);
        }
        Ok(lhs)
    }

    fn parse_ternary(&mut self) -> PResult<Expr> {
        let cond = self.parse_or()?;
        if self.at(&Tok::Question) {
            self.bump();
            let then = self.parse_ternary()?;
            self.expect(Tok::Colon)?;
            let otherwise = self.parse_ternary()?;
            let span = cond.span.to(otherwise.span);
            return Ok(Expr::new(
                ExprKind::Ternary {
                    cond,
                    then,
                    otherwise,
                },
                span,
            ));
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_and()?;
        // `or throw` belongs to parse_expr, so stop before it.
        while self.at_word("or") && !self.word_at(1, "throw") {
            self.bump();
            let rhs = self.parse_and()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op: BinOp::Or,
                    lhs,
                    rhs,
                },
                span,
            );
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_not()?;
        while self.at_word("and") {
            self.bump();
            let rhs = self.parse_not()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op: BinOp::And,
                    lhs,
                    rhs,
                },
                span,
            );
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> PResult<Expr> {
        let start = self.span();
        if self.at(&Tok::Bang) {
            self.bump();
            let rhs = self.parse_not()?;
            let span = start.to(rhs.span);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op: UnaryOp::Not,
                    rhs,
                },
                span,
            ));
        }
        // `not exists (…)` / `not in (…)` are handled in parse_compare.
        if self.at_word("not") && self.word_at(1, "exists") {
            self.bump();
            self.bump();
            self.expect(Tok::LParen)?;
            let q = self.parse_expr()?;
            let end = self.expect(Tok::RParen)?.span;
            return Ok(Expr::new(
                ExprKind::Exists {
                    query: q,
                    negated: true,
                },
                start.to(end),
            ));
        }
        if self.at_word("exists") && self.peek_at(1).is(&Tok::LParen) {
            self.bump();
            self.expect(Tok::LParen)?;
            let q = self.parse_expr()?;
            let end = self.expect(Tok::RParen)?.span;
            return Ok(Expr::new(
                ExprKind::Exists {
                    query: q,
                    negated: false,
                },
                start.to(end),
            ));
        }
        self.parse_compare()
    }

    fn parse_compare(&mut self) -> PResult<Expr> {
        let lhs = self.parse_additive()?;

        if self.at_word("in") || (self.at_word("not") && self.word_at(1, "in")) {
            let negated = self.at_word("not");
            if negated {
                self.bump();
            }
            self.bump(); // in
            self.expect(Tok::LParen)?;
            let mut items = Vec::new();
            if !self.at(&Tok::RParen) {
                items.push(self.parse_expr()?);
                while self.eat(&Tok::Comma) {
                    items.push(self.parse_expr()?);
                }
            }
            let end = self.expect(Tok::RParen)?.span;
            let span = lhs.span.to(end);
            return Ok(Expr::new(
                ExprKind::In {
                    lhs,
                    items,
                    negated,
                },
                span,
            ));
        }

        let op = match &self.peek().tok {
            Tok::EqEq => Some(BinOp::Eq),
            Tok::BangEq => Some(BinOp::Ne),
            Tok::EqEqOpt => Some(BinOp::EqOpt),
            Tok::Lt => Some(BinOp::Lt),
            Tok::LtEq => Some(BinOp::Le),
            Tok::Gt => Some(BinOp::Gt),
            Tok::GtEq => Some(BinOp::Ge),
            Tok::Ident(w) if w == "like" => Some(BinOp::Like),
            Tok::Ident(w) if w == "ilike" => Some(BinOp::ILike),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let rhs = self.parse_additive()?;
            let span = lhs.span.to(rhs.span);
            return Ok(Expr::new(ExprKind::Binary { op, lhs, rhs }, span));
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match &self.peek().tok {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_multiplicative()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(ExprKind::Binary { op, lhs, rhs }, span);
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match &self.peek().tok {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary()?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr::new(ExprKind::Binary { op, lhs, rhs }, span);
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let start = self.span();
        if self.at(&Tok::Minus) {
            self.bump();
            let rhs = self.parse_unary()?;
            let span = start.to(rhs.span);
            return Ok(Expr::new(
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    rhs,
                },
                span,
            ));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at(&Tok::Dot) {
                self.bump();
                let field = self.expect_ident()?;
                // `context.k?` — the nullable read (middleware.md §6.3).
                let mut span = e.span.to(field.span);
                let mut name = field;
                if self.at(&Tok::Question) && !self.peek_at(1).is(&Tok::Colon) {
                    let q = self.bump().span;
                    span = span.to(q);
                    name = Ident::new(format!("{}?", name.name), name.span);
                }
                e = Expr::new(ExprKind::Field { base: e, field: name }, span);
                continue;
            }
            if self.at(&Tok::LParen) {
                self.bump();
                let mut args = Vec::new();
                let mut filter = None;
                if !self.at(&Tok::RParen) {
                    args.push(self.parse_expr()?);
                    while self.eat(&Tok::Comma) {
                        args.push(self.parse_expr()?);
                    }
                    // `count(x where pred)` — queries.md §6.3.
                    if self.eat_word("where") {
                        filter = Some(self.parse_expr()?);
                    }
                }
                let end = self.expect(Tok::RParen)?.span;
                let span = e.span.to(end);
                e = Expr::new(
                    ExprKind::Call {
                        callee: e,
                        args,
                        filter,
                    },
                    span,
                );
                continue;
            }
            if self.at(&Tok::LBracket) {
                self.bump();
                let index = self.parse_expr()?;
                let end = self.expect(Tok::RBracket)?.span;
                let span = e.span.to(end);
                e = Expr::new(ExprKind::Index { base: e, index }, span);
                continue;
            }
            // `request.body() as Register` — only outside a query, where `as`
            // is a clause (routing.md §5.2).
            if self.query_depth == 0
                && self.at_word("as")
                && matches!(self.peek_at(1).tok, Tok::Ident(_))
            {
                self.bump();
                let ty = self.expect_ident()?;
                let span = e.span.to(ty.span);
                e = Expr::new(ExprKind::Cast { value: e, ty }, span);
                continue;
            }
            break;
        }
        Ok(e)
    }

    fn parse_object_entries(&mut self) -> PResult<(Vec<ObjEntry>, Span)> {
        self.expect(Tok::LBrace)?;
        let mut out = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let start = self.span();
            if self.at(&Tok::DotDotDot) {
                self.bump();
                let sspan = self.span();
                let source = match self.peek().tok.clone() {
                    Tok::Local(n) => {
                        self.bump();
                        Ident::new(n, sspan)
                    }
                    other => {
                        self.err_note(
                            "E0012",
                            sspan,
                            format!("expected `$name` after `...`, found {other}"),
                            "a spread source is a local with a declared shape",
                            "types.md §9.1",
                        );
                        return Err(());
                    }
                };
                let except = self.parse_except_list()?;
                let span = start.to(
                    except
                        .last()
                        .map(|i| i.span)
                        .unwrap_or(source.span),
                );
                out.push(ObjEntry::Spread {
                    source,
                    except,
                    span,
                });
            } else {
                // A JSON key may be any word, including one with grammatical
                // meaning elsewhere: `{ error: … }` is an object, not a
                // declaration (names.md §2.6).
                let key = self.expect_ident_or_string_key()?;
                let assign = if self.at(&Tok::Colon) {
                    self.bump();
                    false
                } else if self.at(&Tok::Eq) {
                    self.bump();
                    true
                } else {
                    let s = self.span();
                    self.err("E0013", s, "expected `:` or `=` after an object key");
                    return Err(());
                };
                let value = self.parse_expr()?;
                let span = start.to(value.span);
                out.push(ObjEntry::Field {
                    key,
                    value,
                    assign,
                    span,
                });
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok((out, end))
    }

    /// `except (a, b)`. Parenthesised because a spread lives inside a
    /// comma-separated object literal, where `except a, b` cannot be told
    /// from `except a` followed by the next entry (types.md §9.1).
    fn parse_except_list(&mut self) -> PResult<Vec<Ident>> {
        if !self.at_word("except") {
            return Ok(Vec::new());
        }
        self.bump();
        if !self.at(&Tok::LParen) {
            let s = self.span();
            self.err_note(
                "E0018",
                s,
                "`except` takes a parenthesised list",
                "write `...$req except (password)` — the parentheses are what \
                 separate the excluded names from the next object entry",
                "types.md §9.1",
            );
            return Err(());
        }
        self.parse_ident_list_parens()
    }

    fn expect_ident_or_string_key(&mut self) -> PResult<Ident> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(Ident::new(name, span))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Ident::new(s, span))
            }
            other => {
                self.err("E0013", span, format!("expected an object key, found {other}"));
                Err(())
            }
        }
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        match self.peek().tok.clone() {
            Tok::Int(n) => {
                self.bump();
                Ok(Expr::new(ExprKind::Int(n), span))
            }
            Tok::Decimal(n) => {
                self.bump();
                Ok(Expr::new(ExprKind::Decimal(n), span))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::Str(s), span))
            }
            Tok::RawStr(s) => {
                self.bump();
                Ok(Expr::new(ExprKind::RawStr(s), span))
            }
            Tok::Local(n) => {
                self.bump();
                Ok(Expr::new(ExprKind::Local(Ident::new(n, span)), span))
            }
            Tok::PathParam(n) => {
                self.bump();
                Ok(Expr::new(ExprKind::PathParam(Ident::new(n, span)), span))
            }
            Tok::LBrace => {
                let (entries, end) = self.parse_object_entries()?;
                Ok(Expr::new(ExprKind::Object(entries), span.to(end)))
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                if !self.at(&Tok::RBracket) {
                    items.push(self.parse_expr()?);
                    while self.eat(&Tok::Comma) {
                        if self.at(&Tok::RBracket) {
                            break;
                        }
                        items.push(self.parse_expr()?);
                    }
                }
                let end = self.expect(Tok::RBracket)?.span;
                Ok(Expr::new(ExprKind::Array(items), span.to(end)))
            }
            Tok::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(w) => match w.as_str() {
                "true" => {
                    self.bump();
                    Ok(Expr::new(ExprKind::Bool(true), span))
                }
                "false" => {
                    self.bump();
                    Ok(Expr::new(ExprKind::Bool(false), span))
                }
                "null" => {
                    self.bump();
                    Ok(Expr::new(ExprKind::Null, span))
                }
                "select" => {
                    let s = self.parse_select()?;
                    let sp = s.span;
                    Ok(Expr::new(ExprKind::Select(Box::new(s)), sp))
                }
                "insert" if self.word_at(1, "into") => {
                    let s = self.parse_insert()?;
                    let sp = s.span;
                    Ok(Expr::new(ExprKind::Insert(Box::new(s)), sp))
                }
                "update" if matches!(self.peek_at(1).tok, Tok::Ident(_)) => {
                    let s = self.parse_update()?;
                    let sp = s.span;
                    Ok(Expr::new(ExprKind::Update(Box::new(s)), sp))
                }
                "delete" if self.word_at(1, "from") => {
                    let s = self.parse_delete()?;
                    let sp = s.span;
                    Ok(Expr::new(ExprKind::Delete(Box::new(s)), sp))
                }
                _ => {
                    if self.check_removed_keyword() {
                        return Err(());
                    }
                    self.bump();
                    Ok(Expr::new(ExprKind::Name(Ident::new(w, span)), span))
                }
            },
            other => {
                self.err("E0014", span, format!("expected an expression, found {other}"));
                Err(())
            }
        }
    }

    // ------------------------------------------------------------ queries

    /// Clause order is fixed (queries.md §1). Parsing accepts them in any
    /// order and reports `E0501` naming the expected position, because
    /// "expected `as`, found `orderby`" is a worse message than "the
    /// projection comes before `orderby`".
    fn parse_select(&mut self) -> PResult<SelectExpr> {
        let start = self.span();
        self.expect_word("select")?;
        let binder = match self.peek().tok.clone() {
            Tok::Ident(w) if w != "from" => self.expect_ident()?,
            _ => {
                let s = self.span();
                self.err_note(
                    "E0015",
                    s,
                    "`select` needs a binder before `from`",
                    "write `select I from App.billing.Invoices` — the binder is what column \
                     references and joins attach to",
                    "names.md §5.4",
                );
                return Err(());
            }
        };
        self.expect_word("from")?;
        let source = self.parse_qualified_table()?;

        self.query_depth += 1;
        let r = self.parse_select_tail(start, binder, source);
        self.query_depth -= 1;
        r
    }

    fn parse_select_tail(
        &mut self,
        start: Span,
        binder: Ident,
        source: QualifiedTable,
    ) -> PResult<SelectExpr> {
        let mut joins = Vec::new();
        let mut filter = None;
        let mut group_by = Vec::new();
        let mut having = None;
        let mut projection = None;
        let mut order_by = Vec::new();
        let mut limit = None;
        let mut page = None;
        let mut first = false;
        let mut end = source.span;
        let mut phase = 0u8;

        loop {
            let cspan = self.span();
            let (this_phase, name): (u8, &str) = if self.at_word("left") || self.at_word("inner") {
                (1, "join")
            } else if self.at_word("where") {
                (2, "where")
            } else if self.at_word("group") && self.word_at(1, "by") {
                (3, "group by")
            } else if self.at_word("having") {
                (4, "having")
            } else if self.at_word("as") && self.peek_at(1).is(&Tok::LBrace) {
                (5, "as { }")
            } else if self.at_word("orderby") {
                (6, "orderby")
            } else if self.at_word("page") || self.at_word("limit") {
                (7, "page/limit")
            } else if self.at_word("first") {
                (8, "first")
            } else {
                break;
            };

            if this_phase < phase {
                self.diags.push(
                    Diagnostic::error(
                        "E0501",
                        cspan,
                        format!("`{name}` appears after a later clause"),
                    )
                    .note(
                        "clause order is fixed: joins, where, group by, having, \
                         as { }, orderby, page/limit, first",
                    )
                    .clause("queries.md §1"),
                );
            }
            phase = this_phase.max(phase);

            match this_phase {
                1 => {
                    let j = self.parse_join()?;
                    end = j.span;
                    joins.push(j);
                }
                2 => {
                    self.bump();
                    let e = self.parse_expr()?;
                    end = e.span;
                    filter = Some(e);
                }
                3 => {
                    self.bump();
                    self.bump();
                    group_by.push(self.parse_expr()?);
                    while self.eat(&Tok::Comma) {
                        group_by.push(self.parse_expr()?);
                    }
                    if let Some(g) = group_by.last() {
                        end = g.span;
                    }
                }
                4 => {
                    self.bump();
                    let e = self.parse_expr()?;
                    end = e.span;
                    having = Some(e);
                }
                5 => {
                    self.bump();
                    let shape = self.parse_object_shape()?;
                    end = shape.span;
                    projection = Some(shape);
                }
                6 => {
                    order_by = self.parse_order_by()?;
                    if let Some(k) = order_by.last() {
                        end = k.span;
                    }
                }
                7 => {
                    if self.at_word("page") {
                        let p = self.parse_page()?;
                        end = p.span;
                        page = Some(p);
                    } else {
                        self.bump();
                        let e = self.parse_expr()?;
                        end = e.span;
                        limit = Some(e);
                    }
                }
                8 => {
                    end = self.bump().span;
                    first = true;
                }
                _ => unreachable!(),
            }
        }

        Ok(SelectExpr {
            binder,
            source,
            joins,
            filter,
            group_by,
            having,
            projection,
            order_by,
            limit,
            page,
            first,
            span: start.to(end),
        })
    }

    fn parse_join(&mut self) -> PResult<JoinClause> {
        let start = self.span();
        let kind = if self.eat_word("left") {
            JoinKind::Left
        } else if self.eat_word("inner") {
            JoinKind::Inner
        } else {
            let s = self.span();
            self.err_note(
                "E0016",
                s,
                "expected `left` or `inner`",
                "`right`, `full` and `cross` are not grammar — swap the sides and use `left`",
                "queries.md §4.1",
            );
            return Err(());
        };
        self.expect_word("join")?;
        let table = self.parse_qualified_table()?;

        // Optional binder: an identifier that is not the `on` keyword.
        let binder = if matches!(&self.peek().tok, Tok::Ident(w) if w != "on") {
            self.expect_ident()?
        } else {
            table.object.clone()
        };

        self.expect_word("on")?;
        let on = self.parse_expr()?;
        let mut end = on.span;

        // A `where` here filters the *child* collection (queries.md §4.7).
        let filter = if self.at_word("where") && !self.word_at(1, "exists") {
            self.bump();
            let e = self.parse_expr()?;
            end = e.span;
            Some(e)
        } else {
            None
        };

        // `as {` is the query's projection, not this join's result — a join
        // with no result is E0535 from the planner, and the projection has
        // to keep parsing.
        let result = if self.at_word("as")
            && (self.word_at(1, "one") || self.word_at(1, "many") || self.word_at(1, "group"))
        {
            let rstart = self.span();
            self.bump();
            let cardinality = if self.eat_word("one") {
                Cardinality::One
            } else if self.eat_word("many") {
                Cardinality::Many
            } else if self.at_word("group") && !self.word_at(1, "by") {
                self.bump();
                Cardinality::Group
            } else {
                let s = self.span();
                self.err_note(
                    "E0017",
                    s,
                    "expected `one`, `many` or `group`",
                    "cardinality is written at the join: `as one account`, \
                     `as many members`, or `as group` for a join that only feeds \
                     aggregates",
                    "queries.md §4.3",
                );
                return Err(());
            };
            let name = if cardinality == Cardinality::Group {
                Ident::new("", rstart)
            } else {
                self.expect_ident()?
            };
            let under = if self.eat_word("under") {
                Some(self.expect_ident()?)
            } else {
                None
            };
            let order_by = if self.at_word("orderby") {
                self.parse_order_by()?
            } else {
                Vec::new()
            };
            let limit = if self.at_word("limit") {
                self.bump();
                Some(self.parse_expr()?)
            } else {
                None
            };
            end = limit
                .as_ref()
                .map(|e| e.span)
                .or_else(|| order_by.last().map(|k| k.span))
                .unwrap_or(name.span);
            Some(JoinResult {
                cardinality,
                name,
                under,
                order_by,
                limit,
                span: rstart.to(end),
            })
        } else {
            // Reported by the planner as E0535, not here: a join with no
            // result still parses, so the rest of the file stays checkable.
            None
        };

        Ok(JoinClause {
            kind,
            table,
            binder,
            on,
            filter,
            result,
            span: start.to(end),
        })
    }

    fn parse_object_shape(&mut self) -> PResult<ObjectShape> {
        let start = self.expect(Tok::LBrace)?.span;
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at_eof() {
            let name = self.expect_ident()?;
            if self.at(&Tok::Colon) {
                self.bump();
                if self.at(&Tok::LBrace) {
                    let shape = self.parse_object_shape()?;
                    let span = name.span.to(shape.span);
                    fields.push(ProjField::Nested {
                        alias: name,
                        shape,
                        span,
                    });
                } else {
                    let value = self.parse_expr()?;
                    let span = name.span.to(value.span);
                    fields.push(ProjField::Expr {
                        alias: name,
                        value,
                        span,
                    });
                }
            } else {
                fields.push(ProjField::Column(name));
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        Ok(ObjectShape {
            fields,
            span: start.to(end),
        })
    }

    fn parse_order_by(&mut self) -> PResult<Vec<SortKey>> {
        self.expect_word("orderby")?;
        let mut out = Vec::new();
        loop {
            let expr = self.parse_expr()?;
            let mut span = expr.span;
            let mut desc = false;
            if self.at_word("desc") {
                span = span.to(self.bump().span);
                desc = true;
            } else if self.at_word("asc") {
                span = span.to(self.bump().span);
            }
            let nulls = self.parse_nulls_order();
            out.push(SortKey {
                expr,
                desc,
                nulls,
                span,
            });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    fn parse_page(&mut self) -> PResult<PageClause> {
        let start = self.expect_word("page")?.span;
        let after = if self.eat_word("after") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect_word("size")?;
        let size = self.parse_expr()?;
        let mut end = size.span;
        let max = if self.at_word("max") {
            self.bump();
            let e = self.parse_expr()?;
            end = e.span;
            Some(e)
        } else {
            None
        };
        Ok(PageClause {
            after,
            size,
            max,
            span: start.to(end),
        })
    }

    fn parse_insert(&mut self) -> PResult<InsertExpr> {
        let start = self.expect_word("insert")?.span;
        self.expect_word("into")?;
        let table = self.parse_qualified_table()?;
        self.query_depth += 1;
        let r = (|| -> PResult<InsertExpr> {
            let (values, mut end) = self.parse_object_entries()?;
            let conflict = if self.at_word("on") && self.word_at(1, "conflict") {
                let c = self.parse_conflict()?;
                end = c.span;
                Some(c)
            } else {
                None
            };
            let projection = if self.at_word("as") && self.peek_at(1).is(&Tok::LBrace) {
                self.bump();
                let shape = self.parse_object_shape()?;
                end = shape.span;
                Some(shape)
            } else {
                None
            };
            Ok(InsertExpr {
                table: table.clone(),
                values,
                conflict,
                projection,
                span: start.to(end),
            })
        })();
        self.query_depth -= 1;
        r
    }

    fn parse_conflict(&mut self) -> PResult<ConflictClause> {
        let start = self.expect_word("on")?.span;
        self.expect_word("conflict")?;
        let columns = if self.at(&Tok::LParen) {
            self.parse_ident_list_parens()?
        } else {
            Vec::new()
        };
        self.expect_word("do")?;
        let (action, end) = if self.at_word("nothing") {
            (ConflictAction::DoNothing, self.bump().span)
        } else {
            self.expect_word("update")?;
            let sets = self.parse_set_clause()?;
            let end = sets
                .last()
                .map(|s| match s {
                    SetItem::Set { span, .. } | SetItem::Spread { span, .. } => *span,
                })
                .unwrap_or(start);
            (ConflictAction::DoUpdate(sets), end)
        };
        Ok(ConflictClause {
            columns,
            action,
            span: start.to(end),
        })
    }

    fn parse_set_clause(&mut self) -> PResult<Vec<SetItem>> {
        self.expect_word("set")?;
        let mut out = Vec::new();
        loop {
            let start = self.span();
            if self.at(&Tok::DotDotDot) {
                self.bump();
                let sspan = self.span();
                let source = match self.peek().tok.clone() {
                    Tok::Local(n) => {
                        self.bump();
                        Ident::new(n, sspan)
                    }
                    other => {
                        self.err_note(
                            "E0012",
                            sspan,
                            format!("expected `$name` after `...`, found {other}"),
                            "a spread source is a local with a declared shape",
                            "types.md §9.1",
                        );
                        return Err(());
                    }
                };
                let except = self.parse_except_list()?;
                out.push(SetItem::Spread {
                    span: start.to(source.span),
                    source,
                    except,
                });
            } else {
                let column = self.expect_ident()?;
                let optional = if self.at(&Tok::EqOpt) {
                    self.bump();
                    true
                } else {
                    self.expect(Tok::Eq)?;
                    false
                };
                let value = self.parse_expr()?;
                out.push(SetItem::Set {
                    span: start.to(value.span),
                    column,
                    value,
                    optional,
                });
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    fn parse_update(&mut self) -> PResult<UpdateExpr> {
        let start = self.expect_word("update")?.span;
        let table = self.parse_qualified_table()?;
        self.query_depth += 1;
        let r = (|| -> PResult<UpdateExpr> {
            let sets = self.parse_set_clause()?;
            let mut end = start;
            let mut filter = None;
            let mut projection = None;
            let mut order_by = Vec::new();
            let mut first = false;
            if self.eat_word("where") {
                let e = self.parse_expr()?;
                end = e.span;
                filter = Some(e);
            }
            if self.at_word("as") && self.peek_at(1).is(&Tok::LBrace) {
                self.bump();
                let shape = self.parse_object_shape()?;
                end = shape.span;
                projection = Some(shape);
            }
            if self.at_word("orderby") {
                order_by = self.parse_order_by()?;
                if let Some(k) = order_by.last() {
                    end = k.span;
                }
            }
            if self.at_word("first") {
                end = self.bump().span;
                first = true;
            }
            Ok(UpdateExpr {
                table: table.clone(),
                sets,
                filter,
                projection,
                order_by,
                first,
                span: start.to(end),
            })
        })();
        self.query_depth -= 1;
        r
    }

    fn parse_delete(&mut self) -> PResult<DeleteExpr> {
        let start = self.expect_word("delete")?.span;
        self.expect_word("from")?;
        let table = self.parse_qualified_table()?;
        self.query_depth += 1;
        let r = (|| -> PResult<DeleteExpr> {
            let mut end = table.span;
            let mut filter = None;
            let mut projection = None;
            let mut order_by = Vec::new();
            let mut first = false;
            if self.eat_word("where") {
                let e = self.parse_expr()?;
                end = e.span;
                filter = Some(e);
            }
            if self.at_word("as") && self.peek_at(1).is(&Tok::LBrace) {
                self.bump();
                let shape = self.parse_object_shape()?;
                end = shape.span;
                projection = Some(shape);
            }
            if self.at_word("orderby") {
                order_by = self.parse_order_by()?;
                if let Some(k) = order_by.last() {
                    end = k.span;
                }
            }
            if self.at_word("first") {
                end = self.bump().span;
                first = true;
            }
            Ok(DeleteExpr {
                table: table.clone(),
                filter,
                projection,
                order_by,
                first,
                span: start.to(end),
            })
        })();
        self.query_depth -= 1;
        r
    }
}

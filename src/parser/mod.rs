//! Recursive-descent parser for `.jwc` source.
//!
//! Split across sub-modules:
//! - [`decl`] — declaration parsers (`dbcontext`, `entity`, `class`, `route`,
//!   `function`, `middleware`, `const`, `dome`, `mount`, `group`, `import`,
//!   `namespace`).
//! - [`stmt`] — statement parsers (`let`, `if`, `while`, `for ... in`, `try`,
//!   `validate body`, `transaction`, and the DB statement forms).
//! - [`expr`] — expression precedence ladder, literal forms, the `select ...`
//!   expression, and `where` sub-grammar.
//! - [`validate`] — semantic validation pass (`validate_program`).
//!
//! All sub-modules attach methods to the [`Parser`] state struct via
//! `impl<'a> Parser<'a> { ... }` so they share the lexer/lookahead scaffolding
//! defined here.
//!
//! The public surface (consumed by `cmd/check.rs`, `project.rs`, `jwc_lsp`,
//! and the integration test suite) is:
//! - [`parse_program`] — parse a single `.jwc` source string.
//! - [`parse_program_with_label`] — same, with a display label stamped on the
//!   resulting `SourceFile` so multi-file projects can render
//!   `at <file>:<line>:<col>` for validator errors.
//! - [`validate_program`] — the semantic check pass run after parsing.

use anyhow::{anyhow, Result};

use crate::ast::{Expr, Program, Visibility};
use crate::diag::SourceMap;
use crate::lexer::{Keyword, Lexer, Token, TokenKind};

mod decl;
mod expr;
mod stmt;
mod validate;
mod validate_walk;

#[cfg(test)]
mod tests;

pub use validate::validate_program;

pub fn parse_program(source: &str) -> Result<Program> {
    parse_program_with_label(source, "")
}

/// Like [`parse_program`] but lets the caller stamp a display label on the
/// produced [`SourceFile`] entry. The project loader passes the
/// repo-relative path of each file so multi-file projects can render
/// `at <file>:<line>:<col>` for validator errors.
pub fn parse_program_with_label(source: &str, label: &str) -> Result<Program> {
    let mut parser = Parser::new(source)?;
    let mut program = parser.parse_program()?;
    program.sources = vec![crate::ast::SourceFile {
        label: label.to_string(),
        text: source.to_string(),
    }];
    Ok(program)
}

pub(super) fn visibility_from(is_pub: bool) -> Visibility {
    if is_pub {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

pub(super) struct Parser<'a> {
    pub(super) lexer: Lexer<'a>,
    pub(super) current: Token,
    pub(super) source_map: SourceMap,
    /// Active namespace for the current file. `namespace foo.bar;` sets this
    /// for the rest of the file. Cleared by `__jwc_namespace_reset__;` (see
    /// `project::load_project_from_root` for the inter-file reset trick).
    pub(super) current_namespace: Vec<String>,
    /// Stack of group contexts. Each entry contributes one path segment and
    /// (optionally) middleware names to every `route` and `mount` parsed
    /// inside it. Frames are pushed/popped around `group { ... }` blocks.
    pub(super) group_stack: Vec<GroupFrame>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct GroupFrame {
    /// Just this frame's own prefix segment (already starts with `/` or is empty).
    pub(super) prefix: String,
    /// Just this frame's own middleware list (does not include outer frames).
    pub(super) middlewares: Vec<String>,
}

impl<'a> Parser<'a> {
    pub(super) fn new(source: &'a str) -> Result<Self> {
        let mut lexer = Lexer::new(source);
        let current = lexer.next_token()?;
        Ok(Self {
            lexer,
            current,
            source_map: SourceMap::new(source),
            current_namespace: Vec::new(),
            group_stack: Vec::new(),
        })
    }

    /// The accumulated path prefix from every enclosing group, joined.
    /// Always starts with `/` when non-empty; returns empty string at root.
    pub(super) fn group_prefix(&self) -> String {
        let mut out = String::new();
        for frame in &self.group_stack {
            out.push_str(&frame.prefix);
        }
        out
    }

    /// All middleware names contributed by enclosing groups, in outer→inner
    /// order. Each route's own `use Mw, ...` list is appended after this.
    pub(super) fn group_middlewares(&self) -> Vec<String> {
        let mut out = Vec::new();
        for frame in &self.group_stack {
            out.extend(frame.middlewares.iter().cloned());
        }
        out
    }

    pub(super) fn parse_program(&mut self) -> Result<Program> {
        let mut program = Program::default();
        // Pending visibility modifier: Some(true) → next decl is public,
        // Some(false) → explicit private, None → no modifier (defaults to
        // private). Tracking three states catches `public public function`
        // and `public private function` as user errors.
        let mut pending_vis: Option<bool> = None;

        while !matches!(self.current.kind, TokenKind::Eof) {
            // `public` / `private` modifiers immediately before a declaration.
            if matches!(self.current.kind, TokenKind::Keyword(Keyword::Public)) {
                if pending_vis.is_some() {
                    return Err(self.error_here("duplicate visibility modifier"));
                }
                self.bump()?;
                pending_vis = Some(true);
                continue;
            }
            if matches!(self.current.kind, TokenKind::Keyword(Keyword::Private)) {
                if pending_vis.is_some() {
                    return Err(self.error_here("duplicate visibility modifier"));
                }
                self.bump()?;
                pending_vis = Some(false);
                continue;
            }

            // Snapshot the pending visibility for this iteration. The match
            // arms that accept a modifier call `take()` so the next loop
            // iteration starts clean; arms that reject one leave it as-is
            // and we error out below.
            let next_pub = pending_vis.unwrap_or(false);

            match &self.current.kind {
                TokenKind::Keyword(Keyword::Import) => {
                    pending_vis = None;
                    self.parse_import_stmt(&mut program)?;
                }
                TokenKind::Keyword(Keyword::Namespace) => {
                    pending_vis = None;
                    self.parse_namespace_stmt(&program)?;
                }
                TokenKind::Keyword(Keyword::Mount) => {
                    pending_vis = None;
                    program.mounts.push(self.parse_mount_stmt()?);
                }
                TokenKind::Keyword(Keyword::Group) => {
                    pending_vis = None;
                    self.parse_group_block(&mut program)?;
                }
                TokenKind::Keyword(Keyword::DbContext) => {
                    pending_vis = None;
                    let mut decl = self.parse_dbcontext_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    program.dbcontexts.push(decl);
                }
                TokenKind::Keyword(Keyword::Entity) => {
                    pending_vis = None;
                    let mut decl = self.parse_model_decl(crate::ast::ModelKind::Entity)?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(next_pub);
                    program.models.push(decl);
                }
                TokenKind::Keyword(Keyword::Class) => {
                    pending_vis = None;
                    let mut decl = self.parse_model_decl(crate::ast::ModelKind::Class)?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(next_pub);
                    program.models.push(decl);
                }
                TokenKind::Keyword(Keyword::Route) => {
                    pending_vis = None;
                    let mut decl = self.parse_route_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    // Visibility on routes is currently a no-op — routes are
                    // activation-gated via `register` instead.
                    program.routes.push(decl);
                }
                TokenKind::Keyword(Keyword::Function) => {
                    pending_vis = None;
                    let mut fn_decl = self.parse_function_decl(None)?;
                    fn_decl.namespace = self.current_namespace.clone();
                    fn_decl.visibility = visibility_from(next_pub);
                    program.functions.push(fn_decl);
                }
                TokenKind::Keyword(Keyword::Async) => {
                    pending_vis = None;
                    self.bump()?;
                    if !matches!(self.current.kind, TokenKind::Keyword(Keyword::Function)) {
                        return Err(self.error_here("expected 'function' after 'async'"));
                    }
                    let mut fn_decl = self.parse_function_decl(None)?;
                    fn_decl.is_async = true;
                    fn_decl.namespace = self.current_namespace.clone();
                    fn_decl.visibility = visibility_from(next_pub);
                    program.functions.push(fn_decl);
                }
                TokenKind::Keyword(Keyword::Const) => {
                    if pending_vis.is_some() {
                        return Err(self.error_here("visibility modifier is not valid on const"));
                    }
                    pending_vis = None;
                    let decl = self.parse_const_decl()?;
                    program.consts.push(decl);
                }
                TokenKind::Keyword(Keyword::Dome) => {
                    pending_vis = None;
                    self.parse_dome_decl(&mut program, next_pub)?;
                }
                TokenKind::Keyword(Keyword::Middleware) => {
                    pending_vis = None;
                    let mut decl = self.parse_middleware_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(next_pub);
                    program.middlewares.push(decl);
                }
                TokenKind::Keyword(Keyword::ErrorHandler) => {
                    if pending_vis.is_some() {
                        return Err(
                            self.error_here("visibility modifier is not valid on errorHandler")
                        );
                    }
                    if program.error_handler.is_some() {
                        return Err(self.error_here("only one errorHandler is allowed per project"));
                    }
                    self.bump()?;
                    self.expect_symbol('(')?;
                    let catch_var =
                        self.expect_ident("expected error variable name after 'errorHandler('")?;
                    self.expect_symbol(')')?;
                    let body = self.parse_block()?;
                    program.error_handler = Some(crate::ast::ErrorHandlerDecl { catch_var, body });
                }
                _ => {
                    return Err(self.error_here(
                        "expected import, namespace, mount, group, public, private, dbcontext, entity/class, route, function, const, middleware, errorHandler, or dome",
                    ));
                }
            }
        }

        if pending_vis.is_some() {
            return Err(
                self.error_here("trailing visibility modifier has no declaration to apply to")
            );
        }

        Ok(program)
    }

    pub(super) fn check_ident_eq(&self, expected: &str) -> bool {
        matches!(&self.current.kind, TokenKind::Ident(v) if v.eq_ignore_ascii_case(expected))
    }

    pub(super) fn check_symbol(&self, expected: char) -> bool {
        matches!(self.current.kind, TokenKind::Symbol(c) if c == expected)
    }

    pub(super) fn expect_symbol(&mut self, expected: char) -> Result<()> {
        if self.check_symbol(expected) {
            self.bump()?;
            Ok(())
        } else {
            Err(self.error_here(&format!("expected '{}'", expected)))
        }
    }

    pub(super) fn expect_keyword(&mut self, expected: Keyword) -> Result<()> {
        if self.current.kind == TokenKind::Keyword(expected.clone()) {
            self.bump()?;
            Ok(())
        } else {
            Err(self.error_here("unexpected token"))
        }
    }

    pub(super) fn expect_ident(&mut self, msg: &str) -> Result<String> {
        match &self.current.kind {
            TokenKind::Ident(value) => {
                let value = value.clone();
                self.bump()?;
                Ok(value)
            }
            _ => Err(self.error_here(msg)),
        }
    }

    /// Parse a dotted type identifier: `Ident ('.' Ident)*` → `"A.B.C"`.
    /// Used by `catch (e: A.B)` and any future surface that needs
    /// hierarchical type names. Trailing `.` is a parse error.
    pub(super) fn parse_dotted_type(&mut self) -> Result<String> {
        let mut parts = vec![self.expect_ident("expected type name")?];
        while matches!(self.current.kind, TokenKind::Symbol('.')) {
            self.bump()?;
            parts.push(self.expect_ident("expected type segment after '.'")?);
        }
        Ok(parts.join("."))
    }

    pub(super) fn expect_number(&mut self, msg: &str) -> Result<i64> {
        match self.current.kind.clone() {
            TokenKind::Number(value) => {
                self.bump()?;
                if value.contains('.') {
                    return Err(self.error_here("expected integer number"));
                }
                value.parse::<i64>().map_err(|_| self.error_here(msg))
            }
            _ => Err(self.error_here(msg)),
        }
    }

    pub(super) fn expect_string(&mut self, msg: &str) -> Result<String> {
        match self.current.kind.clone() {
            TokenKind::String(value) => {
                self.bump()?;
                Ok(value)
            }
            _ => Err(self.error_here(msg)),
        }
    }

    pub(super) fn bump(&mut self) -> Result<()> {
        self.current = self.lexer.next_token()?;
        Ok(())
    }

    pub(super) fn error_here(&self, msg: &str) -> anyhow::Error {
        self.error_at(self.current.offset, msg)
    }

    /// Render an error at a known byte offset. Appends a rustc-style source
    /// snippet ("3 | <line>\n  | ^ here") underneath the `at line X, col Y`
    /// header so the user can place the failure without opening an editor.
    pub(super) fn error_at(&self, offset: usize, msg: &str) -> anyhow::Error {
        let (line, col) = self.source_map.line_col(offset);
        let snippet = self.source_map.snippet(offset);
        anyhow!("{msg} at line {line}, col {col}{snippet}")
    }
}

/// Parse a single expression from a template string hole source, e.g. `env("PG_USER")`.
pub(super) fn parse_template_hole(src: &str) -> Result<Expr> {
    let mut p = Parser::new(src.trim())?;
    let expr = p.parse_expr()?;
    Ok(expr)
}

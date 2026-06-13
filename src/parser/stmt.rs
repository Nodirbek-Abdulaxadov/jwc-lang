//! Statement parsers — `parse_stmt` and friends (`if`, `while`, `try`,
//! `for ... in`, `validate body { ... }`, `transaction { ... }`, the
//! DB-statement forms `insert/update/delete`, and the assignment forms),
//! plus `parse_block`.
//!
//! All methods are attached to the `Parser` state struct from `mod.rs`.

use anyhow::Result;

use crate::ast::{Expr, Stmt, ValidateField, ValidateRule};
use crate::lexer::{Keyword, TokenKind};

use super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_stmt(&mut self) -> Result<Stmt> {
        match &self.current.kind {
            TokenKind::Keyword(Keyword::Validate) => self.parse_validate_body_stmt(),
            TokenKind::Keyword(Keyword::Try) => self.parse_try_stmt(),
            TokenKind::Keyword(Keyword::Transaction) => {
                self.bump()?;
                let body = self.parse_block()?;
                Ok(Stmt::Transaction { body })
            }
            TokenKind::Keyword(Keyword::Let) => {
                self.bump()?;
                let name = self.expect_ident("expected variable name")?;
                self.expect_symbol('=')?;
                let value = self.parse_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Let { name, value })
            }
            TokenKind::Keyword(Keyword::Print) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let value = self.parse_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol(';')?;
                Ok(Stmt::Print(value))
            }
            TokenKind::Keyword(Keyword::Return) => {
                self.bump()?;
                if self.check_symbol(';') {
                    self.expect_symbol(';')?;
                    Ok(Stmt::Return(None))
                } else {
                    let value = self.parse_expr()?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::Return(Some(value)))
                }
            }
            TokenKind::Keyword(Keyword::If) => self.parse_if_stmt(),
            TokenKind::Keyword(Keyword::While) => self.parse_while_stmt(),
            TokenKind::Keyword(Keyword::For) => self.parse_for_stmt(),
            TokenKind::Keyword(Keyword::Break) => {
                self.bump()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Break)
            }
            TokenKind::Keyword(Keyword::Continue) => {
                self.bump()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Continue)
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("insert") => {
                self.bump()?;
                let var = self.expect_ident("expected variable name after 'insert'")?;
                let kw = self.expect_ident("expected 'into'")?;
                if !kw.eq_ignore_ascii_case("into") {
                    return Err(self.error_here("expected 'into' in insert statement"));
                }
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbInsert {
                    var,
                    context_var: ctx,
                    table,
                })
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("update") => {
                self.bump()?;
                let first_ident =
                    self.expect_ident("expected variable or context name after 'update'")?;
                // Disambiguate the two forms:
                //   * `update var in CTX.Table;`     — whole-row update (legacy)
                //   * `update CTX.Table set ...;`    — atomic partial update
                // After consuming the first ident:
                //   * If the next token is `in` → it was a variable.
                //   * If the next token is `.` → it was a context name, and a
                //     `set ... where ...` clause follows.
                if self.check_symbol('.') {
                    self.expect_symbol('.')?;
                    let table = self.expect_ident("expected table name after '.'")?;
                    if !self.check_ident_eq("set") {
                        return Err(self.error_here("expected 'set' in atomic update statement"));
                    }
                    self.bump()?;
                    let assignments = self.parse_update_assignments()?;
                    if !self.check_ident_eq("where") {
                        return Err(self.error_here(
                            "error[E011]: atomic 'update CTX.Table set ...' requires a 'where' clause",
                        ));
                    }
                    self.bump()?;
                    let where_clause = Box::new(self.parse_where_or()?);
                    self.expect_symbol(';')?;
                    return Ok(Stmt::DbUpdateSet {
                        context_var: first_ident,
                        table,
                        assignments,
                        where_clause,
                    });
                }
                if self.current.kind != TokenKind::Keyword(Keyword::In) {
                    return Err(self.error_here("expected 'in' in update statement"));
                }
                self.bump()?;
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbUpdate {
                    var: first_ident,
                    context_var: ctx,
                    table,
                })
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("delete") => {
                self.bump()?;

                if self.check_ident_eq("from") {
                    // Bulk form: `delete from CTX.Table where ... ;`
                    self.bump()?;
                    let (ctx, table) = self.parse_db_ref()?;
                    if !self.check_ident_eq("where") {
                        return Err(self.error_here(
                            "error[E013]: bulk 'delete from CTX.Table' requires a 'where' clause",
                        ));
                    }
                    self.bump()?;
                    let where_clause = Box::new(self.parse_where_or()?);
                    self.expect_symbol(';')?;
                    return Ok(Stmt::DbDeleteWhere {
                        context_var: ctx,
                        table,
                        where_clause,
                    });
                }

                let var = self.expect_ident("expected variable name after 'delete'")?;
                let kw = self.expect_ident("expected 'from'")?;
                if !kw.eq_ignore_ascii_case("from") {
                    return Err(self.error_here("expected 'from' in delete statement"));
                }
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbDelete {
                    var,
                    context_var: ctx,
                    table,
                })
            }
            TokenKind::Ident(_) => {
                let name = match self.current.kind.clone() {
                    TokenKind::Ident(v) => v,
                    _ => unreachable!(),
                };
                self.bump()?;
                if self.check_symbol('.') {
                    self.bump()?;
                    let member = self.expect_ident("expected member name after '.'")?;
                    if self.check_symbol('(') {
                        let call = self.parse_call_after_name(format!("{}.{}", name, member))?;
                        self.expect_symbol(';')?;
                        Ok(Stmt::Expr(call))
                    } else {
                        self.expect_symbol('=')?;
                        let value = self.parse_expr()?;
                        self.expect_symbol(';')?;
                        Ok(Stmt::FieldAssign {
                            var: name,
                            field: member,
                            value,
                        })
                    }
                } else if self.check_symbol('=') {
                    self.expect_symbol('=')?;
                    let value = self.parse_expr()?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::Assign { name, value })
                } else if self.check_symbol('(') {
                    let call = self.parse_call_after_name(name)?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::Expr(call))
                } else {
                    self.expect_symbol(';')?;
                    Ok(Stmt::Expr(Expr::Var(name)))
                }
            }
            _ => {
                let expr = self.parse_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    pub(super) fn parse_if_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::If)?;
        self.expect_symbol('(')?;
        let cond = self.parse_expr()?;
        self.expect_symbol(')')?;

        let then_body = self.parse_block()?;
        let else_body = if self.current.kind == TokenKind::Keyword(Keyword::Else) {
            self.bump()?;
            // `else if (...) { ... }` desugars to `else { if (...) { ... } }`
            // so chains of else-if branches parse without curly braces in
            // between.
            if self.current.kind == TokenKind::Keyword(Keyword::If) {
                Some(vec![self.parse_if_stmt()?])
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };

        Ok(Stmt::If {
            cond,
            then_body,
            else_body,
        })
    }

    pub(super) fn parse_while_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::While)?;
        self.expect_symbol('(')?;
        let cond = self.parse_expr()?;
        self.expect_symbol(')')?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    pub(super) fn parse_try_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::Try)?;
        let body = self.parse_block()?;

        self.expect_keyword(Keyword::Catch)?;
        self.expect_symbol('(')?;
        let catch_var = self.expect_ident("expected catch variable name")?;
        let catch_type = if self.check_symbol(':') {
            self.expect_symbol(':')?;
            Some(self.expect_ident("expected error type after ':'")?)
        } else {
            None
        };
        self.expect_symbol(')')?;
        let catch_body = self.parse_block()?;

        Ok(Stmt::Try {
            body,
            catch_var,
            catch_type,
            catch_body,
        })
    }

    pub(super) fn parse_for_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::For)?;
        let var = self.expect_ident("expected variable name after 'for'")?;
        if self.current.kind != TokenKind::Keyword(Keyword::In) {
            return Err(self.error_here("expected 'in' after 'for <var>'"));
        }
        self.bump()?;
        let iter = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::ForIn { var, iter, body })
    }

    pub(super) fn parse_validate_body_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::Validate)?;

        let body_kw = self.expect_ident("expected 'body' after 'validate'")?;
        if !body_kw.eq_ignore_ascii_case("body") {
            return Err(self.error_here("only 'validate body' is supported"));
        }

        self.expect_symbol('{')?;

        let mut fields: Vec<ValidateField> = Vec::new();
        while !self.check_symbol('}') {
            let field_name = self.expect_ident("expected field name in validate body block")?;
            self.expect_symbol(':')?;

            let mut rules = Vec::new();
            rules.push(self.parse_validate_rule()?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                rules.push(self.parse_validate_rule()?);
            }
            self.expect_symbol(';')?;

            fields.push(ValidateField {
                name: field_name,
                rules,
            });
        }

        self.expect_symbol('}')?;
        Ok(Stmt::ValidateBody { fields })
    }

    pub(super) fn parse_validate_rule(&mut self) -> Result<ValidateRule> {
        let name = self.expect_ident("expected validation rule name")?;
        let lower = name.to_ascii_lowercase();

        match lower.as_str() {
            "required" => Ok(ValidateRule::Required),
            "minlength" => {
                let n = self.parse_int_arg("minLength")?;
                Ok(ValidateRule::MinLength(n))
            }
            "maxlength" => {
                let n = self.parse_int_arg("maxLength")?;
                Ok(ValidateRule::MaxLength(n))
            }
            "min" => {
                let n = self.parse_number_arg("min")?;
                Ok(ValidateRule::Min(n))
            }
            "max" => {
                let n = self.parse_number_arg("max")?;
                Ok(ValidateRule::Max(n))
            }
            "pattern" => {
                self.expect_symbol('(')?;
                let regex_src = self.expect_string("expected regex string argument for pattern")?;
                self.expect_symbol(')')?;
                if regex::Regex::new(&regex_src).is_err() {
                    return Err(self.error_here("invalid regex passed to pattern()"));
                }
                Ok(ValidateRule::Pattern(regex_src))
            }
            other => Err(self.error_here(&format!("unknown validation rule '{other}'"))),
        }
    }

    pub(super) fn parse_int_arg(&mut self, rule_name: &str) -> Result<i64> {
        self.expect_symbol('(')?;
        let n = self.parse_signed_number(&format!("expected integer argument for {rule_name}"))?;
        self.expect_symbol(')')?;
        Ok(n)
    }

    pub(super) fn parse_number_arg(&mut self, rule_name: &str) -> Result<String> {
        self.expect_symbol('(')?;
        let sign = if self.check_symbol('-') {
            self.expect_symbol('-')?;
            "-"
        } else {
            ""
        };
        let token = match self.current.kind.clone() {
            TokenKind::Number(v) => v,
            _ => return Err(self.error_here(&format!("expected numeric argument for {rule_name}"))),
        };
        self.bump()?;
        self.expect_symbol(')')?;
        Ok(format!("{sign}{token}"))
    }

    pub(super) fn parse_block(&mut self) -> Result<Vec<Stmt>> {
        self.expect_symbol('{')?;
        let mut body = Vec::new();
        while !self.check_symbol('}') {
            body.push(self.parse_stmt()?);
        }
        self.expect_symbol('}')?;
        Ok(body)
    }
}

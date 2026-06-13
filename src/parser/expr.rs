//! Expression parsers and DB-clause parsers.
//!
//! Covers the full precedence ladder (`parse_or_expr` → `parse_and_expr` → ...
//! → `parse_primary_expr`), the literal forms (`parse_array_literal`,
//! `parse_object_literal`), the call form (`parse_call_after_name`), and the
//! `select ... from CTX.Table [where ... orderby ... limit ... offset ...]`
//! expression along with the `where` sub-grammar (`parse_where_or` / `_and` /
//! `_atom`) and supporting helpers (`parse_at_or_expr`, `parse_db_int_arg`,
//! `parse_cmp_op`, `parse_db_ref`, `parse_field_path`,
//! `parse_update_assignments`).
//!
//! All methods attach to the `Parser` state struct from `mod.rs`.

use anyhow::Result;

use crate::ast::{AggregateKind, DbOrderBy, DbWhere, Expr, SortDir, WhereExpr};
use crate::lexer::{Keyword, TemplatePart, TokenKind};

use super::{parse_template_hole, Parser};

impl<'a> Parser<'a> {
    pub(super) fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or_expr()
    }

    pub(super) fn parse_or_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_and_expr()?;
        while self.current.kind == TokenKind::Keyword(Keyword::Or) {
            self.bump()?;
            let right = self.parse_and_expr()?;
            expr = Expr::Or(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    pub(super) fn parse_and_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_eq_expr()?;
        while self.current.kind == TokenKind::Keyword(Keyword::And) {
            self.bump()?;
            let right = self.parse_eq_expr()?;
            expr = Expr::And(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    pub(super) fn parse_eq_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_cmp_expr()?;

        loop {
            if self.check_symbol('=') {
                self.expect_symbol('=')?;
                self.expect_symbol('=')?;
                let right = self.parse_cmp_expr()?;
                expr = Expr::Eq(Box::new(expr), Box::new(right));
                continue;
            }

            if self.check_symbol('!') {
                self.expect_symbol('!')?;
                self.expect_symbol('=')?;
                let right = self.parse_cmp_expr()?;
                expr = Expr::Neq(Box::new(expr), Box::new(right));
                continue;
            }

            break;
        }

        Ok(expr)
    }

    pub(super) fn parse_cmp_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_add_expr()?;

        loop {
            if self.check_symbol('<') {
                self.expect_symbol('<')?;
                if self.check_symbol('=') {
                    self.expect_symbol('=')?;
                    let right = self.parse_add_expr()?;
                    expr = Expr::Lte(Box::new(expr), Box::new(right));
                } else {
                    let right = self.parse_add_expr()?;
                    expr = Expr::Lt(Box::new(expr), Box::new(right));
                }
                continue;
            }

            if self.check_symbol('>') {
                self.expect_symbol('>')?;
                if self.check_symbol('=') {
                    self.expect_symbol('=')?;
                    let right = self.parse_add_expr()?;
                    expr = Expr::Gte(Box::new(expr), Box::new(right));
                } else {
                    let right = self.parse_add_expr()?;
                    expr = Expr::Gt(Box::new(expr), Box::new(right));
                }
                continue;
            }

            break;
        }

        Ok(expr)
    }

    pub(super) fn parse_add_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_mul_expr()?;
        while self.check_symbol('+') || self.check_symbol('-') {
            if self.check_symbol('+') {
                self.expect_symbol('+')?;
                let right = self.parse_mul_expr()?;
                expr = Expr::Add(Box::new(expr), Box::new(right));
            } else {
                self.expect_symbol('-')?;
                let right = self.parse_mul_expr()?;
                expr = Expr::Sub(Box::new(expr), Box::new(right));
            }
        }
        Ok(expr)
    }

    pub(super) fn parse_mul_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_unary_expr()?;
        while self.check_symbol('*') || self.check_symbol('/') || self.check_symbol('%') {
            if self.check_symbol('*') {
                self.expect_symbol('*')?;
                let right = self.parse_unary_expr()?;
                expr = Expr::Mul(Box::new(expr), Box::new(right));
            } else if self.check_symbol('/') {
                self.expect_symbol('/')?;
                let right = self.parse_unary_expr()?;
                expr = Expr::Div(Box::new(expr), Box::new(right));
            } else {
                self.expect_symbol('%')?;
                let right = self.parse_unary_expr()?;
                expr = Expr::Mod(Box::new(expr), Box::new(right));
            }
        }
        Ok(expr)
    }

    pub(super) fn parse_unary_expr(&mut self) -> Result<Expr> {
        if self.check_symbol('-') {
            self.expect_symbol('-')?;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr::Neg(Box::new(expr)));
        }
        // Unary `!`. Disambiguated from `!=` because that operator is only
        // parsed at the equality precedence level (after a primary operand),
        // never as a leading token.
        if self.check_symbol('!') {
            self.expect_symbol('!')?;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr::Not(Box::new(expr)));
        }
        if matches!(self.current.kind, TokenKind::Keyword(Keyword::Await)) {
            self.bump()?;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr::Await(Box::new(expr)));
        }
        self.parse_primary_expr()
    }

    pub(super) fn parse_primary_expr(&mut self) -> Result<Expr> {
        match self.current.kind.clone() {
            TokenKind::Number(value) => {
                self.bump()?;
                if value.contains('.') {
                    Ok(Expr::Float(value))
                } else {
                    let int_value = value
                        .parse::<i64>()
                        .map_err(|_| self.error_here("invalid integer literal"))?;
                    Ok(Expr::Int(int_value))
                }
            }
            TokenKind::String(value) => {
                self.bump()?;
                Ok(Expr::Str(value))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump()?;
                Ok(Expr::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump()?;
                Ok(Expr::Bool(false))
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.bump()?;
                Ok(Expr::Null)
            }
            TokenKind::Ident(name) if name.eq_ignore_ascii_case("select") => {
                self.bump()?;
                self.parse_select_expr()
            }
            TokenKind::TemplateStr(parts) => {
                let parts = parts.clone();
                self.bump()?;
                let mut result: Option<Expr> = None;
                for part in parts {
                    let seg = match part {
                        TemplatePart::Literal(s) => Expr::Str(s),
                        TemplatePart::Hole(src) => parse_template_hole(&src)?,
                    };
                    result = Some(match result {
                        None => seg,
                        Some(left) => Expr::Add(Box::new(left), Box::new(seg)),
                    });
                }
                Ok(result.unwrap_or(Expr::Str(String::new())))
            }
            TokenKind::Ident(name) if name.eq_ignore_ascii_case("new") => {
                self.bump()?;
                let entity = self.expect_ident("expected entity name after 'new'")?;
                self.expect_symbol('(')?;
                self.expect_symbol(')')?;
                Ok(Expr::NewEntity { entity })
            }
            TokenKind::Ident(name) => {
                self.bump()?;
                if self.check_symbol('(') {
                    self.parse_call_after_name(name)
                } else if self.check_symbol('.') {
                    self.bump()?;
                    let member = self.expect_ident("expected field or function name after '.'")?;
                    if self.check_symbol('(') {
                        self.parse_call_after_name(format!("{}.{}", name, member))
                    } else {
                        Ok(Expr::FieldGet {
                            var: name,
                            field: member,
                        })
                    }
                } else {
                    Ok(Expr::Var(name))
                }
            }
            TokenKind::Symbol('(') => {
                self.expect_symbol('(')?;
                let expr = self.parse_expr()?;
                self.expect_symbol(')')?;
                Ok(expr)
            }
            TokenKind::Symbol('{') => self.parse_object_literal(),
            TokenKind::Symbol('[') => self.parse_array_literal(),
            _ => Err(self.error_here("expected expression")),
        }
    }

    /// Parse an array literal: `[ expr [, expr]* [,]? ]`. The empty form `[]`
    /// is valid; a trailing comma is allowed. Elements may be heterogeneous.
    pub(super) fn parse_array_literal(&mut self) -> Result<Expr> {
        self.expect_symbol('[')?;
        let mut items: Vec<Expr> = Vec::new();
        if !self.check_symbol(']') {
            loop {
                items.push(self.parse_expr()?);
                if !self.check_symbol(',') {
                    break;
                }
                self.expect_symbol(',')?;
                if self.check_symbol(']') {
                    break;
                }
            }
        }
        self.expect_symbol(']')?;
        Ok(Expr::ArrayLit(items))
    }

    /// Parse an expression-position object literal: `{ key: expr [, key: expr]* }`.
    /// Statement-position `{ ... }` blocks never reach this path because their
    /// callers (function body, if/while, validate body, select projection)
    /// consume the brace directly.
    pub(super) fn parse_object_literal(&mut self) -> Result<Expr> {
        self.expect_symbol('{')?;
        let mut fields: Vec<(String, Expr)> = Vec::new();
        if !self.check_symbol('}') {
            loop {
                let key = self.expect_ident("expected key in object literal")?;
                self.expect_symbol(':')?;
                let value = self.parse_expr()?;
                fields.push((key, value));
                if !self.check_symbol(',') {
                    break;
                }
                self.expect_symbol(',')?;
                if self.check_symbol('}') {
                    break;
                }
            }
        }
        self.expect_symbol('}')?;
        Ok(Expr::ObjectLit(fields))
    }

    pub(super) fn parse_call_after_name(&mut self, name: String) -> Result<Expr> {
        self.expect_symbol('(')?;
        let mut args = Vec::new();
        if !self.check_symbol(')') {
            args.push(self.parse_expr()?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                args.push(self.parse_expr()?);
            }
        }
        self.expect_symbol(')')?;
        Ok(Expr::Call { name, args })
    }

    /// Parse `CTX.TABLE` — returns `(context_var, table)`
    pub(super) fn parse_db_ref(&mut self) -> Result<(String, String)> {
        let ctx = self.expect_ident("expected context variable")?;
        self.expect_symbol('.')?;
        let table = self.expect_ident("expected table name after '.'")?;
        Ok((ctx, table))
    }

    /// Parse one or more `col = expr` pairs separated by commas, used by
    /// `update CTX.Table set <pairs> where ...`. Stops once it sees the
    /// `where` keyword (the caller consumes it next). Bare `col` (without
    /// `=`) is a parse error — the user intends a partial update.
    pub(super) fn parse_update_assignments(&mut self) -> Result<Vec<(String, Expr)>> {
        let mut out = Vec::new();
        loop {
            let col = self.expect_ident("expected column name in 'set' clause")?;
            self.expect_symbol('=')?;
            let expr = self.parse_expr()?;
            out.push((col, expr));
            if self.check_symbol(',') {
                self.expect_symbol(',')?;
                continue;
            }
            break;
        }
        Ok(out)
    }

    /// Parse field path: `ident` or `ident.ident` — returns the full string
    pub(super) fn parse_field_path(&mut self) -> Result<String> {
        let first = self.expect_ident("expected field name")?;
        if self.check_symbol('.') {
            self.bump()?;
            let second = self.expect_ident("expected field name after '.'")?;
            Ok(format!("{}.{}", first, second))
        } else {
            Ok(first)
        }
    }

    /// Parse `select [Entity|*|count(*)] from CTX.TABLE
    ///        [where COND [and|or COND ...]]
    ///        [orderby FIELD [asc|desc]]
    ///        [limit N] [offset N] [first]`
    pub(super) fn parse_select_expr(&mut self) -> Result<Expr> {
        // `count(*)` aggregation form
        if self.check_ident_eq("count") {
            self.bump()?;
            self.expect_symbol('(')?;
            self.expect_symbol('*')?;
            self.expect_symbol(')')?;

            let from_kw = self.expect_ident("expected 'from' after count(*)")?;
            if !from_kw.eq_ignore_ascii_case("from") {
                return Err(self.error_here("expected 'from' after count(*)"));
            }
            let (ctx, table) = self.parse_db_ref()?;
            let where_clause = if self.check_ident_eq("where") {
                self.bump()?;
                Some(Box::new(self.parse_where_or()?))
            } else {
                None
            };
            return Ok(Expr::DbCount {
                context_var: ctx,
                table,
                where_clause,
            });
        }

        // `sum|avg|min|max(Entity.col)` aggregation form
        let agg_kind = if self.check_ident_eq("sum") {
            Some(AggregateKind::Sum)
        } else if self.check_ident_eq("avg") {
            Some(AggregateKind::Avg)
        } else if self.check_ident_eq("min") {
            Some(AggregateKind::Min)
        } else if self.check_ident_eq("max") {
            Some(AggregateKind::Max)
        } else {
            None
        };
        if let Some(kind) = agg_kind {
            self.bump()?;
            self.expect_symbol('(')?;
            let field = self.parse_field_path()?;
            self.expect_symbol(')')?;
            let from_kw = self.expect_ident("expected 'from' after aggregate(...)")?;
            if !from_kw.eq_ignore_ascii_case("from") {
                return Err(self.error_here("expected 'from' after aggregate(...)"));
            }
            let (ctx, table) = self.parse_db_ref()?;
            let where_clause = if self.check_ident_eq("where") {
                self.bump()?;
                Some(Box::new(self.parse_where_or()?))
            } else {
                None
            };
            return Ok(Expr::DbAggregate {
                kind,
                field,
                context_var: ctx,
                table,
                where_clause,
            });
        }

        // entity name or `*`
        let entity = if self.check_symbol('*') {
            self.bump()?;
            "*".to_string()
        } else {
            self.expect_ident("expected entity name or '*' after 'select'")?
        };

        // optional `{ col1, col2, ... }` projection
        let projection = if self.check_symbol('{') {
            self.expect_symbol('{')?;
            if entity == "*" {
                return Err(
                    self.error_here("projection `{ ... }` requires a named entity, not '*'")
                );
            }
            let mut cols = Vec::new();
            if !self.check_symbol('}') {
                cols.push(self.expect_ident("expected column name in projection")?);
                while self.check_symbol(',') {
                    self.expect_symbol(',')?;
                    cols.push(self.expect_ident("expected column name after ','")?);
                }
            }
            self.expect_symbol('}')?;
            if cols.is_empty() {
                return Err(self.error_here("projection `{ ... }` must list at least one column"));
            }
            cols
        } else {
            Vec::new()
        };

        // optional `with rel1, rel2, ...`
        let with_relations = if self.check_ident_eq("with") {
            self.bump()?;
            let mut rels = vec![self.expect_ident("expected navigation name after 'with'")?];
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                rels.push(self.expect_ident("expected navigation name after ','")?);
            }
            rels
        } else {
            Vec::new()
        };

        // `from`
        let from_kw = self.expect_ident("expected 'from' after entity name")?;
        if !from_kw.eq_ignore_ascii_case("from") {
            return Err(self.error_here("expected 'from' in select expression"));
        }

        let (ctx, table) = self.parse_db_ref()?;

        // optional `where COND [and|or COND ...]`
        let where_clause = if self.check_ident_eq("where") {
            self.bump()?;
            Some(Box::new(self.parse_where_or()?))
        } else {
            None
        };

        // optional `group by Entity.col [, Entity.col ...]`
        // `group` is a lexer keyword (used by `parse_group_block` at the
        // top level) so we match by TokenKind rather than identifier text.
        // `by` is read as a plain identifier.
        let group_by = if self.current.kind == TokenKind::Keyword(Keyword::Group) {
            self.bump()?;
            let by_kw = self.expect_ident("expected 'by' after 'group'")?;
            if !by_kw.eq_ignore_ascii_case("by") {
                return Err(self.error_here("expected 'by' after 'group'"));
            }
            let mut cols = vec![self.parse_field_path()?];
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                cols.push(self.parse_field_path()?);
            }
            cols
        } else {
            Vec::new()
        };

        // optional `having COND [and|or COND ...]` — only meaningful after a
        // `group by`, but the parser is permissive; validate_program enforces
        // the dependency.
        let having = if self.check_ident_eq("having") {
            self.bump()?;
            Some(Box::new(self.parse_where_or()?))
        } else {
            None
        };

        // optional `orderby FIELD [asc|desc]`
        let order_by = if self.check_ident_eq("orderby") {
            self.bump()?;
            let field = self.parse_field_path()?;
            let dir = if self.check_ident_eq("desc") {
                self.bump()?;
                SortDir::Desc
            } else if self.check_ident_eq("asc") {
                self.bump()?;
                SortDir::Asc
            } else {
                SortDir::Asc
            };
            Some(DbOrderBy { field, dir })
        } else {
            None
        };

        // optional `limit N`
        let limit = if self.check_ident_eq("limit") {
            self.bump()?;
            Some(Box::new(self.parse_db_int_arg("limit")?))
        } else {
            None
        };

        // optional `offset N`
        let offset = if self.check_ident_eq("offset") {
            self.bump()?;
            Some(Box::new(self.parse_db_int_arg("offset")?))
        } else {
            None
        };

        // optional `first`
        let first = if self.check_ident_eq("first") {
            self.bump()?;
            true
        } else {
            false
        };

        Ok(Expr::DbSelect {
            entity,
            context_var: ctx,
            table,
            where_clause,
            order_by,
            limit,
            offset,
            first,
            with_relations,
            projection,
            group_by,
            having,
        })
    }

    pub(super) fn parse_where_or(&mut self) -> Result<WhereExpr> {
        let mut left = self.parse_where_and()?;
        while self.current.kind == TokenKind::Keyword(Keyword::Or) {
            self.bump()?;
            let right = self.parse_where_and()?;
            left = WhereExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    pub(super) fn parse_where_and(&mut self) -> Result<WhereExpr> {
        let mut left = self.parse_where_atom()?;
        while self.current.kind == TokenKind::Keyword(Keyword::And) {
            self.bump()?;
            let right = self.parse_where_atom()?;
            left = WhereExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    pub(super) fn parse_where_atom(&mut self) -> Result<WhereExpr> {
        if self.check_symbol('(') {
            self.expect_symbol('(')?;
            let inner = self.parse_where_or()?;
            self.expect_symbol(')')?;
            return Ok(inner);
        }

        let field = self.parse_field_path()?;

        if self.check_ident_eq("between") {
            self.bump()?;
            let low = self.parse_in_value()?;
            if self.current.kind != TokenKind::Keyword(Keyword::And) {
                return Err(
                    self.error_here("expected 'and' between bounds in 'between ... and ...'")
                );
            }
            self.bump()?;
            let high = self.parse_in_value()?;
            return Ok(WhereExpr::Between { field, low, high });
        }

        if self.current.kind == TokenKind::Keyword(Keyword::In) {
            self.bump()?;
            self.expect_symbol('(')?;
            let mut values = Vec::new();
            if !self.check_symbol(')') {
                values.push(self.parse_in_value()?);
                while self.check_symbol(',') {
                    self.expect_symbol(',')?;
                    values.push(self.parse_in_value()?);
                }
            }
            self.expect_symbol(')')?;
            if values.is_empty() {
                return Err(self.error_here("'in (...)' must list at least one value"));
            }
            return Ok(WhereExpr::InList { field, values });
        }

        // `is null` / `is not null` — surface syntax for IS NULL / IS NOT NULL.
        // Encoded as an Atom with rhs=Null and op `==`/`!=`; the SQL builder
        // already translates that to IS NULL / IS NOT NULL.
        if self.check_ident_eq("is") {
            self.bump()?;
            let negated = if self.check_ident_eq("not") {
                self.bump()?;
                true
            } else {
                false
            };
            if !self.check_ident_eq("null") {
                return Err(self.error_here("expected 'null' after 'is' or 'is not'"));
            }
            self.bump()?;
            let op = if negated { "!=" } else { "==" }.to_string();
            return Ok(WhereExpr::Atom(DbWhere {
                field,
                op,
                rhs: Expr::Null,
            }));
        }

        let op = if self.check_ident_eq("like") {
            self.bump()?;
            "like".to_string()
        } else if self.check_ident_eq("ilike") {
            self.bump()?;
            "ilike".to_string()
        } else {
            self.parse_cmp_op()?
        };

        let rhs = self.parse_at_or_expr()?;
        Ok(WhereExpr::Atom(DbWhere { field, op, rhs }))
    }

    pub(super) fn parse_in_value(&mut self) -> Result<Expr> {
        self.parse_at_or_expr()
    }

    /// Parse the RHS of a where comparison / `in` value. `@name` is shorthand
    /// for an `Expr::Var`; `@name.field` extends it to `Expr::FieldGet` so
    /// callers can write `where User.id == @req.userId first;` instead of
    /// staging an extra `let` binding.
    pub(super) fn parse_at_or_expr(&mut self) -> Result<Expr> {
        if !self.check_symbol('@') {
            return self.parse_expr();
        }
        self.bump()?;
        let name = self.expect_ident("expected parameter name after '@'")?;
        if self.check_symbol('.') {
            self.bump()?;
            let field = self.expect_ident("expected field name after '@var.'")?;
            Ok(Expr::FieldGet { var: name, field })
        } else {
            Ok(Expr::Var(name))
        }
    }

    /// Accepts `@param`, integer literal, or any expression — runtime ensures
    /// the value is an integer when binding to LIMIT/OFFSET.
    pub(super) fn parse_db_int_arg(&mut self, clause: &str) -> Result<Expr> {
        if self.check_symbol('@') {
            self.bump()?;
            let name =
                self.expect_ident(&format!("expected parameter name after '@' in {clause}"))?;
            return Ok(Expr::Var(name));
        }
        self.parse_expr()
    }

    /// Parse a comparison operator token sequence: `=`, `==`, `!=`, `<`, `<=`, `>`, `>=`
    pub(super) fn parse_cmp_op(&mut self) -> Result<String> {
        if self.check_symbol('=') {
            self.bump()?;
            if self.check_symbol('=') {
                self.bump()?;
                Ok("==".to_string())
            } else {
                Ok("=".to_string())
            }
        } else if self.check_symbol('!') {
            self.bump()?;
            self.expect_symbol('=')?;
            Ok("!=".to_string())
        } else if self.check_symbol('<') {
            self.bump()?;
            if self.check_symbol('=') {
                self.bump()?;
                Ok("<=".to_string())
            } else {
                Ok("<".to_string())
            }
        } else if self.check_symbol('>') {
            self.bump()?;
            if self.check_symbol('=') {
                self.bump()?;
                Ok(">=".to_string())
            } else {
                Ok(">".to_string())
            }
        } else {
            Err(self.error_here("expected comparison operator in where clause"))
        }
    }
}

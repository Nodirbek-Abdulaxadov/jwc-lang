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

use crate::ast::{
    AggProj, AggregateKind, AliasedCol, DbOrderBy, DbWhere, Expr, JoinClause, SortDir, WhereExpr,
};
use crate::lexer::{Keyword, TemplatePart, TokenKind};

use super::{parse_template_hole, Parser};

/// What `take_agg_head` found. `Ident` carries an already-consumed identifier
/// back to the caller so a column named `count` still parses as a column.
enum AggHead {
    Aggregate(AggregateKind),
    Ident(String),
    NotAggregate,
}

impl<'a> Parser<'a> {
    /// Entry point. Handles the two lowest-precedence forms, both of which
    /// open with `?`: `a ?? b` and `a ? b : c`. They share the token, so one
    /// `?` is consumed here and the next token decides which it was — the
    /// parser carries no lookahead beyond `current`.
    pub(super) fn parse_expr(&mut self) -> Result<Expr> {
        let lhs = self.parse_or_expr()?;
        if !self.check_symbol('?') {
            return Ok(lhs);
        }
        self.expect_symbol('?')?;

        if self.check_symbol('?') {
            self.expect_symbol('?')?;
            // Right-associative, so `a ?? b ?? c` chains as expected.
            let fallback = self.parse_expr()?;
            return Ok(Expr::Coalesce(Box::new(lhs), Box::new(fallback)));
        }

        let then_expr = self.parse_expr()?;
        self.expect_symbol(':')?;
        let else_expr = self.parse_expr()?;
        Ok(Expr::Ternary {
            cond: Box::new(lhs),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        })
    }

    pub(super) fn parse_or_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_and_expr()?;
        loop {
            if self.current.kind == TokenKind::Keyword(Keyword::Or) {
                self.bump()?;
            } else if self.check_symbol('|') {
                // `||`. A lone `|` is not an operator, so requiring the
                // second one here gives a clear error rather than a stray
                // "unexpected symbol" further along.
                self.expect_symbol('|')?;
                self.expect_symbol('|')?;
            } else {
                break;
            }
            let right = self.parse_and_expr()?;
            expr = Expr::Or(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    pub(super) fn parse_and_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_eq_expr()?;
        loop {
            if self.current.kind == TokenKind::Keyword(Keyword::And) {
                self.bump()?;
            } else if self.check_symbol('&') {
                self.expect_symbol('&')?;
                self.expect_symbol('&')?;
            } else {
                break;
            }
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
                // Key is an identifier (`name:`) or a string literal
                // (`"X-Total-Count":`) — the latter lets an object literal carry
                // arbitrary JSON keys (hyphens, spaces, etc.).
                let key = match self.current.kind.clone() {
                    TokenKind::String(s) => {
                        self.bump()?;
                        s
                    }
                    _ => {
                        self.expect_ident("expected key (identifier or string) in object literal")?
                    }
                };
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

    /// Parse `select [distinct] [Entity|*|count(*)] from CTX.TABLE
    ///        [where COND [and|or COND ...]]
    ///        [orderby FIELD [asc|desc]]
    ///        [limit N] [offset N] [first]`
    pub(super) fn parse_select_expr(&mut self) -> Result<Expr> {
        // `select distinct ...`. Read before the scalar-aggregate forms below
        // so `select distinct count(*)` fails on the aggregate rather than on
        // an unexpected `distinct` — `count(distinct col)` is the SQL for that
        // and isn't supported yet.
        let distinct = if self.check_ident_eq("distinct") {
            self.bump()?;
            true
        } else {
            false
        };

        // `count(*)` aggregation form
        if self.check_ident_eq("count") {
            if distinct {
                return Err(self.error_here(
                    "`select distinct count(*)` is not a thing — de-duplicating a \
                     single-row result does nothing. To count distinct values use \
                     `select distinct` with a projection and count the rows.",
                ));
            }
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
            if distinct {
                return Err(self.error_here(
                    "`distinct` is not supported on a scalar aggregate — that would be \
                     SQL's `sum(distinct col)`, which JWC does not emit yet",
                ));
            }
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

        // optional `{ col1, alias: count(*), colName: Entity.col, ... }`
        // projection — plain columns, aliased aggregates, and aliased columns
        // (the last surfaces a column from a joined entity).
        let mut projection: Vec<String> = Vec::new();
        let mut aggregates: Vec<AggProj> = Vec::new();
        let mut aliased_cols: Vec<AliasedCol> = Vec::new();
        if self.check_symbol('{') {
            self.expect_symbol('{')?;
            if entity == "*" {
                return Err(
                    self.error_here("projection `{ ... }` requires a named entity, not '*'")
                );
            }
            if !self.check_symbol('}') {
                loop {
                    let name =
                        self.expect_ident("expected column name or aggregate alias in projection")?;
                    if self.check_symbol(':') {
                        self.expect_symbol(':')?;
                        // `alias: count(*)` / `alias: sum(col)` (aggregate) vs
                        // `alias: Entity.col` (aliased plain column).
                        let head = self.expect_ident("expected aggregate or column after ':'")?;
                        let is_agg = matches!(
                            head.to_lowercase().as_str(),
                            "count" | "sum" | "avg" | "min" | "max"
                        ) && self.check_symbol('(');
                        if is_agg {
                            self.expect_symbol('(')?;
                            let (kind, col) = if head.eq_ignore_ascii_case("count") {
                                self.expect_symbol('*')?;
                                (AggregateKind::Count, "*".to_string())
                            } else if head.eq_ignore_ascii_case("sum") {
                                (AggregateKind::Sum, self.parse_field_path()?)
                            } else if head.eq_ignore_ascii_case("avg") {
                                (AggregateKind::Avg, self.parse_field_path()?)
                            } else if head.eq_ignore_ascii_case("min") {
                                (AggregateKind::Min, self.parse_field_path()?)
                            } else {
                                (AggregateKind::Max, self.parse_field_path()?)
                            };
                            self.expect_symbol(')')?;
                            aggregates.push(AggProj {
                                alias: name,
                                kind,
                                col,
                            });
                        } else {
                            // aliased plain column: `head` or `head.col`
                            let field = if self.check_symbol('.') {
                                self.expect_symbol('.')?;
                                let col = self.expect_ident("expected column name after '.'")?;
                                format!("{}.{}", head, col)
                            } else {
                                head
                            };
                            aliased_cols.push(AliasedCol { alias: name, field });
                        }
                    } else {
                        projection.push(name);
                    }
                    if self.check_symbol(',') {
                        self.expect_symbol(',')?;
                        continue;
                    }
                    break;
                }
            }
            self.expect_symbol('}')?;
            if projection.is_empty() && aggregates.is_empty() && aliased_cols.is_empty() {
                return Err(self.error_here(
                    "projection `{ ... }` must list at least one column or aggregate",
                ));
            }
        }

        // optional `with rel1, rel2, ...`
        // Each relation is a nav name, optionally dotted for one level of
        // nesting (`with boards.columns`). `parse_field_path` already accepts
        // the `a.b` shape, so reuse it.
        let with_relations = if self.check_ident_eq("with") {
            self.bump()?;
            let mut rels = vec![self.parse_field_path()?];
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                rels.push(self.parse_field_path()?);
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

        // optional `join Entity on <field> == <field>` clauses (equi-join).
        let mut joins: Vec<JoinClause> = Vec::new();
        while self.check_ident_eq("join") {
            self.bump()?;
            let join_entity = self.expect_ident("expected entity name after 'join'")?;
            let on_kw = self.expect_ident("expected 'on' after join entity")?;
            if !on_kw.eq_ignore_ascii_case("on") {
                return Err(self.error_here("expected 'on' after join entity"));
            }
            let left = self.parse_field_path()?;
            self.expect_symbol('=')?;
            self.expect_symbol('=')?;
            let right = self.parse_field_path()?;
            joins.push(JoinClause {
                entity: join_entity,
                left,
                right,
            });
        }

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
            Some(Box::new(self.parse_having_or()?))
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
            aggregates,
            aliased_cols,
            joins,
            group_by,
            distinct,
            having,
        })
    }

    pub(super) fn parse_where_or(&mut self) -> Result<WhereExpr> {
        self.parse_where_or_inner(false)
    }

    /// `having` accepts everything `where` does plus aggregate comparisons
    /// (`count(*) > 5`). `where` runs before aggregation, so an aggregate
    /// there is meaningless — hence the flag rather than one shared grammar.
    pub(super) fn parse_having_or(&mut self) -> Result<WhereExpr> {
        self.parse_where_or_inner(true)
    }

    fn parse_where_or_inner(&mut self, allow_agg: bool) -> Result<WhereExpr> {
        let mut left = self.parse_where_and(allow_agg)?;
        while self.current.kind == TokenKind::Keyword(Keyword::Or) {
            self.bump()?;
            let right = self.parse_where_and(allow_agg)?;
            left = WhereExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_where_and(&mut self, allow_agg: bool) -> Result<WhereExpr> {
        let mut left = self.parse_where_atom(allow_agg)?;
        while self.current.kind == TokenKind::Keyword(Keyword::And) {
            self.bump()?;
            let right = self.parse_where_atom(allow_agg)?;
            left = WhereExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_where_atom(&mut self, allow_agg: bool) -> Result<WhereExpr> {
        if self.check_symbol('(') {
            self.expect_symbol('(')?;
            let inner = self.parse_where_or_inner(allow_agg)?;
            self.expect_symbol(')')?;
            return Ok(inner);
        }

        // The parser has no lookahead past `current`, so an aggregate head has
        // to be consumed before we can tell it apart from a field of the same
        // name. When the `(` doesn't follow, hand the identifier back to the
        // field-path parser rather than losing it — a column really can be
        // called `count`.
        let field = match self.take_agg_head(allow_agg)? {
            AggHead::Aggregate(kind) => {
                self.expect_symbol('(')?;
                let col = if kind == AggregateKind::Count {
                    self.expect_symbol('*')?;
                    "*".to_string()
                } else {
                    self.parse_field_path()?
                };
                self.expect_symbol(')')?;
                let op = self.parse_cmp_op()?;
                let rhs = self.parse_where_rhs()?;
                return Ok(WhereExpr::AggAtom { kind, col, op, rhs });
            }
            AggHead::Ident(head) => self.finish_field_path(head)?,
            AggHead::NotAggregate => self.parse_field_path()?,
        };

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
            // `null` is a lexer keyword, not an identifier, so `check_ident_eq`
            // never matched it and `where col is null` has never parsed —
            // despite being documented as a supported operator. Match the
            // keyword; the identifier arm stays for a project that shadows it.
            if self.current.kind != TokenKind::Keyword(Keyword::Null)
                && !self.check_ident_eq("null")
            {
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
        // `op?` — optional predicate: dropped at runtime when the value is "" /
        // null, so one static query serves an optional filter (e.g. `status ==? @s`).
        let op = if self.check_symbol('?') {
            self.expect_symbol('?')?;
            format!("{}?", op)
        } else {
            op
        };

        let rhs = self.parse_at_or_expr()?;
        Ok(WhereExpr::Atom(DbWhere { field, op, rhs }))
    }

    /// Consume an aggregate head (`count` / `sum` / `avg` / `min` / `max`) if
    /// one is next and aggregates are legal here.
    ///
    /// The identifier is consumed before we can see whether a `(` follows, so
    /// the non-aggregate answer carries it back out for the caller to finish
    /// as a field path. Without that, `where count > 5` on a column actually
    /// named `count` would break.
    fn take_agg_head(&mut self, allow_agg: bool) -> Result<AggHead> {
        if !allow_agg {
            return Ok(AggHead::NotAggregate);
        }
        let kind = if self.check_ident_eq("count") {
            AggregateKind::Count
        } else if self.check_ident_eq("sum") {
            AggregateKind::Sum
        } else if self.check_ident_eq("avg") {
            AggregateKind::Avg
        } else if self.check_ident_eq("min") {
            AggregateKind::Min
        } else if self.check_ident_eq("max") {
            AggregateKind::Max
        } else {
            return Ok(AggHead::NotAggregate);
        };
        let head = self.expect_ident("expected aggregate name")?;
        if self.check_symbol('(') {
            Ok(AggHead::Aggregate(kind))
        } else {
            Ok(AggHead::Ident(head))
        }
    }

    /// Finish a field path whose first identifier is already consumed.
    fn finish_field_path(&mut self, first: String) -> Result<String> {
        if self.check_symbol('.') {
            self.bump()?;
            let second = self.expect_ident("expected field name after '.'")?;
            Ok(format!("{}.{}", first, second))
        } else {
            Ok(first)
        }
    }

    /// RHS of an aggregate comparison. Aggregates compare against a value, so
    /// unlike a column term there is no `like` / `is null` / `op?` form here.
    fn parse_where_rhs(&mut self) -> Result<Expr> {
        self.parse_at_or_expr()
    }

    pub(super) fn parse_in_value(&mut self) -> Result<Expr> {
        self.parse_at_or_expr()
    }

    /// Parse the RHS of a where comparison / `in` value. `@name` is shorthand
    /// for an `Expr::Var`; `@name.field` extends it to `Expr::FieldGet` so
    /// callers can write `where User.id == @req.userId first;` instead of
    /// staging an extra `let` binding.
    ///
    /// The fallback parses at **additive** precedence, not full-expression
    /// precedence. A comparison's right side is an arithmetic expression;
    /// `and` / `or` belong to the surrounding WHERE tree. Handing the RHS to
    /// `parse_expr` let it swallow them, and the second predicate then
    /// vanished from the emitted SQL:
    ///
    /// ```text
    /// where Sale.amount > 2 and Sale.id < 100
    ///   ->  SELECT * FROM "sale" WHERE "amount" > $1
    ///       $1 = (2 and (Sale.id < 100))
    /// ```
    ///
    /// Nothing errored — the filter just silently returned rows it should
    /// have excluded. Only literal right-hand sides reached it; the common
    /// `== @param` form returns from the branch above before `parse_expr` is
    /// ever called, which is why this survived.
    pub(super) fn parse_at_or_expr(&mut self) -> Result<Expr> {
        if !self.check_symbol('@') {
            return self.parse_add_expr();
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

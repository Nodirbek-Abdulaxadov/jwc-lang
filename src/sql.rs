//! `insert` / `update` / `delete`, and the pieces every statement shares.
//!
//! `select` lives in [`crate::query_sql`]: the join compiler subsumes the
//! single-table case, so the one that used to be here was a second
//! implementation of the same thing — and a second one is how the two
//! drift. Writes stay here because writes.md scopes them to one table.
//!
//! Two properties are load-bearing:
//!
//! * **Every value is a bind parameter.** Nothing is interpolated, ever.
//!   Parameters are bound as text and cast in SQL — `($1::text)::bigint`,
//!   not `$1::bigint`, which makes Postgres infer the column's type for
//!   the parameter and then refuse the text the runtime sends.
//! * **The raw path is a compiled projection** (queries.md §7.2). It emits
//!   `json_build_object` with `private` columns dropped and `bigint` /
//!   `numeric` cast to text, so the raw and record paths agree on the wire
//!   (types.md §2.3) — it is not `row_to_json`.

use crate::ast::*;
use crate::model::{ColumnObj, SchemaModel, SqlType, TableObj};
use crate::naming::quote_ident;

/// A statement plus its bind parameters, in order.
pub struct Built {
    pub sql: String,
    /// Each parameter is the expression to evaluate and the Postgres type
    /// to cast the bound text to.
    pub params: Vec<Param>,
    pub shape: Shape,
    /// A query with `as { }` produces a **record**, which the runtime
    /// parses so fields can be read. Without it the result stays `Raw` and
    /// is forwarded to the response without ever being parsed — that is the
    /// performance promise (types.md §5.1).
    pub record: bool,
    /// The projected field names, in projection order. The record path
    /// rebuilds in this order because a JSON object parsed into a sorted
    /// map would come back alphabetised, and the projection order **is**
    /// the key order (queries.md §6.1).
    pub fields: Vec<String>,
    /// Set on a `page` query: the envelope the runtime builds around the
    /// rows (queries.md §9.3).
    pub page: Option<PagePlan>,
}

/// What the runtime needs to turn a page of rows into `{items, next,
/// has_more}`.
pub struct PagePlan {
    /// `after <cursor>` — the caller's cursor, if the query takes one.
    pub after: Option<Expr>,
    /// The requested size, before clamping.
    pub size: Expr,
    /// The clamp: `max m`, else `server { max_page_size }`.
    pub max: i64,
    /// True when `items` stays an unparsed JSON fragment (types.md §5.4).
    pub raw_items: bool,
}

/// The right-hand side of a `set`.
pub enum SetValue {
    /// A value the interpreter computed: bound as a parameter.
    Bound(Expr),
    /// An expression over the row's own columns: emitted as SQL.
    Sql(Expr),
}

pub struct Param {
    pub bind: Bind,
    pub cast: String,
}

/// Where a parameter's value comes from.
///
/// Named rather than positional: the SET clause's values used to be "the
/// first N parameters", which held only while nothing else could bind one
/// — and `set value = value + 1` binds the `1`.
pub enum Bind {
    /// Evaluate this expression at bind time.
    Expr(Expr),
    /// Take the next value the caller computed before building the
    /// statement (an evaluated `set`, whose side effects must not repeat).
    Preset,
    /// The i-th component of the caller's keyset cursor, or NULL when
    /// there is no cursor (queries.md §9.2).
    Cursor(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// A JSON array of rows.
    Rows,
    /// One row or SQL NULL.
    First,
    /// No result.
    None,
}

pub struct Builder<'a> {
    model: &'a SchemaModel,
    params: Vec<Param>,
}

impl<'a> Builder<'a> {
    pub fn new(model: &'a SchemaModel) -> Self {
        Self {
            model,
            params: Vec::new(),
        }
    }

    fn table(&self, q: &QualifiedTable) -> Option<&'a TableObj> {
        self.model
            .tables
            .iter()
            .find(|t| t.schema == q.schema.name && t.declared == q.object.name)
    }

    /// Every parameter is bound as **text** and cast in SQL. The `::text`
    /// hop matters: `$1::bigint` makes Postgres infer `$1` as `bigint` and
    /// then refuse a string, while `($1::text)::bigint` infers `text` and
    /// casts on the server. One binding path, every type.
    fn bind(&mut self, e: &Expr, cast: &str) -> String {
        self.params.push(Param {
            bind: Bind::Expr(e.clone()),
            cast: cast.to_string(),
        });
        format!("(${}::text)::{cast}", self.params.len())
    }

    /// A parameter whose value the caller already computed.
    fn preset(&mut self, cast: &str) -> String {
        self.params.push(Param {
            bind: Bind::Preset,
            cast: cast.to_string(),
        });
        format!("(${}::text)::{cast}", self.params.len())
    }

    /// An arithmetic expression over the row's own columns. Unqualified,
    /// because inside `SET` an unqualified name is the target table's.
    fn set_expr(&mut self, t: &TableObj, e: &Expr, cast: &str) -> Option<String> {
        Some(match &*e.kind {
            ExprKind::Name(n) => quote_ident(&t.column(&n.name)?.physical),
            ExprKind::Binary { op, lhs, rhs } => {
                let sql_op = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Rem => "%",
                    _ => return None,
                };
                let a = self.set_expr(t, lhs, cast)?;
                let b = self.set_expr(t, rhs, cast)?;
                format!("({a} {sql_op} {b})")
            }
            // Anything that is not a column is still a bind parameter.
            _ => self.bind(e, cast),
        })
    }

    /// The projected field names, in order.
    fn field_names(&self, t: &TableObj, proj: Option<&ObjectShape>) -> Vec<String> {
        match proj {
            Some(p) => p
                .fields
                .iter()
                .filter_map(|f| match f {
                    ProjField::Column(i) => Some(i.name.clone()),
                    ProjField::Expr { alias, .. } => Some(alias.name.clone()),
                    ProjField::Nested { .. } => None,
                })
                .collect(),
            None => t
                .columns
                .iter()
                .filter(|c| !c.private)
                .map(|c| c.declared.clone())
                .collect(),
        }
    }

    /// queries.md §7.2 — raw is a compiled projection, not `row_to_json`.
    fn projection(&self, t: &TableObj, proj: Option<&ObjectShape>, alias: &str) -> String {
        let entries: Vec<String> = match proj {
            Some(p) => p
                .fields
                .iter()
                .filter_map(|f| match f {
                    ProjField::Column(i) => {
                        let c = t.column(&i.name)?;
                        Some(self.json_entry(&i.name, c, alias))
                    }
                    ProjField::Expr {
                        alias: a, value, ..
                    } => {
                        // Only a bare column alias is expressible without the
                        // query compiler; anything else waits for v0.25.0.
                        if let ExprKind::Name(n) = &*value.kind {
                            let c = t.column(&n.name)?;
                            Some(self.json_entry(&a.name, c, alias))
                        } else {
                            None
                        }
                    }
                    ProjField::Nested { .. } => None,
                })
                .collect(),
            // The default result excludes `private` columns (schema.md §3.1):
            // "raw by default" and "private never leaves the database" are
            // both true because raw is compiled.
            None => t
                .columns
                .iter()
                .filter(|c| !c.private)
                .map(|c| self.json_entry(&c.declared, c, alias))
                .collect(),
        };
        format!("json_build_object({})", entries.join(", "))
    }

    fn json_entry(&self, key: &str, c: &ColumnObj, alias: &str) -> String {
        let col = format!("{alias}.{}", quote_ident(&c.physical));
        // types.md §2.3 — bigint and numeric go out as JSON strings, on the
        // raw path and the record path alike.
        let value = match crate::ddl::wire_cast(&c.ty) {
            Some(cast) => format!("{col}::{cast}"),
            None => col,
        };
        format!("'{}', {value}", key.replace('\'', "''"))
    }

    fn order_by(&mut self, t: &TableObj, keys: &[SortKey], alias: &str) -> String {
        keys.iter()
            .filter_map(|k| {
                let name = match &*k.expr.kind {
                    ExprKind::Name(n) => n.name.clone(),
                    ExprKind::Field { field, .. } => field.name.clone(),
                    _ => return None,
                };
                let c = t.column(&name)?;
                let mut s = format!("{alias}.{}", quote_ident(&c.physical));
                if k.desc {
                    s.push_str(" DESC");
                }
                match k.nulls {
                    Some(NullsOrder::First) => s.push_str(" NULLS FIRST"),
                    Some(NullsOrder::Last) => s.push_str(" NULLS LAST"),
                    None => {}
                }
                Some(s)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A `where` predicate. Returns `None` for a construct the single-table
    /// builder cannot express, so the caller can report it rather than emit
    /// something approximate.
    fn predicate(&mut self, t: &TableObj, e: &Expr, alias: &str) -> Option<String> {
        Some(match &*e.kind {
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::And | BinOp::Or => {
                    let a = self.predicate(t, lhs, alias)?;
                    let b = self.predicate(t, rhs, alias)?;
                    let sep = if matches!(op, BinOp::And) {
                        "AND"
                    } else {
                        "OR"
                    };
                    format!("({a}) {sep} ({b})")
                }
                BinOp::EqOpt => {
                    // queries.md §3.2 — the predicate is dropped when the
                    // value is absent, so it compiles to a guard on the
                    // parameter rather than to `IS NULL`.
                    let col = self.column_ref(t, lhs, alias)?;
                    let cast = self.cast_for(t, lhs)?;
                    let p = self.bind(rhs, &cast);
                    let n = self.params.len();
                    format!("(${n} IS NULL OR {col} = {p})")
                }
                _ => {
                    if matches!(&*rhs.kind, ExprKind::Null) {
                        let col = self.column_ref(t, lhs, alias)?;
                        return Some(match op {
                            BinOp::Eq => format!("{col} IS NULL"),
                            BinOp::Ne => format!("{col} IS NOT NULL"),
                            _ => return None,
                        });
                    }
                    let col = self.column_ref(t, lhs, alias)?;
                    let cast = self.cast_for(t, lhs)?;
                    let sql_op = match op {
                        BinOp::Eq => "=",
                        BinOp::Ne => "<>",
                        BinOp::Lt => "<",
                        BinOp::Le => "<=",
                        BinOp::Gt => ">",
                        BinOp::Ge => ">=",
                        BinOp::Like => "LIKE",
                        BinOp::ILike => "ILIKE",
                        _ => return None,
                    };
                    let p = self.bind(rhs, &cast);
                    format!("{col} {sql_op} {p}")
                }
            },
            ExprKind::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            _ => return None,
        })
    }

    fn column_ref(&self, t: &TableObj, e: &Expr, alias: &str) -> Option<String> {
        let name = match &*e.kind {
            ExprKind::Name(n) => n.name.clone(),
            ExprKind::Field { field, base } => match &*base.kind {
                ExprKind::Name(_) => field.name.clone(),
                _ => return None,
            },
            _ => return None,
        };
        let c = t.column(&name)?;
        Some(format!("{alias}.{}", quote_ident(&c.physical)))
    }

    fn cast_for(&self, t: &TableObj, e: &Expr) -> Option<String> {
        let name = match &*e.kind {
            ExprKind::Name(n) => n.name.clone(),
            ExprKind::Field { field, .. } => field.name.clone(),
            _ => return None,
        };
        let c = t.column(&name)?;
        Some(pg_type(&c.ty))
    }

    // ------------------------------------------------------------ insert

    pub fn insert(&mut self, i: &InsertExpr, fields: &[(String, Expr)]) -> Option<Built> {
        let t = self.table(&i.table)?;
        let mut cols = Vec::new();
        let mut vals = Vec::new();
        for (name, value) in fields {
            let Some(c) = t.column(name) else { continue };
            cols.push(quote_ident(&c.physical));
            vals.push(self.bind(value, &pg_type(&c.ty)));
        }

        let mut sql = if cols.is_empty() {
            format!("INSERT INTO {} DEFAULT VALUES", t.qualified())
        } else {
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                t.qualified(),
                cols.join(", "),
                vals.join(", ")
            )
        };

        if let Some(c) = &i.conflict {
            let target = if c.columns.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = c
                    .columns
                    .iter()
                    .filter_map(|n| t.column(&n.name).map(|c| quote_ident(&c.physical)))
                    .collect();
                format!(" ({})", names.join(", "))
            };
            match &c.action {
                ConflictAction::DoNothing => {
                    sql.push_str(&format!(" ON CONFLICT{target} DO NOTHING"))
                }
                ConflictAction::DoUpdate(_) => return None,
            }
        }

        let shape = match &i.projection {
            Some(p) => {
                sql.push_str(&format!(
                    " RETURNING {}::text",
                    self.projection(t, Some(p), &t.physical_alias())
                ));
                // `on conflict do nothing` may return no row (writes.md §2.3).
                Shape::First
            }
            None => Shape::None,
        };
        Some(Built {
            sql,
            params: std::mem::take(&mut self.params),
            shape,
            record: i.projection.is_some(),
            fields: self.field_names(t, i.projection.as_ref()),
            page: None,
        })
    }

    // ------------------------------------------------------------ update

    pub fn update(&mut self, u: &UpdateExpr, sets: &[(String, SetValue)]) -> Option<Built> {
        let t = self.table(&u.table)?;
        let mut assigns = Vec::new();
        for (name, value) in sets {
            let Some(c) = t.column(name) else { continue };
            let rhs = match value {
                SetValue::Bound(_) => self.preset(&pg_type(&c.ty)),
                // `set value = value + 1` reads the row it is writing, so
                // the increment has to happen in the database. Evaluating
                // it in the process would need a read first, and two
                // callers doing that both read the same number.
                SetValue::Sql(e) => self.set_expr(t, e, &pg_type(&c.ty))?,
            };
            assigns.push(format!("{} = {rhs}", quote_ident(&c.physical)));
        }
        if assigns.is_empty() {
            return None;
        }

        let mut sql = format!("UPDATE {} x SET {}", t.qualified(), assigns.join(", "));

        if u.first {
            // writes.md §4 — `first` lowers to a locked row selection.
            // `FOR UPDATE` is always emitted: without it two concurrent
            // callers both select the same row and both write.
            let mut sub = format!("SELECT y.ctid FROM {} y", t.qualified());
            if let Some(f) = &u.filter {
                sub.push_str(&format!(" WHERE {}", self.predicate(t, f, "y")?));
            }
            if !u.order_by.is_empty() {
                sub.push_str(&format!(" ORDER BY {}", self.order_by(t, &u.order_by, "y")));
            } else if let Some(pk) = &t.primary_key {
                let cols: Vec<String> = pk
                    .columns
                    .iter()
                    .map(|c| format!("y.{}", quote_ident(c)))
                    .collect();
                sub.push_str(&format!(" ORDER BY {}", cols.join(", ")));
            }
            sub.push_str(" FOR UPDATE LIMIT 1");
            sql.push_str(&format!(" WHERE x.ctid = ({sub})"));
        } else if let Some(f) = &u.filter {
            sql.push_str(&format!(" WHERE {}", self.predicate(t, f, "x")?));
        }

        let shape = match &u.projection {
            Some(p) => {
                sql.push_str(&format!(
                    " RETURNING {}::text",
                    self.projection(t, Some(p), "x")
                ));
                if u.first {
                    Shape::First
                } else {
                    Shape::Rows
                }
            }
            None => Shape::None,
        };
        Some(Built {
            sql,
            params: std::mem::take(&mut self.params),
            shape,
            record: u.projection.is_some(),
            fields: self.field_names(t, u.projection.as_ref()),
            page: None,
        })
    }

    // ------------------------------------------------------------ delete

    pub fn delete(&mut self, d: &DeleteExpr) -> Option<Built> {
        let t = self.table(&d.table)?;
        let mut sql = format!("DELETE FROM {} x", t.qualified());

        if d.first {
            let mut sub = format!("SELECT y.ctid FROM {} y", t.qualified());
            if let Some(f) = &d.filter {
                sub.push_str(&format!(" WHERE {}", self.predicate(t, f, "y")?));
            }
            if !d.order_by.is_empty() {
                sub.push_str(&format!(" ORDER BY {}", self.order_by(t, &d.order_by, "y")));
            } else if let Some(pk) = &t.primary_key {
                let cols: Vec<String> = pk
                    .columns
                    .iter()
                    .map(|c| format!("y.{}", quote_ident(c)))
                    .collect();
                sub.push_str(&format!(" ORDER BY {}", cols.join(", ")));
            }
            sub.push_str(" FOR UPDATE LIMIT 1");
            sql.push_str(&format!(" WHERE x.ctid = ({sub})"));
        } else if let Some(f) = &d.filter {
            sql.push_str(&format!(" WHERE {}", self.predicate(t, f, "x")?));
        }

        let shape = match &d.projection {
            Some(p) => {
                sql.push_str(&format!(
                    " RETURNING {}::text",
                    self.projection(t, Some(p), "x")
                ));
                if d.first {
                    Shape::First
                } else {
                    Shape::Rows
                }
            }
            None => Shape::None,
        };
        Some(Built {
            sql,
            params: std::mem::take(&mut self.params),
            shape,
            record: d.projection.is_some(),
            fields: self.field_names(t, d.projection.as_ref()),
            page: None,
        })
    }
}

impl TableObj {
    /// The alias `RETURNING` sees. Postgres exposes the target table under
    /// its own name there unless the statement aliased it.
    ///
    /// Quoted, because that name can be a reserved word. A table declared
    /// `as "user"` produced `RETURNING json_build_object('id', user.id)`,
    /// where `user` is the SQL `USER` function and the parser stops at the
    /// dot: "syntax error at or near `.`". Every read path already went
    /// through `quote_ident`; only this one did not, so the failure needed
    /// a write, a `RETURNING` projection, and a reserved physical name all
    /// at once — which is an ordinary combination for a ported schema.
    fn physical_alias(&self) -> String {
        quote_ident(&self.physical)
    }
}

/// The Postgres type a bound parameter is cast to.
pub fn pg_type(t: &SqlType) -> String {
    match t {
        SqlType::Scalar(s) => s.clone(),
        SqlType::Enum { qualified, .. } => qualified.clone(),
        SqlType::EnumInline { width, .. } => format!("varchar({width})"),
        SqlType::Array(inner) => format!("{}[]", pg_type(inner)),
    }
}

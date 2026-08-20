//! SQL emission for a join tree.
//!
//! Reads the plan from [`super::query`] and produces one statement whose
//! single text column holds the whole result. The two shapes that matter:
//!
//! * **`as one` under a `left join`** becomes a plain `LEFT JOIN` plus
//!   `CASE WHEN <child pk> IS NULL THEN NULL ELSE json_build_object(…) END`
//!   — a **null object**, not an object of nulls (#3). Under an `inner
//!   join` the CASE is omitted, because it cannot miss.
//! * **`as many`** becomes `LEFT JOIN LATERAL`, with the child's `orderby`
//!   and `limit` applied to the **rows** and the aggregation wrapped around
//!   them. Not `json_agg(… ORDER BY …)`: that form can order but cannot
//!   bound, and a collection with no bound is the thing #44 is about.

use super::ast::*;
use super::model::{SchemaModel, TableObj};
use super::naming::quote_ident;
use super::query::{Node, Plan};
use super::sql::{Param, Shape};
use std::collections::HashMap;

pub struct Compiled {
    pub sql: String,
    pub params: Vec<Param>,
    pub shape: Shape,
    pub record: bool,
    pub fields: Vec<String>,
}

pub struct Compiler<'a> {
    model: &'a SchemaModel,
    params: Vec<Param>,
    /// JWC binding alias -> SQL alias. Generated rather than reused so a
    /// binding named `user` or `order` cannot collide with SQL.
    aliases: HashMap<String, String>,
    /// JWC binding alias -> declared table or view name.
    binding_objects: HashMap<String, String>,
    next: usize,
    /// Why emission gave up, when it did. Named at the point of failure —
    /// "not expressible" told a reader nothing about which release to wait
    /// for, and the compiler is the only thing that knows.
    gap: Option<String>,
}

impl<'a> Compiler<'a> {
    pub fn new(model: &'a SchemaModel) -> Self {
        Self {
            model,
            params: Vec::new(),
            aliases: HashMap::new(),
            binding_objects: HashMap::new(),
            next: 0,
            gap: None,
        }
    }

    /// The reason the last `compile` returned `None`.
    pub fn gap(&self) -> &str {
        self.gap.as_deref().unwrap_or("this query is not expressible yet")
    }

    /// Give up, saying why. The first reason wins: it is the innermost one,
    /// and the outer frames only report that a child failed.
    fn unsupported<T>(&mut self, reason: &str) -> Option<T> {
        if self.gap.is_none() {
            self.gap = Some(reason.to_string());
        }
        None
    }

    fn sql_alias(&mut self, jwc_alias: &str) -> String {
        if let Some(a) = self.aliases.get(jwc_alias) {
            return a.clone();
        }
        let a = format!("t{}", self.next);
        self.next += 1;
        self.aliases.insert(jwc_alias.to_string(), a.clone());
        a
    }

    fn table(&mut self, object: &str) -> Option<&'a TableObj> {
        match self.model.tables.iter().find(|t| t.declared == object) {
            Some(t) => Some(t),
            // A view is a name the planner resolves but the emitter has no
            // relation for yet.
            None => self.unsupported(&format!(
                "`{object}` is a view; selecting from a view arrives in v0.25.d"
            )),
        }
    }

    fn table_of(&self, object: &str) -> Option<&'a TableObj> {
        self.model.tables.iter().find(|t| t.declared == object)
    }

    /// Every parameter is bound as text and cast in SQL; see `sql.rs`.
    fn bind(&mut self, e: &Expr, cast: &str) -> String {
        self.params.push(Param {
            expr: e.clone(),
            cast: cast.to_string(),
        });
        format!("(${}::text)::{cast}", self.params.len())
    }

    // ------------------------------------------------------------ entry

    pub fn compile(&mut self, select: &SelectExpr, plan: &Plan) -> Option<Compiled> {
        // Aliases are assigned in tree order so the output is stable.
        let mut all = Vec::new();
        plan.root.walk(&mut all);
        for n in &all {
            self.sql_alias(&n.alias);
            self.binding_objects
                .insert(n.alias.clone(), n.object.clone());
        }
        for g in &plan.groups {
            self.sql_alias(&g.alias);
            self.binding_objects
                .insert(g.alias.clone(), g.object.clone());
        }

        let projection = select.projection.as_ref();
        let root_alias = self.sql_alias(&plan.root.alias);
        let root_table = self.table(&plan.root.object)?;

        let (json, joins) = self.emit(&plan.root, projection)?;

        let mut from = format!("{} {root_alias}", root_table.qualified());
        from.push_str(&joins);
        for g in &plan.groups {
            let ga = self.sql_alias(&g.alias);
            let gt = self.table(&g.object)?;
            let on = self.predicate(&g.on, &g.alias)?;
            let kind = match g.kind {
                JoinKind::Left => "LEFT JOIN",
                JoinKind::Inner => "JOIN",
            };
            from.push_str(&format!("\n  {kind} {} {ga} ON {on}", gt.qualified()));
            if let Some(f) = &g.filter {
                let extra = self.predicate(f, &g.alias)?;
                from.push_str(&format!(" AND {extra}"));
            }
        }

        let mut sql = format!("SELECT {json} AS j\n  FROM {from}");
        if let Some(f) = &select.filter {
            sql.push_str(&format!("\n  WHERE {}", self.predicate(f, &plan.root.alias)?));
        }
        if !select.group_by.is_empty() {
            let mut cols = Vec::new();
            for g in &select.group_by {
                cols.push(self.column_ref(g, &plan.root.alias)?);
            }
            sql.push_str(&format!("\n  GROUP BY {}", cols.join(", ")));
        }
        if let Some(h) = &select.having {
            sql.push_str(&format!("\n  HAVING {}", self.predicate(h, &plan.root.alias)?));
        }
        if !select.order_by.is_empty() {
            sql.push_str(&format!(
                "\n  ORDER BY {}",
                self.order_by(&select.order_by, &plan.root.alias)?
            ));
        }

        let shape = if select.first {
            sql.push_str("\n  LIMIT 1");
            Shape::First
        } else {
            if let Some(l) = &select.limit {
                let p = self.bind(l, "int");
                sql.push_str(&format!("\n  LIMIT {p}"));
            }
            Shape::Rows
        };

        let wrapped = match shape {
            Shape::Rows => {
                format!("SELECT coalesce(json_agg(q.j), '[]'::json)::text FROM ({sql}) q")
            }
            _ => format!("SELECT q.j::text FROM ({sql}) q"),
        };

        let fields = match projection {
            Some(p) => field_names(p),
            None => root_table
                .columns
                .iter()
                .filter(|c| !c.private)
                .map(|c| c.declared.clone())
                .collect(),
        };

        Some(Compiled {
            sql: wrapped,
            params: std::mem::take(&mut self.params),
            shape,
            record: projection.is_some(),
            fields,
        })
    }

    // ------------------------------------------------------------ tree

    /// One recursive pass: the node's JSON object, and the FROM clauses its
    /// children contribute. A `many` child's lateral needs that child's own
    /// JSON, so building both together is what keeps it single-pass.
    fn emit(
        &mut self,
        node: &Node,
        projection: Option<&ObjectShape>,
    ) -> Option<(String, String)> {
        let alias = self.sql_alias(&node.alias);
        let table = self.table(&node.object)?;
        let mut entries: Vec<String> = Vec::new();
        let mut joins = String::new();

        match projection {
            Some(p) => {
                for f in &p.fields {
                    match f {
                        ProjField::Column(i) => {
                            let c = table.column(&i.name)?;
                            entries.push(json_entry(&i.name, &alias, c));
                        }
                        ProjField::Expr { alias: a, value, .. } => {
                            let sql = self.value_expr(value, &node.alias)?;
                            entries.push(format!("'{}', {sql}", escape(&a.name)));
                        }
                        ProjField::Nested { alias: a, shape, .. } => {
                            let child = node
                                .children
                                .iter()
                                .find(|c| c.link.as_ref().is_some_and(|l| l.field == a.name))?;
                            let (value, from) = self.child(child, shape)?;
                            entries.push(format!("'{}', {value}", escape(&a.name)));
                            joins.push_str(&from);
                        }
                    }
                }
            }
            // The default result excludes `private` columns (schema.md §3.1).
            None => {
                for c in table.columns.iter().filter(|c| !c.private) {
                    entries.push(json_entry(&c.declared, &alias, c));
                }
            }
        }

        Some((
            format!("json_build_object({})", entries.join(", ")),
            joins,
        ))
    }

    /// A nested projection field: its value expression and the FROM it needs.
    fn child(&mut self, child: &Node, shape: &ObjectShape) -> Option<(String, String)> {
        let link = child.link.as_ref()?;
        let alias = self.sql_alias(&child.alias);
        let table = self.table(&child.object)?;
        let (json, inner_joins) = self.emit(child, Some(shape))?;

        match link.cardinality {
            Cardinality::One => {
                let on = self.predicate(&link.on, &child.alias)?;
                let kind = match link.kind {
                    JoinKind::Left => "LEFT JOIN",
                    JoinKind::Inner => "JOIN",
                };
                let mut from =
                    format!("\n  {kind} {} {alias} ON {on}", table.qualified());
                if let Some(f) = &link.filter {
                    from.push_str(&format!(" AND {}", self.predicate(f, &child.alias)?));
                }
                from.push_str(&inner_joins);

                let value = if link.kind == JoinKind::Inner {
                    // An inner join cannot miss, so there is nothing to guard.
                    json
                } else {
                    // #3 — a null object, not an object of nulls. The guard
                    // is the child's primary key: NOT NULL when the row
                    // exists, NULL exactly when the LEFT JOIN found nothing.
                    let guard = table
                        .primary_key
                        .as_ref()
                        .and_then(|pk| pk.columns.first())
                        .map(|c| format!("{alias}.{}", quote_ident(c)))?;
                    format!("CASE WHEN {guard} IS NULL THEN NULL ELSE {json} END")
                };
                Some((value, from))
            }
            Cardinality::Many => {
                let agg = format!("{alias}_agg");
                let mut inner = format!(
                    "SELECT {json} AS j\n        FROM {} {alias}{}",
                    table.qualified(),
                    indent(&inner_joins)
                );
                let mut wheres = vec![self.predicate(&link.on, &child.alias)?];
                if let Some(f) = &link.filter {
                    wheres.push(self.predicate(f, &child.alias)?);
                }
                inner.push_str(&format!("\n        WHERE {}", wheres.join(" AND ")));
                if !link.order_by.is_empty() {
                    inner.push_str(&format!(
                        "\n        ORDER BY {}",
                        self.order_by(&link.order_by, &child.alias)?
                    ));
                }
                if let Some(l) = &link.limit {
                    let p = self.bind(l, "int");
                    inner.push_str(&format!("\n        LIMIT {p}"));
                }
                let from = format!(
                    "\n  LEFT JOIN LATERAL (\n      SELECT coalesce(json_agg(c.j), '[]'::json) AS data\n        FROM ({inner}) c\n  ) {agg} ON true"
                );
                Some((format!("coalesce({agg}.data, '[]'::json)"), from))
            }
            Cardinality::Group => self.unsupported(
                "`as group` is an aggregate join; aggregates arrive in v0.25.c",
            ),
        }
    }

    // ------------------------------------------------------------ exprs

    fn order_by(&mut self, keys: &[SortKey], scope: &str) -> Option<String> {
        let mut out = Vec::new();
        for k in keys {
            let mut s = self.column_ref(&k.expr, scope)?;
            if k.desc {
                s.push_str(" DESC");
            }
            match k.nulls {
                Some(NullsOrder::First) => s.push_str(" NULLS FIRST"),
                Some(NullsOrder::Last) => s.push_str(" NULLS LAST"),
                None => {}
            }
            out.push(s);
        }
        Some(out.join(", "))
    }

    /// A column reference: `alias.physical`. `scope` is the binding an
    /// unqualified name belongs to.
    fn column_ref(&mut self, e: &Expr, scope: &str) -> Option<String> {
        match &*e.kind {
            ExprKind::Name(n) => {
                let alias = self.sql_alias(scope);
                let object = self.object_of(scope)?;
                let table = self.table(&object)?;
                let c = table.column(&n.name)?;
                Some(format!("{alias}.{}", quote_ident(&c.physical)))
            }
            ExprKind::Field { base, field } => {
                let ExprKind::Name(b) = &*base.kind else {
                    return None;
                };
                let object = self.object_of(&b.name)?;
                let alias = self.sql_alias(&b.name);
                let table = self.table(&object)?;
                let c = table.column(&field.name)?;
                Some(format!("{alias}.{}", quote_ident(&c.physical)))
            }
            _ => None,
        }
    }

    fn object_of(&self, jwc_alias: &str) -> Option<String> {
        self.binding_objects.get(jwc_alias).cloned()
    }

    fn value_expr(&mut self, e: &Expr, scope: &str) -> Option<String> {
        if let Some(c) = self.column_ref(e, scope) {
            // A projected column still needs its wire cast (types.md §2.3).
            if let Some(cast) = self.cast_of(e, scope) {
                return Some(format!("{c}::{cast}"));
            }
            return Some(c);
        }
        match &*e.kind {
            ExprKind::Call { .. } => self.aggregate(e, scope),
            _ => self.unsupported("a projection field is a column or an aggregate"),
        }
    }

    fn cast_of(&self, e: &Expr, scope: &str) -> Option<&'static str> {
        let (object, name) = match &*e.kind {
            ExprKind::Name(n) => (self.object_of(scope)?, n.name.clone()),
            ExprKind::Field { base, field } => match &*base.kind {
                ExprKind::Name(b) => (self.object_of(&b.name)?, field.name.clone()),
                _ => return None,
            },
            _ => return None,
        };
        let t = self.table_of(&object)?;
        let c = t.column(&name)?;
        super::ddl::wire_cast(&c.ty)
    }

    fn predicate(&mut self, e: &Expr, scope: &str) -> Option<String> {
        Some(match &*e.kind {
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::And | BinOp::Or => {
                    let a = self.predicate(lhs, scope)?;
                    let b = self.predicate(rhs, scope)?;
                    let sep = if matches!(op, BinOp::And) { "AND" } else { "OR" };
                    format!("({a}) {sep} ({b})")
                }
                BinOp::EqOpt => {
                    let col = self.column_ref(lhs, scope)?;
                    let cast = self.param_cast(lhs, scope)?;
                    let p = self.bind(rhs, &cast);
                    let n = self.params.len();
                    format!("(${n} IS NULL OR {col} = {p})")
                }
                _ => {
                    if matches!(&*rhs.kind, ExprKind::Null) {
                        let col = self.column_ref(lhs, scope)?;
                        return Some(match op {
                            BinOp::Eq => format!("{col} IS NULL"),
                            BinOp::Ne => format!("{col} IS NOT NULL"),
                            _ => return None,
                        });
                    }
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
                    let (col, cast) = self.operand(lhs, scope)?;
                    // Both sides columns: a join condition, no parameter.
                    if let Some(right) = self.column_ref(rhs, scope) {
                        return Some(format!("{col} {sql_op} {right}"));
                    }
                    let p = self.bind(rhs, &cast);
                    format!("{col} {sql_op} {p}")
                }
            },
            ExprKind::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            _ => return None,
        })
    }

    /// The left side of a comparison: a column, or — in `having` — an
    /// aggregate. Returns the SQL and the Postgres type a bound parameter
    /// on the other side must be cast to.
    fn operand(&mut self, e: &Expr, scope: &str) -> Option<(String, String)> {
        if let Some(c) = self.column_ref(e, scope) {
            return Some((c, self.param_cast(e, scope)?));
        }
        let sql = self.aggregate(e, scope)?;
        let ty = self.aggregate_type(e, scope)?;
        Some((sql, ty))
    }

    /// The Postgres type an aggregate comes back as, for casting the
    /// literal it is compared against in `having`.
    fn aggregate_type(&self, e: &Expr, scope: &str) -> Option<String> {
        let ExprKind::Call { callee, args, .. } = &*e.kind else {
            return None;
        };
        let name = match &*callee.kind {
            ExprKind::Name(n) => n.name.as_str(),
            // `count.distinct`
            ExprKind::Field { .. } => "count",
            _ => return None,
        };
        Some(match name {
            "count" => "bigint".to_string(),
            "sum" | "avg" => match self.column_scalar(args.first()?, scope)?.as_str() {
                "smallint" | "int" | "integer" if name == "sum" => "bigint".to_string(),
                _ => "numeric".to_string(),
            },
            // `min` / `max` keep the operand's type.
            _ => self.param_cast(args.first()?, scope)?,
        })
    }

    fn param_cast(&self, e: &Expr, scope: &str) -> Option<String> {
        let (object, name) = match &*e.kind {
            ExprKind::Name(n) => (self.object_of(scope)?, n.name.clone()),
            ExprKind::Field { base, field } => match &*base.kind {
                ExprKind::Name(b) => (self.object_of(&b.name)?, field.name.clone()),
                _ => return None,
            },
            _ => return None,
        };
        let t = self.table_of(&object)?;
        let c = t.column(&name)?;
        Some(super::sql::pg_type(&c.ty))
    }

    /// `count(x)`, `count.distinct(x)`, `sum/min/max/avg(x)`, each
    /// optionally filtered (queries.md §6.3).
    fn aggregate(&mut self, e: &Expr, scope: &str) -> Option<String> {
        let ExprKind::Call {
            callee,
            args,
            filter,
        } = &*e.kind
        else {
            return self.unsupported("only columns and aggregates project");
        };
        let (name, distinct) = match &*callee.kind {
            ExprKind::Name(n) if is_aggregate(&n.name) => (n.name.as_str(), false),
            ExprKind::Field { base, field } => match &*base.kind {
                ExprKind::Name(b) if b.name == "count" && field.name == "distinct" => {
                    ("count", true)
                }
                _ => return self.unsupported("only aggregate calls project"),
            },
            _ => return self.unsupported("only aggregate calls project"),
        };

        let arg = args.first()?;
        let inner = match self.column_ref(arg, scope) {
            Some(c) => c,
            // `count(1)` counts rows and needs no column.
            None => match &*arg.kind {
                ExprKind::Int(n) => n.clone(),
                _ => return self.unsupported("an aggregate takes a column"),
            },
        };

        let mut sql = if distinct {
            format!("count(DISTINCT {inner})")
        } else {
            format!("{name}({inner})")
        };
        let filtered = filter.is_some();
        if let Some(f) = filter {
            // Not `count(CASE WHEN … THEN x END)`: FILTER says what is
            // meant, and the planner reads it (queries.md §6.3).
            sql.push_str(&format!(" FILTER (WHERE {})", self.predicate(f, scope)?));
        }

        // The wire form follows the *widened* result, not the operand:
        // `sum` of a bigint column is numeric, and both are strings on the
        // wire (types.md §2.3, §6.3).
        if let Some(cast) = self.aggregate_wire_cast(name, arg, scope) {
            // `agg(x) FILTER (WHERE p)::text` casts `p`, not the aggregate.
            sql = if filtered {
                format!("({sql})::{cast}")
            } else {
                format!("{sql}::{cast}")
            };
        }
        Some(sql)
    }

    fn aggregate_wire_cast(&self, name: &str, arg: &Expr, scope: &str) -> Option<&'static str> {
        match name {
            // `count` is an int on the wire, which JSON has (queries.md §6.3).
            "count" => None,
            "sum" | "avg" => match self.column_scalar(arg, scope)?.as_str() {
                // smallint and int sum to bigint; everything else that can
                // be summed lands on numeric. Both are text on the wire.
                "smallint" | "int" | "integer" => Some("text"),
                s if s == "bigint" || s.starts_with("numeric") => Some("text"),
                _ => None,
            },
            _ => self.cast_of(arg, scope),
        }
    }

    /// The rendered Postgres type of a column reference.
    fn column_scalar(&self, e: &Expr, scope: &str) -> Option<String> {
        let (object, name) = match &*e.kind {
            ExprKind::Name(n) => (self.object_of(scope)?, n.name.clone()),
            ExprKind::Field { base, field } => match &*base.kind {
                ExprKind::Name(b) => (self.object_of(&b.name)?, field.name.clone()),
                _ => return None,
            },
            _ => return None,
        };
        Some(self.table_of(&object)?.column(&name)?.ty.render())
    }
}

fn json_entry(key: &str, alias: &str, c: &super::model::ColumnObj) -> String {
    let col = format!("{alias}.{}", quote_ident(&c.physical));
    let value = match super::ddl::wire_cast(&c.ty) {
        Some(cast) => format!("{col}::{cast}"),
        None => col,
    };
    format!("'{}', {value}", escape(key))
}

fn escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Re-indent a join list so it lines up inside a lateral's inner select,
/// whose own clauses sit at eight columns.
fn indent(s: &str) -> String {
    s.replace('\n', "\n      ")
}

fn field_names(p: &ObjectShape) -> Vec<String> {
    p.fields
        .iter()
        .map(|f| match f {
            ProjField::Column(i) => i.name.clone(),
            ProjField::Expr { alias, .. } | ProjField::Nested { alias, .. } => alias.name.clone(),
        })
        .collect()
}

// ------------------------------------------------------------ sites

/// One `select` in the source, with a label that names where it is.
///
/// Enumerating every query a program issues is wanted in three places —
/// the golden test, `jwc explain` (25.e), and lint — and walking the AST
/// separately in each is three chances to miss one.
pub struct Site<'a> {
    pub label: String,
    pub select: &'a SelectExpr,
}

/// Every `select` in declaration order. Sub-selects inside a query's own
/// clauses are **not** separate sites: they compile as part of their
/// enclosing statement, so listing them would double-count one query.
pub fn sites(program: &Program) -> Vec<Site<'_>> {
    let mut out = Vec::new();
    for d in &program.decls {
        match d {
            Decl::View(v) => out.push(Site {
                label: format!("view {}", v.name.name),
                select: &v.body,
            }),
            Decl::Service(s) => {
                for f in &s.functions {
                    collect_block(&f.body, &format!("{}.{}", s.name.name, f.name.name), &mut out);
                }
            }
            Decl::Function(f) => collect_block(&f.body, &format!("function {}", f.name.name), &mut out),
            Decl::Middleware(m) => {
                collect_block(&m.body, &format!("middleware {}", m.name.name), &mut out);
                if let Some(a) = &m.after {
                    collect_block(a, &format!("middleware {} after", m.name.name), &mut out);
                }
            }
            Decl::Routes(r) => {
                for route in &r.routes {
                    let label = format!(
                        "route {} {}{}",
                        route.method.name.to_uppercase(),
                        r.prefix,
                        route.suffix
                    );
                    collect_block(&route.body, &label, &mut out);
                }
            }
            Decl::ErrorHandler(h) => {
                for arm in &h.arms {
                    let name = arm.error.as_ref().map(|e| e.name.as_str()).unwrap_or("_");
                    collect_block(&arm.body, &format!("errorHandler {name}"), &mut out);
                }
            }
            Decl::Test(t) => collect_block(&t.body, &format!("test {:?}", t.name), &mut out),
            _ => {}
        }
    }
    out
}

fn collect_block<'a>(block: &'a Block, label: &str, out: &mut Vec<Site<'a>>) {
    let start = out.len();
    for s in block {
        collect_stmt(s, label, out);
    }
    // A function with two queries needs two names; one with a single query
    // reads better without a `#1` nobody has to disambiguate.
    if out.len() - start > 1 {
        for (i, site) in out[start..].iter_mut().enumerate() {
            site.label = format!("{label} #{}", i + 1);
        }
    }
}

fn collect_stmt<'a>(s: &'a Stmt, label: &str, out: &mut Vec<Site<'a>>) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::Expr { expr: value, .. } => {
            collect_expr(value, label, out)
        }
        Stmt::If {
            cond,
            then,
            otherwise,
            ..
        } => {
            collect_expr(cond, label, out);
            for s in then {
                collect_stmt(s, label, out);
            }
            for s in otherwise.iter().flatten() {
                collect_stmt(s, label, out);
            }
        }
        Stmt::For { iterable, body, .. } => {
            collect_expr(iterable, label, out);
            for s in body {
                collect_stmt(s, label, out);
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_expr(v, label, out);
            }
        }
        Stmt::Throw { args, .. } => {
            for a in args {
                collect_expr(a, label, out);
            }
        }
        Stmt::Transaction { body, .. } => {
            for s in body {
                collect_stmt(s, label, out);
            }
        }
        Stmt::Assert { kind, .. } => match kind {
            AssertKind::Expr(e) => collect_expr(e, label, out),
            AssertKind::Fails { body, .. } => {
                for s in body {
                    collect_stmt(s, label, out);
                }
            }
        },
    }
}

fn collect_expr<'a>(e: &'a Expr, label: &str, out: &mut Vec<Site<'a>>) {
    match &*e.kind {
        // The whole query is one site; its clauses are not walked.
        ExprKind::Select(s) => out.push(Site {
            label: label.to_string(),
            select: s,
        }),
        ExprKind::Field { base, .. } | ExprKind::Cast { value: base, .. } => {
            collect_expr(base, label, out)
        }
        ExprKind::Index { base, index } => {
            collect_expr(base, label, out);
            collect_expr(index, label, out);
        }
        ExprKind::Call { callee, args, filter } => {
            collect_expr(callee, label, out);
            for a in args {
                collect_expr(a, label, out);
            }
            if let Some(f) = filter {
                collect_expr(f, label, out);
            }
        }
        ExprKind::Unary { rhs, .. } => collect_expr(rhs, label, out),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            collect_expr(lhs, label, out);
            collect_expr(rhs, label, out);
        }
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => {
            collect_expr(cond, label, out);
            collect_expr(then, label, out);
            collect_expr(otherwise, label, out);
        }
        ExprKind::In { lhs, items, .. } => {
            collect_expr(lhs, label, out);
            for i in items {
                collect_expr(i, label, out);
            }
        }
        ExprKind::Exists { query, .. } => collect_expr(query, label, out),
        ExprKind::Object(entries) => collect_entries(entries, label, out),
        ExprKind::WithHeaders { value, headers } => {
            collect_expr(value, label, out);
            collect_entries(headers, label, out);
        }
        ExprKind::Array(items) => {
            for i in items {
                collect_expr(i, label, out);
            }
        }
        ExprKind::Insert(_) | ExprKind::Update(_) | ExprKind::Delete(_) => {}
        ExprKind::OrThrow { value, args, .. } => {
            collect_expr(value, label, out);
            for a in args {
                collect_expr(a, label, out);
            }
        }
        ExprKind::CatchPostfix { value, body, .. } => {
            collect_expr(value, label, out);
            for s in body {
                collect_stmt(s, label, out);
            }
        }
        ExprKind::Cookie { value, args } => {
            collect_expr(value, label, out);
            for a in args {
                collect_expr(a, label, out);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Decimal(_)
        | ExprKind::Str(_)
        | ExprKind::RawStr(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Name(_)
        | ExprKind::Local(_)
        | ExprKind::PathParam(_) => {}
    }
}

fn collect_entries<'a>(entries: &'a [ObjEntry], label: &str, out: &mut Vec<Site<'a>>) {
    for entry in entries {
        if let ObjEntry::Field { value, .. } = entry {
            collect_expr(value, label, out);
        }
    }
}

fn is_aggregate(name: &str) -> bool {
    matches!(name, "count" | "sum" | "min" | "max" | "avg")
}

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
use super::model::{ColumnObj, SchemaModel, TableObj};
use super::naming::{self, quote_ident};
use super::query::{Node, Plan};
use super::sql::{Bind, PagePlan, Param, Shape};
use super::views::ViewObj;
use std::collections::HashMap;

/// A table or a view. Emission treats them alike — a view is a real
/// relation with columns (queries.md §8.2), which is the whole point of
/// emitting `CREATE VIEW` rather than inlining the body everywhere.
#[derive(Clone, Copy)]
pub enum Rel<'a> {
    Table(&'a TableObj),
    View(&'a ViewObj),
}

impl<'a> Rel<'a> {
    fn qualified(&self) -> String {
        match self {
            Rel::Table(t) => t.qualified(),
            Rel::View(v) => v.qualified(),
        }
    }

    fn column(&self, declared: &str) -> Option<&'a ColumnObj> {
        match self {
            Rel::Table(t) => t.column(declared),
            Rel::View(v) => v.column(declared),
        }
    }

    fn columns(&self) -> &'a [ColumnObj] {
        match self {
            Rel::Table(t) => &t.columns,
            Rel::View(v) => &v.columns,
        }
    }

    /// The first primary-key column — the guard for a nullable `as one`.
    /// A view has none, so joining *to* one cannot be guarded.
    fn key(&self) -> Option<&'a str> {
        match self {
            Rel::Table(t) => t
                .primary_key
                .as_ref()
                .and_then(|pk| pk.columns.first())
                .map(|s| s.as_str()),
            Rel::View(_) => None,
        }
    }
}

/// The first stage of a two-stage page: the CTE, and the key column the
/// second stage joins it back on.
struct Page {
    cte: String,
    key: String,
}

pub struct Compiled {
    pub sql: String,
    pub params: Vec<Param>,
    pub shape: Shape,
    pub record: bool,
    pub fields: Vec<String>,
    pub page: Option<PagePlan>,
}

pub struct Compiler<'a> {
    model: &'a SchemaModel,
    params: Vec<Param>,
    /// JWC binding alias -> SQL alias. Generated rather than reused so a
    /// binding named `user` or `order` cannot collide with SQL.
    aliases: HashMap<String, String>,
    /// JWC binding alias -> declared table or view name.
    binding_objects: HashMap<String, String>,
    /// Projection field of an `as one` -> the binding it came from. The
    /// two are names for the same row, and `orderby org.name` uses the
    /// field (queries.md §5.4).
    one_fields: HashMap<String, String>,
    next: usize,
    /// `server { max_page_size }` — the clamp a `page` with no `max` of
    /// its own gets (config.md §3).
    max_page: i64,
    /// The parameter the page's size was bound to, so the clamp can be
    /// written more than once without binding it twice.
    size_param: Option<usize>,
    /// A view body has no parameters, so its literals are emitted as
    /// literals. Everything else is identical, which is the point: a view
    /// is the same projection, standing still.
    literals: bool,
    /// Inside a view's column list an aggregate keeps its Postgres type
    /// too; the wire cast is the outer projection's job.
    no_wire: bool,
    /// Why emission gave up, when it did. Named at the point of failure —
    /// "not expressible" told a reader nothing about which release to wait
    /// for, and the compiler is the only thing that knows.
    gap: Option<String>,
    /// Set when the gap is a **diagnostic**, not a missing feature: the
    /// program is wrong and the checker reports it under this code.
    gap_code: Option<&'static str>,
}

impl<'a> Compiler<'a> {
    pub fn new(model: &'a SchemaModel) -> Self {
        Self {
            model,
            params: Vec::new(),
            aliases: HashMap::new(),
            binding_objects: HashMap::new(),
            one_fields: HashMap::new(),
            next: 0,
            max_page: 100,
            size_param: None,
            literals: false,
            no_wire: false,
            gap: None,
            gap_code: None,
        }
    }

    /// Set the clamp a `page` with no `max` falls back to.
    pub fn max_page_size(mut self, n: i64) -> Self {
        self.max_page = n;
        self
    }

    /// The reason the last `compile` returned `None`.
    pub fn gap(&self) -> &str {
        self.gap.as_deref().unwrap_or("this query is not expressible yet")
    }

    /// The diagnostic code, when the gap is a rejected program rather than
    /// an unimplemented one.
    pub fn gap_code(&self) -> Option<&'static str> {
        self.gap_code
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

    fn table(&mut self, object: &str) -> Option<Rel<'a>> {
        match self.rel_of(object) {
            Some(r) => Some(r),
            None => self.unsupported(&format!("`{object}` is not a table or a view")),
        }
    }

    fn rel_of(&self, object: &str) -> Option<Rel<'a>> {
        if let Some(t) = self.model.tables.iter().find(|t| t.declared == object) {
            return Some(Rel::Table(t));
        }
        self.model
            .views
            .iter()
            .find(|v| v.declared == object)
            .map(Rel::View)
    }

    /// Every parameter is bound as text and cast in SQL; see `sql.rs`.
    /// In a view body there is nothing to bind to, so a literal is emitted
    /// as one — and anything that is not a literal has no meaning there.
    fn bind(&mut self, e: &Expr, cast: &str) -> String {
        if self.literals {
            return match literal(e, cast) {
                Some(l) => l,
                None => {
                    self.gap = Some(
                        "a view body may only compare against literals — it has no \
                         parameters to bind"
                            .into(),
                    );
                    // Emission continues so the caller sees one clear gap
                    // rather than a cascade; the result is discarded.
                    "NULL".into()
                }
            };
        }
        self.params.push(Param {
            bind: Bind::Expr(e.clone()),
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
            if let Some(l) = &n.link {
                if l.cardinality == Cardinality::One {
                    self.one_fields.insert(l.field.clone(), n.alias.clone());
                }
            }
        }
        for g in &plan.groups {
            self.sql_alias(&g.alias);
            self.binding_objects
                .insert(g.alias.clone(), g.object.clone());
        }

        let projection = select.projection.as_ref();
        let root_alias = self.sql_alias(&plan.root.alias);
        let root_table = self.table(&plan.root.object)?;

        // queries.md §8.3 — a bounded page over a query that carries a
        // collection aggregates every candidate row and then throws all but
        // `limit` of them away. The keys are cheap; the collections are
        // not, so the keys go first.
        let collection = plan.root.has_many()
            || matches!(root_table, Rel::View(v) if v.has_many);
        let bounded = select.limit.is_some()
            || select.page.is_some()
            || (select.first && !select.order_by.is_empty());
        let page = if collection && bounded {
            Some(self.page_cte(select, plan, root_table)?)
        } else {
            None
        };

        let (json, joins) = self.emit(&plan.root, projection)?;

        let mut from = format!("{} {root_alias}", root_table.qualified());
        from.push_str(&joins);
        from.push_str(&self.group_joins(plan)?);

        // The page's key join replaces the filter: the CTE already applied
        // it, and applying it twice would only cost a second scan.
        if let Some(p) = &page {
            from.push_str(&format!(
                "\n  JOIN page ON page.{} = {root_alias}.{}",
                quote_ident(&p.key),
                quote_ident(&p.key)
            ));
        }

        // A `page` query also returns the ordering tuple of each row: the
        // cursor for the next page is the last row's, and the keys are
        // usually not in the projection (queries.md §9.3).
        let keys = match &select.page {
            Some(_) => {
                let cols = self.cursor_keys(select, plan, root_table)?;
                let order = self.page_order(select, root_table);
                let order = self.order_by(&order, &plan.root.alias)?;
                // `rn` rather than trusting a subquery's ORDER BY to survive
                // into the aggregate above it: the order *is* the answer
                // here, and a subquery's order is not a guarantee.
                format!(
                    ", json_build_array({}) AS k, row_number() OVER (ORDER BY {order}) AS rn",
                    cols.join(", ")
                )
            }
            None => String::new(),
        };

        let mut sql = format!("SELECT {json} AS j{keys}\n  FROM {from}");
        if page.is_none() {
            let mut wheres = Vec::new();
            if let Some(f) = &select.filter {
                wheres.push(self.predicate(f, &plan.root.alias)?);
            }
            if select.page.is_some() {
                wheres.push(self.keyset(select, plan, root_table)?);
            }
            if !wheres.is_empty() {
                sql.push_str(&format!("\n  WHERE {}", wheres.join(" AND ")));
            }
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

        // The bound lives in the CTE when there is one; repeating it here
        // would be harmless but would also hide where it is enforced.
        let shape = if select.first {
            if page.is_none() {
                sql.push_str("\n  LIMIT 1");
            }
            Shape::First
        } else if let Some(p) = &select.page {
            if page.is_none() {
                let l = self.page_limit(p);
                sql.push_str(&format!("\n  LIMIT {l}"));
            }
            // One row comes back: the envelope.
            Shape::First
        } else {
            if page.is_none() {
                if let Some(l) = &select.limit {
                    let p = self.bind(l, "int");
                    sql.push_str(&format!("\n  LIMIT {p}"));
                }
            }
            Shape::Rows
        };

        if let Some(p) = &page {
            sql = format!("{}\n{sql}", p.cte);
        }

        let wrapped = if let Some(p) = &select.page {
            // Three columns, not one JSON object: `items` has to reach the
            // response as the text Postgres produced, and anything that
            // parsed it to take it apart would re-serialise it with the
            // keys sorted (queries.md §7.2).
            //
            // The extra row the LIMIT asked for is what answers `has_more`;
            // it is filtered out of `items` rather than counted separately.
            let l = self.page_bound(p);
            format!(
                "SELECT coalesce(json_agg(q.j ORDER BY q.rn) FILTER (WHERE q.rn <= {l}), '[]'::json)::text, coalesce(json_agg(q.k ORDER BY q.rn) FILTER (WHERE q.rn <= {l}), '[]'::json)::text, count(*) > {l} FROM ({sql}) q"
            )
        } else {
            match shape {
                Shape::Rows => {
                    format!("SELECT coalesce(json_agg(q.j), '[]'::json)::text FROM ({sql}) q")
                }
                _ => format!("SELECT q.j::text FROM ({sql}) q"),
            }
        };

        let fields = match projection {
            Some(p) => field_names(p),
            None => root_table
                .columns()
                .iter()
                .filter(|c| !c.private)
                .map(|c| c.declared.clone())
                .collect(),
        };

        Some(Compiled {
            sql: wrapped,
            params: std::mem::take(&mut self.params),
            shape,
            // A view *is* a projection (types.md §5.3), so selecting from
            // one with no `as { }` still yields a record — which is what
            // lets the sample's membership gate read `access.role` off a
            // query that named no fields.
            record: projection.is_some() || matches!(root_table, Rel::View(_)),
            fields,
            page: select.page.as_ref().map(|p| PagePlan {
                after: p.after.clone(),
                size: p.size.clone(),
                max: self.page_max(p),
                // The envelope's `items` keeps whatever the query produced,
                // so raw survives it (types.md §5.4, queries.md §9.3).
                raw_items: projection.is_none() && matches!(root_table, Rel::Table(_)),
            }),
        })
    }

    // ------------------------------------------------------------- page

    /// The ordering tuple, as text, in the order the cursor carries it.
    fn cursor_keys(
        &mut self,
        select: &SelectExpr,
        plan: &Plan,
        rel: Rel<'a>,
    ) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for k in self.page_order(select, rel) {
            let col = self.column_ref(&k.expr, &plan.root.alias)?;
            out.push(format!("{col}::text"));
        }
        Some(out)
    }

    /// The query's `orderby`, with the relation's key appended when it is
    /// not already there.
    ///
    /// A keyset cursor is only a position if the order is **total**: two
    /// rows with the same `issued_at` and no tiebreak sit at the same
    /// cursor, and page 2 either repeats one or skips one, forever.
    fn page_order(&mut self, select: &SelectExpr, rel: Rel<'a>) -> Vec<SortKey> {
        let mut keys = select.order_by.clone();
        let Some(key) = self.page_key(rel) else {
            return keys;
        };
        let has = keys.iter().any(|k| match &*k.expr.kind {
            ExprKind::Name(n) => n.name == key,
            ExprKind::Field { field, .. } => field.name == key,
            _ => false,
        });
        if !has {
            keys.push(SortKey {
                expr: Expr::new(
                    ExprKind::Name(Ident {
                        name: key,
                        span: select.span,
                    }),
                    select.span,
                ),
                desc: false,
                nulls: None,
                span: select.span,
            });
        }
        keys
    }

    /// The declared name, on the driving relation, of the column a page is
    /// keyed by.
    fn page_key(&mut self, rel: Rel<'a>) -> Option<String> {
        match rel {
            Rel::Table(t) => t
                .primary_key
                .as_ref()
                .filter(|pk| pk.columns.len() == 1)
                .and_then(|pk| pk.columns.first())
                .cloned(),
            Rel::View(v) => {
                let base = self.rel_of(v.base.as_deref()?)?;
                let bkey = base.key()?;
                v.base_columns
                    .iter()
                    .find(|(_, b)| b == bkey)
                    .map(|(a, _)| a.clone())
            }
        }
    }

    /// The keyset predicate: "strictly after this tuple, in this order".
    ///
    /// Not a row comparison (`(a, b) < ($1, $2)`), which Postgres only
    /// reads the way it looks when every key runs the same direction. The
    /// expanded chain is longer and says the same thing for mixed
    /// directions, so there is one form to read rather than two.
    fn keyset(&mut self, select: &SelectExpr, plan: &Plan, rel: Rel<'a>) -> Option<String> {
        let keys = self.page_order(select, rel);
        let mut cols = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            let col = self.column_ref(&k.expr, &plan.root.alias)?;
            let cast = self.param_cast(&k.expr, &plan.root.alias)?;
            let n = self.bind_cursor(i, &cast);
            cols.push((col, if k.desc { "<" } else { ">" }, n));
        }
        let mut chain = String::new();
        for (i, (col, op, param)) in cols.iter().enumerate().rev() {
            chain = if chain.is_empty() {
                format!("{col} {op} {param}")
            } else {
                format!("{col} {op} {param} OR ({col} = {param} AND ({chain}))")
            };
            let _ = i;
        }
        // No cursor means the first page, so the predicate drops — the same
        // shape `==?` uses (queries.md §3.2).
        let first = self.params.len() - cols.len() + 1;
        Some(format!("(${first}::text IS NULL OR ({chain}))"))
    }

    /// Bind the i-th value the cursor carries.
    fn bind_cursor(&mut self, index: usize, cast: &str) -> String {
        self.params.push(Param {
            bind: Bind::Cursor(index),
            cast: cast.to_string(),
        });
        format!("(${}::text)::{cast}", self.params.len())
    }

    /// `size`, clamped, plus one — the extra row is how `has_more` is known
    /// without a second count.
    fn page_limit(&mut self, p: &PageClause) -> String {
        format!("({} + 1)", self.page_bound(p))
    }

    /// The clamped size. Bound once and referenced by number afterwards, so
    /// the bound appears in the SQL more than once but is decided once.
    fn page_bound(&mut self, p: &PageClause) -> String {
        let max = self.page_max(p);
        let n = match self.size_param {
            Some(n) => format!("(${n}::text)::int"),
            None => {
                let s = self.bind(&p.size, "int");
                self.size_param = Some(self.params.len());
                s
            }
        };
        format!("LEAST(GREATEST({n}, 1), {max})")
    }

    fn page_max(&self, p: &PageClause) -> i64 {
        match p.max.as_ref().map(|m| &*m.kind) {
            Some(ExprKind::Int(n)) => n.parse().unwrap_or(self.max_page),
            _ => self.max_page,
        }
    }

    /// The key-only first stage of the two-stage rewrite (queries.md §8.3).
    ///
    /// Scans the **base table**, not the driving relation: selecting keys
    /// from a view still evaluates its laterals, because Postgres will not
    /// drop a `LEFT JOIN LATERAL … ON true` it cannot prove row-preserving.
    /// The base table has the index the `orderby` was written for.
    ///
    /// `MATERIALIZED` is deliberate: a single-reference CTE is inlined by
    /// default since Postgres 12, which would put the whole rewrite back
    /// where it started.
    fn page_cte(&mut self, select: &SelectExpr, plan: &Plan, rel: Rel<'a>) -> Option<Page> {
        let (base, key, map) = match rel {
            Rel::Table(t) => {
                let key = t
                    .primary_key
                    .as_ref()
                    .filter(|pk| pk.columns.len() == 1)
                    .and_then(|pk| pk.columns.first())
                    .cloned();
                let Some(key) = key else {
                    return self.cannot_push(
                        "the driving table has no single-column primary key to page on",
                    );
                };
                (Rel::Table(t), key, Vec::new())
            }
            Rel::View(v) => {
                let Some(basename) = &v.base else {
                    return self.cannot_push("the view has no base table");
                };
                let Some(base) = self.rel_of(basename) else {
                    return self.cannot_push("the view's base table is not in the model");
                };
                let Some(bkey) = base.key() else {
                    return self.cannot_push(
                        "the view's base table has no single-column primary key",
                    );
                };
                // The view has to expose the key, or there is nothing to
                // join the page back on.
                let Some((view_col, _)) = v
                    .base_columns
                    .iter()
                    .find(|(_, b)| b == bkey)
                    .map(|(a, b)| (a.clone(), b.clone()))
                else {
                    return self.cannot_push(&format!(
                        "the view does not project `{bkey}`, so a page of keys cannot be \
                         joined back to it"
                    ));
                };
                (base, view_col, v.base_columns.clone())
            }
        };

        // Every column the filter and the order name must exist on the
        // base table under the same name, or the first stage would be
        // filtering something else.
        let to_base = |name: &str| -> Option<String> {
            if map.is_empty() {
                return Some(name.to_string());
            }
            map.iter()
                .find(|(v, _)| v == name)
                .map(|(_, b)| b.clone())
        };
        for name in referenced(select, &plan.root.alias) {
            match to_base(&name).and_then(|b| base.column(&b).map(|_| ())) {
                Some(()) => {}
                None => {
                    return self.cannot_push(&format!(
                        "`{name}` is not a column of the driving table, so the page \
                         cannot be selected before the collections are built — project \
                         it, or order by a column that is"
                    ))
                }
            }
        }

        // The CTE is emitted with the root binding pointed at the base
        // table under its own alias, so the existing predicate and order
        // code needs no second implementation.
        let base_alias = format!("p{}", self.next);
        self.next += 1;
        let saved_alias = self.aliases.insert(plan.root.alias.clone(), base_alias.clone());
        let saved_object = match base {
            Rel::Table(t) => self
                .binding_objects
                .insert(plan.root.alias.clone(), t.declared.clone()),
            Rel::View(v) => self
                .binding_objects
                .insert(plan.root.alias.clone(), v.declared.clone()),
        };

        let base_key = to_base(&key)?;
        let base_col = base.column(&base_key)?.physical.clone();
        let mut cte = format!(
            "WITH page AS MATERIALIZED (\n  SELECT {base_alias}.{} FROM {} {base_alias}",
            quote_ident(&base_col),
            base.qualified()
        );
        let mut ok = true;
        let mut wheres = Vec::new();
        if let Some(f) = &select.filter {
            match self.predicate(f, &plan.root.alias) {
                Some(p) => wheres.push(p),
                None => ok = false,
            }
        }
        if select.page.is_some() {
            match self.keyset(select, plan, base) {
                Some(p) => wheres.push(p),
                None => ok = false,
            }
        }
        if !wheres.is_empty() {
            cte.push_str(&format!("\n   WHERE {}", wheres.join(" AND ")));
        }
        if !select.order_by.is_empty() {
            match self.order_by(&select.order_by, &plan.root.alias) {
                Some(o) => cte.push_str(&format!("\n   ORDER BY {o}")),
                None => ok = false,
            }
        }
        if select.first {
            cte.push_str("\n   LIMIT 1");
        } else if let Some(p) = &select.page {
            let l = self.page_limit(p);
            cte.push_str(&format!("\n   LIMIT {l}"));
        } else if let Some(l) = &select.limit {
            let p = self.bind(l, "int");
            cte.push_str(&format!("\n   LIMIT {p}"));
        }
        cte.push_str("\n)");

        // Restore the binding so the second stage reads the view again.
        match saved_alias {
            Some(a) => self.aliases.insert(plan.root.alias.clone(), a),
            None => self.aliases.remove(&plan.root.alias),
        };
        match saved_object {
            Some(o) => self.binding_objects.insert(plan.root.alias.clone(), o),
            None => self.binding_objects.remove(&plan.root.alias),
        };
        if !ok {
            return None;
        }

        let view_key_col = rel.column(&key)?.physical.clone();
        Some(Page {
            cte,
            key: view_key_col,
        })
    }

    /// queries.md §8.3 — a pushdown that cannot be proven is an error with
    /// the rewrite named, never a silent plan over the whole table.
    fn cannot_push<T>(&mut self, why: &str) -> Option<T> {
        self.gap = Some(format!(
            "this page cannot be pushed down — {why}; select the page of keys in one \
             query and the detail in a second"
        ));
        self.gap_code = Some("E0542");
        None
    }

    /// The `SELECT` behind a `CREATE VIEW`: a column list, not one JSON
    /// value.
    ///
    /// A view is a relation, so scalars keep their Postgres type — a
    /// `bigint` that came back as text would make `where org_id == @id`
    /// compare a number to a string. The wire cast belongs to the
    /// outermost projection, which is the query that selects *from* here.
    /// Nested fields are the exception: they are already final JSON.
    pub fn compile_view(&mut self, select: &SelectExpr, plan: &Plan, view: &ViewObj) -> Option<String> {
        self.literals = true;
        let projection = select.projection.as_ref()?;
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
        let root_alias = self.sql_alias(&plan.root.alias);
        let root = self.table(&plan.root.object)?;

        let mut columns: Vec<String> = Vec::new();
        let mut joins = String::new();
        for f in &projection.fields {
            match f {
                ProjField::Column(i) => {
                    let c = root.column(&i.name)?;
                    columns.push(format!(
                        "{root_alias}.{} AS {}",
                        quote_ident(&c.physical),
                        quote_ident(&naming::physical(&i.name))
                    ));
                }
                ProjField::Expr { alias: a, value, .. } => {
                    let sql = self.bare_value(value, &plan.root.alias)?;
                    columns.push(format!(
                        "{sql} AS {}",
                        quote_ident(&naming::physical(&a.name))
                    ));
                }
                ProjField::Nested { alias: a, shape, .. } => {
                    let child = plan
                        .root
                        .children
                        .iter()
                        .find(|c| c.link.as_ref().is_some_and(|l| l.field == a.name))?;
                    let (value, from) = self.child(child, shape)?;
                    columns.push(format!(
                        "{value} AS {}",
                        quote_ident(&naming::physical(&a.name))
                    ));
                    joins.push_str(&from);
                    // The flattened columns, in the order views.rs put
                    // them in — the two lists are one list, split across
                    // two files, and a mismatch is a wrong column name.
                    if child.link.as_ref().is_some_and(|l| l.cardinality == Cardinality::One) {
                        let calias = self.sql_alias(&child.alias);
                        let crel = self.table(&child.object)?;
                        for nested in &shape.fields {
                            let (name, col) = match nested {
                                ProjField::Column(i) => (i.name.clone(), i.name.clone()),
                                ProjField::Expr { alias: na, value, .. } => match &*value.kind {
                                    ExprKind::Name(n) => (na.name.clone(), n.name.clone()),
                                    _ => continue,
                                },
                                ProjField::Nested { .. } => continue,
                            };
                            let Some(c) = crel.column(&col) else { continue };
                            let flat = format!("{}{}{}", a.name, super::views::FLAT, name);
                            columns.push(format!(
                                "{calias}.{} AS {}",
                                quote_ident(&c.physical),
                                quote_ident(&naming::physical(&flat))
                            ));
                        }
                    }
                }
            }
        }

        let mut from = format!("{} {root_alias}", root.qualified());
        from.push_str(&joins);
        from.push_str(&self.group_joins(plan)?);

        let mut sql = format!("SELECT {}\n  FROM {from}", columns.join(",\n       "));
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
        if self.gap.is_some() {
            return None;
        }
        let _ = view;
        Some(sql)
    }

    /// A projected expression with **no** wire cast: inside a view a column
    /// keeps its Postgres type.
    fn bare_value(&mut self, e: &Expr, scope: &str) -> Option<String> {
        if let Some(c) = self.column_ref(e, scope) {
            return Some(c);
        }
        match &*e.kind {
            ExprKind::Call { .. } => {
                let saved = std::mem::replace(&mut self.no_wire, true);
                let out = self.aggregate(e, scope);
                self.no_wire = saved;
                out
            }
            _ => self.unsupported("a projection field is a column or an aggregate"),
        }
    }

    fn group_joins(&mut self, plan: &Plan) -> Option<String> {
        let mut out = String::new();
        for g in &plan.groups {
            let ga = self.sql_alias(&g.alias);
            let gt = self.table(&g.object)?;
            let on = self.predicate(&g.on, &g.alias)?;
            let kind = match g.kind {
                JoinKind::Left => "LEFT JOIN",
                JoinKind::Inner => "JOIN",
            };
            out.push_str(&format!("\n  {kind} {} {ga} ON {on}", gt.qualified()));
            if let Some(f) = &g.filter {
                let extra = self.predicate(f, &g.alias)?;
                out.push_str(&format!(" AND {extra}"));
            }
        }
        Some(out)
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
                for c in table.columns().iter().filter(|c| !c.private) {
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
                    let Some(key) = table.key() else {
                        return self.unsupported(
                            "a nullable `as one` needs the joined relation's primary \
                             key to tell a missing row from a row of nulls, and a view \
                             has none",
                        );
                    };
                    let guard = format!("{alias}.{}", quote_ident(key));
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
                // `org.name` where `org` is a binding, or the field an
                // `as one` in this query produces — the same row either
                // way.
                let binding = self
                    .one_fields
                    .get(&b.name)
                    .cloned()
                    .unwrap_or_else(|| b.name.clone());
                if let Some(object) = self.object_of(&binding) {
                    let alias = self.sql_alias(&binding);
                    let table = self.table(&object)?;
                    let c = table.column(&field.name)?;
                    return Some(format!("{alias}.{}", quote_ident(&c.physical)));
                }
                // `org.name` where `org` is a nested field of the driving
                // relation: the join is inside the view, so there is
                // nothing to reach for. It lowers to the column the view
                // flattened it into — not to a JSON path, which orders by
                // text (N6, queries.md §8.2).
                let object = self.object_of(scope)?;
                let rel = self.table(&object)?;
                let flat = format!("{}{}{}", b.name, super::views::FLAT, field.name);
                let c = rel.column(&flat)?;
                let alias = self.sql_alias(scope);
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
        let t = self.rel_of(&object)?;
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
                    // `$n::text`, not a bare `$n`: every parameter is bound
                    // as text, and a bare one in a null test gives Postgres
                    // nothing to infer from — "could not determine data
                    // type of parameter $2".
                    format!("(${n}::text IS NULL OR {col} = {p})")
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
            ExprKind::Unary {
                op: UnaryOp::Not,
                rhs,
            } => format!("NOT ({})", self.predicate(rhs, scope)?),
            // queries.md §3.3 — a list, or one array-typed operand. The
            // array form is `= ANY($n)`: one parameter, and no way for a
            // caller's value to reach the SQL as text.
            ExprKind::In {
                lhs,
                items,
                negated,
            } => {
                let col = self.column_ref(lhs, scope)?;
                let cast = self.param_cast(lhs, scope)?;
                let not = if *negated { "NOT " } else { "" };
                if items.len() == 1 && !is_literal(&items[0]) {
                    let p = self.bind(&items[0], &format!("{cast}[]"));
                    // `<> ALL`, not `NOT = ANY` — which is not SQL.
                    if *negated {
                        format!("{col} <> ALL({p})")
                    } else {
                        format!("{col} = ANY({p})")
                    }
                } else {
                    let mut parts = Vec::new();
                    for i in items {
                        parts.push(self.bind(i, &cast));
                    }
                    format!("{col} {not}IN ({})", parts.join(", "))
                }
            }
            // queries.md §3.5 — how a parent is filtered by its children.
            ExprKind::Exists { query, negated } => {
                let ExprKind::Select(inner) = &*query.kind else {
                    return self.unsupported("`exists` takes a query");
                };
                let sql = self.subquery(inner)?;
                let not = if *negated { "NOT " } else { "" };
                format!("{not}EXISTS ({sql})")
            }
            _ => return None,
        })
    }

    /// `SELECT 1 FROM …` for an `exists`.
    ///
    /// Compiled in the *same* compiler, so the outer bindings are still in
    /// scope: the whole point of the construct is that the inner `where`
    /// references the row being filtered.
    fn subquery(&mut self, inner: &SelectExpr) -> Option<String> {
        // `query::plan` reads the symbol table only to turn a qualified
        // path into a declared name, and falls back to the last segment.
        // Declared names are unique, so the fallback is exact.
        let plan = super::query::plan(inner, &super::symbols::Symbols::default());
        if plan
            .diags
            .iter()
            .any(|d| d.severity == super::diag::Severity::Error)
        {
            return self.unsupported("the subquery does not plan");
        }
        let alias = self.sql_alias(&plan.root.alias);
        self.binding_objects
            .insert(plan.root.alias.clone(), plan.root.object.clone());
        let rel = self.table(&plan.root.object)?;
        let mut sql = format!("SELECT 1 FROM {} {alias}", rel.qualified());
        sql.push_str(&self.group_joins(&plan)?);
        if let Some(f) = &inner.filter {
            sql.push_str(&format!(" WHERE {}", self.predicate(f, &plan.root.alias)?));
        }
        Some(sql)
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
            "count" => "int".to_string(),
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
        let t = self.rel_of(&object)?;
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
        // `count` is `int` (queries.md §6.3) and Postgres's is `bigint`, so
        // the cast is the declared type, not the wire form — it applies in
        // a view's column list too, where it is what keeps the column an
        // `int` rather than a `bigint` the outer projection would then
        // serialise as a string.
        if name == "count" {
            return Some("int");
        }
        if self.no_wire {
            return None;
        }
        match name {
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
        Some(self.rel_of(&object)?.column(&name)?.ty.render())
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

/// A view body's comparison values. Only literals reach here — a view has
/// no parameters, and `check.rs` has no locals in scope to offer it.
fn literal(e: &Expr, cast: &str) -> Option<String> {
    Some(match &*e.kind {
        // A numeric literal is already its own type in SQL; only the
        // string-shaped ones need to be told what they are.
        ExprKind::Int(n) => n.clone(),
        ExprKind::Decimal(n) => n.clone(),
        ExprKind::Str(s) => format!("'{}'::{cast}", escape(s)),
        ExprKind::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        ExprKind::Null => "NULL".to_string(),
        // `InvoiceStatus.open` — the wire form of an enum is its member
        // name (types.md §3.4), physical form or not.
        ExprKind::Field { base, field } => match &*base.kind {
            ExprKind::Name(_) => format!("'{}'::{cast}", escape(&field.name)),
            _ => return None,
        },
        _ => return None,
    })
}

/// Every column name a query's `where` and `orderby` reach for, unqualified
/// or qualified to the driving binding. A name reached through another
/// binding is not a driving-table column and is left out on purpose — that
/// is exactly the case the pushdown cannot prove.
fn referenced(select: &SelectExpr, root: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(f) = &select.filter {
        names(f, &mut out);
    }
    for k in &select.order_by {
        names(&k.expr, &mut out);
    }
    // `I.id` and `id` are the same column when `I` is the driving binding;
    // only a *different* binding puts the name out of the base's reach.
    let prefix = format!("{root}.");
    for n in &mut out {
        if let Some(rest) = n.strip_prefix(&prefix) {
            *n = rest.to_string();
        }
    }
    out.sort();
    out.dedup();
    out
}

fn names(e: &Expr, out: &mut Vec<String>) {
    match &*e.kind {
        ExprKind::Name(n) => out.push(n.name.clone()),
        // `org.name` on a view is a flattened column, which the base table
        // does not have — so it stays out and the pushdown is refused.
        ExprKind::Field { base, field } => {
            if let ExprKind::Name(b) = &*base.kind {
                out.push(format!("{}.{}", b.name, field.name));
            }
        }
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            names(lhs, out);
            names(rhs, out);
        }
        ExprKind::Unary { rhs, .. } => names(rhs, out),
        _ => {}
    }
}

/// A literal is a value written in the source; anything else in an `in`
/// list is a reference, and a single reference is the array form.
fn is_literal(e: &Expr) -> bool {
    matches!(
        &*e.kind,
        ExprKind::Int(_)
            | ExprKind::Decimal(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Field { .. }
    )
}

// ------------------------------------------------------- raw tracking

/// Whether a query's result stays `Raw` all the way to the response, and
/// what took it out of that state if not (types.md §5, #41).
///
/// "Raw" is the performance promise — one JSON value comes back from
/// Postgres and is never parsed — and it is easy to lose by accident: a
/// projection is enough, and so is reading one field. `jwc v1 explain`
/// prints this per query so the promise is checkable rather than assumed.
pub enum Raw {
    Preserved,
    Lost(&'static str),
}

impl std::fmt::Display for Raw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Raw::Preserved => write!(f, "raw preserved"),
            Raw::Lost(why) => write!(f, "raw lost here: {why}"),
        }
    }
}

/// Classify one query. The model is needed because a view source is a
/// record by construction, which is a *different* reason from a projection.
pub fn raw_state(model: &SchemaModel, select: &SelectExpr, plan: &Plan) -> Raw {
    if select.projection.is_some() {
        return Raw::Lost("`as { }` — a projection is parsed so its fields can be read");
    }
    if model.views.iter().any(|v| v.declared == plan.root.object) {
        return Raw::Lost("a view is a named projection, so selecting from one is a record");
    }
    if select.page.is_some() {
        // The envelope is a record, but `items` inside it is spliced.
        return Raw::Preserved;
    }
    Raw::Preserved
}

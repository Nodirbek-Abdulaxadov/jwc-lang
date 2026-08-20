//! Views as relations.
//!
//! A `view` is a real `CREATE VIEW`, not a macro (queries.md §8.2). That
//! means it has to have **columns**, and the type of each has to be known
//! before a query can select from it — which is what this module works out.
//!
//! Three shapes come out of a projection:
//!
//! * a scalar field is a column of that scalar's type, keeping its
//!   Postgres type rather than the wire form. The wire cast belongs to the
//!   outermost projection; inside a view a `bigint` stays a `bigint`, or
//!   `where MA.org_id == @org_id` would be comparing text to a number.
//! * a nested `as one` / `as many` field is a `json` column;
//! * a nested `as one` field **also** contributes flattened
//!   `<field>__<name>` columns. `orderby org.name` on a query against a
//!   view has no join to reach for — the join is inside the view — and
//!   ordering by a JSON path is not the same thing as ordering by the
//!   column (N6). The flattened column is what that lowers to.

use crate::ast::*;
use crate::model::{ColumnObj, SchemaModel, SqlType, TableObj};
use crate::naming;
use crate::query::{self, Node};
use crate::symbols::Symbols;
use crate::workspace::{Loc, Workspace};

#[derive(Clone, Debug)]
pub struct ViewObj {
    pub declared: String,
    pub schema: String,
    pub schema_physical: String,
    pub physical: String,
    pub docs: Vec<String>,
    pub columns: Vec<ColumnObj>,
    /// The projection carries a collection. A query against this view with
    /// a bound cannot go through the view object as written — the
    /// collections would be aggregated for every row and then thrown away
    /// (queries.md §8.3).
    pub has_many: bool,
    /// The `SELECT` behind `CREATE VIEW`. `None` when the body uses
    /// something emission cannot express yet; `gap` says what.
    pub body: Option<String>,
    pub gap: Option<String>,
    /// The table the view drives off, and the pushdown key: a query with a
    /// bound is rewritten against this rather than against the view
    /// (queries.md §8.3).
    pub base: Option<String>,
    /// View column -> base-table column, for the columns a pushdown may
    /// filter or order on. Aggregates, nested fields and joined columns
    /// are absent, which is exactly when the pushdown is not provable.
    pub base_columns: Vec<(String, String)>,
    /// Every relation the body names, for emission ordering.
    pub reads: Vec<String>,
    pub loc: Loc,
}

impl ViewObj {
    pub fn qualified(&self) -> String {
        format!(
            "{}.{}",
            naming::quote_ident(&self.schema_physical),
            naming::quote_ident(&self.physical)
        )
    }

    pub fn column(&self, declared: &str) -> Option<&ColumnObj> {
        self.columns.iter().find(|c| c.declared == declared)
    }
}

/// The separator between a nested field and the column it flattens to.
pub const FLAT: &str = "__";

/// Work out every view's columns and hang them on the model.
///
/// Called from `model::build`, so a `SchemaModel` always carries its views:
/// there is no order in which a caller can hold a half-built model.
pub(super) fn attach(model: &mut SchemaModel, ws: &Workspace) {
    // `query::plan` reads the symbol table only to turn `App.org.Members`
    // into `Members`, and falls back to the last path segment when it
    // cannot. Declared names are unique across a program, so the fallback
    // is exact here — which is what lets views be resolved during the model
    // pass, before symbols exist.
    let sym = Symbols::default();
    let mut views: Vec<ViewObj> = Vec::new();
    for (i, file) in ws.files.iter().enumerate() {
        for d in &file.program.decls {
            let Decl::View(v) = d else { continue };
            let loc = Loc {
                file: i,
                span: v.span,
            };
            if let Some(obj) = build_one(model, v, loc, &sym) {
                views.push(obj);
            }
        }
    }
    views.sort_by(|a, b| (&a.schema_physical, &a.physical).cmp(&(&b.schema_physical, &b.physical)));
    model.views = views;

    // Bodies are compiled once every view's *columns* are known, so a view
    // may select from a view.
    let sym = Symbols::default();
    let mut bodies: Vec<(Option<String>, Option<String>)> = Vec::new();
    for v in &model.views {
        let Some(decl) = find_decl(ws, &v.declared) else {
            bodies.push((None, Some("no declaration".into())));
            continue;
        };
        let plan = query::plan(&decl.body, &sym);
        let mut c = crate::query_sql::Compiler::new(model);
        match c.compile_view(&decl.body, &plan, v) {
            Some(sql) => bodies.push((Some(sql), None)),
            None => bodies.push((None, Some(c.gap().to_string()))),
        }
    }
    for (v, (body, gap)) in model.views.iter_mut().zip(bodies) {
        v.body = body;
        v.gap = gap;
    }
}

fn find_decl<'a>(ws: &'a Workspace, declared: &str) -> Option<&'a ViewDecl> {
    ws.files.iter().find_map(|f| {
        f.program.decls.iter().find_map(|d| match d {
            Decl::View(v) if v.name.name == declared => Some(v),
            _ => None,
        })
    })
}

fn build_one(model: &SchemaModel, v: &ViewDecl, loc: Loc, sym: &Symbols) -> Option<ViewObj> {
    let projection = v.body.projection.as_ref()?;
    let plan = query::plan(&v.body, sym);
    // A view whose plan is broken has no columns to speak of; the checker
    // reports the plan's own diagnostics against the source.
    if plan.diags.iter().any(|d| d.severity == crate::diag::Severity::Error) {
        return None;
    }
    let root = model.tables.iter().find(|t| t.declared == plan.root.object)?;
    let schema_physical = root.schema_physical.clone();

    let mut columns = Vec::new();
    for f in &projection.fields {
        match f {
            ProjField::Column(i) => {
                let c = root.column(&i.name)?;
                columns.push(view_column(&i.name, c.ty.clone(), c.nullable));
            }
            ProjField::Expr { alias, value, .. } => {
                let (ty, nullable) = expr_type(model, &plan, value, &plan.root.alias)?;
                columns.push(view_column(&alias.name, ty, nullable));
            }
            ProjField::Nested { alias, shape, .. } => {
                let child = plan
                    .root
                    .children
                    .iter()
                    .find(|c| c.link.as_ref().is_some_and(|l| l.field == alias.name))
                    .or_else(|| plan.root.resolve(&alias.name))?;
                let one = child
                    .link
                    .as_ref()
                    .is_some_and(|l| l.cardinality == Cardinality::One);
                columns.push(view_column(
                    &alias.name,
                    SqlType::Scalar("json".into()),
                    one,
                ));
                if one {
                    flatten(model, child, shape, &alias.name, &mut columns);
                }
            }
        }
    }

    // Column names must line up with what `compile_view` emits, so the
    // two walks are written against the same projection in the same order.
    let mut base_columns = Vec::new();
    for f in &projection.fields {
        match f {
            ProjField::Column(i) => {
                if root.column(&i.name).is_some() {
                    base_columns.push((i.name.clone(), i.name.clone()));
                }
            }
            ProjField::Expr { alias, value, .. } => {
                if let ExprKind::Name(n) = &*value.kind {
                    if root.column(&n.name).is_some() {
                        base_columns.push((alias.name.clone(), n.name.clone()));
                    }
                }
            }
            ProjField::Nested { .. } => {}
        }
    }

    Some(ViewObj {
        declared: v.name.name.clone(),
        schema: v.schema.schema.name.clone(),
        schema_physical,
        physical: v
            .physical
            .clone()
            .unwrap_or_else(|| naming::physical(&v.name.name)),
        docs: v.at.docs.clone(),
        columns,
        has_many: plan.root.has_many(),
        body: None,
        gap: None,
        base: Some(root.declared.clone()),
        base_columns,
        reads: {
            let mut all = Vec::new();
            plan.root.walk(&mut all);
            let mut names: Vec<String> = all.iter().map(|n| n.object.clone()).collect();
            names.extend(plan.groups.iter().map(|g| g.object.clone()));
            names.sort();
            names.dedup();
            names
        },
        loc,
    })
}

/// `as one org` with `{ id, slug }` adds `org__id` and `org__slug`. Only
/// one level: a nested `one` inside a nested `one` is reachable through the
/// JSON, and flattening it would put four names on one value.
fn flatten(
    model: &SchemaModel,
    child: &Node,
    shape: &ObjectShape,
    prefix: &str,
    out: &mut Vec<ColumnObj>,
) {
    let Some(table) = model.tables.iter().find(|t| t.declared == child.object) else {
        return;
    };
    for f in &shape.fields {
        let (name, ty, nullable) = match f {
            ProjField::Column(i) => match table.column(&i.name) {
                Some(c) => (i.name.clone(), c.ty.clone(), true),
                None => continue,
            },
            ProjField::Expr { alias, value, .. } => match &*value.kind {
                ExprKind::Name(n) => match table.column(&n.name) {
                    Some(c) => (alias.name.clone(), c.ty.clone(), true),
                    None => continue,
                },
                _ => continue,
            },
            ProjField::Nested { .. } => continue,
        };
        // Every flattened column is nullable: the join it came from is a
        // `left join` whenever the field is optional, and a view column
        // that claims NOT NULL it cannot keep is worse than one that does
        // not claim it.
        let mut c = view_column(&format!("{prefix}{FLAT}{name}"), ty, nullable);
        // `private` here means what it always means: not part of a default
        // projection. Nobody writes `org__name` — the compiler puts it
        // there so `orderby org.name` has a column to lower to, and a
        // response that shipped both `org` and `org__name` would be
        // answering with the compiler's scratch space.
        c.private = true;
        out.push(c);
    }
}

fn view_column(declared: &str, ty: SqlType, nullable: bool) -> ColumnObj {
    ColumnObj {
        declared: declared.to_string(),
        physical: naming::physical(declared),
        was: None,
        ty,
        nullable,
        identity: false,
        default: None,
        private: false,
        server: false,
        on_update_now: false,
        docs: Vec::new(),
        loc: Loc {
            file: 0,
            span: crate::token::Span { start: 0, end: 0 },
        },
    }
}

/// The type of a projected expression: a column reference, or an aggregate.
fn expr_type(
    model: &SchemaModel,
    plan: &query::Plan,
    e: &Expr,
    scope: &str,
) -> Option<(SqlType, bool)> {
    if let Some((c, _)) = column_of(model, plan, e, scope) {
        return Some((c.ty.clone(), c.nullable));
    }
    let ExprKind::Call { callee, args, .. } = &*e.kind else {
        return None;
    };
    let name = match &*callee.kind {
        ExprKind::Name(n) => n.name.as_str(),
        // `count.distinct`
        ExprKind::Field { .. } => "count",
        _ => return None,
    };
    // An aggregate over an empty group is null; `count` is 0 (types.md §6.3).
    Some(match name {
        // queries.md §6.3 — `count` is `int`, so the emitted column is too;
        // Postgres's `count` is a `bigint`, and a `bigint` column would be
        // a string on the wire.
        "count" => (SqlType::Scalar("int".into()), false),
        "sum" | "avg" => {
            let arg = args.first()?;
            let (c, _) = column_of(model, plan, arg, scope)?;
            let rendered = c.ty.render();
            let widened = match rendered.as_str() {
                "smallint" | "integer" | "int" if name == "sum" => "bigint",
                _ => "numeric",
            };
            (SqlType::Scalar(widened.into()), true)
        }
        "min" | "max" => {
            let arg = args.first()?;
            let (c, _) = column_of(model, plan, arg, scope)?;
            (c.ty.clone(), true)
        }
        _ => return None,
    })
}

/// Resolve a bare or qualified column reference against the plan's
/// bindings, returning the column and the table it came from.
fn column_of<'a>(
    model: &'a SchemaModel,
    plan: &query::Plan,
    e: &Expr,
    scope: &str,
) -> Option<(&'a ColumnObj, &'a TableObj)> {
    let (binding, name) = match &*e.kind {
        ExprKind::Name(n) => (scope.to_string(), n.name.clone()),
        ExprKind::Field { base, field } => match &*base.kind {
            ExprKind::Name(b) => (b.name.clone(), field.name.clone()),
            _ => return None,
        },
        _ => return None,
    };
    let object = object_of(plan, &binding)?;
    let table = model.tables.iter().find(|t| t.declared == object)?;
    Some((table.column(&name)?, table))
}

fn object_of(plan: &query::Plan, alias: &str) -> Option<String> {
    if let Some(n) = plan.root.find(alias) {
        return Some(n.object.clone());
    }
    plan.groups
        .iter()
        .find(|g| g.alias == alias)
        .map(|g| g.object.clone())
}

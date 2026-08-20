//! DDL emission — the DBA test in code.
//!
//! Reads [`SchemaModel`] and produces a list of statements, each carrying
//! the source location that caused it. Two properties are load-bearing:
//!
//! * **Order is fixed** (schema.md §9): schemas → enum types → tables →
//!   *every* foreign key in a separate pass → indexes → triggers →
//!   comments. The separate FK pass is what makes the sample's
//!   `auth → org → auth` cycle emittable at all.
//! * **Order inside a phase is total**, so two runs on the same source are
//!   byte-identical. `tests/v1_ddl_golden.rs` asserts that against checked-in
//!   `.sql` files.

use crate::model::{ColumnObj, SchemaModel, SqlType, TableObj};
use crate::naming::quote_ident;
use crate::snapshot::{
    self, ColumnSnapshot, EnumSnapshot, ForeignKeySnapshot, FunctionSnapshot, IndexSnapshot,
    SchemaSnapshot, TableSnapshot, TriggerSnapshot,
};
use crate::workspace::{Loc, Workspace};

/// One emitted statement plus where it came from.
pub struct Statement {
    pub sql: String,
    pub loc: Loc,
    pub phase: Phase,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Phase {
    Schema = 1,
    EnumType = 2,
    Table = 3,
    ForeignKey = 4,
    Index = 5,
    Trigger = 6,
    /// After the tables it reads, before the comments that name it. A view
    /// that selects from another view sorts after it.
    View = 7,
    Comment = 8,
}

pub fn emit(model: &SchemaModel) -> Vec<Statement> {
    let mut out = Vec::new();

    // Every statement below is rendered from the *snapshot* form of the
    // object, never from the model directly. `jwc migrate` emits from a
    // snapshot too, so one renderer serves both and the DDL a migration
    // applies cannot drift from the DDL `gen-sql` prints. The model is
    // still what is walked, because it is the only side carrying source
    // locations.
    for s in &model.schemas {
        out.push(Statement {
            sql: create_schema(&SchemaSnapshot {
                name: s.physical.clone(),
                declared: s.declared.clone(),
            }),
            loc: s.loc,
            phase: Phase::Schema,
        });
    }

    // 2 — enum types (only the `of` form creates one; schema.md §5)
    for e in &model.enums {
        let Some(schema) = &e.schema else { continue };
        out.push(Statement {
            sql: create_type(&EnumSnapshot {
                schema: schema.clone(),
                name: e.physical.clone(),
                declared: e.declared.clone(),
                values: e.members.clone(),
            }),
            loc: e.loc,
            phase: Phase::EnumType,
        });
    }

    // 3 — tables: columns, primary key, checks, and non-partial uniques.
    //     Foreign keys deliberately excluded (phase 4).
    let snaps: Vec<TableSnapshot> = model.tables.iter().map(snapshot::table_of).collect();
    for (t, st) in model.tables.iter().zip(&snaps) {
        out.push(Statement {
            sql: create_table(st),
            loc: t.loc,
            phase: Phase::Table,
        });
    }

    // 4 — every foreign key, after every table exists.
    for (t, st) in model.tables.iter().zip(&snaps) {
        for (fk, sfk) in t.foreign_keys.iter().zip(&st.foreign_keys) {
            out.push(Statement {
                sql: add_foreign_key(&st.qualified(), sfk),
                loc: fk.loc,
                phase: Phase::ForeignKey,
            });
        }
    }

    // 5 — indexes. A partial unique is an index, not a table constraint
    //     (schema.md §4.3), and comes out ahead of the ordinary ones.
    for (t, st) in model.tables.iter().zip(&snaps) {
        for (u, su) in t.uniques.iter().zip(&st.uniques) {
            if su.predicate.is_none() {
                continue;
            }
            out.push(Statement {
                sql: create_index(&st.qualified(), &unique_as_index(su)),
                loc: u.loc,
                phase: Phase::Index,
            });
        }
        for (ix, six) in t.indexes.iter().zip(&st.indexes) {
            out.push(Statement {
                sql: create_index(&st.qualified(), six),
                loc: ix.loc,
                phase: Phase::Index,
            });
        }
    }

    // 6 — touch functions and triggers (schema.md §6). One pair per table,
    //     however many columns carry `on update now()`.
    for t in &model.tables {
        if t.touch_columns.is_empty() {
            continue;
        }
        let fname = crate::naming::touch_function(&t.physical);
        out.push(Statement {
            sql: create_function(&FunctionSnapshot {
                schema: t.schema_physical.clone(),
                name: fname.clone(),
                table: t.physical.clone(),
                sets_now: t.touch_columns.clone(),
            }),
            loc: t.loc,
            phase: Phase::Trigger,
        });
        out.push(Statement {
            sql: create_trigger(&TriggerSnapshot {
                name: fname.clone(),
                schema: t.schema_physical.clone(),
                table: t.physical.clone(),
                function: fname,
                timing: "BEFORE".into(),
                event: "UPDATE".into(),
            }),
            loc: t.loc,
            phase: Phase::Trigger,
        });
    }

    // 7 — views (queries.md §8.2). A view is a real object, not a macro:
    // a DBA can query it, `\d` lists it, and migrations track it.
    for v in ordered_views(model) {
        let Some(body) = &v.body else { continue };
        out.push(Statement {
            sql: create_view(&v.qualified(), body),
            loc: v.loc,
            phase: Phase::View,
        });
    }

    // 8 — comments (schema.md §7)
    for (t, st) in model.tables.iter().zip(&snaps) {
        if let Some(text) = &st.comment {
            out.push(Statement {
                sql: comment_on("TABLE", &st.qualified(), Some(text)),
                loc: t.loc,
                phase: Phase::Comment,
            });
        }
        for (c, sc) in t.columns.iter().zip(&st.columns) {
            let Some(text) = &sc.comment else { continue };
            out.push(Statement {
                sql: comment_on(
                    "COLUMN",
                    &format!("{}.{}", st.qualified(), quote_ident(&sc.name)),
                    Some(text),
                ),
                loc: c.loc,
                phase: Phase::Comment,
            });
        }
    }
    for v in ordered_views(model) {
        if v.docs.is_empty() {
            continue;
        }
        out.push(Statement {
            sql: comment_on("VIEW", &v.qualified(), Some(&v.docs.join("\n"))),
            loc: v.loc,
            phase: Phase::Comment,
        });
    }

    out
}

// ── the renderers ──────────────────────────────────────────────────────
//
// Everything below takes snapshot types, so `gen-sql` and `jwc migrate`
// produce the same bytes for the same object by construction rather than
// by test.

pub fn create_schema(s: &SchemaSnapshot) -> String {
    format!("CREATE SCHEMA IF NOT EXISTS {};", quote_ident(&s.name))
}

pub fn create_type(e: &EnumSnapshot) -> String {
    let members = e
        .values
        .iter()
        .map(|m| sql_string(m))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TYPE {} AS ENUM ({members});", e.qualified())
}

/// A predicated `unique` has no table-constraint form; it is a unique index
/// (schema.md §4.3). Both paths go through the same lowering so the diff
/// and the DDL agree on what the object is.
pub fn unique_as_index(u: &crate::snapshot::UniqueSnapshot) -> IndexSnapshot {
    IndexSnapshot {
        name: u.name.clone(),
        columns: u
            .columns
            .iter()
            .map(|c| crate::snapshot::IndexColumnSnapshot {
                name: c.clone(),
                desc: false,
                nulls: None,
            })
            .collect(),
        predicate: u.predicate.clone(),
        unique: true,
        method: None,
    }
}

pub fn create_table(t: &TableSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();

    for c in &t.columns {
        parts.push(column_sql(c));
    }
    if let Some(pk) = &t.primary_key {
        parts.push(format!(
            "CONSTRAINT {} PRIMARY KEY ({})",
            quote_ident(&pk.name),
            idents(&pk.columns)
        ));
    }
    for u in &t.uniques {
        // Partial uniques are indexes (phase 5), not constraints.
        if u.predicate.is_some() {
            continue;
        }
        parts.push(format!(
            "CONSTRAINT {} UNIQUE ({})",
            quote_ident(&u.name),
            idents(&u.columns)
        ));
    }
    for c in &t.checks {
        parts.push(format!(
            "CONSTRAINT {} CHECK ({})",
            quote_ident(&c.name),
            c.expr
        ));
    }

    format!(
        "CREATE TABLE {} (\n    {}\n);",
        t.qualified(),
        parts.join(",\n    ")
    )
}

/// `ALTER TABLE … ADD COLUMN`, rendered from the same column writer that
/// `CREATE TABLE` uses — so a column added later is spelled exactly as one
/// created with the table.
pub fn add_column(qualified: &str, c: &ColumnSnapshot) -> String {
    format!("ALTER TABLE {qualified} ADD COLUMN {};", column_sql(c))
}

pub fn add_foreign_key(qualified: &str, fk: &ForeignKeySnapshot) -> String {
    let mut sql = format!(
        "ALTER TABLE {qualified} ADD CONSTRAINT {}\n    FOREIGN KEY ({}) REFERENCES {}.{} ({})",
        quote_ident(&fk.name),
        idents(&fk.columns),
        quote_ident(&fk.target_schema),
        quote_ident(&fk.target_table),
        idents(&fk.target_columns),
    );
    if let Some(a) = &fk.on_delete {
        sql.push_str(&format!("\n    ON DELETE {a}"));
    }
    if let Some(a) = &fk.on_update {
        sql.push_str(&format!("\n    ON UPDATE {a}"));
    }
    sql.push(';');
    sql
}

pub fn create_index(qualified: &str, ix: &IndexSnapshot) -> String {
    let cols = ix
        .columns
        .iter()
        .map(|c| {
            let mut s = quote_ident(&c.name);
            if c.desc {
                s.push_str(" DESC");
            }
            match c.nulls.as_deref() {
                Some("FIRST") => s.push_str(" NULLS FIRST"),
                Some("LAST") => s.push_str(" NULLS LAST"),
                _ => {}
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ");
    let using = ix
        .method
        .as_ref()
        .map(|m| format!(" USING {m}"))
        .unwrap_or_default();
    let unique = if ix.unique { "UNIQUE " } else { "" };
    let mut sql = format!(
        "CREATE {unique}INDEX {}\n    ON {qualified}{using} ({cols})",
        quote_ident(&ix.name)
    );
    if let Some(p) = &ix.predicate {
        sql.push_str(&format!("\n    WHERE {p}"));
    }
    sql.push(';');
    sql
}

pub fn create_function(f: &FunctionSnapshot) -> String {
    let assigns = f
        .sets_now
        .iter()
        .map(|c| format!("  NEW.{} := now();", quote_ident(c)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "CREATE OR REPLACE FUNCTION {}.{}()\nRETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n{assigns}\n  RETURN NEW;\nEND $$;",
        quote_ident(&f.schema),
        quote_ident(&f.name)
    )
}

pub fn create_trigger(t: &TriggerSnapshot) -> String {
    format!(
        "CREATE TRIGGER {}\n    {} {} ON {}.{}\n    FOR EACH ROW EXECUTE FUNCTION {}.{}();",
        quote_ident(&t.name),
        t.timing,
        t.event,
        quote_ident(&t.schema),
        quote_ident(&t.table),
        quote_ident(&t.schema),
        quote_ident(&t.function)
    )
}

pub fn create_view(qualified: &str, body: &str) -> String {
    format!("CREATE VIEW {qualified} AS\n{body};")
}

/// `COMMENT ON <kind> <object> IS …`. `None` removes the comment, which is
/// what an edited-away doc comment has to lower to — dropping the statement
/// instead would leave the old text on a live database forever.
pub fn comment_on(kind: &str, object: &str, text: Option<&str>) -> String {
    match text {
        Some(t) => format!("COMMENT ON {kind} {object} IS {};", sql_string(t)),
        None => format!("COMMENT ON {kind} {object} IS NULL;"),
    }
}

/// Views in dependency order: one that reads another comes after it.
///
/// `CREATE VIEW` resolves its source at creation time, so the order is not
/// cosmetic — the wrong one fails to apply. The list is already sorted, so
/// ties keep that order and the output stays byte-stable.
fn ordered_views(model: &SchemaModel) -> Vec<&crate::views::ViewObj> {
    let mut done: Vec<&crate::views::ViewObj> = Vec::new();
    let mut left: Vec<&crate::views::ViewObj> = model.views.iter().collect();
    while !left.is_empty() {
        let before = left.len();
        left.retain(|v| {
            let ready = v.reads.iter().all(|dep| {
                !model.views.iter().any(|o| &o.declared == dep)
                    || done.iter().any(|d| &d.declared == dep)
            });
            if ready {
                done.push(v);
            }
            !ready
        });
        if left.len() == before {
            // A cycle between views is not emittable; the remaining ones go
            // out in declaration order so the failure is Postgres's, with
            // the offending statement named, rather than a silent drop.
            done.append(&mut left);
        }
    }
    done
}

fn column_sql(c: &ColumnSnapshot) -> String {
    let mut s = format!("{} {}", quote_ident(&c.name), c.ty);
    if c.identity {
        // GENERATED BY DEFAULT, not bigserial: the sequence is owned by the
        // column, and explicit inserts still work for seeding and backfills
        // (schema.md §2.3).
        s.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    }
    if let Some(d) = &c.default {
        s.push_str(&format!(" DEFAULT {d}"));
    }
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    s
}

fn idents(cols: &[String]) -> String {
    cols.iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sql_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Render a whole script. With `explain`, each statement is preceded by a
/// `-- file:line` comment (schema.md §9.1); every statement carries one, and
/// a statement with no location is a compiler bug the golden test catches.
pub fn render(ws: &Workspace, statements: &[Statement], explain: bool) -> String {
    let mut out = String::new();
    out.push_str("-- Generated by `jwc v1 gen-sql`. Do not edit.\n");
    out.push_str("-- Emission order: schema, enum type, table, foreign key, index, trigger, view, comment.\n");
    let mut phase = None;
    for st in statements {
        if phase != Some(st.phase) {
            out.push_str(&format!("\n-- ── {:?} ──\n", st.phase));
            phase = Some(st.phase);
        }
        if explain {
            out.push_str(&format!("-- {}\n", ws.file_line(st.loc)));
        }
        out.push_str(&st.sql);
        out.push_str("\n\n");
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The set of columns a default (raw-path) SELECT may read: everything that
/// is not `private` (schema.md §3.1). Lives here rather than in the query
/// compiler because the rule is a property of the schema.
pub fn readable_columns(t: &TableObj) -> Vec<&ColumnObj> {
    t.columns.iter().filter(|c| !c.private).collect()
}

/// Wire-form cast for the raw path: `bigint` and `numeric` go out as JSON
/// strings on both the raw and the record path (types.md §2.3), so the raw
/// projection casts rather than trusting `row_to_json`.
pub fn wire_cast(ty: &SqlType) -> Option<&'static str> {
    match ty {
        SqlType::Scalar(s) if s == "bigint" => Some("text"),
        SqlType::Scalar(s) if s == "numeric" || s.starts_with("numeric(") => Some("text"),
        _ => None,
    }
}

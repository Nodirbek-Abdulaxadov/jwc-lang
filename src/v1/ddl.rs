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

use super::model::{ColumnObj, SchemaModel, SqlType, TableObj};
use super::naming::quote_ident;
use super::workspace::{Loc, Workspace};
use crate::v1::ast::RefAction;

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

    // 1 — schemas
    for s in &model.schemas {
        out.push(Statement {
            sql: format!("CREATE SCHEMA IF NOT EXISTS {};", quote_ident(&s.physical)),
            loc: s.loc,
            phase: Phase::Schema,
        });
    }

    // 2 — enum types (only the `of` form creates one; schema.md §5)
    for e in &model.enums {
        let Some(schema) = &e.schema else { continue };
        let members = e
            .members
            .iter()
            .map(|m| sql_string(m))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Statement {
            sql: format!(
                "CREATE TYPE {}.{} AS ENUM ({members});",
                quote_ident(schema),
                quote_ident(&e.physical)
            ),
            loc: e.loc,
            phase: Phase::EnumType,
        });
    }

    // 3 — tables: columns, primary key, checks, and non-partial uniques.
    //     Foreign keys deliberately excluded (phase 4).
    for t in &model.tables {
        out.push(Statement {
            sql: create_table(t),
            loc: t.loc,
            phase: Phase::Table,
        });
    }

    // 4 — every foreign key, after every table exists.
    for t in &model.tables {
        for fk in &t.foreign_keys {
            let mut sql = format!(
                "ALTER TABLE {} ADD CONSTRAINT {}\n    FOREIGN KEY ({}) REFERENCES {}.{} ({})",
                t.qualified(),
                quote_ident(&fk.name),
                idents(&fk.columns),
                quote_ident(&fk.target_schema),
                quote_ident(&fk.target_table),
                idents(&fk.target_columns),
            );
            if let Some(a) = fk.on_delete {
                sql.push_str(&format!("\n    ON DELETE {}", action_sql(a)));
            }
            if let Some(a) = fk.on_update {
                sql.push_str(&format!("\n    ON UPDATE {}", action_sql(a)));
            }
            sql.push(';');
            out.push(Statement {
                sql,
                loc: fk.loc,
                phase: Phase::ForeignKey,
            });
        }
    }

    // 5 — indexes. A partial unique is an index, not a table constraint
    //     (schema.md §4.3).
    for t in &model.tables {
        for u in &t.uniques {
            let Some(pred) = &u.predicate else { continue };
            out.push(Statement {
                sql: format!(
                    "CREATE UNIQUE INDEX {}\n    ON {} ({})\n    WHERE {pred};",
                    quote_ident(&u.name),
                    t.qualified(),
                    idents(&u.columns)
                ),
                loc: u.loc,
                phase: Phase::Index,
            });
        }
        for ix in &t.indexes {
            let cols = ix
                .columns
                .iter()
                .map(|c| {
                    let mut s = quote_ident(&c.physical);
                    if c.desc {
                        s.push_str(" DESC");
                    }
                    match c.nulls {
                        Some(crate::v1::ast::NullsOrder::First) => s.push_str(" NULLS FIRST"),
                        Some(crate::v1::ast::NullsOrder::Last) => s.push_str(" NULLS LAST"),
                        None => {}
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
            let mut sql = format!(
                "CREATE INDEX {}\n    ON {}{using} ({cols})",
                quote_ident(&ix.name),
                t.qualified()
            );
            if let Some(p) = &ix.predicate {
                sql.push_str(&format!("\n    WHERE {p}"));
            }
            sql.push(';');
            out.push(Statement {
                sql,
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
        let fname = super::naming::touch_function(&t.physical);
        let assigns = t
            .touch_columns
            .iter()
            .map(|c| format!("  NEW.{} := now();", quote_ident(c)))
            .collect::<Vec<_>>()
            .join("\n");
        out.push(Statement {
            sql: format!(
                "CREATE OR REPLACE FUNCTION {}.{}()\nRETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n{assigns}\n  RETURN NEW;\nEND $$;",
                quote_ident(&t.schema_physical),
                quote_ident(&fname)
            ),
            loc: t.loc,
            phase: Phase::Trigger,
        });
        out.push(Statement {
            sql: format!(
                "CREATE TRIGGER {}\n    BEFORE UPDATE ON {}\n    FOR EACH ROW EXECUTE FUNCTION {}.{}();",
                quote_ident(&fname),
                t.qualified(),
                quote_ident(&t.schema_physical),
                quote_ident(&fname)
            ),
            loc: t.loc,
            phase: Phase::Trigger,
        });
    }

    // 7 — views (queries.md §8.2). A view is a real object, not a macro:
    // a DBA can query it, `\d` lists it, and migrations track it.
    for v in ordered_views(model) {
        let Some(body) = &v.body else { continue };
        out.push(Statement {
            sql: format!("CREATE VIEW {} AS\n{body};", v.qualified()),
            loc: v.loc,
            phase: Phase::View,
        });
    }

    // 8 — comments (schema.md §7)
    for t in &model.tables {
        if !t.docs.is_empty() {
            out.push(Statement {
                sql: format!(
                    "COMMENT ON TABLE {} IS {};",
                    t.qualified(),
                    sql_string(&t.docs.join("\n"))
                ),
                loc: t.loc,
                phase: Phase::Comment,
            });
        }
        for c in &t.columns {
            if c.docs.is_empty() {
                continue;
            }
            out.push(Statement {
                sql: format!(
                    "COMMENT ON COLUMN {}.{} IS {};",
                    t.qualified(),
                    quote_ident(&c.physical),
                    sql_string(&c.docs.join("\n"))
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
            sql: format!(
                "COMMENT ON VIEW {} IS {};",
                v.qualified(),
                sql_string(&v.docs.join("\n"))
            ),
            loc: v.loc,
            phase: Phase::Comment,
        });
    }

    out
}

/// Views in dependency order: one that reads another comes after it.
///
/// `CREATE VIEW` resolves its source at creation time, so the order is not
/// cosmetic — the wrong one fails to apply. The list is already sorted, so
/// ties keep that order and the output stays byte-stable.
fn ordered_views(model: &SchemaModel) -> Vec<&super::views::ViewObj> {
    let mut done: Vec<&super::views::ViewObj> = Vec::new();
    let mut left: Vec<&super::views::ViewObj> = model.views.iter().collect();
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

fn create_table(t: &TableObj) -> String {
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

fn column_sql(c: &ColumnObj) -> String {
    let mut s = format!("{} {}", quote_ident(&c.physical), c.ty.render());
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

fn action_sql(a: RefAction) -> &'static str {
    a.as_sql()
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
    out.push_str("-- Emission order: schema, enum type, table, foreign key, index, trigger, comment.\n");
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

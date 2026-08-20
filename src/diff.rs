//! The diff — two snapshots in, a list of typed operations out.
//!
//! `jwc migrate new` never connects to a database (migrations.md §1), so
//! "what changed" is decided entirely between the previous `.snapshot.json`
//! and the schema the source describes now. This module is that decision;
//! `migrate.rs` lowers the result to SQL in phase order (§4).
//!
//! ## Three rules that shape everything here
//!
//! **Renames are declared, never inferred** (§6). A column that disappears
//! and a column that appears are a drop and an add — total data loss — and
//! the generator will not guess otherwise. `was` is the only way to say
//! "these are the same column", and a `was` that names nothing is `E1103`
//! rather than a silent no-op.
//!
//! **Constraints and indexes are matched structurally, not by name.** Their
//! names are generated from table + columns + canonical predicate
//! (schema.md §8), so renaming a column would otherwise drop and rebuild
//! every constraint on it — a long lock on a large table for a cosmetic
//! change. Matching on the body instead turns that into `RENAME CONSTRAINT`.
//! It is also what lets §2's promise hold: when the snapshot was written
//! under an older naming scheme, the match still succeeds and the rename is
//! *suppressed*, so adopting a `v2` scheme does not rename live constraints.
//!
//! **A replacement drops in phase 4, not phase 9.** Two objects can share a
//! name and differ in body — a check whose expression was edited keeps its
//! name (it is numbered by ordinal), and so does an index whose `nulls`
//! order changed. Emitting the add in phase 4 and the drop in phase 9 would
//! try to create the new one while the old still holds the name. So a drop
//! that exists only to make room for an add travels with the add.

use crate::diag::Diagnostic;
use crate::model::SchemaModel;
use crate::snapshot::*;
use crate::workspace::Loc;
use std::collections::BTreeMap;

/// The ten-phase order of migrations.md §4. A migration file emits phase by
/// phase, and each phase is internally sorted, so two runs on the same
/// inputs are byte-identical (§10.1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Phase {
    /// Every view whose dependencies this migration touches.
    DropView = 0,
    /// `CREATE SCHEMA`, `CREATE TYPE`.
    Create = 1,
    /// `CREATE TABLE`, `ADD COLUMN`, renames, type changes, defaults,
    /// nullability.
    Column = 2,
    /// Data movement — the author's `.data.sql` sidecar (§7.2).
    Data = 3,
    /// `ADD CONSTRAINT` for PK, unique and check, then all foreign keys.
    Constraint = 4,
    Index = 5,
    Trigger = 6,
    Comment = 7,
    /// Everything phase 0 dropped, in dependency order.
    CreateView = 8,
    /// `DROP CONSTRAINT`, `DROP INDEX`, `DROP COLUMN`, `DROP TABLE`. Last,
    /// so a failure anywhere earlier leaves data intact.
    Destructive = 9,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Constraint {
    PrimaryKey(PrimaryKeySnapshot),
    /// Never predicated: a partial unique has no table-constraint form and
    /// travels as a unique index (schema.md §4.3).
    Unique(UniqueSnapshot),
    Check(CheckSnapshot),
    ForeignKey(ForeignKeySnapshot),
}

impl Constraint {
    pub fn name(&self) -> &str {
        match self {
            Constraint::PrimaryKey(p) => &p.name,
            Constraint::Unique(u) => &u.name,
            Constraint::Check(c) => &c.name,
            Constraint::ForeignKey(f) => &f.name,
        }
    }

    /// Everything except the name. Two constraints with equal bodies are
    /// the same constraint however they are called.
    fn body(&self) -> String {
        match self {
            Constraint::PrimaryKey(p) => format!("pk:{}", p.columns.join(",")),
            Constraint::Unique(u) => format!("uq:{}", u.columns.join(",")),
            Constraint::Check(c) => format!("ck:{}", c.expr),
            Constraint::ForeignKey(f) => format!(
                "fk:{}->{}.{}({}):{}:{}",
                f.columns.join(","),
                f.target_schema,
                f.target_table,
                f.target_columns.join(","),
                f.on_delete.as_deref().unwrap_or("-"),
                f.on_update.as_deref().unwrap_or("-"),
            ),
        }
    }

    /// Foreign keys go out after every other constraint (§4, phase 4): the
    /// table they point at may be created by this same migration.
    fn is_fk(&self) -> bool {
        matches!(self, Constraint::ForeignKey(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommentTarget {
    Table {
        schema: String,
        name: String,
    },
    Column {
        schema: String,
        table: String,
        name: String,
    },
    View {
        schema: String,
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    CreateSchema(SchemaSnapshot),
    CreateEnum(EnumSnapshot),
    /// `before` places the value in the declared position. Appending is the
    /// common case; a value inserted in the middle of the declaration gets
    /// `BEFORE` so the type's order in Postgres matches the source, and a
    /// later `\dT+` shows what was written.
    AddEnumValue {
        schema: String,
        name: String,
        value: String,
        before: Option<String>,
    },
    CreateTable(TableSnapshot),
    RenameTable {
        schema: String,
        from: String,
        to: String,
    },
    AddColumn {
        schema: String,
        table: String,
        column: ColumnSnapshot,
    },
    RenameColumn {
        schema: String,
        table: String,
        from: String,
        to: String,
    },
    AlterColumnType {
        schema: String,
        table: String,
        column: String,
        from: String,
        to: String,
    },
    SetNotNull {
        schema: String,
        table: String,
        column: String,
    },
    DropNotNull {
        schema: String,
        table: String,
        column: String,
    },
    SetDefault {
        schema: String,
        table: String,
        column: String,
        value: String,
    },
    DropDefault {
        schema: String,
        table: String,
        column: String,
    },
    SetIdentity {
        schema: String,
        table: String,
        column: String,
    },
    DropIdentity {
        schema: String,
        table: String,
        column: String,
    },
    AddConstraint {
        schema: String,
        table: String,
        constraint: Constraint,
    },
    /// `replaced` marks a drop that exists only to make room for an add of
    /// the same name — see the module note. It travels in phase 4.
    DropConstraint {
        schema: String,
        table: String,
        name: String,
        replaced: bool,
    },
    RenameConstraint {
        schema: String,
        table: String,
        from: String,
        to: String,
    },
    CreateIndex {
        schema: String,
        table: String,
        index: IndexSnapshot,
    },
    DropIndex {
        schema: String,
        name: String,
        replaced: bool,
    },
    RenameIndex {
        schema: String,
        from: String,
        to: String,
    },
    CreateFunction(FunctionSnapshot),
    DropFunction {
        schema: String,
        name: String,
    },
    CreateTrigger(TriggerSnapshot),
    DropTrigger {
        schema: String,
        table: String,
        name: String,
        replaced: bool,
    },
    CommentOn {
        target: CommentTarget,
        text: Option<String>,
    },
    CreateView(ViewSnapshot),
    DropView {
        schema: String,
        name: String,
    },
    DropColumn {
        schema: String,
        table: String,
        column: ColumnSnapshot,
    },
    DropTable(TableSnapshot),
}

impl Op {
    /// The operation's name in migrations.md §3, which is also what
    /// `migrate new --explain` prints.
    pub fn kind(&self) -> &'static str {
        match self {
            Op::CreateSchema(_) => "create_schema",
            Op::CreateEnum(_) => "create_enum",
            Op::AddEnumValue { .. } => "add_enum_value",
            Op::CreateTable(_) => "create_table",
            Op::RenameTable { .. } => "rename_table",
            Op::AddColumn { .. } => "add_column",
            Op::RenameColumn { .. } => "rename_column",
            Op::AlterColumnType { .. } => "alter_column_type",
            Op::SetNotNull { .. } => "set_not_null",
            Op::DropNotNull { .. } => "drop_not_null",
            Op::SetDefault { .. } => "set_default",
            Op::DropDefault { .. } => "drop_default",
            Op::SetIdentity { .. } => "set_identity",
            Op::DropIdentity { .. } => "drop_identity",
            Op::AddConstraint { .. } => "add_constraint",
            Op::DropConstraint { .. } => "drop_constraint",
            Op::RenameConstraint { .. } => "rename_constraint",
            Op::CreateIndex { .. } => "create_index",
            Op::DropIndex { .. } => "drop_index",
            Op::RenameIndex { .. } => "rename_index",
            Op::CreateFunction(_) => "create_function",
            Op::DropFunction { .. } => "drop_function",
            Op::CreateTrigger(_) => "create_trigger",
            Op::DropTrigger { .. } => "drop_trigger",
            Op::CommentOn { .. } => "comment_on",
            Op::CreateView(_) => "create_view",
            Op::DropView { .. } => "drop_view",
            Op::DropColumn { .. } => "drop_column",
            Op::DropTable(_) => "drop_table",
        }
    }

    pub fn phase(&self) -> Phase {
        match self {
            Op::DropView { .. } => Phase::DropView,
            Op::CreateSchema(_) | Op::CreateEnum(_) | Op::AddEnumValue { .. } => Phase::Create,
            Op::CreateTable(_)
            | Op::RenameTable { .. }
            | Op::AddColumn { .. }
            | Op::RenameColumn { .. }
            | Op::AlterColumnType { .. }
            | Op::SetNotNull { .. }
            | Op::DropNotNull { .. }
            | Op::SetDefault { .. }
            | Op::DropDefault { .. }
            | Op::SetIdentity { .. }
            | Op::DropIdentity { .. } => Phase::Column,
            Op::AddConstraint { .. } | Op::RenameConstraint { .. } => Phase::Constraint,
            Op::DropConstraint { replaced, .. } => {
                if *replaced {
                    Phase::Constraint
                } else {
                    Phase::Destructive
                }
            }
            Op::CreateIndex { .. } | Op::RenameIndex { .. } => Phase::Index,
            Op::DropIndex { replaced, .. } => {
                if *replaced {
                    Phase::Index
                } else {
                    Phase::Destructive
                }
            }
            Op::CreateFunction(_) | Op::CreateTrigger(_) => Phase::Trigger,
            Op::DropTrigger { replaced, .. } => {
                if *replaced {
                    Phase::Trigger
                } else {
                    Phase::Destructive
                }
            }
            Op::DropFunction { .. } => Phase::Destructive,
            // A comment on a view goes out with the view, not in phase 7:
            // `DROP VIEW` takes the comment with it, and `COMMENT ON VIEW`
            // in phase 7 would name an object phase 8 has not created yet.
            Op::CommentOn {
                target: CommentTarget::View { .. },
                ..
            } => Phase::CreateView,
            Op::CommentOn { .. } => Phase::Comment,
            Op::CreateView(_) => Phase::CreateView,
            Op::DropColumn { .. } | Op::DropTable(_) => Phase::Destructive,
        }
    }

    /// `ALTER TYPE … ADD VALUE` cannot run inside a transaction block, so a
    /// migration containing one is emitted as its own file (§5.2).
    pub fn needs_no_transaction(&self) -> bool {
        matches!(self, Op::AddEnumValue { .. })
    }

    /// Whether this operation can be undone. A `down` migration for one
    /// that cannot gets a marker instead of a lie (§9.2).
    pub fn reversible(&self) -> bool {
        !matches!(
            self,
            Op::AddEnumValue { .. } | Op::DropColumn { .. } | Op::DropTable(_)
        )
    }

    /// Ordering within a phase. Foreign keys sort after everything else in
    /// phase 4; a drop that makes room sorts before the add that needs it;
    /// renames sort before the alters that use the new name.
    fn rank(&self) -> u8 {
        match self {
            Op::RenameTable { .. } => 0,
            Op::RenameColumn { .. } => 1,
            Op::CreateTable(_) => 2,
            Op::AddColumn { .. } => 3,
            Op::DropConstraint { .. } | Op::DropIndex { .. } => 0,
            Op::RenameConstraint { .. } | Op::RenameIndex { .. } => 1,
            Op::AddConstraint { constraint, .. } if constraint.is_fk() => 3,
            Op::AddConstraint { .. } => 2,
            Op::DropTrigger { .. } => 0,
            Op::DropFunction { .. } => 1,
            // The trigger names the function, so the function goes first.
            Op::CreateFunction(_) => 2,
            Op::CreateTrigger(_) => 3,
            Op::DropColumn { .. } => 2,
            Op::DropTable(_) => 3,
            _ => 4,
        }
    }

    /// A total, stable tiebreak inside a rank.
    fn sort_key(&self) -> String {
        match self {
            Op::CreateSchema(s) => s.name.clone(),
            Op::CreateEnum(e) => format!("{}.{}", e.schema, e.name),
            Op::AddEnumValue {
                schema,
                name,
                value,
                ..
            } => format!("{schema}.{name}.{value}"),
            Op::CreateTable(t) | Op::DropTable(t) => format!("{}.{}", t.schema, t.name),
            Op::RenameTable { schema, to, .. } => format!("{schema}.{to}"),
            Op::AddColumn {
                schema,
                table,
                column,
            }
            | Op::DropColumn {
                schema,
                table,
                column,
            } => format!("{schema}.{table}.{}", column.name),
            Op::RenameColumn {
                schema, table, to, ..
            } => format!("{schema}.{table}.{to}"),
            Op::AlterColumnType {
                schema,
                table,
                column,
                ..
            }
            | Op::SetNotNull {
                schema,
                table,
                column,
            }
            | Op::DropNotNull {
                schema,
                table,
                column,
            }
            | Op::SetDefault {
                schema,
                table,
                column,
                ..
            }
            | Op::DropDefault {
                schema,
                table,
                column,
            }
            | Op::SetIdentity {
                schema,
                table,
                column,
            }
            | Op::DropIdentity {
                schema,
                table,
                column,
            } => format!("{schema}.{table}.{column}"),
            Op::AddConstraint {
                schema,
                table,
                constraint,
            } => format!("{schema}.{table}.{}", constraint.name()),
            Op::DropConstraint {
                schema,
                table,
                name,
                ..
            } => format!("{schema}.{table}.{name}"),
            Op::RenameConstraint {
                schema, table, to, ..
            } => format!("{schema}.{table}.{to}"),
            Op::CreateIndex { schema, index, .. } => format!("{schema}.{}", index.name),
            Op::DropIndex { schema, name, .. } => format!("{schema}.{name}"),
            Op::RenameIndex { schema, to, .. } => format!("{schema}.{to}"),
            Op::CreateFunction(f) => format!("{}.{}", f.schema, f.name),
            Op::DropFunction { schema, name } => format!("{schema}.{name}"),
            Op::CreateTrigger(t) => format!("{}.{}.{}", t.schema, t.table, t.name),
            Op::DropTrigger {
                schema,
                table,
                name,
                ..
            } => format!("{schema}.{table}.{name}"),
            Op::CommentOn { target, .. } => match target {
                CommentTarget::Table { schema, name } => format!("{schema}.{name}"),
                CommentTarget::Column {
                    schema,
                    table,
                    name,
                } => format!("{schema}.{table}.{name}"),
                CommentTarget::View { schema, name } => format!("{schema}.{name}"),
            },
            Op::CreateView(v) => format!("{}.{}", v.schema, v.name),
            Op::DropView { schema, name } => format!("{schema}.{name}"),
        }
    }

    /// One line for `migrate new --explain`.
    pub fn describe(&self) -> String {
        match self {
            Op::AddEnumValue {
                schema,
                name,
                value,
                before,
            } => match before {
                Some(b) => format!("add_enum_value {schema}.{name} '{value}' before '{b}'"),
                None => format!("add_enum_value {schema}.{name} '{value}'"),
            },
            Op::RenameTable { schema, from, to } => {
                format!("rename_table {schema}.{from} -> {to}")
            }
            Op::RenameColumn {
                schema,
                table,
                from,
                to,
            } => format!("rename_column {schema}.{table}.{from} -> {to}"),
            Op::RenameConstraint {
                schema,
                table,
                from,
                to,
            } => format!("rename_constraint {schema}.{table}.{from} -> {to}"),
            Op::RenameIndex { schema, from, to } => {
                format!("rename_index {schema}.{from} -> {to}")
            }
            Op::AlterColumnType { from, to, .. } => {
                format!("alter_column_type {} {from} -> {to}", self.sort_key())
            }
            _ => format!("{} {}", self.kind(), self.sort_key()),
        }
    }
}

/// One operation and the declaration that caused it.
///
/// A drop has no location: nothing in the source causes it — its cause is an
/// absence. `--explain` prints `(removed)` for those rather than inventing a
/// line number.
#[derive(Clone, Debug)]
pub struct Change {
    pub op: Op,
    pub loc: Option<Loc>,
    /// Position within the phase. Defaults to `Op::rank`; view operations
    /// override it with their depth in the dependency graph, because that
    /// order is not cosmetic — `CREATE VIEW` resolves its sources at
    /// creation time, so the wrong one fails to apply.
    order: u16,
}

/// What the diff needs from the source but the snapshot deliberately does
/// not carry: declared renames and source locations.
///
/// Defaulted, `compute` becomes a pure snapshot-to-snapshot function with no
/// renames and no locations — which is the form the round-trip property test
/// uses.
#[derive(Default, Debug)]
pub struct Source {
    /// `"schema.table"` -> the old physical table name.
    pub table_was: BTreeMap<String, String>,
    /// `"schema.table.column"` -> the old physical column name.
    pub column_was: BTreeMap<String, String>,
    /// `"schema.name"` and `"schema.table.column"` -> the declaration.
    pub locs: BTreeMap<String, Loc>,
}

impl Source {
    /// Pull the `was` markers and locations out of a resolved schema.
    pub fn of(model: &SchemaModel) -> Source {
        let mut src = Source::default();
        for s in &model.schemas {
            src.locs.insert(s.physical.clone(), s.loc);
        }
        for e in &model.enums {
            if let Some(schema) = &e.schema {
                src.locs.insert(format!("{schema}.{}", e.physical), e.loc);
            }
        }
        for t in &model.tables {
            let key = format!("{}.{}", t.schema_physical, t.physical);
            src.locs.insert(key.clone(), t.loc);
            if let Some(old) = &t.was {
                src.table_was
                    .insert(key.clone(), crate::naming::physical(old));
            }
            for c in &t.columns {
                let ckey = format!("{key}.{}", c.physical);
                src.locs.insert(ckey.clone(), c.loc);
                if let Some(old) = &c.was {
                    src.column_was.insert(ckey, crate::naming::physical(old));
                }
            }
        }
        for v in &model.views {
            src.locs
                .insert(format!("{}.{}", v.schema_physical, v.physical), v.loc);
        }
        src
    }

    fn loc(&self, key: &str) -> Option<Loc> {
        self.locs.get(key).copied()
    }
}

pub struct Diff {
    pub changes: Vec<Change>,
    pub diags: Vec<(Loc, Diagnostic)>,
    /// The state the database will be in once this migration is applied —
    /// which is `next`, except where an operation was refused. That is what
    /// gets written to `.snapshot.json`, so a refusal does not silently
    /// become "already done" on the following run.
    pub effective: Snapshot,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.diags
            .iter()
            .any(|(_, d)| d.severity == crate::diag::Severity::Error)
    }

    pub fn phase(&self, phase: Phase) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(move |c| c.op.phase() == phase)
    }

    /// Any operation that cannot share a transaction with the others (§5.2).
    pub fn needs_no_transaction(&self) -> bool {
        self.changes.iter().any(|c| c.op.needs_no_transaction())
    }
}

// ── the diff ───────────────────────────────────────────────────────────

/// Everything that has to happen to turn `prev` into `next`.
pub fn compute(prev: &Snapshot, next: &Snapshot, src: &Source) -> Diff {
    let mut d = Builder {
        prev: prev.clone(),
        next,
        src,
        // The naming scheme the previous snapshot was written under. When
        // it differs, generated names are left alone: schema.md §8.2 records
        // the version precisely so that adopting a new scheme does not
        // rename constraints on a live database.
        may_rename_generated: prev.scheme == next.scheme,
        changes: Vec::new(),
        diags: Vec::new(),
        effective: next.clone(),
        touched: Vec::new(),
    };

    d.renames();
    d.schemas();
    d.enums();
    d.tables();
    d.functions_and_triggers();
    d.views();

    let mut changes = d.changes;
    changes.sort_by(|a, b| {
        (a.op.phase(), a.order, a.op.sort_key()).cmp(&(b.op.phase(), b.order, b.op.sort_key()))
    });
    Diff {
        changes,
        diags: d.diags,
        effective: d.effective,
    }
}

struct Builder<'a> {
    /// A working copy: renames are applied to it first, so everything after
    /// compares like for like.
    prev: Snapshot,
    next: &'a Snapshot,
    src: &'a Source,
    may_rename_generated: bool,
    changes: Vec<Change>,
    diags: Vec<(Loc, Diagnostic)>,
    effective: Snapshot,
    /// The declared name of every relation this migration alters.
    touched: Vec<String>,
}

fn key2(a: &str, b: &str) -> String {
    format!("{a}.{b}")
}

impl Builder<'_> {
    fn push(&mut self, op: Op, loc: Option<Loc>) {
        let order = op.rank() as u16;
        self.changes.push(Change { op, loc, order });
    }

    fn push_ordered(&mut self, op: Op, loc: Option<Loc>, order: u16) {
        self.changes.push(Change { op, loc, order });
    }

    /// Record that this migration alters a relation. Phase 0 drops every
    /// view that reads one, phase 8 rebuilds it. Declared names, because
    /// that is what a view's `reads` holds.
    fn touch(&mut self, declared: &str) {
        if !self.touched.iter().any(|t| t == declared) {
            self.touched.push(declared.to_string());
        }
    }

    fn err(&mut self, loc: Option<Loc>, code: &'static str, message: String, clause: &'static str) {
        let Some(loc) = loc else { return };
        self.diags.push((
            loc,
            Diagnostic::error(code, loc.span, message).clause(clause),
        ));
    }

    fn warn(
        &mut self,
        loc: Option<Loc>,
        code: &'static str,
        message: String,
        clause: &'static str,
    ) {
        let Some(loc) = loc else { return };
        self.diags.push((
            loc,
            Diagnostic::warning(code, loc.span, message).clause(clause),
        ));
    }

    // ── §6: renames are declared ───────────────────────────────────────

    fn renames(&mut self) {
        for t in &self.next.tables {
            let key = key2(&t.schema, &t.name);
            let loc = self.src.loc(&key);

            if let Some(old) = self.src.table_was.get(&key).cloned() {
                if self.prev.table(&t.schema, &t.name).is_some() {
                    // The rename already happened; the marker outlived the
                    // migration that used it (§6.4).
                    self.warn(
                        loc,
                        "W1101",
                        format!(
                            "`was \"{old}\"` on table `{}` has already been applied",
                            t.declared
                        ),
                        "migrations.md §6.4",
                    );
                } else if let Some(p) = self
                    .prev
                    .tables
                    .iter_mut()
                    .find(|p| p.schema == t.schema && p.name == old)
                {
                    p.name = t.name.clone();
                    self.push(
                        Op::RenameTable {
                            schema: t.schema.clone(),
                            from: old,
                            to: t.name.clone(),
                        },
                        loc,
                    );
                    self.touch(&t.declared);
                } else {
                    self.err(
                        loc,
                        "E1103",
                        format!("`was \"{old}\"` names no table in the previous snapshot"),
                        "migrations.md §6.3",
                    );
                }
            }

            // Column renames read the table under its *new* name, so a table
            // and a column renamed in the same migration both land.
            let Some(pt) = self.prev.table(&t.schema, &t.name).cloned() else {
                continue;
            };
            for c in &t.columns {
                let ckey = format!("{key}.{}", c.name);
                let Some(old) = self.src.column_was.get(&ckey).cloned() else {
                    continue;
                };
                let cloc = self.src.loc(&ckey);
                if pt.column(&c.name).is_some() {
                    self.warn(
                        cloc,
                        "W1101",
                        format!(
                            "`was \"{old}\"` on column `{}` has already been applied",
                            c.declared
                        ),
                        "migrations.md §6.4",
                    );
                    continue;
                }
                let Some(pc) = pt.column(&old).cloned() else {
                    self.err(
                        cloc,
                        "E1103",
                        format!("`was \"{old}\"` names no column in the previous snapshot"),
                        "migrations.md §6.3",
                    );
                    continue;
                };
                if pc.ty != c.ty {
                    // §6.5. Both halves are separable and the combined
                    // failure is not diagnosable from the resulting error.
                    self.err(
                        cloc,
                        "E1104",
                        format!(
                            "`{}` is renamed from `{old}` and changes type from `{}` to `{}` \
                             in the same migration",
                            c.declared, pc.ty, c.ty
                        ),
                        "migrations.md §6.5",
                    );
                }
                if let Some(p) = self
                    .prev
                    .tables
                    .iter_mut()
                    .find(|p| p.schema == t.schema && p.name == t.name)
                {
                    if let Some(col) = p.columns.iter_mut().find(|x| x.name == old) {
                        col.name = c.name.clone();
                    }
                }
                self.push(
                    Op::RenameColumn {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        from: old,
                        to: c.name.clone(),
                    },
                    cloc,
                );
                self.touch(&t.declared);
            }
        }
    }

    // ── schemas ────────────────────────────────────────────────────────

    fn schemas(&mut self) {
        for s in &self.next.schemas {
            if self.prev.schemas.iter().any(|p| p.name == s.name) {
                continue;
            }
            let loc = self.src.loc(&s.name);
            self.push(Op::CreateSchema(s.clone()), loc);
        }
        // A schema is never dropped. It disappears from the source when the
        // last table in it does, and dropping it would take anything a DBA
        // put there by hand with it. migrations.md §3 lists no such
        // operation for exactly that reason.
    }

    // ── §5: enums ──────────────────────────────────────────────────────

    fn enums(&mut self) {
        for e in &self.next.enums {
            let key = key2(&e.schema, &e.name);
            let loc = self.src.loc(&key);
            let Some(p) = self.prev.enum_type(&e.schema, &e.name).cloned() else {
                self.push(Op::CreateEnum(e.clone()), loc);
                continue;
            };
            if p.values == e.values {
                continue;
            }

            let removed: Vec<&String> = p.values.iter().filter(|v| !e.values.contains(v)).collect();
            if !removed.is_empty() {
                let list = removed
                    .iter()
                    .map(|v| format!("'{v}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.err(
                    loc,
                    "E1102",
                    format!("enum `{}` removes value {list}", e.declared),
                    "migrations.md §5.3",
                );
                self.err_note_enum_recipe(&p, &removed);
                self.keep_enum(&p);
                continue;
            }

            let added: Vec<String> = e
                .values
                .iter()
                .filter(|v| !p.values.contains(v))
                .cloned()
                .collect();

            // The existing members, in the order the new declaration puts
            // them. If that is not the order Postgres already has, this is a
            // permutation — and Postgres cannot move an enum member.
            let kept: Vec<&String> = e.values.iter().filter(|v| p.values.contains(v)).collect();
            let reordered = kept.iter().map(|v| v.as_str()).collect::<Vec<_>>()
                != p.values.iter().map(|v| v.as_str()).collect::<Vec<_>>();

            if reordered {
                // Order carries no meaning in JWC — types.md §3.5 forbids
                // ordering comparisons on an enum — so a permutation is not
                // an error and produces no operation. It does mean the
                // declared order is not the order in the database, and the
                // snapshot has to say so or the next run diffs it again
                // forever.
                let mut values = p.values.clone();
                values.extend(added.iter().cloned());
                self.set_enum_values(&e.schema, &e.name, values);
                for v in &added {
                    self.push(
                        Op::AddEnumValue {
                            schema: e.schema.clone(),
                            name: e.name.clone(),
                            value: v.clone(),
                            before: None,
                        },
                        loc,
                    );
                }
                continue;
            }

            for v in &added {
                // Anchor on the first *existing* member after this one, so a
                // value written in the middle of the declaration lands
                // there. Added values are emitted in declared order, so the
                // ones before it are already in place.
                let idx = e.values.iter().position(|x| x == v).unwrap_or(0);
                let before = e.values[idx + 1..]
                    .iter()
                    .find(|x| p.values.contains(x))
                    .cloned();
                self.push(
                    Op::AddEnumValue {
                        schema: e.schema.clone(),
                        name: e.name.clone(),
                        value: v.clone(),
                        before,
                    },
                    loc,
                );
            }
        }
        // An enum type is never dropped for the same reason a schema is not:
        // a column outside this program may still use it.
    }

    /// migrations.md §5.3 — refusal plus a recipe loses no data; an
    /// automated rebuild across an unknown cross-schema column map does.
    fn err_note_enum_recipe(&mut self, p: &EnumSnapshot, removed: &[&String]) {
        let keep = p
            .values
            .iter()
            .filter(|v| !removed.iter().any(|r| r == v))
            .map(|v| format!("'{v}'"))
            .collect::<Vec<_>>()
            .join(",");
        let users: Vec<String> = self
            .prev
            .tables
            .iter()
            .flat_map(|t| {
                t.columns
                    .iter()
                    .filter(move |c| c.ty == format!("{}.{}", p.schema, p.name))
                    .map(move |c| format!("{}.{}.{}", t.schema, t.name, c.name))
            })
            .collect();
        let alters = users
            .iter()
            .map(|u| {
                let (rest, col) = u.rsplit_once('.').unwrap_or((u.as_str(), ""));
                format!(
                    "    3. ALTER TABLE {rest} ALTER COLUMN {col} TYPE {}.{}_v2\n         USING {col}::text::{}.{}_v2;",
                    p.schema, p.name, p.schema, p.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let note = format!(
            "Postgres cannot drop an enum value. The safe sequence is:\n\
             \x20   1. CREATE TYPE {}.{}_v2 AS ENUM ({keep});\n\
             \x20   2. SELECT count(*) FROM … WHERE <column> IN (the removed values);  -- must be 0\n\
             {alters}\n\
             \x20   4. DROP TYPE {}.{};\n\
             \x20   5. ALTER TYPE {}.{}_v2 RENAME TO {};\n\n\
             Columns using this type: {}",
            p.schema,
            p.name,
            p.schema,
            p.name,
            p.schema,
            p.name,
            p.name,
            if users.is_empty() {
                "(none in this program)".to_string()
            } else {
                users.join(", ")
            }
        );
        if let Some((_, d)) = self.diags.last_mut() {
            d.note = Some(note);
        }
    }

    fn keep_enum(&mut self, p: &EnumSnapshot) {
        self.set_enum_values(&p.schema, &p.name, p.values.clone());
    }

    fn set_enum_values(&mut self, schema: &str, name: &str, values: Vec<String>) {
        if let Some(e) = self
            .effective
            .enums
            .iter_mut()
            .find(|e| e.schema == schema && e.name == name)
        {
            e.values = values;
        }
    }
}

// ── tables ─────────────────────────────────────────────────────────────

/// The constraints a `CREATE TABLE` carries inline, in `ddl.rs`'s order.
/// A predicated unique is absent: it has no table-constraint form and
/// travels as a unique index (schema.md §4.3).
fn constraints_of(t: &TableSnapshot) -> Vec<Constraint> {
    let mut out: Vec<Constraint> = Vec::new();
    if let Some(pk) = &t.primary_key {
        out.push(Constraint::PrimaryKey(pk.clone()));
    }
    for u in &t.uniques {
        if u.predicate.is_none() {
            out.push(Constraint::Unique(u.clone()));
        }
    }
    for c in &t.checks {
        out.push(Constraint::Check(c.clone()));
    }
    for f in &t.foreign_keys {
        out.push(Constraint::ForeignKey(f.clone()));
    }
    out
}

/// Every index on the table, predicated uniques included — which is exactly
/// the set `ddl.rs` phase 5 emits.
fn indexes_of(t: &TableSnapshot) -> Vec<IndexSnapshot> {
    let mut out: Vec<IndexSnapshot> = Vec::new();
    for u in &t.uniques {
        let Some(pred) = &u.predicate else { continue };
        out.push(IndexSnapshot {
            name: u.name.clone(),
            columns: u
                .columns
                .iter()
                .map(|c| IndexColumnSnapshot {
                    name: c.clone(),
                    desc: false,
                    nulls: None,
                })
                .collect(),
            predicate: Some(pred.clone()),
            unique: true,
            method: None,
        });
    }
    out.extend(t.indexes.iter().cloned());
    out
}

impl Builder<'_> {
    fn tables(&mut self) {
        for t in &self.next.tables {
            let key = key2(&t.schema, &t.name);
            let loc = self.src.loc(&key);
            let Some(p) = self.prev.table(&t.schema, &t.name).cloned() else {
                self.push(Op::CreateTable(t.clone()), loc);
                // Foreign keys go out in phase 4 even for a brand-new table:
                // the table they point at may be created by this same
                // migration, and a cross-schema cycle has no valid table
                // order at all (schema.md §9).
                for f in &t.foreign_keys {
                    self.push(
                        Op::AddConstraint {
                            schema: t.schema.clone(),
                            table: t.name.clone(),
                            constraint: Constraint::ForeignKey(f.clone()),
                        },
                        loc,
                    );
                }
                for ix in indexes_of(t) {
                    self.push(
                        Op::CreateIndex {
                            schema: t.schema.clone(),
                            table: t.name.clone(),
                            index: ix,
                        },
                        loc,
                    );
                }
                self.comments(t, None);
                continue;
            };
            self.columns(t, &p);
            self.constraints(t, &p);
            self.indexes(t, &p);
            self.comments(t, Some(&p));
        }

        for p in self.prev.tables.clone() {
            if self.next.table(&p.schema, &p.name).is_some() {
                continue;
            }
            self.touch(&p.declared);
            // Dropping the table takes its constraints, indexes and trigger
            // with it, so they are not enumerated here — one statement, and
            // the `down` file rebuilds the whole object from this snapshot.
            self.push(Op::DropTable(p), None);
        }
    }

    fn columns(&mut self, t: &TableSnapshot, p: &TableSnapshot) {
        let key = key2(&t.schema, &t.name);
        for c in &t.columns {
            let ckey = format!("{key}.{}", c.name);
            let cloc = self.src.loc(&ckey);
            let Some(pc) = p.column(&c.name).cloned() else {
                // schema.md §10 / migrations.md §7.1. "May hold rows" means
                // "not created by this same migration", which is exactly the
                // branch we are in. `identity` supplies its own value, so it
                // is not caught by this.
                if !c.nullable && c.default.is_none() && !c.identity {
                    self.err(
                        cloc,
                        "E0440",
                        format!(
                            "adding NOT NULL column `{}.{}.{}` with no default",
                            t.schema, t.name, c.name
                        ),
                        "schema.md §10",
                    );
                    let recipe = format!(
                        "A NOT NULL column cannot be added to a table that may hold rows.\n\
                         Either:\n\
                         \x20 1. give it a default:          {} {} default …;\n\
                         \x20 2. or make it nullable now and tighten later (expand/contract):\n\
                         \x20      migration 1:  {} {}?              -- add, backfill\n\
                         \x20      migration 2:  {} {}               -- SET NOT NULL",
                        c.declared, c.ty, c.declared, c.ty, c.declared, c.ty
                    );
                    if let Some((_, d)) = self.diags.last_mut() {
                        d.note = Some(recipe);
                    }
                }
                self.touch(&t.declared);
                self.push(
                    Op::AddColumn {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.clone(),
                    },
                    cloc,
                );
                continue;
            };

            if pc.ty != c.ty {
                self.touch(&t.declared);
                self.push(
                    Op::AlterColumnType {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                        from: pc.ty.clone(),
                        to: c.ty.clone(),
                    },
                    cloc,
                );
            }
            if pc.nullable != c.nullable {
                self.touch(&t.declared);
                let op = if c.nullable {
                    Op::DropNotNull {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                    }
                } else {
                    Op::SetNotNull {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                    }
                };
                self.push(op, cloc);
            }
            if pc.default != c.default {
                let op = match &c.default {
                    Some(v) => Op::SetDefault {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                        value: v.clone(),
                    },
                    None => Op::DropDefault {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                    },
                };
                self.push(op, cloc);
            }
            if pc.identity != c.identity {
                let op = if c.identity {
                    Op::SetIdentity {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                    }
                } else {
                    Op::DropIdentity {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                    }
                };
                self.push(op, cloc);
            }
        }

        for pc in &p.columns {
            if t.column(&pc.name).is_some() {
                continue;
            }
            self.touch(&t.declared);
            self.push(
                Op::DropColumn {
                    schema: t.schema.clone(),
                    table: t.name.clone(),
                    column: pc.clone(),
                },
                None,
            );
        }
    }

    fn constraints(&mut self, t: &TableSnapshot, p: &TableSnapshot) {
        let now = constraints_of(t);
        let was = constraints_of(p);
        let loc = self.src.loc(&key2(&t.schema, &t.name));
        let mut used = vec![false; was.len()];
        let mut adds: Vec<Constraint> = Vec::new();

        for c in &now {
            match was
                .iter()
                .enumerate()
                .position(|(i, w)| !used[i] && w.body() == c.body())
            {
                Some(i) => {
                    used[i] = true;
                    if was[i].name() == c.name() {
                        continue;
                    }
                    if self.may_rename_generated {
                        self.push(
                            Op::RenameConstraint {
                                schema: t.schema.clone(),
                                table: t.name.clone(),
                                from: was[i].name().to_string(),
                                to: c.name().to_string(),
                            },
                            loc,
                        );
                    } else {
                        // schema.md §8.2: the snapshot's scheme version is
                        // older, so the name it recorded stands. Adopting a
                        // scheme must not rename live constraints.
                        self.keep_constraint_name(t, c.name(), was[i].name());
                    }
                }
                None => adds.push(c.clone()),
            }
        }

        for c in &adds {
            // An add whose name is still held by a constraint this migration
            // is otherwise dropping has to drop it *first* — see the module
            // note on replacements.
            if let Some(i) = was
                .iter()
                .enumerate()
                .position(|(i, w)| !used[i] && w.name() == c.name())
            {
                used[i] = true;
                self.push(
                    Op::DropConstraint {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        name: c.name().to_string(),
                        replaced: true,
                    },
                    loc,
                );
            }
            self.touch(&t.declared);
            self.push(
                Op::AddConstraint {
                    schema: t.schema.clone(),
                    table: t.name.clone(),
                    constraint: c.clone(),
                },
                loc,
            );
        }

        for (i, w) in was.iter().enumerate() {
            if used[i] {
                continue;
            }
            self.touch(&t.declared);
            self.push(
                Op::DropConstraint {
                    schema: t.schema.clone(),
                    table: t.name.clone(),
                    name: w.name().to_string(),
                    replaced: false,
                },
                None,
            );
        }
    }

    fn indexes(&mut self, t: &TableSnapshot, p: &TableSnapshot) {
        let now = indexes_of(t);
        let was = indexes_of(p);
        let loc = self.src.loc(&key2(&t.schema, &t.name));
        let body = |ix: &IndexSnapshot| {
            format!(
                "{:?}|{}|{}|{}",
                ix.columns,
                ix.predicate.as_deref().unwrap_or("-"),
                ix.unique,
                ix.method.as_deref().unwrap_or("btree")
            )
        };
        let mut used = vec![false; was.len()];
        let mut adds: Vec<IndexSnapshot> = Vec::new();

        for ix in &now {
            match was
                .iter()
                .enumerate()
                .position(|(i, w)| !used[i] && body(w) == body(ix))
            {
                Some(i) => {
                    used[i] = true;
                    if was[i].name == ix.name {
                        continue;
                    }
                    if self.may_rename_generated {
                        self.push(
                            Op::RenameIndex {
                                schema: t.schema.clone(),
                                from: was[i].name.clone(),
                                to: ix.name.clone(),
                            },
                            loc,
                        );
                    } else {
                        self.keep_index_name(t, &ix.name, &was[i].name);
                    }
                }
                None => adds.push(ix.clone()),
            }
        }

        for ix in &adds {
            if let Some(i) = was
                .iter()
                .enumerate()
                .position(|(i, w)| !used[i] && w.name == ix.name)
            {
                used[i] = true;
                self.push(
                    Op::DropIndex {
                        schema: t.schema.clone(),
                        name: ix.name.clone(),
                        replaced: true,
                    },
                    loc,
                );
            }
            self.push(
                Op::CreateIndex {
                    schema: t.schema.clone(),
                    table: t.name.clone(),
                    index: ix.clone(),
                },
                loc,
            );
        }

        for (i, w) in was.iter().enumerate() {
            if used[i] {
                continue;
            }
            self.push(
                Op::DropIndex {
                    schema: t.schema.clone(),
                    name: w.name.clone(),
                    replaced: false,
                },
                None,
            );
        }
    }

    fn comments(&mut self, t: &TableSnapshot, p: Option<&TableSnapshot>) {
        let key = key2(&t.schema, &t.name);
        let loc = self.src.loc(&key);
        let was_table = p.and_then(|p| p.comment.clone());
        if was_table != t.comment {
            self.push(
                Op::CommentOn {
                    target: CommentTarget::Table {
                        schema: t.schema.clone(),
                        name: t.name.clone(),
                    },
                    text: t.comment.clone(),
                },
                loc,
            );
        }
        for c in &t.columns {
            let was = p
                .and_then(|p| p.column(&c.name))
                .and_then(|x| x.comment.clone());
            // A column this migration adds carries no comment yet, so a
            // `None` previous value and a `Some` new one is a real change —
            // which the equality below already says.
            if was == c.comment {
                continue;
            }
            self.push(
                Op::CommentOn {
                    target: CommentTarget::Column {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        name: c.name.clone(),
                    },
                    text: c.comment.clone(),
                },
                self.src.loc(&format!("{key}.{}", c.name)),
            );
        }
    }

    /// Leave a generated constraint under the name the snapshot recorded.
    fn keep_constraint_name(&mut self, t: &TableSnapshot, new: &str, old: &str) {
        let Some(et) = self
            .effective
            .tables
            .iter_mut()
            .find(|x| x.schema == t.schema && x.name == t.name)
        else {
            return;
        };
        if let Some(pk) = et.primary_key.as_mut() {
            if pk.name == new {
                pk.name = old.to_string();
                return;
            }
        }
        for u in &mut et.uniques {
            if u.name == new {
                u.name = old.to_string();
                return;
            }
        }
        for c in &mut et.checks {
            if c.name == new {
                c.name = old.to_string();
                return;
            }
        }
        for f in &mut et.foreign_keys {
            if f.name == new {
                f.name = old.to_string();
                return;
            }
        }
    }

    fn keep_index_name(&mut self, t: &TableSnapshot, new: &str, old: &str) {
        let Some(et) = self
            .effective
            .tables
            .iter_mut()
            .find(|x| x.schema == t.schema && x.name == t.name)
        else {
            return;
        };
        for u in &mut et.uniques {
            if u.name == new && u.predicate.is_some() {
                u.name = old.to_string();
                return;
            }
        }
        for ix in &mut et.indexes {
            if ix.name == new {
                ix.name = old.to_string();
                return;
            }
        }
    }

    // ── touch functions and triggers (schema.md §6) ────────────────────

    fn functions_and_triggers(&mut self) {
        for f in &self.next.functions {
            let prev = self
                .prev
                .functions
                .iter()
                .find(|p| p.schema == f.schema && p.name == f.name);
            if prev != Some(f) {
                // `CREATE OR REPLACE FUNCTION` — no drop, so a trigger that
                // already points at it keeps working across the change.
                let loc = self.src.loc(&key2(&f.schema, &f.table));
                self.push(Op::CreateFunction(f.clone()), loc);
            }
        }
        for t in &self.next.triggers {
            let prev = self
                .prev
                .triggers
                .iter()
                .find(|p| p.schema == t.schema && p.name == t.name && p.table == t.table);
            if prev == Some(t) {
                continue;
            }
            let loc = self.src.loc(&key2(&t.schema, &t.table));
            if prev.is_some() {
                // `CREATE OR REPLACE TRIGGER` is PG14+; drop-then-create
                // works everywhere and is one statement longer.
                self.push(
                    Op::DropTrigger {
                        schema: t.schema.clone(),
                        table: t.table.clone(),
                        name: t.name.clone(),
                        replaced: true,
                    },
                    loc,
                );
            }
            self.push(Op::CreateTrigger(t.clone()), loc);
        }

        let dropped_tables: Vec<(String, String)> = self
            .prev
            .tables
            .iter()
            .filter(|p| self.next.table(&p.schema, &p.name).is_none())
            .map(|p| (p.schema.clone(), p.name.clone()))
            .collect();
        for t in self.prev.triggers.clone() {
            if self
                .next
                .triggers
                .iter()
                .any(|n| n.schema == t.schema && n.name == t.name && n.table == t.table)
            {
                continue;
            }
            // `DROP TABLE` takes its triggers with it; emitting the drop as
            // well would fail on a table that is already gone.
            if dropped_tables.contains(&(t.schema.clone(), t.table.clone())) {
                continue;
            }
            self.push(
                Op::DropTrigger {
                    schema: t.schema.clone(),
                    table: t.table.clone(),
                    name: t.name.clone(),
                    replaced: false,
                },
                None,
            );
        }
        for f in self.prev.functions.clone() {
            if self
                .next
                .functions
                .iter()
                .any(|n| n.schema == f.schema && n.name == f.name)
            {
                continue;
            }
            self.push(
                Op::DropFunction {
                    schema: f.schema.clone(),
                    name: f.name.clone(),
                },
                None,
            );
        }
    }

    // ── §4 phases 0 and 8: views are dropped and rebuilt around the change

    fn views(&mut self) {
        let prev_views = self.prev.views.clone();
        let prev_depth = depths(&prev_views);
        let next_depth = depths(&self.next.views);

        // Which views have to come down. A view that reads a relation this
        // migration alters blocks the `ALTER` underneath it, so it is
        // dropped and rebuilt around the change (§4, phases 0 and 8).
        //
        // This is a fixed point rather than one pass, and it has to be: a
        // dropped view is itself a change, so anything reading *it* must go
        // too — and `DROP VIEW` without `CASCADE` refuses while a dependent
        // still exists. Declaration order is not dependency order, so a
        // single sweep would leave the outer view standing.
        let mut drop_set: Vec<String> = Vec::new();
        loop {
            let mut grew = false;
            for p in &prev_views {
                let key = key2(&p.schema, &p.name);
                if drop_set.contains(&key) {
                    continue;
                }
                let now = self.next.view(&p.schema, &p.name);
                let gone = now.is_none();
                let changed = now.is_some_and(|n| n.body != p.body);
                let reads_touched = p.reads.iter().any(|r| self.touched.iter().any(|t| t == r));
                if !gone && !changed && !reads_touched {
                    continue;
                }
                drop_set.push(key);
                self.touch(&p.declared);
                grew = true;
            }
            if !grew {
                break;
            }
        }

        for p in &prev_views {
            if !drop_set.contains(&key2(&p.schema, &p.name)) {
                continue;
            }
            // Deepest first: a view that reads another has to go before it.
            let depth = prev_depth.get(&p.declared).copied().unwrap_or(0);
            self.push_ordered(
                Op::DropView {
                    schema: p.schema.clone(),
                    name: p.name.clone(),
                },
                None,
                DEPTH_BASE - depth,
            );
        }

        for v in &self.next.views {
            let key = key2(&v.schema, &v.name);
            let loc = self.src.loc(&key);
            let previous = self.prev.view(&v.schema, &v.name);
            if previous.is_some() && !drop_set.contains(&key) {
                // Still standing. Only its documentation can have changed.
                if previous.and_then(|p| p.comment.clone()) != v.comment {
                    self.push_ordered(
                        Op::CommentOn {
                            target: CommentTarget::View {
                                schema: v.schema.clone(),
                                name: v.name.clone(),
                            },
                            text: v.comment.clone(),
                        },
                        loc,
                        COMMENT_ORDER,
                    );
                }
                continue;
            }
            let depth = next_depth.get(&v.declared).copied().unwrap_or(0);
            self.push_ordered(Op::CreateView(v.clone()), loc, depth);
            if let Some(text) = &v.comment {
                // The comment goes out with the view: `DROP VIEW` took the
                // old one, and phase 7 would name an object phase 8 has not
                // created yet.
                self.push_ordered(
                    Op::CommentOn {
                        target: CommentTarget::View {
                            schema: v.schema.clone(),
                            name: v.name.clone(),
                        },
                        text: Some(text.clone()),
                    },
                    loc,
                    COMMENT_ORDER,
                );
            }
        }
    }
}

/// Views sort by how deep they sit in the dependency graph. `DROP VIEW`
/// counts down from here so the deepest goes first.
const DEPTH_BASE: u16 = 4096;
/// After every `CREATE VIEW` in phase 8.
const COMMENT_ORDER: u16 = 8192;

/// How many views each view sits on top of. A view that reads no other view
/// is 0; one that reads a depth-1 view is 2.
///
/// A cycle is not emittable at all — `ddl.rs` says the same — so the
/// iteration is bounded by the view count and whatever is left keeps depth
/// 0, which puts Postgres's error on the offending statement rather than
/// hanging here.
fn depths(views: &[ViewSnapshot]) -> BTreeMap<String, u16> {
    let mut depth: BTreeMap<String, u16> = views.iter().map(|v| (v.declared.clone(), 0)).collect();
    for _ in 0..views.len() {
        let mut moved = false;
        for v in views {
            let want = v
                .reads
                .iter()
                .filter_map(|r| depth.get(r).copied())
                .max()
                .map(|d| d + 1)
                .unwrap_or(0);
            let entry = depth.entry(v.declared.clone()).or_insert(0);
            if *entry < want {
                *entry = want;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    depth
}

//! The snapshot — the schema as a database holds it.
//!
//! `jwc migrate new` is offline (migrations.md §1): it never connects to a
//! database, so it needs a written record of the previous state. That record
//! is a `.snapshot.json` checked in beside the migration that produced it.
//!
//! Reading the previous state from JSON rather than by re-parsing the last
//! `.up.sql` removes a whole class of round-trip bug — the parser only has
//! to understand JSON it wrote (migrations.md §2).
//!
//! ## What goes in
//!
//! Everything that is *in the database*, and nothing that is not. The seven
//! object classes of migrations.md §2:
//!
//! 1. schemas
//! 2. enum types (the `of` form — the inline form is a `varchar` plus a
//!    check, and travels as those)
//! 3. tables: columns, primary key, uniques, checks, foreign keys
//! 4. indexes, predicates already canonical (schema.md §4.3)
//! 5. touch functions and triggers (schema.md §6)
//! 6. comments — carried on the object they are attached to, because
//!    `COMMENT ON` names that object and nothing else
//! 7. views, with their dependency edges
//!
//! What is deliberately **absent** is as load-bearing as what is present,
//! because absence is the statement "editing this produces no migration":
//!
//! * `private` / `server` — access rules in the language, no DDL;
//! * a constraint's `: "message"` — schema.md §8 decouples a constraint's
//!   identity from its message on purpose, so that editing the sentence a
//!   user sees does not rewrite a live constraint;
//! * `was` — a marker describing a *transition*, not a state. The old name
//!   is in the previous snapshot as an ordinary name, which is exactly what
//!   makes `E1103` detectable (migrations.md §6.3).
//!
//! ## Two versions, not one
//!
//! `format` is this file layout. `scheme` is the constraint-naming scheme
//! the names were generated with (schema.md §8.2). They move independently:
//! a new naming scheme must not rename constraints on a live database, so
//! the diff compares against the scheme the snapshot was written with.

use crate::model::{SchemaModel, TableObj};
use crate::naming;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped when the JSON layout changes such that an older reader would
/// misread a newer file. Adding an optional field is not a bump.
pub const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub format: u32,
    /// Constraint-naming scheme version (schema.md §8.2).
    pub scheme: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schemas: Vec<SchemaSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<TableSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<ViewSnapshot>,
}

impl Default for Snapshot {
    /// The empty database. `migrate new` on a project with no `migrations/`
    /// diffs against this, so the first migration is a full `CREATE`.
    fn default() -> Self {
        Snapshot {
            format: FORMAT,
            scheme: naming::SCHEME_VERSION.to_string(),
            schemas: Vec::new(),
            enums: Vec::new(),
            tables: Vec::new(),
            functions: Vec::new(),
            triggers: Vec::new(),
            views: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub name: String,
    pub declared: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EnumSnapshot {
    pub schema: String,
    pub name: String,
    pub declared: String,
    /// Member order as declared. Postgres stores enum members ordered, and
    /// `ADD VALUE` appends — so the order is state, even though JWC gives it
    /// no meaning (types.md §3.5 forbids ordering comparisons).
    pub values: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TableSnapshot {
    pub schema: String,
    pub name: String,
    pub declared: String,
    pub columns: Vec<ColumnSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<PrimaryKeySnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uniques: Vec<UniqueSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKeySnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColumnSnapshot {
    pub name: String,
    pub declared: String,
    /// Rendered SQL type: `bigint`, `varchar(80)`, `billing.invoice_status`,
    /// `text[]`. Rendered rather than structured because the type is what
    /// `ALTER COLUMN … TYPE` takes, and a structured form would have to be
    /// re-rendered identically to avoid a spurious diff.
    pub ty: String,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub identity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrimaryKeySnapshot {
    pub name: String,
    pub columns: Vec<String>,
}

/// A `unique` as the source declares it. One carrying a `predicate` is a
/// partial unique, which Postgres has no table constraint for — the diff
/// lowers it to a unique index (schema.md §4.3), the same way `ddl.rs` does.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UniqueSnapshot {
    pub name: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CheckSnapshot {
    pub name: String,
    pub expr: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ForeignKeySnapshot {
    pub name: String,
    pub columns: Vec<String>,
    pub target_schema: String,
    pub target_table: String,
    pub target_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IndexSnapshot {
    pub name: String,
    pub columns: Vec<IndexColumnSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IndexColumnSnapshot {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub desc: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nulls: Option<String>,
}

/// A touch function (schema.md §6). The *body* is not stored: it is a
/// template over `(schema, table, columns)`, so storing the rendered text
/// would make every edit to that template look like a schema change on
/// every existing project. The inputs are stored instead, and emission
/// re-renders — which means a template change reaches a database only when
/// something about the table actually changes. That is the trade, and it is
/// the right way round: a cosmetic compiler edit must not rewrite live DDL.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FunctionSnapshot {
    pub schema: String,
    pub name: String,
    /// Physical table name the function's `NEW` refers to.
    pub table: String,
    /// Columns assigned `now()`, in declaration order.
    pub sets_now: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TriggerSnapshot {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub function: String,
    pub timing: String,
    pub event: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ViewSnapshot {
    pub schema: String,
    pub name: String,
    pub declared: String,
    /// The `SELECT` behind `CREATE VIEW`, exactly as emitted.
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Declared names of every relation the body reads. Phase 0 drops a
    /// view before anything it depends on changes and phase 8 rebuilds it
    /// after, so these edges decide both orders (migrations.md §4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ── building one from the model ────────────────────────────────────────

/// The current state, from the resolved schema.
///
/// Every list `model::build` produces is already sorted (by physical name,
/// schema-qualified), and this preserves that order — which is what makes
/// two runs on the same source byte-identical (migrations.md §10.1).
pub fn of(model: &SchemaModel) -> Snapshot {
    let mut snap = Snapshot {
        format: FORMAT,
        scheme: model.scheme.to_string(),
        ..Default::default()
    };

    for s in &model.schemas {
        snap.schemas.push(SchemaSnapshot {
            name: s.physical.clone(),
            declared: s.declared.clone(),
        });
    }

    for e in &model.enums {
        // Only the `of` form is a Postgres type. An inline enum is a
        // varchar plus a check constraint, and travels as those.
        let Some(schema) = &e.schema else { continue };
        snap.enums.push(EnumSnapshot {
            schema: schema.clone(),
            name: e.physical.clone(),
            declared: e.declared.clone(),
            values: e.members.clone(),
        });
    }

    for t in &model.tables {
        snap.tables.push(table(t));
        if t.touch_columns.is_empty() {
            continue;
        }
        let fname = naming::touch_function(&t.physical);
        snap.functions.push(FunctionSnapshot {
            schema: t.schema_physical.clone(),
            name: fname.clone(),
            table: t.physical.clone(),
            sets_now: t.touch_columns.clone(),
        });
        snap.triggers.push(TriggerSnapshot {
            // `ddl.rs` names the trigger after the function; the two live in
            // different namespaces in Postgres, so the collision is only
            // apparent.
            name: fname.clone(),
            schema: t.schema_physical.clone(),
            table: t.physical.clone(),
            function: fname,
            timing: "BEFORE".into(),
            event: "UPDATE".into(),
        });
    }

    for v in &model.views {
        // A view whose body could not be emitted is not in the database, so
        // it is not in the snapshot either. `gen-sql` skips it the same way.
        let Some(body) = &v.body else { continue };
        snap.views.push(ViewSnapshot {
            schema: v.schema_physical.clone(),
            name: v.physical.clone(),
            declared: v.declared.clone(),
            body: body.clone(),
            comment: comment(&v.docs),
            reads: v.reads.clone(),
        });
    }

    snap
}

fn table(t: &TableObj) -> TableSnapshot {
    TableSnapshot {
        schema: t.schema_physical.clone(),
        name: t.physical.clone(),
        declared: t.declared.clone(),
        columns: t
            .columns
            .iter()
            .map(|c| ColumnSnapshot {
                name: c.physical.clone(),
                declared: c.declared.clone(),
                ty: c.ty.render(),
                nullable: c.nullable,
                identity: c.identity,
                default: c.default.clone(),
                comment: comment(&c.docs),
            })
            .collect(),
        primary_key: t.primary_key.as_ref().map(|pk| PrimaryKeySnapshot {
            name: pk.name.clone(),
            columns: pk.columns.clone(),
        }),
        uniques: t
            .uniques
            .iter()
            .map(|u| UniqueSnapshot {
                name: u.name.clone(),
                columns: u.columns.clone(),
                predicate: u.predicate.clone(),
            })
            .collect(),
        checks: t
            .checks
            .iter()
            .map(|c| CheckSnapshot {
                name: c.name.clone(),
                expr: c.expr.clone(),
            })
            .collect(),
        foreign_keys: t
            .foreign_keys
            .iter()
            .map(|f| ForeignKeySnapshot {
                name: f.name.clone(),
                columns: f.columns.clone(),
                target_schema: f.target_schema.clone(),
                target_table: f.target_table.clone(),
                target_columns: f.target_columns.clone(),
                on_delete: f.on_delete.map(|a| a.as_sql().to_string()),
                on_update: f.on_update.map(|a| a.as_sql().to_string()),
            })
            .collect(),
        indexes: t
            .indexes
            .iter()
            .map(|ix| IndexSnapshot {
                name: ix.name.clone(),
                columns: ix
                    .columns
                    .iter()
                    .map(|c| IndexColumnSnapshot {
                        name: c.physical.clone(),
                        desc: c.desc,
                        nulls: c.nulls.map(|n| match n {
                            crate::ast::NullsOrder::First => "FIRST".to_string(),
                            crate::ast::NullsOrder::Last => "LAST".to_string(),
                        }),
                    })
                    .collect(),
                predicate: ix.predicate.clone(),
                unique: ix.unique,
                method: ix.method.clone(),
            })
            .collect(),
        comment: comment(&t.docs),
    }
}

fn comment(docs: &[String]) -> Option<String> {
    if docs.is_empty() {
        None
    } else {
        Some(docs.join("\n"))
    }
}

// ── lookups the diff needs ─────────────────────────────────────────────

impl Snapshot {
    pub fn table(&self, schema: &str, name: &str) -> Option<&TableSnapshot> {
        self.tables
            .iter()
            .find(|t| t.schema == schema && t.name == name)
    }

    pub fn enum_type(&self, schema: &str, name: &str) -> Option<&EnumSnapshot> {
        self.enums
            .iter()
            .find(|e| e.schema == schema && e.name == name)
    }

    pub fn view(&self, schema: &str, name: &str) -> Option<&ViewSnapshot> {
        self.views
            .iter()
            .find(|v| v.schema == schema && v.name == name)
    }

    /// Pretty JSON with a trailing newline — a checked-in file that a
    /// reviewer reads and `git diff` shows a line at a time.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("snapshot is plain data");
        s.push('\n');
        s
    }

    pub fn from_json(text: &str) -> Result<Snapshot, String> {
        let snap: Snapshot =
            serde_json::from_str(text).map_err(|e| format!("malformed snapshot: {e}"))?;
        if snap.format > FORMAT {
            return Err(format!(
                "snapshot format {} is newer than this compiler understands ({FORMAT})",
                snap.format
            ));
        }
        Ok(snap)
    }
}

impl TableSnapshot {
    pub fn qualified(&self) -> String {
        format!(
            "{}.{}",
            naming::quote_ident(&self.schema),
            naming::quote_ident(&self.name)
        )
    }

    pub fn column(&self, name: &str) -> Option<&ColumnSnapshot> {
        self.columns.iter().find(|c| c.name == name)
    }
}

impl ViewSnapshot {
    pub fn qualified(&self) -> String {
        format!(
            "{}.{}",
            naming::quote_ident(&self.schema),
            naming::quote_ident(&self.name)
        )
    }
}

impl EnumSnapshot {
    pub fn qualified(&self) -> String {
        format!(
            "{}.{}",
            naming::quote_ident(&self.schema),
            naming::quote_ident(&self.name)
        )
    }
}

// ── the migrations directory ───────────────────────────────────────────

/// One migration on disk, identified by its `NNNN_name` stem.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MigrationFile {
    pub ordinal: u32,
    pub stem: String,
    pub dir: PathBuf,
}

impl MigrationFile {
    pub fn up(&self) -> PathBuf {
        self.dir.join(format!("{}.up.sql", self.stem))
    }
    pub fn down(&self) -> PathBuf {
        self.dir.join(format!("{}.down.sql", self.stem))
    }
    pub fn snapshot_path(&self) -> PathBuf {
        self.dir.join(format!("{}.snapshot.json", self.stem))
    }
    /// Hand-written backfill, run in phase 3 of this migration
    /// (migrations.md §7.2). Never generated.
    pub fn data(&self) -> PathBuf {
        self.dir.join(format!("{}.data.sql", self.stem))
    }
}

/// Every migration under `dir`, in ordinal order.
///
/// A file that does not match `NNNN_name.up.sql` is ignored rather than an
/// error: the directory is also where `.data.sql` sidecars and a reviewer's
/// scratch notes live.
pub fn list(dir: &Path) -> Vec<MigrationFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<MigrationFile> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(file) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(stem) = file.strip_suffix(".up.sql") else {
            continue;
        };
        let Some((num, _)) = stem.split_once('_') else {
            continue;
        };
        let Ok(ordinal) = num.parse::<u32>() else {
            continue;
        };
        out.push(MigrationFile {
            ordinal,
            stem: stem.to_string(),
            dir: dir.to_path_buf(),
        });
    }
    out.sort();
    out
}

/// The snapshot the last migration left behind — the previous state.
///
/// Searches backwards: a migration split into a `no-transaction` file plus
/// an ordinary one (migrations.md §5.2) writes the snapshot once, on the
/// last of the pair, so the newest snapshot is not always on the newest
/// migration.
pub fn previous(dir: &Path) -> Result<Snapshot, String> {
    for m in list(dir).iter().rev() {
        let path = m.snapshot_path();
        if !path.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        return Snapshot::from_json(&text).map_err(|e| format!("{}: {e}", path.display()));
    }
    Ok(Snapshot::default())
}

/// The ordinal `migrate new` should use next.
pub fn next_ordinal(dir: &Path) -> u32 {
    list(dir).last().map(|m| m.ordinal + 1).unwrap_or(1)
}

/// `0007_add_region` — four digits, so an alphabetical listing is also a
/// chronological one until the ten-thousandth migration.
pub fn stem(ordinal: u32, name: &str) -> String {
    format!("{ordinal:04}_{}", naming::physical(name).replace(['-', ' '], "_"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    /// A model from source text. The snapshot is a function of the resolved
    /// schema, so the tests go through the real front-end rather than
    /// hand-building model objects that could drift from what `model::build`
    /// actually produces.
    fn snap(src: &str) -> Snapshot {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.jwc"), src).expect("write");
        let ws = Workspace::load(dir.path()).expect("load");
        assert!(
            !ws.has_parse_errors(),
            "fixture did not parse:\n{}",
            ws.parse_errors().join("")
        );
        let built = crate::model::build(&ws);
        let errors: Vec<String> = built
            .diags
            .iter()
            .filter(|(_, d)| d.severity == crate::diag::Severity::Error)
            .map(|(loc, d)| ws.render(*loc, d))
            .collect();
        assert!(errors.is_empty(), "fixture has errors:\n{}", errors.join(""));
        of(&built.model)
    }

    const BASE: &str = r#"
namespace t;
database App : Postgres;
schema org of App;

enum Plan of App.org { free, pro }

--- Tenants.
table Orgs of App.org {
    --- Surrogate key.
    id   bigint primary key identity;
    slug varchar(40) unique;
    plan Plan;
    name varchar(80)?;
    retired_at timestamptz?;
    updated_at timestamptz on update now();

    unique (name) where retired_at == null : "faol nom bitta";
    check (id > 0) : "musbat";
    index on (slug, id desc);
}
"#;

    #[test]
    fn round_trips_through_json() {
        let a = snap(BASE);
        let text = a.to_json();
        let b = Snapshot::from_json(&text).expect("re-read");
        assert_eq!(a, b, "snapshot changed across a JSON round trip");
        assert!(text.ends_with('\n'), "snapshot file needs a trailing newline");
    }

    #[test]
    fn two_runs_are_byte_identical() {
        // migrations.md §10.1. Every list is sorted upstream; this is the
        // assertion that nothing in here reintroduces hash-map order.
        assert_eq!(snap(BASE).to_json(), snap(BASE).to_json());
    }

    #[test]
    fn covers_the_seven_object_classes() {
        let s = snap(BASE);
        assert_eq!(s.scheme, naming::SCHEME_VERSION);
        assert_eq!(s.schemas.len(), 1, "schemas");
        assert_eq!(s.enums.len(), 1, "enum types");
        assert_eq!(s.tables.len(), 1, "tables");
        assert_eq!(s.functions.len(), 1, "touch functions");
        assert_eq!(s.triggers.len(), 1, "triggers");

        let t = s.table("org", "orgs").expect("orgs");
        assert!(t.primary_key.is_some(), "primary key");
        assert_eq!(t.uniques.len(), 2, "uniques (one of them partial)");
        assert!(!t.checks.is_empty(), "checks");
        assert_eq!(t.indexes.len(), 1, "indexes");
        assert_eq!(t.comment.as_deref(), Some("Tenants."), "table comment");
        assert_eq!(
            t.column("id").and_then(|c| c.comment.as_deref()),
            Some("Surrogate key."),
            "column comment"
        );
        assert_eq!(s.enums[0].values, vec!["free", "pro"], "member order");
    }

    #[test]
    fn index_and_predicate_detail_survives() {
        let t = snap(BASE);
        let t = t.table("org", "orgs").expect("orgs");
        let ix = &t.indexes[0];
        assert_eq!(ix.columns.len(), 2);
        assert!(!ix.columns[0].desc);
        assert!(ix.columns[1].desc, "`id desc` is part of the index");
        // schema.md §4.3: the predicate is stored canonical, so two
        // spellings of the same `where` produce no migration (§8).
        let partial = t
            .uniques
            .iter()
            .find(|u| u.predicate.is_some())
            .expect("partial unique");
        assert!(
            partial.predicate.as_deref().is_some_and(|p| p.contains("IS NULL")),
            "predicate should be canonical SQL, got {:?}",
            partial.predicate
        );
    }

    #[test]
    fn a_message_is_not_schema_state() {
        // schema.md §8 decouples a constraint's identity from its message so
        // that rewording the sentence a user sees does not rewrite a live
        // constraint. If the message reached the snapshot, it would.
        let a = snap(BASE);
        let b = snap(&BASE.replace("faol nom bitta", "totally different text"));
        assert_eq!(a, b, "editing a constraint message produced a diff");
    }

    #[test]
    fn access_rules_are_not_schema_state() {
        // `private` is a rule about who may read the column, enforced in the
        // query compiler. It emits no DDL, so it must produce no migration.
        let a = snap(BASE);
        let b = snap(&BASE.replace("name varchar(80)?;", "name varchar(80)? private;"));
        assert_eq!(a, b, "adding `private` produced a diff");
    }

    #[test]
    fn an_inline_enum_is_not_a_type() {
        // Without `of`, an enum is a varchar plus a check — there is no
        // `CREATE TYPE`, so there is nothing for the enum diff to act on.
        let s = snap(
            r#"
namespace t;
database App : Postgres;
schema s of App;
enum Colour { red, green }
table T of App.s { id bigint primary key identity; c Colour; }
"#,
        );
        assert!(s.enums.is_empty(), "inline enum reached the type list");
        let t = s.table("s", "t").expect("t");
        assert!(
            t.column("c").is_some_and(|c| c.ty.starts_with("varchar")),
            "inline enum column should be a varchar"
        );
    }

    #[test]
    fn the_default_is_the_empty_database() {
        let s = Snapshot::default();
        assert_eq!(s.format, FORMAT);
        assert!(s.tables.is_empty() && s.views.is_empty());
        // A project with no `migrations/` diffs against exactly this.
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(previous(dir.path()), Ok(Snapshot::default()));
        assert_eq!(next_ordinal(dir.path()), 1);
    }

    #[test]
    fn a_newer_format_is_refused_not_misread() {
        let text = r#"{"format": 999, "scheme": "v1"}"#;
        let err = Snapshot::from_json(text).expect_err("should refuse");
        assert!(err.contains("999"), "{err}");
    }

    #[test]
    fn the_directory_is_read_in_ordinal_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        for stem in ["0002_b", "0010_c", "0001_a"] {
            std::fs::write(p.join(format!("{stem}.up.sql")), "").expect("write");
        }
        // Not a migration: no `.up.sql`, and a reviewer's notes should not
        // become one.
        std::fs::write(p.join("NOTES.md"), "").expect("write");
        std::fs::write(p.join("0003_d.data.sql"), "").expect("write");
        let names: Vec<String> = list(p).into_iter().map(|m| m.stem).collect();
        assert_eq!(names, vec!["0001_a", "0002_b", "0010_c"]);
        assert_eq!(next_ordinal(p), 11);
    }

    #[test]
    fn previous_searches_backwards_for_a_snapshot() {
        // A migration split into a `no-transaction` file plus an ordinary
        // one writes the snapshot once, on the last of the pair
        // (migrations.md §5.2) — so the newest file need not carry it.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        std::fs::write(p.join("0001_a.up.sql"), "").expect("write");
        let mut older = Snapshot::default();
        older.schemas.push(SchemaSnapshot {
            name: "org".into(),
            declared: "Org".into(),
        });
        std::fs::write(p.join("0001_a.snapshot.json"), older.to_json()).expect("write");
        std::fs::write(p.join("0002_b.up.sql"), "").expect("write");

        assert_eq!(previous(p), Ok(older));
    }

    #[test]
    fn stems_are_four_digits_and_snake_case() {
        assert_eq!(stem(7, "add_region"), "0007_add_region");
        assert_eq!(stem(7, "AddRegion"), "0007_add_region");
        assert_eq!(stem(12, "add region"), "0012_add_region");
        assert_eq!(stem(1234, "x"), "1234_x");
    }
}

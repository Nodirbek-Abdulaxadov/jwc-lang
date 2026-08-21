//! The resolved schema model.
//!
//! Turns the declarations of a whole project into the six object classes
//! migrations.md §2 snapshots: schemas, enum types, tables (columns + their
//! constraints), indexes, touch triggers, comments. Everything downstream —
//! DDL emission, the migration diff, and eventually the query compiler —
//! reads this rather than the AST, so physical naming and type mapping
//! happen exactly once.
//!
//! Diagnostics raised here are the schema-level rules of schema.md §11.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::naming;
use crate::token::Span;
use crate::workspace::{Loc, Workspace};
use std::collections::BTreeMap;

// ---------------------------------------------------------------- model

#[derive(Clone, Debug)]
pub struct SchemaModel {
    pub database: Option<String>,
    pub schemas: Vec<SchemaObj>,
    pub enums: Vec<EnumObj>,
    pub tables: Vec<TableObj>,
    /// Views as relations, with their columns worked out (views.rs).
    pub views: Vec<crate::views::ViewObj>,
    /// Naming scheme this model was built with (schema.md §8.2).
    pub scheme: &'static str,
}

#[derive(Clone, Debug)]
pub struct SchemaObj {
    pub declared: String,
    pub physical: String,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct EnumObj {
    pub declared: String,
    /// `None` for the varchar + CHECK form (schema.md §5.1).
    pub schema: Option<String>,
    pub physical: String,
    pub members: Vec<String>,
    pub docs: Vec<String>,
    pub loc: Loc,
}

impl EnumObj {
    pub fn is_typed(&self) -> bool {
        self.schema.is_some()
    }

    /// Width of the widest member, for the varchar form.
    pub fn varchar_width(&self) -> u32 {
        self.members
            .iter()
            .map(|m| m.chars().count())
            .max()
            .unwrap_or(1) as u32
    }
}

#[derive(Clone, Debug)]
pub struct TableObj {
    pub declared: String,
    pub schema: String,
    pub schema_physical: String,
    pub physical: String,
    pub was: Option<String>,
    pub docs: Vec<String>,
    pub columns: Vec<ColumnObj>,
    pub primary_key: Option<PrimaryKeyObj>,
    pub uniques: Vec<UniqueObj>,
    pub checks: Vec<CheckObj>,
    pub foreign_keys: Vec<ForeignKeyObj>,
    pub indexes: Vec<IndexObj>,
    /// Columns carrying `on update now()` — one trigger per table
    /// (schema.md §6).
    pub touch_columns: Vec<String>,
    pub loc: Loc,
}

impl TableObj {
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

#[derive(Clone, Debug)]
pub struct ColumnObj {
    pub declared: String,
    pub physical: String,
    pub was: Option<String>,
    pub ty: SqlType,
    pub nullable: bool,
    pub identity: bool,
    pub default: Option<String>,
    pub private: bool,
    pub server: bool,
    pub on_update_now: bool,
    pub docs: Vec<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlType {
    /// A base Postgres type, already rendered (`bigint`, `varchar(255)`,
    /// `numeric(14, 2)`).
    Scalar(String),
    /// A declared `enum ... of ...` — rendered as its qualified type name.
    Enum {
        qualified: String,
        declared: String,
    },
    /// An `enum` with no `of` clause: varchar plus a CHECK.
    EnumInline {
        width: u32,
        declared: String,
    },
    Array(Box<SqlType>),
}

impl SqlType {
    pub fn render(&self) -> String {
        match self {
            SqlType::Scalar(s) => s.clone(),
            SqlType::Enum { qualified, .. } => qualified.clone(),
            SqlType::EnumInline { width, .. } => format!("varchar({width})"),
            SqlType::Array(inner) => format!("{}[]", inner.render()),
        }
    }

    pub fn inline_enum(&self) -> Option<&str> {
        match self {
            SqlType::EnumInline { declared, .. } => Some(declared),
            SqlType::Array(inner) => inner.inline_enum(),
            _ => None,
        }
    }

    fn is_integer(&self) -> bool {
        matches!(self, SqlType::Scalar(s) if matches!(s.as_str(), "smallint" | "integer" | "bigint"))
    }

    fn is_temporal(&self) -> bool {
        matches!(self, SqlType::Scalar(s) if matches!(s.as_str(), "timestamptz" | "date" | "time"))
    }
}

#[derive(Clone, Debug)]
pub struct PrimaryKeyObj {
    pub name: String,
    pub columns: Vec<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct UniqueObj {
    pub name: String,
    pub columns: Vec<String>,
    /// Canonical SQL predicate; `Some` makes this a partial unique **index**
    /// rather than a constraint (schema.md §4.3).
    pub predicate: Option<String>,
    pub message: Option<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct CheckObj {
    pub name: String,
    pub expr: String,
    pub message: Option<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct ForeignKeyObj {
    pub name: String,
    pub columns: Vec<String>,
    pub target_schema: String,
    /// The target's **physical** name. Resolved after every table is known
    /// (`resolve_foreign_key_targets`), because a table may rename itself
    /// with `as "…"` and the reference names it as declared.
    pub target_table: String,
    /// The target as the source wrote it — `Users`, not `user`.
    pub target_declared: String,
    pub target_columns: Vec<String>,
    pub on_delete: Option<RefAction>,
    pub on_update: Option<RefAction>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct IndexObj {
    pub name: String,
    pub columns: Vec<IndexColumnObj>,
    pub predicate: Option<String>,
    pub unique: bool,
    pub method: Option<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct IndexColumnObj {
    pub physical: String,
    pub desc: bool,
    pub nulls: Option<NullsOrder>,
}

// ---------------------------------------------------------------- builder

pub struct Built {
    pub model: SchemaModel,
    pub diags: Vec<(Loc, Diagnostic)>,
}

pub fn build(ws: &Workspace) -> Built {
    let mut b = Builder {
        ws,
        diags: Vec::new(),
        enums: BTreeMap::new(),
        schemas: BTreeMap::new(),
        model: SchemaModel {
            database: None,
            schemas: Vec::new(),
            enums: Vec::new(),
            tables: Vec::new(),
            views: Vec::new(),
            scheme: naming::SCHEME_VERSION,
        },
    };
    b.run();
    // Views resolve against finished tables, so they are a second pass —
    // but they are part of the model, not something a caller can forget.
    let mut model = b.model;
    crate::views::attach(&mut model, ws);
    Built {
        model,
        diags: b.diags,
    }
}

struct Builder<'a> {
    ws: &'a Workspace,
    diags: Vec<(Loc, Diagnostic)>,
    /// declared enum name -> resolved object
    enums: BTreeMap<String, EnumObj>,
    /// declared schema name -> physical
    schemas: BTreeMap<String, String>,
    model: SchemaModel,
}

impl<'a> Builder<'a> {
    fn err(&mut self, loc: Loc, code: &'static str, msg: impl Into<String>, clause: &'static str) {
        self.diags
            .push((loc, Diagnostic::error(code, loc.span, msg).clause(clause)));
    }

    fn err_note(
        &mut self,
        loc: Loc,
        code: &'static str,
        msg: impl Into<String>,
        note: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags.push((
            loc,
            Diagnostic::error(code, loc.span, msg)
                .note(note)
                .clause(clause),
        ));
    }

    fn warn(&mut self, loc: Loc, code: &'static str, msg: impl Into<String>, clause: &'static str) {
        self.diags
            .push((loc, Diagnostic::warning(code, loc.span, msg).clause(clause)));
    }

    fn run(&mut self) {
        // Pass 1 — databases, schemas and enums, because tables reference
        // both. Sorted by (file, span) so diagnostics are stable.
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let loc = Loc {
                    file: fi,
                    span: d.span(),
                };
                match d {
                    Decl::Database(db) => {
                        self.check_init_keys(db, loc);
                        if self.model.database.is_some() {
                            self.err_note(
                                loc,
                                "E1203",
                                "more than one `database` declaration",
                                "multi-database is a non-goal; one connection per program",
                                "config.md §2.5",
                            );
                        } else {
                            self.model.database = Some(db.name.name.clone());
                        }
                    }
                    Decl::Schema(s) => self.add_schema(s, loc),
                    Decl::Enum(e) => self.add_enum(e, loc),
                    _ => {}
                }
            }
        }

        // Pass 2 — tables.
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                if let Decl::Table(t) = d {
                    let loc = Loc {
                        file: fi,
                        span: t.span,
                    };
                    self.add_table(t, loc);
                }
            }
        }

        self.model.enums = self.enums.values().cloned().collect();
        self.model.enums.sort_by(|a, b| {
            (a.schema.clone().unwrap_or_default(), a.physical.clone())
                .cmp(&(b.schema.clone().unwrap_or_default(), b.physical.clone()))
        });
        self.model
            .schemas
            .sort_by(|a, b| a.physical.cmp(&b.physical));
        self.model.tables.sort_by(|a, b| {
            (&a.schema_physical, &a.physical).cmp(&(&b.schema_physical, &b.physical))
        });

        self.resolve_foreign_key_targets();
        self.check_foreign_key_targets();
        self.check_physical_collisions();
    }

    fn add_schema(&mut self, s: &SchemaDecl, loc: Loc) {
        let physical = s
            .physical
            .clone()
            .unwrap_or_else(|| naming::physical(&s.name.name));
        if self.schemas.contains_key(&s.name.name) {
            self.err(
                loc,
                "E0111",
                format!("schema `{}` is declared more than once", s.name.name),
                "names.md §5.1",
            );
            return;
        }
        self.schemas.insert(s.name.name.clone(), physical.clone());
        self.model.schemas.push(SchemaObj {
            declared: s.name.name.clone(),
            physical,
            loc,
        });
    }

    fn add_enum(&mut self, e: &EnumDecl, loc: Loc) {
        if self.enums.contains_key(&e.name.name) {
            self.err(
                loc,
                "E0111",
                format!("enum `{}` is declared more than once", e.name.name),
                "names.md §5.1",
            );
            return;
        }
        let physical = e
            .physical
            .clone()
            .unwrap_or_else(|| naming::physical(&e.name.name));
        let schema = e.schema.as_ref().map(|q| {
            self.schemas
                .get(&q.schema.name)
                .cloned()
                .unwrap_or_else(|| naming::physical(&q.schema.name))
        });
        if let Some(q) = &e.schema {
            if !self.schemas.contains_key(&q.schema.name) {
                self.err_note(
                    loc,
                    "E0450",
                    format!("unknown schema `{}`", q.schema.name),
                    "declare it: `schema <name> of <Database>;`",
                    "schema.md §1",
                );
            }
        }
        self.enums.insert(
            e.name.name.clone(),
            EnumObj {
                declared: e.name.name.clone(),
                schema,
                physical,
                members: e.members.iter().map(|m| m.name.clone()).collect(),
                docs: e.at.docs.clone(),
                loc,
            },
        );
    }

    fn add_table(&mut self, t: &TableDecl, loc: Loc) {
        let schema_physical = match self.schemas.get(&t.schema.schema.name) {
            Some(p) => p.clone(),
            None => {
                self.err_note(
                    Loc {
                        file: loc.file,
                        span: t.schema.span,
                    },
                    "E0450",
                    format!("unknown schema `{}`", t.schema.schema.name),
                    "declare it: `schema <name> of <Database>;`",
                    "schema.md §1",
                );
                naming::physical(&t.schema.schema.name)
            }
        };
        let physical = t
            .physical
            .clone()
            .unwrap_or_else(|| naming::physical(&t.name.name));

        let mut columns = Vec::new();
        let mut primary_key: Option<PrimaryKeyObj> = None;
        let mut uniques = Vec::new();
        let mut checks = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut indexes = Vec::new();
        let mut touch_columns = Vec::new();
        let mut pk_from_column: Vec<String> = Vec::new();
        let mut table_check_ordinal = 0usize;

        for c in &t.columns {
            let cloc = Loc {
                file: loc.file,
                span: c.span,
            };
            let ty = self.map_type(&c.ty, cloc);
            let mut col = ColumnObj {
                declared: c.name.name.clone(),
                physical: naming::physical(&c.name.name),
                was: None,
                ty,
                nullable: c.ty.optional || c.ty.array_optional.last().copied().unwrap_or(false),
                identity: false,
                default: None,
                private: false,
                server: false,
                on_update_now: false,
                docs: c.at.docs.clone(),
                loc: cloc,
            };

            for m in &c.modifiers {
                match m {
                    ColumnModifier::PrimaryKey(_) => pk_from_column.push(col.physical.clone()),
                    ColumnModifier::Identity(sp) => {
                        if !col.ty.is_integer() {
                            self.err_note(
                                Loc {
                                    file: loc.file,
                                    span: *sp,
                                },
                                "E0401",
                                format!(
                                    "`identity` on `{}`, which is `{}`",
                                    col.declared,
                                    col.ty.render()
                                ),
                                "GENERATED AS IDENTITY needs smallint, int or bigint",
                                "schema.md §2.3",
                            );
                        }
                        col.identity = true;
                    }
                    ColumnModifier::Private(_) => col.private = true,
                    ColumnModifier::Server(_) => col.server = true,
                    ColumnModifier::Physical(p, _) => col.physical = p.clone(),
                    ColumnModifier::Was(p, _) => col.was = Some(p.clone()),
                    ColumnModifier::Default(e, sp) => {
                        match self.const_default(e, &col.ty) {
                            Some(sql) => col.default = Some(sql),
                            None => self.err_note(
                                Loc {
                                    file: loc.file,
                                    span: *sp,
                                },
                                "E0402",
                                format!("`default` on `{}` is not a constant", col.declared),
                                "a default is a literal, an enum member, `now()` or \
                                 `gen_random_uuid()` — it is evaluated by Postgres",
                                "schema.md §2.4",
                            ),
                        }
                        if is_now_call(e) && !col.ty.is_temporal() {
                            self.err(
                                Loc {
                                    file: loc.file,
                                    span: *sp,
                                },
                                "E0403",
                                format!(
                                    "`default now()` on `{}`, which is `{}`",
                                    col.declared,
                                    col.ty.render()
                                ),
                                "schema.md §2.4",
                            );
                        }
                    }
                    ColumnModifier::OnUpdate(e, sp) => {
                        if !is_now_call(e) {
                            self.err_note(
                                Loc {
                                    file: loc.file,
                                    span: *sp,
                                },
                                "E0430",
                                "`on update` accepts only `now()`",
                                "a general expression here would be a stored procedure language",
                                "schema.md §6",
                            );
                        }
                        col.on_update_now = true;
                        touch_columns.push(col.physical.clone());
                    }
                    ColumnModifier::Unique { message, span } => {
                        uniques.push(UniqueObj {
                            name: naming::unique_constraint(&physical, &[col.physical.clone()]),
                            columns: vec![col.physical.clone()],
                            predicate: None,
                            message: message.clone(),
                            loc: Loc {
                                file: loc.file,
                                span: *span,
                            },
                        });
                    }
                    ColumnModifier::Rule(r) => {
                        if let Some(chk) = self.rule_check(&physical, &col, r) {
                            checks.push(chk);
                        }
                    }
                }
            }

            // An inline enum (no `of`) carries its own membership CHECK.
            if let Some(declared) = col.ty.inline_enum() {
                if let Some(e) = self.enums.get(declared) {
                    let members = e
                        .members
                        .iter()
                        .map(|m| format!("'{m}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let col_sql = naming::quote_ident(&col.physical);
                    let body = if col.nullable {
                        format!("{col_sql} IS NULL OR {col_sql} IN ({members})")
                    } else {
                        format!("{col_sql} IN ({members})")
                    };
                    checks.push(CheckObj {
                        name: naming::check_column(&physical, &col.physical, "enum"),
                        expr: body,
                        message: None,
                        loc: cloc,
                    });
                }
            }

            columns.push(col);
        }

        if !pk_from_column.is_empty() {
            primary_key = Some(PrimaryKeyObj {
                name: naming::primary_key(&physical),
                columns: pk_from_column.clone(),
                loc,
            });
        }

        for c in &t.constraints {
            match c {
                TableConstraint::PrimaryKey {
                    columns: cols,
                    span,
                } => {
                    let cloc = Loc {
                        file: loc.file,
                        span: *span,
                    };
                    if primary_key.is_some() {
                        self.err_note(
                            cloc,
                            "E0420",
                            "this table declares a primary key twice",
                            "use either the column-level `primary key` or the table-level form",
                            "schema.md §4.1",
                        );
                        continue;
                    }
                    let phys = self.resolve_columns(&columns, cols, cloc);
                    primary_key = Some(PrimaryKeyObj {
                        name: naming::primary_key(&physical),
                        columns: phys,
                        loc: cloc,
                    });
                }
                TableConstraint::ForeignKey {
                    columns: cols,
                    target,
                    target_columns,
                    on_delete,
                    on_update,
                    span,
                } => {
                    let cloc = Loc {
                        file: loc.file,
                        span: *span,
                    };
                    if cols.len() != target_columns.len() {
                        self.err_note(
                            cloc,
                            "E0421",
                            format!(
                                "foreign key has {} column(s) but references {}",
                                cols.len(),
                                target_columns.len()
                            ),
                            "a composite foreign key is one constraint, not one per column",
                            "schema.md §4.2",
                        );
                    }
                    let phys = self.resolve_columns(&columns, cols, cloc);
                    if matches!(on_delete, Some(RefAction::SetNull)) {
                        for name in cols {
                            if let Some(col) = columns.iter().find(|c| c.declared == name.name) {
                                if !col.nullable {
                                    self.err_note(
                                        cloc,
                                        "E0423",
                                        format!(
                                            "`on delete set null` on `{}`, which is NOT NULL",
                                            col.declared
                                        ),
                                        "declare it nullable: `{name} T?`",
                                        "schema.md §4.2",
                                    );
                                }
                            }
                        }
                    }
                    let target_schema = self
                        .schemas
                        .get(&target.schema.name)
                        .cloned()
                        .unwrap_or_else(|| naming::physical(&target.schema.name));
                    foreign_keys.push(ForeignKeyObj {
                        name: naming::foreign_key(&physical, &phys),
                        columns: phys,
                        target_schema,
                        // Provisional: the snake_case of the declared name is
                        // right only when the target did not override its
                        // physical name. `resolve_foreign_key_targets` fixes
                        // it once every table is in the model.
                        target_table: naming::physical(&target.object.name),
                        target_declared: target.object.name.clone(),
                        target_columns: target_columns
                            .iter()
                            .map(|i| naming::physical(&i.name))
                            .collect(),
                        on_delete: *on_delete,
                        on_update: *on_update,
                        loc: cloc,
                    });
                }
                TableConstraint::Unique {
                    columns: cols,
                    predicate,
                    message,
                    span,
                } => {
                    let cloc = Loc {
                        file: loc.file,
                        span: *span,
                    };
                    let phys = self.resolve_columns(&columns, cols, cloc);
                    let pred = predicate
                        .as_ref()
                        .map(|p| self.canonical_predicate(p, &columns));
                    let name = match &pred {
                        Some(p) => naming::unique_partial_index(&physical, &phys, p),
                        None => naming::unique_constraint(&physical, &phys),
                    };
                    uniques.push(UniqueObj {
                        name,
                        columns: phys,
                        predicate: pred,
                        message: message.clone(),
                        loc: cloc,
                    });
                }
                TableConstraint::Check {
                    expr,
                    message,
                    span,
                } => {
                    let cloc = Loc {
                        file: loc.file,
                        span: *span,
                    };
                    self.check_functions(expr, cloc);
                    let sql = self.canonical_predicate(expr, &columns);
                    let mentioned = mentioned_columns(expr, &columns);
                    // The ordinal counts table-form checks only. Counting
                    // column rules too would renumber every table check
                    // whenever a `minLength(…)` is added elsewhere in the
                    // table, and renaming a live constraint is what
                    // schema.md §8.2 exists to prevent.
                    table_check_ordinal += 1;
                    let ordinal = table_check_ordinal;
                    checks.push(CheckObj {
                        name: naming::check_table(&physical, &mentioned, ordinal),
                        expr: sql,
                        message: message.clone(),
                        loc: cloc,
                    });
                }
            }
        }

        for ix in &t.indexes {
            let iloc = Loc {
                file: loc.file,
                span: ix.span,
            };
            let cols: Vec<IndexColumnObj> = ix
                .columns
                .iter()
                .map(|c| IndexColumnObj {
                    physical: columns
                        .iter()
                        .find(|x| x.declared == c.name.name)
                        .map(|x| x.physical.clone())
                        .unwrap_or_else(|| naming::physical(&c.name.name)),
                    desc: c.desc,
                    nulls: c.nulls,
                })
                .collect();
            for c in &ix.columns {
                if !columns.iter().any(|x| x.declared == c.name.name) {
                    self.err(
                        iloc,
                        "E0451",
                        format!("`{}` is not a column of `{}`", c.name.name, t.name.name),
                        "schema.md §1",
                    );
                }
            }
            let phys: Vec<String> = cols.iter().map(|c| c.physical.clone()).collect();
            let pred = ix
                .predicate
                .as_ref()
                .map(|p| self.canonical_predicate(p, &columns));
            if let Some(m) = &ix.method {
                self.check_index_method(m, &cols, &columns, iloc);
            }
            let method = ix.method.as_ref().map(|m| m.name.as_str());
            let name = match &pred {
                Some(p) => naming::index_partial(&physical, &phys, p, method),
                None => naming::index(&physical, &phys, method),
            };
            indexes.push(IndexObj {
                name,
                columns: cols,
                predicate: pred,
                unique: false,
                method: ix.method.as_ref().map(|m| m.name.clone()),
                loc: iloc,
            });
        }

        if primary_key.is_none() {
            self.warn(
                loc,
                "W0401",
                format!("table `{}` has no primary key", t.name.name),
                "schema.md §4.1",
            );
        }

        self.model.tables.push(TableObj {
            declared: t.name.name.clone(),
            schema: t.schema.schema.name.clone(),
            schema_physical,
            physical,
            was: t.was.clone(),
            docs: t.at.docs.clone(),
            columns,
            primary_key,
            uniques,
            checks,
            foreign_keys,
            indexes,
            touch_columns,
            loc,
        });
    }

    /// `using <method>` is passed to Postgres, but two mistakes are worth
    /// catching before deploy: an access method that does not exist, and
    /// GIN over a plain scalar — which needs an operator class JWC does not
    /// install (schema.md §4.5).
    fn check_index_method(
        &mut self,
        method: &Ident,
        cols: &[IndexColumnObj],
        columns: &[ColumnObj],
        loc: Loc,
    ) {
        const METHODS: &[&str] = &["btree", "hash", "gin", "gist", "brin", "spgist"];
        let m = method.name.to_lowercase();
        if !METHODS.contains(&m.as_str()) {
            self.err_note(
                loc,
                "E0431",
                format!("`{}` is not a Postgres access method", method.name),
                "one of btree, hash, gin, gist, brin, spgist",
                "schema.md §4.5",
            );
            return;
        }
        if m != "gin" {
            return;
        }
        for c in cols {
            let Some(col) = columns.iter().find(|x| x.physical == c.physical) else {
                continue;
            };
            let ok = matches!(&col.ty, SqlType::Array(_))
                || matches!(&col.ty, SqlType::Scalar(s) if s == "jsonb" || s == "tsvector");
            if !ok {
                self.err_note(
                    loc,
                    "E0431",
                    format!(
                        "GIN index on `{}`, which is `{}`",
                        col.declared,
                        col.ty.render()
                    ),
                    "GIN indexes an array, jsonb or tsvector out of the box. For text \
                     search on a varchar/text column Postgres needs `gin_trgm_ops` from \
                     the pg_trgm extension, which JWC does not install — create that \
                     index by hand",
                    "schema.md §4.5",
                );
            }
        }
    }

    fn resolve_columns(&mut self, columns: &[ColumnObj], names: &[Ident], loc: Loc) -> Vec<String> {
        let mut out = Vec::new();
        for n in names {
            match columns.iter().find(|c| c.declared == n.name) {
                Some(c) => out.push(c.physical.clone()),
                None => {
                    self.err(
                        loc,
                        "E0451",
                        format!("`{}` is not a column of this table", n.name),
                        "schema.md §1",
                    );
                    out.push(naming::physical(&n.name));
                }
            }
        }
        out
    }

    fn map_type(&mut self, t: &TypeRef, loc: Loc) -> SqlType {
        let base = match &t.kind {
            TypeKind::Scalar { name, args } => SqlType::Scalar(render_scalar(name, args)),
            TypeKind::Named(d) => {
                let declared = d.text();
                match self.enums.get(&declared) {
                    Some(e) if e.is_typed() => SqlType::Enum {
                        qualified: format!(
                            "{}.{}",
                            naming::quote_ident(e.schema.as_deref().unwrap_or("public")),
                            naming::quote_ident(&e.physical)
                        ),
                        declared,
                    },
                    Some(e) => SqlType::EnumInline {
                        width: e.varchar_width(),
                        declared,
                    },
                    None => {
                        self.err_note(
                            loc,
                            "E0301",
                            format!("unknown type `{declared}`"),
                            "a column's type is a scalar (types.md §2.1) or a declared `enum`",
                            "types.md §2.1",
                        );
                        SqlType::Scalar("text".into())
                    }
                }
            }
            TypeKind::Record(_) => {
                self.err_note(
                    loc,
                    "E0301",
                    "a record type is not a column type",
                    "record types describe function returns, not storage",
                    "types.md §1",
                );
                SqlType::Scalar("jsonb".into())
            }
        };
        let mut ty = base;
        for _ in 0..t.array_depth {
            ty = SqlType::Array(Box::new(ty));
        }
        ty
    }

    /// A column rule (`minLength(2)`, `pattern(r"…")`, `min(0)`, …) becomes
    /// a CHECK. On a nullable column the constraint is guarded so NULL never
    /// violates a length or pattern rule (schema.md §4.4).
    fn rule_check(&mut self, table: &str, col: &ColumnObj, r: &RuleCall) -> Option<CheckObj> {
        let c = naming::quote_ident(&col.physical);
        let arg = |i: usize| -> Option<String> { r.args.get(i).map(literal_sql) };
        let body = match r.name.name.as_str() {
            "minLength" => Some(format!("char_length({c}) >= {}", arg(0)?)),
            "maxLength" => Some(format!("char_length({c}) <= {}", arg(0)?)),
            "min" => Some(format!("{c} >= {}", arg(0)?)),
            "max" => Some(format!("{c} <= {}", arg(0)?)),
            "pattern" => Some(format!("{c} ~ {}", arg(0)?)),
            "oneOf" => Some(format!(
                "{c} IN ({})",
                r.args
                    .iter()
                    .map(literal_sql)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            // `required` is meaningful on a class field, not on a column:
            // a column is NOT NULL unless it says `?`.
            "required" => {
                self.err_note(
                    col.loc,
                    "E0452",
                    "`required` is a class rule, not a column modifier",
                    "a column is NOT NULL by default; write `T?` to make it nullable",
                    "schema.md §2.1",
                );
                None
            }
            other => {
                self.err_note(
                    col.loc,
                    "E0453",
                    format!("unknown column rule `{other}`"),
                    "one of minLength, maxLength, min, max, pattern, oneOf",
                    "schema.md §2.2",
                );
                None
            }
        }?;
        let expr = if col.nullable {
            format!("{c} IS NULL OR {body}")
        } else {
            body
        };
        Some(CheckObj {
            name: naming::check_column(table, &col.physical, &r.name.name.to_lowercase()),
            expr,
            message: None,
            loc: col.loc,
        })
    }

    /// Canonical SQL for a constraint / index predicate (schema.md §4.3).
    ///
    /// Canonical means: `== null` becomes `IS NULL`, an enum member becomes
    /// its physical literal, and `AND`/`OR` operands are sorted by their own
    /// canonical text — so `a and b` and `b and a` produce the same index
    /// name and therefore no spurious migration.
    /// config.md §2.4 — an unknown `init()` key.
    ///
    /// A typo here is silent otherwise: the pool takes its default and the
    /// deployment runs with settings nobody chose, which shows up as
    /// latency rather than as an error.
    fn check_init_keys(&mut self, db: &DatabaseDecl, loc: Loc) {
        const KEYS: [&str; 7] = [
            "pool_size",
            "pool_timeout",
            "statement_timeout",
            "connect_timeout",
            "tls",
            "tls_root_cert",
            "application_name",
        ];
        // config.md §2.3 — `init()` runs before any connection is opened,
        // so a query there is circular and I/O is a surprise at boot.
        const ALLOWED: [&str; 6] = ["env", "int", "bigint", "boolean", "text", "numeric"];
        for a in &db.init {
            let mut calls = Vec::new();
            init_calls(&a.value, &mut calls);
            for (name, span) in calls {
                if !ALLOWED.contains(&name.as_str()) {
                    self.err_note(
                        Loc {
                            file: loc.file,
                            span,
                        },
                        "E1201",
                        format!("`{name}(...)` inside `init()`"),
                        "`init()` runs before any connection is opened; it may call \
                         `env()` and the coercions and nothing else",
                        "config.md §2.3",
                    );
                }
            }
            if !KEYS.contains(&a.key.name.as_str()) {
                self.err_note(
                    Loc {
                        file: loc.file,
                        span: a.key.span,
                    },
                    "E1202",
                    format!("unknown `init()` key `{}`", a.key.name),
                    format!("the keys are: {}", KEYS.join(", ")),
                    "config.md §2.4",
                );
            }
        }
    }

    /// schema.md §4.4 — a `check` may call only the canonical set.
    ///
    /// The constraint is stored in the database and re-evaluated on every
    /// write forever. A call to anything else is either not there at all
    /// (the DDL fails to apply, which is the good case) or is a
    /// user-defined function whose definition the schema does not carry —
    /// so the table cannot be recreated from this source.
    fn check_functions(&mut self, e: &Expr, loc: Loc) {
        const CANONICAL: [&str; 4] = ["char_length", "lower", "upper", "coalesce"];
        match &*e.kind {
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Name(n) = &*callee.kind {
                    if !CANONICAL.contains(&n.name.as_str()) {
                        self.err_note(
                            loc,
                            "E0424",
                            format!("`{}(...)` is not allowed in a `check`", n.name),
                            "a check is stored in the database and re-evaluated on every \
                             write; only `char_length`, `lower`, `upper`, `coalesce` and \
                             the `~` operator are portable enough to live there",
                            "schema.md §4.4",
                        );
                    }
                }
                for a in args {
                    self.check_functions(a, loc);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
                self.check_functions(lhs, loc);
                self.check_functions(rhs, loc);
            }
            ExprKind::Unary { rhs, .. } => self.check_functions(rhs, loc),
            ExprKind::In { lhs, items, .. } => {
                self.check_functions(lhs, loc);
                for i in items {
                    self.check_functions(i, loc);
                }
            }
            _ => {}
        }
    }

    fn canonical_predicate(&self, e: &Expr, columns: &[ColumnObj]) -> String {
        canonical_expr(e, columns, &self.enums)
    }

    fn const_default(&self, e: &Expr, ty: &SqlType) -> Option<String> {
        match &*e.kind {
            ExprKind::Int(n) | ExprKind::Decimal(n) => Some(n.clone()),
            ExprKind::Str(s) => Some(sql_string(s)),
            ExprKind::Bool(b) => Some(b.to_string()),
            ExprKind::Null => Some("NULL".into()),
            ExprKind::Array(items) if items.is_empty() => Some("'{}'".into()),
            ExprKind::Call { callee, args, .. } if args.is_empty() => match &*callee.kind {
                ExprKind::Name(n) if n.name == "now" => Some("now()".into()),
                ExprKind::Name(n) if n.name == "gen_random_uuid" => {
                    Some("gen_random_uuid()".into())
                }
                _ => None,
            },
            // `MemberRole.member` — an enum member.
            ExprKind::Field { base, field } => match &*base.kind {
                ExprKind::Name(n) => {
                    let e = self.enums.get(&n.name)?;
                    if !e.members.contains(&field.name) {
                        return None;
                    }
                    let lit = sql_string(&field.name);
                    Some(match ty {
                        SqlType::Enum { qualified, .. } => format!("{lit}::{qualified}"),
                        _ => lit,
                    })
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Point every foreign key at its target's **physical** name.
    ///
    /// A reference names the target as declared — `references App.public.Users`
    /// — and the target may have renamed itself with `as "user"`. Deriving the
    /// physical name from the reference instead of from the target made every
    /// such key unresolvable: `E0422: public.users is not a declared table`,
    /// against a program that declares exactly that table. `as "…"` exists so
    /// a program can keep the physical names a database already has, which is
    /// what a port needs, and this made it unusable on any table another one
    /// points at.
    fn resolve_foreign_key_targets(&mut self) {
        use std::collections::HashMap;
        let by_declared: HashMap<(String, String), String> = self
            .model
            .tables
            .iter()
            .map(|t| {
                (
                    (t.schema_physical.clone(), t.declared.clone()),
                    t.physical.clone(),
                )
            })
            .collect();
        for t in &mut self.model.tables {
            for fk in &mut t.foreign_keys {
                let key = (fk.target_schema.clone(), fk.target_declared.clone());
                if let Some(physical) = by_declared.get(&key) {
                    fk.target_table = physical.clone();
                }
                // No entry means the target is not declared at all, which is
                // `check_foreign_key_targets`'s message to give.
            }
        }
    }

    /// Every foreign key must point at a declared table, and at a column set
    /// that is that table's primary key or a unique constraint — Postgres
    /// requires it, and saying so here turns a deploy failure into an edit
    /// (schema.md §4.2).
    fn check_foreign_key_targets(&mut self) {
        let tables = self.model.tables.clone();
        let mut problems: Vec<(Loc, &'static str, String, String)> = Vec::new();
        for t in &tables {
            for fk in &t.foreign_keys {
                let target = tables.iter().find(|x| {
                    x.schema_physical == fk.target_schema && x.physical == fk.target_table
                });
                let Some(target) = target else {
                    problems.push((
                        fk.loc,
                        "E0422",
                        format!(
                            "`{}.{}` is not a declared table",
                            fk.target_schema, fk.target_table
                        ),
                        "every foreign key target must be declared in this program".into(),
                    ));
                    continue;
                };
                let pk_match = target
                    .primary_key
                    .as_ref()
                    .is_some_and(|pk| same_set(&pk.columns, &fk.target_columns));
                let uq_match = target
                    .uniques
                    .iter()
                    .any(|u| u.predicate.is_none() && same_set(&u.columns, &fk.target_columns));
                if !pk_match && !uq_match {
                    problems.push((
                        fk.loc,
                        "E0422",
                        format!(
                            "`{}.{} ({})` is not a primary key or unique constraint",
                            fk.target_schema,
                            fk.target_table,
                            fk.target_columns.join(", ")
                        ),
                        "Postgres requires a foreign key to reference a unique column set; \
                         a partial unique index does not qualify"
                            .into(),
                    ));
                }
            }
        }
        for (loc, code, msg, note) in problems {
            self.err_note(loc, code, msg, note, "schema.md §4.2");
        }
    }

    /// Two objects in one schema whose physical names collide. Reachable
    /// only through an `as "…"` override (names.md §4.3).
    fn check_physical_collisions(&mut self) {
        let mut seen: BTreeMap<(String, String), Loc> = BTreeMap::new();
        let mut problems = Vec::new();
        for t in &self.model.tables {
            let key = (t.schema_physical.clone(), t.physical.clone());
            match seen.get(&key) {
                Some(_) => problems.push((
                    t.loc,
                    format!(
                        "two objects in schema `{}` both map to `{}`",
                        t.schema_physical, t.physical
                    ),
                )),
                None => {
                    seen.insert(key, t.loc);
                }
            }
        }
        for (loc, msg) in problems {
            self.err(loc, "E0110", msg, "names.md §4.3");
        }
    }
}

// ---------------------------------------------------------------- helpers

fn same_set(a: &[String], b: &[String]) -> bool {
    let mut a: Vec<&String> = a.iter().collect();
    let mut b: Vec<&String> = b.iter().collect();
    a.sort();
    b.sort();
    a == b
}

fn render_scalar(name: &str, args: &[u32]) -> String {
    match name {
        "int" => "integer".into(),
        "varchar" => format!("varchar({})", args.first().copied().unwrap_or(255)),
        "numeric" => {
            if args.len() == 2 {
                format!("numeric({}, {})", args[0], args[1])
            } else {
                "numeric".into()
            }
        }
        other => other.to_string(),
    }
}

fn sql_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn is_now_call(e: &Expr) -> bool {
    matches!(&*e.kind, ExprKind::Call { callee, args, .. }
        if args.is_empty() && matches!(&*callee.kind, ExprKind::Name(n) if n.name == "now"))
}

fn literal_sql(e: &Expr) -> String {
    match &*e.kind {
        ExprKind::Int(n) | ExprKind::Decimal(n) => n.clone(),
        ExprKind::Str(s) | ExprKind::RawStr(s) => sql_string(s),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Null => "NULL".into(),
        _ => "NULL".into(),
    }
}

fn mentioned_columns(e: &Expr, columns: &[ColumnObj]) -> Vec<String> {
    let mut out = Vec::new();
    walk_names(e, &mut |n| {
        if let Some(c) = columns.iter().find(|c| c.declared == n) {
            if !out.contains(&c.physical) {
                out.push(c.physical.clone());
            }
        }
    });
    if out.is_empty() {
        out.push("expr".into());
    }
    out
}

fn walk_names(e: &Expr, f: &mut impl FnMut(&str)) {
    match &*e.kind {
        ExprKind::Name(n) => f(&n.name),
        ExprKind::Field { base, .. } => walk_names(base, f),
        ExprKind::Unary { rhs, .. } => walk_names(rhs, f),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            walk_names(lhs, f);
            walk_names(rhs, f);
        }
        ExprKind::In { lhs, items, .. } => {
            walk_names(lhs, f);
            for i in items {
                walk_names(i, f);
            }
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                walk_names(a, f);
            }
        }
        _ => {}
    }
}

/// The canonicaliser. Public so migrations can reuse it verbatim — a
/// predicate that canonicalises differently between `gen-sql` and
/// `migrate new` would produce a phantom diff.
pub fn canonical_expr(
    e: &Expr,
    columns: &[ColumnObj],
    enums: &BTreeMap<String, EnumObj>,
) -> String {
    match &*e.kind {
        ExprKind::Binary { op, lhs, rhs } => {
            let is_null_rhs = matches!(&*rhs.kind, ExprKind::Null);
            let is_null_lhs = matches!(&*lhs.kind, ExprKind::Null);
            match op {
                BinOp::Eq if is_null_rhs => {
                    return format!("{} IS NULL", canonical_expr(lhs, columns, enums))
                }
                BinOp::Eq if is_null_lhs => {
                    return format!("{} IS NULL", canonical_expr(rhs, columns, enums))
                }
                BinOp::Ne if is_null_rhs => {
                    return format!("{} IS NOT NULL", canonical_expr(lhs, columns, enums))
                }
                BinOp::Ne if is_null_lhs => {
                    return format!("{} IS NOT NULL", canonical_expr(rhs, columns, enums))
                }
                BinOp::And | BinOp::Or => {
                    // Sort operands so `a and b` and `b and a` are the same
                    // predicate, and therefore the same index name.
                    let mut parts = [
                        canonical_expr(lhs, columns, enums),
                        canonical_expr(rhs, columns, enums),
                    ];
                    parts.sort();
                    let sep = if matches!(op, BinOp::And) {
                        "AND"
                    } else {
                        "OR"
                    };
                    return parts
                        .iter()
                        .map(|p| format!("({p})"))
                        .collect::<Vec<_>>()
                        .join(&format!(" {sep} "));
                }
                _ => {}
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
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Rem => "%",
                BinOp::EqOpt => "=",
                BinOp::And => "AND",
                BinOp::Or => "OR",
            };
            format!(
                "{} {sql_op} {}",
                canonical_expr(lhs, columns, enums),
                canonical_expr(rhs, columns, enums)
            )
        }
        ExprKind::Unary { op, rhs } => match op {
            UnaryOp::Not => format!("NOT ({})", canonical_expr(rhs, columns, enums)),
            UnaryOp::Neg => format!("-{}", canonical_expr(rhs, columns, enums)),
        },
        ExprKind::Name(n) => match columns.iter().find(|c| c.declared == n.name) {
            Some(c) => naming::quote_ident(&c.physical),
            None => naming::quote_ident(&naming::physical(&n.name)),
        },
        // An enum member reduces to its physical literal (schema.md §4.3).
        ExprKind::Field { base, field } => match &*base.kind {
            ExprKind::Name(n) if enums.contains_key(&n.name) => sql_string(&field.name),
            _ => format!(
                "{}.{}",
                canonical_expr(base, columns, enums),
                naming::quote_ident(&field.name)
            ),
        },
        ExprKind::Int(n) | ExprKind::Decimal(n) => n.clone(),
        ExprKind::Str(s) | ExprKind::RawStr(s) => sql_string(s),
        ExprKind::Bool(b) => b.to_string().to_uppercase(),
        ExprKind::Null => "NULL".into(),
        ExprKind::In {
            lhs,
            items,
            negated,
        } => format!(
            "{} {}IN ({})",
            canonical_expr(lhs, columns, enums),
            if *negated { "NOT " } else { "" },
            items
                .iter()
                .map(|i| canonical_expr(i, columns, enums))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ExprKind::Call { callee, args, .. } => {
            let name = match &*callee.kind {
                ExprKind::Name(n) => n.name.clone(),
                other => format!("{other:?}"),
            };
            format!(
                "{name}({})",
                args.iter()
                    .map(|a| canonical_expr(a, columns, enums))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        _ => "NULL".into(),
    }
}

/// Convenience for callers that have no column table (migration snapshots
/// re-canonicalise stored text).
pub fn canonical_span(_s: Span) {}

/// Every call in an `init()` value.
fn init_calls(e: &Expr, out: &mut Vec<(String, crate::token::Span)>) {
    match &*e.kind {
        ExprKind::Call { callee, args, .. } => {
            match &*callee.kind {
                ExprKind::Name(n) => out.push((n.name.clone(), e.span)),
                // `hash.password(...)`, `redis.get(...)` — anything with a
                // namespace is out by construction.
                ExprKind::Field { base, field } => {
                    if let ExprKind::Name(b) = &*base.kind {
                        out.push((format!("{}.{}", b.name, field.name), e.span));
                    }
                }
                _ => {}
            }
            for a in args {
                init_calls(a, out);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            init_calls(lhs, out);
            init_calls(rhs, out);
        }
        ExprKind::Unary { rhs, .. } => init_calls(rhs, out),
        ExprKind::Select(_) | ExprKind::Insert(_) | ExprKind::Update(_) | ExprKind::Delete(_) => {
            out.push(("a query".into(), e.span))
        }
        _ => {}
    }
}

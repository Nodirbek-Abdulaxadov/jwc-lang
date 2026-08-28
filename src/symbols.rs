//! The program-wide symbol table.
//!
//! One flat declaration space (names.md §5.1) built from every file's AST
//! plus the resolved schema model. The checker reads this; nothing here
//! type-checks bodies.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::model::{SchemaModel, SqlType, TableObj};
use crate::types::{Fields, Scalar, Ty};
use crate::workspace::{Loc, Workspace};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct TableSym {
    pub declared: String,
    /// Per column: has a `default`, and is `identity`. An insert may leave
    /// either kind unset (types.md §9.5).
    pub defaulted: Vec<(String, bool, bool)>,
    pub schema: String,
    /// Declared column name -> type, in declaration order.
    pub columns: Vec<(String, Ty)>,
    pub private: Vec<String>,
    pub server: Vec<String>,
    /// Column sets that provably select at most one row: the primary key
    /// and every non-partial unique constraint (queries.md §5.2).
    pub unique_sets: Vec<Vec<String>>,
    /// Partial uniques, as (columns, canonical predicate).
    pub partial_uniques: Vec<(Vec<String>, String)>,
    /// Constraint promotion inputs (errors.md §6): a constraint carrying a
    /// message raises a declared error, a message-less one is a fault.
    pub has_messaged_unique: bool,
    pub has_messaged_check: bool,
    pub has_foreign_key: bool,
    pub loc: Loc,
}

impl Symbols {
    /// `(column, has_default, is_identity)` for a table.
    pub fn table_defaults(&self, table: &str) -> Vec<(String, bool, bool)> {
        self.tables
            .get(table)
            .map(|t| t.defaulted.clone())
            .unwrap_or_default()
    }
}

impl TableSym {
    pub fn column(&self, name: &str) -> Option<&Ty> {
        self.columns.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    pub fn is_private(&self, name: &str) -> bool {
        self.private.iter().any(|n| n == name)
    }

    pub fn is_server(&self, name: &str) -> bool {
        self.server.iter().any(|n| n == name)
    }
}

#[derive(Clone, Debug)]
pub struct ViewSym {
    pub declared: String,
    pub schema: String,
    /// The projected shape — a view is a named projection, so selecting
    /// from one yields a `Record` (types.md §5.3).
    pub shape: Fields,
    /// Aliases of driving-table columns that inherit its uniqueness
    /// (queries.md §5.2.1): projected name -> source column.
    pub inherited: BTreeMap<String, String>,
    pub driving_table: String,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct ClassSym {
    pub declared: String,
    pub fields: Vec<ClassFieldSym>,
    pub loc: Loc,
}

/// One validation rule on a class field: `minLength(2)`, and the message
/// its violation should carry.
///
/// A struct rather than a `(String, Vec<Expr>)` tuple because the third
/// member is optional and easy to misread positionally.
#[derive(Clone, Debug)]
pub struct ClassRule {
    pub name: String,
    pub args: Vec<Expr>,
    /// `: "…"`. `None` falls back to the generated sentence.
    pub message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ClassFieldSym {
    pub name: String,
    pub ty: Ty,
    pub transient: bool,
    pub rules: Vec<ClassRule>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct EnumSym {
    pub declared: String,
    pub members: Vec<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct ErrorSym {
    pub declared: String,
    pub params: Vec<(String, Ty)>,
    pub status: u16,
    pub loc: Loc,
    pub predeclared: bool,
}

#[derive(Clone, Debug)]
pub struct FunctionSym {
    pub name: String,
    /// `Some` for a service method.
    pub service: Option<String>,
    pub params: Vec<(String, Ty)>,
    pub returns: Option<Ty>,
    pub raises: Vec<String>,
    pub loc: Loc,
}

impl FunctionSym {
    pub fn qualified(&self) -> String {
        match &self.service {
            Some(s) => format!("{s}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MiddlewareSym {
    pub name: String,
    pub binders: Vec<(String, Ty)>,
    pub requires: Vec<String>,
    pub provides: Vec<(String, Ty)>,
    pub has_after: bool,
    pub loc: Loc,
}

/// One declared `job` (jobs.md §1).
#[derive(Clone, Debug)]
pub struct JobSym {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    /// Total attempts before the dead-letter queue. Default 5.
    pub retries: i64,
    /// Seconds to wait after a failed attempt. Default 30.
    pub backoff_secs: i64,
    pub loc: Loc,
}

#[derive(Default)]
pub struct Symbols {
    pub tables: BTreeMap<String, TableSym>,
    pub views: BTreeMap<String, ViewSym>,
    pub classes: BTreeMap<String, ClassSym>,
    pub enums: BTreeMap<String, EnumSym>,
    pub errors: BTreeMap<String, ErrorSym>,
    pub functions: BTreeMap<String, FunctionSym>,
    pub middleware: BTreeMap<String, MiddlewareSym>,
    pub jobs: BTreeMap<String, JobSym>,
    pub services: BTreeMap<String, Vec<String>>,
    /// `const NAME = …` — the declaration expression, kept so the checker
    /// can type it and both backends can evaluate it (names.md §5.6).
    pub consts: BTreeMap<String, crate::ast::ConstDecl>,
    /// Qualified path (`App.auth.Accounts`) -> declared table or view name.
    pub by_path: BTreeMap<String, String>,
    pub diags: Vec<(Loc, Diagnostic)>,
}

/// One predeclared error: name, default status, parameter list.
pub type PredeclaredError = (&'static str, u16, &'static [(&'static str, Scalar)]);

/// The eight error types that exist without being declared (errors.md §1.2).
pub const PREDECLARED_ERRORS: &[PredeclaredError] = &[
    ("BadRequest", 400, &[("message", Scalar::Text)]),
    ("Unauthorized", 401, &[("message", Scalar::Text)]),
    ("Forbidden", 403, &[("message", Scalar::Text)]),
    ("NotFound", 404, &[("message", Scalar::Text)]),
    ("Conflict", 409, &[("message", Scalar::Text)]),
    ("Gone", 410, &[("message", Scalar::Text)]),
    ("TooManyRequests", 429, &[("message", Scalar::Text)]),
    (
        "ConstraintViolation",
        400,
        &[("message", Scalar::Text), ("constraint", Scalar::Text)],
    ),
];

pub fn build(ws: &Workspace, model: &SchemaModel) -> Symbols {
    let mut s = Symbols::default();

    for (name, status, params) in PREDECLARED_ERRORS {
        s.errors.insert(
            (*name).to_string(),
            ErrorSym {
                declared: (*name).to_string(),
                params: params
                    .iter()
                    .map(|(n, sc)| ((*n).to_string(), Ty::Scalar(*sc)))
                    .collect(),
                status: *status,
                loc: Loc {
                    file: 0,
                    span: Default::default(),
                },
                predeclared: true,
            },
        );
    }

    for t in &model.tables {
        s.tables.insert(t.declared.clone(), table_sym(t));
        s.by_path.insert(
            format!("App.{}.{}", t.schema, t.declared),
            t.declared.clone(),
        );
    }
    // The database identifier is whatever the program declared.
    if let Some(db) = &model.database {
        let paths: Vec<(String, String)> = model
            .tables
            .iter()
            .map(|t| {
                (
                    format!("{db}.{}.{}", t.schema, t.declared),
                    t.declared.clone(),
                )
            })
            .collect();
        for (k, v) in paths {
            s.by_path.insert(k, v);
        }
    }

    for e in &model.enums {
        s.enums.insert(
            e.declared.clone(),
            EnumSym {
                declared: e.declared.clone(),
                members: e.members.clone(),
                loc: e.loc,
            },
        );
    }

    for (fi, file) in ws.files.iter().enumerate() {
        for d in &file.program.decls {
            let loc = Loc {
                file: fi,
                span: d.span(),
            };
            match d {
                Decl::Const(c) => {
                    // Two consts with one name is the same ambiguity two
                    // tables with one name is: which value applies would
                    // depend on the order the files happened to load in.
                    if s.consts.contains_key(&c.name.name) {
                        s.diags.push((
                            loc,
                            Diagnostic::error(
                                "E0215",
                                c.name.span,
                                format!("`{}` is declared twice", c.name.name),
                            )
                            .note("a `const` name is global")
                            .clause("names.md §5.6"),
                        ));
                    } else {
                        s.consts.insert(c.name.name.clone(), c.clone());
                    }
                }
                Decl::Class(c) => {
                    // The enum table is complete by now (pass 1), and a
                    // class field of enum type must resolve to `Enum`, not
                    // to an unresolved name.
                    let enums = s.enums.clone();
                    let sym = class_sym(&mut s.diags, c, loc, &enums);
                    s.classes.insert(c.name.name.clone(), sym);
                }
                Decl::Error(e) => {
                    // Every error carries `message`, declared or not: the
                    // `: "…"` default is that field's value (errors.md §1.1),
                    // and `errorHandler` arms read it.
                    let mut params: Vec<(String, Ty)> = e
                        .params
                        .iter()
                        .map(|p| (p.name.name.clone(), type_of(&p.ty, &s.enums, &s.classes)))
                        .collect();
                    if !params.iter().any(|(n, _)| n == "message") {
                        params.insert(0, ("message".to_string(), Ty::text()));
                    }
                    s.errors.insert(
                        e.name.name.clone(),
                        ErrorSym {
                            declared: e.name.name.clone(),
                            params,
                            status: e.status,
                            loc,
                            predeclared: false,
                        },
                    );
                }
                Decl::Function(f) => {
                    let sym = function_sym(f, None, &s.enums, &s.classes, loc);
                    s.functions.insert(sym.name.clone(), sym);
                }
                Decl::Job(j) => {
                    let params = j
                        .params
                        .iter()
                        .map(|p| (p.name.name.clone(), type_of(&p.ty, &s.enums, &s.classes)))
                        .collect::<Vec<(String, Ty)>>();
                    // A payload has to survive a round trip through the
                    // queue table as JSON, so a job cannot take a class or
                    // a record: those are the request boundary's shapes,
                    // and re-validating one on the way out is a contract
                    // nothing states.
                    for (name, ty) in &params {
                        if matches!(
                            ty.clone().strip_opt(),
                            Ty::Class(_) | Ty::Record(_) | Ty::Raw
                        ) {
                            s.diags.push((
                                loc,
                                Diagnostic::error(
                                    "E0362",
                                    j.span,
                                    format!("job parameter `{name}` is `{ty}`"),
                                )
                                .note(
                                    "a job payload is stored and replayed, so its parameters \
                                     are scalars and arrays of scalars — pass the id, and \
                                     read the row in the handler",
                                )
                                .clause("jobs.md §1.1"),
                            ));
                        }
                    }
                    if s.jobs.contains_key(&j.name.name) {
                        s.diags.push((
                            loc,
                            Diagnostic::error(
                                "E0363",
                                j.span,
                                format!("`job {}` is declared twice", j.name.name),
                            )
                            .note("a job name is the key its queued rows carry")
                            .clause("jobs.md §1.1"),
                        ));
                        // The first wins. Letting the second overwrite it
                        // would recheck every `dispatch` in the program
                        // against the wrong signature, and bury the one
                        // real error under a page of consequences.
                        continue;
                    }
                    s.jobs.insert(
                        j.name.name.clone(),
                        JobSym {
                            name: j.name.name.clone(),
                            params,
                            retries: j.retries.unwrap_or(5),
                            backoff_secs: j
                                .backoff
                                .as_deref()
                                .and_then(|d| {
                                    crate::serve::parse_duration(d).map(|x| x.as_secs() as i64)
                                })
                                .unwrap_or(30),
                            loc,
                        },
                    );
                }
                Decl::Service(sv) => {
                    let mut names = Vec::new();
                    for f in &sv.functions {
                        let floc = Loc {
                            file: fi,
                            span: f.span,
                        };
                        let sym =
                            function_sym(f, Some(sv.name.name.clone()), &s.enums, &s.classes, floc);
                        names.push(sym.name.clone());
                        s.functions.insert(sym.qualified(), sym);
                    }
                    s.services.insert(sv.name.name.clone(), names);
                }
                Decl::Middleware(m) => {
                    s.middleware.insert(
                        m.name.name.clone(),
                        MiddlewareSym {
                            name: m.name.name.clone(),
                            binders: m
                                .binders
                                .iter()
                                .map(|b| {
                                    (b.name.name.clone(), type_of(&b.ty, &s.enums, &s.classes))
                                })
                                .collect(),
                            requires: m.requires.iter().map(|i| i.name.clone()).collect(),
                            provides: m
                                .provides
                                .iter()
                                .map(|p| {
                                    (p.name.name.clone(), type_of(&p.ty, &s.enums, &s.classes))
                                })
                                .collect(),
                            has_after: m.after.is_some(),
                            loc,
                        },
                    );
                }
                _ => {}
            }
        }
    }

    // Views last: their shape is computed from tables that must already be
    // in the table.
    for (fi, file) in ws.files.iter().enumerate() {
        for d in &file.program.decls {
            if let Decl::View(v) = d {
                let loc = Loc {
                    file: fi,
                    span: v.span,
                };
                let sym = view_sym(&s, v, loc);
                s.by_path.insert(
                    format!("App.{}.{}", v.schema.schema.name, v.name.name),
                    v.name.name.clone(),
                );
                if let Some(db) = &model.database {
                    s.by_path.insert(
                        format!("{db}.{}.{}", v.schema.schema.name, v.name.name),
                        v.name.name.clone(),
                    );
                }
                s.views.insert(v.name.name.clone(), sym);
            }
        }
    }

    s
}

fn table_sym(t: &TableObj) -> TableSym {
    let columns = t
        .columns
        .iter()
        .map(|c| {
            let base = sql_to_ty(&c.ty);
            (
                c.declared.clone(),
                if c.nullable { base.opt() } else { base },
            )
        })
        .collect();

    // Physical -> declared, so constraint column lists (which are physical)
    // can be reported in declared terms.
    let declared_of = |phys: &str| -> String {
        t.columns
            .iter()
            .find(|c| c.physical == phys)
            .map(|c| c.declared.clone())
            .unwrap_or_else(|| phys.to_string())
    };

    let mut unique_sets = Vec::new();
    if let Some(pk) = &t.primary_key {
        unique_sets.push(pk.columns.iter().map(|c| declared_of(c)).collect());
    }
    let mut partial_uniques = Vec::new();
    for u in &t.uniques {
        let cols: Vec<String> = u.columns.iter().map(|c| declared_of(c)).collect();
        match &u.predicate {
            None => unique_sets.push(cols),
            Some(p) => partial_uniques.push((cols, p.clone())),
        }
    }

    TableSym {
        declared: t.declared.clone(),
        defaulted: t
            .columns
            .iter()
            .map(|c| (c.declared.clone(), c.default.is_some(), c.identity))
            .collect(),
        schema: t.schema.clone(),
        columns,
        private: t
            .columns
            .iter()
            .filter(|c| c.private)
            .map(|c| c.declared.clone())
            .collect(),
        server: t
            .columns
            .iter()
            .filter(|c| c.server)
            .map(|c| c.declared.clone())
            .collect(),
        unique_sets,
        partial_uniques,
        has_messaged_unique: t.uniques.iter().any(|u| u.message.is_some()),
        has_messaged_check: t.checks.iter().any(|c| c.message.is_some()),
        has_foreign_key: !t.foreign_keys.is_empty(),
        loc: t.loc,
    }
}

fn sql_to_ty(t: &SqlType) -> Ty {
    match t {
        SqlType::Scalar(s) => {
            let name = s.split('(').next().unwrap_or(s);
            let name = if name == "integer" { "int" } else { name };
            Ty::Scalar(Scalar::from_name(name).unwrap_or(Scalar::Text))
        }
        SqlType::Enum { declared, .. } | SqlType::EnumInline { declared, .. } => {
            Ty::Enum(declared.clone())
        }
        SqlType::Array(inner) => sql_to_ty(inner).array(),
    }
}

fn class_sym(
    diags: &mut Vec<(Loc, Diagnostic)>,
    c: &ClassDecl,
    loc: Loc,
    enums: &BTreeMap<String, EnumSym>,
) -> ClassSym {
    let empty_classes = BTreeMap::new();
    let fields = c
        .fields
        .iter()
        .map(|f| {
            let floc = Loc {
                file: loc.file,
                span: f.span,
            };
            let base = type_of(&f.ty, enums, &empty_classes);
            let ty = if f.ty.optional || f.ty.array_optional.last().copied().unwrap_or(false) {
                base.opt()
            } else {
                base
            };
            let rules: Vec<ClassRule> = f
                .rules
                .iter()
                .map(|r| ClassRule {
                    name: r.name.name.clone(),
                    args: r.args.clone(),
                    message: r.message.clone(),
                })
                .collect();

            // types.md §11.1: `minLength` on an array is the overload the
            // gap named; arrays use `minItems`.
            let is_array = f.ty.array_depth > 0;
            for r in &rules {
                match r.name.as_str() {
                    "minLength" | "maxLength" if is_array => diags.push((
                        floc,
                        Diagnostic::error(
                            "E0360",
                            f.span,
                            format!("`{}` on an array field `{}`", r.name, f.name.name),
                        )
                        .note("arrays use `minItems` / `maxItems`")
                        .clause("types.md §11.1"),
                    )),
                    "minItems" | "maxItems" if !is_array => diags.push((
                        floc,
                        Diagnostic::error(
                            "E0360",
                            f.span,
                            format!("`{}` on a scalar field `{}`", r.name, f.name.name),
                        )
                        .note("scalars use `minLength` / `maxLength`")
                        .clause("types.md §11.1"),
                    )),
                    "required" if f.ty.optional => diags.push((
                        floc,
                        Diagnostic::error(
                            "E0361",
                            f.span,
                            format!("`{}` is both `required` and `?`", f.name.name),
                        )
                        .note("drop one: `?` means the field may be absent or null")
                        .clause("types.md §11.1"),
                    )),
                    _ => {}
                }
            }

            ClassFieldSym {
                name: f.name.name.clone(),
                ty,
                transient: f.transient,
                rules,
                loc: floc,
            }
        })
        .collect();
    ClassSym {
        declared: c.name.name.clone(),
        fields,
        loc,
    }
}

fn function_sym(
    f: &FunctionDecl,
    service: Option<String>,
    enums: &BTreeMap<String, EnumSym>,
    classes: &BTreeMap<String, ClassSym>,
    loc: Loc,
) -> FunctionSym {
    FunctionSym {
        name: f.name.name.clone(),
        service,
        params: f
            .params
            .iter()
            .map(|p| (p.name.name.clone(), type_of(&p.ty, enums, classes)))
            .collect(),
        returns: f.returns.as_ref().map(|t| type_of(t, enums, classes)),
        raises: f.raises.iter().map(|i| i.name.clone()).collect(),
        loc,
    }
}

/// A view's projected shape. Nested projections come from join results, so
/// this walks the same structure the query checker does — but only far
/// enough to name the fields; the checker validates them.
fn view_sym(s: &Symbols, v: &ViewDecl, loc: Loc) -> ViewSym {
    let driving = v.body.source.object.name.clone();
    let mut shape = Vec::new();
    let mut inherited = BTreeMap::new();

    if let Some(proj) = &v.body.projection {
        for f in &proj.fields {
            match f {
                ProjField::Column(i) => {
                    let ty = s
                        .tables
                        .get(&driving)
                        .and_then(|t| t.column(&i.name))
                        .cloned()
                        .unwrap_or(Ty::Unknown);
                    inherited.insert(i.name.clone(), i.name.clone());
                    shape.push((i.name.clone(), ty));
                }
                ProjField::Expr { alias, value, .. } => {
                    // `org_id: id` — an alias of a driving column keeps its
                    // uniqueness (queries.md §5.2.1).
                    if let ExprKind::Name(src) = &*value.kind {
                        if let Some(ty) = s.tables.get(&driving).and_then(|t| t.column(&src.name)) {
                            inherited.insert(alias.name.clone(), src.name.clone());
                            shape.push((alias.name.clone(), ty.clone()));
                            continue;
                        }
                    }
                    shape.push((alias.name.clone(), Ty::Unknown));
                }
                ProjField::Nested {
                    alias,
                    shape: inner,
                    ..
                } => {
                    let card = v
                        .body
                        .joins
                        .iter()
                        .find_map(|j| j.result.as_ref().filter(|r| r.name.name == alias.name));
                    let table = v
                        .body
                        .joins
                        .iter()
                        .find(|j| j.result.as_ref().is_some_and(|r| r.name.name == alias.name))
                        .map(|j| j.table.object.name.clone());
                    let fields: Fields = inner
                        .fields
                        .iter()
                        .map(|nf| {
                            let name = proj_name(nf);
                            let ty = table
                                .as_ref()
                                .and_then(|t| s.tables.get(t))
                                .and_then(|t| t.column(&name))
                                .cloned()
                                .unwrap_or(Ty::Unknown);
                            (name, ty)
                        })
                        .collect();
                    let rec = Ty::Record(fields);
                    let ty = match card {
                        Some(r) if r.cardinality == Cardinality::Many => rec.array(),
                        Some(r) if r.cardinality == Cardinality::One => {
                            // `left join ... as one` may not match
                            // (types.md §6.3).
                            let is_left = v
                                .body
                                .joins
                                .iter()
                                .find(|j| {
                                    j.result.as_ref().is_some_and(|x| x.name.name == alias.name)
                                })
                                .map(|j| j.kind == JoinKind::Left)
                                .unwrap_or(true);
                            if is_left {
                                rec.opt()
                            } else {
                                rec
                            }
                        }
                        _ => rec,
                    };
                    shape.push((alias.name.clone(), ty));
                }
            }
        }
    }

    ViewSym {
        declared: v.name.name.clone(),
        schema: v.schema.schema.name.clone(),
        shape,
        inherited,
        driving_table: driving,
        loc,
    }
}

fn proj_name(f: &ProjField) -> String {
    match f {
        ProjField::Column(i) => i.name.clone(),
        ProjField::Expr { alias, .. } | ProjField::Nested { alias, .. } => alias.name.clone(),
    }
}

/// AST type reference -> lattice type.
pub fn type_of(
    t: &TypeRef,
    enums: &BTreeMap<String, EnumSym>,
    classes: &BTreeMap<String, ClassSym>,
) -> Ty {
    let mut base = match &t.kind {
        TypeKind::Scalar { name, .. } => match Scalar::from_name(name) {
            Some(s) => Ty::Scalar(s),
            None => Ty::Unknown,
        },
        TypeKind::Record(fields) => Ty::Record(
            fields
                .iter()
                .map(|(n, ty)| (n.name.clone(), type_of(ty, enums, classes)))
                .collect(),
        ),
        TypeKind::Named(d) => {
            let name = d.text();
            if enums.contains_key(&name) {
                Ty::Enum(name)
            } else if classes.contains_key(&name) {
                Ty::Class(name)
            } else {
                // Resolved later by the checker against the full table; an
                // unknown name is reported there, once.
                Ty::Class(name)
            }
        }
    };
    if t.optional {
        base = base.opt();
    }
    for i in 0..t.array_depth as usize {
        base = base.array();
        if t.array_optional.get(i).copied().unwrap_or(false) {
            base = base.opt();
        }
    }
    base
}

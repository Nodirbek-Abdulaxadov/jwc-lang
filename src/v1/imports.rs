//! The import graph (names.md §6).
//!
//! `import` does not restrict visibility — the declaration space is flat,
//! so a name is reachable whether or not you import it (§6.3.1). It is
//! checked anyway, and that is the whole point: without the check the
//! import list drifts into fiction, and a fictional dependency graph is
//! worse than none because people read it.
//!
//! So this pass answers two questions per file: does every `import` name
//! something that exists, and does every namespace the file *reaches into*
//! appear in its import list.

use super::ast::*;
use super::diag::Diagnostic;
use super::workspace::{Loc, Workspace};
use std::collections::{BTreeMap, BTreeSet};

/// Check a workspace's imports. `packages` are the keys of
/// `jwcproj.json`'s `dependencies`.
pub fn check(ws: &Workspace, packages: &BTreeSet<String>) -> Vec<(Loc, Diagnostic)> {
    let mut out = Vec::new();

    // Namespace per file, and the namespace each declaration lives in.
    let mut namespaces: BTreeSet<String> = BTreeSet::new();
    let mut file_ns: Vec<Option<String>> = Vec::with_capacity(ws.files.len());
    for file in &ws.files {
        let ns = file.program.decls.iter().find_map(|d| match d {
            Decl::Namespace(n) => Some(n.name.text()),
            _ => None,
        });
        if let Some(n) = &ns {
            namespaces.insert(n.clone());
        }
        file_ns.push(ns);
    }

    let mut home: BTreeMap<String, String> = BTreeMap::new();
    for (i, file) in ws.files.iter().enumerate() {
        let Some(ns) = &file_ns[i] else { continue };
        for d in &file.program.decls {
            if let Some(name) = declared_name(d) {
                home.insert(name, ns.clone());
            }
        }
    }

    for (fi, file) in ws.files.iter().enumerate() {
        // §6.1.4 — the namespace matches the path under `src/`. A
        // convention nobody checks is a convention that half the files
        // follow, and then the namespace stops telling you where the file
        // is, which is the only thing it was for.
        if let Some(ns) = &file_ns[fi] {
            if let Some(expected) = namespace_for(&ws.root, &file.source.path) {
                if *ns != expected {
                    let span = file
                        .program
                        .decls
                        .iter()
                        .find_map(|d| match d {
                            Decl::Namespace(n) => Some(n.span),
                            _ => None,
                        })
                        .unwrap_or_default();
                    out.push((
                        Loc { file: fi, span },
                        Diagnostic::warning(
                            "W0102",
                            span,
                            format!("namespace `{ns}` does not match the path (`{expected}`)"),
                        )
                        .clause("names.md §6.1.4"),
                    ));
                }
            }
        }

        let mut imported: BTreeMap<String, Loc> = BTreeMap::new();
        for d in &file.program.decls {
            let Decl::Import(im) = d else { continue };
            let path = im.name.text();
            let loc = Loc {
                file: fi,
                span: im.span,
            };
            let is_ns = namespaces.contains(&path);
            let is_pkg = packages.contains(&path);
            match (is_ns, is_pkg) {
                // §6.2.2 — no precedence rule, deliberately: whichever one
                // the compiler picked would be the one the reader did not.
                (true, true) => out.push((
                    loc,
                    Diagnostic::error(
                        "E0203",
                        im.span,
                        format!("`{}` is both a local namespace and a package", path),
                    )
                    .note("rename one; there is no precedence rule")
                    .clause("names.md §6.2.2"),
                )),
                (false, false) => out.push((
                    loc,
                    Diagnostic::error("E0201", im.span, format!("unknown import `{}`", path))
                        .note(
                            "it is neither a namespace declared in this project nor a key \
                             in `jwcproj.json`'s `dependencies`",
                        )
                        .clause("names.md §6.2.1"),
                )),
                _ => {
                    imported.insert(path.clone(), loc);
                }
            }
        }

        // Every namespace this file reaches into.
        let mut reached: BTreeSet<String> = BTreeSet::new();
        let mut mentions: Vec<(String, Span)> = Vec::new();
        for d in &file.program.decls {
            mentioned(d, &mut mentions);
        }
        let own = file_ns[fi].clone().unwrap_or_default();
        for (name, span) in mentions {
            let Some(ns) = home.get(&name) else { continue };
            if *ns == own {
                continue;
            }
            reached.insert(ns.clone());
            if !imported.contains_key(ns) {
                out.push((
                    Loc { file: fi, span },
                    Diagnostic::error(
                        "E0202",
                        span,
                        format!("`{name}` is declared in `{ns}`, which this file does not import"),
                    )
                    .note(format!("add `import {ns};`"))
                    .clause("names.md §6.3.2"),
                ));
            }
        }

        // §6.3.3 — an import that contributes nothing. A package import is
        // exempt: it brings a builtin namespace in (§6.2.3), and those
        // names are not declarations this pass can see.
        for (path, loc) in &imported {
            if packages.contains(path) || reached.contains(path) {
                continue;
            }
            out.push((
                *loc,
                Diagnostic::warning("W0103", loc.span, format!("unused import `{path}`"))
                    .clause("names.md §6.3.3"),
            ));
        }
    }

    out
}

/// names.md §3 — the case convention, as a warning.
///
/// A convention nobody checks is one half the files follow, and then it
/// stops carrying information: the reader can no longer tell a `class`
/// from a column by looking.
pub fn case_convention(ws: &Workspace) -> Vec<(Loc, Diagnostic)> {
    let mut out = Vec::new();
    for (fi, file) in ws.files.iter().enumerate() {
        for d in &file.program.decls {
            let (name, span, want) = match d {
                Decl::Database(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Table(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::View(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Class(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Enum(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Service(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Middleware(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Error(x) => (&x.name.name, x.name.span, Case::Pascal),
                Decl::Schema(x) => (&x.name.name, x.name.span, Case::Snake),
                Decl::Function(x) => (&x.name.name, x.name.span, Case::Snake),
                _ => continue,
            };
            if !want.holds(name) {
                out.push((
                    Loc { file: fi, span },
                    Diagnostic::warning(
                        "W0101",
                        span,
                        format!("`{name}` is not {}", want.describe()),
                    )
                    .clause("names.md §3"),
                ));
            }
        }
    }
    out
}

#[derive(Clone, Copy)]
enum Case {
    Pascal,
    Snake,
}

impl Case {
    fn holds(self, name: &str) -> bool {
        let first = name.chars().next();
        match self {
            // Not a full spelling check: an underscore or a leading
            // lowercase is what people actually write by accident, and a
            // rule that argues about `OAuth2Token` is a rule people
            // silence.
            Case::Pascal => first.is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('_'),
            Case::Snake => {
                first.is_some_and(|c| c.is_ascii_lowercase())
                    && !name.chars().any(|c| c.is_ascii_uppercase())
            }
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Case::Pascal => "PascalCase (names.md §3.1)",
            Case::Snake => "snake_case (names.md §3.2)",
        }
    }
}

/// The name a declaration introduces into the flat declaration space.
fn declared_name(d: &Decl) -> Option<String> {
    Some(match d {
        Decl::Table(t) => t.name.name.clone(),
        Decl::View(v) => v.name.name.clone(),
        Decl::Enum(e) => e.name.name.clone(),
        Decl::Class(c) => c.name.name.clone(),
        Decl::Error(e) => e.name.name.clone(),
        Decl::Service(s) => s.name.name.clone(),
        // `App.org.Orgs` names the database too, so a file that writes a
        // qualified path depends on wherever `database App` is declared.
        Decl::Database(db) => db.name.name.clone(),
        Decl::Middleware(m) => m.name.name.clone(),
        Decl::Function(f) => f.name.name.clone(),
        _ => return None,
    })
}

use super::token::Span;

/// Names a declaration reaches for, in the positions where a name means a
/// declaration: type annotations, query sources, `use` lists, `requires`,
/// `raises`, thrown and caught errors, and call targets.
///
/// Deliberately not "every identifier in the file": a column named `orgs`
/// is not a reference to a table called `Orgs`, and an over-approximation
/// here would be a spurious *error*.
fn mentioned(d: &Decl, out: &mut Vec<(String, Span)>) {
    match d {
        Decl::Table(t) => {
            for c in &t.columns {
                ty_ref(&c.ty, out);
            }
            for con in &t.constraints {
                if let TableConstraint::ForeignKey { target, .. } = con {
                    qualified(target, out);
                }
            }
        }
        Decl::View(v) => select(&v.body, out),
        Decl::Class(c) => {
            for f in &c.fields {
                ty_ref(&f.ty, out);
            }
        }
        Decl::Enum(_) | Decl::Error(_) => {}
        Decl::Service(s) => {
            for f in &s.functions {
                function(f, out);
            }
        }
        Decl::Function(f) => function(f, out),
        Decl::Middleware(m) => {
            for r in &m.requires {
                out.push((r.name.clone(), r.span));
            }
            block(&m.body, out);
            if let Some(a) = &m.after {
                block(a, out);
            }
        }
        Decl::Routes(r) => {
            for u in &r.uses {
                out.push((u.name.clone(), u.span));
            }
            for route in &r.routes {
                for u in &route.uses {
                    out.push((u.name.clone(), u.span));
                }
                block(&route.body, out);
            }
        }
        Decl::ErrorHandler(h) => {
            for arm in &h.arms {
                if let Some(e) = &arm.error {
                    out.push((e.name.clone(), e.span));
                }
                block(&arm.body, out);
            }
        }
        Decl::Test(t) => block(&t.body, out),
        _ => {}
    }
}

fn function(f: &FunctionDecl, out: &mut Vec<(String, Span)>) {
    for p in &f.params {
        ty_ref(&p.ty, out);
    }
    if let Some(r) = &f.returns {
        ty_ref(r, out);
    }
    for r in &f.raises {
        out.push((r.name.clone(), r.span));
    }
    block(&f.body, out);
}

fn ty_ref(t: &TypeRef, out: &mut Vec<(String, Span)>) {
    match &t.kind {
        // A scalar is a keyword, not a declaration.
        TypeKind::Scalar { .. } => {}
        TypeKind::Named(n) => out.push((n.text(), n.span)),
        TypeKind::Record(fields) => {
            for (_, ft) in fields {
                ty_ref(ft, out);
            }
        }
    }
}

fn block(b: &Block, out: &mut Vec<(String, Span)>) {
    for s in b {
        match s {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    ty_ref(t, out);
                }
                expr(value, out);
            }
            Stmt::Assign { value, .. } | Stmt::Expr { expr: value, .. } => expr(value, out),
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                expr(cond, out);
                block(then, out);
                for alt in otherwise.iter() {
                    block(alt, out);
                }
            }
            Stmt::For { iterable, body, .. } => {
                expr(iterable, out);
                block(body, out);
            }
            Stmt::Return { value, .. } => {
                for v in value.iter() {
                    expr(v, out);
                }
            }
            Stmt::Throw { error, args, .. } => {
                out.push((error.name.clone(), error.span));
                for a in args {
                    expr(a, out);
                }
            }
            Stmt::Transaction { body, .. } => block(body, out),
            Stmt::Assert { kind, .. } => match kind {
                AssertKind::Expr(e) => expr(e, out),
                AssertKind::Fails { error, body } => {
                    for e in error.iter() {
                        out.push((e.name.clone(), e.span));
                    }
                    block(body, out);
                }
            },
        }
    }
}

fn expr(e: &Expr, out: &mut Vec<(String, Span)>) {
    match &*e.kind {
        // `Service.method(...)`, `EnumType.member`, `Class` in a cast.
        ExprKind::Field { base, .. } => {
            if let ExprKind::Name(n) = &*base.kind {
                out.push((n.name.clone(), n.span));
            }
            expr(base, out);
        }
        ExprKind::Cast { value, ty } => {
            out.push((ty.name.clone(), ty.span));
            expr(value, out);
        }
        ExprKind::Call { callee, args, filter } => {
            // A bare call is a free function.
            if let ExprKind::Name(n) = &*callee.kind {
                out.push((n.name.clone(), n.span));
            }
            expr(callee, out);
            for a in args {
                expr(a, out);
            }
            for f in filter.iter() {
                expr(f, out);
            }
        }
        ExprKind::Select(s) => select(s, out),
        ExprKind::Insert(i) => {
            qualified(&i.table, out);
            for v in &i.values {
                entry(v, out);
            }
        }
        ExprKind::Update(u) => {
            qualified(&u.table, out);
            for s in &u.sets {
                if let SetItem::Set { value, .. } = s {
                    expr(value, out);
                }
            }
            for f in u.filter.iter() {
                expr(f, out);
            }
        }
        ExprKind::Delete(d) => {
            qualified(&d.table, out);
            for f in d.filter.iter() {
                expr(f, out);
            }
        }
        ExprKind::OrThrow { value, error, args } => {
            expr(value, out);
            out.push((error.name.clone(), error.span));
            for a in args {
                expr(a, out);
            }
        }
        ExprKind::CatchPostfix {
            value,
            error,
            body,
            ..
        } => {
            expr(value, out);
            out.push((error.name.clone(), error.span));
            block(body, out);
        }
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            expr(lhs, out);
            expr(rhs, out);
        }
        ExprKind::Unary { rhs, .. } => expr(rhs, out),
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => {
            expr(cond, out);
            expr(then, out);
            expr(otherwise, out);
        }
        ExprKind::In { lhs, items, .. } => {
            expr(lhs, out);
            for i in items {
                expr(i, out);
            }
        }
        ExprKind::Exists { query, .. } => expr(query, out),
        ExprKind::Index { base, index } => {
            expr(base, out);
            expr(index, out);
        }
        ExprKind::Object(entries) => {
            for en in entries {
                entry(en, out);
            }
        }
        ExprKind::Array(items) => {
            for i in items {
                expr(i, out);
            }
        }
        ExprKind::WithHeaders { value, headers } => {
            expr(value, out);
            for h in headers {
                entry(h, out);
            }
        }
        ExprKind::Cookie { value, args } => {
            expr(value, out);
            for a in args {
                expr(a, out);
            }
        }
        _ => {}
    }
}

fn entry(en: &ObjEntry, out: &mut Vec<(String, Span)>) {
    if let ObjEntry::Field { value, .. } = en {
        expr(value, out);
    }
}

fn qualified(q: &QualifiedTable, out: &mut Vec<(String, Span)>) {
    out.push((q.object.name.clone(), q.span));
    out.push((q.database.name.clone(), q.database.span));
}

fn select(s: &SelectExpr, out: &mut Vec<(String, Span)>) {
    qualified(&s.source, out);
    for j in &s.joins {
        qualified(&j.table, out);
        expr(&j.on, out);
        for f in j.filter.iter() {
            expr(f, out);
        }
    }
    for f in s.filter.iter() {
        expr(f, out);
    }
    for h in s.having.iter() {
        expr(h, out);
    }
    if let Some(p) = &s.projection {
        for f in &p.fields {
            if let ProjField::Expr { value, .. } = f {
                expr(value, out);
            }
        }
    }
}

/// The namespace a file's path implies: its location under `src/`, with
/// `/` as `.` and the extension dropped.
///
/// `None` when the file is not under a `src/` directory — a single-file
/// project has no path to match against, and inventing one would warn on
/// every scratch file.
fn namespace_for(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let first = parts.first()?.clone();
    if first != "src" {
        return None;
    }
    parts.remove(0);
    let last = parts.last_mut()?;
    *last = last.strip_suffix(".jwc")?.to_string();
    Some(parts.join("."))
}

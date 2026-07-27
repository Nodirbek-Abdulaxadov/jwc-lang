//! Native AOT compilation: `.jwc` → Rust source → `cargo build` → native binary.
//!
//! **Scope is specified in `docs/spec/aot-scope.md`** — that file is the
//! source of truth for what the native build supports vs. what it
//! deliberately defers to `jwc run`. One-line summary: native AOT covers
//! the stateless route tier (routes, helpers, value/response builders,
//! simple `select` / `update` / `insert`, cache, sleep, http_get, jwt,
//! hash). Stateful work — `savepoint` boundaries, the persistent
//! Postgres-backed job queue, OTLP traces — still runs under the
//! interpreter.
//!
//! Package system: namespaces, `import`, and `mount`/`group` are honoured by
//! `flatten_namespaces` before any codegen — function calls are resolved to
//! their target FQN, library routes expanded per mount (prefix + middlewares
//! applied), inactive ones dropped. Visibility (`public`/`private`) is
//! enforced statically by `parser::validate::check_visibility`
//! (`src/parser/validate.rs`, raised as `error[E021]`) — `validate_program`
//! is the single source of truth and AOT codegen trusts it. See
//! `docs/spec/visibility.md` for the public/private surface and the proof
//! that the validator covers every cross-namespace edge the AOT lowers
//! (function call, route handler ref, middleware body call). The
//! interpreter still has its own runtime check
//! (`runner::Vm::check_visibility`) as belt-and-braces for hand-built
//! Programs that bypass the parser; the AOT path does not need it because
//! every emission goes through `validate_program` first.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::ast::{
    AggregateKind, ConstDecl, Expr, FunctionDecl, ImportDecl, ModelKind, MountDecl, Program,
    RouteDecl, Stmt,
};

/// Compute the FQN form a decl will be emitted under in generated Rust:
/// lowercase, dot-joined, e.g. `"math.utils.add"`. Root decls (empty
/// namespace) keep their bare name. `user_fn_name` later sanitises `.` to `_`
/// so the resulting Rust identifier is valid.
fn ns_fqn(ns: &[String], name: &str) -> String {
    if ns.is_empty() {
        name.to_lowercase()
    } else {
        format!(
            "{}.{}",
            ns.iter()
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
                .join("."),
            name.to_lowercase()
        )
    }
}

/// Apply one mount to a library route: prepend prefix, prepend middlewares.
/// Same shape as the runner's helper so the interpreter and native binary
/// produce identical effective route tables.
fn apply_mount_to_route(route: &RouteDecl, mount: &MountDecl) -> RouteDecl {
    let mut copy = route.clone();
    if let Some(prefix) = &mount.prefix {
        copy.path = format!("{}{}", prefix, copy.path);
    }
    let mut mws = mount.middlewares.clone();
    mws.extend(route.middlewares.iter().cloned());
    copy.middlewares = mws;
    copy
}

/// Walk `program` and emit a new Program where every Decl `name` and every
/// Expr::Call `name` is rewritten into its fully-qualified form. After this
/// pass the rest of native_build can treat the AST as flat — no namespace
/// awareness needed downstream.
fn flatten_namespaces(program: &Program) -> Program {
    // Index library routes by their namespace so each `mount` can find them
    // when expanding. Same shape as `runner::expand_routes`.
    let mut routes_by_ns: HashMap<String, Vec<&RouteDecl>> = HashMap::new();
    for r in &program.routes {
        let key = r
            .namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        routes_by_ns.entry(key).or_default().push(r);
    }

    // Dbcontexts always come along when their declaring namespace is
    // referenced anywhere via `mount` or `import`. Compute that set here so
    // we don't drop the dbcontext when the project depends on its entities.
    let mut active_dbctx_ns: HashSet<String> = HashSet::new();
    active_dbctx_ns.insert(String::new());
    for m in &program.mounts {
        active_dbctx_ns.insert(
            m.target
                .iter()
                .map(|x| x.to_lowercase())
                .collect::<Vec<_>>()
                .join("."),
        );
    }
    for imp in &program.imports {
        active_dbctx_ns.insert(
            imp.path
                .iter()
                .map(|x| x.to_lowercase())
                .collect::<Vec<_>>()
                .join("."),
        );
    }

    // Build the lookup tables the resolver needs:
    //   short_name → list of FQNs declaring that name (any namespace)
    //   imports_by_ns → file-scoped `import` declarations per namespace
    let mut fn_short_to_fqns: HashMap<String, Vec<String>> = HashMap::new();
    let mut mw_short_to_fqns: HashMap<String, Vec<String>> = HashMap::new();
    let mut fqn_set: HashSet<String> = HashSet::new();
    for f in &program.functions {
        let fqn = ns_fqn(&f.namespace, &f.name);
        fn_short_to_fqns
            .entry(f.name.to_lowercase())
            .or_default()
            .push(fqn.clone());
        fqn_set.insert(fqn);
    }
    for m in &program.middlewares {
        let fqn = ns_fqn(&m.namespace, &m.name);
        mw_short_to_fqns
            .entry(m.name.to_lowercase())
            .or_default()
            .push(fqn.clone());
        fqn_set.insert(fqn);
    }

    let mut imports_by_ns: HashMap<String, Vec<&ImportDecl>> = HashMap::new();
    for imp in &program.imports {
        let key = imp
            .in_namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        imports_by_ns.entry(key).or_default().push(imp);
    }

    let resolver = NameResolver {
        fn_short_to_fqns: &fn_short_to_fqns,
        mw_short_to_fqns: &mw_short_to_fqns,
        fqn_set: &fqn_set,
        imports_by_ns: &imports_by_ns,
    };

    let mut out = Program::default();

    // Functions: rename to FQN, walk body to resolve calls in caller's namespace.
    for f in &program.functions {
        let mut nf = f.clone();
        nf.name = ns_fqn(&f.namespace, &f.name);
        resolver.rewrite_stmts(&mut nf.body, &f.namespace);
        nf.namespace = Vec::new();
        out.functions.push(nf);
    }

    // Middlewares: same.
    for m in &program.middlewares {
        let mut nm = m.clone();
        nm.name = ns_fqn(&m.namespace, &m.name);
        resolver.rewrite_stmts(&mut nm.body, &m.namespace);
        nm.namespace = Vec::new();
        out.middlewares.push(nm);
    }

    // Helper to lower-and-resolve a single source route into the output set.
    let emit_route = |out: &mut Program, r: &RouteDecl| {
        let mut nr = r.clone();
        nr.middlewares = nr
            .middlewares
            .iter()
            .map(|m| resolver.resolve_mw(m, &r.namespace))
            .collect();
        if let Some(h) = &nr.handler {
            nr.handler = Some(resolver.resolve_fn(h, &r.namespace));
        }
        resolver.rewrite_stmts(&mut nr.body, &r.namespace);
        nr.namespace = Vec::new();
        out.routes.push(nr);
    };

    // Root namespace routes — always active, emit as-is.
    if let Some(roots) = routes_by_ns.get("") {
        for r in roots {
            emit_route(&mut out, r);
        }
    }

    // Library routes — emit one copy per mount, with prefix and inherited
    // middleware chain applied (same as runner::expand_routes).
    for mount in &program.mounts {
        let key = mount
            .target
            .iter()
            .map(|x| x.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        if let Some(lib_routes) = routes_by_ns.get(&key) {
            for r in lib_routes {
                let mounted = apply_mount_to_route(r, mount);
                emit_route(&mut out, &mounted);
            }
        }
    }

    // DbContexts come along when their namespace is mounted OR imported.
    for c in &program.dbcontexts {
        let key = c
            .namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        if !active_dbctx_ns.contains(&key) {
            continue;
        }
        let mut nc = c.clone();
        nc.namespace = Vec::new();
        out.dbcontexts.push(nc);
    }

    // Models keep their unqualified name — entity/class lookup in native_build
    // is still by simple name. Two models with the same short name across
    // namespaces would already fail validate_program, so a collision here
    // can't slip through.
    out.models = program.models.clone();
    for m in &mut out.models {
        m.namespace = Vec::new();
    }

    out.error_handler = program.error_handler.clone();
    // Module-level consts carry over verbatim — they are namespace-agnostic and
    // already validated to be pure constant expressions.
    out.consts = program.consts.clone();
    // imports / mounts were consumed by this pass — drop them.
    out
}

struct NameResolver<'a> {
    fn_short_to_fqns: &'a HashMap<String, Vec<String>>,
    mw_short_to_fqns: &'a HashMap<String, Vec<String>>,
    fqn_set: &'a HashSet<String>,
    imports_by_ns: &'a HashMap<String, Vec<&'a ImportDecl>>,
}

impl<'a> NameResolver<'a> {
    /// Resolve a function reference written inside namespace `caller_ns` to
    /// a concrete FQN. Falls back to the original name if no user function
    /// matches (builtin or unknown — codegen will reject the latter).
    fn resolve_fn(&self, name: &str, caller_ns: &[String]) -> String {
        let lower = name.to_lowercase();
        // 1. Already a known FQN.
        if self.fqn_set.contains(&lower) {
            return lower;
        }
        if !lower.contains('.') {
            // 2. caller_ns.name
            if !caller_ns.is_empty() {
                let key = ns_fqn(caller_ns, name);
                if self.fqn_set.contains(&key) {
                    return key;
                }
            }
            // 3. imports active in caller_ns.
            let ns_key = caller_ns
                .iter()
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(imps) = self.imports_by_ns.get(&ns_key) {
                for imp in imps {
                    let key = ns_fqn(&imp.path, name);
                    if self.fqn_set.contains(&key) {
                        return key;
                    }
                }
            }
            // 4. Root.
            if let Some(fqns) = self.fn_short_to_fqns.get(&lower) {
                if let Some(root_fqn) = fqns.iter().find(|f| !f.contains('.')) {
                    return root_fqn.clone();
                }
                // 5. Any single unique match.
                if fqns.len() == 1 {
                    return fqns[0].clone();
                }
            }
        }
        // Unresolved — leave as-is. Could be a builtin or an error to surface
        // later in codegen.
        lower
    }

    fn resolve_mw(&self, name: &str, caller_ns: &[String]) -> String {
        let lower = name.to_lowercase();
        if !lower.contains('.') {
            if !caller_ns.is_empty() {
                let key = ns_fqn(caller_ns, name);
                if self.mw_short_to_fqns.values().flatten().any(|f| f == &key) {
                    return key;
                }
            }
            if let Some(fqns) = self.mw_short_to_fqns.get(&lower) {
                if let Some(root_fqn) = fqns.iter().find(|f| !f.contains('.')) {
                    return root_fqn.clone();
                }
                if fqns.len() == 1 {
                    return fqns[0].clone();
                }
            }
        }
        lower
    }

    fn rewrite_stmts(&self, stmts: &mut [Stmt], caller_ns: &[String]) {
        for s in stmts {
            self.rewrite_stmt(s, caller_ns);
        }
    }

    fn rewrite_stmt(&self, s: &mut Stmt, caller_ns: &[String]) {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value)
            | Stmt::Expr(value) => self.rewrite_expr(value, caller_ns),
            Stmt::Return(Some(v)) => self.rewrite_expr(v, caller_ns),
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                self.rewrite_expr(cond, caller_ns);
                self.rewrite_stmts(then_body, caller_ns);
                if let Some(eb) = else_body {
                    self.rewrite_stmts(eb, caller_ns);
                }
            }
            Stmt::While { cond, body } => {
                self.rewrite_expr(cond, caller_ns);
                self.rewrite_stmts(body, caller_ns);
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                self.rewrite_stmts(body, caller_ns);
                self.rewrite_stmts(catch_body, caller_ns);
            }
            Stmt::Transaction { body } => self.rewrite_stmts(body, caller_ns),
            Stmt::ForIn { iter, body, .. } => {
                self.rewrite_expr(iter, caller_ns);
                self.rewrite_stmts(body, caller_ns);
            }
            _ => {}
        }
    }

    fn rewrite_expr(&self, e: &mut Expr, caller_ns: &[String]) {
        match e {
            Expr::Call { name, args } => {
                let lower = name.to_lowercase();
                let resolved = self.resolve_fn(&lower, caller_ns);
                // Rewrite to the resolved FQN whenever it names a user function
                // (always lowercased by `ns_fqn`, so a camelCase source name
                // like `byStatus` must be swapped to its `bystatus` FQN to match
                // the renamed decl). Builtins resolve to their own lowercased
                // name but aren't in `fqn_set`, so their original casing — which
                // `builtin_fn_name` depends on — is left intact.
                if self.fqn_set.contains(&resolved) {
                    *name = resolved;
                }
                for a in args {
                    self.rewrite_expr(a, caller_ns);
                }
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                self.rewrite_expr(cond, caller_ns);
                self.rewrite_expr(then_expr, caller_ns);
                self.rewrite_expr(else_expr, caller_ns);
            }
            Expr::Coalesce(a, b) => {
                self.rewrite_expr(a, caller_ns);
                self.rewrite_expr(b, caller_ns);
            }
            Expr::Await(inner) | Expr::Not(inner) | Expr::Neg(inner) => {
                self.rewrite_expr(inner, caller_ns)
            }
            Expr::Add(a, b)
            | Expr::Sub(a, b)
            | Expr::Mul(a, b)
            | Expr::Div(a, b)
            | Expr::Mod(a, b)
            | Expr::Eq(a, b)
            | Expr::Neq(a, b)
            | Expr::Lt(a, b)
            | Expr::Lte(a, b)
            | Expr::Gt(a, b)
            | Expr::Gte(a, b)
            | Expr::And(a, b)
            | Expr::Or(a, b) => {
                self.rewrite_expr(a, caller_ns);
                self.rewrite_expr(b, caller_ns);
            }
            Expr::ObjectLit(pairs) => {
                for (_, v) in pairs {
                    self.rewrite_expr(v, caller_ns);
                }
            }
            Expr::ArrayLit(items) => {
                for item in items {
                    self.rewrite_expr(item, caller_ns);
                }
            }
            Expr::DbSelect {
                where_clause,
                limit,
                offset,
                ..
            } => {
                if let Some(wc) = where_clause {
                    self.rewrite_where(wc, caller_ns);
                }
                if let Some(l) = limit {
                    self.rewrite_expr(l, caller_ns);
                }
                if let Some(o) = offset {
                    self.rewrite_expr(o, caller_ns);
                }
            }
            Expr::DbCount { where_clause, .. } | Expr::DbAggregate { where_clause, .. } => {
                if let Some(wc) = where_clause {
                    self.rewrite_where(wc, caller_ns);
                }
            }
            _ => {}
        }
    }

    fn rewrite_where(&self, wc: &mut crate::ast::WhereExpr, caller_ns: &[String]) {
        use crate::ast::WhereExpr;
        match wc {
            WhereExpr::Atom(a) => self.rewrite_expr(&mut a.rhs, caller_ns),
            WhereExpr::InList { values, .. } => {
                for v in values {
                    self.rewrite_expr(v, caller_ns);
                }
            }
            WhereExpr::Between { low, high, .. } => {
                self.rewrite_expr(low, caller_ns);
                self.rewrite_expr(high, caller_ns);
            }
            WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
                self.rewrite_where(l, caller_ns);
                self.rewrite_where(r, caller_ns);
            }
        }
    }
}

const BUILD_DIR_NAME: &str = ".jwc-build";

/// Built-in JWC functions emitted as inlined helpers in the generated Rust
/// prelude. Calls to anything outside this set or the user's own functions
/// produce an "unsupported" error at codegen.
// Built-in metadata now lives in `src/builtins.rs` so the lint pass,
// validator, and any future tooling can share the same source of truth
// without depending on this codegen-heavy module. The native AOT whitelist
// is `native_builtin_names()` (name + aliases of every `native: true` def)
// combined with `SPECIAL_BUILTINS`.
pub use crate::builtins::{native_builtin_names, SPECIAL_BUILTINS};

/// Suggest the closest entry in `candidates` to `target` via Levenshtein
/// distance. Returns `None` when no candidate is within max(2, len/3) —
/// the same noise floor `runner::closest_match` and
/// `parser::suggest_column` use, so all three suggestion paths surface
/// hits with consistent strictness.
fn codegen_closest_match(target: &str, candidates: &[String]) -> Option<String> {
    let target_lc = target.to_ascii_lowercase();
    let threshold = std::cmp::max(2, target_lc.chars().count() / 3);
    let mut best: Option<(usize, &String)> = None;
    for c in candidates {
        if c.eq_ignore_ascii_case(target) {
            continue;
        }
        let d = codegen_levenshtein(&target_lc, &c.to_ascii_lowercase());
        if d > threshold {
            continue;
        }
        match best {
            Some((prev, _)) if prev <= d => {}
            _ => best = Some((d, c)),
        }
    }
    best.map(|(_, s)| s.clone())
}

fn codegen_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Codegen-time metadata for a DB-bound entity. Captures the column list and
/// each column's Postgres-target type so INSERT param boxing can pick the
/// right `ToSql` impl.
#[derive(Clone)]
struct EntityMeta {
    table: String,
    fields: Vec<EntityField>,
}

#[derive(Clone)]
struct EntityField {
    name: String,
    pg: PgKind,
    is_auto_increment: bool,
    is_primary_key: bool,
    /// `true` when the column was declared `nullable` in the entity. Used
    /// by struct monomorphization (Phase 1 spike) to decide whether the
    /// generated Rust field is `T` or `Option<T>`.
    is_nullable: bool,
}

/// Coarse-grained Postgres type bucket. JWC types collapse onto these because
/// the codegen only needs to pick a `jwc_param_*` boxing helper, not emit DDL.
///
/// Integer widths are distinguished because tokio-postgres' `ToSql` impls are
/// width-specific: `i32` only binds to `INT2`/`INT4`, `i64` only to `INT8`.
/// Boxing the wrong width raises `WrongType { postgres: Int4, rust: "i64" }`
/// at execute time (the exact panic the AOT path used to hit on every
/// `int` column).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PgKind {
    Smallint, // int2 / smallint -> i16
    Int,      // int / int4 / integer -> i32
    Bigint,   // int8 / bigint -> i64
    Float,    // double / float / real -> f64
    /// numeric / decimal -> `rust_decimal::Decimal`. Distinct from `Float`
    /// because tokio-postgres has no f64 <-> NUMERIC conversion in either
    /// direction: binding one raises `WrongType`, and reading one used to
    /// fall through to `V::Null`, losing the value without an error.
    Numeric,
    Bool,
    /// datetime / timestamp / timestamptz / date — carried as text by the VM
    /// but bound through a `$N::timestamptz` placeholder so Postgres casts the
    /// TEXT value to the column's actual temporal type. Plain `$N` binding
    /// trips `error serializing parameter N` because tokio-postgres' ToSql
    /// impl for `String` reports `WrongType { postgres: Timestamptz, rust: "&str" }`.
    Timestamp,
    Str, // string / varchar / text / uuid — all carried as text
}

fn pg_kind_for(type_name: &str) -> PgKind {
    let t = type_name.to_lowercase();
    match t.as_str() {
        "int2" | "smallint" => PgKind::Smallint,
        "int" | "int4" | "integer" => PgKind::Int,
        "int8" | "bigint" => PgKind::Bigint,
        // `numeric` is NOT a float: binding f64 to a NUMERIC column is
        // rejected by tokio-postgres, and reading one back matched no arm at
        // all, so decimal columns silently came out as null.
        "decimal" | "numeric" => PgKind::Numeric,
        "double" | "float" | "float4" | "float8" | "real" => PgKind::Float,
        "bool" | "boolean" => PgKind::Bool,
        // Temporal types — every JWC `datetime` lowers to `timestamptz`; we
        // keep the older `timestamp`/`date` aliases for hand-written entities.
        "datetime"
        | "timestamp"
        | "timestamptz"
        | "date"
        | "timestamp with time zone"
        | "timestamp without time zone" => PgKind::Timestamp,
        _ => PgKind::Str,
    }
}

pub struct CompileReport {
    pub binary_path: PathBuf,
    pub workspace: PathBuf,
}

/// Scan the AST to decide whether the generated Cargo.toml needs `reqwest`.
/// True when any block contains a call to `sleep_ms` / `http_get` / `fetch_json`.
fn program_uses_http_client(program: &Program) -> bool {
    fn walk_expr(e: &Expr) -> bool {
        match e {
            Expr::Call { name, args } => {
                if matches!(name.as_str(), "sleep_ms" | "http_get" | "fetch_json") {
                    return true;
                }
                args.iter().any(walk_expr)
            }
            Expr::Await(inner) | Expr::Not(inner) | Expr::Neg(inner) => walk_expr(inner),
            Expr::Add(a, b)
            | Expr::Sub(a, b)
            | Expr::Mul(a, b)
            | Expr::Div(a, b)
            | Expr::Mod(a, b)
            | Expr::Eq(a, b)
            | Expr::Neq(a, b)
            | Expr::Lt(a, b)
            | Expr::Lte(a, b)
            | Expr::Gt(a, b)
            | Expr::Gte(a, b)
            | Expr::And(a, b)
            | Expr::Or(a, b) => walk_expr(a) || walk_expr(b),
            Expr::ObjectLit(pairs) => pairs.iter().any(|(_, v)| walk_expr(v)),
            Expr::ArrayLit(items) => items.iter().any(walk_expr),
            Expr::DbSelect {
                where_clause,
                limit,
                offset,
                ..
            } => {
                where_clause.as_deref().map(walk_where).unwrap_or(false)
                    || limit.as_deref().map(walk_expr).unwrap_or(false)
                    || offset.as_deref().map(walk_expr).unwrap_or(false)
            }
            Expr::DbCount { where_clause, .. } | Expr::DbAggregate { where_clause, .. } => {
                where_clause.as_deref().map(walk_where).unwrap_or(false)
            }
            _ => false,
        }
    }
    fn walk_where(w: &crate::ast::WhereExpr) -> bool {
        use crate::ast::WhereExpr;
        match w {
            WhereExpr::Atom(a) => walk_expr(&a.rhs),
            WhereExpr::And(l, r) | WhereExpr::Or(l, r) => walk_where(l) || walk_where(r),
            WhereExpr::InList { values, .. } => values.iter().any(walk_expr),
            WhereExpr::Between { low, high, .. } => walk_expr(low) || walk_expr(high),
        }
    }
    fn walk_stmt(s: &Stmt) -> bool {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value)
            | Stmt::Expr(value) => walk_expr(value),
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                walk_expr(cond)
                    || then_body.iter().any(walk_stmt)
                    || else_body
                        .as_ref()
                        .map(|b| b.iter().any(walk_stmt))
                        .unwrap_or(false)
            }
            Stmt::While { cond, body } => walk_expr(cond) || body.iter().any(walk_stmt),
            Stmt::ForIn { iter, body, .. } => walk_expr(iter) || body.iter().any(walk_stmt),
            Stmt::Return(Some(e)) => walk_expr(e),
            Stmt::Try {
                body, catch_body, ..
            } => body.iter().any(walk_stmt) || catch_body.iter().any(walk_stmt),
            Stmt::Transaction { body } => body.iter().any(walk_stmt),
            Stmt::DbDeleteWhere { where_clause, .. } => walk_where(where_clause),
            Stmt::DbUpdateSet {
                assignments,
                where_clause,
                ..
            } => assignments.iter().any(|(_, e)| walk_expr(e)) || walk_where(where_clause),
            _ => false,
        }
    }
    for f in &program.functions {
        if f.body.iter().any(walk_stmt) {
            return true;
        }
    }
    for r in &program.routes {
        if r.body.iter().any(walk_stmt) {
            return true;
        }
    }
    for m in &program.middlewares {
        if m.body.iter().any(walk_stmt) {
            return true;
        }
    }
    if let Some(eh) = &program.error_handler {
        if eh.body.iter().any(walk_stmt) {
            return true;
        }
    }
    false
}

/// Scan the AST to decide whether the generated Cargo.toml needs the crypto
/// crates (`sha2`/`sha1`/`md-5`/`hmac`/`argon2`) and whether to concatenate
/// `native_prelude_crypto.rs.in`. True when any call names a hash built-in.
fn program_uses_crypto(program: &Program) -> bool {
    fn walk_expr(e: &Expr) -> bool {
        match e {
            Expr::Call { name, args } => {
                if matches!(
                    name.as_str(),
                    "sha256"
                        | "sha1"
                        | "md5"
                        | "hmac_sha256"
                        | "hash_password"
                        | "verify_password"
                        | "jwt_sign"
                        | "jwt_verify"
                ) {
                    return true;
                }
                args.iter().any(walk_expr)
            }
            Expr::Await(inner) | Expr::Not(inner) | Expr::Neg(inner) => walk_expr(inner),
            Expr::Add(a, b)
            | Expr::Sub(a, b)
            | Expr::Mul(a, b)
            | Expr::Div(a, b)
            | Expr::Mod(a, b)
            | Expr::Eq(a, b)
            | Expr::Neq(a, b)
            | Expr::Lt(a, b)
            | Expr::Lte(a, b)
            | Expr::Gt(a, b)
            | Expr::Gte(a, b)
            | Expr::And(a, b)
            | Expr::Or(a, b) => walk_expr(a) || walk_expr(b),
            Expr::ObjectLit(pairs) => pairs.iter().any(|(_, v)| walk_expr(v)),
            Expr::ArrayLit(items) => items.iter().any(walk_expr),
            Expr::DbSelect {
                where_clause,
                limit,
                offset,
                ..
            } => {
                where_clause.as_deref().map(walk_where).unwrap_or(false)
                    || limit.as_deref().map(walk_expr).unwrap_or(false)
                    || offset.as_deref().map(walk_expr).unwrap_or(false)
            }
            Expr::DbCount { where_clause, .. } | Expr::DbAggregate { where_clause, .. } => {
                where_clause.as_deref().map(walk_where).unwrap_or(false)
            }
            _ => false,
        }
    }
    fn walk_where(w: &crate::ast::WhereExpr) -> bool {
        use crate::ast::WhereExpr;
        match w {
            WhereExpr::Atom(a) => walk_expr(&a.rhs),
            WhereExpr::And(l, r) | WhereExpr::Or(l, r) => walk_where(l) || walk_where(r),
            WhereExpr::InList { values, .. } => values.iter().any(walk_expr),
            WhereExpr::Between { low, high, .. } => walk_expr(low) || walk_expr(high),
        }
    }
    fn walk_stmt(s: &Stmt) -> bool {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value)
            | Stmt::Expr(value) => walk_expr(value),
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                walk_expr(cond)
                    || then_body.iter().any(walk_stmt)
                    || else_body
                        .as_ref()
                        .map(|b| b.iter().any(walk_stmt))
                        .unwrap_or(false)
            }
            Stmt::While { cond, body } => walk_expr(cond) || body.iter().any(walk_stmt),
            Stmt::ForIn { iter, body, .. } => walk_expr(iter) || body.iter().any(walk_stmt),
            Stmt::Return(Some(e)) => walk_expr(e),
            Stmt::Try {
                body, catch_body, ..
            } => body.iter().any(walk_stmt) || catch_body.iter().any(walk_stmt),
            Stmt::Transaction { body } => body.iter().any(walk_stmt),
            Stmt::DbDeleteWhere { where_clause, .. } => walk_where(where_clause),
            Stmt::DbUpdateSet {
                assignments,
                where_clause,
                ..
            } => assignments.iter().any(|(_, e)| walk_expr(e)) || walk_where(where_clause),
            _ => false,
        }
    }
    for f in &program.functions {
        if f.body.iter().any(walk_stmt) {
            return true;
        }
    }
    for r in &program.routes {
        if r.body.iter().any(walk_stmt) {
            return true;
        }
    }
    for m in &program.middlewares {
        if m.body.iter().any(walk_stmt) {
            return true;
        }
    }
    if let Some(eh) = &program.error_handler {
        if eh.body.iter().any(walk_stmt) {
            return true;
        }
    }
    false
}

pub fn compile(
    program: &Program,
    root: &Path,
    app_name: &str,
    release: bool,
) -> Result<CompileReport> {
    compile_with_target(program, root, app_name, release, None)
}

/// Same as [`compile`] but lets the caller cross-compile to a Rust target
/// triple (e.g. `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`).
/// `None` keeps the host behaviour. The host's installed rustup toolchain
/// must already provide the target; if cargo can't find the target it
/// bails with cargo's own error.
///
/// We don't try to install missing targets ourselves. The caller is
/// expected to have run `rustup target add <triple>` (and, for the
/// `x86_64-unknown-linux-musl` triple, to have the musl C toolchain on
/// PATH) before invoking this. v1 only changes the cargo invocation, not
/// the generated Cargo.toml — see `render_cargo_toml` for the TODO on
/// per-target feature toggling (rustls vs native-tls).
pub fn compile_with_target(
    program: &Program,
    root: &Path,
    app_name: &str,
    release: bool,
    target: Option<&str>,
) -> Result<CompileReport> {
    // Flatten namespaces / imports / mounts before any other pass so every
    // downstream step (reject_unsupported, codegen) sees a single
    // root-namespace Program with FQN-keyed names and expanded route copies.
    let flat = flatten_namespaces(program);
    let program = &flat;

    reject_unsupported(program)?;

    let cargo = find_cargo().context(
        "`cargo` not found.\n\
         Native build requires a Rust toolchain on PATH. Install via https://rustup.rs/\n\
         or run `jwc build` (without --native) for the interpreter-bundled launcher.",
    )?;

    let needs_db = !program.dbcontexts.is_empty() || !program.models.is_empty();
    let needs_http_client = program_uses_http_client(program);
    let needs_crypto = program_uses_crypto(program);
    let rust_src = codegen(program, needs_db, needs_crypto)?;
    let workspace = scaffold_workspace(
        root,
        app_name,
        &rust_src,
        needs_db,
        needs_http_client,
        needs_crypto,
    )?;
    let bin = invoke_cargo(&cargo, &workspace, app_name, release, target)?;
    let final_path = copy_to_project_bin(root, &bin, release, target)?;

    Ok(CompileReport {
        binary_path: final_path,
        workspace,
    })
}

/// Run codegen only and write the generated Rust source into the project's
/// `bin/<profile>/<app>.generated.rs`. Skips cargo entirely. Lets users
/// inspect / debug exactly what the native pipeline would compile.
pub fn emit_rust_source(
    program: &Program,
    root: &Path,
    app_name: &str,
    release: bool,
) -> Result<PathBuf> {
    let flat = flatten_namespaces(program);
    let program = &flat;

    reject_unsupported(program)?;

    let needs_db = !program.dbcontexts.is_empty() || !program.models.is_empty();
    let needs_crypto = program_uses_crypto(program);
    let rust_src = codegen(program, needs_db, needs_crypto)?;

    let profile = if release { "release" } else { "debug" };
    let out_dir = root.join("bin").join(profile);
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create bin/{profile} dir"))?;
    let out_path = out_dir.join(format!("{app_name}.generated.rs"));
    std::fs::write(&out_path, rust_src).with_context(|| format!("write {}", out_path.display()))?;
    Ok(out_path)
}

// --- Unsupported-shape rejection ----------------------------------------------

fn reject_unsupported(program: &Program) -> Result<()> {
    // Multiple dbcontexts collapse to one connection — runtime has a single
    // pooled client. Drivers other than Postgres aren't wired up.
    for ctx in &program.dbcontexts {
        if !ctx.driver.eq_ignore_ascii_case("postgres") {
            bail!(unsupported(&format!(
                "dbcontext driver `{}` (only `Postgres` is supported)",
                ctx.driver
            )));
        }
    }
    // `class` DTOs are accepted but produce no codegen — they only exist as
    // shape annotations for the interpreter's type checker.
    if !program.functions.iter().any(|f| f.name == "main") {
        bail!("Native build requires a `main()` function");
    }

    let known_funcs: HashSet<String> = program
        .functions
        .iter()
        .map(|f| f.name.clone())
        .chain(program.middlewares.iter().map(|m| m.name.clone()))
        .collect();
    let builtins: HashSet<&str> = native_builtin_names()
        .into_iter()
        .chain(SPECIAL_BUILTINS.iter().copied())
        .collect();

    for func in &program.functions {
        if func.name == "main" && !func.params.is_empty() {
            bail!(unsupported("parameters on main()"));
        }
        check_block(&func.body, &known_funcs, &builtins)?;
    }
    for mw in &program.middlewares {
        check_block(&mw.body, &known_funcs, &builtins)?;
    }
    if let Some(eh) = &program.error_handler {
        check_block(&eh.body, &known_funcs, &builtins)?;
    }

    for route in &program.routes {
        check_block(&route.body, &known_funcs, &builtins)?;
    }

    Ok(())
}

fn check_block(body: &[Stmt], funcs: &HashSet<String>, builtins: &HashSet<&str>) -> Result<()> {
    for stmt in body {
        check_stmt(stmt, funcs, builtins)?;
    }
    Ok(())
}

fn check_stmt(stmt: &Stmt, funcs: &HashSet<String>, builtins: &HashSet<&str>) -> Result<()> {
    match stmt {
        Stmt::Print(e) | Stmt::Let { value: e, .. } | Stmt::Assign { value: e, .. } => {
            check_expr(e, funcs, builtins)
        }
        Stmt::FieldAssign { value, .. } => check_expr(value, funcs, builtins),
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            check_expr(cond, funcs, builtins)?;
            check_block(then_body, funcs, builtins)?;
            if let Some(e) = else_body {
                check_block(e, funcs, builtins)?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            check_expr(cond, funcs, builtins)?;
            check_block(body, funcs, builtins)
        }
        Stmt::ForIn { iter, body, .. } => {
            check_expr(iter, funcs, builtins)?;
            check_block(body, funcs, builtins)
        }
        Stmt::Break | Stmt::Continue => Ok(()),
        Stmt::Return(opt) => {
            if let Some(e) = opt {
                check_expr(e, funcs, builtins)?;
            }
            Ok(())
        }
        Stmt::Expr(e) => check_expr(e, funcs, builtins),
        Stmt::DbInsert { .. } | Stmt::DbUpdate { .. } | Stmt::DbDelete { .. } => Ok(()),
        Stmt::DbDeleteWhere { where_clause, .. } => check_where(where_clause, funcs, builtins),
        Stmt::DbUpdateSet {
            assignments,
            where_clause,
            ..
        } => {
            for (_, rhs) in assignments {
                check_expr(rhs, funcs, builtins)?;
            }
            check_where(where_clause, funcs, builtins)
        }
        Stmt::ValidateBody { .. } => Ok(()),
        Stmt::Try {
            body, catch_body, ..
        } => {
            check_block(body, funcs, builtins)?;
            check_block(catch_body, funcs, builtins)
        }
        Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => {
            check_block(body, funcs, builtins)
        }
    }
}

fn check_expr(expr: &Expr, funcs: &HashSet<String>, builtins: &HashSet<&str>) -> Result<()> {
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_) => Ok(()),
        Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Mul(a, b)
        | Expr::Div(a, b)
        | Expr::Mod(a, b)
        | Expr::Eq(a, b)
        | Expr::Neq(a, b)
        | Expr::Lt(a, b)
        | Expr::Lte(a, b)
        | Expr::Gt(a, b)
        | Expr::Gte(a, b)
        | Expr::And(a, b)
        | Expr::Or(a, b) => {
            check_expr(a, funcs, builtins)?;
            check_expr(b, funcs, builtins)
        }
        Expr::Neg(e) | Expr::Not(e) => check_expr(e, funcs, builtins),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            check_expr(cond, funcs, builtins)?;
            check_expr(then_expr, funcs, builtins)?;
            check_expr(else_expr, funcs, builtins)
        }
        Expr::Coalesce(a, b) => {
            check_expr(a, funcs, builtins)?;
            check_expr(b, funcs, builtins)
        }
        Expr::FieldGet { .. } => Ok(()),
        Expr::NewEntity { .. } => Ok(()),
        Expr::ObjectLit(pairs) => {
            for (_, v) in pairs {
                check_expr(v, funcs, builtins)?;
            }
            Ok(())
        }
        Expr::ArrayLit(items) => {
            for item in items {
                check_expr(item, funcs, builtins)?;
            }
            Ok(())
        }
        Expr::Call { name, args } => {
            // `print(...)` as an expression evaluates to V::Null after the
            // side effect — emit_expr handles the special case.
            // Dotted names (`Dome.fn`) are flattened by the parser into a
            // single string; codegen sanitizes the `.` to `_` in user_fn_name.
            if name != "print" && !funcs.contains(name) && !builtins.contains(name.as_str()) {
                // Use a did-you-mean hint rather than dumping the full
                // builtin list — the old shape produced a paragraph-long
                // error for a typo. Walk both user functions and the
                // native builtin set so a typo'd `slep_ms` finds
                // `sleep_ms`; we only surface a candidate when it's
                // within the same Levenshtein noise floor the
                // interpreter uses for "Did you mean" on unknown
                // function calls.
                let mut candidates: Vec<String> = funcs.iter().cloned().collect();
                candidates.extend(native_builtin_names().iter().map(|s| (*s).to_string()));
                let hint = match codegen_closest_match(name, &candidates) {
                    Some(s) => format!(" — did you mean `{}`?", s),
                    None => String::new(),
                };
                bail!(unsupported(&format!(
                    "call to `{name}(...)` — unknown function{hint}. \
                     `jwc lint --explain E001` lists the full builtin set."
                )));
            }
            for a in args {
                check_expr(a, funcs, builtins)?;
            }
            Ok(())
        }
        Expr::Await(inner) => check_expr(inner, funcs, builtins),
        Expr::DbCount { where_clause, .. } => {
            if let Some(w) = where_clause {
                check_where(w, funcs, builtins)?;
            }
            Ok(())
        }
        Expr::DbAggregate { where_clause, .. } => {
            if let Some(w) = where_clause {
                check_where(w, funcs, builtins)?;
            }
            Ok(())
        }
        Expr::DbSelect {
            where_clause,
            limit,
            offset,
            ..
        } => {
            if let Some(w) = where_clause {
                check_where(w, funcs, builtins)?;
            }
            if let Some(e) = limit {
                check_limit_literal(e)?;
            }
            if let Some(e) = offset {
                check_limit_literal(e)?;
            }
            Ok(())
        }
    }
}

fn check_where(
    w: &crate::ast::WhereExpr,
    funcs: &HashSet<String>,
    builtins: &HashSet<&str>,
) -> Result<()> {
    use crate::ast::WhereExpr;
    match w {
        WhereExpr::Atom(a) => check_expr(&a.rhs, funcs, builtins),
        WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
            check_where(l, funcs, builtins)?;
            check_where(r, funcs, builtins)
        }
        WhereExpr::InList { values, .. } => {
            for v in values {
                check_expr(v, funcs, builtins)?;
            }
            Ok(())
        }
        WhereExpr::Between { low, high, .. } => {
            check_expr(low, funcs, builtins)?;
            check_expr(high, funcs, builtins)
        }
    }
}

/// `limit` / `offset` accept any expression; codegen wraps non-literal forms
/// in `jwc_to_int(...)` and binds them as i64 params.
fn check_limit_literal(_e: &Expr) -> Result<()> {
    Ok(())
}

fn unsupported(what: &str) -> String {
    format!(
        "Native build does not yet support {what}.\n\
         The compiled backend is being rolled out incrementally — see ROADMAP.md Phase 4.\n\
         Use `jwc build` (without --native) or `jwc run` for the interpreter for now."
    )
}

// --- Codegen ------------------------------------------------------------------

const PRELUDE: &str = include_str!("native_prelude.rs.in");
const PRELUDE_DB: &str = include_str!("native_prelude_db.rs.in");
const PRELUDE_WS: &str = include_str!("native_prelude_ws.rs.in");
const PRELUDE_CRYPTO: &str = include_str!("native_prelude_crypto.rs.in");

fn codegen(program: &Program, needs_db: bool, needs_crypto: bool) -> Result<String> {
    // `program` is already a flattened Program (see compile()) — every
    // FunctionDecl/Stmt::Call references functions by their resolved FQN.
    let known_funcs: HashSet<String> = program
        .functions
        .iter()
        .map(|f| f.name.clone())
        .chain(program.middlewares.iter().map(|m| m.name.clone()))
        .collect();
    let entities = collect_entities(program);

    // Always include the WS prelude so the HTTP dispatcher's
    // `RouteKind::Ws => jwc_run_ws_handler(...)` reference resolves even when
    // the program has no WS routes (dead-code-eliminated at link time).
    let needs_ws = true;

    let mut out = String::new();
    out.push_str(PRELUDE);
    if needs_db {
        out.push('\n');
        out.push_str(PRELUDE_DB);
    }
    if needs_ws {
        out.push('\n');
        out.push_str(PRELUDE_WS);
    }
    if needs_crypto {
        out.push('\n');
        out.push_str(PRELUDE_CRYPTO);
    }
    out.push('\n');

    // Phase 1 spike — struct monomorphization. Every `entity` gets a
    // concrete Rust struct + a `jwc_to_v` serializer that lifts it to
    // the dynamic V enum the rest of the runtime speaks. Not yet wired
    // onto the hot path; the next slice replaces `V::Object` on
    // `select` results with these structs so JSON serialisation skips
    // the FxHashMap lookup. `#[allow(dead_code)]` because the structs
    // are emitted unconditionally — the linker drops the ones nothing
    // references.
    emit_entity_structs(&mut out, &entities);

    let fn_decls: HashMap<String, &FunctionDecl> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let const_names: HashSet<String> = program
        .consts
        .iter()
        .map(|c| c.name.to_lowercase())
        .collect();
    let mw_refs: Vec<&crate::ast::MiddlewareDecl> = program.middlewares.iter().collect();
    // Inputs for the shared navigation SQL builder (reused from the
    // interpreter so native `with` eager-load emits identical SQL).
    let nav_models: HashMap<String, &crate::ast::ModelDecl> = program
        .models
        .iter()
        .map(|m| (m.name.to_lowercase(), m))
        .collect();
    let mut pk_by_table: HashMap<String, Vec<String>> = HashMap::new();
    for m in &program.models {
        let pks: Vec<String> = m
            .fields
            .iter()
            .filter(|f| f.is_primary_key)
            .map(|f| f.name.clone())
            .collect();
        pk_by_table.insert(m.name.to_lowercase(), pks);
    }
    let ctx = CodegenCtx {
        funcs: &known_funcs,
        entities: &entities,
        fn_decls: &fn_decls,
        middlewares: mw_refs,
        consts: &const_names,
        has_error_handler: program.error_handler.is_some(),
        closure_depth: std::cell::Cell::new(0),
        shapes: std::cell::RefCell::new(HashMap::new()),
        nav_models: &nav_models,
        pk_by_table: &pk_by_table,
    };

    // Module-level consts become zero-arg functions. A const that references
    // another const emits a call to that const's function (see emit_expr's
    // Var arm), so declaration order does not matter for the emitted Rust.
    for c in &program.consts {
        emit_const_fn(&mut out, c, &ctx);
    }

    for func in &program.functions {
        emit_user_fn(&mut out, func, &ctx);
    }
    for mw in &program.middlewares {
        emit_middleware_fn(&mut out, mw, &ctx);
    }
    if let Some(eh) = &program.error_handler {
        emit_error_handler_fn(&mut out, eh, &ctx);
    }

    for (idx, route) in program.routes.iter().enumerate() {
        emit_route_handler(&mut out, idx, route, &ctx);
    }

    emit_serve_impl(&mut out, &program.routes);

    // **Phase 1 [1.0-blocker]** — flush all interned object-literal shapes
    // as `__jwc_shape_N()` getters. Rust resolves item references regardless
    // of declaration order at module scope, so emitting these after every
    // route/function still binds correctly from every literal site above.
    // Using `std::sync::OnceLock` instead of `once_cell::Lazy` to avoid
    // pulling a new dep into the generated Cargo.toml — the prelude
    // already uses `OnceLock` for the HTTP client / param store.
    emit_shape_getters(&mut out, &ctx);

    out.push_str("\n#[tokio::main(flavor = \"multi_thread\")]\nasync fn main() {\n    jwc_install_panic_hook();\n    jwc_load_dotenv();\n    let _ = user_main().await;\n}\n");
    Ok(out)
}

/// Drain `ctx.shapes` into Rust source: one `__jwc_shape_N() -> &'static Arc<Vec<JwcStr>>`
/// per distinct literal key set in the program. Each getter wraps a
/// `OnceLock` so the field-name Arc is allocated once on first construction
/// and reused for every subsequent `v_record()` call with the same shape.
fn emit_shape_getters(out: &mut String, ctx: &CodegenCtx) {
    let shapes = ctx.shapes.borrow();
    if shapes.is_empty() {
        return;
    }
    // Emit in index order so the generated source is byte-stable across
    // builds (parity tests grep for shape constants).
    let mut pairs: Vec<(&Vec<String>, &usize)> = shapes.iter().collect();
    pairs.sort_by_key(|(_, idx)| **idx);
    out.push_str("\n// ── Phase 1 typed-shape constants for object literals ──\n");
    for (keys, idx) in pairs {
        out.push_str(&format!(
            "#[inline]\nfn __jwc_shape_{}() -> &'static ::std::sync::Arc<Vec<JwcStr>> {{\n",
            idx
        ));
        out.push_str("    static SHAPE: ::std::sync::OnceLock<::std::sync::Arc<Vec<JwcStr>>> = ::std::sync::OnceLock::new();\n");
        out.push_str("    SHAPE.get_or_init(|| ::std::sync::Arc::new(vec![");
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str("::std::borrow::Cow::Borrowed(\"");
            push_str_escaped(out, k);
            out.push_str("\")");
        }
        out.push_str("]))\n}\n");
    }
}

/// Phase 1 — struct monomorphization. For every `entity` declared in
/// the program we emit a concrete Rust struct (`JwcEnt_<Name>`) whose
/// fields match the column types one-to-one, plus a `jwc_to_v` method
/// that lifts the struct into the dynamic `V` enum. This is the foundation
/// the next slice will hook into `select` codegen so JSON serialisation
/// skips the FxHashMap that `V::Object` round-trips through (the analysis
/// in benchmark/REPORT.md attributes the `/json-large` gap to exactly
/// this dynamic shape).
///
/// PgKind → Rust type mapping:
///   * Smallint → i16, Int → i32, Bigint → i64
///   * Float    → f64
///   * Bool     → bool
///   * Timestamp / Str → String  (timestamps round-trip as ISO text)
///
/// Nullable columns wrap in `Option<T>`. Field names are emitted as-is
/// (snake_case in JWC source already), with `r#` raw-identifier escapes
/// for reserved words. The struct itself is `#[allow(dead_code, non_camel_case_types)]`
/// because (a) the spike isn't wired onto the hot path yet, and (b) the
/// JWC entity name may not be PascalCase by convention.
fn emit_entity_structs(out: &mut String, entities: &HashMap<String, EntityMeta>) {
    if entities.is_empty() {
        return;
    }
    // Iterate in a deterministic order so the generated source is
    // byte-stable across builds (parity tests assert substring matches).
    let mut names: Vec<&String> = entities.keys().collect();
    names.sort();

    out.push_str("// ── Phase 1 struct monomorphization (Phase 1 spike — not yet wired) ──\n");
    for name in names {
        let meta = &entities[name];
        out.push_str("#[allow(dead_code, non_camel_case_types, non_snake_case)]\n");
        out.push_str("#[derive(Clone, Debug, Default)]\n");
        out.push_str(&format!("struct JwcEnt_{} {{\n", name));
        for f in &meta.fields {
            let ty = rust_type_for_field(f);
            let raw = sanitize_struct_field_name(&f.name);
            out.push_str(&format!("    {}: {},\n", raw, ty));
        }
        out.push_str("}\n");

        out.push_str("#[allow(dead_code)]\n");
        out.push_str(&format!("impl JwcEnt_{} {{\n", name));
        out.push_str("    /// Lift this struct into the dynamic `V` enum so the\n");
        out.push_str("    /// existing FxHashMap-based code paths keep working until\n");
        out.push_str("    /// callers are upgraded to consume the struct directly.\n");
        out.push_str("    fn jwc_to_v(&self) -> V {\n");
        out.push_str("        let mut obj = JwcObj::default();\n");
        for f in &meta.fields {
            let field_ident = sanitize_struct_field_name(&f.name);
            let lift = lift_field_to_v(&field_ident, f);
            out.push_str(&format!(
                "        obj.insert(\"{}\".to_string(), {});\n",
                f.name, lift
            ));
        }
        out.push_str("        V::Object(Arc::new(obj))\n");
        out.push_str("    }\n");

        // ── Phase 1.5 — typed row reader ──────────────────────────────
        // Read columns by index in declared order. The select codegen
        // already lists columns in this exact order (whether `SELECT *`
        // or an explicit projection), so positional reads avoid the
        // per-row column-name lookup the dynamic path does. Eventually
        // replaces the `jwc_row_to_v` round-trip on `select`.
        out.push_str("    /// Read one row by column index in declared order. Caller is\n");
        out.push_str("    /// responsible for ensuring the SELECT projects columns in the\n");
        out.push_str("    /// same order the entity declares them in.\n");
        out.push_str("    fn jwc_from_row(row: &tokio_postgres::Row) -> Self {\n");
        out.push_str(&format!("        JwcEnt_{} {{\n", name));
        for (i, f) in meta.fields.iter().enumerate() {
            let field_ident = sanitize_struct_field_name(&f.name);
            let read = read_field_from_row(i, f);
            out.push_str(&format!("            {}: {},\n", field_ident, read));
        }
        out.push_str("        }\n");
        out.push_str("    }\n");

        // ── Phase 1.5 — direct JSON writer ────────────────────────────
        // Streams `{"col":value,...}` straight into a `String`. No
        // FxHashMap, no `V::Object` allocation, no `jwc_write_json`
        // dynamic dispatch on the hot path. Closing the `/json-large`
        // RPS gap hinges on this — the dynamic path goes through three
        // boxed allocations (Vec<V> per row, JwcObj per object,
        // serde_json::Value per stringify).
        out.push_str("    /// Append `{\"col\":value, ...}` to `out` without going\n");
        out.push_str("    /// through V::Object / serde_json::Value. Matches the\n");
        out.push_str("    /// byte-for-byte shape `jwc_write_json` produces for a `V::Object`,\n");
        out.push_str("    /// so client code can swap the two paths transparently.\n");
        out.push_str("    fn jwc_write_json(&self, out: &mut String) {\n");
        out.push_str("        out.push('{');\n");
        for (i, f) in meta.fields.iter().enumerate() {
            if i > 0 {
                out.push_str("        out.push(',');\n");
            }
            let key = json_string_escape(&f.name);
            out.push_str(&format!("        out.push_str(\"\\\"{}\\\":\");\n", key));
            let field_ident = sanitize_struct_field_name(&f.name);
            let write = write_field_to_json(&field_ident, f);
            out.push_str(&format!("        {}\n", write));
        }
        out.push_str("        out.push('}');\n");
        out.push_str("    }\n");
        out.push_str("}\n");
    }
    out.push('\n');
}

/// Escape a JSON string literal (just enough for column names — they
/// can't contain newlines or control bytes from the entity declaration,
/// so a `"` / `\\` escape pass is sufficient).
fn json_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Generate the expression that reads one column out of a
/// `tokio_postgres::Row` into the struct's field type. Mirrors the
/// `row.try_get::<i32, _>(i)` shape that `jwc_row_to_v` already uses for
/// the dynamic path. Width is locked to the declared `PgKind` to avoid
/// `WrongType` surprises.
fn read_field_from_row(idx: usize, f: &EntityField) -> String {
    let pg_ty = match f.pg {
        PgKind::Smallint => "i16",
        PgKind::Int => "i32",
        PgKind::Bigint => "i64",
        PgKind::Float => "f64",
        PgKind::Numeric => "rust_decimal::Decimal",
        PgKind::Bool => "bool",
        PgKind::Timestamp | PgKind::Str => "String",
    };
    if f.is_nullable {
        format!(
            "row.try_get::<_, Option<{ty}>>({idx}).unwrap_or(None)",
            ty = pg_ty,
            idx = idx
        )
    } else {
        // Non-null column — still go through `unwrap_or_default` so a
        // surprise NULL surfaces as the type's zero value instead of
        // panicking the worker on an unwrap. Mirrors `jwc_row_to_v`'s
        // "default to Null" branch for null columns.
        format!(
            "row.try_get::<_, {ty}>({idx}).unwrap_or_default()",
            ty = pg_ty,
            idx = idx
        )
    }
}

/// Generate the statement that appends `<field>` to a JSON output
/// buffer. Strings go through the existing `jwc_write_json_string`
/// escape helper in the prelude (handles quotes, backslashes, control
/// bytes). Numbers and booleans use `write!` for IEEE-correct output.
fn write_field_to_json(field_ident: &str, f: &EntityField) -> String {
    // `body` is the Rust statement that writes the inner value referenced
    // by the identifier `__v`. For non-null columns we just bind
    // `__v = &self.<field>` and run it; for nullable columns we wrap in
    // a `match` and use the same body in the `Some(__v)` arm, with a
    // `null` literal in the `None` arm.
    let body = match f.pg {
        // `Decimal`'s Display is a plain decimal literal (`1234.56`), which is
        // already valid JSON — no quoting, no exponent form.
        PgKind::Smallint | PgKind::Int | PgKind::Bigint | PgKind::Float | PgKind::Numeric => {
            "{ use std::fmt::Write; let _ = write!(out, \"{}\", __v); }".to_string()
        }
        PgKind::Bool => "{ out.push_str(if *__v { \"true\" } else { \"false\" }); }".to_string(),
        PgKind::Timestamp | PgKind::Str => "jwc_write_json_string(__v, out);".to_string(),
    };
    if f.is_nullable {
        format!(
            "match &self.{f} {{ Some(__v) => {{ {body} }}, None => out.push_str(\"null\"), }}",
            f = field_ident,
            body = body,
        )
    } else {
        format!("{{ let __v = &self.{}; {} }}", field_ident, body)
    }
}

/// Map a `PgKind` (+ nullability) to the concrete Rust type the
/// monomorphized struct field uses.
fn rust_type_for_field(f: &EntityField) -> String {
    let base = match f.pg {
        PgKind::Smallint => "i16",
        PgKind::Int => "i32",
        PgKind::Bigint => "i64",
        PgKind::Float => "f64",
        PgKind::Numeric => "rust_decimal::Decimal",
        PgKind::Bool => "bool",
        PgKind::Timestamp | PgKind::Str => "String",
    };
    if f.is_nullable {
        format!("Option<{}>", base)
    } else {
        base.to_string()
    }
}

/// Avoid Rust reserved-word collisions (`type`, `fn`, `match`, …) by
/// emitting a raw identifier (`r#type`) where needed. Anything else
/// passes through.
fn sanitize_struct_field_name(name: &str) -> String {
    const RESERVED: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ];
    if RESERVED.contains(&name) {
        format!("r#{}", name)
    } else {
        name.to_string()
    }
}

/// Generate the Rust expression that wraps a struct field into a `V`
/// variant. Mirrors the Postgres → V translation the dynamic codepath
/// already does for `V::Object` row reads.
fn lift_field_to_v(field_ident: &str, f: &EntityField) -> String {
    let inner = match f.pg {
        PgKind::Smallint => format!("V::Int(self.{} as i64)", field_ident),
        PgKind::Int => format!("V::Int(self.{} as i64)", field_ident),
        PgKind::Bigint => format!("V::Int(self.{})", field_ident),
        PgKind::Float => format!("V::Float(self.{})", field_ident),
        PgKind::Numeric => format!("V::Float(jwc_decimal_f64(self.{}))", field_ident),
        PgKind::Bool => format!("V::Bool(self.{})", field_ident),
        PgKind::Timestamp | PgKind::Str => format!("v_str(self.{}.clone())", field_ident),
    };
    if f.is_nullable {
        // Option<T>::as_ref().map(|v| <inner>).unwrap_or(V::Null) —
        // but we can't `self.x` twice through map() with the same form,
        // so reroute through a match for clarity.
        let inner_match = match f.pg {
            PgKind::Smallint => "V::Int(*v as i64)".to_string(),
            PgKind::Int => "V::Int(*v as i64)".to_string(),
            PgKind::Bigint => "V::Int(*v)".to_string(),
            PgKind::Float => "V::Float(*v)".to_string(),
            PgKind::Numeric => "V::Float(jwc_decimal_f64(*v))".to_string(),
            PgKind::Bool => "V::Bool(*v)".to_string(),
            PgKind::Timestamp | PgKind::Str => "v_str(v.clone())".to_string(),
        };
        format!(
            "match &self.{} {{ Some(v) => {}, None => V::Null }}",
            field_ident, inner_match
        )
    } else {
        inner
    }
}

fn collect_entities(program: &Program) -> HashMap<String, EntityMeta> {
    let mut out = HashMap::new();
    for model in &program.models {
        if !matches!(model.kind, ModelKind::Entity) {
            continue;
        }
        let fields = model
            .fields
            .iter()
            .map(|f| EntityField {
                name: f.name.clone(),
                pg: pg_kind_for(&f.ty.name),
                is_auto_increment: f.is_auto_increment,
                is_primary_key: f.is_primary_key,
                is_nullable: f.is_nullable,
            })
            .collect();
        out.insert(
            model.name.clone(),
            EntityMeta {
                // Postgres tables are snake_case (see sql::to_snake_case), so
                // the generated SQL must match what migrations produced.
                table: crate::sql::to_snake_case(&model.name),
                fields,
            },
        );
    }
    out
}

struct CodegenCtx<'a> {
    funcs: &'a HashSet<String>,
    entities: &'a HashMap<String, EntityMeta>,
    /// Lookup table from function name → its declaration. Used by route
    /// `-> handler` codegen to pull declared parameter names so we can bind
    /// path/query params positionally.
    fn_decls: &'a HashMap<String, &'a FunctionDecl>,
    /// MiddlewareDecl references for `after { ... }` lookups during
    /// route-handler codegen. Routes name middlewares by string; this
    /// vec lets the dispatch loop find the matching decl and emit
    /// `mw_<name>_after()` calls when the middleware ships one.
    middlewares: Vec<&'a crate::ast::MiddlewareDecl>,
    /// Lowercased names of all module-level consts. A `Var` matching one emits
    /// a call to its `jwc_const_*` function instead of a local binding clone.
    consts: &'a HashSet<String>,
    has_error_handler: bool,
    /// Number of nested catch_unwind closures we're currently emitting into.
    /// `return X;` inside any closure (try/transaction body) must park the
    /// value in the thread-local slot instead of doing a direct Rust return.
    closure_depth: std::cell::Cell<usize>,
    /// **Phase 1 [1.0-blocker]** — shape-dedup table for object-literal
    /// codegen. Key is the field-name list in **declaration order** (matches
    /// the interpreter, which preserves declaration order in
    /// `Value::Record::field_names`). Value is the shape's stable index N,
    /// which gets emitted as the `__jwc_shape_N()` getter. Every literal
    /// `{ id, name }` site reuses the same shape Arc, so the bench app's
    /// `/json-large` route (1000 same-shape constructions per request) gets
    /// one shape allocation instead of one FxHashMap per row.
    shapes: std::cell::RefCell<HashMap<Vec<String>, usize>>,
    /// Interpreter-shared inputs for the navigation SQL builder
    /// (`runner::sql::build_navigation_subqueries`). Native `with` eager-load
    /// reuses the exact correlated json_agg subquery SQL the interpreter emits
    /// — same SQL, no divergent second implementation. Both keyed by
    /// lowercased entity name.
    nav_models: &'a HashMap<String, &'a crate::ast::ModelDecl>,
    pk_by_table: &'a HashMap<String, Vec<String>>,
}

impl<'a> CodegenCtx<'a> {
    fn enter_closure(&self) {
        self.closure_depth.set(self.closure_depth.get() + 1);
    }
    fn exit_closure(&self) {
        self.closure_depth.set(self.closure_depth.get() - 1);
    }
    fn in_closure(&self) -> bool {
        self.closure_depth.get() > 0
    }

    /// Intern an object-literal key list and return the stable shape index.
    /// Identical key lists (in **declaration order** — we don't sort, so
    /// `{a, b}` and `{b, a}` get distinct shapes, matching the interpreter
    /// where field-name order is observable through `value_to_json`) share
    /// the same `__jwc_shape_N` getter.
    fn intern_shape(&self, keys: Vec<String>) -> usize {
        let mut m = self.shapes.borrow_mut();
        if let Some(&idx) = m.get(&keys) {
            return idx;
        }
        let idx = m.len();
        m.insert(keys, idx);
        idx
    }
}

fn emit_route_handler(
    out: &mut String,
    idx: usize,
    route: &crate::ast::RouteDecl,
    ctx: &CodegenCtx,
) {
    // Collect MiddlewareDecl references for the `after` lookups. Routes
    // reference middlewares by name; we resolve them against the
    // already-flattened decl list `CodegenCtx` carries.
    let mw_decls: Vec<&crate::ast::MiddlewareDecl> = ctx.middlewares.to_vec();
    out.push_str(&format!(
        "\nfn route_{idx}_inner() -> std::pin::Pin<Box<dyn std::future::Future<Output = V> + Send>> {{\n"
    ));
    out.push_str("    Box::pin(async move {\n");
    for mw in &route.middlewares {
        out.push_str(&format!(
            "        {{ let __mw = {}().await; if !matches!(__mw, V::Null) {{ return __mw; }} }}\n",
            middleware_fn_name(mw),
        ));
    }
    // Capture the handler result so the after-chain can run on the way
    // out. The variable is shared between the handler call and the
    // after-block dispatch below.
    if let Some(handler) = &route.handler {
        let args = handler_arg_exprs(handler, ctx);
        out.push_str(&format!(
            "        let __resp = {}({}).await;\n",
            user_fn_name(handler),
            args
        ));
    } else {
        out.push_str("        let __resp = {\n");
        for stmt in &route.body {
            emit_stmt(out, stmt, 3, ctx);
        }
        out.push_str("            if let Some(__r) = jwc_take_return() { __r } else { V::Null }\n");
        out.push_str("        };\n");
    }
    // Populate `Request::response_status` so `response_status()` is
    // non-null inside after-blocks. Done after `__resp` is bound and
    // before the reverse-order after-chain dispatches — mirrors the
    // interpreter, which also computes the status at handler-return
    // time so middleware on the way out can read it.
    //
    // **Skip** when this route has zero middlewares with an after-body:
    // the status-set is observable only through `response_status()`
    // inside an after-block, so without one it's pure per-request
    // overhead (~50ns: V match + atomic store) that nothing reads.
    // Concretely: a stateless route like `route GET "/ping" { return "pong"; }`
    // with no middleware emits zero Phase-5 instrumentation now.
    let has_after_in_chain = route
        .middlewares
        .iter()
        .any(|mw| middleware_has_after(mw, &mw_decls));
    if has_after_in_chain {
        out.push_str("        jwc_set_response_status(jwc_status_of(&__resp));\n");
        // Response-phase middleware: reverse order so an outer `after` can
        // wrap the inner ones. Mirrors koa / express / ring + the
        // interpreter's order. Errors from an after-block are caught at
        // the dispatcher level — but in this slice they propagate via the
        // standard try-shape; codegen doesn't catch them yet.
        for mw in route.middlewares.iter().rev() {
            if middleware_has_after(mw, &mw_decls) {
                out.push_str(&format!(
                    "        let _ = {}_after().await;\n",
                    middleware_fn_name(mw),
                ));
            }
        }
    }
    out.push_str("        if let Some(__r) = jwc_take_return() { return __r; }\n");
    out.push_str("        __resp\n");
    out.push_str("    })\n");
    out.push_str("}\n");

    out.push_str(&format!(
        "\nfn route_{idx}() -> std::pin::Pin<Box<dyn std::future::Future<Output = V> + Send>> {{\n"
    ));
    if ctx.has_error_handler {
        // `futures::FutureExt::catch_unwind` catches panics during polling.
        out.push_str("    Box::pin(async move {\n");
        out.push_str(&format!(
            "        match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(route_{idx}_inner())).await {{\n"
        ));
        out.push_str("            Ok(v) => v,\n");
        out.push_str("            Err(e) => {\n");
        out.push_str("                let __msg = jwc_panic_payload_to_string(e);\n");
        out.push_str("                let _ = jwc_take_return();\n");
        out.push_str("                jwc_user_error_handler(jwc_error_value(__msg)).await\n");
        out.push_str("            }\n");
        out.push_str("        }\n");
        out.push_str("    })\n");
    } else {
        out.push_str(&format!("    route_{idx}_inner()\n"));
    }
    out.push_str("}\n");
}

/// Map a route handler call's positional args from its declared parameter
/// names: try the path param of the same name first, fall back to query
/// param, finally `V::Null`. Mirrors `runner.rs::build_handler_args`.
fn handler_arg_exprs(handler: &str, ctx: &CodegenCtx) -> String {
    let Some(decl) = ctx.fn_decls.get(handler) else {
        return String::new();
    };
    let mut parts = Vec::with_capacity(decl.params.len());
    for p in &decl.params {
        let name_lit = p.name.replace('"', "\\\"");
        parts.push(format!("jwc_b_handler_arg(\"{}\")", name_lit));
    }
    parts.join(", ")
}

fn middleware_fn_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("mw_{safe}")
}

fn emit_middleware_fn(out: &mut String, mw: &crate::ast::MiddlewareDecl, ctx: &CodegenCtx) {
    out.push_str(&format!(
        "\nfn {}() -> std::pin::Pin<Box<dyn std::future::Future<Output = V> + Send>> {{\n",
        middleware_fn_name(&mw.name)
    ));
    out.push_str("    Box::pin(async move {\n");
    for stmt in &mw.body {
        emit_stmt(out, stmt, 2, ctx);
    }
    out.push_str("        if let Some(__r) = jwc_take_return() { return __r; }\n");
    out.push_str("        V::Null\n");
    out.push_str("    })\n");
    out.push_str("}\n");

    // Response-phase `after { ... }` block. Emitted whenever the source
    // declares one — the dispatcher calls it in reverse middleware
    // order after the handler completes. Same shape as the main body
    // (boxed future, take_return drain) so callers don't need to
    // branch on "has after".
    if let Some(after) = &mw.after_body {
        out.push_str(&format!(
            "\nfn {}_after() -> std::pin::Pin<Box<dyn std::future::Future<Output = V> + Send>> {{\n",
            middleware_fn_name(&mw.name)
        ));
        out.push_str("    Box::pin(async move {\n");
        for stmt in after {
            emit_stmt(out, stmt, 2, ctx);
        }
        out.push_str("        if let Some(__r) = jwc_take_return() { return __r; }\n");
        out.push_str("        V::Null\n");
        out.push_str("    })\n");
        out.push_str("}\n");
    }
}

/// True when the named middleware ships an `after { ... }` block.
/// Used by `emit_route_handler` to decide whether to wire a call.
fn middleware_has_after(name: &str, decls: &[&crate::ast::MiddlewareDecl]) -> bool {
    decls
        .iter()
        .any(|m| m.name.eq_ignore_ascii_case(name) && m.after_body.is_some())
}

fn emit_error_handler_fn(out: &mut String, eh: &crate::ast::ErrorHandlerDecl, ctx: &CodegenCtx) {
    out.push_str(&format!(
        "\nfn jwc_user_error_handler(mut {}: V) -> std::pin::Pin<Box<dyn std::future::Future<Output = V> + Send>> {{\n",
        sanitize_ident(&eh.catch_var),
    ));
    out.push_str("    Box::pin(async move {\n");
    for stmt in &eh.body {
        emit_stmt(out, stmt, 2, ctx);
    }
    out.push_str("        if let Some(__r) = jwc_take_return() { return __r; }\n");
    out.push_str("        V::Null\n");
    out.push_str("    })\n");
    out.push_str("}\n");
}

fn emit_serve_impl(out: &mut String, routes: &[crate::ast::RouteDecl]) {
    out.push_str("\nasync fn jwc_serve_impl(port: u16) {\n");
    out.push_str("    let mut router = Router::new();\n");
    for (idx, route) in routes.iter().enumerate() {
        let path = normalise_path(&route.path);
        if matches!(route.protocol, crate::ast::RouteProtocol::Ws) {
            // WS routes always come in on GET; the dispatcher checks
            // Upgrade headers + Sec-WebSocket-Key before promoting.
            out.push_str("    router.add_ws(\"");
            push_str_escaped(out, &path);
            out.push_str(&format!("\", route_{idx});\n"));
        } else {
            let method = route.method.to_uppercase();
            out.push_str("    router.add(\"");
            push_str_escaped(out, &method);
            out.push_str("\", \"");
            push_str_escaped(out, &path);
            out.push_str(&format!("\", route_{idx});\n"));
        }
    }
    out.push_str("    HttpServer::new(port, router).run().await;\n");
    out.push_str("}\n");
}

/// `users/{id}` and `/users/{id}` both reach the runtime as `/users/{id}`.
fn normalise_path(p: &str) -> String {
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

fn emit_user_fn(out: &mut String, func: &FunctionDecl, ctx: &CodegenCtx) {
    out.push_str("\nfn ");
    out.push_str(&user_fn_name(&func.name));
    out.push('(');
    let mut first = true;
    for p in &func.params {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str("mut ");
        out.push_str(&sanitize_ident(&p.name));
        out.push_str(": V");
    }
    out.push_str(") -> std::pin::Pin<Box<dyn std::future::Future<Output = V> + Send>> {\n");
    out.push_str("    Box::pin(async move {\n");
    for stmt in &func.body {
        emit_stmt(out, stmt, 2, ctx);
    }
    out.push_str("        if let Some(__r) = jwc_take_return() { return __r; }\n");
    out.push_str("        V::Null\n");
    out.push_str("    })\n");
    out.push_str("}\n");
}

fn user_fn_name(name: &str) -> String {
    // Namespaced functions arrive as `Dome.fn` and package-qualified ones can
    // include `-` from `kebab-case` package names. Map any non-ident char to
    // `_` so the emitted Rust identifier is always valid.
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("user_{safe}")
}

fn builtin_fn_name(name: &str) -> String {
    let snake = match name {
        "notFound" => "not_found",
        "noContent" => "no_content",
        "internalError" => "internal_error",
        "statusCode" => "status_code",
        "badRequest" => "bad_request",
        _ => name,
    };
    format!("jwc_b_{snake}")
}

fn emit_stmt(out: &mut String, stmt: &Stmt, indent: usize, ctx: &CodegenCtx) {
    let pad = "    ".repeat(indent);
    match stmt {
        Stmt::Let { name, value } => {
            out.push_str(&pad);
            if name == "_" {
                // `let _: V = ...` — placeholder binding can't be `mut`.
                out.push_str("let _: V = ");
            } else {
                out.push_str("let mut ");
                out.push_str(&sanitize_ident(name));
                out.push_str(": V = ");
            }
            emit_expr(out, value, ctx);
            out.push_str(";\n");
        }
        Stmt::Assign { name, value } => {
            out.push_str(&pad);
            out.push_str(&sanitize_ident(name));
            out.push_str(" = ");
            emit_expr(out, value, ctx);
            out.push_str(";\n");
        }
        Stmt::FieldAssign { var, field, value } => {
            // Evaluate RHS into a temp first so the &mut and any & borrows of
            // `var` don't overlap (Rust borrow checker rejects `f(&mut x, …, &x)`
            // because arg evaluation order keeps the &mut alive across &).
            out.push_str(&pad);
            out.push_str("{ let __rhs = ");
            emit_expr(out, value, ctx);
            out.push_str("; jwc_set_field(&mut ");
            out.push_str(&sanitize_ident(var));
            out.push_str(", \"");
            push_str_escaped(out, field);
            out.push_str("\", __rhs); }\n");
        }
        Stmt::Print(expr) => {
            out.push_str(&pad);
            out.push_str("jwc_print(");
            emit_expr(out, expr, ctx);
            out.push_str(");\n");
        }
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            out.push_str(&pad);
            out.push_str("if jwc_truthy(&");
            emit_expr(out, cond, ctx);
            out.push_str(") {\n");
            for s in then_body {
                emit_stmt(out, s, indent + 1, ctx);
            }
            out.push_str(&pad);
            if let Some(else_b) = else_body {
                out.push_str("} else {\n");
                for s in else_b {
                    emit_stmt(out, s, indent + 1, ctx);
                }
                out.push_str(&pad);
            }
            out.push_str("}\n");
        }
        Stmt::While { cond, body } => {
            out.push_str(&pad);
            out.push_str("while jwc_truthy(&");
            emit_expr(out, cond, ctx);
            out.push_str(") {\n");
            for s in body {
                emit_stmt(out, s, indent + 1, ctx);
            }
            out.push_str(&pad);
            out.push_str("}\n");
        }
        Stmt::ForIn { var, iter, body } => {
            out.push_str(&pad);
            out.push_str("for __item in jwc_to_array(");
            emit_expr(out, iter, ctx);
            out.push_str(") {\n");
            let inner = "    ".repeat(indent + 1);
            out.push_str(&inner);
            out.push_str("let mut ");
            out.push_str(&sanitize_ident(var));
            out.push_str(": V = __item;\n");
            for s in body {
                emit_stmt(out, s, indent + 1, ctx);
            }
            out.push_str(&pad);
            out.push_str("}\n");
        }
        Stmt::Break => {
            out.push_str(&pad);
            out.push_str("break;\n");
        }
        Stmt::Continue => {
            out.push_str(&pad);
            out.push_str("continue;\n");
        }
        Stmt::Return(opt) => {
            out.push_str(&pad);
            if ctx.in_closure() {
                // Inside a try/transaction closure, a Rust `return` only exits
                // the closure. Park the value in the thread-local so the outer
                // function's drain picks it up.
                out.push_str("{ jwc_set_return(");
                match opt {
                    Some(e) => emit_expr(out, e, ctx),
                    None => out.push_str("V::Null"),
                }
                out.push_str("); return V::Null; }\n");
            } else {
                out.push_str("return ");
                match opt {
                    Some(e) => emit_expr(out, e, ctx),
                    None => out.push_str("V::Null"),
                }
                out.push_str(";\n");
            }
        }
        Stmt::Expr(e) => {
            out.push_str(&pad);
            out.push_str("let _ = ");
            emit_expr(out, e, ctx);
            out.push_str(";\n");
        }
        Stmt::DbInsert { var, table, .. } => {
            emit_db_insert(out, &pad, var, table, ctx);
        }
        Stmt::DbUpdate { var, table, .. } => {
            emit_db_update(out, &pad, var, table, ctx);
        }
        Stmt::DbDelete { var, table, .. } => {
            emit_db_delete_by_var(out, &pad, var, table, ctx);
        }
        Stmt::DbDeleteWhere {
            table,
            where_clause,
            ..
        } => {
            emit_db_delete_where(out, &pad, table, where_clause, ctx);
        }
        Stmt::DbUpdateSet {
            table,
            assignments,
            where_clause,
            ..
        } => {
            emit_db_update_set(out, &pad, table, assignments, where_clause, ctx);
        }
        Stmt::ValidateBody { fields } => {
            emit_validate_body(out, &pad, fields, ctx);
        }
        Stmt::Try {
            body,
            catch_var,
            catch_type,
            catch_body,
            ..
        } => {
            emit_try_catch(
                out,
                &pad,
                body,
                catch_var,
                catch_type.as_deref(),
                catch_body,
                indent,
                ctx,
            );
        }
        Stmt::Transaction { body } => {
            emit_transaction(out, &pad, body, indent, ctx);
        }
        Stmt::Savepoint { .. } => {
            // Sprint 4B — `savepoint <name> { ... }` is interpreter-only in
            // this slice. The native AOT path doesn't yet model nested
            // transaction boundaries; emit a clear panic so a user who hit
            // `jwc build --native` on a project using savepoints gets a
            // helpful diagnostic instead of silent breakage. The
            // interpreter (`jwc run`) handles the full flow.
            out.push_str(&pad);
            out.push_str(
                "panic!(\"savepoint not supported in --native build yet; use `jwc run`\");\n",
            );
        }
    }
}

fn emit_try_catch(
    out: &mut String,
    pad: &str,
    body: &[Stmt],
    catch_var: &str,
    catch_type: Option<&str>,
    catch_body: &[Stmt],
    indent: usize,
    ctx: &CodegenCtx,
) {
    let inner = format!("{pad}    ");
    out.push_str(pad);
    out.push_str("{\n");
    out.push_str(&inner);
    // Mark the region so the panic hook stays quiet for a failure the user's
    // `catch` block is about to handle.
    out.push_str("jwc_enter_caught_region();\n");
    out.push_str(&inner);
    out.push_str(
        "let __res = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {\n",
    );

    ctx.enter_closure();
    for s in body {
        emit_stmt(out, s, indent + 2, ctx);
    }
    ctx.exit_closure();

    let inner2 = format!("{inner}    ");
    out.push_str(&inner2);
    out.push_str("V::Null\n");
    out.push_str(&inner);
    out.push_str("})).await;\n");
    out.push_str(&inner);
    out.push_str("jwc_leave_caught_region();\n");

    // If body parked a return, propagate it (skip catch).
    out.push_str(&inner);
    out.push_str("if jwc_has_return() {\n");
    if ctx.in_closure() {
        out.push_str(&inner2);
        out.push_str("return V::Null;\n");
    } else {
        out.push_str(&inner2);
        out.push_str("return jwc_take_return().unwrap();\n");
    }
    out.push_str(&inner);
    out.push_str("}\n");

    // On panic, classify the error, optionally filter by catch_type, then
    // bind error to catch_var and run catch_body. Mismatched typed catches
    // re-raise via `resume_unwind` so an outer handler can see them.
    out.push_str(&inner);
    out.push_str("if let Err(__e) = __res {\n");
    out.push_str(&inner2);
    out.push_str("let __msg = jwc_panic_payload_to_string(__e);\n");
    out.push_str(&inner2);
    out.push_str("let __kind = jwc_classify_error(&__msg);\n");
    let type_arg = match catch_type {
        Some(t) => format!("Some(\"{}\")", t.replace('\\', "\\\\").replace('"', "\\\"")),
        None => "None".to_string(),
    };
    out.push_str(&inner2);
    out.push_str(&format!(
        "if !jwc_catch_type_matches({}, __kind) {{ std::panic::resume_unwind(Box::new(__msg)); }}\n",
        type_arg
    ));
    out.push_str(&inner2);
    out.push_str(&format!(
        "let mut {}: V = jwc_error_value(__msg);\n",
        sanitize_ident(catch_var),
    ));
    for s in catch_body {
        emit_stmt(out, s, indent + 2, ctx);
    }
    out.push_str(&inner);
    out.push_str("}\n");
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_transaction(out: &mut String, pad: &str, body: &[Stmt], indent: usize, ctx: &CodegenCtx) {
    let inner = format!("{pad}    ");
    out.push_str(pad);
    out.push_str("{\n");
    out.push_str(&inner);
    out.push_str("let _ = jwc_db_exec(\"BEGIN\", vec![]).await;\n");
    out.push_str(&inner);
    // Mark the region so the panic hook stays quiet for a failure the user's
    // `catch` block is about to handle.
    out.push_str("jwc_enter_caught_region();\n");
    out.push_str(&inner);
    out.push_str(
        "let __res = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {\n",
    );

    ctx.enter_closure();
    for s in body {
        emit_stmt(out, s, indent + 2, ctx);
    }
    ctx.exit_closure();

    let inner2 = format!("{inner}    ");
    out.push_str(&inner2);
    out.push_str("V::Null\n");
    out.push_str(&inner);
    out.push_str("})).await;\n");
    out.push_str(&inner);
    out.push_str("jwc_leave_caught_region();\n");

    // Same return-slot propagation: an explicit `return` inside the
    // transaction body commits before bubbling the return up.
    out.push_str(&inner);
    out.push_str("if jwc_has_return() {\n");
    out.push_str(&inner2);
    out.push_str("let _ = jwc_db_exec(\"COMMIT\", vec![]).await;\n");
    if ctx.in_closure() {
        out.push_str(&inner2);
        out.push_str("return V::Null;\n");
    } else {
        out.push_str(&inner2);
        out.push_str("return jwc_take_return().unwrap();\n");
    }
    out.push_str(&inner);
    out.push_str("}\n");

    out.push_str(&inner);
    out.push_str("match __res {\n");
    out.push_str(&inner2);
    out.push_str("Ok(_) => { let _ = jwc_db_exec(\"COMMIT\", vec![]).await; }\n");
    out.push_str(&inner2);
    out.push_str("Err(__e) => { let _ = jwc_db_exec(\"ROLLBACK\", vec![]).await; std::panic::resume_unwind(__e); }\n");
    out.push_str(&inner);
    out.push_str("}\n");
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_validate_body(
    out: &mut String,
    pad: &str,
    fields: &[crate::ast::ValidateField],
    ctx: &CodegenCtx,
) {
    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __body = jwc_b_body();\n");
    out.push_str(&inner);
    out.push_str("let mut __errors: JwcObj = JwcObj::default();\n");
    for field in fields {
        let fname = field.name.replace('"', "\\\"");
        out.push_str(&inner);
        out.push_str(&format!(
            "let __field = jwc_get_field(&__body, \"{}\");\n",
            fname
        ));
        for rule in &field.rules {
            out.push_str(&inner);
            match rule {
                crate::ast::ValidateRule::Required => {
                    out.push_str(&format!(
                        "if matches!(__field, V::Null) {{ __errors.insert(\"{f}\".to_string(), v_str(\"required\")); }}\n",
                        f = fname,
                    ));
                }
                crate::ast::ValidateRule::MinLength(n) => {
                    out.push_str(&format!(
                        "if let V::Str(ref __s) = __field {{ if __s.chars().count() < {n} {{ __errors.insert(\"{f}\".to_string(), v_str(\"minLength {n}\")); }} }}\n",
                        n = n, f = fname,
                    ));
                }
                crate::ast::ValidateRule::MaxLength(n) => {
                    out.push_str(&format!(
                        "if let V::Str(ref __s) = __field {{ if __s.chars().count() > {n} {{ __errors.insert(\"{f}\".to_string(), v_str(\"maxLength {n}\")); }} }}\n",
                        n = n, f = fname,
                    ));
                }
                crate::ast::ValidateRule::Min(v) => {
                    out.push_str(&format!(
                        "{{ if let Some(__n) = jwc_to_float(&__field) {{ if __n < {v}_f64 {{ __errors.insert(\"{f}\".to_string(), v_str(\"min {v}\")); }} }} }}\n",
                        v = v, f = fname,
                    ));
                }
                crate::ast::ValidateRule::Max(v) => {
                    out.push_str(&format!(
                        "{{ if let Some(__n) = jwc_to_float(&__field) {{ if __n > {v}_f64 {{ __errors.insert(\"{f}\".to_string(), v_str(\"max {v}\")); }} }} }}\n",
                        v = v, f = fname,
                    ));
                }
                crate::ast::ValidateRule::Pattern(_) => {
                    // No regex crate in the native prelude yet — skip with a
                    // best-effort non-empty string check. Document below.
                    out.push_str(&format!(
                        "if !matches!(__field, V::Str(_)) {{ __errors.insert(\"{f}\".to_string(), v_str(\"pattern\")); }}\n",
                        f = fname,
                    ));
                }
            }
        }
    }
    out.push_str(&inner);
    out.push_str("if !__errors.is_empty() {\n");
    let inner2 = format!("{inner}    ");
    out.push_str(&inner2);
    out.push_str("let mut __payload: JwcObj = JwcObj::default();\n");
    out.push_str(&inner2);
    out.push_str("__payload.insert(\"status\".to_string(), V::Int(400));\n");
    out.push_str(&inner2);
    out.push_str("__payload.insert(\"error\".to_string(), v_str(\"Validation failed\"));\n");
    out.push_str(&inner2);
    out.push_str("__payload.insert(\"fields\".to_string(), v_obj(__errors));\n");
    out.push_str(&inner2);
    if ctx.in_closure() {
        out.push_str("{ jwc_set_return(v_obj(__payload)); return V::Null; }\n");
    } else {
        out.push_str("return v_obj(__payload);\n");
    }
    out.push_str(&inner);
    out.push_str("}\n");
    out.push_str(pad);
    out.push_str("}\n");
}

fn helper_for_kind(k: PgKind) -> &'static str {
    match k {
        PgKind::Smallint => "jwc_param_smallint",
        PgKind::Int => "jwc_param_int",
        PgKind::Bigint => "jwc_param_bigint",
        PgKind::Float => "jwc_param_float",
        PgKind::Numeric => "jwc_param_numeric",
        PgKind::Bool => "jwc_param_bool",
        // Timestamp values arrive as RFC 3339 strings; the helper parses
        // them into `chrono::DateTime<Utc>` so tokio-postgres binds the
        // value against TIMESTAMPTZ natively. A bare `$N::timestamptz`
        // SQL cast does NOT fix this — Postgres still infers `$N` as
        // timestamptz from the cast, so the client-side type check rejects
        // the String bind before the cast ever runs.
        PgKind::Timestamp => "jwc_param_timestamp",
        PgKind::Str => "jwc_param_str",
    }
}

/// SQL placeholder for parameter `idx` (1-based).
///
/// Used to emit `${idx}::timestamptz` for Timestamp columns on the theory
/// the cast would coerce a TEXT bind. It doesn't — `prepare_cached` still
/// reports `$idx` as `timestamptz` (Postgres infers the parameter type
/// from the cast target), so tokio-postgres rejected the String bind
/// client-side before the cast ever ran. The real fix lives in
/// `jwc_param_timestamp`, which boxes a real `chrono::DateTime<Utc>`.
fn placeholder_for(idx: usize, _k: PgKind) -> String {
    format!("${}", idx)
}

fn pk_fields(meta: &EntityMeta) -> Vec<&EntityField> {
    let mut pks: Vec<&EntityField> = meta.fields.iter().filter(|f| f.is_primary_key).collect();
    if pks.is_empty() {
        // Fall back to a field literally named `id` (mirrors runner.rs default).
        if let Some(f) = meta
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case("id"))
        {
            pks.push(f);
        }
    }
    pks
}

fn emit_db_insert(out: &mut String, pad: &str, var: &str, table: &str, ctx: &CodegenCtx) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!(
                "panic!(\"insert into unknown entity {}\");\n",
                table
            ));
            return;
        }
    };
    let cols: Vec<&EntityField> = meta
        .fields
        .iter()
        .filter(|f| !f.is_auto_increment)
        .collect();
    let mut sql = format!("INSERT INTO \"{}\" (", meta.table);
    for (i, f) in cols.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("\"{}\"", f.name));
    }
    sql.push_str(") VALUES (");
    for (i, f) in cols.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&placeholder_for(i + 1, f.pg));
    }
    sql.push(')');

    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __var = &");
    out.push_str(&sanitize_ident(var));
    out.push_str(";\n");
    out.push_str(&inner);
    out.push_str("let __params: DbParams = vec![\n");
    for f in &cols {
        out.push_str(&format!(
            "{inner}    {helper}(jwc_get_field(__var, \"{name}\")),\n",
            inner = inner,
            helper = helper_for_kind(f.pg),
            name = f.name,
        ));
    }
    out.push_str(&inner);
    out.push_str("];\n");
    out.push_str(&inner);
    out.push_str(&format!(
        "let _ = jwc_db_exec(\"{}\", __params).await;\n",
        escape_sql(&sql)
    ));
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_db_update(out: &mut String, pad: &str, var: &str, table: &str, ctx: &CodegenCtx) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!(
                "panic!(\"update on unknown entity {}\");\n",
                table
            ));
            return;
        }
    };
    let pks = pk_fields(meta);
    let set_cols: Vec<&EntityField> = meta
        .fields
        .iter()
        .filter(|f| !pks.iter().any(|p| p.name == f.name))
        .collect();
    if set_cols.is_empty() {
        out.push_str(pad);
        out.push_str(&format!(
            "panic!(\"update on `{}` has no non-PK columns to set\");\n",
            table
        ));
        return;
    }
    let mut sql = format!("UPDATE \"{}\" SET ", meta.table);
    for (i, f) in set_cols.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!(
            "\"{}\" = {}",
            f.name,
            placeholder_for(i + 1, f.pg),
        ));
    }
    sql.push_str(" WHERE ");
    for (i, p) in pks.iter().enumerate() {
        if i > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&format!(
            "\"{}\" = {}",
            p.name,
            placeholder_for(set_cols.len() + i + 1, p.pg),
        ));
    }

    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __var = &");
    out.push_str(&sanitize_ident(var));
    out.push_str(";\n");
    out.push_str(&inner);
    out.push_str("let __params: DbParams = vec![\n");
    for f in set_cols.iter().chain(pks.iter()) {
        out.push_str(&format!(
            "{inner}    {helper}(jwc_get_field(__var, \"{name}\")),\n",
            inner = inner,
            helper = helper_for_kind(f.pg),
            name = f.name,
        ));
    }
    out.push_str(&inner);
    out.push_str("];\n");
    out.push_str(&inner);
    out.push_str(&format!(
        "let _ = jwc_db_exec(\"{}\", __params).await;\n",
        escape_sql(&sql)
    ));
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_db_delete_by_var(out: &mut String, pad: &str, var: &str, table: &str, ctx: &CodegenCtx) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!(
                "panic!(\"delete from unknown entity {}\");\n",
                table
            ));
            return;
        }
    };
    let pks = pk_fields(meta);
    let mut sql = format!("DELETE FROM \"{}\" WHERE ", meta.table);
    for (i, p) in pks.iter().enumerate() {
        if i > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&format!("\"{}\" = ${}", p.name, i + 1));
    }

    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __var = &");
    out.push_str(&sanitize_ident(var));
    out.push_str(";\n");
    out.push_str(&inner);
    out.push_str("let __params: DbParams = vec![\n");
    for p in &pks {
        out.push_str(&format!(
            "{inner}    {helper}(jwc_get_field(__var, \"{name}\")),\n",
            inner = inner,
            helper = helper_for_kind(p.pg),
            name = p.name,
        ));
    }
    out.push_str(&inner);
    out.push_str("];\n");
    out.push_str(&inner);
    out.push_str(&format!(
        "let _ = jwc_db_exec(\"{}\", __params).await;\n",
        escape_sql(&sql)
    ));
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_db_delete_where(
    out: &mut String,
    pad: &str,
    table: &str,
    where_clause: &crate::ast::WhereExpr,
    ctx: &CodegenCtx,
) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!(
                "panic!(\"delete from unknown entity {}\");\n",
                table
            ));
            return;
        }
    };
    let mut wb = WhereBuilder::new(meta);
    if let Err(e) = wb.emit(where_clause) {
        out.push_str(pad);
        out.push_str(&format!("compile_error!({:?});\n", e.to_string()));
        return;
    }
    let sql = format!("DELETE FROM \"{}\" WHERE {}", meta.table, wb.sql);

    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __params: DbParams = vec![\n");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{inner}    {}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),\n");
    }
    out.push_str(&inner);
    out.push_str("];\n");
    out.push_str(&inner);
    out.push_str(&format!(
        "let _ = jwc_db_exec(\"{}\", __params).await;\n",
        escape_sql(&sql)
    ));
    out.push_str(pad);
    out.push_str("}\n");
}

/// Codegen for `update CTX.Table set col1 = expr1, col2 = expr2 where ...;`
///
/// The hot pattern is `hits = hits + 1` (an atomic increment). Bare
/// identifiers that name a column on the entity stay inline as `"col"`
/// SQL; arithmetic ops over them recursively fold the same way. Anything
/// else falls back to a `$N` bind, matching the interpreter's
/// `build_set_rhs_sql` semantics. Without this rule the increment would
/// compile to a host read-then-write loop and race itself under load —
/// the exact production bug this feature exists to close.
fn emit_db_update_set(
    out: &mut String,
    pad: &str,
    table: &str,
    assignments: &[(String, Expr)],
    where_clause: &crate::ast::WhereExpr,
    ctx: &CodegenCtx,
) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!(
                "panic!(\"update set unknown entity {}\");\n",
                table
            ));
            return;
        }
    };
    let mut wb = WhereBuilder::new(meta);
    // Build SET clause first so parameter indices remain stable: WHERE
    // params follow the SET ones (Postgres `$N` is positional).
    let mut set_fragments: Vec<String> = Vec::with_capacity(assignments.len());
    for (col, rhs) in assignments {
        let col_lc = col.to_ascii_lowercase();
        // Validate the column exists on the entity, mirroring WhereBuilder.
        if !meta
            .fields
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case(&col_lc))
        {
            out.push_str(pad);
            out.push_str(&format!(
                "compile_error!({:?});\n",
                format!("unknown column `{}` on entity `{}`", col, meta.table)
            ));
            return;
        }
        let rhs_sql = match build_set_rhs_sql_native(rhs, &mut wb) {
            Ok(s) => s,
            Err(e) => {
                out.push_str(pad);
                out.push_str(&format!("compile_error!({:?});\n", e.to_string()));
                return;
            }
        };
        set_fragments.push(format!("\"{}\" = {}", col_lc, rhs_sql));
    }
    let set_clause = set_fragments.join(", ");
    if let Err(e) = wb.emit(where_clause) {
        out.push_str(pad);
        out.push_str(&format!("compile_error!({:?});\n", e.to_string()));
        return;
    }
    let sql = format!(
        "UPDATE \"{}\" SET {} WHERE {}",
        meta.table, set_clause, wb.sql
    );

    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __params: DbParams = vec![\n");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{inner}    {}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),\n");
    }
    out.push_str(&inner);
    out.push_str("];\n");
    out.push_str(&inner);
    out.push_str(&format!(
        "let _ = jwc_db_exec(\"{}\", __params).await;\n",
        escape_sql(&sql)
    ));
    out.push_str(pad);
    out.push_str("}\n");
}

/// Mirror of `runner::build_set_rhs_sql` for native codegen. Bare column
/// references stay inline (`"col"`); arithmetic ops recurse; everything
/// else lands on the `WhereBuilder`'s param list (the SQL `$N` is
/// allocated in the order params are pushed, and SET runs before WHERE).
fn build_set_rhs_sql_native<'a>(expr: &'a Expr, wb: &mut WhereBuilder<'a>) -> Result<String> {
    match expr {
        Expr::Var(name) => {
            let key = name.to_ascii_lowercase();
            // Treat the identifier as a column reference when the entity
            // has a matching field; otherwise it's a runtime variable and
            // we bind through the param list. Mirrors the interpreter's
            // "local vars in scope shadow column refs" rule.
            if let Some(f) = wb
                .entity
                .fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(&key))
            {
                return Ok(format!("\"{}\"", f.name));
            }
            wb.params.push((PgKind::Str, expr));
            Ok(format!("${}", wb.params.len()))
        }
        Expr::Add(a, b) => {
            let ls = build_set_rhs_sql_native(a, wb)?;
            let rs = build_set_rhs_sql_native(b, wb)?;
            Ok(format!("({} + {})", ls, rs))
        }
        Expr::Sub(a, b) => {
            let ls = build_set_rhs_sql_native(a, wb)?;
            let rs = build_set_rhs_sql_native(b, wb)?;
            Ok(format!("({} - {})", ls, rs))
        }
        Expr::Mul(a, b) => {
            let ls = build_set_rhs_sql_native(a, wb)?;
            let rs = build_set_rhs_sql_native(b, wb)?;
            Ok(format!("({} * {})", ls, rs))
        }
        Expr::Div(a, b) => {
            let ls = build_set_rhs_sql_native(a, wb)?;
            let rs = build_set_rhs_sql_native(b, wb)?;
            Ok(format!("({} / {})", ls, rs))
        }
        _ => {
            // Best-effort param kind: Int literals get Bigint, Float gets
            // Float, the rest get Str and rely on Postgres's text-coercion
            // path. The hot pattern (`col + 1`) hits the Int branch via
            // Add(Var, Int).
            let kind = match expr {
                Expr::Int(_) => PgKind::Bigint,
                Expr::Float(_) => PgKind::Float,
                Expr::Bool(_) => PgKind::Bool,
                _ => PgKind::Str,
            };
            wb.params.push((kind, expr));
            Ok(format!("${}", wb.params.len()))
        }
    }
}

fn escape_sql(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// --- WHERE clause builder ----------------------------------------------------

struct WhereBuilder<'a> {
    entity: &'a EntityMeta,
    sql: String,
    params: Vec<(PgKind, &'a Expr)>,
    /// Table-alias map (entity name → `t` / `j{i}`) for JOIN queries; `None`
    /// for single-table selects (columns stay unqualified, byte-identical to
    /// the historical output). Param TYPES are still resolved against the main
    /// entity, so a WHERE on a *joined* entity's column is not supported here.
    aliases: Option<HashMap<String, String>>,
}

impl<'a> WhereBuilder<'a> {
    fn new(entity: &'a EntityMeta) -> Self {
        WhereBuilder {
            entity,
            sql: String::new(),
            params: Vec::new(),
            aliases: None,
        }
    }

    fn new_joined(entity: &'a EntityMeta, aliases: HashMap<String, String>) -> Self {
        WhereBuilder {
            entity,
            sql: String::new(),
            params: Vec::new(),
            aliases: Some(aliases),
        }
    }

    /// SQL reference for a WHERE column — qualified by table alias when this is
    /// a JOIN query, bare otherwise. Shares the interpreter's `where_col_sql`.
    fn col_ref(&self, field: &str) -> String {
        crate::runner::sql::where_col_sql(field, self.aliases.as_ref())
    }

    fn col_kind(&self, raw_field: &str) -> Result<(String, PgKind)> {
        let col = match raw_field.split_once('.') {
            Some((_, c)) => c,
            None => raw_field,
        };
        let f = self
            .entity
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(col))
            .ok_or_else(|| anyhow!("unknown column `{}` on entity `{}`", col, self.entity.table))?;
        Ok((f.name.clone(), f.pg))
    }

    fn emit(&mut self, w: &'a crate::ast::WhereExpr) -> Result<()> {
        use crate::ast::WhereExpr;
        match w {
            WhereExpr::Atom(atom) => {
                // `col_kind` validates the column exists (on the main entity)
                // and gives the bind type; `col_ref` renders the SQL reference
                // (alias-qualified for JOIN queries).
                let (_, kind) = self.col_kind(&atom.field)?;
                let cref = self.col_ref(&atom.field);
                // `==?` / `like?` etc. — optional predicate: a NULL (or, for a
                // string column, empty) bound value drops the filter. The
                // interpreter omits the clause at runtime; native can't rebuild
                // SQL per-call, so it emits an equivalent guard that short-
                // circuits to TRUE for those values.
                let optional = atom.op.ends_with('?');
                let raw_op = atom.op.trim_end_matches('?');
                // is null / is not null sentinel from the parser: op `==`/`!=`
                // with rhs == Expr::Null (only meaningful for a non-optional
                // literal-null comparison).
                if !optional
                    && matches!(&atom.rhs, Expr::Null)
                    && (raw_op == "==" || raw_op == "!=")
                {
                    let neg = raw_op == "!=";
                    self.sql.push_str(&format!(
                        "{} IS {}NULL",
                        cref,
                        if neg { "NOT " } else { "" }
                    ));
                    return Ok(());
                }
                let op = match raw_op {
                    "==" | "=" => "=",
                    "!=" | "<>" => "<>",
                    "<" => "<",
                    "<=" => "<=",
                    ">" => ">",
                    ">=" => ">=",
                    "like" => "LIKE",
                    "ilike" => "ILIKE",
                    other => bail!(unsupported(&format!("WHERE operator `{}`", other))),
                };
                let n = self.params.len() + 1;
                self.params.push((kind, &atom.rhs));
                if optional {
                    // Repeated `$n` is fine — one bound param, referenced thrice.
                    if matches!(kind, PgKind::Str) {
                        self.sql
                            .push_str(&format!("(${n} IS NULL OR ${n} = '' OR {cref} {op} ${n})"));
                    } else {
                        self.sql
                            .push_str(&format!("(${n} IS NULL OR {cref} {op} ${n})"));
                    }
                } else {
                    self.sql.push_str(&format!("{} {} ${}", cref, op, n));
                }
                Ok(())
            }
            WhereExpr::And(l, r) => {
                self.sql.push('(');
                self.emit(l)?;
                self.sql.push_str(" AND ");
                self.emit(r)?;
                self.sql.push(')');
                Ok(())
            }
            WhereExpr::Or(l, r) => {
                self.sql.push('(');
                self.emit(l)?;
                self.sql.push_str(" OR ");
                self.emit(r)?;
                self.sql.push(')');
                Ok(())
            }
            WhereExpr::InList { field, values } => {
                let (_, kind) = self.col_kind(field)?;
                let cref = self.col_ref(field);
                if values.is_empty() {
                    // SQL `IN ()` is a parse error; `FALSE` makes the row count zero.
                    self.sql.push_str("FALSE");
                    return Ok(());
                }
                self.sql.push_str(&format!("{} IN (", cref));
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        self.sql.push_str(", ");
                    }
                    let n = self.params.len() + 1;
                    self.params.push((kind, v));
                    self.sql.push_str(&format!("${}", n));
                }
                self.sql.push(')');
                Ok(())
            }
            WhereExpr::Between { field, low, high } => {
                let (_, kind) = self.col_kind(field)?;
                let cref = self.col_ref(field);
                let nl = self.params.len() + 1;
                self.params.push((kind, low));
                let nh = self.params.len() + 1;
                self.params.push((kind, high));
                self.sql
                    .push_str(&format!("{} BETWEEN ${} AND ${}", cref, nl, nh));
                Ok(())
            }
        }
    }
}

fn emit_expr(out: &mut String, expr: &Expr, ctx: &CodegenCtx) {
    match expr {
        Expr::Int(n) => {
            out.push_str("V::Int(");
            out.push_str(&n.to_string());
            out.push_str("i64)");
        }
        Expr::Float(s) => {
            out.push_str("V::Float(");
            out.push_str(s);
            out.push_str("_f64)");
        }
        Expr::Str(s) => {
            // Phase A3: source-literal strings live as `Cow::Borrowed(&'static str)`
            // — zero per-request allocation, since the literal is baked into
            // the binary's .rodata.
            out.push_str("v_str(\"");
            push_str_escaped(out, s);
            out.push_str("\")");
        }
        Expr::Bool(b) => {
            out.push_str("V::Bool(");
            out.push_str(if *b { "true" } else { "false" });
            out.push(')');
        }
        Expr::Null => out.push_str("V::Null"),
        Expr::Var(name) => {
            if ctx.consts.contains(&name.to_lowercase()) {
                out.push_str("jwc_const_");
                out.push_str(&sanitize_ident(&name.to_lowercase()));
                out.push_str("()");
            } else {
                out.push_str(&sanitize_ident(name));
                out.push_str(".clone()");
            }
        }
        Expr::FieldGet { var, field } => {
            out.push_str("jwc_get_field(&");
            out.push_str(&sanitize_ident(var));
            out.push_str(", \"");
            push_str_escaped(out, field);
            out.push_str("\")");
        }
        Expr::NewEntity { .. } => {
            out.push_str("v_obj(JwcObj::default())");
        }
        Expr::ObjectLit(pairs) => {
            // **Phase 1 [1.0-blocker]** — typed-shape fast path. The AST
            // guarantees keys are static strings (no computed keys at this
            // layer), so every literal has a compile-time-known field-name
            // list and can take `v_record`. Shapes are interned in
            // `CodegenCtx::shapes` so identical key sets across the whole
            // program share ONE `__jwc_shape_N()` getter — the bench app's
            // /json-large route (1000 `{ id, name, value }` constructions
            // per request) collapses to one shape allocation.
            let keys: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
            let idx = ctx.intern_shape(keys);
            out.push_str(&format!(
                "v_record(::std::sync::Arc::clone(__jwc_shape_{}()), vec![",
                idx
            ));
            for (i, (_, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(out, v, ctx);
            }
            out.push_str("])");
        }
        Expr::ArrayLit(items) => {
            out.push_str("v_arr(vec![");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(out, item, ctx);
            }
            out.push_str("])");
        }
        Expr::Add(a, b) => emit_binop(out, "jwc_add", a, b, ctx),
        Expr::Sub(a, b) => emit_binop(out, "jwc_sub", a, b, ctx),
        Expr::Mul(a, b) => emit_binop(out, "jwc_mul", a, b, ctx),
        Expr::Div(a, b) => emit_binop(out, "jwc_div", a, b, ctx),
        Expr::Mod(a, b) => emit_binop(out, "jwc_mod", a, b, ctx),
        Expr::Neg(e) => {
            out.push_str("jwc_neg(");
            emit_expr(out, e, ctx);
            out.push(')');
        }
        Expr::Eq(a, b) => emit_cmp(out, "jwc_eq", a, b, ctx),
        Expr::Neq(a, b) => {
            out.push_str("V::Bool(!jwc_eq(&");
            emit_expr(out, a, ctx);
            out.push_str(", &");
            emit_expr(out, b, ctx);
            out.push_str("))");
        }
        Expr::Lt(a, b) => emit_cmp(out, "jwc_lt", a, b, ctx),
        Expr::Lte(a, b) => emit_cmp(out, "jwc_lte", a, b, ctx),
        Expr::Gt(a, b) => emit_cmp(out, "jwc_gt", a, b, ctx),
        Expr::Gte(a, b) => emit_cmp(out, "jwc_gte", a, b, ctx),
        Expr::And(a, b) => {
            out.push_str("{ let __a = ");
            emit_expr(out, a, ctx);
            out.push_str("; if !jwc_truthy(&__a) { __a } else { ");
            emit_expr(out, b, ctx);
            out.push_str(" } }");
        }
        Expr::Or(a, b) => {
            out.push_str("{ let __a = ");
            emit_expr(out, a, ctx);
            out.push_str("; if jwc_truthy(&__a) { __a } else { ");
            emit_expr(out, b, ctx);
            out.push_str(" } }");
        }
        Expr::Not(e) => {
            out.push_str("V::Bool(!jwc_truthy(&");
            emit_expr(out, e, ctx);
            out.push_str("))");
        }
        // Both short-circuit: the untaken branch is never evaluated, so
        // `x ?? expensive()` and `c ? a : b` cost only what they use.
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            out.push_str("{ if jwc_truthy(&");
            emit_expr(out, cond, ctx);
            out.push_str(") { ");
            emit_expr(out, then_expr, ctx);
            out.push_str(" } else { ");
            emit_expr(out, else_expr, ctx);
            out.push_str(" } }");
        }
        Expr::Coalesce(a, b) => {
            out.push_str("{ let __a = ");
            emit_expr(out, a, ctx);
            out.push_str("; if matches!(__a, V::Null) { ");
            emit_expr(out, b, ctx);
            out.push_str(" } else { __a } }");
        }
        Expr::Call { name, args } => {
            if name == "print" {
                // `print(...)` is a statement-shaped builtin; when used as an
                // expression we still run the side effect and yield V::Null.
                out.push_str("{ jwc_print(");
                match args.first() {
                    Some(a) => emit_expr(out, a, ctx),
                    None => out.push_str("V::Null"),
                }
                out.push_str("); V::Null }");
                return;
            }
            if name == "serve" {
                // `serve(port)` lowers to the async route-registering wrapper.
                out.push_str("{ jwc_serve_impl(jwc_to_u16(");
                if let Some(a) = args.first() {
                    emit_expr(out, a, ctx);
                } else {
                    out.push_str("V::Int(8080)");
                }
                out.push_str(")).await; V::Null }");
                return;
            }
            if name == "query_param" {
                // Variadic in JWC (`query_param(name)` / `query_param(name, default)`);
                // the helper has a fixed 2-arg signature, so pad with V::Null.
                out.push_str("jwc_b_query_param(");
                match args.as_slice() {
                    [a] => {
                        emit_expr(out, a, ctx);
                        out.push_str(", V::Null");
                    }
                    [a, b] => {
                        emit_expr(out, a, ctx);
                        out.push_str(", ");
                        emit_expr(out, b, ctx);
                    }
                    _ => out.push_str("V::Null, V::Null"),
                }
                out.push(')');
                return;
            }
            if name == "raw_sql" {
                // Variadic in JWC (`raw_sql(sql)` / `raw_sql(sql, params_json)`).
                // The prelude impl is async with a fixed 2-arg signature.
                // Default-pad the missing JSON params array to "[]".
                out.push_str("jwc_b_raw_sql(");
                match args.as_slice() {
                    [a] => {
                        emit_expr(out, a, ctx);
                        out.push_str(", v_str(\"[]\")");
                    }
                    [a, b] => {
                        emit_expr(out, a, ctx);
                        out.push_str(", ");
                        emit_expr(out, b, ctx);
                    }
                    _ => out.push_str("V::Null, v_str(\"[]\")"),
                }
                out.push_str(").await");
                return;
            }
            if name == "range" {
                // Variadic in JWC (`range(n)` / `range(start, end)` /
                // `range(start, end, step)`); pad to the prelude's fixed
                // (start, end, step) signature.
                out.push_str("jwc_b_range(");
                match args.as_slice() {
                    [n] => {
                        out.push_str("V::Int(0), ");
                        emit_expr(out, n, ctx);
                        out.push_str(", V::Int(1)");
                    }
                    [s, e] => {
                        emit_expr(out, s, ctx);
                        out.push_str(", ");
                        emit_expr(out, e, ctx);
                        out.push_str(", V::Int(1)");
                    }
                    [s, e, st] => {
                        emit_expr(out, s, ctx);
                        out.push_str(", ");
                        emit_expr(out, e, ctx);
                        out.push_str(", ");
                        emit_expr(out, st, ctx);
                    }
                    _ => out.push_str("V::Int(0), V::Int(0), V::Int(1)"),
                }
                out.push(')');
                return;
            }
            if name == "push" || name == "append" {
                // Mutating: append to the array variable in place, then yield
                // the array. Requires the first arg to be a plain variable.
                match args.as_slice() {
                    [Expr::Var(v), elem] => {
                        out.push_str("{ jwc_push(&mut ");
                        out.push_str(&sanitize_ident(v));
                        out.push_str(", ");
                        emit_expr(out, elem, ctx);
                        out.push_str("); ");
                        out.push_str(&sanitize_ident(v));
                        out.push_str(".clone() }");
                    }
                    _ => out.push_str(
                        "compile_error!(\"push(arr, x): first arg must be an array variable\")",
                    ),
                }
                return;
            }
            let is_user = ctx.funcs.contains(name);
            // Async builtins implemented in the prelude as `async fn jwc_b_*`.
            // `raw_sql` is also async but goes through its own variadic codegen
            // branch above, so it doesn't need to be listed here.
            let is_async_builtin = matches!(
                name.as_str(),
                "sleep_ms"
                    | "http_get"
                    | "fetch_json"
                    | "setConnectionString"
                    | "ws_send"
                    | "ws_recv"
                    | "ws_close"
            );
            if is_user {
                out.push_str(&user_fn_name(name));
            } else {
                out.push_str(&builtin_fn_name(name));
            }
            // HTTP response helpers in the prelude all take one V; the
            // interpreter accepts 0-arg calls (`notFound()`, `noContent()`),
            // so pad with V::Null when the user omitted the body.
            let pads_to_one = !is_user
                && args.is_empty()
                && matches!(
                    name.as_str(),
                    "not_found"
                        | "notFound"
                        | "no_content"
                        | "noContent"
                        | "ok"
                        | "created"
                        | "unauthorized"
                        | "forbidden"
                        | "internal_error"
                        | "internalError"
                        | "bad_request"
                        | "badRequest"
                );
            out.push('(');
            if pads_to_one {
                out.push_str("V::Null");
            } else {
                let mut first = true;
                for a in args {
                    if !first {
                        out.push_str(", ");
                    }
                    first = false;
                    emit_expr(out, a, ctx);
                }
            }
            out.push(')');
            if is_user || is_async_builtin {
                out.push_str(".await");
            }
        }
        Expr::DbSelect {
            table,
            first,
            where_clause,
            order_by,
            limit,
            offset,
            with_relations,
            projection,
            aggregates,
            aliased_cols,
            joins,
            group_by,
            having,
            entity: _,
            context_var: _,
        } => {
            if aggregates.is_empty() && joins.is_empty() && aliased_cols.is_empty() {
                // Plain select / `with` eager-load.
                emit_db_select(
                    out,
                    table,
                    *first,
                    where_clause.as_deref(),
                    order_by.as_ref(),
                    limit.as_deref(),
                    offset.as_deref(),
                    with_relations,
                    projection,
                    ctx,
                );
            } else {
                // Grouped aggregation / explicit JOIN / aliased columns.
                emit_db_select_explicit(
                    out,
                    table,
                    *first,
                    where_clause.as_deref(),
                    order_by.as_ref(),
                    limit.as_deref(),
                    offset.as_deref(),
                    projection,
                    aggregates,
                    aliased_cols,
                    joins,
                    group_by,
                    having.as_deref(),
                    ctx,
                );
            }
        }
        Expr::DbCount {
            table,
            where_clause,
            ..
        } => {
            emit_db_count(out, table, where_clause.as_deref(), ctx);
        }
        Expr::DbAggregate {
            kind,
            field,
            table,
            where_clause,
            ..
        } => {
            emit_db_aggregate(out, *kind, field, table, where_clause.as_deref(), ctx);
        }
        Expr::Await(inner) => {
            // `await expr` — inner is already an async call that emits `.await`
            // via the Call arm above. Pass through; the `.await` is on the call,
            // not on `await` itself (Rust's `expr.await` is the right shape).
            emit_expr(out, inner, ctx);
        }
    }
}

fn emit_db_select(
    out: &mut String,
    table: &str,
    first: bool,
    where_clause: Option<&crate::ast::WhereExpr>,
    order_by: Option<&crate::ast::DbOrderBy>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    with_relations: &[String],
    projection: &[String],
    ctx: &CodegenCtx,
) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(&format!("panic!(\"select on unknown entity {}\")", table));
            return;
        }
    };

    // `with` eager-load goes through the shared json-subquery path (identical
    // SQL to the interpreter): one query, nav collections materialised as
    // nested JSON. Handles every nav kind + projection + ordering + nesting.
    if !with_relations.is_empty() {
        emit_db_select_with_navs(
            out,
            meta,
            table,
            first,
            where_clause,
            order_by,
            limit,
            offset,
            with_relations,
            projection,
            ctx,
        );
        return;
    }

    // Resolve projection columns up front so unknown names surface as a clear
    // codegen error rather than a Postgres runtime error.
    let select_clause: String = if projection.is_empty() {
        "*".to_string()
    } else {
        let mut parts = Vec::with_capacity(projection.len());
        for c in projection {
            match meta.fields.iter().find(|f| f.name.eq_ignore_ascii_case(c)) {
                Some(f) => parts.push(format!("\"{}\"", f.name)),
                None => {
                    out.push_str(&format!(
                        "{{ compile_error!({:?}); V::Null }}",
                        format!(
                            "unknown projection column `{}` on entity `{}`",
                            c, meta.table
                        ),
                    ));
                    return;
                }
            }
        }
        parts.join(", ")
    };

    let mut sql = format!("SELECT {} FROM \"{}\"", select_clause, meta.table);
    let mut wb = WhereBuilder::new(meta);
    if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
        sql.push_str(" WHERE ");
        sql.push_str(&wb.sql);
    }
    if let Some(ord) = order_by {
        let col = match ord.field.split_once('.') {
            Some((_, c)) => c,
            None => ord.field.as_str(),
        };
        let dir = match ord.dir {
            crate::ast::SortDir::Asc => "ASC",
            crate::ast::SortDir::Desc => "DESC",
        };
        sql.push_str(&format!(" ORDER BY \"{}\" {}", col, dir));
    }
    if first {
        sql.push_str(" LIMIT 1");
    } else if let Some(l) = limit {
        match l {
            Expr::Int(n) => sql.push_str(&format!(" LIMIT {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                // Postgres binds LIMIT / OFFSET as bigint — emit an i64 helper
                // so the boxed param matches the prepared statement type.
                wb.params.push((PgKind::Bigint, l));
                sql.push_str(&format!(" LIMIT ${}", n));
            }
        }
    }
    if let Some(o) = offset {
        match o {
            Expr::Int(n) => sql.push_str(&format!(" OFFSET {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                wb.params.push((PgKind::Bigint, o));
                sql.push_str(&format!(" OFFSET ${}", n));
            }
        }
    }

    // Phase 1 monomorphization wiring. "Simple" selects — no projection,
    // no `with` relations, full-row SELECT * — read rows straight into
    // the concrete `JwcEnt_<Name>` struct, then lift to `V` via the
    // emitted `jwc_to_v` method. Skips the dynamic per-row `jwc_row_to_v`
    // FxHashMap construction. The downstream wrap (`v_arr`,
    // `into_iter().next()`) keeps its existing shape.
    //
    // Complex selects (projection subset or `with` eager-load) still go
    // through the dynamic `jwc_db_query` → `Vec<V>` path because the
    // monomorphized struct has a fixed shape that doesn't match a
    // partial projection, and eager-load injects extra columns into the
    // map. That switchover is a follow-up.
    let typed_read = projection.is_empty() && with_relations.is_empty();

    out.push('{');
    out.push_str(" let __params: DbParams = vec![");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),");
    }
    out.push_str("];");
    if typed_read {
        // Phase 1.6 — full read+write monomorphization. Each row's
        // concrete struct goes straight into the V::Array, but each
        // element is a `V::RawJson` already-serialised fragment that
        // jwc_write_json writes verbatim. No FxHashMap allocation, no
        // V::Object roundtrip on either side. Closes the /json-large
        // RPS gap PRODUCTION_READINESS_PLAN.md flags as the headline
        // Phase 1 1.0-blocker.
        out.push_str(&format!(
            " let __raw = jwc_db_query_rows(\"{}\", __params).await; \
             let mut __rows: Vec<V> = __raw.iter().map(|r| {{ \
                let mut __json_buf = String::with_capacity(128); \
                JwcEnt_{ent}::jwc_from_row(r).jwc_write_json(&mut __json_buf); \
                V::RawJson(JwcStr::from(__json_buf)) \
             }}).collect();",
            escape_sql(&sql),
            ent = table,
        ));
    } else {
        out.push_str(&format!(
            " let mut __rows = jwc_db_query(\"{}\", __params).await;",
            escape_sql(&sql)
        ));
    }

    if first {
        out.push_str(" __rows.into_iter().next().unwrap_or(V::Null) }");
    } else {
        out.push_str(" v_arr(__rows) }");
    }
}

/// `select Entity [ { cols } ] with rel[, rel.child ...] from ...` — eager-load
/// path. Reuses the interpreter's navigation SQL builder so the emitted query
/// is byte-identical to `jwc run`/`serve`: one statement, each nav collection
/// materialised as a correlated `json_agg` subquery (belongs-to / m2m /
/// projected / ordered / nested all handled there). The whole result row is
/// wrapped in `row_to_json(r)::text` (or `json_agg(...)::text` for a list) and
/// read back through `jwc_db_query_json`. Nav subqueries carry no bind
/// parameters, so the only params are the outer WHERE/LIMIT/OFFSET ones the
/// `WhereBuilder` already handles.
#[allow(clippy::too_many_arguments)]
fn emit_db_select_with_navs(
    out: &mut String,
    meta: &EntityMeta,
    entity: &str,
    first: bool,
    where_clause: Option<&crate::ast::WhereExpr>,
    order_by: Option<&crate::ast::DbOrderBy>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    with_relations: &[String],
    projection: &[String],
    ctx: &CodegenCtx,
) {
    let navs = match crate::runner::sql::build_navigation_subqueries(
        entity,
        &meta.table,
        with_relations,
        ctx.nav_models,
        ctx.pk_by_table,
    ) {
        Ok(n) => n,
        Err(e) => {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
    };

    // Inner SELECT list: projected columns (or `t.*`) followed by one
    // correlated json subquery per requested navigation.
    let mut cols: Vec<String> = Vec::new();
    if projection.is_empty() {
        cols.push("t.*".to_string());
    } else {
        for c in projection {
            match meta.fields.iter().find(|f| f.name.eq_ignore_ascii_case(c)) {
                Some(f) => cols.push(format!("\"{}\"", f.name)),
                None => {
                    out.push_str(&format!(
                        "{{ compile_error!({:?}); V::Null }}",
                        format!(
                            "unknown projection column `{}` on entity `{}`",
                            c, meta.table
                        ),
                    ));
                    return;
                }
            }
        }
    }
    for nav in &navs {
        cols.push(format!(
            "{} AS \"{}\"",
            nav.sql_fragment("t", 0),
            nav.alias()
        ));
    }

    let mut inner = format!("SELECT {} FROM \"{}\" t", cols.join(", "), meta.table);
    let mut wb = WhereBuilder::new(meta);
    if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
        inner.push_str(" WHERE ");
        inner.push_str(&wb.sql);
    }
    if let Some(ord) = order_by {
        let col = match ord.field.split_once('.') {
            Some((_, c)) => c,
            None => ord.field.as_str(),
        };
        let dir = match ord.dir {
            crate::ast::SortDir::Asc => "ASC",
            crate::ast::SortDir::Desc => "DESC",
        };
        inner.push_str(&format!(" ORDER BY \"{}\" {}", col, dir));
    }
    if first {
        inner.push_str(" LIMIT 1");
    } else if let Some(l) = limit {
        match l {
            Expr::Int(n) => inner.push_str(&format!(" LIMIT {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                wb.params.push((PgKind::Bigint, l));
                inner.push_str(&format!(" LIMIT ${}", n));
            }
        }
    }
    if let Some(o) = offset {
        match o {
            Expr::Int(n) => inner.push_str(&format!(" OFFSET {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                wb.params.push((PgKind::Bigint, o));
                inner.push_str(&format!(" OFFSET ${}", n));
            }
        }
    }

    // Wrap so the whole row (with nested nav JSON) comes back as one text
    // column — same shape the interpreter reads.
    let sql = if first {
        format!("SELECT row_to_json(r)::text FROM ({}) r", inner)
    } else {
        format!(
            "SELECT COALESCE(json_agg(row_to_json(r)), '[]')::text FROM ({}) r",
            inner
        )
    };

    out.push('{');
    out.push_str(" let __params: DbParams = vec![");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),");
    }
    out.push_str("];");
    out.push_str(&format!(
        " jwc_db_query_json(\"{}\", __params).await }}",
        escape_sql(&sql)
    ));
}

/// `select Entity { col, alias: count(*), alias2: Other.col } [join ...]
/// [group by ...] [having ...]` — grouped aggregation / explicit JOIN /
/// aliased columns. Mirrors the interpreter's `build_select_sql`: a `t` alias
/// for the main table, `j{i}` per join, columns qualified through the shared
/// `where_col_sql` / `agg_select_sql`, the whole row wrapped
/// `row_to_json(r)::text` and read via `jwc_db_query_json`. WHERE/HAVING params
/// come from the alias-aware `WhereBuilder` (so the bind types resolve against
/// the main entity — a WHERE on a joined entity's column is not supported here).
#[allow(clippy::too_many_arguments)]
fn emit_db_select_explicit(
    out: &mut String,
    table: &str,
    first: bool,
    where_clause: Option<&crate::ast::WhereExpr>,
    order_by: Option<&crate::ast::DbOrderBy>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    projection: &[String],
    aggregates: &[crate::ast::AggProj],
    aliased_cols: &[crate::ast::AliasedCol],
    joins: &[crate::ast::JoinClause],
    group_by: &[String],
    having: Option<&crate::ast::WhereExpr>,
    ctx: &CodegenCtx,
) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(&format!("panic!(\"select on unknown entity {}\")", table));
            return;
        }
    };

    // Alias map: main → t, each joined entity → j{i}. None when there are no
    // joins (columns stay unqualified, matching the single-table path).
    let mut aliases: HashMap<String, String> = HashMap::new();
    aliases.insert(table.to_lowercase(), "t".to_string());
    for (i, j) in joins.iter().enumerate() {
        if !ctx.entities.contains_key(&j.entity) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                format!("join references unknown entity `{}`", j.entity),
            ));
            return;
        }
        aliases.insert(j.entity.to_lowercase(), format!("j{}", i));
    }
    let has_joins = !joins.is_empty();
    let alias_opt = if has_joins { Some(&aliases) } else { None };

    // SELECT list: projected cols (main table), aliased columns from any
    // entity, then the aggregate terms.
    let mut bits: Vec<String> = Vec::new();
    for c in projection {
        match meta.fields.iter().find(|f| f.name.eq_ignore_ascii_case(c)) {
            Some(f) => bits.push(format!("t.\"{}\"", f.name)),
            None => {
                out.push_str(&format!(
                    "{{ compile_error!({:?}); V::Null }}",
                    format!(
                        "unknown projection column `{}` on entity `{}`",
                        c, meta.table
                    ),
                ));
                return;
            }
        }
    }
    for ac in aliased_cols {
        bits.push(format!(
            "{} AS \"{}\"",
            crate::runner::sql::where_col_sql(&ac.field, alias_opt),
            ac.alias
        ));
    }
    for agg in aggregates {
        // count(*) needs no column; other aggregates must name a real column on
        // the main entity (joined-entity aggregation is interpreter-only).
        if !matches!(agg.kind, AggregateKind::Count) {
            let col = match agg.col.split_once('.') {
                Some((_, c)) => c,
                None => agg.col.as_str(),
            };
            if !has_joins && !meta.fields.iter().any(|f| f.name.eq_ignore_ascii_case(col)) {
                out.push_str(&format!(
                    "{{ compile_error!({:?}); V::Null }}",
                    format!(
                        "unknown aggregate column `{}` on entity `{}`",
                        col, meta.table
                    ),
                ));
                return;
            }
        }
        bits.push(crate::runner::sql::agg_select_sql(agg, alias_opt));
    }
    let select = if bits.is_empty() {
        "*".to_string()
    } else {
        bits.join(", ")
    };

    // FROM + JOINs.
    let mut from = format!("\"{}\" t", meta.table);
    for (i, j) in joins.iter().enumerate() {
        let jt = crate::sql::to_snake_case(&j.entity);
        from.push_str(&format!(
            " JOIN \"{}\" j{} ON {} = {}",
            jt,
            i,
            crate::runner::sql::where_col_sql(&j.left, alias_opt),
            crate::runner::sql::where_col_sql(&j.right, alias_opt),
        ));
    }

    // WHERE + HAVING share one param counter (where params first, then
    // having), matching the interpreter's bind order.
    let mut wb = if has_joins {
        WhereBuilder::new_joined(meta, aliases.clone())
    } else {
        WhereBuilder::new(meta)
    };
    let where_sql = if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
        std::mem::take(&mut wb.sql)
    } else {
        String::new()
    };
    let group_sql = if group_by.is_empty() {
        String::new()
    } else {
        let cols: Vec<String> = group_by
            .iter()
            .map(|f| crate::runner::sql::where_col_sql(f, alias_opt))
            .collect();
        format!(" GROUP BY {}", cols.join(", "))
    };
    let having_sql = if let Some(hv) = having {
        if let Err(e) = wb.emit(hv) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
        format!(" HAVING {}", std::mem::take(&mut wb.sql))
    } else {
        String::new()
    };
    let order_sql = if let Some(ob) = order_by {
        let dir = match ob.dir {
            crate::ast::SortDir::Asc => "ASC",
            crate::ast::SortDir::Desc => "DESC",
        };
        format!(
            " ORDER BY {} {}",
            crate::runner::sql::where_col_sql(&ob.field, alias_opt),
            dir
        )
    } else {
        String::new()
    };
    let mut limit_sql = String::new();
    if let Some(l) = limit {
        match l {
            Expr::Int(n) => limit_sql.push_str(&format!(" LIMIT {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                wb.params.push((PgKind::Bigint, l));
                limit_sql.push_str(&format!(" LIMIT ${}", n));
            }
        }
    } else if first {
        limit_sql.push_str(" LIMIT 1");
    }
    if let Some(o) = offset {
        match o {
            Expr::Int(n) => limit_sql.push_str(&format!(" OFFSET {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                wb.params.push((PgKind::Bigint, o));
                limit_sql.push_str(&format!(" OFFSET ${}", n));
            }
        }
    }

    let where_clause_sql = if where_sql.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_sql)
    };
    let inner = format!(
        "SELECT {} FROM {}{}{}{}{}{}",
        select, from, where_clause_sql, group_sql, having_sql, order_sql, limit_sql
    );
    let sql = if first {
        format!("SELECT row_to_json(r)::text FROM ({}) r", inner)
    } else {
        format!(
            "SELECT COALESCE(json_agg(row_to_json(r)), '[]')::text FROM ({}) r",
            inner
        )
    };

    out.push('{');
    out.push_str(" let __params: DbParams = vec![");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),");
    }
    out.push_str("];");
    out.push_str(&format!(
        " jwc_db_query_json(\"{}\", __params).await }}",
        escape_sql(&sql)
    ));
}

fn emit_db_aggregate(
    out: &mut String,
    kind: AggregateKind,
    field: &str,
    table: &str,
    where_clause: Option<&crate::ast::WhereExpr>,
    ctx: &CodegenCtx,
) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(&format!(
                "panic!(\"aggregate on unknown entity {}\")",
                table
            ));
            return;
        }
    };
    let col_name = match field.split_once('.') {
        Some((_, c)) => c,
        None => field,
    };
    let f = match meta
        .fields
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(col_name))
    {
        Some(f) => f,
        None => {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                format!("unknown column `{}` on entity `{}`", col_name, meta.table),
            ));
            return;
        }
    };
    let op = match kind {
        AggregateKind::Sum => "sum",
        AggregateKind::Avg => "avg",
        AggregateKind::Min => "min",
        AggregateKind::Max => "max",
        AggregateKind::Count => "count",
    };
    let mut sql = format!(
        "SELECT {}(\"{}\") AS __agg FROM \"{}\"",
        op, f.name, meta.table,
    );
    let mut wb = WhereBuilder::new(meta);
    if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
        sql.push_str(" WHERE ");
        sql.push_str(&wb.sql);
    }
    out.push('{');
    out.push_str(" let __params: DbParams = vec![");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),");
    }
    out.push_str("];");
    out.push_str(&format!(
        " let __r = jwc_db_query(\"{}\", __params).await; match __r.into_iter().next() {{ Some(V::Object(m)) => m.get(\"__agg\").cloned().unwrap_or(V::Null), _ => V::Null }} }}",
        escape_sql(&sql),
    ));
}

fn emit_db_count(
    out: &mut String,
    table: &str,
    where_clause: Option<&crate::ast::WhereExpr>,
    ctx: &CodegenCtx,
) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(&format!("panic!(\"count on unknown entity {}\")", table));
            return;
        }
    };
    let mut sql = format!("SELECT count(*) FROM \"{}\"", meta.table);
    let mut wb = WhereBuilder::new(meta);
    if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                e.to_string()
            ));
            return;
        }
        sql.push_str(" WHERE ");
        sql.push_str(&wb.sql);
    }

    out.push('{');
    out.push_str(" let __params: DbParams = vec![");
    for (kind, expr) in &wb.params {
        out.push_str(&format!("{}(", helper_for_kind(*kind)));
        emit_expr(out, expr, ctx);
        out.push_str("),");
    }
    out.push_str("];");
    out.push_str(&format!(
        " let __r = jwc_db_query(\"{}\", __params).await; match __r.into_iter().next() {{ Some(V::Object(m)) => m.get(\"count\").cloned().unwrap_or(V::Int(0)), _ => V::Int(0) }} }}",
        escape_sql(&sql)
    ));
}

fn emit_binop(out: &mut String, op: &str, a: &Expr, b: &Expr, ctx: &CodegenCtx) {
    out.push_str(op);
    out.push('(');
    emit_expr(out, a, ctx);
    out.push_str(", ");
    emit_expr(out, b, ctx);
    out.push(')');
}

fn emit_cmp(out: &mut String, op: &str, a: &Expr, b: &Expr, ctx: &CodegenCtx) {
    out.push_str("V::Bool(");
    out.push_str(op);
    out.push_str("(&");
    emit_expr(out, a, ctx);
    out.push_str(", &");
    emit_expr(out, b, ctx);
    out.push_str("))");
}

fn push_str_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
}

/// Emit a module-level const as a zero-arg function returning its value.
/// Const bodies are pure (enforced by `validate_program`), so calling the
/// function at each use site is equivalent to inlining the literal.
fn emit_const_fn(out: &mut String, c: &ConstDecl, ctx: &CodegenCtx) {
    out.push_str("fn jwc_const_");
    out.push_str(&sanitize_ident(&c.name.to_lowercase()));
    out.push_str("() -> V {\n    ");
    emit_expr(out, &c.expr, ctx);
    out.push_str("\n}\n");
}

fn sanitize_ident(name: &str) -> String {
    if matches!(
        name,
        "fn" | "let"
            | "mut"
            | "if"
            | "else"
            | "while"
            | "for"
            | "in"
            | "loop"
            | "break"
            | "continue"
            | "return"
            | "match"
            | "struct"
            | "enum"
            | "impl"
            | "trait"
            | "use"
            | "mod"
            | "pub"
            | "crate"
            | "self"
            | "Self"
            | "super"
            | "where"
            | "as"
            | "type"
            | "const"
            | "static"
            | "ref"
            | "move"
            | "dyn"
            | "async"
            | "await"
            | "true"
            | "false"
            | "box"
            | "abstract"
            | "become"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
            | "union"
    ) || name.starts_with("jwc_")
        || name.starts_with("user_")
        || name == "V"
    {
        format!("var_{name}")
    } else {
        name.to_string()
    }
}

// --- Toolchain ----------------------------------------------------------------

fn find_cargo() -> Result<PathBuf> {
    if let Ok(path) = which_cargo() {
        return Ok(path);
    }
    if let Some(home) = dirs_home() {
        let candidate = home
            .join(".jwc")
            .join("toolchain")
            .join("bin")
            .join(cargo_exe_name());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("cargo not found on PATH"))
}

fn which_cargo() -> Result<PathBuf> {
    let name = cargo_exe_name();
    let path_var = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH unset"))?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("cargo not in PATH"))
}

fn cargo_exe_name() -> &'static str {
    if cfg!(windows) {
        "cargo.exe"
    } else {
        "cargo"
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

// --- Workspace scaffolding ----------------------------------------------------

fn scaffold_workspace(
    root: &Path,
    app_name: &str,
    rust_src: &str,
    needs_db: bool,
    needs_http_client: bool,
    needs_crypto: bool,
) -> Result<PathBuf> {
    let workspace = root.join(BUILD_DIR_NAME);
    let src_dir = workspace.join("src");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("Failed to create {}", src_dir.display()))?;

    let cargo_toml = workspace.join("Cargo.toml");
    std::fs::write(
        &cargo_toml,
        render_cargo_toml(app_name, needs_db, needs_http_client, needs_crypto),
    )
    .with_context(|| format!("Failed to write {}", cargo_toml.display()))?;

    let main_rs = src_dir.join("main.rs");
    std::fs::write(&main_rs, rust_src)
        .with_context(|| format!("Failed to write {}", main_rs.display()))?;

    let gitignore = workspace.join(".gitignore");
    if !gitignore.is_file() {
        let _ = std::fs::write(&gitignore, "target/\n");
    }

    Ok(workspace)
}

fn render_cargo_toml(
    app_name: &str,
    needs_db: bool,
    needs_http_client: bool,
    needs_crypto: bool,
) -> String {
    // reqwest is always included because the prelude contains the http_get /
    // fetch_json helpers unconditionally — gating them by feature would require
    // splitting the prelude. `needs_http_client` is kept for future use.
    //
    // TODO(phase-10.6): the manifest is currently target-agnostic. Future
    // iterations may want to toggle reqwest's TLS backend per target
    // (e.g. `rustls-tls` for musl/static builds, `native-tls` for the
    // host glibc target) and possibly switch `panic = "abort"` on for
    // size-sensitive cross builds. Keep this single shared manifest as
    // long as the matrix is small — multiplying it per target is an
    // anti-feature.
    let _ = needs_http_client;
    let mut deps = String::new();
    deps.push_str("tokio = { version = \"1\", features = [\"full\"] }\n");
    deps.push_str("futures = \"0.3\"\n");
    deps.push_str("axum = { version = \"0.7\", features = [\"http2\"] }\n");
    deps.push_str(
        "reqwest = { version = \"0.12\", default-features = false, features = [\"rustls-tls\", \"json\"] }\n",
    );
    // `uuid()` and `now()` are always-on built-ins; ship the supporting
    // crates unconditionally. Combined wire weight is ~50 KB stripped, far
    // below the noise floor of axum + reqwest + tokio.
    deps.push_str("uuid = { version = \"1\", features = [\"v4\"] }\n");
    deps.push_str("chrono = { version = \"0.4\", default-features = false, features = [\"clock\", \"std\"] }\n");
    // Hot-path V::Object payload is an FxHashMap (Phase A1 of PERF_PLAN.md):
    // O(1) lookup + fxhash, replacing BTreeMap's O(log n) + node alloc cost.
    deps.push_str("rustc-hash = \"2\"\n");
    // The prelude uses these unconditionally — `serde_json` for JSON body
    // validation / `json_*` builtins, `url` for the SSRF host-allowlist parse
    // in `http_get`. They must be direct deps or every native build fails
    // with `E0433: unresolved module or unlinked crate`.
    deps.push_str("serde_json = \"1\"\n");
    deps.push_str("url = \"2\"\n");
    if needs_db {
        // `with-chrono-0_4` plugs `chrono::DateTime` into tokio-postgres'
        // ToSql/FromSql so `jwc_param_timestamp` can bind directly to
        // `TIMESTAMPTZ` columns. Without the feature, the generated code
        // fails to compile with `the trait bound DateTime<Utc>: ToSql is
        // not satisfied`, and the only workaround (binding String + a
        // `$N::timestamptz` cast) trips a client-side WrongType check.
        deps.push_str("tokio-postgres = { version = \"0.7\", features = [\"with-chrono-0_4\"] }\n");
        deps.push_str("deadpool-postgres = \"0.14\"\n");
        // `db-postgres` plugs `Decimal` into tokio-postgres' ToSql/FromSql so
        // `jwc_param_numeric` can bind NUMERIC columns. Without it there is no
        // conversion for numeric in either direction.
        deps.push_str(
            "rust_decimal = { version = \"1\", default-features = false, features = [\"db-postgres\"] }\n",
        );
    }
    if needs_crypto {
        deps.push_str("sha2 = \"0.10\"\n");
        deps.push_str("sha1 = \"0.10\"\n");
        deps.push_str("md-5 = \"0.10\"\n");
        deps.push_str("hmac = \"0.12\"\n");
        deps.push_str("argon2 = { version = \"0.5\", features = [\"std\"] }\n");
        // JWT segments are base64url without padding.
        deps.push_str("base64 = \"0.22\"\n");
    }
    // Phase A4 (PERF_PLAN.md): global allocator. mimalloc on Windows
    // sidesteps the notoriously slow `HeapAlloc` / `HeapFree` path that
    // dominates Vec / String / HashMap churn on a Windows host; on Linux
    // we fall back to the system allocator (glibc malloc is competitive
    // for our workload, and jemalloc adds 100+ KB to every binary).
    // The actual `#[global_allocator]` declaration lives in the prelude,
    // gated on `#[cfg(windows)]`.
    //
    // This MUST stay the last block: it opens a
    // `[target.'cfg(windows)'.dependencies]` table, so any crate pushed
    // after it lands under that table instead of `[dependencies]` and
    // silently vanishes on non-Windows targets — exactly how tokio-postgres
    // went missing on the Linux CI build.
    deps.push_str("[target.'cfg(windows)'.dependencies]\n");
    deps.push_str("mimalloc = { version = \"0.1\", default-features = false }\n");
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
publish = false

[[bin]]
name = "{name}"
path = "src/main.rs"

[dependencies]
{deps}
[profile.release]
# Phase A5 of PERF_PLAN.md. `opt-level = 3` (was "z") trades a slightly
# bigger binary for actual loop / inline optimization — release builds
# are perf-sensitive, not size-sensitive. `lto = "fat"` (was bare `true`,
# which is already fat — written explicitly so future readers don't
# downgrade to thin). `codegen-units = 1` keeps a single LLVM module
# so the linker sees everything for inlining.
# `panic = "abort"` is intentionally left off: the runtime relies on
# `catch_unwind` to power `try {{}} catch {{}}` and `transaction {{}}` —
# enabling abort would silently break both.
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
"#,
        name = app_name,
        deps = deps,
    )
}

// --- Cargo invocation ---------------------------------------------------------

fn invoke_cargo(
    cargo: &Path,
    workspace: &Path,
    app_name: &str,
    release: bool,
    target: Option<&str>,
) -> Result<PathBuf> {
    let mut cmd = Command::new(cargo);
    cmd.arg("build").current_dir(workspace);
    if release {
        cmd.arg("--release");
        // Phase A5 of PERF_PLAN.md: build with `-C target-cpu=native` so
        // LLVM emits instructions for the host's exact micro-architecture
        // (AVX2 / BMI2 / etc. when available) — meaningful on hot integer
        // and string paths. Restricted to release builds because:
        //   * debug binaries are rebuilt constantly and target-cpu=native
        //     defeats Cargo's incremental cache when machines differ;
        //   * cross-target builds use an explicit `--target` and must
        //     not be miscompiled for the *host* CPU.
        if target.is_none() {
            let existing = std::env::var("RUSTFLAGS").unwrap_or_default();
            let flag = "-C target-cpu=native";
            let combined = if existing.is_empty() {
                flag.to_string()
            } else if existing.contains("target-cpu") {
                // Honour the user's override — don't double-set.
                existing
            } else {
                format!("{} {}", existing, flag)
            };
            cmd.env("RUSTFLAGS", combined);
        }
    }
    if let Some(t) = target {
        cmd.arg("--target").arg(t);
    }
    cmd.arg("--bin").arg(app_name);

    let status = cmd
        .status()
        .with_context(|| format!("Failed to spawn cargo at {}", cargo.display()))?;
    if !status.success() {
        bail!("cargo build failed (exit {})", status.code().unwrap_or(-1));
    }

    let profile_dir = if release { "release" } else { "debug" };
    let exe = if target_is_windows(target) {
        format!("{app_name}.exe")
    } else {
        app_name.to_string()
    };
    // With --target, cargo emits to target/<triple>/<profile>/ instead of
    // target/<profile>/.
    let mut bin = workspace.join("target");
    if let Some(t) = target {
        bin = bin.join(t);
    }
    let bin = bin.join(profile_dir).join(&exe);
    if !bin.is_file() {
        bail!(
            "cargo reported success but binary not found: {}",
            bin.display()
        );
    }
    Ok(bin)
}

/// Cargo's target triples follow the `<arch>-<vendor>-<sys>-<env>?`
/// convention. The third segment ("sys") is enough to tell us whether
/// the produced executable will carry a `.exe` suffix.
fn target_is_windows(target: Option<&str>) -> bool {
    match target {
        Some(t) => t.contains("-windows-"),
        None => cfg!(windows),
    }
}

fn copy_to_project_bin(
    root: &Path,
    src: &Path,
    release: bool,
    target: Option<&str>,
) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    // With --target, segregate the produced artifact under
    // bin/<target>/<profile>/ so multiple target builds can coexist.
    let bin_dir = if let Some(t) = target {
        root.join("bin").join(t).join(profile)
    } else {
        root.join("bin").join(profile)
    };
    std::fs::create_dir_all(&bin_dir)
        .with_context(|| format!("Failed to create {}", bin_dir.display()))?;
    let file_name = src
        .file_name()
        .ok_or_else(|| anyhow!("cargo output has no file name"))?;
    let dest = bin_dir.join(file_name);
    std::fs::copy(src, &dest)
        .with_context(|| format!("Failed to copy {} to {}", src.display(), dest.display()))?;
    Ok(dest)
}

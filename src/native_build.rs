//! Native AOT compilation: `.jwc` → Rust source → `cargo build` → native binary.
//!
//! Phase 4 incremental rollout. The supported subset today:
//!   * Any number of user-defined `function name(params...)`, including `main`.
//!   * `route GET|POST|PUT|DELETE|PATCH "path" { ... }` — block-body routes.
//!     Path params (`{id}`) and query params are exposed inside the body via
//!     `path_param(name)` / `query_param(name[, default])` built-ins.
//!   * `serve(port)` starts a dependency-free HTTP/1.0 server (std-only TCP
//!     listener + thread-per-connection worker pool) that dispatches into the
//!     registered routes.
//!   * Function bodies: `let` / `Assign` / `FieldAssign`, `if` / `while` /
//!     `for in`, `break` / `continue` / `return`, `print`.
//!   * Expressions: literals, variable refs, `var.field`, object literals,
//!     `new Entity()`, arithmetic / comparison / boolean operators.
//!   * Built-in functions: `length`, `lower`, `upper`, `trim`, `contains`,
//!     `starts_with`, `ends_with`, `replace`, `split`, `first`, `last`,
//!     `json_parse`, `json_stringify`, `path_param`, `query_param`, `body`,
//!     `header`, `json`, `text`, `ok`, `created`, `not_found`,
//!     `unauthorized`, `forbidden`, `internal_error`, `status_code`.
//!
//! Not yet supported (clear error pointing at `jwc run` / `jwc build` without
//! --native): dbcontext / entity / class / middleware / errorHandler / route
//! `-> handler` form, DB / WS / job-queue built-ins, `try`/`catch`,
//! `transaction`, `await`. These land in subsequent slices.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::ast::{AggregateKind, Expr, FunctionDecl, ModelKind, NavigationField, Program, Stmt};

const BUILD_DIR_NAME: &str = ".jwc-build";

/// Built-in JWC functions emitted as inlined helpers in the generated Rust
/// prelude. Calls to anything outside this set or the user's own functions
/// produce an "unsupported" error at codegen.
const BUILTINS: &[&str] = &[
    "length",
    "lower",
    "upper",
    "trim",
    "contains",
    "starts_with",
    "ends_with",
    "replace",
    "split",
    "first",
    "last",
    "json_parse",
    "json_stringify",
    // HTTP request inspection — only meaningful inside a route body.
    "path_param",
    "query_param",
    "body",
    "header",
    // HTTP response helpers.
    "json",
    "text",
    "ok",
    "created",
    "not_found",
    "unauthorized",
    "forbidden",
    "internal_error",
    "status_code",
    // DB.
    "setConnectionString",
    // WebSocket — only valid inside a `route WS "/path" { ... }` body.
    "ws_send",
    "ws_recv",
    "ws_close",
];

/// Built-ins that codegen handles itself (not via `jwc_b_<name>` dispatch).
const SPECIAL_BUILTINS: &[&str] = &["serve"];

/// Codegen-time metadata for a DB-bound entity. Captures the column list and
/// each column's Postgres-target type so INSERT param boxing can pick the
/// right `ToSql` impl.
#[derive(Clone)]
struct EntityMeta {
    table: String,
    fields: Vec<EntityField>,
    navigations: Vec<NavigationField>,
}

#[derive(Clone)]
struct EntityField {
    name: String,
    pg: PgKind,
    is_auto_increment: bool,
    is_primary_key: bool,
}

/// Coarse-grained Postgres type bucket. JWC types collapse onto these because
/// the codegen only needs to pick a `jwc_param_*` boxing helper, not emit DDL.
#[derive(Clone, Copy)]
enum PgKind {
    Int,    // int / int2 / int4 / int8 / bigint
    Float,  // double / decimal / float / real
    Bool,
    Str,    // string / varchar / text / uuid / datetime — all carried as text
}

fn pg_kind_for(type_name: &str) -> PgKind {
    let t = type_name.to_lowercase();
    match t.as_str() {
        "int" | "int2" | "int4" | "int8" | "smallint" | "bigint" | "integer" => PgKind::Int,
        "double" | "decimal" | "float" | "float4" | "float8" | "real" | "numeric" => PgKind::Float,
        "bool" | "boolean" => PgKind::Bool,
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
            Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) | Expr::Mod(a, b)
            | Expr::Eq(a, b) | Expr::Neq(a, b) | Expr::Lt(a, b) | Expr::Lte(a, b)
            | Expr::Gt(a, b) | Expr::Gte(a, b) | Expr::And(a, b) | Expr::Or(a, b) => walk_expr(a) || walk_expr(b),
            Expr::ObjectLit(pairs) => pairs.iter().any(|(_, v)| walk_expr(v)),
            Expr::DbSelect { where_clause, limit, offset, .. } => {
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
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value) | Stmt::Expr(value) => walk_expr(value),
            Stmt::If { cond, then_body, else_body } => {
                walk_expr(cond)
                    || then_body.iter().any(walk_stmt)
                    || else_body.as_ref().map(|b| b.iter().any(walk_stmt)).unwrap_or(false)
            }
            Stmt::While { cond, body } => walk_expr(cond) || body.iter().any(walk_stmt),
            Stmt::ForIn { iter, body, .. } => walk_expr(iter) || body.iter().any(walk_stmt),
            Stmt::Return(Some(e)) => walk_expr(e),
            Stmt::Try { body, catch_body, .. } => {
                body.iter().any(walk_stmt) || catch_body.iter().any(walk_stmt)
            }
            Stmt::Transaction { body } => body.iter().any(walk_stmt),
            Stmt::DbDeleteWhere { where_clause, .. } => walk_where(where_clause),
            _ => false,
        }
    }
    for f in &program.functions {
        if f.body.iter().any(walk_stmt) { return true; }
    }
    for r in &program.routes {
        if r.body.iter().any(walk_stmt) { return true; }
    }
    for m in &program.middlewares {
        if m.body.iter().any(walk_stmt) { return true; }
    }
    if let Some(eh) = &program.error_handler {
        if eh.body.iter().any(walk_stmt) { return true; }
    }
    false
}

pub fn compile(program: &Program, root: &Path, app_name: &str, release: bool) -> Result<CompileReport> {
    reject_unsupported(program)?;

    let cargo = find_cargo().context(
        "`cargo` not found.\n\
         Native build requires a Rust toolchain on PATH. Install via https://rustup.rs/\n\
         or run `jwc build` (without --native) for the interpreter-bundled launcher.",
    )?;

    let needs_db = !program.dbcontexts.is_empty() || !program.models.is_empty();
    let needs_http_client = program_uses_http_client(program);
    let rust_src = codegen(program, needs_db)?;
    let workspace = scaffold_workspace(root, app_name, &rust_src, needs_db, needs_http_client)?;
    let bin = invoke_cargo(&cargo, &workspace, app_name, release)?;
    let final_path = copy_to_project_bin(root, &bin, release)?;

    Ok(CompileReport {
        binary_path: final_path,
        workspace,
    })
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
    let builtins: HashSet<&str> = BUILTINS.iter().chain(SPECIAL_BUILTINS.iter()).copied().collect();

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
        Stmt::If { cond, then_body, else_body } => {
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
        Stmt::ValidateBody { .. } => Ok(()),
        Stmt::Try { body, catch_body, .. } => {
            check_block(body, funcs, builtins)?;
            check_block(catch_body, funcs, builtins)
        }
        Stmt::Transaction { body } => check_block(body, funcs, builtins),
    }
}

fn check_expr(expr: &Expr, funcs: &HashSet<String>, builtins: &HashSet<&str>) -> Result<()> {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Var(_) => {
            Ok(())
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
            check_expr(a, funcs, builtins)?;
            check_expr(b, funcs, builtins)
        }
        Expr::Neg(e) | Expr::Not(e) => check_expr(e, funcs, builtins),
        Expr::FieldGet { .. } => Ok(()),
        Expr::NewEntity { .. } => Ok(()),
        Expr::ObjectLit(pairs) => {
            for (_, v) in pairs {
                check_expr(v, funcs, builtins)?;
            }
            Ok(())
        }
        Expr::Call { name, args } => {
            // `print(...)` as an expression evaluates to V::Null after the
            // side effect — emit_expr handles the special case.
            if name.contains('.') {
                bail!(unsupported(&format!(
                    "dotted call `{name}(...)` — namespaced functions need the module system (Phase 6)"
                )));
            }
            if name != "print"
                && !funcs.contains(name)
                && !builtins.contains(name.as_str())
            {
                bail!(unsupported(&format!(
                    "call to `{name}(...)` — unknown function. Define it or use one of the built-ins: {}, serve",
                    BUILTINS.join(", ")
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
        Expr::DbSelect { where_clause, limit, offset, .. } => {
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

fn check_where(w: &crate::ast::WhereExpr, funcs: &HashSet<String>, builtins: &HashSet<&str>) -> Result<()> {
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

fn codegen(program: &Program, needs_db: bool) -> Result<String> {
    let known_funcs: HashSet<String> = program
        .functions
        .iter()
        .map(|f| f.name.clone())
        .chain(program.middlewares.iter().map(|m| m.name.clone()))
        .collect();
    let entities = collect_entities(program);

    let needs_ws = program
        .routes
        .iter()
        .any(|r| matches!(r.protocol, crate::ast::RouteProtocol::Ws));

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
    out.push('\n');

    let fn_decls: HashMap<String, &FunctionDecl> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let ctx = CodegenCtx {
        funcs: &known_funcs,
        entities: &entities,
        fn_decls: &fn_decls,
        has_error_handler: program.error_handler.is_some(),
        closure_depth: std::cell::Cell::new(0),
    };

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

    out.push_str("\n#[tokio::main(flavor = \"multi_thread\")]\nasync fn main() {\n    let _ = user_main().await;\n}\n");
    Ok(out)
}

fn collect_entities(program: &Program) -> HashMap<String, EntityMeta> {
    let mut out = HashMap::new();
    for model in &program.models {
        if !matches!(model.kind, ModelKind::Entity) { continue; }
        let fields = model.fields.iter().map(|f| EntityField {
            name: f.name.clone(),
            pg: pg_kind_for(&f.ty.name),
            is_auto_increment: f.is_auto_increment,
            is_primary_key: f.is_primary_key,
        }).collect();
        out.insert(model.name.clone(), EntityMeta {
            table: model.name.clone(),
            fields,
            navigations: model.navigations.clone(),
        });
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
    has_error_handler: bool,
    /// Number of nested catch_unwind closures we're currently emitting into.
    /// `return X;` inside any closure (try/transaction body) must park the
    /// value in the thread-local slot instead of doing a direct Rust return.
    closure_depth: std::cell::Cell<usize>,
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
}

fn emit_route_handler(out: &mut String, idx: usize, route: &crate::ast::RouteDecl, ctx: &CodegenCtx) {
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
    if let Some(handler) = &route.handler {
        let args = handler_arg_exprs(handler, ctx);
        out.push_str(&format!("        return {}({}).await;\n", user_fn_name(handler), args));
    } else {
        for stmt in &route.body {
            emit_stmt(out, stmt, 2, ctx);
        }
    }
    out.push_str("        if let Some(__r) = jwc_take_return() { return __r; }\n");
    out.push_str("        V::Null\n");
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
    format!("mw_{name}")
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
    if p.starts_with('/') { p.to_string() } else { format!("/{p}") }
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
    format!("user_{name}")
}

fn builtin_fn_name(name: &str) -> String {
    format!("jwc_b_{name}")
}

fn emit_stmt(out: &mut String, stmt: &Stmt, indent: usize, ctx: &CodegenCtx) {
    let pad = "    ".repeat(indent);
    match stmt {
        Stmt::Let { name, value } => {
            out.push_str(&pad);
            out.push_str("let mut ");
            out.push_str(&sanitize_ident(name));
            out.push_str(": V = ");
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
            out.push_str(&pad);
            out.push_str("jwc_set_field(&mut ");
            out.push_str(&sanitize_ident(var));
            out.push_str(", \"");
            push_str_escaped(out, field);
            out.push_str("\", ");
            emit_expr(out, value, ctx);
            out.push_str(");\n");
        }
        Stmt::Print(expr) => {
            out.push_str(&pad);
            out.push_str("jwc_print(");
            emit_expr(out, expr, ctx);
            out.push_str(");\n");
        }
        Stmt::If { cond, then_body, else_body } => {
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
        Stmt::DbDeleteWhere { table, where_clause, .. } => {
            emit_db_delete_where(out, &pad, table, where_clause, ctx);
        }
        Stmt::ValidateBody { fields } => {
            emit_validate_body(out, &pad, fields, ctx);
        }
        Stmt::Try { body, catch_var, catch_body, .. } => {
            emit_try_catch(out, &pad, body, catch_var, catch_body, indent, ctx);
        }
        Stmt::Transaction { body } => {
            emit_transaction(out, &pad, body, indent, ctx);
        }
    }
}

fn emit_try_catch(
    out: &mut String,
    pad: &str,
    body: &[Stmt],
    catch_var: &str,
    catch_body: &[Stmt],
    indent: usize,
    ctx: &CodegenCtx,
) {
    let inner = format!("{pad}    ");
    out.push_str(pad);
    out.push_str("{\n");
    out.push_str(&inner);
    out.push_str("let __res = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {\n");

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

    // On panic, bind error to catch_var and run catch_body.
    out.push_str(&inner);
    out.push_str("if let Err(__e) = __res {\n");
    out.push_str(&inner2);
    out.push_str(&format!(
        "let mut {}: V = jwc_error_value(jwc_panic_payload_to_string(__e));\n",
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
    out.push_str("let __res = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {\n");

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

fn emit_validate_body(out: &mut String, pad: &str, fields: &[crate::ast::ValidateField], ctx: &CodegenCtx) {
    out.push_str(pad);
    out.push_str("{\n");
    let inner = format!("{pad}    ");
    out.push_str(&inner);
    out.push_str("let __body = jwc_b_body();\n");
    out.push_str(&inner);
    out.push_str("let mut __errors: BTreeMap<String, V> = BTreeMap::new();\n");
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
                        "if matches!(__field, V::Null) {{ __errors.insert(\"{f}\".to_string(), V::Str(\"required\".to_string())); }}\n",
                        f = fname,
                    ));
                }
                crate::ast::ValidateRule::MinLength(n) => {
                    out.push_str(&format!(
                        "if let V::Str(ref __s) = __field {{ if __s.chars().count() < {n} {{ __errors.insert(\"{f}\".to_string(), V::Str(\"minLength {n}\".to_string())); }} }}\n",
                        n = n, f = fname,
                    ));
                }
                crate::ast::ValidateRule::MaxLength(n) => {
                    out.push_str(&format!(
                        "if let V::Str(ref __s) = __field {{ if __s.chars().count() > {n} {{ __errors.insert(\"{f}\".to_string(), V::Str(\"maxLength {n}\".to_string())); }} }}\n",
                        n = n, f = fname,
                    ));
                }
                crate::ast::ValidateRule::Min(v) => {
                    out.push_str(&format!(
                        "{{ if let Some(__n) = jwc_to_float(&__field) {{ if __n < {v}_f64 {{ __errors.insert(\"{f}\".to_string(), V::Str(\"min {v}\".to_string())); }} }} }}\n",
                        v = v, f = fname,
                    ));
                }
                crate::ast::ValidateRule::Max(v) => {
                    out.push_str(&format!(
                        "{{ if let Some(__n) = jwc_to_float(&__field) {{ if __n > {v}_f64 {{ __errors.insert(\"{f}\".to_string(), V::Str(\"max {v}\".to_string())); }} }} }}\n",
                        v = v, f = fname,
                    ));
                }
                crate::ast::ValidateRule::Pattern(_) => {
                    // No regex crate in the native prelude yet — skip with a
                    // best-effort non-empty string check. Document below.
                    out.push_str(&format!(
                        "if !matches!(__field, V::Str(_)) {{ __errors.insert(\"{f}\".to_string(), V::Str(\"pattern\".to_string())); }}\n",
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
    out.push_str("let mut __payload = BTreeMap::new();\n");
    out.push_str(&inner2);
    out.push_str("__payload.insert(\"status\".to_string(), V::Int(400));\n");
    out.push_str(&inner2);
    out.push_str("__payload.insert(\"error\".to_string(), V::Str(\"Validation failed\".to_string()));\n");
    out.push_str(&inner2);
    out.push_str("__payload.insert(\"fields\".to_string(), V::Object(__errors));\n");
    out.push_str(&inner2);
    if ctx.in_closure() {
        out.push_str("{ jwc_set_return(V::Object(__payload)); return V::Null; }\n");
    } else {
        out.push_str("return V::Object(__payload);\n");
    }
    out.push_str(&inner);
    out.push_str("}\n");
    out.push_str(pad);
    out.push_str("}\n");
}

fn helper_for_kind(k: PgKind) -> &'static str {
    match k {
        PgKind::Int => "jwc_param_int",
        PgKind::Float => "jwc_param_float",
        PgKind::Bool => "jwc_param_bool",
        PgKind::Str => "jwc_param_str",
    }
}

fn pk_fields(meta: &EntityMeta) -> Vec<&EntityField> {
    let mut pks: Vec<&EntityField> = meta.fields.iter().filter(|f| f.is_primary_key).collect();
    if pks.is_empty() {
        // Fall back to a field literally named `id` (mirrors runner.rs default).
        if let Some(f) = meta.fields.iter().find(|f| f.name.eq_ignore_ascii_case("id")) {
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
            out.push_str(&format!("panic!(\"insert into unknown entity {}\");\n", table));
            return;
        }
    };
    let cols: Vec<&EntityField> = meta.fields.iter().filter(|f| !f.is_auto_increment).collect();
    let mut sql = format!("INSERT INTO \"{}\" (", meta.table);
    for (i, f) in cols.iter().enumerate() {
        if i > 0 { sql.push_str(", "); }
        sql.push_str(&format!("\"{}\"", f.name));
    }
    sql.push_str(") VALUES (");
    for i in 0..cols.len() {
        if i > 0 { sql.push_str(", "); }
        sql.push_str(&format!("${}", i + 1));
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
    out.push_str(&format!("let _ = jwc_db_exec(\"{}\", __params).await;\n", escape_sql(&sql)));
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_db_update(out: &mut String, pad: &str, var: &str, table: &str, ctx: &CodegenCtx) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!("panic!(\"update on unknown entity {}\");\n", table));
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
        out.push_str(&format!("panic!(\"update on `{}` has no non-PK columns to set\");\n", table));
        return;
    }
    let mut sql = format!("UPDATE \"{}\" SET ", meta.table);
    for (i, f) in set_cols.iter().enumerate() {
        if i > 0 { sql.push_str(", "); }
        sql.push_str(&format!("\"{}\" = ${}", f.name, i + 1));
    }
    sql.push_str(" WHERE ");
    for (i, p) in pks.iter().enumerate() {
        if i > 0 { sql.push_str(" AND "); }
        sql.push_str(&format!("\"{}\" = ${}", p.name, set_cols.len() + i + 1));
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
    out.push_str(&format!("let _ = jwc_db_exec(\"{}\", __params).await;\n", escape_sql(&sql)));
    out.push_str(pad);
    out.push_str("}\n");
}

fn emit_db_delete_by_var(out: &mut String, pad: &str, var: &str, table: &str, ctx: &CodegenCtx) {
    let meta = match ctx.entities.get(table) {
        Some(m) => m,
        None => {
            out.push_str(pad);
            out.push_str(&format!("panic!(\"delete from unknown entity {}\");\n", table));
            return;
        }
    };
    let pks = pk_fields(meta);
    let mut sql = format!("DELETE FROM \"{}\" WHERE ", meta.table);
    for (i, p) in pks.iter().enumerate() {
        if i > 0 { sql.push_str(" AND "); }
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
    out.push_str(&format!("let _ = jwc_db_exec(\"{}\", __params).await;\n", escape_sql(&sql)));
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
            out.push_str(&format!("panic!(\"delete from unknown entity {}\");\n", table));
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
    out.push_str(&format!("let _ = jwc_db_exec(\"{}\", __params).await;\n", escape_sql(&sql)));
    out.push_str(pad);
    out.push_str("}\n");
}

fn escape_sql(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// --- WHERE clause builder ----------------------------------------------------

struct WhereBuilder<'a> {
    entity: &'a EntityMeta,
    sql: String,
    params: Vec<(PgKind, &'a Expr)>,
}

impl<'a> WhereBuilder<'a> {
    fn new(entity: &'a EntityMeta) -> Self {
        WhereBuilder { entity, sql: String::new(), params: Vec::new() }
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
                let (col, kind) = self.col_kind(&atom.field)?;
                // is null / is not null sentinel from the parser: op `==`/`!=`
                // with rhs == Expr::Null.
                if matches!(&atom.rhs, Expr::Null) && (atom.op == "==" || atom.op == "!=") {
                    let neg = atom.op == "!=";
                    self.sql.push_str(&format!(
                        "\"{}\" IS {}NULL",
                        col,
                        if neg { "NOT " } else { "" }
                    ));
                    return Ok(());
                }
                let op = match atom.op.as_str() {
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
                self.sql.push_str(&format!("\"{}\" {} ${}", col, op, n));
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
                let (col, kind) = self.col_kind(field)?;
                if values.is_empty() {
                    // SQL `IN ()` is a parse error; `FALSE` makes the row count zero.
                    self.sql.push_str("FALSE");
                    return Ok(());
                }
                self.sql.push_str(&format!("\"{}\" IN (", col));
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
                let (col, kind) = self.col_kind(field)?;
                let nl = self.params.len() + 1;
                self.params.push((kind, low));
                let nh = self.params.len() + 1;
                self.params.push((kind, high));
                self.sql
                    .push_str(&format!("\"{}\" BETWEEN ${} AND ${}", col, nl, nh));
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
            out.push_str("V::Str(\"");
            push_str_escaped(out, s);
            out.push_str("\".to_string())");
        }
        Expr::Bool(b) => {
            out.push_str("V::Bool(");
            out.push_str(if *b { "true" } else { "false" });
            out.push(')');
        }
        Expr::Null => out.push_str("V::Null"),
        Expr::Var(name) => {
            out.push_str(&sanitize_ident(name));
            out.push_str(".clone()");
        }
        Expr::FieldGet { var, field } => {
            out.push_str("jwc_get_field(&");
            out.push_str(&sanitize_ident(var));
            out.push_str(", \"");
            push_str_escaped(out, field);
            out.push_str("\")");
        }
        Expr::NewEntity { .. } => {
            out.push_str("V::Object(std::collections::BTreeMap::new())");
        }
        Expr::ObjectLit(pairs) => {
            out.push_str("{ let mut __o = std::collections::BTreeMap::<String, V>::new();");
            for (k, v) in pairs {
                out.push_str(" __o.insert(\"");
                push_str_escaped(out, k);
                out.push_str("\".to_string(), ");
                emit_expr(out, v, ctx);
                out.push_str(");");
            }
            out.push_str(" V::Object(__o) }");
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
            let is_user = ctx.funcs.contains(name);
            // Async builtins implemented in the prelude as `async fn jwc_b_*`.
            let is_async_builtin = matches!(
                name.as_str(),
                "sleep_ms" | "http_get" | "fetch_json" | "setConnectionString"
                | "ws_send" | "ws_recv" | "ws_close"
            );
            if is_user {
                out.push_str(&user_fn_name(name));
            } else {
                out.push_str(&builtin_fn_name(name));
            }
            out.push('(');
            let mut first = true;
            for a in args {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                emit_expr(out, a, ctx);
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
            ..
        } => {
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
        }
        Expr::DbCount { table, where_clause, .. } => {
            emit_db_count(out, table, where_clause.as_deref(), ctx);
        }
        Expr::DbAggregate { kind, field, table, where_clause, .. } => {
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
                        format!("unknown projection column `{}` on entity `{}`", c, meta.table),
                    ));
                    return;
                }
            }
        }
        parts.join(", ")
    };

    // Eager-load needs the PK in each parent row, so when `with` is requested
    // expand `*` is fine; with explicit projection, force-include the PK so
    // navigation lookups still work.
    let select_clause = if !with_relations.is_empty() && !projection.is_empty() {
        let pks = pk_fields(meta);
        let mut cols: Vec<String> = projection
            .iter()
            .filter_map(|c| meta.fields.iter().find(|f| f.name.eq_ignore_ascii_case(c)).map(|f| format!("\"{}\"", f.name)))
            .collect();
        for p in &pks {
            let q = format!("\"{}\"", p.name);
            if !cols.contains(&q) {
                cols.push(q);
            }
        }
        cols.join(", ")
    } else {
        select_clause
    };

    // Validate `with` relations early.
    struct NavPlan<'a> {
        nav: &'a NavigationField,
        target: &'a EntityMeta,
        parent_pk: &'a EntityField,
    }
    let mut plans: Vec<NavPlan> = Vec::new();
    for rel in with_relations {
        let nav = match meta.navigations.iter().find(|n| n.name.eq_ignore_ascii_case(rel)) {
            Some(n) => n,
            None => {
                out.push_str(&format!(
                    "{{ compile_error!({:?}); V::Null }}",
                    format!("unknown navigation `{}` on entity `{}`", rel, meta.table),
                ));
                return;
            }
        };
        let target = match ctx.entities.get(&nav.target_entity) {
            Some(m) => m,
            None => {
                out.push_str(&format!(
                    "{{ compile_error!({:?}); V::Null }}",
                    format!("navigation `{}` targets unknown entity `{}`", rel, nav.target_entity),
                ));
                return;
            }
        };
        let pks = pk_fields(meta);
        if pks.is_empty() {
            out.push_str(&format!(
                "{{ compile_error!({:?}); V::Null }}",
                format!("entity `{}` has no PK; `with {}` needs one", meta.table, rel),
            ));
            return;
        }
        plans.push(NavPlan { nav, target, parent_pk: pks[0] });
    }

    let mut sql = format!("SELECT {} FROM \"{}\"", select_clause, meta.table);
    let mut wb = WhereBuilder::new(meta);
    if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!("{{ compile_error!({:?}); V::Null }}", e.to_string()));
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
                wb.params.push((PgKind::Int, l));
                sql.push_str(&format!(" LIMIT ${}", n));
            }
        }
    }
    if let Some(o) = offset {
        match o {
            Expr::Int(n) => sql.push_str(&format!(" OFFSET {}", n)),
            _ => {
                let n = wb.params.len() + 1;
                wb.params.push((PgKind::Int, o));
                sql.push_str(&format!(" OFFSET ${}", n));
            }
        }
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
        " let mut __rows = jwc_db_query(\"{}\", __params).await;",
        escape_sql(&sql)
    ));

    // Eager-load each `with` relation by issuing `WHERE fk IN (...)` against
    // the target table and grouping client-side. One round-trip per relation
    // (N+1 collapsed to N+R, where R = number of `with` clauses).
    for plan in &plans {
        let pk_is_int = matches!(plan.parent_pk.pg, PgKind::Int);
        let one_to_one = matches!(plan.nav.kind, crate::ast::NavigationKind::OneToOne);
        out.push_str(&format!(
            " jwc_db_eager_load(&mut __rows, \"{pk}\", \"{tgt}\", \"{fk}\", {is_int}, \"{nav}\", {oto}).await;",
            pk = plan.parent_pk.name,
            tgt = plan.target.table,
            fk = plan.nav.target_field,
            is_int = pk_is_int,
            nav = plan.nav.name,
            oto = one_to_one,
        ));
    }

    if first {
        out.push_str(" __rows.into_iter().next().unwrap_or(V::Null) }");
    } else {
        out.push_str(" V::Array(__rows) }");
    }
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
            out.push_str(&format!("panic!(\"aggregate on unknown entity {}\")", table));
            return;
        }
    };
    let col_name = match field.split_once('.') {
        Some((_, c)) => c,
        None => field,
    };
    let f = match meta.fields.iter().find(|f| f.name.eq_ignore_ascii_case(col_name)) {
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
    };
    let mut sql = format!(
        "SELECT {}(\"{}\") AS __agg FROM \"{}\"",
        op, f.name, meta.table,
    );
    let mut wb = WhereBuilder::new(meta);
    if let Some(w) = where_clause {
        if let Err(e) = wb.emit(w) {
            out.push_str(&format!("{{ compile_error!({:?}); V::Null }}", e.to_string()));
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
            out.push_str(&format!("{{ compile_error!({:?}); V::Null }}", e.to_string()));
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

fn sanitize_ident(name: &str) -> String {
    if matches!(
        name,
        "fn" | "let" | "mut" | "if" | "else" | "while" | "for" | "in" | "loop"
            | "break" | "continue" | "return" | "match" | "struct" | "enum" | "impl"
            | "trait" | "use" | "mod" | "pub" | "crate" | "self" | "Self" | "super"
            | "where" | "as" | "type" | "const" | "static" | "ref" | "move" | "dyn"
            | "async" | "await" | "true" | "false" | "box" | "abstract" | "become"
            | "do" | "final" | "macro" | "override" | "priv" | "typeof" | "unsized"
            | "virtual" | "yield" | "try" | "union"
    ) || name.starts_with("jwc_") || name.starts_with("user_") || name == "V"
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
        let candidate = home.join(".jwc").join("toolchain").join("bin").join(cargo_exe_name());
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
    if cfg!(windows) { "cargo.exe" } else { "cargo" }
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
) -> Result<PathBuf> {
    let workspace = root.join(BUILD_DIR_NAME);
    let src_dir = workspace.join("src");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("Failed to create {}", src_dir.display()))?;

    let cargo_toml = workspace.join("Cargo.toml");
    std::fs::write(&cargo_toml, render_cargo_toml(app_name, needs_db, needs_http_client))
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

fn render_cargo_toml(app_name: &str, needs_db: bool, needs_http_client: bool) -> String {
    // reqwest is always included because the prelude contains the http_get /
    // fetch_json helpers unconditionally — gating them by feature would require
    // splitting the prelude. `needs_http_client` is kept for future use.
    let _ = needs_http_client;
    let mut deps = String::new();
    deps.push_str("tokio = { version = \"1\", features = [\"full\"] }\n");
    deps.push_str("futures = \"0.3\"\n");
    deps.push_str(
        "reqwest = { version = \"0.12\", default-features = false, features = [\"rustls-tls\", \"json\"] }\n",
    );
    if needs_db {
        deps.push_str("tokio-postgres = \"0.7\"\n");
        deps.push_str("deadpool-postgres = \"0.14\"\n");
    }
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
opt-level = "z"
lto = true
codegen-units = 1
strip = true
# panic = abort would disable catch_unwind needed by try/catch and transaction.
"#,
        name = app_name,
        deps = deps,
    )
}

// --- Cargo invocation ---------------------------------------------------------

fn invoke_cargo(cargo: &Path, workspace: &Path, app_name: &str, release: bool) -> Result<PathBuf> {
    let mut cmd = Command::new(cargo);
    cmd.arg("build").current_dir(workspace);
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--bin").arg(app_name);

    let status = cmd
        .status()
        .with_context(|| format!("Failed to spawn cargo at {}", cargo.display()))?;
    if !status.success() {
        bail!("cargo build failed (exit {})", status.code().unwrap_or(-1));
    }

    let profile_dir = if release { "release" } else { "debug" };
    let exe = if cfg!(windows) {
        format!("{app_name}.exe")
    } else {
        app_name.to_string()
    };
    let bin = workspace.join("target").join(profile_dir).join(&exe);
    if !bin.is_file() {
        bail!("cargo reported success but binary not found: {}", bin.display());
    }
    Ok(bin)
}

fn copy_to_project_bin(root: &Path, src: &Path, release: bool) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    let bin_dir = root.join("bin").join(profile);
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

//! The JWC interpreter `Vm`, public request entry points, and front-door
//! orchestration. Heavy lifting lives in sibling sub-modules:
//!
//! * [`builtins`] — `eval_*_call` impl methods for every built-in name
//!   dispatched out of `Expr::Call` (path_param, http_get, cache_set, ...).
//! * [`dispatch`] — HTTP route matching, middleware chain execution,
//!   `errorHandler` fallback, and the response envelope sentinels.
//! * [`eval`] — `Vm::eval_expr` and the numeric helpers shared with the
//!   comparison operators.
//! * [`exec`] — `Vm::exec_block` / `Vm::exec_stmt`, the atomic `update SET`
//!   helper, and every `Stmt::Db*` arm.
//! * [`sql`] — pure SQL builders (`build_insert_sql`, `build_select_sql`,
//!   `build_where_sql`, ...) used by both `exec` and `eval`.
//! * [`types`] — runtime type-checking for typed parameters / returns and
//!   JSON-coerced model objects.
//! * [`validation`] — `validate body { ... }` rule engine.
//! * [`util`] — pure leaf helpers (Levenshtein suggestions, ISO 8601
//!   formatting, base64/uuid sniffs, connection-string parsing).
//!
//! Each method that hangs off `Vm` lives in `impl<'a> Vm<'a> { ... }` blocks
//! inside whichever sub-module owns its concern — Rust's orphan rules let us
//! split an impl across files as long as they all share the same crate.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::ast::{
    ConstDecl, ErrorHandlerDecl, Expr, FunctionDecl, ImportDecl, MiddlewareDecl, ModelDecl,
    ModelKind, MountDecl, Program, RouteDecl, RouteProtocol, Visibility,
};

// Phase 1 [1.0-blocker]: the runtime `Value` model lives in the standalone
// `jwc-runtime` sub-crate so it can be shared with the native AOT without
// taking on the interpreter's transitive deps. Re-export the items that the
// rest of `runner::*` (and the sub-modules via `super::*`) reach for by their
// original local name so existing call sites keep compiling.
pub use jwc_runtime::{
    format_float, json_to_value, materialize_select_result, value_to_json, value_to_json_smart,
    Value,
};

mod builtins;
mod dispatch;
mod eval;
mod exec;
mod sql;
mod types;
mod util;
mod validation;

use util::{closest_match, levenshtein};

/// Known error kinds for typed-catch dispatch (Phase 10.5).
///
/// `"Error"` is the catch-all super-kind — every error matches it, equivalent
/// to a bare `catch (e)` without a type annotation. Specific kinds match
/// only when the error chain looks like it came from that subsystem.
///
/// Sprint 3A: kinds are now hierarchical via dot-separated paths.
/// `classify_jwc_error` returns the most specific subtype it can determine
/// (e.g. `"DbError.UniqueViolation"` for a PG `23505` SQLSTATE), and
/// `catch_type_matches` does prefix matching on the dot boundary so a
/// parent kind (`DbError`) catches every subtype (`DbError.*`).
///
/// Curation notes:
/// - JWT.Expired is intentionally absent — the built-in `verify_hs256` does
///   not check the `exp` claim, so we cannot reliably detect expiry.
/// - `HttpError.BadGateway` rolls up 502 / 503 / 504 since they share the
///   "upstream is unhealthy" production motivation.
pub(crate) const JWC_ERROR_KINDS: &[&str] = &[
    "Error",
    "DbError",
    "DbError.UniqueViolation",      // PG SQLSTATE 23505
    "DbError.ForeignKeyViolation",  // 23503
    "DbError.NotNullViolation",     // 23502
    "DbError.CheckViolation",       // 23514
    "DbError.SerializationFailure", // 40001
    "DbError.DeadlockDetected",     // 40P01
    "DbError.ConnectionFailure",    // tokio_postgres Error::is_closed()
    "HttpError",
    "HttpError.NotFound",     // 404
    "HttpError.Unauthorized", // 401
    "HttpError.Forbidden",    // 403
    "HttpError.BadGateway",   // 502 / 503 / 504
    "ValidationError",
    "TimeoutError",
    "JwtError",
    "JwtError.InvalidSignature",
];

/// Classify an `anyhow::Error` into the most specific well-known JWC error
/// kind reachable from its `.chain()`.
///
/// Strategy:
/// 1. Walk the error chain and try to downcast each link into the concrete
///    sub-system error type (`tokio_postgres::Error`, `reqwest::Error`).
///    These give us SQLSTATE / HTTP status — the precise signals we need to
///    pick a subtype.
/// 2. If a downcast succeeds but doesn't carry a specific signal, fall back
///    to the parent kind (e.g. an unknown SQLSTATE returns `"DbError"`, not
///    a subtype that would silently mismatch on `catch (e: DbError.X)`).
/// 3. If no downcast hits, fall back to the v1 substring scan so the
///    existing JWT / validation / timeout / loose "this smells like HTTP"
///    paths keep classifying. Substring matching is messy but the only way
///    to surface kinds whose origin is plain `anyhow!(...)` text.
///
/// The returned `&'static str` is one of `JWC_ERROR_KINDS`. The signature
/// is intentionally stable — only the internal logic changes between
/// sprints.
pub(crate) fn classify_jwc_error(e: &anyhow::Error) -> &'static str {
    // --- Pass 1: typed downcasts on the error chain ----------------------
    // These give us authoritative SQLSTATE / HTTP-status signals so we can
    // pick the right subtype without guessing from strings.
    for cause in e.chain() {
        if let Some(pg) = cause.downcast_ref::<tokio_postgres::Error>() {
            return classify_pg_error(pg);
        }
        if let Some(http) = cause.downcast_ref::<reqwest::Error>() {
            return classify_reqwest_error(http);
        }
    }

    // --- Pass 2: substring scan (legacy path) ----------------------------
    let mut msgs: Vec<String> = e.chain().map(|c| c.to_string().to_lowercase()).collect();
    let blob = msgs.join("\n");
    msgs.push(blob);
    let combined = msgs.join("\n");

    let has = |needles: &[&str]| -> bool { needles.iter().any(|n| combined.contains(n)) };

    // JWT first — its substrings are distinctive and we want them ahead of
    // the generic ValidationError catch-all so `jwt_verify: signature
    // mismatch` doesn't get mis-classified as a validation failure.
    if has(&["jwt_verify", "jwt_sign", "jwt:"]) {
        if has(&["signature mismatch", "invalid base64", "hs256"]) {
            return "JwtError.InvalidSignature";
        }
        return "JwtError";
    }
    if has(&[
        "validate body",
        "validation failed",
        "field '",
        "required",
        "minlength",
        "maxlength",
        "is not declared on",
        "type error",
    ]) {
        return "ValidationError";
    }
    if has(&[
        "deadpool",
        "tokio-postgres",
        "postgres",
        "db error",
        "no connection",
        "pool",
        "advisory lock",
        "migration",
        "sql",
    ]) {
        return "DbError";
    }
    if has(&["timeout", "deadline", "elapsed"]) {
        return "TimeoutError";
    }
    if has(&[
        "http",
        "reqwest",
        "status code",
        "url",
        "fetch_json",
        "http_get",
        "http_post",
    ]) {
        return "HttpError";
    }
    "Error"
}

/// Map a `tokio_postgres::Error` onto the most specific `DbError.*` subtype.
/// Falls back to bare `"DbError"` when the error has no SQLSTATE code we
/// recognise — never invents a subtype.
fn classify_pg_error(pg: &tokio_postgres::Error) -> &'static str {
    use tokio_postgres::error::SqlState;
    if let Some(code) = pg.code() {
        if code == &SqlState::UNIQUE_VIOLATION {
            return "DbError.UniqueViolation";
        }
        if code == &SqlState::FOREIGN_KEY_VIOLATION {
            return "DbError.ForeignKeyViolation";
        }
        if code == &SqlState::NOT_NULL_VIOLATION {
            return "DbError.NotNullViolation";
        }
        if code == &SqlState::CHECK_VIOLATION {
            return "DbError.CheckViolation";
        }
        if code == &SqlState::T_R_SERIALIZATION_FAILURE {
            return "DbError.SerializationFailure";
        }
        if code == &SqlState::T_R_DEADLOCK_DETECTED {
            return "DbError.DeadlockDetected";
        }
    }
    if pg.is_closed() {
        return "DbError.ConnectionFailure";
    }
    "DbError"
}

/// Map a `reqwest::Error` onto the most specific `HttpError.*` subtype using
/// its HTTP status (when one was attached). Network/timeout failures with
/// no status fall back to `"HttpError"` so a `catch (e: HttpError)` still
/// catches them.
fn classify_reqwest_error(http: &reqwest::Error) -> &'static str {
    if let Some(status) = http.status() {
        let code = status.as_u16();
        return match code {
            401 => "HttpError.Unauthorized",
            403 => "HttpError.Forbidden",
            404 => "HttpError.NotFound",
            502 | 503 | 504 => "HttpError.BadGateway",
            _ => "HttpError",
        };
    }
    "HttpError"
}

/// Returns true when an error of `kind` should be caught by a `catch (e: T)`
/// clause whose annotated type is `catch_type` (or any error if `catch_type`
/// is `None`).
///
/// Matching rules:
/// - `None` matches every kind (untyped `catch (e)`).
/// - `Some("Error")` matches every kind (the explicit catch-all super-kind).
/// - `Some("DbError")` matches `"DbError"` AND every `"DbError.*"` subtype
///   (parent catches child).
/// - `Some("DbError.UniqueViolation")` matches only that exact kind —
///   sibling subtypes (`DbError.ForeignKeyViolation`) do NOT match.
///
/// All comparisons are exact string compares on the kind returned by
/// `classify_jwc_error`; we never split on `.` more than once because the
/// hierarchy is intentionally two-level (parent → leaf).
pub(crate) fn catch_type_matches(catch_type: Option<&str>, kind: &str) -> bool {
    match catch_type {
        None => true,
        Some("Error") => true,
        Some(t) if t == kind => true,
        Some(t) => {
            // Parent kind catches its dotted children: catch (e: DbError)
            // matches a kind of "DbError.UniqueViolation". We only treat
            // `t` as a parent when the kind literally starts with `t.` —
            // bare prefix overlap (e.g. "Db" vs "DbError") must NOT match.
            kind.len() > t.len()
                && kind.as_bytes().get(t.len()) == Some(&b'.')
                && kind.starts_with(t)
        }
    }
}

/// Best closest-known-kind suggestion for an unknown catch-type identifier.
/// Returns `None` when nothing is similar enough to surface as a hint.
pub(crate) fn closest_known_kind(target: &str) -> Option<&'static str> {
    let target_lc = target.to_ascii_lowercase();
    let threshold = std::cmp::max(2, target_lc.len() / 3);
    let mut best: Option<(usize, &'static str)> = None;
    for &candidate in JWC_ERROR_KINDS {
        if candidate.eq_ignore_ascii_case(target) {
            continue;
        }
        let dist = levenshtein(&target_lc, &candidate.to_ascii_lowercase());
        if dist > threshold {
            continue;
        }
        match best {
            Some((d, _)) if d <= dist => {}
            _ => best = Some((dist, candidate)),
        }
    }
    best.map(|(_, s)| s)
}

/// Compute the FQN key for a decl: `"ns.sub.name"` (lowercase) or just `"name"`
/// if the decl is in the root namespace.
pub(crate) fn fqn_key(namespace: &[String], name: &str) -> String {
    if namespace.is_empty() {
        name.to_lowercase()
    } else {
        let ns = namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        format!("{}.{}", ns, name.to_lowercase())
    }
}

/// Expand the program's routes by applying each `mount` declaration.
///
/// - Root-namespace routes are always included verbatim.
/// - For every `mount` whose target matches a library route's namespace, an
///   active route is emitted with the mount's prefix prepended to the path
///   and the mount's middleware chain prepended to the route's own list.
/// - The same library namespace may be mounted multiple times (e.g.
///   `mount greet at "/api";` and `mount greet at "/public";`) — each yields
///   its own expanded route set.
fn expand_routes(program: &Program) -> Vec<RouteDecl> {
    use std::collections::HashMap;
    let mut by_ns: HashMap<String, Vec<&RouteDecl>> = HashMap::new();
    for r in &program.routes {
        let key = r
            .namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        by_ns.entry(key).or_default().push(r);
    }

    let mut out: Vec<RouteDecl> = Vec::new();

    // Root routes — always active.
    if let Some(roots) = by_ns.get("") {
        for r in roots {
            out.push((*r).clone());
        }
    }

    // Mounted library routes.
    for mount in &program.mounts {
        let key = mount
            .target
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        if let Some(lib_routes) = by_ns.get(&key) {
            for r in lib_routes {
                out.push(apply_mount(r, mount));
            }
        }
    }
    out
}

/// Clone a library `RouteDecl` and apply a single mount: prepend prefix to
/// the path, prepend the mount's middleware chain to the route's own list.
fn apply_mount(route: &RouteDecl, mount: &MountDecl) -> RouteDecl {
    let mut copy = route.clone();
    if let Some(prefix) = &mount.prefix {
        copy.path = format!("{}{}", prefix, copy.path);
    }
    let mut mws = mount.middlewares.clone();
    mws.extend(route.middlewares.iter().cloned());
    copy.middlewares = mws;
    copy
}

use std::sync::Arc;
use std::sync::OnceLock as StdOnceLock;

tokio::task_local! {
    /// Active WebSocket channels for the current handler task. `ws_send`
    /// pushes onto the sender; `ws_recv` awaits on the receiver. The handle
    /// is set by `run_ws_request` before invoking user code and is
    /// task-local so each WS connection has its own.
    pub static WS_HANDLE: Arc<tokio::sync::Mutex<Option<WsHandle>>>;
}

pub struct WsHandle {
    pub rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    pub tx: tokio::sync::mpsc::UnboundedSender<String>,
}
use crate::engine;

static REQWEST_CLIENT: StdOnceLock<reqwest::Client> = StdOnceLock::new();

pub(super) fn http_client() -> &'static reqwest::Client {
    REQWEST_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .build()
            .expect("failed to build reqwest client")
    })
}

#[derive(Debug)]
pub struct RunMainResult {
    pub output: String,
    /// If `serve(port)` was called in `main()`, contains the port to listen on.
    pub serve_port: Option<u16>,
}

pub async fn run_main(program: &Program) -> Result<RunMainResult> {
    let mut vm = Vm::new(program);
    vm.init_consts().await?;
    let _ = vm.call_function("main", Vec::new()).await?;
    Ok(RunMainResult {
        output: vm.output,
        serve_port: vm.serve_requested,
    })
}

/// Dispatch a single HTTP request to the matching route and return (status_code, body).
/// Convenience wrapper around `run_request_with_headers` without headers. Kept
/// public so test code can keep its concise call shape. Discards the response
/// content-type — use `run_request_with_headers` directly if you need it.
#[allow(dead_code)]
pub async fn run_request(
    program: &Program,
    method: &str,
    path: &str,
    body: Option<String>,
) -> Result<(u16, String)> {
    let (status, body, _ct, _headers) =
        run_request_with_headers(program, method, path, body, HashMap::new()).await?;
    Ok((status, body))
}

/// Run a WebSocket route handler. The caller provides two channels that
/// bridge the async axum socket: `rx` carries inbound text frames toward
/// JWC, `tx` carries outbound messages from JWC back to the wire. The
/// channels live in a thread-local so `ws_send` / `ws_recv` / `ws_close`
/// built-ins inside the route body can reach them without plumbing.
pub async fn run_ws_request(
    program: &Program,
    route_path: &str,
    path_params: HashMap<String, String>,
    headers: HashMap<String, String>,
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<()> {
    // Build the Vm first so we get the post-`mount` expanded route table
    // (a library WS route mounted at "/foo" is only reachable through the
    // expanded copy, not the unprefixed library declaration).
    let mut vm = Vm::new(program);
    vm.init_consts().await?;
    let route = vm
        .routes
        .iter()
        .find(|r| r.protocol == RouteProtocol::Ws && r.path == route_path)
        .ok_or_else(|| anyhow!("Unknown WS route: {route_path}"))?;

    let handler = route.handler.clone();
    let body_stmts = route.body.clone();
    let middleware_names: Vec<String> = route.middlewares.clone();
    let route_namespace = route.namespace.clone();
    vm.current_path_params = Some(path_params);
    vm.current_headers = Some(headers);

    let cell = Arc::new(tokio::sync::Mutex::new(Some(WsHandle { rx, tx })));
    WS_HANDLE
        .scope(cell, async move {
            vm.current_namespace_stack.push(route_namespace);
            // Same short-circuit semantics as HTTP routes: if a middleware
            // returns a value, abort the WS handshake by closing the channel
            // before the user's handler runs.
            for mw_name in &middleware_names {
                if vm.run_middleware(mw_name).await?.is_some() {
                    vm.current_namespace_stack.pop();
                    return Ok(());
                }
            }
            if let Some(handler_name) = &handler {
                let args = vm.build_handler_args(handler_name);
                let _ = vm.call_function(handler_name, args).await?;
            } else {
                let mut route_vars = HashMap::new();
                let _ = vm.exec_block(&body_stmts, &mut route_vars).await?;
            }
            vm.current_namespace_stack.pop();
            Ok(())
        })
        .await
}

/// Same as `run_request` but with request headers accessible via `header(name)`.
/// The third tuple element is the response `content-type` declared by the
/// handler (via `html(...)`, `text(...)`, etc.) — `None` means "no override,
/// the transport should pick a sensible default" (today the HTTP server
/// defaults to `application/json` to preserve the historical behaviour).
pub async fn run_request_with_headers(
    program: &Program,
    method: &str,
    path: &str,
    body: Option<String>,
    headers: HashMap<String, String>,
) -> Result<(u16, String, Option<String>, Vec<(String, String)>)> {
    run_request_with_headers_and_id(program, method, path, body, headers, None).await
}

/// Same as [`run_request_with_headers`] but lets the server stamp a
/// per-request id that handlers / middleware / the error handler can read
/// via `request_id()`. Server.rs generates the id; the runner just keeps
/// it in `Vm::current_request_id` for the lifetime of the dispatch.
pub async fn run_request_with_headers_and_id(
    program: &Program,
    method: &str,
    path: &str,
    body: Option<String>,
    headers: HashMap<String, String>,
    request_id: Option<String>,
) -> Result<(u16, String, Option<String>, Vec<(String, String)>)> {
    let mut vm = Vm::new(program);
    vm.init_consts().await?;
    vm.request_body = body;
    vm.current_headers = Some(headers);
    vm.current_request_id = request_id;
    vm.dispatch_route(method, path).await
}

/// Invoke a JWC function by name with a single string payload. Used by the
/// background job queue: workers receive `Job { name, payload }`, look up
/// the registered handler function name, then call this. Any return value
/// is discarded — handlers communicate via side effects (db, cache, email).
pub async fn run_handler(program: &Program, function_name: &str, payload: String) -> Result<()> {
    let mut vm = Vm::new(program);
    vm.init_consts().await?;
    vm.call_function(function_name, vec![Value::Str(payload)])
        .await?;
    Ok(())
}

/// Collect every `Expr::Var` name appearing in `expr` (recursing through all
/// child expressions). Used by `init_consts` to evaluate consts in dependency
/// order. Total over the `Expr` enum.
fn const_var_refs(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_var_refs(expr, &mut out);
    out
}

fn collect_var_refs(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Var(name) => out.push(name.clone()),
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
            collect_var_refs(a, out);
            collect_var_refs(b, out);
        }
        Expr::Neg(inner) | Expr::Not(inner) | Expr::Await(inner) => collect_var_refs(inner, out),
        Expr::ArrayLit(items) => {
            for item in items {
                collect_var_refs(item, out);
            }
        }
        Expr::ObjectLit(pairs) => {
            for (_, value) in pairs {
                collect_var_refs(value, out);
            }
        }
        _ => {}
    }
}

pub(super) struct Vm<'a> {
    pub(super) functions: HashMap<String, &'a FunctionDecl>,
    /// Model schema map (entity + class) for runtime JSON validation on typed params/returns.
    pub(super) models: HashMap<String, &'a ModelDecl>,
    /// Expanded, owned route set produced by `expand_routes(program)`.
    /// Each mount on a library namespace contributes its own copies here,
    /// already prefixed and middleware-chained.
    pub(super) routes: Vec<RouteDecl>,
    pub(super) middlewares: HashMap<String, &'a MiddlewareDecl>,
    pub(super) error_handler: Option<&'a ErrorHandlerDecl>,
    /// All imports in the program, indexed by the namespace they live in.
    pub(super) imports_by_namespace: HashMap<String, Vec<&'a ImportDecl>>,
    /// Namespace of the currently-executing function. Each `call_function`
    /// pushes the callee's namespace; the previous value is restored on
    /// return. Empty Vec = root namespace.
    pub(super) current_namespace_stack: Vec<Vec<String>>,
    /// Primary-key columns per table (keyed by both `Entity` name and its snake_case form).
    /// Empty Vec means no `pk` declared on that entity — falls back to a single `"id"` column.
    pub(super) pk_by_table: HashMap<String, Vec<String>>,
    /// Per-call dirty field tracking: var name (lower-cased) → fields assigned
    /// via `var.field = ...` since the variable entered scope. Used by
    /// `update var in ...` to SET only the modified columns.
    pub(super) dirty_fields: HashMap<String, HashSet<String>>,
    pub(super) current_path_params: Option<HashMap<String, String>>,
    /// Query-string params parsed from the request URL (`?a=1&b=hello`).
    pub(super) current_query_params: Option<HashMap<String, String>>,
    /// Request headers (lower-cased keys) for `header(name)` look-ups.
    pub(super) current_headers: Option<HashMap<String, String>>,
    /// HTTP method of the current request (`GET`, `POST`, ...). Read via
    /// `request_method()`. Set by `dispatch_route` for the lifetime of the
    /// request, restored on exit.
    pub(super) current_method: Option<String>,
    /// Path of the current request, query string stripped (`/api/links`,
    /// `/abc1234`). Read via `request_path()`. Set/restored alongside
    /// `current_method` so middlewares can log a meaningful endpoint.
    pub(super) current_request_path: Option<String>,
    /// Stable identifier the server assigns per HTTP request. Read via
    /// `request_id()` so middleware / handlers / error handlers can stamp
    /// the same value on every log line and propagate it to downstream
    /// services via a header. `None` outside route execution.
    pub(super) current_request_id: Option<String>,
    /// Wall-clock instant the dispatch began. Subtracted in
    /// `response_duration_ms()` so response-phase middleware can record
    /// real observed latencies — closes the dogfooding gap where
    /// pre-handler middleware had to hardcode `latency_ms = 0`.
    pub(super) current_request_started: Option<std::time::Instant>,
    /// HTTP status the handler produced, set just before the `after { }`
    /// body of each applied middleware runs. Read via `response_status()`.
    /// `None` outside an `after` block.
    pub(super) current_response_status: Option<u16>,
    /// Per-request key-value bag written by `setContext` and read by `context`.
    pub(super) request_context: HashMap<String, Value>,
    pub(super) output: String,
    pub(super) depth: usize,
    /// Body of the current HTTP request (set by run_request)
    pub(super) request_body: Option<String>,
    /// Set when `serve(port)` is called from main()
    pub(super) serve_requested: Option<u16>,
    /// Module-level const declarations (borrowed from the program), evaluated
    /// lazily into `consts` by `init_consts`.
    pub(super) const_decls: Vec<&'a ConstDecl>,
    /// Evaluated const values keyed by lowercased const name. Frozen after
    /// `init_consts`; read-only at every `Var` site (locals shadow consts).
    pub(super) consts: HashMap<String, Value>,
}

impl<'a> Vm<'a> {
    pub(super) fn new(program: &'a Program) -> Self {
        // Functions: indexed by simple name AND fully-qualified name. The
        // simple-name slot is the legacy fast path for root-namespace calls
        // and most existing user code; the FQN slot disambiguates calls
        // across namespaces (e.g., `math.add()`).
        let mut functions: HashMap<String, &'a FunctionDecl> = HashMap::new();
        for function in &program.functions {
            let fqn = fqn_key(&function.namespace, &function.name);
            functions.insert(fqn.clone(), function);
            // Root-namespace functions also get a simple-name binding so
            // existing call sites like `call_function("main")` keep working.
            if function.namespace.is_empty() {
                functions.insert(function.name.to_lowercase(), function);
            }
        }

        let mut models: HashMap<String, &'a ModelDecl> = HashMap::new();
        for model in &program.models {
            if matches!(model.kind, ModelKind::Entity | ModelKind::Class) {
                let fqn = fqn_key(&model.namespace, &model.name);
                models.insert(fqn.clone(), model);
                // Always keep a short-name binding so `new EntityName()` and
                // DB lookups keep working even for non-root entities.
                models.insert(model.name.to_lowercase(), model);
            }
        }

        // Expand library routes per the program's `mount` declarations.
        // Root routes are always active; library routes only appear here if
        // a mount targets their namespace.
        let routes: Vec<RouteDecl> = expand_routes(program);

        let mut middlewares: HashMap<String, &'a MiddlewareDecl> = HashMap::new();
        for mw in &program.middlewares {
            let fqn = fqn_key(&mw.namespace, &mw.name);
            middlewares.insert(fqn, mw);
            middlewares.insert(mw.name.to_lowercase(), mw);
        }

        let mut pk_by_table = HashMap::new();
        for model in &program.models {
            if model.kind != ModelKind::Entity {
                continue;
            }
            let pks: Vec<String> = model
                .fields
                .iter()
                .filter(|f| f.is_primary_key)
                .map(|f| f.name.clone())
                .collect();
            pk_by_table.insert(model.name.to_lowercase(), pks.clone());
            pk_by_table.insert(crate::sql::to_snake_case(&model.name).to_lowercase(), pks);
        }

        let mut imports_by_namespace: HashMap<String, Vec<&'a ImportDecl>> = HashMap::new();
        for imp in &program.imports {
            let ns_key = imp
                .in_namespace
                .iter()
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
                .join(".");
            imports_by_namespace.entry(ns_key).or_default().push(imp);
        }

        Self {
            functions,
            models,
            routes,
            middlewares,
            error_handler: program.error_handler.as_ref(),
            imports_by_namespace,
            current_namespace_stack: Vec::new(),
            pk_by_table,
            dirty_fields: HashMap::new(),
            current_path_params: None,
            current_query_params: None,
            current_headers: None,
            current_method: None,
            current_request_path: None,
            current_request_id: None,
            current_request_started: None,
            current_response_status: None,
            request_context: HashMap::new(),
            output: String::new(),
            depth: 0,
            request_body: None,
            serve_requested: None,
            const_decls: program.consts.iter().collect(),
            consts: HashMap::new(),
        }
    }

    /// Evaluate module-level consts once at startup into `self.consts`.
    /// `validate_program` already guarantees the dependency graph is acyclic
    /// and that every `Var` ref names another const, so a fixpoint that defers
    /// a const until its const-deps are ready always terminates.
    pub(super) async fn init_consts(&mut self) -> Result<()> {
        let mut remaining: Vec<&'a ConstDecl> = self.const_decls.clone();
        let mut empty_vars: HashMap<String, Value> = HashMap::new();
        while !remaining.is_empty() {
            let mut progressed = false;
            let mut still = Vec::new();
            for decl in remaining {
                let deps_ready = const_var_refs(&decl.expr)
                    .iter()
                    .all(|d| self.consts.contains_key(&d.to_lowercase()));
                if deps_ready {
                    let v = self.eval_expr(&decl.expr, &mut empty_vars).await?;
                    self.consts.insert(decl.name.to_lowercase(), v);
                    progressed = true;
                } else {
                    still.push(decl);
                }
            }
            if !progressed {
                bail!("circular or unresolved const reference");
            }
            remaining = still;
        }
        Ok(())
    }

    /// Resolve a function name to a registered FunctionDecl using the
    /// current namespace stack (caller context) and imported namespaces.
    ///
    /// Priority:
    /// 1. If `name` contains a `.`, treat it as an explicit FQN.
    /// 2. Caller's own namespace.
    /// 3. Imported namespaces (from `using` statements in caller's file).
    /// 4. Root namespace (legacy backward-compat: bare names always reach root).
    fn resolve_function(&self, name: &str) -> Option<&'a FunctionDecl> {
        // Fast path: exact FQN match (works for both qualified calls and
        // root-namespace calls thanks to the dual binding in Vm::new).
        if let Some(f) = self.functions.get(&name.to_lowercase()) {
            return Some(*f);
        }
        // No literal dot — try caller-aware resolution.
        if !name.contains('.') {
            if let Some(caller_ns) = self.current_namespace_stack.last() {
                if !caller_ns.is_empty() {
                    let key = fqn_key(caller_ns, name);
                    if let Some(f) = self.functions.get(&key) {
                        return Some(*f);
                    }
                }
                // Try each import in caller's namespace.
                let ns_key = caller_ns
                    .iter()
                    .map(|s| s.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(".");
                if let Some(imports) = self.imports_by_namespace.get(&ns_key) {
                    for imp in imports {
                        let key = fqn_key(&imp.path, name);
                        if let Some(f) = self.functions.get(&key) {
                            return Some(*f);
                        }
                    }
                }
            }
        }
        None
    }

    /// Visibility check for cross-namespace calls. Same namespace: always ok.
    /// Different namespace: requires the callee to be Public.
    fn check_visibility(&self, callee: &FunctionDecl) -> Result<()> {
        let caller_ns = self
            .current_namespace_stack
            .last()
            .cloned()
            .unwrap_or_default();
        if caller_ns == callee.namespace {
            return Ok(());
        }
        if matches!(callee.visibility, Visibility::Public) {
            return Ok(());
        }
        bail!(
            "Function '{}' is private to namespace '{}' and cannot be called from '{}'",
            callee.name,
            if callee.namespace.is_empty() {
                "<root>".to_string()
            } else {
                callee.namespace.join(".")
            },
            if caller_ns.is_empty() {
                "<root>".to_string()
            } else {
                caller_ns.join(".")
            },
        );
    }

    #[async_recursion::async_recursion]
    pub(super) async fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>> {
        const MAX_DEPTH: usize = 256;
        if self.depth >= MAX_DEPTH {
            bail!("Call stack depth exceeded ({MAX_DEPTH})");
        }

        let function = self.resolve_function(name).ok_or_else(|| {
            let suggestion = closest_match(name, self.functions.keys());
            match suggestion {
                Some(s) => anyhow!("Unknown function: {name}. Did you mean '{s}'?"),
                None => anyhow!("Unknown function: {name}"),
            }
        })?;

        self.check_visibility(function)?;

        if function.params.len() != args.len() {
            bail!(
                "Function '{}' expects {} args but got {}",
                function.name,
                function.params.len(),
                args.len()
            );
        }

        self.depth += 1;
        self.current_namespace_stack
            .push(function.namespace.clone());

        let mut vars = HashMap::new();
        for (param, value) in function.params.iter().zip(args.into_iter()) {
            let value = self.check_param_type(param, value)?;
            vars.insert(param.name.to_lowercase(), value);
        }

        let saved_dirty = std::mem::take(&mut self.dirty_fields);
        let flow_result = self.exec_block(&function.body, &mut vars).await;
        self.dirty_fields = saved_dirty;
        let flow = flow_result?;
        self.depth -= 1;
        self.current_namespace_stack.pop();

        match flow {
            Flow::Continue => Ok(None),
            Flow::Return(v) => {
                let had_explicit_value = v.is_some();
                if let Some(return_ty) = &function.return_type {
                    let checked = self.check_typed_value(
                        &format!("return of function '{}'", function.name),
                        return_ty,
                        v.unwrap_or(Value::Null),
                    )?;
                    if !had_explicit_value && checked == Value::Null {
                        // `return;` with a typed function → caller gets Void.
                        Ok(None)
                    } else {
                        Ok(Some(checked))
                    }
                } else {
                    Ok(v)
                }
            }
            Flow::Break => bail!("'break' used outside loop"),
            Flow::ContinueLoop => bail!("'continue' used outside loop"),
        }
    }
}

pub(super) enum Flow {
    Continue,
    Return(Option<Value>),
    Break,
    ContinueLoop,
}

#[cfg(test)]
mod tests;

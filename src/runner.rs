use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value as JsonValue};
use tokio_postgres::types::ToSql;

use crate::ast::{
    AggregateKind, DbOrderBy, ErrorHandlerDecl, Expr, FunctionDecl, ImportDecl, MiddlewareDecl,
    ModelDecl, ModelKind, MountDecl, NavigationKind, Program, RouteDecl, RouteProtocol, SortDir,
    Stmt, TypedParam, ValidateField, ValidateRule, Visibility, WhereExpr,
};

/// Known error kinds for typed-catch dispatch (Phase 10.5).
///
/// `"Error"` is the catch-all super-kind — every error matches it, equivalent
/// to a bare `catch (e)` without a type annotation. Specific kinds match
/// only when the error chain looks like it came from that subsystem.
pub(crate) const JWC_ERROR_KINDS: &[&str] = &[
    "Error",
    "DbError",
    "HttpError",
    "ValidationError",
    "TimeoutError",
];

/// Classify an `anyhow::Error` into one of the well-known JWC error kinds.
///
/// v1 is pattern-based — it inspects the messages on the error chain looking
/// for signature substrings each subsystem leaves behind (deadpool/postgres
/// for DB, reqwest/http for HTTP, validate-body shape for Validation, tokio
/// timeout for Timeout). When nothing matches we return `"Error"`, which
/// also matches a `catch (e: Error)` clause and any untyped catch.
///
/// Future versions can switch to a typed `JwcError` enum and downcast on the
/// chain — keep the classifier signature stable so callers don't change.
pub(crate) fn classify_jwc_error(e: &anyhow::Error) -> &'static str {
    let mut msgs: Vec<String> = e.chain().map(|c| c.to_string().to_lowercase()).collect();
    let blob = msgs.join("\n");
    msgs.push(blob);
    let combined = msgs.join("\n");

    let has = |needles: &[&str]| -> bool { needles.iter().any(|n| combined.contains(n)) };

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

/// Returns true when an error of `kind` should be caught by a `catch (e: T)`
/// clause whose annotated type is `catch_type` (or any error if `catch_type`
/// is `None`). `"Error"` as the catch type matches everything, mirroring the
/// untyped catch.
pub(crate) fn catch_type_matches(catch_type: Option<&str>, kind: &str) -> bool {
    match catch_type {
        None => true,
        Some("Error") => true,
        Some(t) => t == kind,
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

use async_recursion::async_recursion;
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

fn http_client() -> &'static reqwest::Client {
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
    let _ = vm.call_function("main", Vec::new()).await?;
    Ok(RunMainResult {
        output: vm.output,
        serve_port: vm.serve_requested,
    })
}

/// Dispatch a single HTTP request to the matching route and return (status_code, body).
/// Convenience wrapper around `run_request_with_headers` without headers. Kept
/// public so test code can keep its concise call shape.
#[allow(dead_code)]
pub async fn run_request(
    program: &Program,
    method: &str,
    path: &str,
    body: Option<String>,
) -> Result<(u16, String)> {
    run_request_with_headers(program, method, path, body, HashMap::new()).await
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
pub async fn run_request_with_headers(
    program: &Program,
    method: &str,
    path: &str,
    body: Option<String>,
    headers: HashMap<String, String>,
) -> Result<(u16, String)> {
    let mut vm = Vm::new(program);
    vm.request_body = body;
    vm.current_headers = Some(headers);
    vm.dispatch_route(method, path).await
}

/// Invoke a JWC function by name with a single string payload. Used by the
/// background job queue: workers receive `Job { name, payload }`, look up
/// the registered handler function name, then call this. Any return value
/// is discarded — handlers communicate via side effects (db, cache, email).
pub async fn run_handler(program: &Program, function_name: &str, payload: String) -> Result<()> {
    let mut vm = Vm::new(program);
    vm.call_function(function_name, vec![Value::Str(payload)])
        .await?;
    Ok(())
}

struct Vm<'a> {
    functions: HashMap<String, &'a FunctionDecl>,
    /// Model schema map (entity + class) for runtime JSON validation on typed params/returns.
    models: HashMap<String, &'a ModelDecl>,
    /// Expanded, owned route set produced by `expand_routes(program)`.
    /// Each mount on a library namespace contributes its own copies here,
    /// already prefixed and middleware-chained.
    routes: Vec<RouteDecl>,
    middlewares: HashMap<String, &'a MiddlewareDecl>,
    error_handler: Option<&'a ErrorHandlerDecl>,
    /// All imports in the program, indexed by the namespace they live in.
    imports_by_namespace: HashMap<String, Vec<&'a ImportDecl>>,
    /// Namespace of the currently-executing function. Each `call_function`
    /// pushes the callee's namespace; the previous value is restored on
    /// return. Empty Vec = root namespace.
    current_namespace_stack: Vec<Vec<String>>,
    /// Primary-key columns per table (keyed by both `Entity` name and its snake_case form).
    /// Empty Vec means no `pk` declared on that entity — falls back to a single `"id"` column.
    pk_by_table: HashMap<String, Vec<String>>,
    /// Per-call dirty field tracking: var name (lower-cased) → fields assigned
    /// via `var.field = ...` since the variable entered scope. Used by
    /// `update var in ...` to SET only the modified columns.
    dirty_fields: HashMap<String, HashSet<String>>,
    current_path_params: Option<HashMap<String, String>>,
    /// Query-string params parsed from the request URL (`?a=1&b=hello`).
    current_query_params: Option<HashMap<String, String>>,
    /// Request headers (lower-cased keys) for `header(name)` look-ups.
    current_headers: Option<HashMap<String, String>>,
    /// Per-request key-value bag written by `setContext` and read by `context`.
    request_context: HashMap<String, Value>,
    output: String,
    depth: usize,
    /// Body of the current HTTP request (set by run_request)
    request_body: Option<String>,
    /// Set when `serve(port)` is called from main()
    serve_requested: Option<u16>,
}

impl<'a> Vm<'a> {
    fn new(program: &'a Program) -> Self {
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
            request_context: HashMap::new(),
            output: String::new(),
            depth: 0,
            request_body: None,
            serve_requested: None,
        }
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

    #[async_recursion]
    async fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Option<Value>> {
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

    fn check_param_type(&self, param: &TypedParam, value: Value) -> Result<Value> {
        let ty = match &param.ty {
            None => return Ok(value),
            Some(t) => t,
        };
        self.check_typed_value(&format!("parameter '{}'", param.name), ty, value)
    }

    fn check_typed_value(&self, subject: &str, ty: &str, value: Value) -> Result<Value> {
        // Strip trailing `?` nullable marker
        let (base, nullable_marker) = match ty.strip_suffix('?') {
            Some(stripped) => (stripped, true),
            None => (ty, false),
        };

        // Desugar `Optional<T>` → same nullable semantics as `T?`
        let (base, nullable) = if let Some(inner) = strip_generic_wrapper(base, "Optional") {
            (inner.to_string(), true)
        } else {
            (base.to_string(), nullable_marker)
        };

        if nullable {
            if matches!(value, Value::Null) {
                return Ok(Value::Null);
            }
            if let Value::Str(s) = &value {
                if s == "null" {
                    return Ok(Value::Null);
                }
            }
        }

        // List<T> — JSON array where each element matches T
        if let Some(elem_ty) = strip_generic_wrapper(&base, "List") {
            return self.check_list_value(subject, elem_ty, value);
        }

        match base.as_str() {
            "string" | "str" => match &value {
                Value::Str(_) => Ok(value),
                Value::Int(n) => Ok(Value::Str(n.to_string())),
                Value::Float(n) => Ok(Value::Str(format_float(*n))),
                _ => bail!(
                    "Type error: {subject} expects string, got {}",
                    value.type_name()
                ),
            },
            "int" | "integer" | "number" | "bigint" => match &value {
                Value::Int(_) => Ok(value),
                Value::Float(n) if n.fract() == 0.0 => Ok(Value::Int(*n as i64)),
                Value::Str(s) => s.parse::<i64>().map(Value::Int).map_err(|_| {
                    anyhow!("Type error: {subject} expects int, got string \"{}\"", s)
                }),
                _ => bail!(
                    "Type error: {subject} expects int, got {}",
                    value.type_name()
                ),
            },
            "double" | "float" => match &value {
                Value::Float(_) => Ok(value),
                Value::Int(n) => Ok(Value::Float(*n as f64)),
                Value::Str(s) => s.parse::<f64>().map(Value::Float).map_err(|_| {
                    anyhow!("Type error: {subject} expects double, got string \"{}\"", s)
                }),
                _ => bail!(
                    "Type error: {subject} expects double, got {}",
                    value.type_name()
                ),
            },
            "bool" | "boolean" => match &value {
                Value::Bool(_) => Ok(value),
                _ => bail!(
                    "Type error: {subject} expects bool, got {}",
                    value.type_name()
                ),
            },
            "uuid" => match &value {
                Value::Str(s) if looks_like_uuid(s) => Ok(value),
                Value::Str(s) => bail!("Type error: {subject} expects uuid, got string \"{s}\""),
                _ => bail!(
                    "Type error: {subject} expects uuid, got {}",
                    value.type_name()
                ),
            },
            "datetime" | "timestamp" => match &value {
                Value::Str(s) if looks_like_datetime(s) => Ok(value),
                Value::Str(s) => {
                    bail!("Type error: {subject} expects datetime (ISO 8601), got string \"{s}\"")
                }
                _ => bail!(
                    "Type error: {subject} expects datetime, got {}",
                    value.type_name()
                ),
            },
            "decimal" => match &value {
                Value::Float(_) | Value::Int(_) => Ok(value),
                Value::Str(s) if s.parse::<f64>().is_ok() => Ok(Value::Str(s.clone())),
                Value::Str(s) => bail!("Type error: {subject} expects decimal, got string \"{s}\""),
                _ => bail!(
                    "Type error: {subject} expects decimal, got {}",
                    value.type_name()
                ),
            },
            "json" => match &value {
                Value::Str(s) => {
                    if serde_json::from_str::<JsonValue>(s).is_ok() {
                        Ok(value)
                    } else {
                        bail!("Type error: {subject} expects json, got non-json string")
                    }
                }
                Value::Null => Ok(value),
                _ => bail!(
                    "Type error: {subject} expects json, got {}",
                    value.type_name()
                ),
            },
            // `bytes` / `byte[]` cross the JSON boundary as a base64
            // string. Accept any string for now (length / charset
            // validation lands with a real `Value::Bytes` variant in a
            // follow-up sprint); reject everything else with a clear
            // message so the surprise surface is small.
            "bytes" | "byte[]" => match &value {
                Value::Str(_) => Ok(value),
                _ => bail!(
                    "Type error: {subject} expects bytes (base64 string), got {}",
                    value.type_name()
                ),
            },
            model_ty => {
                let model = self.models.get(&model_ty.to_lowercase());
                if model.is_none() {
                    return Ok(value);
                }

                match value {
                    Value::Null => Ok(Value::Null),
                    Value::Str(raw) => {
                        let parsed: JsonValue = serde_json::from_str(&raw).map_err(|_| {
                            anyhow!("Type error: {subject} expects {model_ty}, got non-json string")
                        })?;

                        self.validate_json_against_model(subject, model.unwrap(), &parsed)?;
                        Ok(Value::Str(parsed.to_string()))
                    }
                    other => bail!(
                        "Type error: {subject} expects {}, got {}",
                        model_ty,
                        other.type_name()
                    ),
                }
            }
        }
    }

    fn check_list_value(&self, subject: &str, elem_ty: &str, value: Value) -> Result<Value> {
        let raw = match value {
            Value::Str(s) => s,
            Value::Null => bail!("Type error: {subject} expects List<{elem_ty}>, got null"),
            other => bail!(
                "Type error: {subject} expects List<{elem_ty}>, got {}",
                other.type_name()
            ),
        };

        let parsed: JsonValue = serde_json::from_str(&raw)
            .map_err(|_| anyhow!("Type error: {subject} expects List<{elem_ty}>, got non-json"))?;

        let arr = parsed.as_array().ok_or_else(|| {
            anyhow!("Type error: {subject} expects List<{elem_ty}>, got non-array")
        })?;

        for (i, item) in arr.iter().enumerate() {
            if !self.json_value_matches_type(item, elem_ty) {
                bail!(
                    "Type error: {subject} expects List<{elem_ty}>, item at index {i} is not {elem_ty}"
                );
            }
        }

        Ok(Value::Str(parsed.to_string()))
    }

    fn validate_json_against_model(
        &self,
        subject: &str,
        model: &ModelDecl,
        value: &JsonValue,
    ) -> Result<()> {
        if let Some(arr) = value.as_array() {
            for item in arr {
                self.validate_model_object(subject, model, item)?;
            }
            return Ok(());
        }

        self.validate_model_object(subject, model, value)
    }

    fn validate_model_object(
        &self,
        subject: &str,
        model: &ModelDecl,
        value: &JsonValue,
    ) -> Result<()> {
        let Some(obj) = value.as_object() else {
            bail!(
                "Type error: {subject} expects {}, got non-object json",
                model.name
            );
        };

        for field in &model.fields {
            let Some(v) = obj.get(&field.name) else {
                if field.is_nullable {
                    continue;
                }
                bail!(
                    "Type error: {subject} expects field '{}' for {}",
                    field.name,
                    model.name
                );
            };

            if v.is_null() {
                if !field.is_nullable {
                    bail!(
                        "Type error: {subject} field '{}' cannot be null for {}",
                        field.name,
                        model.name
                    );
                }
                continue;
            }

            if !self.json_value_matches_type(v, &field.ty.name) {
                bail!(
                    "Type error: {subject} field '{}' has invalid type for {}",
                    field.name,
                    model.name
                );
            }
        }

        Ok(())
    }

    fn json_value_matches_type(&self, value: &JsonValue, type_name: &str) -> bool {
        let (base, nullable) = match type_name.strip_suffix('?') {
            Some(stripped) => (stripped, true),
            None => (type_name, false),
        };

        if nullable && value.is_null() {
            return true;
        }

        match base.to_ascii_lowercase().as_str() {
            "string" | "str" | "text" | "varchar" => value.is_string(),
            "uuid" => value.as_str().map(looks_like_uuid).unwrap_or(false),
            "datetime" | "timestamp" => value.as_str().map(looks_like_datetime).unwrap_or(false),
            "int" | "integer" | "number" | "bigint" => {
                value.as_i64().is_some() || value.as_u64().is_some()
            }
            "double" | "float" | "decimal" => value.is_number(),
            "bool" | "boolean" => value.is_boolean(),
            "json" => value.is_object() || value.is_array(),
            // bytes / byte[]: payloads cross JSON as base64 strings. We
            // accept any string here and leave decoding to user code (or
            // a future `decode_base64()` helper). Length / charset
            // validation is intentionally lax for now — Phase 2.1 v2 will
            // tighten this when a real `bytes` Value variant lands.
            "bytes" | "byte[]" => value.is_string(),
            other => {
                if let Some(model) = self.models.get(other) {
                    return self
                        .validate_json_against_model("nested model", model, value)
                        .is_ok();
                }
                true
            }
        }
    }

    #[async_recursion]
    async fn exec_block(
        &mut self,
        stmts: &[Stmt],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Flow> {
        for stmt in stmts {
            let flow = self.exec_stmt(stmt, vars).await?;
            if !matches!(flow, Flow::Continue) {
                return Ok(flow);
            }
        }
        Ok(Flow::Continue)
    }

    #[async_recursion]
    async fn exec_stmt(&mut self, stmt: &Stmt, vars: &mut HashMap<String, Value>) -> Result<Flow> {
        match stmt {
            Stmt::Let { name, value } => {
                let key = name.to_lowercase();
                if vars.contains_key(&key) {
                    bail!("Duplicate variable declaration: {name}");
                }
                let evaluated = self.eval_expr(value, vars).await?;
                vars.insert(key.clone(), evaluated);
                // New binding has no modified fields yet.
                self.dirty_fields.remove(&key);
                Ok(Flow::Continue)
            }
            Stmt::Assign { name, value } => {
                let key = name.to_lowercase();
                if !vars.contains_key(&key) {
                    bail!("Assignment to undefined variable: {name}");
                }
                let evaluated = self.eval_expr(value, vars).await?;
                vars.insert(key.clone(), evaluated);
                // Full rebind resets the modified-field set.
                self.dirty_fields.remove(&key);
                Ok(Flow::Continue)
            }
            Stmt::Print(expr) => {
                let value = self.eval_expr(expr, vars).await?;
                self.output.push_str(&value.as_string());
                self.output.push('\n');
                Ok(Flow::Continue)
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond_value = self.eval_expr(cond, vars).await?;
                match cond_value {
                    Value::Bool(true) => self.exec_block(then_body, vars).await,
                    Value::Bool(false) => {
                        if let Some(else_body) = else_body {
                            self.exec_block(else_body, vars).await
                        } else {
                            Ok(Flow::Continue)
                        }
                    }
                    other => bail!("if condition must be bool, got {}", other.type_name()),
                }
            }
            Stmt::While { cond, body } => {
                const MAX_ITERS: usize = 100_000;
                for _ in 0..MAX_ITERS {
                    let cond_value = self.eval_expr(cond, vars).await?;
                    match cond_value {
                        Value::Bool(true) => match self.exec_block(body, vars).await? {
                            Flow::Continue => {}
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Break => return Ok(Flow::Continue),
                            Flow::ContinueLoop => continue,
                        },
                        Value::Bool(false) => return Ok(Flow::Continue),
                        other => bail!("while condition must be bool, got {}", other.type_name()),
                    }
                }
                bail!("while loop exceeded iteration limit ({MAX_ITERS})")
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::ContinueLoop),
            Stmt::Expr(expr) => {
                let _ = self.eval_expr(expr, vars).await?;
                Ok(Flow::Continue)
            }
            Stmt::Return(None) => Ok(Flow::Return(None)),
            Stmt::Return(Some(expr)) => {
                let value = self.eval_expr(expr, vars).await?;
                Ok(Flow::Return(Some(value)))
            }
            Stmt::FieldAssign { var, field, value } => {
                let key = var.to_lowercase();
                let current = vars
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| anyhow!("FieldAssign: variable '{}' not found", var))?;
                let new_val = self.eval_expr(value, vars).await?;
                let json_str = match current {
                    Value::Str(s) => s,
                    Value::Null => "{}".to_string(),
                    other => bail!(
                        "FieldAssign: '{}' is not an object, got {}",
                        var,
                        other.type_name()
                    ),
                };
                let mut doc: serde_json::Value = serde_json::from_str(&json_str)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                if let Some(obj) = doc.as_object_mut() {
                    obj.insert(field.clone(), value_to_json(&new_val));
                }
                vars.insert(key.clone(), Value::Str(doc.to_string()));
                self.dirty_fields
                    .entry(key)
                    .or_default()
                    .insert(field.clone());
                Ok(Flow::Continue)
            }
            Stmt::Try {
                body,
                catch_var,
                catch_type,
                catch_body,
            } => {
                let try_result = self.exec_block(body, vars).await;
                match try_result {
                    Ok(flow) => Ok(flow),
                    Err(e) => {
                        let kind = classify_jwc_error(&e);
                        if !catch_type_matches(catch_type.as_deref(), kind) {
                            // Annotated `catch (e: T)` clause that doesn't match
                            // this error kind — re-raise so an outer handler
                            // (or the errorHandler) gets a shot at it.
                            return Err(e);
                        }
                        let mut all_msgs: Vec<String> = e.chain().map(|c| c.to_string()).collect();
                        let message = if all_msgs.is_empty() {
                            "unknown error".to_string()
                        } else {
                            all_msgs.remove(0)
                        };
                        let err_obj = json!({
                            "type": kind,
                            "message": message,
                            "causes": all_msgs,
                        });
                        let key = catch_var.to_lowercase();
                        // Inject the error binding; restore the prior value (if any)
                        // when leaving the catch block.
                        let prior = vars.insert(key.clone(), Value::Str(err_obj.to_string()));
                        let catch_flow = self.exec_block(catch_body, vars).await;
                        match prior {
                            Some(v) => {
                                vars.insert(key, v);
                            }
                            None => {
                                vars.remove(&key);
                            }
                        }
                        catch_flow
                    }
                }
            }
            Stmt::Transaction { body } => {
                let body_clone = body.clone();
                // Run the body inside an engine-level transaction scope. The
                // body is a future capturing `self` and `vars` by mutable
                // reference, which is fine because `with_tx` polls it
                // immediately to completion.
                let outcome: Result<Flow> = engine::with_tx(|| {
                    Box::pin(async move { self.exec_block(&body_clone, vars).await })
                })
                .await;
                outcome
            }
            Stmt::ForIn { var, iter, body } => {
                let iter_val = self.eval_expr(iter, vars).await?;
                let raw = match iter_val {
                    Value::Str(s) => s,
                    Value::Null => return Ok(Flow::Continue),
                    other => bail!(
                        "for-in: iter must be a JSON array string, got {}",
                        other.type_name()
                    ),
                };
                let parsed: JsonValue = serde_json::from_str(&raw)
                    .map_err(|_| anyhow!("for-in: iter is not valid JSON"))?;
                let items = parsed
                    .as_array()
                    .ok_or_else(|| anyhow!("for-in: iter is not a JSON array"))?
                    .clone();
                let key = var.to_lowercase();
                let prior = vars.get(&key).cloned();
                let prior_dirty = self.dirty_fields.remove(&key);
                let mut early_return: Option<Option<Value>> = None;
                for item in items.iter() {
                    vars.insert(key.clone(), json_to_value(item));
                    match self.exec_block(body, vars).await? {
                        Flow::Continue | Flow::ContinueLoop => continue,
                        Flow::Break => break,
                        Flow::Return(v) => {
                            early_return = Some(v);
                            break;
                        }
                    }
                }
                match prior {
                    Some(p) => {
                        vars.insert(key.clone(), p);
                    }
                    None => {
                        vars.remove(&key);
                    }
                }
                if let Some(d) = prior_dirty {
                    self.dirty_fields.insert(key, d);
                }
                if let Some(v) = early_return {
                    return Ok(Flow::Return(v));
                }
                Ok(Flow::Continue)
            }
            Stmt::ValidateBody { fields } => {
                let body = self.request_body.clone().unwrap_or_default();
                let parsed: JsonValue = if body.trim().is_empty() {
                    JsonValue::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&body)
                        .with_context(|| "validate body: request body is not valid JSON")?
                };

                let errors = run_validation_rules(fields, &parsed);
                if !errors.is_empty() {
                    let mut error_doc = serde_json::Map::new();
                    error_doc.insert("status".into(), json!(400));
                    error_doc.insert("errors".into(), JsonValue::Object(errors));
                    return Ok(Flow::Return(Some(Value::Str(
                        JsonValue::Object(error_doc).to_string(),
                    ))));
                }

                Ok(Flow::Continue)
            }
            Stmt::DbInsert {
                var,
                context_var: _,
                table,
            } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let (sql, boxed_params) = build_insert_sql(&table_name, &json_str)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let returned = engine::query_text(&sql, &param_refs).await?;
                if !returned.is_empty() && returned != "null" {
                    vars.insert(var.to_lowercase(), Value::Str(returned));
                }
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbUpdate {
                var,
                context_var: _,
                table,
            } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let pk_fields = self.resolve_pk_fields(table);
                let key = var.to_lowercase();
                let dirty_snapshot = self.dirty_fields.get(&key).cloned();
                let (sql, boxed_params) =
                    build_update_sql(&table_name, &json_str, &pk_fields, dirty_snapshot.as_ref())?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let returned = engine::query_text(&sql, &param_refs).await?;
                if !returned.is_empty() && returned != "null" {
                    vars.insert(key.clone(), Value::Str(returned));
                }
                // Persisted to DB → object is now clean.
                self.dirty_fields.remove(&key);
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbDelete {
                var,
                context_var: _,
                table,
            } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let pk_fields = self.resolve_pk_fields(table);
                let (sql, boxed_params) = build_delete_sql(&table_name, &json_str, &pk_fields)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let _ = engine::exec(&sql, &param_refs).await?;
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbDeleteWhere {
                context_var: _,
                table,
                where_clause,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let mut shape_bits: Vec<String> = Vec::new();
                let mut cache_bits: Vec<String> = Vec::new();
                let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
                let where_sql = build_where_sql(
                    where_clause,
                    &mut params,
                    &mut shape_bits,
                    &mut cache_bits,
                    vars,
                    self,
                )
                .await?;
                let sql = format!("DELETE FROM \"{}\" WHERE {};", table_name, where_sql);
                let param_refs = boxed_params_to_refs(&params);
                let _ = engine::exec(&sql, &param_refs).await?;
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
        }
    }

    #[async_recursion]
    async fn eval_expr(&mut self, expr: &Expr, vars: &mut HashMap<String, Value>) -> Result<Value> {
        match expr {
            Expr::Int(v) => Ok(Value::Int(*v)),
            Expr::Float(v) => {
                let parsed = v
                    .parse::<f64>()
                    .map_err(|_| anyhow!("Invalid float literal: {v}"))?;
                Ok(Value::Float(parsed))
            }
            Expr::Str(v) => Ok(Value::Str(v.clone())),
            Expr::Bool(v) => Ok(Value::Bool(*v)),
            Expr::Null => Ok(Value::Null),
            Expr::Var(name) => vars.get(&name.to_lowercase()).cloned().ok_or_else(|| {
                let suggestion = closest_match(name, vars.keys());
                match suggestion {
                    Some(s) => anyhow!("Undefined variable: {name}. Did you mean '{s}'?"),
                    None => anyhow!("Undefined variable: {name}"),
                }
            }),
            Expr::NewEntity { entity: _ } => Ok(Value::Str("{}".to_string())),
            Expr::FieldGet { var, field } => {
                let obj_val = vars
                    .get(&var.to_lowercase())
                    .cloned()
                    .ok_or_else(|| anyhow!("Undefined variable: {var}"))?;
                match obj_val {
                    Value::Str(s) => {
                        let doc: serde_json::Value = serde_json::from_str(&s)
                            .with_context(|| format!("FieldGet: '{}' is not valid JSON", var))?;
                        match doc.get(field.as_str()) {
                            Some(serde_json::Value::String(s)) => Ok(Value::Str(s.clone())),
                            Some(serde_json::Value::Number(n)) => {
                                if let Some(i) = n.as_i64() {
                                    Ok(Value::Int(i))
                                } else if let Some(f) = n.as_f64() {
                                    Ok(Value::Float(f))
                                } else {
                                    Ok(Value::Null)
                                }
                            }
                            Some(serde_json::Value::Bool(b)) => Ok(Value::Bool(*b)),
                            Some(serde_json::Value::Null) | None => Ok(Value::Null),
                            Some(v) => Ok(Value::Str(v.to_string())),
                        }
                    }
                    Value::Null => Ok(Value::Null),
                    other => bail!(
                        "FieldGet: '{}' is not a JSON object, got {}",
                        var,
                        other.type_name()
                    ),
                }
            }
            Expr::DbAggregate {
                kind,
                field,
                context_var: _,
                table,
                where_clause,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let col = field_path_to_col(field);
                let agg_sql = match kind {
                    AggregateKind::Sum => format!("SUM(\"{}\")", col),
                    AggregateKind::Avg => format!("AVG(\"{}\")", col),
                    AggregateKind::Min => format!("MIN(\"{}\")", col),
                    AggregateKind::Max => format!("MAX(\"{}\")", col),
                };
                let kind_tag = match kind {
                    AggregateKind::Sum => "sum",
                    AggregateKind::Avg => "avg",
                    AggregateKind::Min => "min",
                    AggregateKind::Max => "max",
                };

                let mut shape_bits: Vec<String> = vec![format!("agg:{kind_tag}:{col}")];
                let mut cache_bits: Vec<String> = vec![format!("agg:{kind_tag}:{col}")];
                let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

                let where_sql = if let Some(wc) = where_clause {
                    let s = build_where_sql(
                        wc,
                        &mut params,
                        &mut shape_bits,
                        &mut cache_bits,
                        vars,
                        self,
                    )
                    .await?;
                    format!(" WHERE {}", s)
                } else {
                    String::new()
                };

                let sql = format!(
                    "SELECT ({})::text FROM \"{}\"{};",
                    agg_sql, table_name, where_sql
                );
                let shape_key = format!("aggregate|table:{table_name}|{}", shape_bits.join("|"));
                let cache_key = format!(
                    "result|aggregate|table:{table_name}|{}",
                    cache_bits.join("|")
                );

                let param_refs = boxed_params_to_refs(&params);
                let compiled = engine::get_or_compile_sql(&shape_key, || Ok(sql.clone()))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled, &param_refs)
                        .await?;
                let trimmed = result.trim();
                if trimmed.is_empty() || trimmed == "null" {
                    return Ok(Value::Null);
                }
                if let Ok(i) = trimmed.parse::<i64>() {
                    return Ok(Value::Int(i));
                }
                if let Ok(f) = trimmed.parse::<f64>() {
                    return Ok(Value::Float(f));
                }
                Ok(Value::Str(trimmed.to_string()))
            }
            Expr::DbCount {
                context_var: _,
                table,
                where_clause,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let mut shape_bits: Vec<String> = Vec::new();
                let mut cache_bits: Vec<String> = Vec::new();
                let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

                let where_sql = if let Some(wc) = where_clause {
                    let s = build_where_sql(
                        wc,
                        &mut params,
                        &mut shape_bits,
                        &mut cache_bits,
                        vars,
                        self,
                    )
                    .await?;
                    format!(" WHERE {}", s)
                } else {
                    String::new()
                };

                let sql = format!(
                    "SELECT COUNT(*)::text FROM \"{}\"{};",
                    table_name, where_sql
                );
                let shape_key = format!(
                    "count|table:{table_name}|{}",
                    if shape_bits.is_empty() {
                        "no_where".to_string()
                    } else {
                        shape_bits.join("|")
                    }
                );
                let cache_key = format!(
                    "result|count|table:{table_name}|{}",
                    if cache_bits.is_empty() {
                        "no_where".to_string()
                    } else {
                        cache_bits.join("|")
                    }
                );

                let param_refs = boxed_params_to_refs(&params);
                let compiled = engine::get_or_compile_sql(&shape_key, || Ok(sql.clone()))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled, &param_refs)
                        .await?;
                let n = result.trim().parse::<i64>().with_context(|| {
                    format!("count(*): expected integer text, got '{}'", result.trim())
                })?;
                Ok(Value::Int(n))
            }
            Expr::DbSelect {
                entity,
                context_var: _,
                table,
                where_clause,
                order_by,
                limit,
                offset,
                first,
                with_relations,
                projection,
                group_by,
                having,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let nav_subqueries = build_navigation_subqueries(
                    entity,
                    &table_name,
                    with_relations,
                    &self.models,
                    &self.pk_by_table,
                )?;
                let (sql, boxed_params, shape_key, cache_key) = build_select_sql(
                    table_name,
                    where_clause.as_deref(),
                    order_by.as_ref(),
                    limit.as_deref(),
                    offset.as_deref(),
                    *first,
                    &nav_subqueries,
                    projection,
                    group_by,
                    having.as_deref(),
                    vars,
                    self,
                )
                .await?;
                // Cache hit short-circuit: skip get_or_compile_sql + DB roundtrip.
                if let Some(cached) = engine::try_cached_result(&cache_key)? {
                    return if cached == "null" || cached.is_empty() {
                        Ok(Value::Null)
                    } else {
                        Ok(Value::Str(cached))
                    };
                }
                let param_refs = boxed_params_to_refs(&boxed_params);
                let compiled_sql = engine::get_or_compile_sql(&shape_key, || Ok(sql))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled_sql, &param_refs)
                        .await?;
                if result == "null" || result.is_empty() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Str(result))
                }
            }
            Expr::Call { name, args } => {
                if name.eq_ignore_ascii_case("dispatch") {
                    return self.eval_dispatch_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("path_param") {
                    return self.eval_path_param_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("query_param") {
                    return self.eval_query_param_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("header") {
                    return self.eval_header_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("context") {
                    return self.eval_context_get_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("setContext")
                    || name.eq_ignore_ascii_case("set_context")
                {
                    return self.eval_context_set_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("http_get") {
                    return self.eval_http_get_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("http_post") {
                    return self.eval_http_post_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("jwt_sign") {
                    return self.eval_jwt_sign_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("jwt_verify") {
                    return self.eval_jwt_verify_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("hash_password") {
                    return self.eval_hash_password_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("ws_send") {
                    return self.eval_ws_send_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("ws_recv") {
                    return self.eval_ws_recv_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("ws_close") {
                    return self.eval_ws_close_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("verify_password") {
                    return self.eval_verify_password_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_get") {
                    return self.eval_cache_get_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_set") {
                    return self.eval_cache_set_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_del") {
                    return self.eval_cache_del_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_clear") {
                    return self.eval_cache_clear_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("send_email") {
                    return self.eval_send_email_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("register_job_handler") {
                    return self.eval_register_job_handler_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("enqueue") {
                    return self.eval_enqueue_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("enqueue_urgent") {
                    return self.eval_enqueue_urgent_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("job_count") {
                    return self.eval_job_count_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("dlq_count") {
                    return self.eval_dlq_count_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("dlq_drain") {
                    return self.eval_dlq_drain_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("unauthorized") {
                    return Ok(Value::Str(
                        r#"{"status":401,"error":"Unauthorized"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("forbidden") {
                    return Ok(Value::Str(
                        r#"{"status":403,"error":"Forbidden"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("db_query") {
                    return self.eval_db_query_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("raw_sql") {
                    return self.eval_raw_sql_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("request_body") {
                    return self.eval_request_body_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("uuid") {
                    return self.eval_uuid_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("length") || name.eq_ignore_ascii_case("len") {
                    return self.eval_length_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("lower") {
                    return self
                        .eval_string_call(args, vars, "lower", |s| s.to_lowercase())
                        .await;
                }
                if name.eq_ignore_ascii_case("upper") {
                    return self
                        .eval_string_call(args, vars, "upper", |s| s.to_uppercase())
                        .await;
                }
                if name.eq_ignore_ascii_case("trim") {
                    return self
                        .eval_string_call(args, vars, "trim", |s| s.trim().to_string())
                        .await;
                }
                if name.eq_ignore_ascii_case("contains") {
                    return self.eval_contains_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("starts_with") {
                    return self
                        .eval_two_string_bool_call(args, vars, "starts_with", |s, p| {
                            s.starts_with(p)
                        })
                        .await;
                }
                if name.eq_ignore_ascii_case("ends_with") {
                    return self
                        .eval_two_string_bool_call(args, vars, "ends_with", |s, p| s.ends_with(p))
                        .await;
                }
                if name.eq_ignore_ascii_case("replace") {
                    return self.eval_replace_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("split") {
                    return self.eval_split_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("first") {
                    return self.eval_first_or_last_call(args, vars, true).await;
                }
                if name.eq_ignore_ascii_case("last") {
                    return self.eval_first_or_last_call(args, vars, false).await;
                }
                if name.eq_ignore_ascii_case("json_parse") {
                    return self.eval_json_parse_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("json_stringify") {
                    return self.eval_json_stringify_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("now") {
                    if !args.is_empty() {
                        bail!("now() expects no args");
                    }
                    return Ok(Value::Str(current_utc_iso8601()?));
                }

                if name.eq_ignore_ascii_case("unix_timestamp") {
                    if !args.is_empty() {
                        bail!("unix_timestamp() expects no args");
                    }
                    let secs = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| anyhow!("System clock error"))?
                        .as_secs() as i64;
                    return Ok(Value::Int(secs));
                }

                if name.eq_ignore_ascii_case("set_json_field") {
                    return self.eval_set_json_field_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("body") {
                    return self.eval_request_body_call(args, vars).await;
                }

                // ── `serve(port?)` — starts HTTP server from main() ───────
                if name.eq_ignore_ascii_case("serve") {
                    let port: u16 = if let Some(arg) = args.first() {
                        match self.eval_expr(arg, vars).await? {
                            Value::Int(n) if n > 0 && n <= 65535 => n as u16,
                            Value::Int(n) => bail!("serve(): invalid port {n}"),
                            other => {
                                bail!("serve(port): port must be int, got {}", other.type_name())
                            }
                        }
                    } else {
                        8080
                    };
                    self.serve_requested = Some(port);
                    return Ok(Value::Void);
                }

                // ── `env("VAR_NAME")` — read an environment variable ──────
                if name.eq_ignore_ascii_case("env") {
                    if args.len() != 1 {
                        bail!("env(name) expects exactly 1 arg");
                    }
                    let var_name = self.eval_expr(&args[0], vars).await?;
                    let var_name = match var_name {
                        Value::Str(s) => s,
                        other => bail!("env(name): name must be string, got {}", other.type_name()),
                    };
                    let val = std::env::var(&var_name).unwrap_or_default();
                    return Ok(Value::Str(val));
                }

                // ── `setConnectionString(...)` — pin DATABASE_URL for this process.
                //
                // Three legal forms:
                //   setConnectionString();                           // pull from env (.env auto-loaded)
                //   setConnectionString("postgres://user:p@h:port/db");
                //   setConnectionString({
                //       host: "localhost", port: 5432,
                //       user: "postgres",  password: "x",
                //       database: "myapp"
                //   });
                if name.eq_ignore_ascii_case("setConnectionString")
                    || name.eq_ignore_ascii_case("set_connection_string")
                {
                    return self.eval_set_connection_string_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("sleep_ms") {
                    if args.len() != 1 {
                        bail!("sleep_ms(n) expects exactly 1 arg");
                    }
                    let n = match self.eval_expr(&args[0], vars).await? {
                        Value::Int(n) if n >= 0 => n as u64,
                        Value::Int(n) => bail!("sleep_ms(n): n must be >= 0, got {n}"),
                        other => bail!("sleep_ms(n): n must be int, got {}", other.type_name()),
                    };
                    tokio::time::sleep(std::time::Duration::from_millis(n)).await;
                    return Ok(Value::Null);
                }

                if name.eq_ignore_ascii_case("fetch_json") {
                    if args.len() != 1 {
                        bail!("fetch_json(url) expects exactly 1 arg");
                    }
                    let url = match self.eval_expr(&args[0], vars).await? {
                        Value::Str(s) => s,
                        other => bail!(
                            "fetch_json(url): url must be string, got {}",
                            other.type_name()
                        ),
                    };
                    let resp = http_client()
                        .get(&url)
                        .send()
                        .await
                        .map_err(|e| anyhow!("fetch_json({url}) failed: {e}"))?;
                    let body = resp
                        .text()
                        .await
                        .map_err(|e| anyhow!("fetch_json: read body failed: {e}"))?;
                    let parsed: JsonValue = serde_json::from_str(&body)
                        .map_err(|e| anyhow!("fetch_json: invalid JSON: {e}"))?;
                    return Ok(json_to_value(&parsed));
                }

                // ── HTTP response helpers ──────────────────────────────────
                if name.eq_ignore_ascii_case("json") {
                    if args.len() != 1 {
                        bail!("json(val) expects exactly 1 arg");
                    }
                    let val = self.eval_expr(&args[0], vars).await?;
                    // Hot path: DB selects already return Value::Str(json_text).
                    // Reuse it instead of cloning the (often-large) buffer.
                    return Ok(match val {
                        Value::Str(_) => val,
                        other => Value::Str(other.as_string()),
                    });
                }

                if name.eq_ignore_ascii_case("created") {
                    if args.len() != 1 {
                        bail!("created(val) expects exactly 1 arg");
                    }
                    let val = self.eval_expr(&args[0], vars).await?;
                    let s = val.as_string();
                    let result = if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&s)
                    {
                        match doc.as_object_mut() {
                            Some(obj) => {
                                obj.insert("status".into(), json!(201));
                                doc.to_string()
                            }
                            None => format!(r#"{{"status":201,"data":{s}}}"#),
                        }
                    } else {
                        format!(r#"{{"status":201,"data":{s:?}}}"#)
                    };
                    return Ok(Value::Str(result));
                }

                if name.eq_ignore_ascii_case("notFound") {
                    return Ok(Value::Str(
                        r#"{"status":404,"error":"Not Found"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("noContent") {
                    return Ok(Value::Str(r#"{"status":204}"#.to_string()));
                }

                if name.eq_ignore_ascii_case("internalError") {
                    let msg = if let Some(arg) = args.first() {
                        self.eval_expr(arg, vars).await?.as_string()
                    } else {
                        "Internal Server Error".to_string()
                    };
                    let escaped = msg.replace('"', "\\\"");
                    return Ok(Value::Str(format!(
                        r#"{{"status":500,"error":"{escaped}"}}"#
                    )));
                }
                // ──────────────────────────────────────────────────────────

                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval_expr(arg, vars).await?);
                }
                Ok(self
                    .call_function(name, values)
                    .await?
                    .unwrap_or(Value::Void))
            }
            Expr::Await(inner) => self.eval_expr(inner, vars).await,
            Expr::Not(inner) => match self.eval_expr(inner, vars).await? {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => bail!(
                    "Unsupported unary '!' for {} (only bool is allowed)",
                    other.type_name()
                ),
            },
            Expr::ObjectLit(fields) => {
                let mut obj = serde_json::Map::with_capacity(fields.len());
                for (key, expr) in fields {
                    let value = self.eval_expr(expr, vars).await?;
                    obj.insert(key.clone(), value_to_json_smart(&value));
                }
                Ok(Value::Str(serde_json::Value::Object(obj).to_string()))
            }
            Expr::Add(left, right) => {
                let left = self.eval_expr(left, vars).await?;
                let right = self.eval_expr(right, vars).await?;
                match (left, right) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                    (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
                    (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
                    (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
                    (Value::Str(a), b) => Ok(Value::Str(format!("{a}{}", b.as_string()))),
                    (a, Value::Str(b)) => Ok(Value::Str(format!("{}{b}", a.as_string()))),
                    (a, b) => bail!(
                        "Unsupported '+' for {} and {}",
                        a.type_name(),
                        b.type_name()
                    ),
                }
            }
            Expr::Sub(left, right) => {
                self.eval_numeric_bin(left, right, vars, |a, b| a - b, |a, b| a - b)
                    .await
            }
            Expr::Mul(left, right) => {
                self.eval_numeric_bin(left, right, vars, |a, b| a * b, |a, b| a * b)
                    .await
            }
            Expr::Div(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                match (l, r) {
                    (Value::Int(_), Value::Int(0)) => bail!("division by zero"),
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
                    (Value::Float(_), Value::Float(0.0)) => bail!("division by zero"),
                    (Value::Int(_), Value::Float(0.0)) => bail!("division by zero"),
                    (Value::Float(_), Value::Int(0)) => bail!("division by zero"),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                    (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 / b)),
                    (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / b as f64)),
                    (a, b) => bail!(
                        "Unsupported '/' for {} and {}",
                        a.type_name(),
                        b.type_name()
                    ),
                }
            }
            Expr::Mod(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                match (l, r) {
                    (Value::Int(_), Value::Int(0)) => bail!("modulo by zero"),
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                    (Value::Float(_), Value::Float(0.0)) => bail!("modulo by zero"),
                    (Value::Int(_), Value::Float(0.0)) => bail!("modulo by zero"),
                    (Value::Float(_), Value::Int(0)) => bail!("modulo by zero"),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
                    (Value::Int(a), Value::Float(b)) => Ok(Value::Float((a as f64) % b)),
                    (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % (b as f64))),
                    (a, b) => bail!(
                        "Unsupported '%' for {} and {}",
                        a.type_name(),
                        b.type_name()
                    ),
                }
            }
            Expr::Neg(inner) => {
                let value = self.eval_expr(inner, vars).await?;
                match value {
                    Value::Int(v) => Ok(Value::Int(-v)),
                    Value::Float(v) => Ok(Value::Float(-v)),
                    other => bail!("Unsupported unary '-' for {}", other.type_name()),
                }
            }
            Expr::Eq(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                Ok(Value::Bool(l == r))
            }
            Expr::Neq(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                Ok(Value::Bool(l != r))
            }
            Expr::Lt(left, right) => self.eval_numeric_cmp(left, right, vars, |a, b| a < b).await,
            Expr::Lte(left, right) => {
                self.eval_numeric_cmp(left, right, vars, |a, b| a <= b)
                    .await
            }
            Expr::Gt(left, right) => self.eval_numeric_cmp(left, right, vars, |a, b| a > b).await,
            Expr::Gte(left, right) => {
                self.eval_numeric_cmp(left, right, vars, |a, b| a >= b)
                    .await
            }
            Expr::And(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                match l {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => {
                        let r = self.eval_expr(right, vars).await?;
                        match r {
                            Value::Bool(v) => Ok(Value::Bool(v)),
                            other => bail!("'and' expects bool, got {}", other.type_name()),
                        }
                    }
                    other => bail!("'and' expects bool, got {}", other.type_name()),
                }
            }
            Expr::Or(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                match l {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => {
                        let r = self.eval_expr(right, vars).await?;
                        match r {
                            Value::Bool(v) => Ok(Value::Bool(v)),
                            other => bail!("'or' expects bool, got {}", other.type_name()),
                        }
                    }
                    other => bail!("'or' expects bool, got {}", other.type_name()),
                }
            }
        }
    }

    async fn eval_numeric_bin<FInt, FFloat>(
        &mut self,
        left: &Expr,
        right: &Expr,
        vars: &mut HashMap<String, Value>,
        int_func: FInt,
        float_func: FFloat,
    ) -> Result<Value>
    where
        FInt: Fn(i64, i64) -> i64,
        FFloat: Fn(f64, f64) -> f64,
    {
        let l = self.eval_expr(left, vars).await?;
        let r = self.eval_expr(right, vars).await?;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(int_func(a, b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(float_func(a, b))),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(float_func(a as f64, b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(float_func(a, b as f64))),
            (a, b) => bail!(
                "Unsupported numeric op for {} and {}",
                a.type_name(),
                b.type_name()
            ),
        }
    }

    async fn eval_numeric_cmp<F>(
        &mut self,
        left: &Expr,
        right: &Expr,
        vars: &mut HashMap<String, Value>,
        func: F,
    ) -> Result<Value>
    where
        F: Fn(f64, f64) -> bool,
    {
        let l = self.eval_expr(left, vars).await?;
        let r = self.eval_expr(right, vars).await?;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(func(a as f64, b as f64))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(func(a, b))),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool(func(a as f64, b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(func(a, b as f64))),
            (a, b) => bail!(
                "Unsupported comparison for {} and {}",
                a.type_name(),
                b.type_name()
            ),
        }
    }

    /// Dispatch a single HTTP request directly (used by the real HTTP server).
    /// Returns (http_status_code, response_body).
    pub async fn dispatch_route(&mut self, method: &str, path: &str) -> Result<(u16, String)> {
        // Split `?query` off the path for route matching; keep the query for
        // `query_param(name)` lookups inside the handler.
        let (clean_path, query_params) = split_path_and_query(path);

        // Find matching route index and collect params (avoid holding borrow across mut calls)
        let mut found_idx: Option<usize> = None;
        let mut found_params: HashMap<String, String> = HashMap::new();

        for (i, route) in self.routes.iter().enumerate() {
            if !route.method.eq_ignore_ascii_case(method) {
                continue;
            }
            if let Some(params) = match_route_pattern(&route.path, &clean_path) {
                found_idx = Some(i);
                found_params = params;
                break;
            }
        }

        let Some(idx) = found_idx else {
            return Ok((
                404,
                format!(
                    "{{\"status\":404,\"error\":\"Not Found\",\"method\":\"{method}\",\"path\":\"{clean_path}\"}}"
                ),
            ));
        };

        // Clone what we need so we can mutably borrow self below
        let handler: Option<String> = self.routes[idx].handler.clone();
        let body_stmts: Vec<Stmt> = self.routes[idx].body.clone();
        let middleware_names: Vec<String> = self.routes[idx].middlewares.clone();
        // Route's own namespace — pushed onto the stack so bare-name calls
        // inside the inline body resolve against the route's namespace and
        // its file-level imports (same as a function call would).
        let route_namespace: Vec<String> = self.routes[idx].namespace.clone();

        let previous = self.current_path_params.take();
        let previous_query = self.current_query_params.take();
        let previous_dirty = std::mem::take(&mut self.dirty_fields);
        self.current_path_params = Some(found_params);
        self.current_query_params = Some(query_params);
        self.current_namespace_stack.push(route_namespace);

        // Run middlewares first; if any returns a value, short-circuit
        // the request with that response.
        let mut middleware_response: Option<String> = None;
        for mw_name in &middleware_names {
            if let Some(resp) = self.run_middleware(mw_name).await? {
                middleware_response = Some(resp);
                break;
            }
        }

        // Build the body response. If anything within the route fails AND a
        // top-level `errorHandler` is declared, give that handler a chance to
        // produce the response instead of bubbling up as 500.
        let body_result: Result<Option<String>> = if let Some(resp) = middleware_response {
            Ok(Some(resp))
        } else if let Some(ref handler_name) = handler {
            let args = self.build_handler_args(handler_name);
            self.call_function(handler_name, args)
                .await
                .map(|v| v.map(|v| v.as_string()))
        } else {
            let mut route_vars = HashMap::new();
            self.exec_block(&body_stmts, &mut route_vars)
                .await
                .map(|flow| match flow {
                    Flow::Return(Some(v)) => Some(v.as_string()),
                    Flow::Return(None) => Some("null".to_string()),
                    _ => {
                        if !self.output.is_empty() {
                            let out = self.output.trim_end_matches('\n').to_string();
                            self.output.clear();
                            Some(out)
                        } else {
                            None
                        }
                    }
                })
        };

        let response_str = match body_result {
            Ok(v) => v,
            Err(e) => {
                if let Some(handler) = self.error_handler {
                    let handler = handler.clone();
                    Some(self.run_error_handler(&handler, &e).await?)
                } else {
                    self.current_path_params = previous;
                    self.current_query_params = previous_query;
                    self.dirty_fields = previous_dirty;
                    self.current_namespace_stack.pop();
                    return Err(e);
                }
            }
        };
        self.current_path_params = previous;
        self.current_query_params = previous_query;
        self.dirty_fields = previous_dirty;
        self.current_namespace_stack.pop();

        let body = response_str.unwrap_or_else(|| "null".to_string());

        // Derive HTTP status from a "status" field in JSON, default 200.
        // Then strip the internal "status" field before sending to client.
        let (status, clean_body) =
            if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&body) {
                let code = doc
                    .get("status")
                    .and_then(|s| s.as_u64())
                    .and_then(|s| u16::try_from(s).ok())
                    .filter(|s| *s >= 100 && *s < 600)
                    .unwrap_or(200);
                // Strip the "status" key from the response body
                if let Some(obj) = doc.as_object_mut() {
                    obj.remove("status");
                }
                let body_out = if code == 204 {
                    String::new()
                } else {
                    doc.to_string()
                };
                (code, body_out)
            } else {
                (200, body)
            };

        Ok((status, clean_body))
    }

    #[async_recursion]
    async fn eval_dispatch_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("dispatch(method, path) expects exactly 2 args");
        }

        let method = self.eval_expr(&args[0], vars).await?;
        let path = self.eval_expr(&args[1], vars).await?;

        let method = match method {
            Value::Str(v) => v.to_ascii_uppercase(),
            other => bail!(
                "dispatch(method, path): method must be string, got {}",
                other.type_name()
            ),
        };

        let path = match path {
            Value::Str(v) => v,
            other => bail!(
                "dispatch(method, path): path must be string, got {}",
                other.type_name()
            ),
        };

        // Routes are owned data (Vec<RouteDecl>) since mount-expansion now
        // produces fresh copies. We pick the index here, then clone what we
        // need before any &mut self call to keep the borrow checker happy.
        let mut matched: Option<(usize, std::collections::HashMap<String, String>)> = None;
        for (i, route) in self.routes.iter().enumerate() {
            if !method.eq_ignore_ascii_case(&route.method) {
                continue;
            }
            if let Some(params) = match_route_pattern(&route.path, &path) {
                matched = Some((i, params));
                break;
            }
        }

        if let Some((idx, params)) = matched {
            let handler: Option<String> = self.routes[idx].handler.clone();
            let body_stmts: Vec<Stmt> = self.routes[idx].body.clone();
            let route_namespace: Vec<String> = self.routes[idx].namespace.clone();

            let previous = self.current_path_params.take();
            self.current_path_params = Some(params);
            self.current_namespace_stack.push(route_namespace);

            if let Some(handler_name) = handler {
                let args = self.build_handler_args(&handler_name);
                let result = self.call_function(&handler_name, args).await?;
                if let Some(value) = result {
                    self.output.push_str(&value.as_string());
                    self.output.push('\n');
                }
            } else {
                let mut route_vars = HashMap::new();
                let flow = self.exec_block(&body_stmts, &mut route_vars).await?;
                match flow {
                    Flow::Break | Flow::ContinueLoop => {
                        self.current_path_params = previous;
                        self.current_namespace_stack.pop();
                        bail!("break/continue cannot be used at route top-level");
                    }
                    Flow::Return(Some(value)) => {
                        self.output.push_str(&value.as_string());
                        self.output.push('\n');
                    }
                    Flow::Return(None) => {
                        self.output.push_str("null\n");
                    }
                    Flow::Continue => {}
                }
            }

            self.current_path_params = previous;
            self.current_namespace_stack.pop();
            return Ok(Value::Bool(true));
        }

        self.output.push_str(&format!(
            "{{\"status\":404,\"error\":\"Not Found\",\"method\":\"{}\",\"path\":\"{}\"}}\n",
            method, path
        ));
        Ok(Value::Bool(false))
    }

    /// Run the project's top-level `errorHandler (e) { ... }` block with
    /// `e` bound to the error envelope. Returns the handler's response body
    /// string (post-`as_string()`), or `null` if the handler didn't return.
    async fn run_error_handler(
        &mut self,
        handler: &ErrorHandlerDecl,
        err: &anyhow::Error,
    ) -> Result<String> {
        let mut all: Vec<String> = err.chain().map(|c| c.to_string()).collect();
        let message = if all.is_empty() {
            "unknown error".to_string()
        } else {
            all.remove(0)
        };
        let payload = json!({
            "message": message,
            "causes": all,
        });
        let mut handler_vars: HashMap<String, Value> = HashMap::new();
        handler_vars.insert(
            handler.catch_var.to_lowercase(),
            Value::Str(payload.to_string()),
        );

        let flow = self.exec_block(&handler.body, &mut handler_vars).await?;
        Ok(match flow {
            Flow::Return(Some(v)) => v.as_string(),
            Flow::Return(None) => "null".to_string(),
            _ => "null".to_string(),
        })
    }

    /// Look up the primary-key column names for `table` (matching the entity
    /// name or its snake_case form). Falls back to `["id"]` when no `pk` is
    /// declared, so ad-hoc tables not modelled as JWC entities still work.
    fn resolve_pk_fields(&self, table: &str) -> Vec<String> {
        let lc = table.to_lowercase();
        if let Some(pks) = self.pk_by_table.get(&lc) {
            if !pks.is_empty() {
                return pks.clone();
            }
        }
        let snake = crate::sql::to_snake_case(table).to_lowercase();
        if let Some(pks) = self.pk_by_table.get(&snake) {
            if !pks.is_empty() {
                return pks.clone();
            }
        }
        vec!["id".to_string()]
    }

    /// Execute a middleware by name. Returns `Some(response_body)` when the
    /// middleware short-circuits the request (returns a value), otherwise
    /// `None` to fall through to the route handler.
    async fn run_middleware(&mut self, name: &str) -> Result<Option<String>> {
        let mw = self
            .middlewares
            .get(&name.to_lowercase())
            .copied()
            .ok_or_else(|| anyhow!("Unknown middleware: {name}"))?;

        let mw_body: Vec<Stmt> = mw.body.clone();
        let mut mw_vars = HashMap::new();
        let flow = self.exec_block(&mw_body, &mut mw_vars).await?;

        // Middleware contract:
        //   no return / bare `return;` / `return null;`  → continue to next
        //   middleware (or handler)
        //   `return <value>;` with a non-null Value     → short-circuit and
        //   send that value as the HTTP response body
        match flow {
            Flow::Continue => Ok(None),
            Flow::Return(None) => Ok(None),
            Flow::Return(Some(Value::Null)) => Ok(None),
            Flow::Return(Some(v)) => Ok(Some(v.as_string())),
            Flow::Break | Flow::ContinueLoop => {
                bail!("'break'/'continue' cannot be used at middleware top level")
            }
        }
    }

    /// Build the argument list for a `route ... -> handler;` style call by
    /// matching the handler's declared parameter names against the current path
    /// and query params. Missing values become `Value::Null`; `check_param_type`
    /// later coerces strings to the declared type.
    fn build_handler_args(&self, handler_name: &str) -> Vec<Value> {
        let Some(handler) = self.functions.get(&handler_name.to_lowercase()).copied() else {
            return Vec::new();
        };

        handler
            .params
            .iter()
            .map(|param| {
                let key = param.name.clone();
                let from_path = self
                    .current_path_params
                    .as_ref()
                    .and_then(|m| m.get(&key).or_else(|| m.get(&key.to_lowercase())))
                    .cloned();
                if let Some(s) = from_path {
                    return Value::Str(s);
                }

                let from_query = self
                    .current_query_params
                    .as_ref()
                    .and_then(|m| m.get(&key).or_else(|| m.get(&key.to_lowercase())))
                    .cloned();
                match from_query {
                    Some(s) => Value::Str(s),
                    None => Value::Null,
                }
            })
            .collect()
    }

    async fn eval_path_param_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("path_param(name) expects exactly 1 arg");
        }

        let name = self.eval_expr(&args[0], vars).await?;
        let name = match name {
            Value::Str(v) => v,
            other => bail!(
                "path_param(name): name must be string, got {}",
                other.type_name()
            ),
        };

        let params = self
            .current_path_params
            .as_ref()
            .ok_or_else(|| anyhow!("path_param() can only be used inside route execution"))?;

        match params.get(&name) {
            Some(v) => Ok(Value::Str(v.clone())),
            None => Ok(Value::Null),
        }
    }

    async fn eval_query_param_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 2 {
            bail!("query_param(name[, default]) expects 1 or 2 args");
        }

        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "query_param(name): name must be string, got {}",
                other.type_name()
            ),
        };

        let value = self
            .current_query_params
            .as_ref()
            .and_then(|q| q.get(&name).cloned());

        match (value, args.get(1)) {
            (Some(v), _) => Ok(Value::Str(v)),
            (None, Some(default_expr)) => self.eval_expr(default_expr, vars).await,
            (None, None) => Ok(Value::Null),
        }
    }

    async fn eval_http_get_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 2 {
            bail!("http_get(url[, headers_json]) expects 1 or 2 args");
        }
        let url = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "http_get(url): url must be string, got {}",
                other.type_name()
            ),
        };

        let headers_json = if let Some(arg) = args.get(1) {
            match self.eval_expr(arg, vars).await? {
                Value::Str(s) => Some(s),
                Value::Null => None,
                other => bail!(
                    "http_get(url, headers): headers must be json string or null, got {}",
                    other.type_name()
                ),
            }
        } else {
            None
        };

        let mut req = http_client().get(&url);
        if let Some(json_str) = headers_json {
            req = apply_headers_reqwest(req, &json_str)?;
        }

        let response = req
            .send()
            .await
            .map_err(|e| anyhow!("http_get({url}) failed: {e}"))?;
        Ok(Value::Str(http_response_to_json_string(response).await?))
    }

    async fn eval_http_post_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 3 {
            bail!("http_post(url[, body[, headers_json]]) expects 1, 2 or 3 args");
        }
        let url = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "http_post(url): url must be string, got {}",
                other.type_name()
            ),
        };

        let body_str = match args.get(1) {
            None => None,
            Some(arg) => match self.eval_expr(arg, vars).await? {
                Value::Str(s) => Some(s),
                Value::Null => None,
                other => Some(other.as_string()),
            },
        };

        let headers_json = if let Some(arg) = args.get(2) {
            match self.eval_expr(arg, vars).await? {
                Value::Str(s) => Some(s),
                Value::Null => None,
                other => bail!(
                    "http_post(url, body, headers): headers must be json string or null, got {}",
                    other.type_name()
                ),
            }
        } else {
            None
        };

        let mut req = http_client().post(&url);
        if let Some(json_str) = headers_json {
            req = apply_headers_reqwest(req, &json_str)?;
        }

        let body_is_json = body_str
            .as_deref()
            .map(|s| serde_json::from_str::<JsonValue>(s).is_ok())
            .unwrap_or(false);
        if body_is_json {
            req = req.header("content-type", "application/json");
        }

        let response = match body_str {
            Some(b) => req.body(b).send().await,
            None => req.send().await,
        }
        .map_err(|e| anyhow!("http_post({url}) failed: {e}"))?;

        Ok(Value::Str(http_response_to_json_string(response).await?))
    }

    async fn eval_jwt_sign_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("jwt_sign(payload_json, secret) expects exactly 2 args");
        }
        let payload = self.eval_expr(&args[0], vars).await?.as_string();
        let secret = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_sign(payload, secret): secret must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Str(crate::jwt::sign_hs256(&payload, &secret)?))
    }

    async fn eval_jwt_verify_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("jwt_verify(token, secret) expects exactly 2 args");
        }
        let token = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_verify(token, secret): token must be string, got {}",
                other.type_name()
            ),
        };
        let secret = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_verify(token, secret): secret must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Str(crate::jwt::verify_hs256(&token, &secret)?))
    }

    async fn eval_ws_send_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("ws_send(msg) expects exactly 1 arg");
        }
        let msg = self.eval_expr(&args[0], vars).await?.as_string();
        let cell = match WS_HANDLE.try_with(|c| c.clone()) {
            Ok(c) => c,
            Err(_) => bail!("ws_send(): only valid inside a WS route handler"),
        };
        let guard = cell.lock().await;
        let sent = match guard.as_ref() {
            Some(w) => w.tx.send(msg).is_ok(),
            None => bail!("ws_send(): only valid inside a WS route handler"),
        };
        if sent {
            Ok(Value::Void)
        } else {
            bail!("ws_send: client disconnected")
        }
    }

    async fn eval_ws_recv_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("ws_recv() expects no args");
        }
        let cell = match WS_HANDLE.try_with(|c| c.clone()) {
            Ok(c) => c,
            Err(_) => bail!("ws_recv(): only valid inside a WS route handler"),
        };
        let mut guard = cell.lock().await;
        let result = match guard.as_mut() {
            Some(w) => w.rx.recv().await,
            None => bail!("ws_recv(): only valid inside a WS route handler"),
        };
        match result {
            None => Ok(Value::Null), // client closed
            Some(text) => Ok(Value::Str(text)),
        }
    }

    async fn eval_ws_close_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("ws_close() expects no args");
        }
        let cell = match WS_HANDLE.try_with(|c| c.clone()) {
            Ok(c) => c,
            Err(_) => bail!("ws_close(): only valid inside a WS route handler"),
        };
        cell.lock().await.take();
        Ok(Value::Void)
    }

    async fn eval_hash_password_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("hash_password(pwd) expects exactly 1 arg");
        }
        let pwd = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "hash_password(pwd): pwd must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Str(crate::password::hash_password(&pwd)?))
    }

    async fn eval_verify_password_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("verify_password(pwd, stored_hash) expects exactly 2 args");
        }
        let pwd = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "verify_password(pwd, hash): pwd must be string, got {}",
                other.type_name()
            ),
        };
        let stored = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "verify_password(pwd, hash): hash must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Bool(crate::password::verify_password(
            &pwd, &stored,
        )?))
    }

    async fn eval_cache_get_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("cache_get(key) expects exactly 1 arg");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_get(key): key must be string, got {}",
                other.type_name()
            ),
        };
        match crate::cache::get(&key) {
            Some(v) => Ok(Value::Str(v)),
            None => Ok(Value::Null),
        }
    }

    async fn eval_cache_set_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("cache_set(key, value, ttl_secs) expects exactly 3 args");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_set(key, value, ttl_secs): key must be string, got {}",
                other.type_name()
            ),
        };
        let value = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_set(key, value, ttl_secs): value must be string, got {}",
                other.type_name()
            ),
        };
        let ttl = match self.eval_expr(&args[2], vars).await? {
            Value::Int(n) if n >= 0 => n as u64,
            Value::Int(n) => {
                bail!("cache_set(key, value, ttl_secs): ttl_secs must be >= 0, got {n}")
            }
            other => bail!(
                "cache_set(key, value, ttl_secs): ttl_secs must be int, got {}",
                other.type_name()
            ),
        };
        crate::cache::set(&key, &value, ttl);
        Ok(Value::Void)
    }

    async fn eval_cache_del_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("cache_del(key) expects exactly 1 arg");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_del(key): key must be string, got {}",
                other.type_name()
            ),
        };
        crate::cache::del(&key);
        Ok(Value::Void)
    }

    async fn eval_cache_clear_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("cache_clear() expects no args");
        }
        crate::cache::clear();
        Ok(Value::Void)
    }

    async fn eval_send_email_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("send_email(to, subject, body_html) expects exactly 3 args");
        }
        let to = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "send_email(to, subject, body_html): to must be string, got {}",
                other.type_name()
            ),
        };
        let subject = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "send_email(to, subject, body_html): subject must be string, got {}",
                other.type_name()
            ),
        };
        let body_html = match self.eval_expr(&args[2], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "send_email(to, subject, body_html): body_html must be string, got {}",
                other.type_name()
            ),
        };
        crate::email::send_email(&to, &subject, &body_html)?;
        Ok(Value::Void)
    }

    async fn eval_register_job_handler_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("register_job_handler(name, handler_fn_name) expects exactly 2 args");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "register_job_handler(name, handler_fn_name): name must be string, got {}",
                other.type_name()
            ),
        };
        let handler = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "register_job_handler(name, handler_fn_name): handler_fn_name must be string, got {}",
                other.type_name()
            ),
        };
        if !self.functions.contains_key(&handler.to_lowercase()) {
            bail!(
                "error[E010]: register_job_handler: handler function '{}' is not defined in this program",
                handler
            );
        }
        crate::queue::register_handler(&name, &handler);
        Ok(Value::Void)
    }

    async fn eval_enqueue_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("enqueue(name, payload_json) expects exactly 2 args");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue(name, payload_json): name must be string, got {}",
                other.type_name()
            ),
        };
        let payload = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue(name, payload_json): payload_json must be string, got {}",
                other.type_name()
            ),
        };
        crate::queue::enqueue(&name, &payload);
        Ok(Value::Void)
    }

    async fn eval_enqueue_urgent_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("enqueue_urgent(name, payload_json) expects exactly 2 args");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue_urgent(name, payload_json): name must be string, got {}",
                other.type_name()
            ),
        };
        let payload = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue_urgent(name, payload_json): payload_json must be string, got {}",
                other.type_name()
            ),
        };
        crate::queue::enqueue_urgent(&name, &payload);
        Ok(Value::Void)
    }

    async fn eval_job_count_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("job_count() expects no args");
        }
        Ok(Value::Int(crate::queue::pending_count() as i64))
    }

    async fn eval_dlq_count_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("dlq_count() expects no args");
        }
        Ok(Value::Int(crate::queue::dlq_count() as i64))
    }

    /// Drain every permanently-failed job from the queue's dead-letter
    /// queue and return them as a JSON array. Each entry is
    /// `{name, payload, attempts, last_error}`. After this returns the
    /// DLQ is empty, so user code must persist anything it wants to keep.
    async fn eval_dlq_drain_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("dlq_drain() expects no args");
        }
        let entries = crate::queue::dlq_drain();
        let arr: Vec<JsonValue> = entries
            .into_iter()
            .map(|f| {
                serde_json::json!({
                    "name": f.job.name,
                    "payload": f.job.payload,
                    "attempts": f.job.attempts,
                    "last_error": f.last_error,
                })
            })
            .collect();
        Ok(Value::Str(JsonValue::Array(arr).to_string()))
    }

    async fn eval_header_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("header(name) expects exactly 1 arg");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "header(name): name must be string, got {}",
                other.type_name()
            ),
        };
        let key = name.to_ascii_lowercase();
        let value = self
            .current_headers
            .as_ref()
            .and_then(|h| h.get(&key).cloned());
        match value {
            Some(v) => Ok(Value::Str(v)),
            None => Ok(Value::Null),
        }
    }

    async fn eval_context_get_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("context(key) expects exactly 1 arg");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "context(key): key must be string, got {}",
                other.type_name()
            ),
        };
        Ok(self
            .request_context
            .get(&key)
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn eval_context_set_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("setContext(key, value) expects exactly 2 args");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "setContext(key, value): key must be string, got {}",
                other.type_name()
            ),
        };
        let value = self.eval_expr(&args[1], vars).await?;
        self.request_context.insert(key, value);
        Ok(Value::Void)
    }

    async fn eval_raw_sql_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 2 {
            bail!("raw_sql(sql[, params_json]) expects 1 or 2 args");
        }
        let sql = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "raw_sql(sql, ...): sql must be string, got {}",
                other.type_name()
            ),
        };

        let mut boxed_params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
        if let Some(arg) = args.get(1) {
            let raw = match self.eval_expr(arg, vars).await? {
                Value::Str(s) => s,
                Value::Null => "[]".to_string(),
                other => bail!(
                    "raw_sql(sql, params): params must be a JSON array string, got {}",
                    other.type_name()
                ),
            };
            let parsed: JsonValue = serde_json::from_str(&raw)
                .map_err(|_| anyhow!("raw_sql: params must be a JSON array, got invalid json"))?;
            let arr = parsed
                .as_array()
                .ok_or_else(|| anyhow!("raw_sql: params must be a JSON array"))?;
            for v in arr {
                boxed_params.push(json_value_to_sql_param(v));
            }
        }

        let param_refs = boxed_params_to_refs(&boxed_params);
        let lowered = sql.trim_start().to_ascii_lowercase();
        // For SELECT-shaped queries return the first column as text; everything
        // else routes through `exec` to also support write statements.
        if lowered.starts_with("select") || lowered.starts_with("with") {
            let result = engine::query_text(&sql, &param_refs).await?;
            if result.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(Value::Str(result))
            }
        } else {
            let affected = engine::exec(&sql, &param_refs).await?;
            engine::invalidate_result_cache()?;
            Ok(Value::Int(affected as i64))
        }
    }

    async fn eval_set_connection_string_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() > 1 {
            bail!(
                "setConnectionString() expects 0 or 1 args (use a URL string, an \
                 object literal, or no args to pull from the env)"
            );
        }

        // Form 1: no args → read from env. `.env` was already loaded by the
        // CLI before `main()` ran, so PG_* / DATABASE_URL should be present.
        if args.is_empty() {
            if let Ok(url) = std::env::var("DATABASE_URL") {
                // SAFETY: setter is only called from single-threaded main startup.
                std::env::set_var("DATABASE_URL", url);
                return Ok(Value::Void);
            }
            if let Some(url) = assemble_url_from_pg_env() {
                std::env::set_var("DATABASE_URL", url);
                return Ok(Value::Void);
            }
            bail!(
                "setConnectionString(): no DATABASE_URL and no PG_HOST/PORT/USER/PASSWORD/DATABASE in env"
            );
        }

        let value = self.eval_expr(&args[0], vars).await?;
        let url_string = match value {
            Value::Str(s) => connection_string_from_arg(&s)?,
            other => bail!(
                "setConnectionString(arg): arg must be a URL string or an object literal, got {}",
                other.type_name()
            ),
        };
        std::env::set_var("DATABASE_URL", url_string);
        Ok(Value::Void)
    }

    async fn eval_db_query_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("db_query(sql) expects exactly 1 arg");
        }

        let sql = self.eval_expr(&args[0], vars).await?;
        let sql = match sql {
            Value::Str(v) => v,
            other => bail!(
                "db_query(sql): sql must be string, got {}",
                other.type_name()
            ),
        };

        let database_url = std::env::var("DATABASE_URL")
            .or_else(|_| std::env::var("JWC_DATABASE_URL"))
            .map_err(|_| anyhow!("DATABASE_URL (or JWC_DATABASE_URL) is required for db_query"))?;

        engine::init_engine(&database_url)?;
        let value = engine::query_text(&sql, &[]).await?;
        if value.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(value))
        }
    }

    async fn eval_request_body_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("request_body() expects no args");
        }

        // Prefer the body injected by run_request().await, fall back to env var for legacy use
        let body = self
            .request_body
            .clone()
            .or_else(|| std::env::var("JWC_REQUEST_BODY").ok())
            .unwrap_or_else(|| "null".to_string());
        Ok(Value::Str(body))
    }

    /// `length(x)` — characters for a string, element count for a JSON array
    /// carried as a string, key count for a JSON object, 0 for null. Falls
    /// back to a friendly error for other shapes.
    async fn eval_length_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("length(x) expects exactly 1 arg");
        }
        match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(&s) {
                    if let Some(arr) = parsed.as_array() {
                        return Ok(Value::Int(arr.len() as i64));
                    }
                    if let Some(obj) = parsed.as_object() {
                        return Ok(Value::Int(obj.len() as i64));
                    }
                }
                Ok(Value::Int(s.chars().count() as i64))
            }
            Value::Null => Ok(Value::Int(0)),
            other => bail!("length(x): unsupported type {}", other.type_name()),
        }
    }

    async fn eval_string_call<F: Fn(&str) -> String>(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        name: &str,
        op: F,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("{name}(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("{name}(s): s must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(op(&s)))
    }

    async fn eval_two_string_bool_call<F: Fn(&str, &str) -> bool>(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        name: &str,
        op: F,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("{name}(s, p) expects exactly 2 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Bool(false)),
            other => bail!(
                "{name}: first arg must be string, got {}",
                other.type_name()
            ),
        };
        let p = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "{name}: second arg must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Bool(op(&s, &p)))
    }

    /// `contains(s, sub)` — substring check for strings, element check for
    /// JSON arrays carried as strings, key check for JSON objects.
    async fn eval_contains_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("contains(haystack, needle) expects exactly 2 args");
        }
        let haystack = self.eval_expr(&args[0], vars).await?;
        let needle = self.eval_expr(&args[1], vars).await?;

        match haystack {
            Value::Null => Ok(Value::Bool(false)),
            Value::Str(s) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(&s) {
                    if let Some(arr) = parsed.as_array() {
                        let target = value_to_json_smart(&needle);
                        return Ok(Value::Bool(arr.iter().any(|v| v == &target)));
                    }
                    if let Some(obj) = parsed.as_object() {
                        if let Value::Str(key) = needle {
                            return Ok(Value::Bool(obj.contains_key(&key)));
                        }
                    }
                }
                match needle {
                    Value::Str(sub) => Ok(Value::Bool(s.contains(&sub))),
                    other => bail!(
                        "contains: needle must be string for string haystack, got {}",
                        other.type_name()
                    ),
                }
            }
            other => bail!(
                "contains: haystack must be string/array, got {}",
                other.type_name()
            ),
        }
    }

    async fn eval_replace_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("replace(s, from, to) expects exactly 3 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("replace: s must be string, got {}", other.type_name()),
        };
        let from = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!("replace: from must be string, got {}", other.type_name()),
        };
        let to = match self.eval_expr(&args[2], vars).await? {
            Value::Str(s) => s,
            other => bail!("replace: to must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(s.replace(&from, &to)))
    }

    async fn eval_split_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("split(s, sep) expects exactly 2 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Str("[]".to_string())),
            other => bail!("split: s must be string, got {}", other.type_name()),
        };
        let sep = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!("split: sep must be string, got {}", other.type_name()),
        };
        let pieces: Vec<JsonValue> = s
            .split(&sep)
            .map(|p| JsonValue::String(p.to_string()))
            .collect();
        Ok(Value::Str(JsonValue::Array(pieces).to_string()))
    }

    async fn eval_first_or_last_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        first: bool,
    ) -> Result<Value> {
        let name = if first { "first" } else { "last" };
        if args.len() != 1 {
            bail!("{name}(xs) expects exactly 1 arg");
        }
        let raw = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!(
                "{name}: xs must be array (JSON string), got {}",
                other.type_name()
            ),
        };
        let parsed: JsonValue =
            serde_json::from_str(&raw).map_err(|_| anyhow!("{name}: xs is not valid JSON"))?;
        let arr = parsed
            .as_array()
            .ok_or_else(|| anyhow!("{name}: xs is not a JSON array"))?;
        let elem = if first { arr.first() } else { arr.last() };
        Ok(elem.map(json_to_value).unwrap_or(Value::Null))
    }

    async fn eval_json_parse_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("json_parse(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("json_parse: s must be string, got {}", other.type_name()),
        };
        let parsed: JsonValue =
            serde_json::from_str(&s).map_err(|e| anyhow!("json_parse: invalid JSON: {e}"))?;
        Ok(json_to_value(&parsed))
    }

    async fn eval_json_stringify_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("json_stringify(v) expects exactly 1 arg");
        }
        let v = self.eval_expr(&args[0], vars).await?;
        Ok(Value::Str(value_to_json_smart(&v).to_string()))
    }

    async fn eval_uuid_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("uuid() expects no args");
        }
        // RFC 4122 v4 — random, includes proper version/variant bits, no
        // sub-nanosecond collisions like the old `SystemTime::now()` hack.
        Ok(Value::Str(uuid::Uuid::new_v4().to_string()))
    }

    async fn eval_set_json_field_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("set_json_field(obj_json, field, value) expects 3 args");
        }

        let source = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "set_json_field: first arg must be string json, got {}",
                other.type_name()
            ),
        };
        let field = match self.eval_expr(&args[1], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "set_json_field: field must be string, got {}",
                other.type_name()
            ),
        };
        let value = self.eval_expr(&args[2], vars).await?;

        let mut json_value: JsonValue = serde_json::from_str(&source)
            .with_context(|| "set_json_field: invalid json in first arg")?;
        let object = json_value
            .as_object_mut()
            .ok_or_else(|| anyhow!("set_json_field: first arg must be a json object"))?;

        object.insert(field, value_to_json(&value));

        Ok(Value::Str(json_value.to_string()))
    }
}

/// Extract the column name from a field path like `"Entity.field"` → `"field"`
fn field_path_to_col(path: &str) -> String {
    if let Some(pos) = path.rfind('.') {
        path[pos + 1..].to_string()
    } else {
        path.to_string()
    }
}

/// Normalize JWC comparison operators to SQL operators
fn normalize_sql_op(op: &str) -> &str {
    match op {
        "==" | "=" => "=",
        "!=" => "!=",
        "<" => "<",
        "<=" => "<=",
        ">" => ">",
        ">=" => ">=",
        "like" => "LIKE",
        "ilike" => "ILIKE",
        _ => "=",
    }
}

/// Get a variable value as a JSON string, or error
fn get_var_as_json(var: &str, vars: &HashMap<String, Value>) -> Result<String> {
    match vars.get(&var.to_lowercase()).cloned() {
        Some(Value::Str(s)) => Ok(s),
        Some(other) => bail!("'{}' must be a JSON object, got {}", var, other.type_name()),
        None => bail!("variable '{}' not found", var),
    }
}

/// Build `INSERT INTO "table" (...) VALUES (...) RETURNING *` from a JSON object string
/// Uses a CTE so all columns (including SERIAL id) are returned.
fn build_insert_sql(
    table: &str,
    json_str: &str,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "insert: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("insert: value must be a JSON object"))?;
    if obj.is_empty() {
        bail!("insert: object has no fields to insert");
    }
    // Keep all provided fields, including explicit primary keys.
    // Some schemas (including example projects) use int PK without identity/default.
    let mut filtered: Vec<(&String, &serde_json::Value)> = obj.iter().collect();
    filtered.sort_by(|a, b| a.0.cmp(b.0));

    let fields: Vec<String> = filtered.iter().map(|(k, _)| format!("\"{}\"", k)).collect();
    let placeholders: Vec<String> = (1..=filtered.len()).map(|i| format!("${}", i)).collect();
    let params: Vec<Box<dyn ToSql + Sync + Send>> = filtered
        .iter()
        .map(|(_, v)| json_value_to_sql_param(v))
        .collect();
    Ok((format!(
        "WITH _ins AS (INSERT INTO \"{}\" ({}) VALUES ({}) RETURNING *) SELECT row_to_json(t)::text FROM _ins t;",
        table,
        fields.join(", "),
        placeholders.join(", "),
    ), params))
}

/// Build `UPDATE "table" SET ... WHERE <pk-cols> = ... RETURNING *;` from a
/// JSON object string. PK columns are excluded from the SET clause and used in
/// the WHERE filter; with composite PKs all of them are required in the JSON.
///
/// `dirty_fields` — when `Some`, only those columns are included in SET (the
/// natural "modified-since-load" semantics). When `None`, every non-PK field
/// from the JSON is updated (used for objects materialised entirely from code).
fn build_update_sql(
    table: &str,
    json_str: &str,
    pk_fields: &[String],
    dirty_fields: Option<&HashSet<String>>,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "update: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("update: value must be a JSON object"))?;

    if pk_fields.is_empty() {
        bail!("update: table '{}' has no primary key declared", table);
    }
    let mut pk_values: Vec<&serde_json::Value> = Vec::with_capacity(pk_fields.len());
    for pk in pk_fields {
        let v = obj.get(pk).ok_or_else(|| {
            anyhow!(
                "update: object must have field '{}' for the primary-key WHERE clause",
                pk
            )
        })?;
        pk_values.push(v);
    }

    let pk_set: std::collections::HashSet<String> =
        pk_fields.iter().map(|p| p.to_lowercase()).collect();
    let mut updates: Vec<(&String, &serde_json::Value)> = obj
        .iter()
        .filter(|(k, _)| !pk_set.contains(&k.to_lowercase()))
        .filter(|(k, _)| match dirty_fields {
            None => true,
            Some(d) => d.contains(*k) || d.contains(&k.to_lowercase()),
        })
        .collect();
    updates.sort_by(|a, b| a.0.cmp(b.0));

    if updates.is_empty() {
        if dirty_fields.is_some() {
            bail!("update: no fields have been modified since the object was loaded");
        }
        bail!("update: no fields to update (only primary key present in object)");
    }

    let sets: Vec<String> = updates
        .iter()
        .enumerate()
        .map(|(idx, (k, _))| format!("\"{}\" = ${}", k, idx + 1))
        .collect();

    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = updates
        .iter()
        .map(|(_, v)| json_value_to_sql_param(v))
        .collect();
    let mut where_parts = Vec::with_capacity(pk_fields.len());
    for (idx, pk) in pk_fields.iter().enumerate() {
        params.push(json_value_to_sql_param(pk_values[idx]));
        where_parts.push(format!("\"{}\" = ${}", pk, params.len()));
    }

    Ok((
        format!(
            "WITH _upd AS (UPDATE \"{}\" SET {} WHERE {} RETURNING *) SELECT row_to_json(t)::text FROM _upd t;",
            table,
            sets.join(", "),
            where_parts.join(" AND "),
        ),
        params,
    ))
}

/// Build `DELETE FROM "table" WHERE <pk-cols> = $N [AND ...];` from a JSON
/// object string. Composite primary keys are supported when all pk fields are
/// present in the JSON.
fn build_delete_sql(
    table: &str,
    json_str: &str,
    pk_fields: &[String],
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "delete: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("delete: value must be a JSON object"))?;

    if pk_fields.is_empty() {
        bail!("delete: table '{}' has no primary key declared", table);
    }
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::with_capacity(pk_fields.len());
    let mut where_parts = Vec::with_capacity(pk_fields.len());
    for pk in pk_fields {
        let v = obj.get(pk).ok_or_else(|| {
            anyhow!(
                "delete: object must have field '{}' for the primary-key WHERE clause",
                pk
            )
        })?;
        params.push(json_value_to_sql_param(v));
        where_parts.push(format!("\"{}\" = ${}", pk, params.len()));
    }

    Ok((
        format!(
            "DELETE FROM \"{}\" WHERE {};",
            table,
            where_parts.join(" AND ")
        ),
        params,
    ))
}

fn json_value_to_sql_param(val: &serde_json::Value) -> Box<dyn ToSql + Sync + Send> {
    match val {
        serde_json::Value::Null => Box::new(Option::<String>::None),
        serde_json::Value::Bool(b) => Box::new(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                    Box::new(i as i32)
                } else {
                    Box::new(i)
                }
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        serde_json::Value::String(s) => string_to_sql_param(s),
        other => Box::new(other.to_string()),
    }
}

/// Postgres rejects plain `String` against `uuid` / `timestamptz` columns —
/// no auto-cast. Try to recognise these shapes from the literal value and
/// box them as the proper Rust type so postgres-types accepts the bind.
/// Falls back to `String` for anything that doesn't match.
fn string_to_sql_param(s: &str) -> Box<dyn ToSql + Sync + Send> {
    if looks_like_uuid(s) {
        if let Ok(u) = uuid::Uuid::parse_str(s) {
            return Box::new(u);
        }
    }
    if looks_like_datetime(s) {
        if let Some(ts) = parse_rfc3339(s) {
            return Box::new(ts);
        }
    }
    Box::new(s.to_string())
}

/// Accept the most common ISO 8601 / RFC 3339 shapes. Strict chrono parsing
/// is intentionally permissive: trailing `Z`, sub-second precision, and an
/// optional timezone offset all parse cleanly.
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // Date-only forms like "2026-05-19" — promote to midnight UTC.
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = d.and_hms_opt(0, 0, 0)?;
        return Some(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            dt,
            chrono::Utc,
        ));
    }
    None
}

fn boxed_params_to_refs(params: &[Box<dyn ToSql + Sync + Send>]) -> Vec<&(dyn ToSql + Sync)> {
    params
        .iter()
        .map(|p| p.as_ref() as &(dyn ToSql + Sync))
        .collect()
}

fn value_to_sql_param(val: &Value) -> Box<dyn ToSql + Sync + Send> {
    match val {
        Value::Int(n) => {
            if (i32::MIN as i64..=i32::MAX as i64).contains(n) {
                Box::new(*n as i32)
            } else {
                Box::new(*n)
            }
        }
        Value::Str(s) => string_to_sql_param(s),
        Value::Float(n) => Box::new(*n),
        Value::Bool(b) => Box::new(*b),
        Value::Null | Value::Void => Box::new(Option::<String>::None),
    }
}

fn value_to_cache_fragment(val: &Value) -> String {
    match val {
        Value::Int(n) => format!("int:{n}"),
        Value::Float(n) => format!("float:{}", format_float(*n)),
        Value::Str(s) => format!("str:{s}"),
        Value::Bool(b) => format!("bool:{b}"),
        Value::Null => "null".to_string(),
        Value::Void => "void".to_string(),
    }
}

// Wide signature is intentional — this is the shared SELECT builder used by
// the runtime path. Splitting into a builder struct is tracked under the
// runner.rs modularisation sprint (Sprint 7).
#[allow(clippy::too_many_arguments)]
async fn build_select_sql(
    table_name: String,
    where_clause: Option<&WhereExpr>,
    order_by: Option<&DbOrderBy>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    first: bool,
    nav_subqueries: &[NavigationSubquery],
    projection: &[String],
    group_by: &[String],
    having: Option<&WhereExpr>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>, String, String)> {
    let mut sql_where = String::new();
    let mut shape_bits: Vec<String> = Vec::new();
    let mut cache_bits: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

    if let Some(wc) = where_clause {
        let where_sql =
            build_where_sql(wc, &mut params, &mut shape_bits, &mut cache_bits, vars, vm).await?;
        sql_where = format!(" WHERE {}", where_sql);
    }

    // GROUP BY + HAVING — emitted between WHERE and ORDER BY. The shape /
    // cache keys include the column list so two different groupings don't
    // collide in the prepared-statement cache.
    let mut sql_group = String::new();
    if !group_by.is_empty() {
        let cols: Vec<String> = group_by
            .iter()
            .map(|f| format!("\"{}\"", field_path_to_col(f)))
            .collect();
        sql_group = format!(" GROUP BY {}", cols.join(", "));
        shape_bits.push(format!("group:{}", cols.join(",")));
        cache_bits.push(format!("group:{}", cols.join(",")));
    }

    let mut sql_having = String::new();
    if let Some(hv) = having {
        let having_sql =
            build_where_sql(hv, &mut params, &mut shape_bits, &mut cache_bits, vars, vm).await?;
        sql_having = format!(" HAVING {}", having_sql);
    }

    let mut sql_order = String::new();
    if let Some(ob) = order_by {
        let col = field_path_to_col(&ob.field);
        let dir = match ob.dir {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        };
        sql_order = format!(" ORDER BY \"{}\" {}", col, dir);
        shape_bits.push(format!("orderby:{col}:{dir}"));
        cache_bits.push(format!("orderby:{col}:{dir}"));
    }

    let mut sql_limit_offset = String::new();

    if let Some(limit_expr) = limit {
        let v = vm.eval_expr(limit_expr, vars).await?;
        let n = value_to_positive_int(&v, "limit")?;
        sql_limit_offset.push_str(&format!(" LIMIT {n}"));
        shape_bits.push("limit:literal".to_string());
        cache_bits.push(format!("limit:{n}"));
    } else if first {
        sql_limit_offset.push_str(" LIMIT 1");
    }

    if let Some(offset_expr) = offset {
        let v = vm.eval_expr(offset_expr, vars).await?;
        let n = value_to_positive_int(&v, "offset")?;
        sql_limit_offset.push_str(&format!(" OFFSET {n}"));
        shape_bits.push("offset:literal".to_string());
        cache_bits.push(format!("offset:{n}"));
    }

    // Build the inner SELECT column list.
    //
    // - No `{ ... }` projection and no `with` clause → SELECT * (cheapest).
    // - Explicit `{ col1, col2 }` → only those columns from the source table.
    // - `with rel` → always include the named columns (or `t.*`) plus a
    //   correlated json subquery per relation.
    let base_cols: Vec<String> = if projection.is_empty() {
        Vec::new()
    } else {
        projection.iter().map(|c| format!("t.\"{}\"", c)).collect()
    };
    let inner_projection = if nav_subqueries.is_empty() && base_cols.is_empty() {
        "*".to_string()
    } else {
        let mut bits: Vec<String> = if base_cols.is_empty() {
            vec!["t.*".to_string()]
        } else {
            base_cols.clone()
        };
        if !projection.is_empty() {
            shape_bits.push(format!("project:{}", projection.join(",")));
            cache_bits.push(format!("project:{}", projection.join(",")));
        }
        for nav in nav_subqueries {
            bits.push(format!("{} AS \"{}\"", nav.sql_fragment("t"), nav.alias()));
            shape_bits.push(format!("with:{}", nav.alias()));
            cache_bits.push(format!("with:{}", nav.alias()));
        }
        bits.join(", ")
    };

    let inner_sql = format!(
        "SELECT {} FROM \"{}\" t{}{}{}{}{}",
        inner_projection, table_name, sql_where, sql_group, sql_having, sql_order, sql_limit_offset
    );

    let sql = if first {
        format!(
            "SELECT row_to_json(r)::text FROM ({}) r;",
            inner_sql.trim_end()
        )
    } else {
        format!(
            "SELECT COALESCE(json_agg(row_to_json(r)), '[]')::text FROM ({}) r;",
            inner_sql.trim_end()
        )
    };

    let shape_suffix = if shape_bits.is_empty() {
        "no_clauses".to_string()
    } else {
        shape_bits.join("|")
    };
    let cache_suffix = if cache_bits.is_empty() {
        "no_clauses".to_string()
    } else {
        cache_bits.join("|")
    };
    let shape_key = format!("select|table:{table_name}|first:{first}|{shape_suffix}");
    let cache_key = format!("result|table:{table_name}|first:{first}|{cache_suffix}");

    Ok((sql, params, shape_key, cache_key))
}

struct NavigationSubquery {
    name: String,
    kind: NavigationKind,
    target_table: String,
    target_fk_col: String,
    source_pk_col: String,
}

impl NavigationSubquery {
    fn alias(&self) -> &str {
        &self.name
    }

    fn sql_fragment(&self, source_alias: &str) -> String {
        match self.kind {
            NavigationKind::OneToMany => format!(
                "COALESCE((SELECT json_agg(row_to_json(c)) FROM \"{}\" c WHERE c.\"{}\" = {}.\"{}\"), '[]'::json)",
                self.target_table, self.target_fk_col, source_alias, self.source_pk_col
            ),
            NavigationKind::OneToOne => format!(
                "(SELECT row_to_json(c) FROM \"{}\" c WHERE c.\"{}\" = {}.\"{}\" LIMIT 1)",
                self.target_table, self.target_fk_col, source_alias, self.source_pk_col
            ),
        }
    }
}

fn build_navigation_subqueries(
    entity: &str,
    _source_table_name: &str,
    requested: &[String],
    models: &HashMap<String, &ModelDecl>,
    pk_by_table: &HashMap<String, Vec<String>>,
) -> Result<Vec<NavigationSubquery>> {
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    if entity == "*" {
        bail!("'with' clause requires a named entity, not '*'");
    }
    let entity_key = entity.to_lowercase();
    let model = models
        .get(&entity_key)
        .ok_or_else(|| anyhow!("unknown entity '{}' for 'with' clause", entity))?;

    let source_pk_col = pk_by_table
        .get(&entity_key)
        .and_then(|v| v.first().cloned())
        .unwrap_or_else(|| "id".to_string());

    let mut out = Vec::with_capacity(requested.len());
    for rel in requested {
        let rel_key = rel.to_lowercase();
        let nav = model
            .navigations
            .iter()
            .find(|n| n.name.to_lowercase() == rel_key)
            .ok_or_else(|| anyhow!("entity '{}' has no navigation '{}'", entity, rel))?;
        out.push(NavigationSubquery {
            name: nav.name.clone(),
            kind: nav.kind,
            target_table: crate::sql::to_snake_case(&nav.target_entity),
            target_fk_col: nav.target_field.clone(),
            source_pk_col: source_pk_col.clone(),
        });
    }
    Ok(out)
}

#[async_recursion]
async fn build_where_sql(
    expr: &WhereExpr,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    shape: &mut Vec<String>,
    cache: &mut Vec<String>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
) -> Result<String> {
    match expr {
        WhereExpr::Atom(wc) => {
            let col = field_path_to_col(&wc.field);
            let op = normalize_sql_op(&wc.op);
            let rhs_val = vm.eval_expr(&wc.rhs, vars).await?;

            Ok(match rhs_val {
                Value::Null | Value::Void => {
                    if op == "!=" {
                        shape.push(format!("where:{col}:is_not_null"));
                        cache.push(format!("where:{col}:is_not_null"));
                        format!("\"{}\" IS NOT NULL", col)
                    } else {
                        shape.push(format!("where:{col}:is_null"));
                        cache.push(format!("where:{col}:is_null"));
                        format!("\"{}\" IS NULL", col)
                    }
                }
                other => {
                    params.push(value_to_sql_param(&other));
                    let idx = params.len();
                    shape.push(format!("where:{col}:{op}:param"));
                    cache.push(format!(
                        "where:{col}:{op}:{}",
                        value_to_cache_fragment(&other)
                    ));
                    format!("\"{}\" {} ${}", col, op, idx)
                }
            })
        }
        WhereExpr::Between { field, low, high } => {
            let col = field_path_to_col(field);
            let low_v = vm.eval_expr(low, vars).await?;
            let high_v = vm.eval_expr(high, vars).await?;
            params.push(value_to_sql_param(&low_v));
            let low_idx = params.len();
            params.push(value_to_sql_param(&high_v));
            let high_idx = params.len();
            shape.push(format!("where:{col}:between"));
            cache.push(format!(
                "where:{col}:between:{}..{}",
                value_to_cache_fragment(&low_v),
                value_to_cache_fragment(&high_v)
            ));
            Ok(format!(
                "\"{}\" BETWEEN ${} AND ${}",
                col, low_idx, high_idx
            ))
        }
        WhereExpr::InList { field, values } => {
            let col = field_path_to_col(field);
            if values.is_empty() {
                bail!("WHERE 'in (...)' must have at least one value");
            }
            let mut placeholders = Vec::with_capacity(values.len());
            let mut cache_parts = Vec::with_capacity(values.len());
            for value_expr in values {
                let v = vm.eval_expr(value_expr, vars).await?;
                params.push(value_to_sql_param(&v));
                placeholders.push(format!("${}", params.len()));
                cache_parts.push(value_to_cache_fragment(&v));
            }
            shape.push(format!("where:{col}:in({})", values.len()));
            cache.push(format!("where:{col}:in[{}]", cache_parts.join(",")));
            Ok(format!("\"{}\" IN ({})", col, placeholders.join(", ")))
        }
        WhereExpr::And(l, r) => {
            let ls = build_where_sql(l, params, shape, cache, vars, vm).await?;
            shape.push("AND".to_string());
            cache.push("AND".to_string());
            let rs = build_where_sql(r, params, shape, cache, vars, vm).await?;
            Ok(format!("({} AND {})", ls, rs))
        }
        WhereExpr::Or(l, r) => {
            let ls = build_where_sql(l, params, shape, cache, vars, vm).await?;
            shape.push("OR".to_string());
            cache.push("OR".to_string());
            let rs = build_where_sql(r, params, shape, cache, vars, vm).await?;
            Ok(format!("({} OR {})", ls, rs))
        }
    }
}

fn value_to_positive_int(value: &Value, clause: &str) -> Result<i64> {
    match value {
        Value::Int(n) if *n >= 0 => Ok(*n),
        Value::Int(n) => bail!("{clause} must be non-negative, got {n}"),
        Value::Str(s) => s
            .parse::<i64>()
            .map_err(|_| anyhow!("{clause} must be integer, got string '{s}'"))
            .and_then(|n| {
                if n >= 0 {
                    Ok(n)
                } else {
                    Err(anyhow!("{clause} must be non-negative, got {n}"))
                }
            }),
        other => bail!("{clause} must be integer, got {}", other.type_name()),
    }
}

/// Resolve the argument to `setConnectionString(...)` into a Postgres URL.
///
/// Accepts either:
/// - A literal connection URL (`postgres://user:pw@host:port/db`), passed
///   through unchanged.
/// - A JSON object literal `{ host, port, user, password, database }` —
///   the format JWC's object-literal syntax produces. Every field except
///   `port` is required; missing keys surface as a clear error.
fn connection_string_from_arg(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        return Ok(trimmed.to_string());
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|_| {
        anyhow!(
            "setConnectionString(arg): expected a postgres:// URL or a JSON object literal, got '{trimmed}'"
        )
    })?;
    let obj = parsed.as_object().ok_or_else(|| {
        anyhow!("setConnectionString(arg): expected a JSON object, got non-object")
    })?;

    let host = pick_string_field(obj, "host")?;
    let port = obj
        .get("port")
        .map(|v| match v {
            serde_json::Value::Number(n) => Ok(n.to_string()),
            serde_json::Value::String(s) => Ok(s.clone()),
            other => Err(anyhow!(
                "setConnectionString: 'port' must be a number or string, got {other:?}"
            )),
        })
        .transpose()?
        .unwrap_or_else(|| "5432".to_string());
    let user = pick_string_field(obj, "user")?;
    let password = pick_string_field(obj, "password")?;
    let database = pick_string_field(obj, "database")?;

    Ok(format!(
        "postgresql://{}:{}@{}:{}/{}",
        user, password, host, port, database
    ))
}

fn pick_string_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String> {
    obj.get(key)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .ok_or_else(|| {
            anyhow!("setConnectionString({{ ... }}): missing or non-string field '{key}'")
        })
}

fn assemble_url_from_pg_env() -> Option<String> {
    let user = std::env::var("PG_USER").ok()?;
    let password = std::env::var("PG_PASSWORD").ok()?;
    let host = std::env::var("PG_HOST").ok()?;
    let port = std::env::var("PG_PORT").ok()?;
    let database = std::env::var("PG_DATABASE").ok()?;
    Some(format!(
        "postgresql://{}:{}@{}:{}/{}",
        user, password, host, port, database
    ))
}

/// Convert a parsed JSON value back into the runtime's untagged Value enum.
/// Objects and arrays round-trip through their serialised form because the
/// runtime carries nested shapes as JSON strings.
fn json_to_value(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Str(n.to_string())
            }
        }
        JsonValue::String(s) => Value::Str(s.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => Value::Str(value.to_string()),
    }
}

/// JSON-encode a runtime Value, embedding nested JSON shapes raw.
///
/// `Value::Str` is the language's universal carrier for both plain strings
/// AND nested objects/arrays returned by `select` / `body()` / `cache_get`.
/// When the string parses as a JSON object/array, embed it as-is so an
/// object literal like `{ items: posts }` produces `{"items": [...]}`
/// rather than the double-encoded `{"items": "[...]"}`.
fn value_to_json_smart(value: &Value) -> JsonValue {
    if let Value::Str(s) = value {
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
            if parsed.is_object() || parsed.is_array() {
                return parsed;
            }
        }
    }
    value_to_json(value)
}

fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Int(v) => json!(v),
        Value::Float(v) => json!(v),
        Value::Str(v) => json!(v),
        Value::Bool(v) => json!(v),
        Value::Null | Value::Void => JsonValue::Null,
    }
}

/// Format the current UTC time as an RFC 3339 / ISO 8601 string with millis,
/// using Howard Hinnant's civil-from-days algorithm so we avoid pulling in
/// chrono just for one call.
fn current_utc_iso8601() -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("System clock is before UNIX_EPOCH"))?;
    let seconds = now.as_secs() as i64;
    let nanos = now.subsec_nanos();
    Ok(format_iso8601_utc(seconds, nanos))
}

fn format_iso8601_utc(seconds: i64, nanos: u32) -> String {
    let mut days = seconds.div_euclid(86_400);
    let secs_today = seconds.rem_euclid(86_400);
    let hh = secs_today / 3600;
    let mm = (secs_today % 3600) / 60;
    let ss = secs_today % 60;

    // Civil-from-days: days since 1970-01-01 → (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let _ = &mut days;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        hh,
        mm,
        ss,
        nanos / 1_000_000
    )
}

/// Apply a JSON object of `{"Header-Name": "value"}` pairs onto a reqwest request.
fn apply_headers_reqwest(
    mut req: reqwest::RequestBuilder,
    headers_json: &str,
) -> Result<reqwest::RequestBuilder> {
    let parsed: JsonValue = serde_json::from_str(headers_json)
        .map_err(|_| anyhow!("headers must be a JSON object literal, got invalid json"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| anyhow!("headers must be a JSON object, got non-object"))?;
    for (k, v) in obj {
        let val = match v {
            JsonValue::String(s) => s.clone(),
            other => other.to_string(),
        };
        req = req.header(k.as_str(), val);
    }
    Ok(req)
}

/// Wrap a reqwest response into a JSON envelope `{"status": N, "body": "..."}` —
/// JSON body is preserved as-is when parseable, otherwise stored as a string.
async fn http_response_to_json_string(response: reqwest::Response) -> Result<String> {
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| anyhow!("failed to read response body: {e}"))?;

    let body_value = match serde_json::from_str::<JsonValue>(&body) {
        Ok(v) => v,
        Err(_) => JsonValue::String(body),
    };

    let envelope = json!({
        "status": status,
        "body": body_value,
    });
    Ok(envelope.to_string())
}

/// Returns the candidate from `candidates` closest to `target` by Levenshtein
/// distance, but only when the match is "close enough" (distance ≤ max(2, len/3))
/// — this keeps the suggestion useful and avoids unrelated noise.
fn closest_match<'a, I>(target: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = &'a String>,
{
    let target_lc = target.to_ascii_lowercase();
    let threshold = std::cmp::max(2, target_lc.len() / 3);

    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        if candidate.eq_ignore_ascii_case(target) {
            continue;
        }
        let dist = levenshtein(&target_lc, &candidate.to_ascii_lowercase());
        if dist > threshold {
            continue;
        }
        match best {
            Some((d, _)) if d <= dist => {}
            _ => best = Some((dist, candidate.as_str())),
        }
    }

    best.map(|(_, s)| s.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut prev = (0..=b.len()).collect::<Vec<_>>();
    let mut curr = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}

/// If `s` looks like `Wrapper<Inner>` returns `Some("Inner")`. Returns `None`
/// when the wrapper name doesn't match (case-insensitive) or no `< >` present.
fn strip_generic_wrapper<'a>(s: &'a str, wrapper: &str) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    let want = format!("{}<", wrapper.to_ascii_lowercase());
    if lower.starts_with(&want) && s.ends_with('>') {
        let start = want.len();
        let end = s.len() - 1;
        if end > start {
            return Some(&s[start..end]);
        }
    }
    None
}

/// `8-4-4-4-12` hex pattern. Hyphen positions are checked; characters can be
/// uppercase or lowercase hex. Matches RFC 4122 textual form.
fn looks_like_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let is_dash = matches!(i, 8 | 13 | 18 | 23);
        if is_dash {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Minimal ISO-8601-ish heuristic: starts with `YYYY-MM-DD` and has at least
/// 10 characters. Avoids pulling in chrono just for type checking.
fn looks_like_datetime(s: &str) -> bool {
    if s.len() < 10 {
        return false;
    }
    let b = s.as_bytes();
    b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}

/// Run the rules from a `validate body { ... }` block against a parsed JSON body.
/// Returns a map `{ field: "first failing rule" }`. Empty map means all rules passed.
fn run_validation_rules(
    fields: &[ValidateField],
    body: &JsonValue,
) -> serde_json::Map<String, JsonValue> {
    let mut errors = serde_json::Map::new();

    for field in fields {
        let value = body.get(&field.name);

        for rule in &field.rules {
            if let Some(msg) = check_rule(rule, value) {
                errors.insert(field.name.clone(), JsonValue::String(msg));
                break;
            }
        }
    }

    errors
}

fn check_rule(rule: &ValidateRule, value: Option<&JsonValue>) -> Option<String> {
    match rule {
        ValidateRule::Required => match value {
            None | Some(JsonValue::Null) => Some("required".to_string()),
            _ => None,
        },
        ValidateRule::MinLength(n) => match value {
            Some(JsonValue::String(s)) => {
                if (s.chars().count() as i64) < *n {
                    Some(format!("minLength({n})"))
                } else {
                    None
                }
            }
            None | Some(JsonValue::Null) => None,
            _ => Some(format!("minLength({n}): not a string")),
        },
        ValidateRule::MaxLength(n) => match value {
            Some(JsonValue::String(s)) => {
                if (s.chars().count() as i64) > *n {
                    Some(format!("maxLength({n})"))
                } else {
                    None
                }
            }
            None | Some(JsonValue::Null) => None,
            _ => Some(format!("maxLength({n}): not a string")),
        },
        ValidateRule::Min(bound) => check_numeric_bound(value, bound, true),
        ValidateRule::Max(bound) => check_numeric_bound(value, bound, false),
        ValidateRule::Pattern(regex_src) => match value {
            Some(JsonValue::String(s)) => {
                let re = match regex::Regex::new(regex_src) {
                    Ok(re) => re,
                    Err(_) => return Some(format!("pattern({regex_src}): invalid regex")),
                };
                if re.is_match(s) {
                    None
                } else {
                    Some(format!("pattern({regex_src})"))
                }
            }
            None | Some(JsonValue::Null) => None,
            _ => Some(format!("pattern({regex_src}): not a string")),
        },
    }
}

fn check_numeric_bound(value: Option<&JsonValue>, bound: &str, is_min: bool) -> Option<String> {
    let bound_num: f64 = match bound.parse() {
        Ok(v) => v,
        Err(_) => return Some(format!("invalid numeric bound '{bound}'")),
    };
    let label = if is_min { "min" } else { "max" };

    match value {
        Some(JsonValue::Number(n)) => {
            let v = n.as_f64().unwrap_or(0.0);
            let ok = if is_min {
                v >= bound_num
            } else {
                v <= bound_num
            };
            if ok {
                None
            } else {
                Some(format!("{label}({bound})"))
            }
        }
        None | Some(JsonValue::Null) => None,
        _ => Some(format!("{label}({bound}): not a number")),
    }
}

/// Split `"/items?limit=10&q=hi"` into `("/items", { "limit": "10", "q": "hi" })`.
/// Percent-decoding is intentionally not done here — keep it lazy for now.
fn split_path_and_query(raw: &str) -> (String, HashMap<String, String>) {
    let mut iter = raw.splitn(2, '?');
    let path = iter.next().unwrap_or("").to_string();
    let mut params = HashMap::new();

    if let Some(query) = iter.next() {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            match pair.split_once('=') {
                Some((k, v)) if !k.is_empty() => {
                    params.insert(k.to_string(), v.to_string());
                }
                None if !pair.is_empty() => {
                    params.insert(pair.to_string(), String::new());
                }
                _ => {}
            }
        }
    }

    (path, params)
}

fn match_route_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_segments: Vec<&str> = pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let path_segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if pattern_segments.len() != path_segments.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (p, v) in pattern_segments.iter().zip(path_segments.iter()) {
        if p.starts_with('{') && p.ends_with('}') && p.len() > 2 {
            let key = p.trim_start_matches('{').trim_end_matches('}').to_string();
            params.insert(key, (*v).to_string());
            continue;
        }

        if p != v {
            return None;
        }
    }

    Some(params)
}

enum Flow {
    Continue,
    Return(Option<Value>),
    Break,
    ContinueLoop,
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Void,
}

impl Value {
    fn as_string(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            Value::Str(v) => v.clone(),
            Value::Bool(v) => v.to_string(),
            Value::Null => "null".to_string(),
            Value::Void => String::new(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "double",
            Value::Str(_) => "string",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Void => "void",
        }
    }
}

fn format_float(value: f64) -> String {
    let mut s = format!("{value:.15}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_program, validate_program};

    #[tokio::test]
    async fn runs_main_and_prints_output() {
        let src = r#"
            function main() {
                let name = "JWC";
                print("Hello " + name);
                print(1 + 2 * 3);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "Hello JWC\n7\n");
    }

    #[tokio::test]
    async fn supports_function_call_and_return() {
        let src = r#"
            function add(a, b) {
                return a + b;
            }

            function main() {
                let x = add(20, 22);
                print(x);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "42\n");
    }

    #[tokio::test]
    async fn supports_float_literals_and_arithmetic() {
        let src = r#"
            function main() {
                let a = 0.2;
                let b = 0.1;
                let sum = a + b;
                print(sum);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "0.3\n");
    }

    #[tokio::test]
    async fn supports_if_while_break_continue() {
        let src = r#"
            function main() {
                let i = 0;
                while (i < 6) {
                    i = i + 1;
                    if (i == 2) {
                        continue;
                    }
                    if (i == 5) {
                        break;
                    }
                    print(i);
                }
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "1\n3\n4\n");
    }

    #[tokio::test]
    async fn supports_logical_ops() {
        let src = r#"
            function main() {
                if (true and (1 < 2) or false) {
                    print("ok");
                } else {
                    print("bad");
                }
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "ok\n");
    }

    #[tokio::test]
    async fn supports_declarative_routes_with_dispatch() {
        let src = r#"
            route GET "/health" {
                print("GET /health -> 200 OK");
            }

            function main() {
                dispatch("GET", "/health");
                dispatch("GET", "/unknown");
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(
            out.output,
            "GET /health -> 200 OK\n{\"status\":404,\"error\":\"Not Found\",\"method\":\"GET\",\"path\":\"/unknown\"}\n"
        );
    }

    #[tokio::test]
    async fn supports_route_path_params() {
        let src = r#"
            route GET "/todos/{id}" {
                let id = path_param("id");
                print("todo=" + id);
            }

            function main() {
                dispatch("GET", "/todos/42");
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "todo=42\n");
    }

    #[tokio::test]
    async fn dispatch_outputs_json_from_route_return() {
        let src = r#"
            route GET "/todos" {
                return "{\"items\":[]}";
            }

            function main() {
                dispatch("GET", "/todos");
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "{\"items\":[]}\n");
    }

    #[tokio::test]
    async fn supports_new_entity_and_field_ops() {
        let src = r#"
            function main() {
                let car = new CarEntity();
                car.model = "Tesla";
                car.year = 2024;
                let m = car.model;
                let y = car.year;
                print(m);
                print(y);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap();
        assert_eq!(out.output, "Tesla\n2024\n");
    }

    #[tokio::test]
    async fn run_request_dispatches_route() {
        let src = r#"
            route GET "/ping" {
                return "{\"ok\":true}";
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/ping", None).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn run_request_returns_404_for_unknown_route() {
        let src = r#"
            route GET "/ping" {
                return "pong";
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, _body) = run_request(&program, "GET", "/missing", None)
            .await
            .unwrap();
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn body_is_auto_parsed_for_typed_class_param() {
        let src = r#"
            class BrandInput {
                id int;
                name string;
            }

            function echoBrand(input: BrandInput): BrandInput {
                return input;
            }

            route POST "/brands" {
                let payload = echoBrand(body());
                return json(payload);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        let (status, body) = run_request(
            &program,
            "POST",
            "/brands",
            Some("{\"id\":1,\"name\":\"Acme\"}".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(status, 200);
        assert_eq!(body, "{\"id\":1,\"name\":\"Acme\"}");
    }

    #[tokio::test]
    async fn body_parse_fails_when_typed_class_missing_required_field() {
        let src = r#"
            class BrandInput {
                id int;
                name string;
            }

            function createBrand(input: BrandInput) {
                return input;
            }

            route POST "/brands" {
                let payload = createBrand(body());
                return json(payload);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        let err = run_request(&program, "POST", "/brands", Some("{\"id\":1}".to_string()))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("expects field 'name'"));
    }

    #[tokio::test]
    async fn query_param_reads_value_from_url() {
        let src = r#"
            route GET "/items" {
                let limit = query_param("limit");
                let q = query_param("q");
                return json("limit=" + limit + ",q=" + q);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/items?limit=10&q=hello", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("limit=10"));
        assert!(body.contains("q=hello"));
    }

    #[tokio::test]
    async fn query_param_default_used_when_missing() {
        let src = r#"
            route GET "/items" {
                let limit = query_param("limit", "20");
                return json("limit=" + limit);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/items", None).await.unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("limit=20"));
    }

    #[tokio::test]
    async fn uuid_type_accepts_valid_string_and_rejects_invalid() {
        let src = r#"
            function take(id: uuid): uuid {
                return id;
            }

            function main() {
                let good = take("550e8400-e29b-41d4-a716-446655440000");
                print(good);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("550e8400-e29b-41d4-a716-446655440000"));

        let bad_src = r#"
            function take(id: uuid): uuid { return id; }
            function main() { take("not-a-uuid"); }
        "#;
        let bad_program = parse_program(bad_src).unwrap();
        validate_program(&bad_program).unwrap();
        let err = run_main(&bad_program).await.unwrap_err().to_string();
        assert!(err.contains("uuid"));
    }

    #[tokio::test]
    async fn datetime_type_accepts_iso_string() {
        let src = r#"
            function take(at: datetime): datetime {
                return at;
            }

            function main() {
                let v = take("2026-05-19T10:00:00Z");
                print(v);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("2026-05-19"));
    }

    #[tokio::test]
    async fn nullable_type_marker_allows_null_value() {
        let src = r#"
            function take(name: string?): string? {
                return name;
            }

            function main() {
                let v = take(null);
                print(v);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "null");
    }

    #[tokio::test]
    async fn optional_wrapper_is_equivalent_to_nullable_marker() {
        let src = r#"
            function take(x: Optional<int>): Optional<int> {
                return x;
            }

            function main() {
                let v = take(null);
                print(v);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "null");
    }

    #[tokio::test]
    async fn list_of_int_validates_each_element() {
        let src = r#"
            function take(xs: List<int>): List<int> {
                return xs;
            }

            function main() {
                let v = take("[1, 2, 3]");
                print(v);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("[1,2,3]") || out.contains("[1, 2, 3]"));

        let bad_src = r#"
            function take(xs: List<int>): List<int> { return xs; }
            function main() { take("[1, \"two\", 3]"); }
        "#;
        let bad = parse_program(bad_src).unwrap();
        validate_program(&bad).unwrap();
        let err = run_main(&bad).await.unwrap_err().to_string();
        assert!(err.contains("List<int>"));
    }

    #[tokio::test]
    async fn try_catch_swallows_runtime_error() {
        let src = r#"
            function risky() {
                let x = undefined_var;
            }

            function main() {
                try {
                    risky();
                    print("unreachable");
                } catch (e) {
                    print("caught: " + e);
                }
                print("after");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("caught:"));
        assert!(out.contains("Undefined variable"));
        assert!(out.contains("after"));
        assert!(!out.contains("unreachable"));
    }

    #[tokio::test]
    async fn try_catch_returns_from_catch_block() {
        let src = r#"
            function broken(): int {
                let x = 0;
                return x / 0;
            }

            function main() {
                try {
                    let y = broken();
                    print(y);
                } catch (e) {
                    print("recovered");
                }
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "recovered");
    }

    #[tokio::test]
    async fn try_catch_lets_success_pass_through() {
        let src = r#"
            function safe(): int {
                return 7;
            }

            function main() {
                try {
                    let n = safe();
                    print(n);
                } catch (e) {
                    print("never");
                }
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "7");
    }

    /// Tests that mutate process env need to be serialised — cargo runs
    /// tests in parallel by default and `setConnectionString(...)` writes
    /// to `DATABASE_URL`. One shared mutex keeps the three env-touching
    /// tests below from racing each other.
    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[tokio::test]
    async fn set_connection_string_accepts_url_form() {
        let _g = env_test_lock();
        let key = "DATABASE_URL";
        let backup = std::env::var(key).ok();
        std::env::remove_var(key);

        let src = r#"
            function main() {
                setConnectionString("postgresql://postgres:secret@127.0.0.1:5432/myapp");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        run_main(&program).await.unwrap();
        assert_eq!(
            std::env::var(key).unwrap(),
            "postgresql://postgres:secret@127.0.0.1:5432/myapp"
        );

        if let Some(v) = backup {
            std::env::set_var(key, v);
        } else {
            std::env::remove_var(key);
        }
    }

    #[tokio::test]
    async fn set_connection_string_accepts_object_literal_form() {
        let _g = env_test_lock();
        let key = "DATABASE_URL";
        let backup = std::env::var(key).ok();
        std::env::remove_var(key);

        let src = r#"
            function main() {
                setConnectionString({
                    host:     "db.example.com",
                    port:     5433,
                    user:     "app",
                    password: "topsecret",
                    database: "prod"
                });
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        run_main(&program).await.unwrap();
        assert_eq!(
            std::env::var(key).unwrap(),
            "postgresql://app:topsecret@db.example.com:5433/prod"
        );

        if let Some(v) = backup {
            std::env::set_var(key, v);
        } else {
            std::env::remove_var(key);
        }
    }

    #[tokio::test]
    async fn set_connection_string_no_args_reads_from_env() {
        let _g = env_test_lock();
        let backup = std::env::var("DATABASE_URL").ok();
        std::env::set_var(
            "DATABASE_URL",
            "postgresql://envuser:envpw@envhost:5400/envdb",
        );

        let src = r#"
            function main() {
                setConnectionString();
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        run_main(&program).await.unwrap();
        assert_eq!(
            std::env::var("DATABASE_URL").unwrap(),
            "postgresql://envuser:envpw@envhost:5400/envdb"
        );

        if let Some(v) = backup {
            std::env::set_var("DATABASE_URL", v);
        } else {
            std::env::remove_var("DATABASE_URL");
        }
    }

    #[tokio::test]
    async fn uuid_builtin_is_v4_and_never_collides_on_a_tight_loop() {
        let src = r#"
            function main() {
                let a = uuid();
                let b = uuid();
                let c = uuid();
                print(a);
                print(b);
                print(c);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        // All three must be distinct — v4 should never collide here.
        assert!(lines[0] != lines[1]);
        assert!(lines[1] != lines[2]);
        assert!(lines[0] != lines[2]);
        // And every one is a proper RFC 4122 v4: version nibble is '4' at
        // the 14th hex digit (index 14, position [14..15] in the dashed form).
        for line in &lines {
            assert_eq!(
                line.as_bytes()[14],
                b'4',
                "expected v4 version nibble in {line}"
            );
        }
    }

    #[tokio::test]
    async fn ws_path_params_reach_the_handler() {
        // Smoke check that the runtime path-params plumbing accepts a
        // pre-populated map exactly the way `server.rs::handle_ws` will
        // hand it over. We exercise it by calling `run_ws_request`
        // directly and watching the value land in `path_param(...)`.
        let src = r#"
            route WS "/chat/{room}" {
                let r = path_param("room");
                ws_send(r);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        let (tx_to_vm, rx_to_vm) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (tx_from_vm, mut rx_from_vm) = tokio::sync::mpsc::unbounded_channel::<String>();
        // No inbound messages, so the handler runs ws_send then exits.
        drop(tx_to_vm);

        let mut params = HashMap::new();
        params.insert("room".to_string(), "general".to_string());

        run_ws_request(
            &program,
            "/chat/{room}",
            params,
            HashMap::new(),
            rx_to_vm,
            tx_from_vm,
        )
        .await
        .unwrap();

        let received = rx_from_vm.try_recv().expect("ws_send fired");
        assert_eq!(received, "general");
    }

    #[tokio::test]
    async fn ws_route_parses_with_protocol_marker() {
        let src = r#"
            route WS "/chat/{room}" {
                let msg = ws_recv();
                ws_send(msg);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.routes.len(), 1);
        assert_eq!(program.routes[0].method, "WS");
        assert_eq!(program.routes[0].path, "/chat/{room}");
        assert_eq!(program.routes[0].protocol, crate::ast::RouteProtocol::Ws);
    }

    #[tokio::test]
    async fn ws_builtins_error_outside_a_ws_handler() {
        let src = r#"
            function main() {
                ws_send("hi");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let err = run_main(&program).await.unwrap_err().to_string();
        assert!(err.contains("only valid inside a WS route"));
    }

    #[tokio::test]
    async fn string_helpers_basic_shapes() {
        let src = r#"
            function main() {
                print(lower("HELLO"));
                print(upper("Najim"));
                print(trim("   ok   "));
                print(replace("a-b-c", "-", "/"));
                print(contains("hello world", "world"));
                print(starts_with("hello", "he"));
                print(ends_with("hello", "lo"));
                print(length("hello"));
                let parts = split("a,b,c", ",");
                print(parts);
                print(length(parts));
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("hello"));
        assert!(out.contains("NAJIM"));
        assert!(out.contains("\nok\n"));
        assert!(out.contains("a/b/c"));
        assert!(out.contains("true\ntrue\ntrue\n5\n"));
        assert!(out.contains("[\"a\",\"b\",\"c\"]"));
        // length(parts) — array of 3 strings → 3.
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.last().map(|l| l.trim() == "3").unwrap_or(false),
            "{out}"
        );
    }

    #[tokio::test]
    async fn for_in_iterates_json_array() {
        let src = r#"
            function main() {
                let xs = "[1, 2, 3, 4]";
                let sum = 0;
                for x in xs {
                    sum = sum + x;
                    if (x == 3) { break; }
                }
                print(sum);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "6");
    }

    #[tokio::test]
    async fn for_in_continue_skips_iteration() {
        let src = r#"
            function main() {
                let xs = "[1, 2, 3, 4, 5]";
                let kept = 0;
                for x in xs {
                    if (x == 3) { continue; }
                    kept = kept + 1;
                }
                print(kept);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "4");
    }

    #[tokio::test]
    async fn first_last_return_array_endpoints() {
        let src = r#"
            function main() {
                let xs = "[10, 20, 30]";
                print(first(xs));
                print(last(xs));
                let empty = "[]";
                print(first(empty));
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "10");
        assert_eq!(lines[1], "30");
        assert_eq!(lines[2], "null");
    }

    #[tokio::test]
    async fn json_parse_then_stringify_roundtrip() {
        let src = r#"
            function main() {
                let parsed = json_parse("{\"a\":1,\"b\":\"x\"}");
                print(parsed);
                let back = json_stringify({ a: 1, b: "x" });
                print(back);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("\"a\":1"));
        assert!(out.contains("\"b\":\"x\""));
    }

    #[tokio::test]
    async fn error_handler_catches_uncaught_route_error() {
        let src = r#"
            errorHandler (e) {
                return internalError(e.message);
            }

            route GET "boom" {
                let x = undefined_var;
                return json("nope");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        let (status, body) = run_request(&program, "GET", "/boom", None).await.unwrap();
        assert_eq!(status, 500);
        assert!(body.contains("Undefined variable"), "body: {body}");
    }

    #[tokio::test]
    async fn error_handler_does_not_intercept_normal_responses() {
        let src = r#"
            errorHandler (e) {
                return internalError(e.message);
            }

            route GET "ok" {
                return json("hello");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        let (status, body) = run_request(&program, "GET", "/ok", None).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "hello");
    }

    #[tokio::test]
    async fn object_literal_serializes_to_json_with_nested_embedding() {
        let src = r#"
            function main() {
                let inner = "[1,2,3]";
                let payload = { name: "Najim", count: 5, items: inner, ok: true };
                print(payload);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        let trimmed = out.trim_end();
        // Order isn't guaranteed by serde_json::Map (insertion order preserved
        // since 1.0.79 with preserve_order disabled defaults to BTreeMap-like),
        // so check field presence instead of exact text.
        assert!(trimmed.contains("\"name\":\"Najim\""), "got: {trimmed}");
        assert!(trimmed.contains("\"count\":5"), "got: {trimmed}");
        assert!(
            trimmed.contains("\"items\":[1,2,3]"),
            "nested JSON should embed raw, got: {trimmed}"
        );
        assert!(trimmed.contains("\"ok\":true"), "got: {trimmed}");
    }

    #[tokio::test]
    async fn unary_not_inverts_bool() {
        let src = r#"
            function main() {
                let ok = false;
                if (!ok) { print("flipped"); }
                let n = true;
                if (!!n) { print("doubled"); }
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        assert!(out.contains("flipped"));
        assert!(out.contains("doubled"));
    }

    #[tokio::test]
    async fn now_built_in_returns_iso_8601() {
        let src = r#"
            function main() {
                let ts = now();
                print(ts);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        let trimmed = out.trim_end();
        // 2026-05-19T12:00:00.000Z shape — 4 digits, dashes, T, ms, Z.
        let bytes = trimmed.as_bytes();
        assert!(bytes.len() >= 20, "too short: {trimmed}");
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b'T');
        assert_eq!(bytes[trimmed.len() - 1], b'Z');
    }

    #[tokio::test]
    async fn at_var_field_shortcut_in_where_clause() {
        let src = r#"
            dbcontext AppDb : Postgres;
            entity User of AppDb {
                id uuid pk;
                username varchar(40);
            }

            function lookup(req) {
                let u = select User from AppDb.User
                    where User.username == @req.username first;
                return u;
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        match &program.functions[0].body[0] {
            crate::ast::Stmt::Let { value, .. } => match value {
                crate::ast::Expr::DbSelect { where_clause, .. } => {
                    let wc = where_clause.as_ref().unwrap();
                    let atom = match wc.as_ref() {
                        crate::ast::WhereExpr::Atom(a) => a,
                        _ => panic!("expected atom"),
                    };
                    match &atom.rhs {
                        crate::ast::Expr::FieldGet { var, field } => {
                            assert_eq!(var, "req");
                            assert_eq!(field, "username");
                        }
                        other => panic!("expected FieldGet, got {:?}", other),
                    }
                }
                _ => panic!("expected DbSelect"),
            },
            _ => panic!("expected Let stmt"),
        }
    }

    #[tokio::test]
    async fn unknown_function_suggests_closest_match() {
        let src = r#"
            function getAllUsers() { return 1; }
            function main() {
                let v = getAllUser();
                print(v);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let err = run_main(&program).await.unwrap_err().to_string();
        assert!(err.contains("Did you mean 'getallusers'"));
    }

    #[tokio::test]
    async fn undefined_variable_suggests_closest_match() {
        let src = r#"
            function main() {
                let userName = "x";
                print(usrName);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let err = run_main(&program).await.unwrap_err().to_string();
        assert!(err.contains("Did you mean 'username'"));
    }

    #[tokio::test]
    async fn async_function_and_await_keyword_parse_and_run() {
        let src = r#"
            async function fetch(): int {
                return 42;
            }

            function main() {
                let v = await fetch();
                print(v);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert!(program
            .functions
            .iter()
            .find(|f| f.name == "fetch")
            .map(|f| f.is_async)
            .unwrap_or(false));
        let out = run_main(&program).await.unwrap().output;
        assert_eq!(out.trim(), "42");
    }

    #[tokio::test]
    async fn middleware_short_circuits_route_when_returning() {
        let src = r#"
            middleware AuthMw {
                let token = header("authorization");
                if (token == null) {
                    return unauthorized();
                }
            }

            route GET "/secret" use AuthMw {
                return json("payload");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();

        let (status, _body) = run_request(&program, "GET", "/secret", None).await.unwrap();
        assert_eq!(status, 401);

        let mut headers = HashMap::new();
        headers.insert("authorization".into(), "Bearer xyz".into());
        let (status_ok, body_ok) =
            run_request_with_headers(&program, "GET", "/secret", None, headers)
                .await
                .unwrap();
        assert_eq!(status_ok, 200);
        assert_eq!(body_ok, "payload");
    }

    #[tokio::test]
    async fn middleware_can_share_context_with_route_handler() {
        let src = r#"
            middleware UserMw {
                setContext("userId", "u-1");
            }

            route GET "/me" use UserMw {
                return json("user=" + context("userId"));
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/me", None).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "user=u-1");
    }

    #[tokio::test]
    async fn unknown_middleware_fails_at_validation() {
        let src = r#"
            route GET "/x" use MissingMw {
                return json("hi");
            }
        "#;
        let program = parse_program(src).unwrap();
        let err = validate_program(&program).unwrap_err().to_string();
        assert!(err.contains("unknown middleware"));
    }

    #[tokio::test]
    async fn typed_route_handler_receives_path_param() {
        let src = r#"
            function getUser(id: int) {
                return "user=" + id;
            }

            route GET "/users/{id}" -> getUser;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/users/42", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "user=42");
    }

    #[tokio::test]
    async fn typed_route_handler_receives_query_param_fallback() {
        let src = r#"
            function search(q: string) {
                return "q=" + q;
            }

            route GET "/search" -> search;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/search?q=jwc", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "q=jwc");
    }

    #[tokio::test]
    async fn validate_body_returns_400_on_missing_required_field() {
        let src = r#"
            route POST "/users" {
                validate body {
                    name: required, minLength(2);
                    age: min(0), max(150);
                }
                return json("ok");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) =
            run_request(&program, "POST", "/users", Some("{\"age\":10}".to_string()))
                .await
                .unwrap();
        assert_eq!(status, 400);
        assert!(body.contains("\"errors\""));
        assert!(body.contains("\"name\""));
    }

    #[tokio::test]
    async fn validate_body_passes_when_all_rules_satisfied() {
        let src = r#"
            route POST "/users" {
                validate body {
                    name: required, minLength(2), maxLength(10);
                    age: min(0), max(150);
                }
                return json("ok");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(
            &program,
            "POST",
            "/users",
            Some("{\"name\":\"Najim\",\"age\":25}".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn validate_body_min_max_bound_violation() {
        let src = r#"
            route POST "/score" {
                validate body {
                    value: min(0), max(100);
                }
                return json("ok");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(
            &program,
            "POST",
            "/score",
            Some("{\"value\":250}".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(status, 400);
        assert!(body.contains("max(100)"));
    }

    #[tokio::test]
    async fn query_string_does_not_break_route_matching() {
        let src = r#"
            route GET "/ping" {
                return "{\"ok\":true}";
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/ping?ignored=1", None)
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
    }
}

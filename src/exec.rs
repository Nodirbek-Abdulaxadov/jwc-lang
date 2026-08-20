//! The v1 interpreter.
//!
//! Scope is v0.24.0's plus the join layer: `select` with joins, `insert` /
//! `update` / `delete` on one table, the expression core, middleware
//! chains, and the error model. Every `select` goes through
//! [`crate::query_sql`] — the join compiler subsumes the single-table case,
//! so there is no second SQL builder to keep in step. What it cannot emit
//! yet (aggregates, a view as a source) says which release it waits for
//! rather than producing an approximation.
//!
//! There is one implementation. `--native` is frozen (DEFERRED-2), so this
//! is the reference: no second backend to keep in step while the semantics
//! are still moving.

use crate::ast::*;
use crate::model::SchemaModel;
use crate::sql::{Builder, Shape};
use crate::symbols::Symbols;
use crate::value::Value;
use crate::wiring::ResolvedRoute;
use anyhow::anyhow;
use std::collections::HashMap;
use std::sync::Arc;

/// `jwc serve --dev`. Read by `debug.dump`, which prints only under it
/// (tooling.md §3.3).
static DEV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_dev_mode(on: bool) {
    DEV.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn dev_mode() -> bool {
    DEV.load(std::sync::atomic::Ordering::Relaxed)
}

// ---------------------------------------------------------------- signals

/// How a statement finished. `Throw` carries a declared error; a fault is
/// an `Err` from the surrounding `Result` (errors.md §1.4).
pub enum Flow {
    Normal,
    Return(Value),
    /// A bare `return;` — ends an `after` block (middleware.md §5.3).
    ReturnVoid,
}

#[derive(Debug, Clone)]
pub struct Thrown {
    pub error: String,
    pub args: Vec<Value>,
}

impl Thrown {
    pub fn message(&self) -> String {
        self.args
            .first()
            .and_then(|v| v.as_text().map(|s| s.to_string()))
            .unwrap_or_default()
    }
}

/// Either a declared error (which maps to a status) or a fault (500).
#[derive(Debug)]
pub enum Abort {
    Thrown(Thrown),
    Fault(anyhow::Error),
}

impl From<anyhow::Error> for Abort {
    fn from(e: anyhow::Error) -> Self {
        Abort::Fault(e)
    }
}

pub type Exec<T> = std::result::Result<T, Abort>;

fn fault(msg: impl Into<String>) -> Abort {
    Abort::Fault(anyhow!(msg.into()))
}

// ---------------------------------------------------------------- program

/// Everything the interpreter needs, resolved once at boot.
pub struct Program {
    pub model: SchemaModel,
    pub symbols: Symbols,
    pub routes: Vec<ResolvedRoute>,
    pub functions: HashMap<String, FunctionDecl>,
    pub middleware: HashMap<String, MiddlewareDecl>,
    pub route_bodies: HashMap<(String, String), Block>,
    pub error_handler: Option<ErrorHandlerDecl>,
    pub errors: HashMap<String, (u16, Option<String>, Vec<String>)>,
    pub server: ServerConfig,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub max_body_bytes: usize,
    /// Whole-request ceiling (config.md §3.2). Past it the answer is 504
    /// and the handler's task is dropped — a request that has already lost
    /// its client is a connection and a pool slot nobody is waiting on.
    pub request_timeout: std::time::Duration,
    /// Drain window on SIGTERM.
    pub shutdown_grace: std::time::Duration,
    pub max_page_size: i64,
    /// HMAC key for keyset cursors (config.md §3). A `page` query with no
    /// secret configured is `E1205`, so the runtime never has to decide
    /// what an unsigned cursor means.
    pub cursor_secret: String,
    pub strict_slash: bool,
    /// The address the listener binds (config.md §3.2). `0.0.0.0` is the
    /// default because a container's port mapping needs it; `127.0.0.1`
    /// is what keeps a development machine off its own LAN, and there was
    /// no way to ask for that while the address was hardcoded.
    pub bind: String,
    pub trusted_proxies: Vec<String>,
    /// config.md §3.4 — when present, `OPTIONS` is answered for every
    /// declared route and the headers go on every response. When absent, no
    /// CORS header is emitted at all.
    pub cors: Option<CorsConfig>,
    /// config.md §3.5 — when present the listener is HTTPS. Absent means
    /// plain HTTP, which is correct behind a terminating proxy.
    pub tls: Option<TlsConfig>,
    /// A `tls { }` block was written, whether or not its `cert` and `key`
    /// resolved. The pair is what separates "no TLS was asked for" from
    /// "TLS was asked for and `env(\"TLS_CERT_PATH\")` came back unset" —
    /// the second must stop the boot, because plain HTTP under a `tls { }`
    /// is the one misconfiguration an operator cannot see for themselves.
    pub tls_declared: bool,
    /// config.md §3.2 — the ceiling on reading the request line and
    /// headers, separate from `request_timeout` because a client that
    /// dribbles headers one byte at a time never reaches the handler the
    /// whole-request timer guards.
    pub header_timeout: std::time::Duration,
}

/// Where the listener's certificate and key are read from. Both are
/// paths, and both are resolved at boot: a `tls { }` naming a file that
/// is missing or malformed stops the server rather than falling back to
/// plain HTTP, which is the failure an operator cannot see.
#[derive(Clone, Debug)]
pub struct TlsConfig {
    pub cert: String,
    pub key: String,
}

#[derive(Clone, Debug, Default)]
pub struct CorsConfig {
    pub origins: Vec<String>,
    pub methods: Vec<String>,
    pub headers: Vec<String>,
    pub credentials: bool,
    pub max_age: Option<std::time::Duration>,
}

impl CorsConfig {
    /// The `Access-Control-Allow-Origin` for a request, or `None` when the
    /// origin is not allowed.
    ///
    /// The origin is echoed rather than answered with `*`, because `*` and
    /// `credentials` are mutually exclusive in the fetch spec and echoing
    /// keeps one code path for both. `["*"]` still means "any origin", and
    /// with credentials on it echoes — which is the only way that
    /// combination can work at all, and the reason `*` with credentials is
    /// worth thinking about before writing.
    pub fn allow(&self, origin: &str) -> Option<String> {
        if self.origins.iter().any(|o| o == "*") || self.origins.iter().any(|o| o == origin) {
            return Some(origin.to_string());
        }
        None
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 1_048_576,
            request_timeout: std::time::Duration::from_secs(30),
            shutdown_grace: std::time::Duration::from_secs(20),
            max_page_size: 100,
            cursor_secret: String::new(),
            strict_slash: true,
            bind: "0.0.0.0".into(),
            trusted_proxies: Vec::new(),
            cors: None,
            tls: None,
            tls_declared: false,
            header_timeout: std::time::Duration::from_secs(10),
        }
    }
}

/// Per-request state.
pub struct Request {
    pub method: String,
    pub path: String,
    /// The declared pattern (routing.md §5.4).
    pub route: String,
    pub headers: HashMap<String, String>,
    pub query: Vec<(String, String)>,
    /// Read once, before middleware (routing.md §5.1). `raw_body()` and
    /// `body() as C` are two views of this one buffer.
    pub body: String,
    pub peer_ip: String,
    pub client_ip: String,
    pub id: String,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn json(status: u16, value: &Value) -> Response {
        let mut body = String::new();
        value.write_json(&mut body);
        Response {
            status,
            body,
            headers: vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            )],
        }
    }

    pub fn message(status: u16, message: &str) -> Response {
        Response::json(
            status,
            &Value::Record(vec![("error".into(), Value::Text(message.to_string()))]),
        )
    }

    pub fn empty(status: u16) -> Response {
        Response {
            status,
            body: String::new(),
            headers: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------- vm

pub struct Vm<'a> {
    pub program: &'a Program,
    pub request: Arc<Request>,
    scopes: Vec<HashMap<String, Value>>,
    params: HashMap<String, Value>,
    context: HashMap<String, Value>,
    /// Set once an `after` block is running: `response.status()` reads it.
    pub response_status: Option<u16>,
    pub extra_headers: Vec<(String, String)>,
    depth: u32,
}

const MAX_DEPTH: u32 = 128;

impl<'a> Vm<'a> {
    pub fn new(program: &'a Program, request: Arc<Request>) -> Self {
        Self {
            program,
            request,
            scopes: vec![HashMap::new()],
            params: HashMap::new(),
            context: HashMap::new(),
            response_status: None,
            extra_headers: Vec::new(),
            depth: 0,
        }
    }

    pub fn set_params(&mut self, params: HashMap<String, Value>) {
        self.params = params;
    }

    pub fn set_context(&mut self, key: &str, v: Value) {
        self.context.insert(key.to_string(), v);
    }

    /// A call gets a fresh local frame: parameters are the only things
    /// visible, and the caller's locals are not.
    pub(super) fn enter_function(&mut self) -> Vec<HashMap<String, Value>> {
        let saved = std::mem::take(&mut self.scopes);
        self.scopes.push(HashMap::new());
        saved
    }

    pub(super) fn leave_function(&mut self, saved: Vec<HashMap<String, Value>>) {
        self.scopes = saved;
    }

    pub(super) fn bind_param(&mut self, name: &str, v: Value) {
        self.declare(name, v);
    }

    pub(super) async fn run_body(&mut self, b: &Block) -> Exec<Flow> {
        // A postfix `catch` unwinds through a synthetic throw so it can
        // return from the *enclosing* function (errors.md §7.2).
        match Box::pin(self.run_block(b)).await {
            Err(Abort::Thrown(t)) if t.error == "__return" => Ok(Flow::Return(
                t.args.into_iter().next().unwrap_or(Value::Null),
            )),
            Err(Abort::Thrown(t)) if t.error == "__return_void" => Ok(Flow::ReturnVoid),
            other => other,
        }
    }

    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, v: Value) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), v);
        }
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn assign(&mut self, name: &str, v: Value) {
        for s in self.scopes.iter_mut().rev() {
            if s.contains_key(name) {
                s.insert(name.to_string(), v);
                return;
            }
        }
        self.declare(name, v);
    }

    // ------------------------------------------------------------ blocks

    pub async fn run_block(&mut self, b: &Block) -> Exec<Flow> {
        self.push();
        let r = self.run_stmts(b).await;
        self.pop();
        r
    }

    async fn run_stmts(&mut self, b: &Block) -> Exec<Flow> {
        for s in b {
            match Box::pin(self.stmt(s)).await? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    async fn stmt(&mut self, s: &Stmt) -> Exec<Flow> {
        match s {
            Stmt::Let { name, value, .. } => {
                let v = self.eval(value).await?;
                self.declare(&name.name, v);
                Ok(Flow::Normal)
            }
            Stmt::Assign { target, value, .. } => {
                let v = self.eval(value).await?;
                match target {
                    AssignTarget::Local(i) => self.assign(&i.name, v),
                    AssignTarget::Context(k) => {
                        self.context.insert(k.name.clone(), v);
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let c = self.eval(cond).await?;
                let taken = c
                    .truthy()
                    .ok_or_else(|| fault("condition is not boolean"))?;
                if taken {
                    Box::pin(self.run_block(then)).await
                } else if let Some(alt) = otherwise {
                    Box::pin(self.run_block(alt)).await
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::For {
                binder,
                iterable,
                body,
                ..
            } => {
                let it = self.eval(iterable).await?;
                let items = match it {
                    Value::Array(items) => items,
                    Value::Null => Vec::new(),
                    _ => return Err(fault("`for` needs an array")),
                };
                for item in items {
                    self.push();
                    self.declare(&binder.name, item);
                    let r = Box::pin(self.run_stmts(body)).await;
                    self.pop();
                    match r? {
                        Flow::Normal => {}
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Return { value, .. } => match value {
                None => Ok(Flow::ReturnVoid),
                Some(v) => {
                    let val = self.eval(v).await?;
                    Ok(Flow::Return(val))
                }
            },
            Stmt::Throw { error, args, .. } => {
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval(a).await?);
                }
                Err(Abort::Thrown(Thrown {
                    error: error.name.clone(),
                    args: vals,
                }))
            }
            // writes.md §7 — the block commits on `return` and rolls back on
            // a throw. The transaction is the connection's, so the whole
            // block runs inside `with_tx`.
            Stmt::Transaction { body, .. } => Box::pin(self.transaction(body)).await,
            Stmt::Assert { kind, .. } => match kind {
                AssertKind::Expr(e) => {
                    let v = self.eval(e).await?;
                    if v.truthy() == Some(true) {
                        Ok(Flow::Normal)
                    } else {
                        Err(fault("assertion failed"))
                    }
                }
                AssertKind::Fails {
                    error,
                    body,
                    message,
                    ..
                } => {
                    let want = error.as_ref().map(|e| e.name.as_str()).unwrap_or("");
                    match Box::pin(self.in_savepoint(body)).await {
                        Ok(_) => Err(fault(format!("expected `{want}`, but the block succeeded"))),
                        Err(Abort::Thrown(t)) if t.error == want => match message {
                            // testing.md §4.3 — compared exactly, and both
                            // strings printed. "Close enough" is how a
                            // message drifts.
                            Some(m) if &t.message() != m => Err(fault(format!(
                                "expected `{want}` with message\n  want: {m}\n  got:  {}",
                                t.message()
                            ))),
                            _ => Ok(Flow::Normal),
                        },
                        Err(Abort::Thrown(t)) => Err(fault(format!(
                            "expected `{want}`, got `{}`: {}",
                            t.error,
                            t.message()
                        ))),
                        Err(Abort::Fault(e)) => {
                            Err(fault(format!("expected `{want}`, got a fault: {e}")))
                        }
                    }
                }
            },
            Stmt::Expr { expr, .. } => {
                self.eval(expr).await?;
                Ok(Flow::Normal)
            }
        }
    }

    /// `transaction { }` — BEGIN, run, COMMIT on `return`, ROLLBACK on a
    /// throw or a fault (writes.md §7.1). The errorHandler runs *outside*,
    /// after the rollback (§7.2), which is the caller's job.
    async fn transaction(&mut self, body: &Block) -> Exec<Flow> {
        self.in_scoped_transaction(body, false).await
    }

    /// `BEGIN`, run, then `COMMIT` (or `ROLLBACK` on failure, or always
    /// when `rollback_always` — which is how a test is isolated,
    /// testing.md §2.1).
    ///
    /// The connection is **pinned** for the duration. Every statement the
    /// block issues goes through `db.rs`, which reads the pin: without it
    /// the `BEGIN` lands on one pooled connection and the statements on
    /// whichever others the pool hands out, so the block commits nothing
    /// and rolls back nothing.
    pub async fn in_scoped_transaction(
        &mut self,
        body: &Block,
        rollback_always: bool,
    ) -> Exec<Flow> {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let conn = crate::engine::get_connection()
            .await
            .map_err(Abort::Fault)?;
        conn.batch_execute("BEGIN")
            .await
            .map_err(|e| fault(e.to_string()))?;
        let cell = Arc::new(Mutex::new(Some(conn)));

        let r = crate::engine::TX_CONN
            .scope(cell.clone(), Box::pin(self.run_block(body)))
            .await;

        let mut held = cell.lock().await;
        if let Some(conn) = held.take() {
            if r.is_ok() && !rollback_always {
                conn.batch_execute("COMMIT")
                    .await
                    .map_err(|e| fault(e.to_string()))?;
            } else {
                let _ = conn.batch_execute("ROLLBACK").await;
            }
        }
        r
    }

    /// The body of an `assert fails`, inside a savepoint (testing.md §4.4).
    ///
    /// Postgres refuses every statement in a transaction that has seen an
    /// error (`25P02`), so without the savepoint a test that asserts a
    /// failure could not do anything afterwards — including the rollback
    /// that isolates it.
    async fn in_savepoint(&mut self, body: &Block) -> Exec<Flow> {
        let Some(cell) = crate::engine::pinned_connection() else {
            // Outside a transaction there is nothing to protect.
            return Box::pin(self.run_block(body)).await;
        };
        const NAME: &str = "jwc_assert_fails";
        {
            let mut held = cell.lock().await;
            if let Some(conn) = held.as_mut() {
                conn.batch_execute(&format!("SAVEPOINT {NAME}"))
                    .await
                    .map_err(|e| fault(e.to_string()))?;
            }
        }
        let r = Box::pin(self.run_block(body)).await;
        let mut held = cell.lock().await;
        if let Some(conn) = held.as_mut() {
            let sql = match &r {
                Ok(_) => format!("RELEASE SAVEPOINT {NAME}"),
                Err(_) => format!("ROLLBACK TO SAVEPOINT {NAME}; RELEASE SAVEPOINT {NAME}"),
            };
            let _ = conn.batch_execute(&sql).await;
        }
        r
    }

    // ------------------------------------------------------------ exprs

    pub async fn eval(&mut self, e: &Expr) -> Exec<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(fault("expression nesting is too deep"));
        }
        let r = Box::pin(self.eval_inner(e)).await;
        self.depth -= 1;
        r
    }

    async fn eval_inner(&mut self, e: &Expr) -> Exec<Value> {
        Ok(match &*e.kind {
            ExprKind::Int(n) => Value::Int(n.parse().unwrap_or(0)),
            ExprKind::Decimal(n) => Value::Numeric(n.clone()),
            ExprKind::Str(s) | ExprKind::RawStr(s) => Value::Text(s.clone()),
            ExprKind::Bool(b) => Value::Bool(*b),
            ExprKind::Null => Value::Null,

            ExprKind::Local(i) => self
                .lookup(&i.name)
                .cloned()
                .ok_or_else(|| fault(format!("unknown local `${}`", i.name)))?,

            ExprKind::PathParam(i) => self
                .params
                .get(&i.name)
                .cloned()
                .ok_or_else(|| fault(format!("unknown path parameter `@{}`", i.name)))?,

            ExprKind::Name(i) => Value::Text(i.name.clone()),

            ExprKind::Field { base, field } => self.field(base, field).await?,

            ExprKind::Index { base, index } => {
                let b = self.eval(base).await?;
                let i = self.eval(index).await?;
                let idx = i.as_i64().unwrap_or(-1);
                match b {
                    Value::Array(items) if idx >= 0 => {
                        items.get(idx as usize).cloned().unwrap_or(Value::Null)
                    }
                    _ => Value::Null,
                }
            }

            ExprKind::Call { callee, args, .. } => self.call(callee, args).await?,

            ExprKind::Unary { op, rhs } => {
                let v = self.eval(rhs).await?;
                match op {
                    UnaryOp::Not => {
                        Value::Bool(!v.truthy().ok_or_else(|| fault("`!` needs a boolean"))?)
                    }
                    UnaryOp::Neg => match v {
                        Value::Int(n) => Value::Int(-n),
                        Value::Bigint(n) => Value::Bigint(-n),
                        Value::Numeric(s) => Value::Numeric(format!("-{s}")),
                        _ => return Err(fault("cannot negate this value")),
                    },
                }
            }

            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs).await?,

            ExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => {
                let c = self.eval(cond).await?;
                if c.truthy()
                    .ok_or_else(|| fault("condition is not boolean"))?
                {
                    self.eval(then).await?
                } else {
                    self.eval(otherwise).await?
                }
            }

            ExprKind::Coalesce { lhs, rhs } => {
                let a = self.eval(lhs).await?;
                if a.is_null() {
                    self.eval(rhs).await?
                } else {
                    a
                }
            }

            ExprKind::In {
                lhs,
                items,
                negated,
            } => {
                let l = self.eval(lhs).await?;
                let mut found = false;
                for i in items {
                    let v = self.eval(i).await?;
                    match &v {
                        Value::Array(xs) => {
                            if xs.contains(&l) {
                                found = true;
                            }
                        }
                        other => {
                            if *other == l {
                                found = true;
                            }
                        }
                    }
                }
                Value::Bool(found != *negated)
            }

            ExprKind::Exists { .. } => {
                return Err(fault("`exists (...)` needs the query compiler (v0.25.0)"))
            }

            ExprKind::Object(entries) => {
                let mut fields = Vec::new();
                for entry in entries {
                    match entry {
                        ObjEntry::Field { key, value, .. } => {
                            let v = self.eval(value).await?;
                            fields.push((key.name.clone(), v));
                        }
                        ObjEntry::Spread { source, except, .. } => {
                            let v = self
                                .lookup(&source.name)
                                .cloned()
                                .ok_or_else(|| fault("unknown spread source"))?;
                            if let Value::Record(inner) = v {
                                for (k, val) in inner {
                                    if except.iter().any(|x| x.name == k) {
                                        continue;
                                    }
                                    fields.push((k, val));
                                }
                            }
                        }
                    }
                }
                Value::Record(fields)
            }

            ExprKind::Array(items) => {
                let mut out = Vec::new();
                for i in items {
                    out.push(self.eval(i).await?);
                }
                Value::Array(out)
            }

            ExprKind::Select(s) => self.run_select(s).await?,
            ExprKind::Insert(i) => self.run_insert(i).await?,
            ExprKind::Update(u) => self.run_update(u).await?,
            ExprKind::Delete(d) => self.run_delete(d).await?,

            ExprKind::OrThrow { value, error, args } => {
                let v = self.eval(value).await?;
                if v.is_null() {
                    let mut vals = Vec::new();
                    for a in args {
                        vals.push(self.eval(a).await?);
                    }
                    return Err(Abort::Thrown(Thrown {
                        error: error.name.clone(),
                        args: vals,
                    }));
                }
                v
            }

            ExprKind::CatchPostfix {
                value,
                error,
                binder,
                body,
            } => match Box::pin(self.eval(value)).await {
                Ok(v) => v,
                Err(Abort::Thrown(t)) if t.error == error.name => {
                    self.push();
                    let payload = Value::Record(vec![("message".into(), Value::Text(t.message()))]);
                    self.declare(&binder.name, payload);
                    let r = Box::pin(self.run_stmts(body)).await;
                    self.pop();
                    return match r? {
                        // errors.md §7.2 — the block must diverge, which the
                        // checker enforces; a `return` here returns from the
                        // enclosing function.
                        Flow::Return(v) => Err(Abort::Thrown(Thrown {
                            error: "__return".into(),
                            args: vec![v],
                        })),
                        _ => Err(Abort::Thrown(Thrown {
                            error: "__return_void".into(),
                            args: vec![],
                        })),
                    };
                }
                Err(other) => return Err(other),
            },

            // routing.md §5.2 — the cast is what validates.
            ExprKind::Cast { value: _, ty } => self.validate_body(&ty.name)?,

            // routing.md §6.2 — the header suffix attaches to the response
            // it decorates, so it survives being nested in `created(...)`.
            ExprKind::WithHeaders { value, headers } => {
                let mut v = self.eval(value).await?;
                let mut collected = Vec::new();
                for h in headers {
                    if let ObjEntry::Field { key, value, .. } = h {
                        let hv = self.eval(value).await?;
                        collected.push((key.name.clone(), value_text(&hv)));
                    }
                }
                match &mut v {
                    Value::Response { headers, .. } => headers.extend(collected),
                    _ => self.extra_headers.extend(collected),
                }
                v
            }

            // A repeated header needs a form a JSON object cannot express.
            ExprKind::Cookie { value, args } => {
                let mut v = self.eval(value).await?;
                let mut parts = Vec::new();
                for a in args {
                    parts.push(self.eval(a).await?);
                }
                if let (Some(name), Some(val)) = (parts.first(), parts.get(1)) {
                    let cookie = (
                        "set-cookie".to_string(),
                        format!("{}={}; Path=/", value_text(name), value_text(val)),
                    );
                    match &mut v {
                        Value::Response { headers, .. } => headers.push(cookie),
                        _ => self.extra_headers.push(cookie),
                    }
                }
                v
            }
        })
    }

    async fn field(&mut self, base: &Expr, field: &Ident) -> Exec<Value> {
        // `context.k` / `context.k?`
        if let ExprKind::Name(n) = &*base.kind {
            if n.name == "context" {
                let key = field.name.trim_end_matches('?');
                return Ok(self.context.get(key).cloned().unwrap_or(Value::Null));
            }
            // An enum member is its own name on the wire (types.md §3.4).
            if self.program.symbols.enums.contains_key(&n.name) {
                return Ok(Value::Text(field.name.clone()));
            }
        }
        let b = self.eval(base).await?;
        if b.is_null() {
            return Err(fault(format!("`{}` read on a null value", field.name)));
        }
        if let Value::Raw(_) = b {
            return Err(fault("field read on a raw result"));
        }
        Ok(b.field(&field.name).cloned().unwrap_or(Value::Null))
    }

    async fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Exec<Value> {
        // Short-circuit before evaluating the right side.
        if matches!(op, BinOp::And | BinOp::Or) {
            let a = self.eval(lhs).await?;
            let a = a
                .truthy()
                .ok_or_else(|| fault("`and`/`or` need booleans"))?;
            if (op == BinOp::And && !a) || (op == BinOp::Or && a) {
                return Ok(Value::Bool(a));
            }
            let b = self.eval(rhs).await?;
            return Ok(Value::Bool(
                b.truthy()
                    .ok_or_else(|| fault("`and`/`or` need booleans"))?,
            ));
        }

        let a = self.eval(lhs).await?;
        let b = self.eval(rhs).await?;
        Ok(match op {
            BinOp::Eq | BinOp::EqOpt => Value::Bool(equal(&a, &b)),
            BinOp::Ne => Value::Bool(!equal(&a, &b)),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ord = compare(&a, &b).ok_or_else(|| fault("values do not order"))?;
                Value::Bool(match op {
                    BinOp::Lt => ord.is_lt(),
                    BinOp::Le => ord.is_le(),
                    BinOp::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                })
            }
            BinOp::Add => add(&a, &b)
                .ok_or_else(|| fault(format!("`+` is not defined here: {a:?} + {b:?}")))?,
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                numeric_op(op, &a, &b).ok_or_else(|| fault("arithmetic is not defined here"))?
            }
            BinOp::Like | BinOp::ILike => Value::Bool(false),
            BinOp::And | BinOp::Or => unreachable!(),
        })
    }

    /// routing.md §5.2 / types.md §11 — parse the one body buffer and
    /// validate it against the class. Failure is `BadRequest` with the
    /// fixed `validation_failed` shape.
    fn validate_body(&mut self, class: &str) -> Exec<Value> {
        let Some(sym) = self.program.symbols.classes.get(class) else {
            return Err(fault(format!("unknown class `{class}`")));
        };
        let parsed: serde_json::Value = serde_json::from_str(&self.request.body).map_err(|_| {
            validation_error(vec![field_error(
                "",
                "json",
                None,
                "body is not valid JSON",
            )])
        })?;

        let mut fields = Vec::new();
        let mut failures = Vec::new();
        crate::validate::validate_class(
            sym,
            &self.program.symbols,
            &parsed,
            "",
            &mut fields,
            &mut failures,
        );
        if !failures.is_empty() {
            return Err(validation_error(failures));
        }
        Ok(Value::Record(fields))
    }
}

pub fn validation_error(failures: Vec<Value>) -> Abort {
    Abort::Thrown(Thrown {
        error: "BadRequest".into(),
        args: vec![
            Value::Text("validation_failed".into()),
            Value::Array(failures),
        ],
    })
}

pub fn field_error(path: &str, rule: &str, limit: Option<i64>, message: &str) -> Value {
    let mut f = vec![
        ("path".into(), Value::Text(path.to_string())),
        ("rule".into(), Value::Text(rule.to_string())),
    ];
    if let Some(l) = limit {
        f.push(("limit".into(), Value::Int(l)));
    }
    f.push(("message".into(), Value::Text(message.to_string())));
    Value::Record(f)
}

// ---------------------------------------------------------------- ops

fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(x), Value::Bigint(y)) | (Value::Bigint(x), Value::Int(y)) => x == y,
        _ => {
            if let (Some(x), Some(y)) = (a.as_text(), b.as_text()) {
                return x == y;
            }
            a == b
        }
    }
}

fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(x), Some(y)) = (numeric_of(a), numeric_of(b)) {
        return x.partial_cmp(&y);
    }
    match (a.as_text(), b.as_text()) {
        (Some(x), Some(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn numeric_of(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) | Value::Bigint(n) => Some(*n as f64),
        Value::Numeric(s) => s.parse().ok(),
        _ => None,
    }
}

fn add(a: &Value, b: &Value) -> Option<Value> {
    // types.md §12.1 — three overloads and no implicit stringification.
    if let (Value::Text(x), Value::Text(y)) = (a, b) {
        return Some(Value::Text(format!("{x}{y}")));
    }
    if let (Value::Timestamptz(t), Value::Interval(i)) = (a, b) {
        return Some(Value::Timestamptz(shift(t, i)?));
    }
    numeric_op(BinOp::Add, a, b)
}

fn numeric_op(op: BinOp, a: &Value, b: &Value) -> Option<Value> {
    // Integer arithmetic stays exact; anything with a decimal goes through
    // an exact decimal string so money never touches a float.
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        if !matches!(a, Value::Numeric(_)) && !matches!(b, Value::Numeric(_)) {
            let r = match op {
                BinOp::Add => x.checked_add(y),
                BinOp::Sub => x.checked_sub(y),
                BinOp::Mul => x.checked_mul(y),
                BinOp::Div => x.checked_div(y),
                BinOp::Rem => x.checked_rem(y),
                _ => None,
            }?;
            let wide = matches!(a, Value::Bigint(_)) || matches!(b, Value::Bigint(_));
            return Some(if wide {
                Value::Bigint(r)
            } else {
                Value::Int(r)
            });
        }
    }
    let (x, y) = (numeric_of(a)?, numeric_of(b)?);
    let r = match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => {
            if y == 0.0 {
                return None;
            }
            x / y
        }
        _ => return None,
    };
    Some(Value::Numeric(format_decimal(r)))
}

fn format_decimal(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() {
        "0".to_string()
    } else {
        s
    }
}

/// `timestamptz + interval`. Intervals are carried in ISO 8601 form, which
/// is also their wire form (types.md §2.1).
fn shift(ts: &str, interval: &str) -> Option<String> {
    use chrono::{DateTime, Duration, Utc};
    let base: DateTime<Utc> = ts.parse().ok()?;
    let d = parse_iso_duration(interval)?;
    let out = base.checked_add_signed(Duration::seconds(d))?;
    Some(out.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
}

fn parse_iso_duration(s: &str) -> Option<i64> {
    // `P30D`, `PT10S`, `PT2H`, `PT5M`
    let s = s.strip_prefix('P')?;
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let mut total = 0i64;
    let mut num = String::new();
    for c in date.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            total += match c {
                'D' => n * 86_400,
                'W' => n * 604_800,
                _ => return None,
            };
        }
    }
    for c in time.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            total += match c {
                'H' => n * 3_600,
                'M' => n * 60,
                'S' => n,
                _ => return None,
            };
        }
    }
    Some(total)
}

// ---------------------------------------------------------------- queries

impl<'a> Vm<'a> {
    async fn run_select(&mut self, s: &SelectExpr) -> Exec<Value> {
        // One path for every select: the join compiler subsumes the
        // single-table case, so there is no second implementation to keep
        // in step.
        let plan = crate::query::plan(s, &self.program.symbols);
        let mut c = crate::query_sql::Compiler::new(&self.program.model)
            .max_page_size(self.program.server.max_page_size);
        let Some(compiled) = c.compile(s, &plan) else {
            // The compiler names the missing piece; repeating a generic
            // "not expressible" here would throw that away.
            return Err(fault(c.gap()));
        };
        self.run_sql(crate::sql::Built {
            sql: compiled.sql,
            params: compiled.params,
            shape: compiled.shape,
            record: compiled.record,
            fields: compiled.fields,
            page: compiled.page,
        })
        .await
    }

    async fn run_insert(&mut self, i: &InsertExpr) -> Exec<Value> {
        let fields = self.write_fields(&i.values).await?;
        let mut b = Builder::new(&self.program.model);
        let built = b
            .insert(i, &fields.0)
            .ok_or_else(|| fault("this insert is not expressible yet"))?;
        // The values were already evaluated, so re-evaluating a parameter
        // must not repeat a side effect: bind the computed ones.
        self.run_sql_with(built, fields.1).await
    }

    async fn run_update(&mut self, u: &UpdateExpr) -> Exec<Value> {
        // One list, in source order: each assignment is either a value the
        // interpreter computed or an expression the database will.
        let mut sets: Vec<(String, crate::sql::SetValue)> = Vec::new();
        let mut preset: Vec<Value> = Vec::new();
        for it in &u.sets {
            match it {
                SetItem::Set {
                    column,
                    value,
                    optional,
                    ..
                } => {
                    // An expression that reads the row's own columns
                    // belongs in the database: `set value = value + 1` is
                    // an increment, and computing it here would need a read
                    // first — which is the race writes.md §2.3 is about.
                    if self.reads_a_column(&u.table, value) {
                        sets.push((
                            column.name.clone(),
                            crate::sql::SetValue::Sql(value.clone()),
                        ));
                        continue;
                    }
                    let v = self.eval(value).await?;
                    // writes.md §3.3 — `=?` skips the assignment when the
                    // value is absent.
                    if *optional && v.is_null() {
                        continue;
                    }
                    sets.push((
                        column.name.clone(),
                        crate::sql::SetValue::Bound(placeholder(u.span)),
                    ));
                    preset.push(v);
                }
                SetItem::Spread { source, except, .. } => {
                    let v = self
                        .lookup(&source.name)
                        .cloned()
                        .ok_or_else(|| fault("unknown spread source"))?;
                    if let Value::Record(inner) = v {
                        for (k, val) in inner {
                            if except.iter().any(|x| x.name == k) {
                                continue;
                            }
                            // types.md §9.2 — an absent field is omitted,
                            // so its default or current value stands.
                            sets.push((k, crate::sql::SetValue::Bound(placeholder(u.span))));
                            preset.push(val);
                        }
                    }
                }
            }
        }

        // types.md §9.5 — an all-absent spread skips the UPDATE and reads
        // the current row instead of emitting an empty SET.
        if sets.is_empty() {
            let probe = SelectExpr {
                binder: Ident::new("x", u.span),
                source: u.table.clone(),
                joins: vec![],
                filter: u.filter.clone(),
                group_by: vec![],
                having: None,
                projection: u.projection.clone(),
                order_by: u.order_by.clone(),
                limit: None,
                page: None,
                first: u.first,
                span: u.span,
            };
            return Box::pin(self.run_select(&probe)).await;
        }

        let mut b = Builder::new(&self.program.model);
        let built = b
            .update(u, &sets)
            .ok_or_else(|| fault("this update is not expressible yet"))?;
        let values = self.bind_params(&built, Vec::new(), preset).await?;
        self.run_sql_with(built, values).await
    }

    async fn run_delete(&mut self, d: &DeleteExpr) -> Exec<Value> {
        let mut b = Builder::new(&self.program.model);
        let built = b
            .delete(d)
            .ok_or_else(|| fault("this delete is not expressible yet"))?;
        self.run_sql(built).await
    }

    /// Evaluate the object entries of an INSERT, returning both the
    /// placeholder list the builder needs and the computed values.
    #[allow(clippy::type_complexity)]
    async fn write_fields(
        &mut self,
        entries: &[ObjEntry],
    ) -> Exec<(Vec<(String, Expr)>, Vec<Value>)> {
        let mut names = Vec::new();
        let mut values = Vec::new();
        for e in entries {
            match e {
                ObjEntry::Field {
                    key, value, span, ..
                } => {
                    let v = self.eval(value).await?;
                    names.push((key.name.clone(), placeholder(*span)));
                    values.push(v);
                }
                ObjEntry::Spread {
                    source,
                    except,
                    span,
                } => {
                    let v = self
                        .lookup(&source.name)
                        .cloned()
                        .ok_or_else(|| fault("unknown spread source"))?;
                    if let Value::Record(inner) = v {
                        for (k, val) in inner {
                            if except.iter().any(|x| x.name == k) {
                                continue;
                            }
                            // An absent field is omitted from the column
                            // list so the column's default applies.
                            if val.is_null() {
                                continue;
                            }
                            names.push((k, placeholder(*span)));
                            values.push(val);
                        }
                    }
                }
            }
        }
        Ok((names, values))
    }

    async fn run_sql(&mut self, built: crate::sql::Built) -> Exec<Value> {
        // A `page` query's cursor parameters are the values the *caller's*
        // cursor carries, so the cursor is read once, here, before any of
        // them are bound.
        let cursor = match &built.page {
            Some(p) => self.read_cursor(p).await?,
            None => Vec::new(),
        };
        let values = self.bind_params(&built, cursor, Vec::new()).await?;
        self.run_sql_with(built, values).await
    }

    /// Every parameter, in emission order.
    ///
    /// `preset` holds values the caller evaluated before the statement was
    /// built — an `insert`'s or `update`'s values, which must not be
    /// re-evaluated because evaluating them again would repeat whatever
    /// they did the first time.
    async fn bind_params(
        &mut self,
        built: &crate::sql::Built,
        cursor: Vec<Option<String>>,
        preset: Vec<Value>,
    ) -> Exec<Vec<Value>> {
        let mut preset = preset.into_iter();
        let mut out = Vec::with_capacity(built.params.len());
        for p in &built.params {
            out.push(match &p.bind {
                crate::sql::Bind::Preset => preset.next().unwrap_or(Value::Null),
                crate::sql::Bind::Cursor(i) => cursor
                    .get(*i)
                    .cloned()
                    .flatten()
                    .map_or(Value::Null, Value::Text),
                crate::sql::Bind::Expr(e) => self.eval(&e.clone()).await?,
            });
        }
        Ok(out)
    }

    /// True when an expression reads a column of the table being written.
    fn reads_a_column(&self, table: &QualifiedTable, e: &Expr) -> bool {
        let object = self
            .program
            .symbols
            .by_path
            .get(&table.text())
            .cloned()
            .unwrap_or_else(|| table.object.name.clone());
        let Some(t) = self
            .program
            .model
            .tables
            .iter()
            .find(|t| t.declared == object)
        else {
            return false;
        };
        match &*e.kind {
            ExprKind::Name(n) => t.column(&n.name).is_some(),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.reads_a_column(table, lhs) || self.reads_a_column(table, rhs)
            }
            ExprKind::Unary { rhs, .. } => self.reads_a_column(table, rhs),
            _ => false,
        }
    }

    /// queries.md §9.3 — `{items, next, has_more}`.
    ///
    /// `items` is spliced, not rebuilt: it reaches the response as the text
    /// Postgres produced, which is what makes raw survive the envelope
    /// (types.md §5.4).
    async fn page_envelope(
        &mut self,
        built: &crate::sql::Built,
        plan: &crate::sql::PagePlan,
        binds: &[Option<String>],
    ) -> Exec<Value> {
        let (items, keys, has_more) = crate::db::run_page(&built.sql, binds)
            .await
            .map_err(map_db_error)?;

        // The next page starts after the last row on this one. With no next
        // page there is no cursor: a caller that follows `next` until it is
        // null cannot loop.
        let next = if has_more {
            last_tuple(&keys).map(|t| {
                Value::Text(crate::cursor::encode(
                    &self.program.server.cursor_secret,
                    &t,
                ))
            })
        } else {
            None
        };

        let items = if plan.raw_items {
            Value::Raw(items)
        } else {
            match serde_json::from_str::<serde_json::Value>(&items) {
                Ok(j) => reorder(Value::from_json(&j), &built.fields),
                Err(_) => Value::Raw(items),
            }
        };

        Ok(Value::Record(vec![
            ("items".into(), items),
            ("next".into(), next.unwrap_or(Value::Null)),
            ("has_more".into(), Value::Bool(has_more)),
        ]))
    }

    /// The key values the caller's cursor carries, or an empty tuple for
    /// the first page.
    ///
    /// A cursor that does not verify is a `BadRequest`, not a 500 and not a
    /// silently-empty page. It is client input, and the only honest answer
    /// is that it is not a cursor we issued (queries.md §9.3).
    async fn read_cursor(&mut self, plan: &crate::sql::PagePlan) -> Exec<Vec<Option<String>>> {
        let Some(expr) = &plan.after else {
            return Ok(Vec::new());
        };
        let v = self.eval(&expr.clone()).await?;
        let Some(text) = v.as_text().map(|s| s.to_string()) else {
            return Ok(Vec::new());
        };
        if text.is_empty() {
            return Ok(Vec::new());
        }
        match crate::cursor::decode(&self.program.server.cursor_secret, &text) {
            Some(keys) => Ok(keys),
            None => Err(Abort::Thrown(Thrown {
                error: "BadRequest".into(),
                args: vec![Value::Text("kursor yaroqsiz".into())],
            })),
        }
    }

    async fn run_sql_with(&mut self, built: crate::sql::Built, values: Vec<Value>) -> Exec<Value> {
        let binds: Vec<Option<String>> = values.iter().map(|v| v.to_bind()).collect();
        if let Some(plan) = &built.page {
            return self.page_envelope(&built, plan, &binds).await;
        }

        let text = crate::db::run(&built.sql, &binds, built.shape)
            .await
            .map_err(map_db_error)?;

        // A projected result is parsed so its fields can be read; a raw one
        // never is (types.md §5.1, §5.3). The two agree on the wire because
        // the projection already applied the casts (queries.md §7.2).
        let order = built.fields.clone();
        let wrap = |t: String| -> Value {
            if built.record {
                match serde_json::from_str::<serde_json::Value>(&t) {
                    // serde_json's default object map is sorted, so the
                    // parsed value must be rebuilt in projection order.
                    Ok(j) => reorder(Value::from_json(&j), &order),
                    Err(_) => Value::Raw(t),
                }
            } else {
                Value::Raw(t)
            }
        };

        Ok(match built.shape {
            Shape::None => Value::Null,
            Shape::First => match text {
                None => Value::Null,
                Some(t) if t == "null" => Value::Null,
                Some(t) => wrap(t),
            },
            Shape::Rows => wrap(text.unwrap_or_else(|| "[]".into())),
        })
    }
}

/// errors.md §6 — a violated constraint carrying a message becomes a
/// declared error; a message-less one stays a fault.
pub(super) fn map_db_error(e: crate::db::DbError) -> Abort {
    match e {
        crate::db::DbError::Constraint {
            name,
            message,
            kind,
        } => match message {
            Some(m) => Abort::Thrown(Thrown {
                error: match kind {
                    crate::db::ConstraintKind::Unique => "Conflict",
                    _ => "BadRequest",
                }
                .to_string(),
                args: vec![Value::Text(m)],
            }),
            None => Abort::Fault(anyhow!("constraint {name} violated")),
        },
        crate::db::DbError::ForeignKey => Abort::Thrown(Thrown {
            error: "BadRequest".into(),
            args: vec![Value::Text("referenced row does not exist".into())],
        }),
        crate::db::DbError::Other(e) => Abort::Fault(e),
    }
}

/// Rebuild a parsed record in projection order, recursing into arrays.
fn reorder(v: Value, order: &[String]) -> Value {
    match v {
        Value::Record(fields) => Value::Record(
            order
                .iter()
                .filter_map(|k| {
                    fields
                        .iter()
                        .find(|(n, _)| n == k)
                        .map(|(n, val)| (n.clone(), val.clone()))
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(|i| reorder(i, order)).collect()),
        other => other,
    }
}

pub(super) fn value_text(v: &Value) -> String {
    match v {
        Value::Text(s) | Value::Numeric(s) | Value::Timestamptz(s) | Value::Interval(s) => {
            s.clone()
        }
        Value::Int(n) | Value::Bigint(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => {
            let mut s = String::new();
            other.write_json(&mut s);
            s
        }
    }
}

fn placeholder(span: crate::token::Span) -> Expr {
    Expr::new(ExprKind::Null, span)
}

/// The last ordering tuple in a page's key column.
fn last_tuple(keys: &str) -> Option<Vec<Option<String>>> {
    let parsed: serde_json::Value = serde_json::from_str(keys).ok()?;
    let last = parsed.as_array()?.last()?;
    Some(
        last.as_array()?
            .iter()
            .map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
    )
}

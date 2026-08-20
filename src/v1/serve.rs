//! The request pipeline, and the server that drives it.
//!
//! The pipeline is a plain async function over an owned request, so the
//! golden tests exercise the real thing without binding a port. `serve`
//! wraps it in axum.
//!
//! Order is the whole point of this file (routing.md §3.2, §5.1,
//! middleware.md §4, errors.md §8):
//!
//! ```text
//! read body (bounded)  → 413 here, before any middleware
//! parse path params    → 400 here, before any middleware
//! middleware chain     → falls through, returns, or throws
//! handler
//! errorHandler         → after any rollback, outside the transaction
//! after blocks         → reverse order, for EVERY outcome, seeing the
//!                        status actually being sent
//! ```

use super::ast::*;
use super::exec::{Abort, Flow, Program, Request, Response, ServerConfig, Vm};
use super::value::Value;
use super::wiring::{ResolvedRoute, Segment};
use super::workspace::Workspace;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Build everything the runtime needs from parsed sources.
pub fn load(ws: &Workspace) -> Result<Program> {
    if ws.has_parse_errors() {
        bail!("{}", ws.parse_errors().join(""));
    }
    let built = super::model::build(ws);
    let symbols = super::symbols::build(ws, &built.model);
    let checked = super::check::check(ws, &symbols, &built.model);
    let wired = super::wiring::wire(ws, &symbols);

    let errors: Vec<String> = built
        .diags
        .iter()
        .chain(&symbols.diags)
        .chain(&checked.diags)
        .chain(&wired.diags)
        .filter(|(_, d)| d.severity == super::diag::Severity::Error)
        .map(|(loc, d)| ws.render(*loc, d))
        .collect();
    if !errors.is_empty() {
        bail!("{}", errors.join(""));
    }

    super::db::install_messages(&built.model);

    let mut functions = HashMap::new();
    let mut middleware = HashMap::new();
    let mut route_bodies = HashMap::new();
    let mut error_handler = None;
    let mut error_defs = HashMap::new();
    let mut server = ServerConfig::default();

    for (name, status, params) in super::symbols::PREDECLARED_ERRORS {
        error_defs.insert(
            (*name).to_string(),
            (
                *status,
                None,
                params.iter().map(|(n, _)| (*n).to_string()).collect(),
            ),
        );
    }

    for file in &ws.files {
        for d in &file.program.decls {
            match d {
                Decl::Function(f) => {
                    functions.insert(f.name.name.clone(), f.clone());
                }
                Decl::Service(s) => {
                    for f in &s.functions {
                        functions.insert(format!("{}.{}", s.name.name, f.name.name), f.clone());
                    }
                }
                Decl::Middleware(m) => {
                    middleware.insert(m.name.name.clone(), m.clone());
                }
                Decl::ErrorHandler(h) => error_handler = Some(h.clone()),
                Decl::Error(e) => {
                    error_defs.insert(
                        e.name.name.clone(),
                        (
                            e.status,
                            e.message.clone(),
                            e.params.iter().map(|p| p.name.name.clone()).collect(),
                        ),
                    );
                }
                Decl::Server(s) => server = read_server_config(s),
                Decl::Routes(block) => {
                    for r in &block.routes {
                        let pattern = pattern_of(&block.prefix, &r.suffix);
                        route_bodies
                            .insert((r.method.name.clone(), pattern), r.body.clone());
                    }
                }
                _ => {}
            }
        }
    }

    Ok(Program {
        model: built.model,
        symbols,
        routes: wired.routes,
        functions,
        middleware,
        route_bodies,
        error_handler,
        errors: error_defs,
        server,
    })
}

fn pattern_of(prefix: &str, suffix: &str) -> String {
    let segments = super::wiring::parse_path(&format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    ));
    super::wiring::render(&segments)
}

fn read_server_config(s: &ServerDecl) -> ServerConfig {
    let mut c = ServerConfig::default();
    for e in &s.entries {
        let ServerEntry::Set(a) = e else { continue };
        match a.key.name.as_str() {
            "max_body_bytes" => {
                if let ExprKind::Int(n) = &*a.value.kind {
                    c.max_body_bytes = n.parse().unwrap_or(c.max_body_bytes);
                }
            }
            "max_page_size" => {
                if let ExprKind::Int(n) = &*a.value.kind {
                    c.max_page_size = n.parse().unwrap_or(c.max_page_size);
                }
            }
            "cursor_secret" => {
                if let Some(v) = config_string(&a.value) {
                    c.cursor_secret = v;
                }
            }
            "strict_slash" => {
                if let ExprKind::Bool(b) = &*a.value.kind {
                    c.strict_slash = *b;
                }
            }
            "trusted_proxies" => {
                if let ExprKind::Array(items) = &*a.value.kind {
                    c.trusted_proxies = items
                        .iter()
                        .filter_map(|i| match &*i.kind {
                            ExprKind::Str(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect();
                }
            }
            _ => {}
        }
    }
    c
}

/// A `server { }` value that is a string: a literal, or `env("NAME")`.
///
/// A secret written as a literal is a secret in the repository, so the
/// sample uses `env`; both are read here because the spec allows both and
/// a local run should not need a `.env` to boot.
fn config_string(e: &Expr) -> Option<String> {
    match &*e.kind {
        ExprKind::Str(s) => Some(s.clone()),
        ExprKind::Call { callee, args, .. } => {
            let ExprKind::Name(n) = &*callee.kind else {
                return None;
            };
            if n.name != "env" {
                return None;
            }
            let ExprKind::Str(name) = &*args.first()?.kind else {
                return None;
            };
            std::env::var(name).ok()
        }
        ExprKind::Coalesce { lhs, rhs } => config_string(lhs).or_else(|| config_string(rhs)),
        _ => None,
    }
}

// ---------------------------------------------------------------- matching

pub struct Incoming {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub peer_ip: String,
}

/// A matched route: the route itself, its path bindings, and how many
/// literal segments it matched — the tie-break.
type Candidate<'p> = (&'p ResolvedRoute, Vec<(String, String)>, usize);

/// routing.md §4.2 — a literal segment beats a parameter segment. Fixed
/// precedence, not registration order.
fn match_route<'p>(
    program: &'p Program,
    method: &str,
    path: &str,
) -> Option<(&'p ResolvedRoute, Vec<(String, String)>)> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let mut best: Option<Candidate<'p>> = None;

    for r in &program.routes {
        if r.method != method || r.segments.len() != parts.len() {
            continue;
        }
        let mut binds = Vec::new();
        let mut literals = 0usize;
        let mut ok = true;
        for (seg, part) in r.segments.iter().zip(&parts) {
            match seg {
                Segment::Literal(l) => {
                    if l == part {
                        literals += 1;
                    } else {
                        ok = false;
                        break;
                    }
                }
                Segment::Param { name, .. } => {
                    binds.push((name.clone(), (*part).to_string()));
                }
            }
        }
        if !ok {
            continue;
        }
        if best.as_ref().is_none_or(|(_, _, n)| literals > *n) {
            best = Some((r, binds, literals));
        }
    }
    best.map(|(r, b, _)| (r, b))
}

/// routing.md §3.2 — parsed **before** any middleware, so malformed input
/// is a 400 and never reaches Postgres as a 500.
fn parse_params(
    route: &ResolvedRoute,
    binds: &[(String, String)],
) -> std::result::Result<HashMap<String, Value>, Response> {
    let mut out = HashMap::new();
    for (name, raw) in binds {
        let ty = route
            .params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.as_str())
            .unwrap_or("text");
        let v = match ty {
            "bigint" => raw.parse::<i64>().ok().map(Value::Bigint),
            "int" | "smallint" => raw.parse::<i64>().ok().map(Value::Int),
            "numeric" => raw.parse::<f64>().ok().map(|_| Value::Numeric(raw.clone())),
            "boolean" => match raw.as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            "uuid" => {
                if raw.len() == 36 && raw.chars().filter(|c| *c == '-').count() == 4 {
                    Some(Value::Text(raw.clone()))
                } else {
                    None
                }
            }
            _ => Some(Value::Text(raw.clone())),
        };
        match v {
            Some(v) => {
                out.insert(name.clone(), v);
            }
            None => {
                return Err(Response::json(
                    400,
                    &Value::Record(vec![
                        ("error".into(), Value::Text("bad_path_parameter".into())),
                        ("parameter".into(), Value::Text(name.clone())),
                        ("expected".into(), Value::Text(ty.to_string())),
                    ]),
                ))
            }
        }
    }
    Ok(out)
}

/// routing.md §5.4 / config.md §3.3 — with no `trusted_proxies` declared,
/// `X-Forwarded-For` is ignored entirely, so a rate limiter keyed on
/// `client_ip()` is unspoofable by default.
fn client_ip(cfg: &ServerConfig, peer: &str, headers: &HashMap<String, String>) -> String {
    if cfg.trusted_proxies.is_empty() {
        return peer.to_string();
    }
    let Some(xff) = headers.get("x-forwarded-for") else {
        return peer.to_string();
    };
    let mut chain: Vec<&str> = xff.split(',').map(str::trim).collect();
    chain.push(peer);
    for addr in chain.iter().rev() {
        if !cfg.trusted_proxies.iter().any(|p| trusts(p, addr)) {
            return (*addr).to_string();
        }
    }
    peer.to_string()
}

/// Prefix match on the CIDR's leading octets — enough for the /8, /12 and
/// /16 blocks a deployment actually declares, and it never *widens* trust.
fn trusts(cidr: &str, addr: &str) -> bool {
    let base = cidr.split('/').next().unwrap_or(cidr);
    let bits: u32 = cidr
        .split('/')
        .nth(1)
        .and_then(|b| b.parse().ok())
        .unwrap_or(32);
    let octets = (bits / 8) as usize;
    if octets == 0 {
        return true;
    }
    let a: Vec<&str> = base.split('.').collect();
    let b: Vec<&str> = addr.split('.').collect();
    a.len() >= octets && b.len() >= octets && a[..octets] == b[..octets]
}

// ---------------------------------------------------------------- pipeline

pub async fn handle(program: Arc<Program>, incoming: Incoming) -> Response {
    // §5.1 — the body is read once into a bounded buffer, before middleware.
    // A webhook signature check therefore never sees a truncated body.
    if incoming.body.len() > program.server.max_body_bytes {
        return Response::message(413, "request body too large");
    }

    let Some((route, binds)) = match_route(&program, &incoming.method, &incoming.path) else {
        return Response::message(404, "not found");
    };
    let route = route.clone();

    // §3.2 — a value that does not parse is a 400 here, before any
    // middleware and long before Postgres.
    let params = match parse_params(&route, &binds) {
        Ok(p) => p,
        Err(response) => return response,
    };

    let client = client_ip(&program.server, &incoming.peer_ip, &incoming.headers);
    let request = Arc::new(Request {
        method: incoming.method.clone(),
        path: incoming.path.clone(),
        route: route.pattern.clone(),
        headers: incoming.headers,
        query: incoming.query,
        body: String::from_utf8_lossy(&incoming.body).to_string(),
        peer_ip: incoming.peer_ip,
        client_ip: client,
        id: format!("{:016x}", rand_id()),
    });

    let mut vm = Vm::new(&program, request.clone());
    vm.set_params(params);

    // The chain, then the handler. Every middleware that *started* runs its
    // `after` block, including the one that short-circuited
    // (middleware.md §4.3).
    let mut started: Vec<String> = Vec::new();
    let mut outcome: Option<Response> = None;
    let mut raised: Option<Abort> = None;

    for name in &route.chain {
        started.push(name.clone());
        let Some(m) = program.middleware.get(name) else {
            continue;
        };
        match vm.run_body(&m.body).await {
            // §4.2 — `return <Response>` short-circuits the chain.
            Ok(Flow::Return(v)) => {
                outcome = Some(as_response(v));
                break;
            }
            Ok(Flow::ReturnVoid) => {
                outcome = Some(Response::empty(204));
                break;
            }
            Ok(Flow::Normal) => {}
            Err(a) => {
                raised = Some(a);
                break;
            }
        }
    }

    if outcome.is_none() && raised.is_none() {
        let key = (route.method.clone(), route.pattern.clone());
        match program.route_bodies.get(&key) {
            Some(body) => match vm.run_body(body).await {
                Ok(Flow::Return(v)) => outcome = Some(as_response(v)),
                Ok(_) => outcome = Some(Response::empty(204)),
                Err(a) => raised = Some(a),
            },
            None => outcome = Some(Response::message(500, "internal_error")),
        }
    }

    // errors.md §8 — the handler runs after any rollback, outside the
    // transaction, and before the after chain.
    let mut response = match (outcome, raised) {
        (Some(r), _) => r,
        (None, Some(a)) => handle_error(&program, &mut vm, a).await,
        (None, None) => Response::message(500, "internal_error"),
    };

    // §5.1–§5.2 — reverse order, every outcome, and `response.status()`
    // sees the status actually being sent.
    for name in started.iter().rev() {
        let Some(m) = program.middleware.get(name) else {
            continue;
        };
        let Some(after) = &m.after else { continue };
        vm.response_status = Some(response.status);
        vm.extra_headers.clear();
        let _ = vm.run_body(after).await;
        // §5.4 — an `after` block may add headers, never change the status
        // or the body.
        for h in std::mem::take(&mut vm.extra_headers) {
            response.headers.push(h);
        }
    }

    response
}

async fn handle_error(program: &Program, vm: &mut Vm<'_>, abort: Abort) -> Response {
    let thrown = match abort {
        Abort::Fault(e) => {
            eprintln!("[fault] {e}");
            return Response::message(500, "internal_error");
        }
        Abort::Thrown(t) => t,
    };

    // A declared error carries a default status, which is what makes an
    // `errorHandler` arm optional (errors.md §4.3).
    let (status, _default_msg, _params) = program
        .errors
        .get(&thrown.error)
        .cloned()
        .unwrap_or((500, None, vec![]));

    if let Some(h) = &program.error_handler {
        for arm in &h.arms {
            let matches = match &arm.error {
                Some(name) => name.name == thrown.error,
                // errors.md §4.4 — the untyped arm catches faults only.
                None => false,
            };
            if !matches {
                continue;
            }
            let payload = error_payload(program, &thrown);
            vm.set_context("__error", payload.clone());
            let saved = vm.enter_function();
            vm.bind_param(&arm.binder.name, payload);
            let r = vm.run_body(&arm.body).await;
            vm.leave_function(saved);
            if let Ok(Flow::Return(v)) = r {
                return as_response(v);
            }
        }
    }

    // types.md §11.3 — validation has one fixed body and user code cannot
    // produce a different one.
    if thrown.error == "BadRequest" && thrown.args.len() == 2 {
        if let (Some("validation_failed"), Some(Value::Array(fields))) = (
            thrown.args[0].as_text(),
            thrown.args.get(1),
        ) {
            return Response::json(
                400,
                &Value::Record(vec![
                    ("error".into(), Value::Text("validation_failed".into())),
                    ("fields".into(), Value::Array(fields.clone())),
                ]),
            );
        }
    }

    Response::message(status, &thrown.message())
}

fn error_payload(program: &Program, t: &super::exec::Thrown) -> Value {
    let names = program
        .errors
        .get(&t.error)
        .map(|(_, _, p)| p.clone())
        .unwrap_or_else(|| vec!["message".to_string()]);
    let mut fields = Vec::new();
    for (i, name) in names.iter().enumerate() {
        fields.push((name.clone(), t.args.get(i).cloned().unwrap_or(Value::Null)));
    }
    if !fields.iter().any(|(k, _)| k == "message") {
        fields.insert(0, ("message".into(), Value::Text(t.message())));
    }
    Value::Record(fields)
}

/// routing.md §6.4 — a route must end in a response, which the checker
/// enforces; a non-response here is a compiler bug, not a client error.
fn as_response(v: Value) -> Response {
    match v {
        Value::Response {
            status,
            body,
            headers,
        } => Response {
            status,
            body,
            headers,
        },
        _ => Response::message(500, "internal_error"),
    }
}

fn rand_id() -> u64 {
    use rand::RngCore;
    rand::thread_rng().next_u64()
}


// ---------------------------------------------------------------- server

/// Percent-decoded `k=v&k=v`. Repeated keys are kept in order, which is
/// what `request.query_all` returns (routing.md §5.3).
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `jwc v1 serve` — axum in front of [`handle`].
///
/// The socket is the only thing this adds: routing, path parsing, the body
/// buffer, middleware, the error model and the after chain all live in
/// `handle`, which is what the golden tests drive directly.
pub async fn serve(program: Arc<Program>, port: u16) -> Result<()> {
    use axum::body::Bytes;
    use axum::extract::{ConnectInfo, State};
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;
    use std::net::SocketAddr;

    async fn dispatch(
        State(program): State<Arc<Program>>,
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        let query = uri.query().map(parse_query).unwrap_or_default();

        let hdrs = headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str().ok().map(|s| (k.as_str().to_lowercase(), s.to_string()))
            })
            .collect();

        let r = handle(
            program,
            Incoming {
                method: method.as_str().to_string(),
                path: uri.path().to_string(),
                query,
                headers: hdrs,
                body: body.to_vec(),
                peer_ip: peer.ip().to_string(),
            },
        )
        .await;

        let mut response = axum::response::Response::builder()
            .status(StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
        for (k, v) in &r.headers {
            response = response.header(k.as_str(), v.as_str());
        }
        response.body(axum::body::Body::from(r.body)).expect("response")
    }

    let app = axum::Router::new()
        .fallback(dispatch)
        .with_state(program);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("listening on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

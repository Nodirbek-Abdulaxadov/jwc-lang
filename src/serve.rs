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

use crate::ast::*;
use crate::exec::{Abort, Flow, Program, Request, Response, ServerConfig, Vm};
use crate::value::Value;
use crate::wiring::{ResolvedRoute, Segment};
use crate::workspace::Workspace;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Build everything the runtime needs from parsed sources.
pub fn load(ws: &Workspace) -> Result<Program> {
    if ws.has_parse_errors() {
        bail!("{}", ws.parse_errors().join(""));
    }
    let built = crate::model::build(ws);
    let symbols = crate::symbols::build(ws, &built.model);
    let checked = crate::check::check(ws, &symbols, &built.model);
    let wired = crate::wiring::wire(ws, &symbols);

    let errors: Vec<String> = built
        .diags
        .iter()
        .chain(&symbols.diags)
        .chain(&checked.diags)
        .chain(&wired.diags)
        .filter(|(_, d)| d.severity == crate::diag::Severity::Error)
        .map(|(loc, d)| ws.render(*loc, d))
        .collect();
    if !errors.is_empty() {
        bail!("{}", errors.join(""));
    }

    crate::db::install_messages(&built.model);

    let mut functions = HashMap::new();
    let mut middleware = HashMap::new();
    let mut route_bodies = HashMap::new();
    let mut error_handler = None;
    let mut error_defs = HashMap::new();
    let mut server = ServerConfig::default();

    for (name, status, params) in crate::symbols::PREDECLARED_ERRORS {
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
                        route_bodies.insert((r.method.name.clone(), pattern), r.body.clone());
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
    let segments = crate::wiring::parse_path(&format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    ));
    crate::wiring::render(&segments)
}

pub(crate) fn read_server_config(s: &ServerDecl) -> ServerConfig {
    let mut c = ServerConfig::default();
    for e in &s.entries {
        let a = match e {
            ServerEntry::Set(a) => a,
            ServerEntry::Group { name, entries, .. } => {
                match name.name.as_str() {
                    "cors" => c.cors = Some(read_cors(entries)),
                    "tls" => {
                        c.tls_declared = true;
                        c.tls = read_tls(entries);
                    }
                    _ => {}
                }
                continue;
            }
        };
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
            "bind" => {
                if let Some(v) = config_string(&a.value) {
                    c.bind = v;
                }
            }
            "strict_slash" => {
                if let ExprKind::Bool(b) = &*a.value.kind {
                    c.strict_slash = *b;
                }
            }
            "request_timeout" => {
                if let Some(d) = config_duration(&a.value) {
                    c.request_timeout = d;
                }
            }
            "shutdown_grace" => {
                if let Some(d) = config_duration(&a.value) {
                    c.shutdown_grace = d;
                }
            }
            "header_timeout" => {
                if let Some(d) = config_duration(&a.value) {
                    c.header_timeout = d;
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

/// `tls { cert = …; key = …; }` (config.md §3.5).
///
/// A block missing either half yields `None`, which reads as "no TLS" —
/// and that is deliberately not silent: `serve` refuses to boot on a
/// `tls { }` it could not resolve, because falling back to plain HTTP
/// under a block that says otherwise is the one misconfiguration an
/// operator cannot see for themselves.
fn read_tls(entries: &[crate::ast::Assignment]) -> Option<crate::exec::TlsConfig> {
    let mut cert = None;
    let mut key = None;
    for a in entries {
        match a.key.name.as_str() {
            "cert" => cert = config_string(&a.value),
            "key" => key = config_string(&a.value),
            _ => {}
        }
    }
    Some(crate::exec::TlsConfig {
        cert: cert?,
        key: key?,
    })
}

fn read_cors(entries: &[crate::ast::Assignment]) -> crate::exec::CorsConfig {
    let mut cors = crate::exec::CorsConfig::default();
    for a in entries {
        match a.key.name.as_str() {
            "origins" => cors.origins = string_array(&a.value),
            "methods" => cors.methods = string_array(&a.value),
            "headers" => cors.headers = string_array(&a.value),
            "credentials" => {
                if let ExprKind::Bool(b) = &*a.value.kind {
                    cors.credentials = *b;
                }
            }
            "max_age" => cors.max_age = config_duration(&a.value),
            _ => {}
        }
    }
    cors
}

fn string_array(e: &Expr) -> Vec<String> {
    match &*e.kind {
        ExprKind::Array(items) => items
            .iter()
            .filter_map(|i| match &*i.kind {
                ExprKind::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `"30s"`, `"600ms"`, `"5m"`, `"1h"` (config.md §3.2).
///
/// A bare number is refused rather than read as seconds: `request_timeout =
/// 30` and `= "30s"` would then mean the same thing and `= 30000` would
/// silently mean eight hours.
pub fn parse_duration(text: &str) -> Option<std::time::Duration> {
    let t = text.trim();
    let i = t.find(|c: char| c.is_ascii_alphabetic())?;
    let (value, unit) = (&t[..i], &t[i..]);
    let n: u64 = value.parse().ok()?;
    let d = match unit {
        "ms" => std::time::Duration::from_millis(n),
        "s" => std::time::Duration::from_secs(n),
        "m" => std::time::Duration::from_secs(n * 60),
        "h" => std::time::Duration::from_secs(n * 3600),
        _ => return None,
    };
    Some(d)
}

fn config_duration(e: &Expr) -> Option<std::time::Duration> {
    config_string(e).as_deref().and_then(parse_duration)
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
    let parts: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
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
    // A webhook signature check therefore never sees a truncated body, and
    // an oversized one is refused **here**, ahead of the chain: middleware
    // that has already run a rate-limit or a signature check on a body the
    // server was going to reject anyway is work an attacker chose.
    let origin = incoming.headers.get("origin").cloned();
    if incoming.body.len() > program.server.max_body_bytes {
        return with_cors(
            &program,
            origin.as_deref(),
            Response::message(413, "request body too large"),
        );
    }

    // config.md §3.4 — a preflight is answered before routing, because
    // `OPTIONS` reaches no handler and the browser is asking about the
    // route, not calling it.
    if incoming.method.eq_ignore_ascii_case("OPTIONS") && program.server.cors.is_some() {
        return with_cors(&program, origin.as_deref(), Response::empty(204));
    }
    let answer = handle_inner(program.clone(), incoming).await;
    with_cors(&program, origin.as_deref(), answer)
}

/// `/healthz`, `/readyz`, `/metrics` — config.md §4.
///
/// These are not declarable in the vocabulary and they should not be: an
/// operator needs them at a fixed path before reading anyone's source, and
/// a deployment whose liveness probe depends on the application having
/// remembered to write one is a deployment that restarts for the wrong
/// reasons. The 0.9.x runtime served all three; v1 lost them at the
/// cutover, which is also why the soak's "zero pool leaks" criterion had
/// nothing to read.
async fn operational(program: &Program, incoming: &Incoming) -> Option<Response> {
    if !incoming.method.eq_ignore_ascii_case("GET") {
        return None;
    }
    match incoming.path.trim_end_matches('/') {
        // Liveness. Touches nothing: a process that answers this is one
        // the supervisor should not kill. Wiring a dependency in here is
        // the classic way to turn a database blip into a restart storm.
        "/healthz" => Some(Response::json(
            200,
            &Value::Record(vec![("status".into(), Value::Text("ok".into()))]),
        )),

        // Readiness. Every configured dependency, actually round-tripped.
        // Redis only when it is configured — an existing deployment that
        // never set `JWC_REDIS_URL` must not start failing its probe
        // because the runtime grew a Redis driver.
        "/readyz" => {
            let mut failed: Vec<&str> = Vec::new();
            if crate::engine::pool_status().is_none() {
                failed.push("db_uninitialised");
            } else if crate::engine::ping().await.is_err() {
                failed.push("db_unreachable");
            }
            if crate::redis_engine::is_enabled() && crate::redis_engine::ping().await.is_err() {
                failed.push("redis_unreachable");
            }
            Some(if failed.is_empty() {
                Response::json(
                    200,
                    &Value::Record(vec![("status".into(), Value::Text("ready".into()))]),
                )
            } else {
                // 503, and the body names which dependency. A probe that
                // only says "not ready" sends the operator to the logs of
                // a pod that is already out of rotation.
                Response::json(
                    503,
                    &Value::Record(vec![
                        ("status".into(), Value::Text("unready".into())),
                        (
                            "failed".into(),
                            Value::Array(
                                failed
                                    .iter()
                                    .map(|f| Value::Text((*f).to_string()))
                                    .collect(),
                            ),
                        ),
                    ]),
                )
            })
        }

        "/metrics" => Some(Response {
            status: 200,
            headers: vec![(
                "content-type".into(),
                "text/plain; version=0.0.4; charset=utf-8".into(),
            )],
            body: metrics_text(program),
        }),
        _ => None,
    }
}

/// Prometheus text format. Gauges only — a counter would need per-request
/// bookkeeping on the hot path, and what the soak criterion asks about is
/// the pool.
fn metrics_text(program: &Program) -> String {
    let mut out = String::new();
    if let Some(s) = crate::engine::pool_status() {
        out.push_str(
            "# HELP jwc_db_pool_size Connections the pool currently holds.\n\
             # TYPE jwc_db_pool_size gauge\n",
        );
        out.push_str(&format!("jwc_db_pool_size {}\n", s.size));
        out.push_str(
            "# HELP jwc_db_pool_available Connections idle and checkout-ready.\n\
             # TYPE jwc_db_pool_available gauge\n",
        );
        out.push_str(&format!("jwc_db_pool_available {}\n", s.available));
        out.push_str(
            "# HELP jwc_db_pool_max_size Ceiling from JWC_DB_POOL_SIZE.\n\
             # TYPE jwc_db_pool_max_size gauge\n",
        );
        out.push_str(&format!("jwc_db_pool_max_size {}\n", s.max_size));
        // The leak signal. A pool that never returns a connection shows up
        // here as `available` pinned at 0 while `waiting` climbs, which is
        // exactly the shape `soak/analyze.py` is looking for.
        out.push_str(
            "# HELP jwc_db_pool_waiting Tasks blocked waiting for a connection.\n\
             # TYPE jwc_db_pool_waiting gauge\n",
        );
        out.push_str(&format!("jwc_db_pool_waiting {}\n", s.waiting));
    }
    if let Some(s) = crate::redis_engine::pool_status() {
        out.push_str(
            "# HELP jwc_redis_pool_size Connections the Redis pool holds.\n\
             # TYPE jwc_redis_pool_size gauge\n",
        );
        out.push_str(&format!("jwc_redis_pool_size {}\n", s.size));
        out.push_str(
            "# HELP jwc_redis_pool_available Idle Redis connections.\n\
             # TYPE jwc_redis_pool_available gauge\n",
        );
        out.push_str(&format!("jwc_redis_pool_available {}\n", s.available));
        out.push_str(
            "# HELP jwc_redis_pool_max_size Ceiling from JWC_REDIS_POOL_SIZE.\n\
             # TYPE jwc_redis_pool_max_size gauge\n",
        );
        out.push_str(&format!("jwc_redis_pool_max_size {}\n", s.max_size));
        out.push_str(
            "# HELP jwc_redis_pool_waiting Tasks blocked on a Redis connection.\n\
             # TYPE jwc_redis_pool_waiting gauge\n",
        );
        out.push_str(&format!("jwc_redis_pool_waiting {}\n", s.waiting));
    }
    out.push_str(
        "# HELP jwc_routes Declared routes.\n\
         # TYPE jwc_routes gauge\n",
    );
    out.push_str(&format!("jwc_routes {}\n", program.routes.len()));
    out
}

/// The CORS headers for a request, when a `cors { }` block is declared.
///
/// Absent, nothing is added at all: a browser refusing a cross-origin call
/// is the correct default, and a header emitted "just in case" is a policy
/// nobody wrote.
fn with_cors(program: &Program, origin: Option<&str>, mut r: Response) -> Response {
    let Some(cors) = &program.server.cors else {
        return r;
    };
    let Some(origin) = origin else {
        return r;
    };
    let Some(allow) = cors.allow(origin) else {
        return r;
    };
    r.headers
        .push(("access-control-allow-origin".into(), allow));
    // Any cache in front of this has to key on the origin, or one caller's
    // answer is served to another's.
    r.headers.push(("vary".into(), "Origin".into()));
    if cors.credentials {
        r.headers
            .push(("access-control-allow-credentials".into(), "true".into()));
    }
    if !cors.methods.is_empty() {
        r.headers.push((
            "access-control-allow-methods".into(),
            cors.methods.join(", "),
        ));
    }
    if !cors.headers.is_empty() {
        r.headers.push((
            "access-control-allow-headers".into(),
            cors.headers.join(", "),
        ));
    }
    if let Some(age) = cors.max_age {
        r.headers
            .push(("access-control-max-age".into(), age.as_secs().to_string()));
    }
    r
}

async fn handle_inner(program: Arc<Program>, incoming: Incoming) -> Response {
    // Started before the chain, so `response.duration_*()` reports the
    // whole request — middleware included — and not just the handler.
    let started_at = std::time::Instant::now();
    let matched = match_route(&program, &incoming.method, &incoming.path);

    // config.md §4.0.3 — a declared route wins, and "declared" means the
    // path was **written down**. A program that writes its own `/metrics`
    // keeps it; a wildcard that happens to span the name does not.
    //
    // The difference is not academic. jwc-shortener declares `/{code}` for
    // its redirects, which matches one segment and therefore matched
    // `/readyz` too — so the readiness probe answered 404 with the
    // shortener's "no such link", every pod stayed out of rotation, and
    // nothing in the source mentioned `/readyz` for an operator to find.
    // §4.0.2 promises these three are reachable before reading anyone's
    // source; a pattern nobody aimed at them must not take that away.
    let shadows_operational = matched.as_ref().is_some_and(|(_, binds)| !binds.is_empty());
    if shadows_operational {
        if let Some(r) = operational(&program, &incoming).await {
            return r;
        }
    }

    let Some((route, binds)) = matched else {
        // Only once nothing declared matched.
        if let Some(r) = operational(&program, &incoming).await {
            return r;
        }
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
            // A middleware body is not a loop; the checker rejects both
            // before they reach here (E0813).
            Ok(Flow::Break) | Ok(Flow::Continue) => {}
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
        vm.response_micros = Some(started_at.elapsed().as_micros() as u64);
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
    let (status, _default_msg, _params) =
        program
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
        if let (Some("validation_failed"), Some(Value::Array(fields))) =
            (thrown.args[0].as_text(), thrown.args.get(1))
        {
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

fn error_payload(program: &Program, t: &crate::exec::Thrown) -> Value {
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
/// The port `main()`'s `serve(...)` asked for.
///
/// The call had never been evaluated: `main` was parsed, type-checked for
/// arity, and then ignored, so the listener took the CLI default and a
/// program asking for 3000 silently got 8080. `serve(int(env("PORT") ??
/// "8080"))` — the form the spec's own sample uses — could not work at all.
///
/// `main` is an ordinary body, so it runs on an ordinary Vm. A program with
/// no `main`, or one whose `main` never reaches `serve`, keeps 8080.
pub async fn declared_port(program: &Arc<Program>) -> Result<u16> {
    const FALLBACK: u16 = 8080;
    let Some(main) = program.functions.get("main") else {
        return Ok(FALLBACK);
    };

    // `main` runs before any request exists. The synthetic one carries the
    // shape the Vm needs and nothing a handler would read.
    let request = Arc::new(crate::exec::Request {
        method: "BOOT".into(),
        path: "/".into(),
        route: "/".into(),
        headers: Default::default(),
        query: Vec::new(),
        body: String::new(),
        peer_ip: "127.0.0.1".into(),
        client_ip: "127.0.0.1".into(),
        id: "boot".into(),
    });
    let mut vm = crate::exec::Vm::new(program, request);
    // A `main` that raises is a boot failure and says so, rather than
    // listening on a port nobody asked for.
    vm.run_block(&main.body).await.map_err(|e| match e {
        crate::exec::Abort::Thrown(t) => {
            anyhow!("main() raised {} at boot: {}", t.error, t.message())
        }
        crate::exec::Abort::Fault(f) => anyhow!("main() failed at boot: {f}"),
    })?;
    Ok(vm.serve_port.unwrap_or(FALLBACK))
}

pub async fn serve(program: Arc<Program>, port: u16) -> Result<()> {
    // A `tls { }` whose `cert`/`key` did not resolve — an unset
    // `env("TLS_CERT_PATH")`, most often — must stop the boot. Reading it
    // as "no TLS" would serve every byte in the clear under a block that
    // says otherwise, which is the one misconfiguration an operator
    // cannot see for themselves: the listener answers either way.
    if program.server.tls_declared && program.server.tls.is_none() {
        bail!(
            "`server {{ tls {{ … }} }}` is declared but `cert` and `key` did not both \
             resolve to a path. Set them, or remove the block — serving plain HTTP \
             under it would be invisible from outside."
        );
    }
    let grace_period = program.server.shutdown_grace;
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
                v.to_str()
                    .ok()
                    .map(|s| (k.as_str().to_lowercase(), s.to_string()))
            })
            .collect();

        let limit = program.server.request_timeout;
        let r = match tokio::time::timeout(
            limit,
            handle(
                program,
                Incoming {
                    method: method.as_str().to_string(),
                    path: uri.path().to_string(),
                    query,
                    headers: hdrs,
                    body: body.to_vec(),
                    peer_ip: peer.ip().to_string(),
                },
            ),
        )
        .await
        {
            Ok(r) => r,
            // config.md §3.2 — the handler's task is dropped here, which
            // releases whatever it was holding. A request that has already
            // lost its client is a connection and a pool slot nobody is
            // waiting on.
            Err(_) => Response::message(504, "request timed out"),
        };

        let mut response = axum::response::Response::builder()
            .status(StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
        for (k, v) in &r.headers {
            response = response.header(k.as_str(), v.as_str());
        }
        response
            .body(axum::body::Body::from(r.body))
            .expect("response")
    }

    let header_timeout = program.server.header_timeout;
    let tls = program.server.tls.clone();
    let bind = program.server.bind.clone();

    let app = axum::Router::new().fallback(dispatch).with_state(program);

    // Resolved before the socket opens. A certificate that is missing or
    // malformed is a boot failure, not a first-request failure: the
    // second would leave a listener up and answering nothing.
    let acceptor = match &tls {
        Some(t) => Some(tls_acceptor(t)?),
        None => None,
    };

    // Parsed rather than defaulted on failure: `bind = "127.0.0..1"` must
    // not quietly become `0.0.0.0`, which is the opposite of what the typo
    // was reaching for and would put the listener on every interface.
    let ip: std::net::IpAddr = bind
        .parse()
        .map_err(|_| anyhow!("`server {{ bind }}` is not an IP address: {bind}"))?;
    let addr = std::net::SocketAddr::new(ip, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let scheme = if acceptor.is_some() { "https" } else { "http" };
    println!("listening on {scheme}://{addr}");

    // The accept loop `axum::serve` would otherwise own. It is written out
    // here because both of config.md §3's remaining promises live below
    // that wrapper: `header_read_timeout` is on hyper's builder, and TLS
    // means wrapping the `TcpStream` before hyper ever sees it.
    let mut make = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    // config.md §3.2 — the request line and headers must arrive inside
    // this window. `request_timeout` cannot cover it: that timer starts in
    // `handle`, and a client dribbling headers a byte at a time never gets
    // there. HTTP/2 has its own frame-level limits and takes no equivalent.
    //
    // `timer` is not optional here. hyper carries no clock of its own, and
    // a `header_read_timeout` with no timer installed panics the worker on
    // *every* HTTP/1 connection — which no unit test sees, because the
    // panic is inside hyper's poll and not in anything this crate calls
    // directly. `tests/serve_listener.rs` drives a real socket for it.
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(header_timeout);
    let builder = Arc::new(builder);
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown_signal());

    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(v) => v,
                // One failed accept is a per-connection condition (the
                // peer went away mid-handshake, or the process is at its
                // descriptor ceiling). Tearing the listener down over it
                // would turn a transient into an outage.
                Err(e) => {
                    eprintln!("accept failed: {e}");
                    continue;
                }
            },
            _ = shutdown.as_mut() => break,
        };

        let svc = <_ as tower::Service<std::net::SocketAddr>>::call(&mut make, peer).await;
        let svc = match svc {
            Ok(s) => s,
            Err(e) => {
                eprintln!("service setup failed: {e}");
                continue;
            }
        };
        let svc = hyper_util::service::TowerToHyperService::new(svc);
        let builder = builder.clone();
        let acceptor = acceptor.clone();
        // An owned watcher, so the drain below waits for this connection's
        // in-flight request rather than cutting it mid-response. It has to
        // be owned: the TLS handshake happens inside the task, so the
        // connection this watches does not exist yet at this point.
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            match acceptor {
                Some(a) => {
                    // The handshake is inside the spawned task on purpose:
                    // doing it in the accept loop would let one slow or
                    // hostile peer stall every other connection behind it.
                    let stream = match a.accept(stream).await {
                        Ok(s) => s,
                        // Routine: port scanners, health checks that speak
                        // plain HTTP, clients with no shared cipher. Not
                        // worth a line each at default verbosity.
                        Err(_) => return,
                    };
                    let conn = builder
                        .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), svc);
                    let _ = watcher.watch(conn).await;
                }
                None => {
                    let conn = builder
                        .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), svc);
                    let _ = watcher.watch(conn).await;
                }
            }
        });
    }

    println!("draining for {}s", grace_period.as_secs());
    // config.md §3.8 — in-flight requests get the window to finish; past
    // it the remaining connections are dropped rather than held open for a
    // client that may never send another byte.
    tokio::select! {
        _ = graceful.shutdown() => {}
        _ = tokio::time::sleep(grace_period) => {
            eprintln!("drain window elapsed with connections still open");
        }
    }
    Ok(())
}

/// Build the TLS acceptor from a `tls { }` block.
///
/// PEM parsing comes from `rustls-pki-types` rather than `rustls-pemfile`:
/// the latter carries an open advisory and reaches this tree only as a
/// dev-dependency of `testcontainers`, which `tests/hardening.rs` asserts
/// against the real dependency graph.
fn tls_acceptor(t: &crate::exec::TlsConfig) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls_pki_types::pem::PemObject;

    let certs = rustls_pki_types::CertificateDer::pem_file_iter(&t.cert)
        .map_err(|e| anyhow!("`server {{ tls {{ cert }} }}` {}: {e}", t.cert))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("parsing certificates from {}: {e}", t.cert))?;
    if certs.is_empty() {
        bail!("{} holds no certificate", t.cert);
    }
    let key = rustls_pki_types::PrivateKeyDer::from_pem_file(&t.key)
        .map_err(|e| anyhow!("`server {{ tls {{ key }} }}` {}: {e}", t.key))?;

    let mut config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow!("the certificate and key do not go together: {e}"))?;
    // Advertise HTTP/2 as well as HTTP/1.1. Without this every client
    // negotiates 1.1 over TLS, which is a silent downgrade from what the
    // plain-HTTP listener already serves.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// SIGTERM, or Ctrl-C. A deploy sends the first and a developer the second,
/// and a server that only handles one of them drops in-flight requests on
/// whichever it missed.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

#[cfg(test)]
mod route_matching {
    use super::*;
    use crate::exec::ServerConfig;

    fn route(method: &str, pattern: &str) -> ResolvedRoute {
        ResolvedRoute {
            method: method.to_string(),
            pattern: pattern.to_string(),
            segments: crate::wiring::parse_path(pattern),
            params: Vec::new(),
            chain: Vec::new(),
            after: Vec::new(),
            loc: crate::workspace::Loc {
                file: 0,
                span: crate::token::Span { start: 0, end: 0 },
            },
        }
    }

    fn program(routes: Vec<ResolvedRoute>) -> Program {
        Program {
            model: crate::model::SchemaModel {
                database: None,
                schemas: Vec::new(),
                enums: Vec::new(),
                tables: Vec::new(),
                views: Vec::new(),
                scheme: crate::naming::SCHEME_VERSION,
            },
            symbols: Default::default(),
            routes,
            functions: HashMap::new(),
            middleware: HashMap::new(),
            route_bodies: HashMap::new(),
            error_handler: None,
            errors: HashMap::new(),
            server: ServerConfig::default(),
        }
    }

    /// routing.md §4.3 — the rule that makes `E0711` unreachable.
    ///
    /// A literal is never shadowed by a parameter, **in either declaration
    /// order**, because the router scores candidates by how many literal
    /// segments they match. If this ever becomes first-match-wins, a
    /// shadowing check has to come back.
    #[test]
    fn a_literal_beats_a_parameter_in_either_order() {
        for routes in [
            vec![route("GET", "/orgs/new"), route("GET", "/orgs/{id}")],
            vec![route("GET", "/orgs/{id}"), route("GET", "/orgs/new")],
        ] {
            let p = program(routes);
            let (r, binds) = match_route(&p, "GET", "/orgs/new").expect("a match");
            assert_eq!(r.pattern, "/orgs/new", "the literal must win");
            assert!(binds.is_empty());

            let (r, binds) = match_route(&p, "GET", "/orgs/7").expect("a match");
            assert_eq!(r.pattern, "/orgs/{id}");
            assert_eq!(binds, vec![("id".to_string(), "7".to_string())]);
        }
    }

    #[test]
    fn a_longer_literal_prefix_wins() {
        let p = program(vec![
            route("GET", "/orgs/{id}/members"),
            route("GET", "/orgs/{id}/{rest}"),
        ]);
        let (r, _) = match_route(&p, "GET", "/orgs/7/members").expect("a match");
        assert_eq!(r.pattern, "/orgs/{id}/members");
    }

    #[test]
    fn the_method_is_part_of_the_match() {
        let p = program(vec![route("GET", "/orgs"), route("POST", "/orgs")]);
        assert_eq!(match_route(&p, "POST", "/orgs").unwrap().0.pattern, "/orgs");
        assert_eq!(match_route(&p, "POST", "/orgs").unwrap().0.method, "POST");
        assert!(match_route(&p, "PUT", "/orgs").is_none());
    }
}

#[cfg(test)]
mod client_ip_tests {
    use super::*;
    use crate::exec::ServerConfig;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.to_string()))
            .collect()
    }

    #[test]
    fn with_no_trusted_proxies_the_header_is_ignored_entirely() {
        // config.md §3.3 — this is what makes a rate limiter keyed on
        // `client_ip()` unspoofable by default. A caller who can set a
        // header could otherwise mint a fresh bucket per request and the
        // limit would never bind.
        let cfg = ServerConfig::default();
        assert!(cfg.trusted_proxies.is_empty());
        for spoof in [
            "1.2.3.4",
            "9.9.9.9, 8.8.8.8",
            "  10.0.0.1  ",
            "not-an-address",
            "",
        ] {
            assert_eq!(
                client_ip(&cfg, "203.0.113.7", &headers(&[("X-Forwarded-For", spoof)])),
                "203.0.113.7",
                "`X-Forwarded-For: {spoof}` changed the answer"
            );
        }
    }

    #[test]
    fn a_trusted_proxy_is_peeled_and_an_untrusted_hop_stops_the_walk() {
        let cfg = ServerConfig {
            trusted_proxies: vec!["10.0.0.0/8".into()],
            ..Default::default()
        };
        // The peer is the proxy, and the header names the real client.
        assert_eq!(
            client_ip(
                &cfg,
                "10.0.0.1",
                &headers(&[("X-Forwarded-For", "203.0.113.7")])
            ),
            "203.0.113.7"
        );
        // Two trusted hops: keep peeling.
        assert_eq!(
            client_ip(
                &cfg,
                "10.0.0.1",
                &headers(&[("X-Forwarded-For", "203.0.113.7, 10.0.0.2")])
            ),
            "203.0.113.7"
        );
        // A client that prepends its own hops does not get to choose which
        // one is read: the walk stops at the first address the configured
        // proxies do not vouch for, which is the rightmost untrusted one.
        assert_eq!(
            client_ip(
                &cfg,
                "10.0.0.1",
                &headers(&[("X-Forwarded-For", "1.1.1.1, 203.0.113.7")])
            ),
            "203.0.113.7"
        );
        // And a peer that is not itself trusted is the answer, header or no
        // header.
        assert_eq!(
            client_ip(
                &cfg,
                "198.51.100.9",
                &headers(&[("X-Forwarded-For", "1.1.1.1")])
            ),
            "198.51.100.9"
        );
    }

    #[test]
    fn a_header_by_any_other_name_is_still_ignored() {
        // `X-Real-IP` and friends are not read at all. One header, one
        // rule: a second source of truth is a second thing to get wrong.
        let cfg = ServerConfig {
            trusted_proxies: vec!["10.0.0.0/8".into()],
            ..Default::default()
        };
        assert_eq!(
            client_ip(
                &cfg,
                "10.0.0.1",
                &headers(&[("X-Real-IP", "1.2.3.4"), ("Forwarded", "for=1.2.3.4")])
            ),
            "10.0.0.1"
        );
    }
}

#[cfg(test)]
mod server_limits {
    use super::*;
    use crate::exec::CorsConfig;

    #[test]
    fn durations_carry_a_unit_or_are_not_read() {
        assert_eq!(
            parse_duration("30s"),
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            parse_duration("600ms"),
            Some(std::time::Duration::from_millis(600))
        );
        assert_eq!(
            parse_duration("5m"),
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(
            parse_duration("1h"),
            Some(std::time::Duration::from_secs(3600))
        );
        assert_eq!(
            parse_duration(" 30s "),
            Some(std::time::Duration::from_secs(30))
        );
        // A bare number is refused, not read as seconds: `= 30` and `= "30s"`
        // would then mean the same thing and `= 30000` would silently mean
        // eight hours.
        assert_eq!(parse_duration("30"), None);
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration("30 seconds"), None);
    }

    #[test]
    fn an_origin_is_echoed_and_an_unlisted_one_gets_nothing() {
        let cors = CorsConfig {
            origins: vec!["https://app.example.com".into()],
            ..Default::default()
        };
        assert_eq!(
            cors.allow("https://app.example.com").as_deref(),
            Some("https://app.example.com")
        );
        assert_eq!(cors.allow("https://evil.example.com"), None);
        // Echoed rather than answered with `*`, because `*` and
        // `credentials` are mutually exclusive in the fetch spec and
        // echoing keeps one code path for both.
        let any = CorsConfig {
            origins: vec!["*".into()],
            ..Default::default()
        };
        assert_eq!(
            any.allow("https://anything.example").as_deref(),
            Some("https://anything.example")
        );
    }

    #[test]
    fn the_tls_block_and_header_timeout_are_read_as_values() {
        let src = "namespace s;\n\
                   server { header_timeout = \"3s\"; tls { cert = \"/c\"; key = \"/k\"; } }\n";
        let cfg = server_config_of(src);
        assert_eq!(cfg.header_timeout, std::time::Duration::from_secs(3));
        let tls = cfg.tls.expect("tls block");
        assert_eq!(tls.cert, "/c");
        assert_eq!(tls.key, "/k");
        assert!(cfg.tls_declared);
    }

    #[test]
    fn bind_defaults_to_every_interface_and_is_settable() {
        // `0.0.0.0` is the default because a container publishes a port
        // and expects the process to be reachable through it. The point of
        // the key is the other direction: a development machine that
        // should not be answering its own LAN.
        assert_eq!(ServerConfig::default().bind, "0.0.0.0");
        let cfg = server_config_of("namespace s;\nserver { bind = \"127.0.0.1\"; }\n");
        assert_eq!(cfg.bind, "127.0.0.1");
        let cfg = server_config_of("namespace s;\nserver { bind = \"::1\"; }\n");
        assert_eq!(cfg.bind, "::1");
    }

    #[test]
    fn header_timeout_defaults_to_the_documented_ten_seconds() {
        // config.md §3.2's table is the promise; a program that writes no
        // `server` block still gets it.
        assert_eq!(
            ServerConfig::default().header_timeout,
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn a_tls_block_whose_paths_do_not_resolve_stays_declared_and_unresolved() {
        // The pair is what `serve` refuses on. `env("TLS_CERT_PATH")` unset
        // must not read as "no TLS was asked for": that would serve every
        // byte in the clear under a block saying otherwise, and the
        // listener answers either way, so nothing outside can tell.
        let src = "namespace s;\n\
                   server { tls { cert = env(\"JWC_TEST_ABSENT_CERT\"); key = \"/k\"; } }\n";
        let cfg = server_config_of(src);
        assert!(cfg.tls_declared, "the block was written");
        assert!(cfg.tls.is_none(), "and it did not resolve");
    }

    fn server_config_of(src: &str) -> ServerConfig {
        let parsed = crate::parse_str("<t>", src);
        let decl = parsed.program.decls.iter().find_map(|d| match d {
            Decl::Server(s) => Some(s),
            _ => None,
        });
        read_server_config(decl.expect("server block"))
    }

    #[test]
    fn the_cors_block_is_read_whole() {
        let src = "namespace s;\n\
                   server {\n\
                   \x20   request_timeout = \"45s\";\n\
                   \x20   shutdown_grace  = \"5s\";\n\
                   \x20   cors {\n\
                   \x20       origins     = [\"https://a.example\"];\n\
                   \x20       methods     = [\"GET\", \"POST\"];\n\
                   \x20       headers     = [\"authorization\"];\n\
                   \x20       credentials = true;\n\
                   \x20       max_age     = \"600s\";\n\
                   \x20   }\n\
                   }\n";
        let parsed = crate::parse_str("<t>", src);
        assert!(!parsed.has_errors(), "{}", parsed.render_all());
        let decl = parsed.program.decls.iter().find_map(|d| match d {
            Decl::Server(s) => Some(s),
            _ => None,
        });
        let cfg = read_server_config(decl.expect("server block"));
        assert_eq!(cfg.request_timeout, std::time::Duration::from_secs(45));
        assert_eq!(cfg.shutdown_grace, std::time::Duration::from_secs(5));
        let cors = cfg.cors.expect("cors");
        assert_eq!(cors.origins, vec!["https://a.example".to_string()]);
        assert_eq!(cors.methods, vec!["GET".to_string(), "POST".to_string()]);
        assert!(cors.credentials);
        assert_eq!(cors.max_age, Some(std::time::Duration::from_secs(600)));
    }
}

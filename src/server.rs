//! HTTP + WebSocket server built on top of axum.
//!
//! The JWC interpreter is fully synchronous (recursive `Vm::eval_expr`,
//! blocking SQL calls, etc.), so this layer keeps a sync API for the rest
//! of the codebase and bridges into the async world by running every route
//! handler on a `spawn_blocking` worker. WS routes get their own bridge:
//! two `tokio::sync::mpsc::unbounded_channel`s carry text frames between
//! the async socket and the blocking JWC handler thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, State,
    },
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};

use crate::ast::{Program, RouteProtocol};
use crate::engine;
use crate::error_report;
use crate::queue;
use crate::runner;

struct ServerMetrics {
    total: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    in_flight: AtomicUsize,
    total_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
}

impl ServerMetrics {
    fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            total_latency_us: AtomicU64::new(0),
            max_latency_us: AtomicU64::new(0),
        }
    }

    fn record_latency_us(&self, latency_us: u64) {
        self.total_latency_us
            .fetch_add(latency_us, Ordering::Relaxed);
        let mut observed = self.max_latency_us.load(Ordering::Relaxed);
        while latency_us > observed {
            match self.max_latency_us.compare_exchange_weak(
                observed,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
    }

    /// Render the counters / gauges in Prometheus exposition format
    /// (text/plain; version=0.0.4). Every metric carries a `# HELP` line
    /// so Grafana's metric explorer surfaces a description, and a
    /// `# TYPE` line so the aggregator picks the right query semantics
    /// (counter vs gauge). No histograms in this slice — latency is
    /// exposed as a running mean and a max so dashboards have something
    /// to chart immediately; bucketed histograms land alongside the
    /// tracing/OTel work.
    fn render_prometheus(&self) -> String {
        let total = self.total.load(Ordering::Relaxed);
        let completed = self.completed.load(Ordering::Relaxed);
        let failed = self.failed.load(Ordering::Relaxed);
        let in_flight = self.in_flight.load(Ordering::Relaxed);
        let total_latency_us = self.total_latency_us.load(Ordering::Relaxed);
        let max_latency_us = self.max_latency_us.load(Ordering::Relaxed);
        let avg_us = if completed == 0 {
            0.0
        } else {
            total_latency_us as f64 / completed as f64
        };
        let mut out = String::with_capacity(1024);
        out.push_str("# HELP jwc_http_requests_total Total HTTP requests received.\n");
        out.push_str("# TYPE jwc_http_requests_total counter\n");
        out.push_str(&format!("jwc_http_requests_total {total}\n"));
        out.push_str(
            "# HELP jwc_http_requests_completed_total HTTP requests that finished with a response.\n",
        );
        out.push_str("# TYPE jwc_http_requests_completed_total counter\n");
        out.push_str(&format!("jwc_http_requests_completed_total {completed}\n"));
        out.push_str(
            "# HELP jwc_http_requests_failed_total HTTP requests that returned a 5xx or panicked.\n",
        );
        out.push_str("# TYPE jwc_http_requests_failed_total counter\n");
        out.push_str(&format!("jwc_http_requests_failed_total {failed}\n"));
        out.push_str(
            "# HELP jwc_http_in_flight Number of HTTP requests being handled right now.\n",
        );
        out.push_str("# TYPE jwc_http_in_flight gauge\n");
        out.push_str(&format!("jwc_http_in_flight {in_flight}\n"));
        out.push_str(
            "# HELP jwc_http_request_latency_avg_seconds Running mean latency since process start.\n",
        );
        out.push_str("# TYPE jwc_http_request_latency_avg_seconds gauge\n");
        out.push_str(&format!(
            "jwc_http_request_latency_avg_seconds {:.6}\n",
            avg_us / 1_000_000.0
        ));
        out.push_str(
            "# HELP jwc_http_request_latency_max_seconds Peak latency observed since start.\n",
        );
        out.push_str("# TYPE jwc_http_request_latency_max_seconds gauge\n");
        out.push_str(&format!(
            "jwc_http_request_latency_max_seconds {:.6}\n",
            max_latency_us as f64 / 1_000_000.0
        ));
        out
    }
}

fn parse_worker_count() -> usize {
    std::env::var("JWC_SERVER_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().max(2))
                .unwrap_or(4)
        })
}

fn parse_metrics_enabled() -> bool {
    std::env::var("JWC_SERVER_METRICS")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn parse_metrics_interval_secs() -> u64 {
    std::env::var("JWC_SERVER_METRICS_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(10)
}

/// Honours `JWC_LOG_FORMAT=json` for the per-request access line so a
/// k8s log aggregator can ingest the stream as line-delimited JSON.
/// Stays in legacy `[JWC] METHOD path -> status` text shape otherwise so
/// `jwc serve --request-logging` reads naturally in a terminal.
///
/// `request_id` is included in the JSON envelope (and at the tail of the
/// text line for grepability) so a single request's lifecycle —
/// access line, error line, downstream service log — can be reassembled
/// by id without timestamp guessing.
fn log_request_line(method: &str, path: &str, status: u16, latency_us: u64, request_id: &str) {
    if std::env::var("JWC_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        // No escaping needed — method is an HTTP token (ASCII), status
        // is a number, latency is a number. The path can contain
        // arbitrary bytes; pass it through a minimal `"` / `\\`
        // replace which is sufficient for log shapes (control bytes
        // are already filtered by axum's URL parser).
        let path_esc = path.replace('\\', "\\\\").replace('"', "\\\"");
        eprintln!(
            "{{\"level\":\"info\",\"kind\":\"access\",\"request_id\":\"{rid}\",\"method\":\"{m}\",\"path\":\"{p}\",\"status\":{s},\"latency_us\":{lat}}}",
            rid = request_id,
            m = method,
            p = path_esc,
            s = status,
            lat = latency_us,
        );
    } else {
        eprintln!("[JWC] {method} {path} -> {status} (rid={request_id})");
    }
}

fn parse_shutdown_timeout_secs() -> u64 {
    std::env::var("JWC_SHUTDOWN_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(5)
}

/// Per-request budget in seconds. `0` opts out (no watchdog). Default
/// 30 covers normal CRUD plus a healthy slack for slow DB queries;
/// streaming endpoints belong on a separate handler that bypasses this
/// path, so a long-running response there isn't artificially capped.
fn parse_request_timeout_secs() -> u64 {
    match std::env::var("JWC_REQUEST_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(0) => 0,
        Some(n) => n,
        None => 30,
    }
}

/// Request body cap applied by [`build_router`]. The axum default is 2
/// MiB, which is what we keep when `JWC_MAX_BODY_BYTES` is unset — that's
/// large enough for typical JSON payloads and small uploads, small enough
/// that a runaway `curl -d @huge.bin` can't OOM the worker. Setting the
/// var to `0` disables the cap (interpreted as "trust the proxy" for
/// users running behind nginx / cloud load balancers that already enforce
/// a size).
const DEFAULT_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

fn parse_max_body_bytes() -> Option<usize> {
    match std::env::var("JWC_MAX_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(DEFAULT_MAX_BODY_BYTES),
    }
}

/// Resolves on the first of SIGINT (Ctrl+C, both platforms) or SIGTERM
/// (Unix only — Windows has no SIGTERM equivalent). Logs which signal
/// fired so kubelet's TERM during a rolling deploy is distinguishable
/// from an operator's Ctrl+C, signals open WebSocket handlers to send a
/// `1001 Going Away` close, and arms a watchdog that force-exits if
/// inflight requests don't drain within `JWC_SHUTDOWN_TIMEOUT` (default
/// 5s). Handed to axum's `with_graceful_shutdown`, which stops accepting
/// new connections and waits for inflight requests to complete.
///
/// Without the SIGTERM branch the kubelet would hit the
/// `terminationGracePeriodSeconds` ceiling on every pod and SIGKILL the
/// process, breaking in-flight requests — the exact failure mode the
/// production-readiness plan calls out as a 1.0-blocker.
async fn shutdown_signal(metrics: Arc<ServerMetrics>, ws_shutdown: watch::Sender<bool>) {
    let reason = wait_for_shutdown_signal().await;
    let in_flight = metrics.in_flight.load(Ordering::Relaxed);
    let pending = queue::pending_count();
    eprintln!(
        "Shutdown signal {reason} received, draining {in_flight} inflight requests + {pending} pending jobs..."
    );
    // Tell open WS writer loops to emit a 1001 close frame and wind down.
    let _ = ws_shutdown.send(true);
    let timeout = parse_shutdown_timeout_secs();
    // Block-spawn a queue drain on a worker thread (it polls in a tight
    // loop with sleeps — fine on the blocking pool, the request path
    // doesn't go through it). Best-effort: we let axum's own
    // with_graceful_shutdown handle the HTTP side in parallel.
    tokio::task::spawn_blocking(move || {
        let left = queue::drain_for(Duration::from_secs(timeout));
        if left > 0 {
            eprintln!("Queue drain timed out with {left} jobs still pending.");
        } else {
            eprintln!("Queue drained cleanly.");
        }
    });
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(timeout)).await;
        eprintln!("Graceful shutdown timed out after {timeout}s; forcing exit.");
        std::process::exit(0);
    });
}

/// Race SIGINT (every platform) against SIGTERM (Unix only) and return
/// a short label naming whichever fires first. Returned label appears
/// verbatim in the shutdown log line so operators can distinguish a
/// kubelet rolling-deploy SIGTERM from an interactive Ctrl+C.
#[cfg(unix)]
async fn wait_for_shutdown_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            // SIGTERM listener unavailable — fall back to SIGINT only.
            let _ = tokio::signal::ctrl_c().await;
            return "SIGINT";
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = term.recv() => "SIGTERM",
    }
}

/// Windows has no SIGTERM. Ctrl+C / Ctrl+Break both surface through
/// `tokio::signal::ctrl_c` — keep the label simple.
#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "SIGINT"
}

#[derive(Clone)]
struct AppState {
    program: Arc<Program>,
    request_logging: bool,
    metrics: Arc<ServerMetrics>,
    /// Flips to `true` on shutdown so WS writer loops send a `1001` close.
    ws_shutdown: watch::Receiver<bool>,
}

pub fn serve(program: &Program, port: u16, request_logging: bool) -> Result<()> {
    if std::env::var("DATABASE_URL").is_ok() || std::env::var("JWC_DATABASE_URL").is_ok() {
        engine::init_engine_from_env()?;
    }

    let shared_program = Arc::new(program.clone());
    queue::init_queue(Arc::clone(&shared_program));

    let metrics = Arc::new(ServerMetrics::new());
    let worker_count = parse_worker_count();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_count)
        .enable_all()
        .thread_name("jwc-server")
        .build()
        .map_err(|e| anyhow!("Failed to build tokio runtime: {e}"))?;

    let (ws_shutdown_tx, ws_shutdown_rx) = watch::channel(false);

    let state = AppState {
        program: Arc::clone(&shared_program),
        request_logging,
        metrics: Arc::clone(&metrics),
        ws_shutdown: ws_shutdown_rx,
    };

    if parse_metrics_enabled() {
        let metrics = Arc::clone(&metrics);
        let interval = Duration::from_secs(parse_metrics_interval_secs());
        rt.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let total = metrics.total.load(Ordering::Relaxed);
                let completed = metrics.completed.load(Ordering::Relaxed);
                let failed = metrics.failed.load(Ordering::Relaxed);
                let in_flight = metrics.in_flight.load(Ordering::Relaxed);
                let total_latency_us = metrics.total_latency_us.load(Ordering::Relaxed);
                let max_latency_us = metrics.max_latency_us.load(Ordering::Relaxed);
                let avg_us = if completed == 0 {
                    0.0
                } else {
                    total_latency_us as f64 / completed as f64
                };
                eprintln!(
                    "[JWC-METRICS] in_flight={in_flight} total={total} completed={completed} failed={failed} avg_latency_ms={:.3} max_latency_ms={:.3}",
                    avg_us / 1000.0,
                    max_latency_us as f64 / 1000.0
                );
            }
        });
    }

    let app = build_router(state);
    // Bind to 0.0.0.0 so the server accepts traffic on every interface,
    // but print the loopback URL — most browsers/curl reject `0.0.0.0`,
    // and `localhost` is what users actually paste into a tab.
    let addr = format!("0.0.0.0:{port}");
    let public_url = format!("http://localhost:{port}");
    let label = format!("║  {:<34}  ║", public_url);
    println!("╔══════════════════════════════════════╗");
    println!("║         JWC Server started           ║");
    println!("╠══════════════════════════════════════╣");
    println!("{}", label);
    println!("║  Press Ctrl+C to stop                ║");
    println!("╚══════════════════════════════════════╝");
    println!();

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| anyhow!("Failed to bind to {addr}: {e}"))?;
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(Arc::clone(&metrics), ws_shutdown_tx))
            .await
            .map_err(|e| anyhow!("axum serve error: {e}"))?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Whether the user's program declared its own route for the given path.
/// `/healthz` and `/readyz` are reserved for the built-in handlers — but
/// only when the user hasn't taken them over. Mirrors axum's path
/// matching (case-sensitive, exact match with leading slash) so the
/// built-ins yield to a project that wants its own implementation.
fn route_owned_by_user(program: &Program, path: &str) -> bool {
    program
        .routes
        .iter()
        .any(|r| ensure_leading_slash(&r.path) == path)
}

/// Process-alive probe. Always 200 — if axum can run a handler the
/// process is up. Kubernetes' `livenessProbe` target.
async fn handle_healthz() -> Response {
    let body = "{\"status\":\"ok\"}".to_string();
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        "content-type",
        "application/json"
            .parse()
            .expect("INVARIANT: 'application/json' is a valid HeaderValue"),
    );
    resp
}

/// Readiness probe with a real DB round-trip. Used by k8s `readinessProbe`
/// — when the DB is unreachable the pod stops getting traffic instead of
/// returning 500s. jwc-shortener hand-rolled its `/healthz` with no DB
/// check, so probes stayed green through a database outage; this closes
/// that gap by default.
async fn handle_readyz() -> Response {
    // No DB configured → readiness is just process-alive (nothing to
    // probe). Avoid wedging the gate on a missing env var.
    if std::env::var("DATABASE_URL").is_err() && std::env::var("JWC_DATABASE_URL").is_err() {
        return handle_healthz().await;
    }
    match engine::get_connection().await {
        Ok(conn) => match conn.simple_query("SELECT 1").await {
            Ok(_) => {
                let body = "{\"status\":\"ready\",\"db\":\"ok\"}".to_string();
                let mut resp = Response::new(body.into());
                *resp.status_mut() = StatusCode::OK;
                resp.headers_mut().insert(
                    "content-type",
                    "application/json"
                        .parse()
                        .expect("INVARIANT: 'application/json' is a valid HeaderValue"),
                );
                resp
            }
            Err(e) => readyz_db_failure(format!("query: {e}")),
        },
        Err(e) => readyz_db_failure(format!("pool: {e}")),
    }
}

/// Prometheus scrape target. Returns the running counters / gauges in
/// text exposition format so dashboards can chart traffic, error rate,
/// in-flight saturation, and latency without a hand-rolled instrumentation
/// route in every project. The PRODUCTION_READINESS_PLAN's Phase 5
/// observability gap names exactly this endpoint.
async fn handle_metrics(State(state): State<AppState>) -> Response {
    let mut body = state.metrics.render_prometheus();
    // Background queue depth — operators chart this to spot a backlog
    // before it becomes a SLO breach. The Phase 5 plan flags it as part
    // of the observability gap.
    let pending = queue::pending_count();
    let dlq = queue::dlq_count();
    body.push_str("# HELP jwc_queue_pending Number of jobs waiting to be processed by a worker.\n");
    body.push_str("# TYPE jwc_queue_pending gauge\n");
    body.push_str(&format!("jwc_queue_pending {pending}\n"));
    body.push_str("# HELP jwc_queue_dlq Number of permanently-failed jobs held in the DLQ.\n");
    body.push_str("# TYPE jwc_queue_dlq gauge\n");
    body.push_str(&format!("jwc_queue_dlq {dlq}\n"));
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::OK;
    // Prometheus' text exposition spec. Versioned content-type so
    // scrapers can distinguish from a future protobuf body.
    resp.headers_mut().insert(
        "content-type",
        "text/plain; version=0.0.4; charset=utf-8"
            .parse()
            .expect("INVARIANT: prometheus content-type literal is a valid HeaderValue"),
    );
    resp
}

/// Monotonic per-process counter — combined with the process start time
/// it produces a hard-to-collide request id without pulling a uuid or
/// rand crate into the runtime. Collisions across two processes are
/// fine: the id only needs to be unique within ONE process's logs;
/// cross-host correlation belongs to the tracing layer.
static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a 16-hex-char request id of the form `<wall_secs_hex><counter_hex>`.
/// Reused by both the access log and the runner-side `request_id()`
/// builtin so a single request shows the same id in every log line.
fn next_request_id() -> String {
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let n = REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:08x}{:08x}", wall as u32, n as u32)
}

/// Parse a W3C `traceparent` header (`00-<trace-id-32-hex>-<span-id-16-hex>-<flags>`)
/// and return the trace-id. Validation is minimal: format shape +
/// hex-only on the trace-id slot. Anything malformed falls back to
/// generating a local id — never refuse a request over a broken
/// upstream tracing header.
fn extract_traceparent_trace_id(value: &str) -> Option<String> {
    let mut parts = value.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let _span_id = parts.next()?;
    let _flags = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if version.len() != 2 || trace_id.len() != 32 {
        return None;
    }
    if !trace_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(trace_id.to_string())
}

fn readyz_db_failure(detail: String) -> Response {
    let escaped = detail.replace('"', "'");
    let body = format!("{{\"status\":\"not_ready\",\"db\":\"{escaped}\"}}");
    let mut resp = Response::new(body.into());
    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    resp.headers_mut().insert(
        "content-type",
        "application/json"
            .parse()
            .expect("INVARIANT: 'application/json' is a valid HeaderValue"),
    );
    resp
}

fn build_router(state: AppState) -> Router {
    let mut router: Router<AppState> = Router::new();

    // Built-in health endpoints. Registered BEFORE the user's routes so
    // they take precedence, then deliberately overridden when the user
    // ships their own `/healthz` / `/readyz`. axum routes are matched
    // exact-first; the fallback at the end catches everything else.
    if !route_owned_by_user(&state.program, "/healthz") {
        router = router.route("/healthz", get(handle_healthz));
    }
    if !route_owned_by_user(&state.program, "/readyz") {
        router = router.route("/readyz", get(handle_readyz));
    }
    if !route_owned_by_user(&state.program, "/metrics") {
        router = router.route("/metrics", get(handle_metrics));
    }

    // Each WS route gets its own axum entry. JWC path placeholders
    // (`/items/{id}`) line up with axum's `{name}` form one-to-one.
    for route in state.program.routes.iter() {
        if route.protocol != RouteProtocol::Ws {
            continue;
        }
        let axum_path = ensure_leading_slash(&route.path);
        let captured_route_path = route.path.clone();
        router = router.route(
            &axum_path,
            get(
                move |ws: WebSocketUpgrade,
                      Path(path_params): Path<HashMap<String, String>>,
                      headers: HeaderMap,
                      State(s): State<AppState>| {
                    let route_path = captured_route_path.clone();
                    let header_map: HashMap<String, String> = headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_ascii_lowercase(),
                                v.to_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect();
                    async move {
                        ws.on_upgrade(move |socket| {
                            handle_ws(socket, s, route_path, path_params, header_map)
                        })
                    }
                },
            ),
        );
    }

    let mut built = router.fallback(handle_http_fallback).with_state(state);
    // Body-size cap — a missing limit lets a single client OOM the
    // worker by streaming an unbounded `curl -d @huge.bin`.
    // `JWC_MAX_BODY_BYTES=0` opts out for users running behind a proxy
    // that already enforces a size; everything else takes a hard cap.
    if let Some(max) = parse_max_body_bytes() {
        built = built.layer(DefaultBodyLimit::max(max));
    } else {
        built = built.layer(DefaultBodyLimit::disable());
    }
    built
}

fn ensure_leading_slash(p: &str) -> String {
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{}", p)
    }
}

async fn handle_http_fallback(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.metrics.total.fetch_add(1, Ordering::Relaxed);
    state.metrics.in_flight.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();

    let header_map: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    // Preserve the query string so `query_param(name)` keeps working.
    let path = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());
    let method_str = method.as_str().to_string();
    let body_string = String::from_utf8_lossy(&body).to_string();
    let body_opt = if body_string.trim().is_empty() {
        None
    } else {
        Some(body_string)
    };

    // Stamp a request id BEFORE dispatching so middleware / handler /
    // errorHandler all read the same value via `request_id()`. Honour
    // an inbound W3C `traceparent` header — if the upstream service
    // already started a trace, reuse its `trace-id` so distributed
    // tracing dashboards (Tempo, Jaeger, Honeycomb) can correlate
    // hops without manual stitching. Fall back to the local
    // counter id when no traceparent is present.
    let request_id = headers
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .and_then(extract_traceparent_trace_id)
        .unwrap_or_else(next_request_id);
    // Per-request budget. The watchdog fires after `JWC_REQUEST_TIMEOUT`
    // (default 30s) and returns a 504 instead of letting a slow handler
    // hold the worker indefinitely. `0` opts out for projects that
    // genuinely need long-running responses (SSE / streaming WS sit on
    // separate handlers and aren't covered by this path).
    let timeout = parse_request_timeout_secs();
    let program = Arc::clone(&state.program);
    let req_id_for_runner = request_id.clone();
    let join_handle = tokio::spawn(async move {
        runner::run_request_with_headers_and_id(
            &program,
            &method_str,
            &path,
            body_opt,
            header_map,
            Some(req_id_for_runner),
        )
        .await
    });
    let result = if timeout > 0 {
        match tokio::time::timeout(Duration::from_secs(timeout), join_handle).await {
            Ok(joined) => joined,
            Err(_) => {
                // The handler is still running on a worker thread —
                // we can't cancel `spawn_blocking`-flavoured work
                // mid-flight, but we can stop waiting on it and return
                // 504 to the client. The handler will finish on its
                // own (or be killed by the watchdog on shutdown) and
                // its return value is discarded.
                state.metrics.failed.fetch_add(1, Ordering::Relaxed);
                state.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                if state.request_logging {
                    log_request_line(
                        method.as_str(),
                        uri.path(),
                        504,
                        timeout * 1_000_000,
                        &request_id,
                    );
                }
                let body = format!(
                    "{{\"error\":\"request timed out after {timeout}s\",\"request_id\":\"{request_id}\"}}"
                );
                let mut resp = Response::new(body.into());
                *resp.status_mut() = StatusCode::GATEWAY_TIMEOUT;
                resp.headers_mut().insert(
                    "content-type",
                    "application/json"
                        .parse()
                        .expect("INVARIANT: 'application/json' is a valid HeaderValue"),
                );
                if let Ok(rid) = request_id.parse() {
                    resp.headers_mut().insert("x-request-id", rid);
                }
                return resp;
            }
        }
    } else {
        join_handle.await
    };

    let elapsed = started.elapsed().as_micros() as u64;
    state.metrics.record_latency_us(elapsed);
    state.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);

    let response: Response = match result {
        Ok(Ok((status, body, content_type, extra_headers))) => {
            state.metrics.completed.fetch_add(1, Ordering::Relaxed);
            if state.request_logging {
                log_request_line(method.as_str(), uri.path(), status, elapsed, &request_id);
            }
            let mut resp = Response::new(body.into());
            *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            if let Ok(rid) = request_id.parse() {
                resp.headers_mut().insert("x-request-id", rid);
            }
            // Echo inbound `traceparent` / `tracestate` back as response
            // headers so the caller and downstream services can stitch
            // their traces with ours without re-parsing the body. Spec:
            // W3C Trace Context §3 — tracestate may be a comma-separated
            // list, kept verbatim, header value casing preserved.
            if let Some(tp) = headers.get("traceparent").cloned() {
                resp.headers_mut().insert("traceparent", tp);
            }
            if let Some(ts) = headers.get("tracestate").cloned() {
                resp.headers_mut().insert("tracestate", ts);
            }
            // `html(...)` / future `text(...)` declare an explicit content-type
            // via the runtime envelope; everything else falls back to JSON for
            // backward compatibility with handlers that just `return obj`.
            let ct = content_type.as_deref().unwrap_or("application/json");
            if let Ok(value) = ct.parse() {
                resp.headers_mut().insert("content-type", value);
            } else {
                resp.headers_mut().insert(
                    "content-type",
                    "application/json"
                        .parse()
                        .expect("INVARIANT: 'application/json' is a valid HeaderValue"),
                );
            }
            // Extra headers declared by the handler (e.g.
            // `statusCode(302, { Location: url })` produces a `Location`
            // entry). Skip names/values that don't parse as valid HTTP
            // header bits rather than failing the whole response.
            for (name, value) in extra_headers {
                if let (Ok(name), Ok(value)) = (
                    name.parse::<axum::http::HeaderName>(),
                    value.parse::<axum::http::HeaderValue>(),
                ) {
                    resp.headers_mut().insert(name, value);
                }
            }
            resp
        }
        Ok(Err(e)) => {
            state.metrics.failed.fetch_add(1, Ordering::Relaxed);
            error_report::log_runtime_error(&format!("HTTP {} {} failed", method, uri.path()), &e);
            let msg = error_report::to_single_line(&e).replace('"', "'");
            let body = format!("{{\"error\":\"{msg}\"}}");
            let mut resp = Response::new(body.into());
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp.headers_mut().insert(
                "content-type",
                "application/json"
                    .parse()
                    .expect("INVARIANT: 'application/json' is a valid HeaderValue"),
            );
            resp
        }
        Err(_join_err) => {
            state.metrics.failed.fetch_add(1, Ordering::Relaxed);
            let mut resp = Response::new("{\"error\":\"task join\"}".into());
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        }
    };

    response
}

async fn handle_ws(
    socket: WebSocket,
    state: AppState,
    route_path: String,
    path_params: HashMap<String, String>,
    headers: HashMap<String, String>,
) {
    let (tx_to_vm, rx_to_vm) = mpsc::unbounded_channel::<String>();
    let (tx_from_vm, mut rx_from_vm) = mpsc::unbounded_channel::<String>();

    // Reader loop: WS frames → vm-input queue.
    let (mut ws_sink, mut ws_stream) = futures_util::StreamExt::split(socket);
    let reader = tokio::spawn(async move {
        while let Some(frame) = ws_stream.next().await {
            match frame {
                Ok(Message::Text(t)) => {
                    if tx_to_vm.send(t.to_string()).is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    // Writer loop: vm-output queue → WS frames. Also watches the server-wide
    // shutdown signal so an in-progress connection is closed with `1001 Going
    // Away` instead of being dropped mid-stream.
    let mut ws_shutdown = state.ws_shutdown.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = rx_from_vm.recv() => match msg {
                    Some(msg) => {
                        if ws_sink.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                changed = ws_shutdown.changed() => {
                    if changed.is_err() || *ws_shutdown.borrow() {
                        let _ = ws_sink
                            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                                code: 1001,
                                reason: "server shutting down".into(),
                            })))
                            .await;
                        break;
                    }
                }
            }
        }
        let _ = ws_sink.close().await;
    });

    let program = Arc::clone(&state.program);
    let path_str = route_path.clone();
    let join = tokio::spawn(async move {
        runner::run_ws_request(
            &program,
            &path_str,
            path_params,
            headers,
            rx_to_vm,
            tx_from_vm,
        )
        .await
    })
    .await;

    if let Err(e) = &join {
        eprintln!("[JWC-WS] handler task join error: {e}");
    } else if let Ok(Err(e)) = &join {
        error_report::log_runtime_error(&format!("WS {} failed", route_path), e);
    }

    // Best effort: close out the helper tasks.
    let _ = reader.await;
    let _ = writer.await;
}

// Silence unused warning when downstream callers don't need `socket` rebound.
#[allow(dead_code)]
fn _socket_marker(_s: WebSocket) {}

#[cfg(test)]
mod tests {
    //! Phase 5 hardening defaults — verifies the env-driven knobs apply
    //! the right cap. The body-limit and shutdown-timeout pure parsers
    //! get exercised here; integration coverage (curl a 3 MiB body and
    //! see a 413) belongs in the Phase 5 e2e suite once it lands.
    use super::*;

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        // Snapshot the env var so concurrent tests don't clobber each
        // other on shared process state. Each call sets / clears, runs
        // the closure, then restores.
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn make_program_with_route(path: &str) -> Program {
        Program {
            routes: vec![crate::ast::RouteDecl {
                method: "GET".into(),
                path: path.into(),
                handler: None,
                body: Vec::new(),
                middlewares: Vec::new(),
                protocol: RouteProtocol::Http,
                namespace: Vec::new(),
                offset: 0,
                file_idx: 0,
            }],
            ..Program::default()
        }
    }

    #[test]
    fn route_owned_by_user_detects_user_takeover() {
        // Built-in `/healthz` and `/readyz` should yield when the user
        // ships their own — the precedence test of the dogfooding gap
        // (jwc-shortener's hand-rolled `/healthz` had no DB check).
        let prog = make_program_with_route("/healthz");
        assert!(route_owned_by_user(&prog, "/healthz"));
        assert!(!route_owned_by_user(&prog, "/readyz"));
    }

    #[test]
    fn route_owned_by_user_handles_missing_leading_slash() {
        // JWC route paths sometimes drop the leading slash; the lookup
        // must compare against the normalized form so `route GET "healthz"`
        // also overrides the built-in.
        let prog = make_program_with_route("healthz");
        assert!(route_owned_by_user(&prog, "/healthz"));
    }

    #[test]
    fn metrics_render_prometheus_shapes_text_exposition_format() {
        let m = ServerMetrics::new();
        m.total.fetch_add(7, Ordering::Relaxed);
        m.completed.fetch_add(5, Ordering::Relaxed);
        m.failed.fetch_add(2, Ordering::Relaxed);
        m.in_flight.fetch_add(1, Ordering::Relaxed);
        m.record_latency_us(1500);
        m.record_latency_us(3000);
        let text = m.render_prometheus();
        // Each metric must carry both HELP + TYPE lines so Grafana's
        // explorer surfaces them and the aggregator picks the right
        // query semantics.
        assert!(text.contains("# HELP jwc_http_requests_total"));
        assert!(text.contains("# TYPE jwc_http_requests_total counter"));
        assert!(text.contains("jwc_http_requests_total 7"));
        assert!(text.contains("jwc_http_requests_completed_total 5"));
        assert!(text.contains("jwc_http_requests_failed_total 2"));
        assert!(text.contains("jwc_http_in_flight 1"));
        // Latency is exposed as seconds (Prometheus convention) — the
        // raw micros sum at 4500 turns into a 0.0009s running mean
        // across the 5 completed requests, peaking at 0.003s.
        assert!(text.contains("jwc_http_request_latency_max_seconds 0.003000"));
    }

    #[tokio::test]
    async fn healthz_returns_ok_and_json_content_type() {
        let resp = handle_healthz().await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap()),
            Some("application/json"),
        );
    }

    #[tokio::test]
    async fn readyz_falls_back_to_healthz_when_no_db_configured() {
        // No DATABASE_URL → /readyz behaves like /healthz, no pool ping.
        with_env("DATABASE_URL", None, || {});
        with_env("JWC_DATABASE_URL", None, || {});
        let prev = (
            std::env::var("DATABASE_URL").ok(),
            std::env::var("JWC_DATABASE_URL").ok(),
        );
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("JWC_DATABASE_URL");
        let resp = handle_readyz().await;
        assert_eq!(resp.status(), StatusCode::OK);
        if let Some(v) = prev.0 {
            std::env::set_var("DATABASE_URL", v);
        }
        if let Some(v) = prev.1 {
            std::env::set_var("JWC_DATABASE_URL", v);
        }
    }

    #[test]
    fn traceparent_extracts_trace_id_from_well_formed_header() {
        let trace = "0af7651916cd43dd8448eb211c80319c";
        let header = format!("00-{trace}-b7ad6b7169203331-01");
        assert_eq!(
            extract_traceparent_trace_id(&header).as_deref(),
            Some(trace)
        );
    }

    #[test]
    fn traceparent_rejects_wrong_section_count() {
        assert!(extract_traceparent_trace_id("00-aaaa-bbbb").is_none());
        assert!(extract_traceparent_trace_id("00-aaaa-bbbb-01-extra").is_none());
    }

    #[test]
    fn traceparent_rejects_non_hex_trace_id() {
        // Letter 'z' isn't hex — must fall back to local id generation.
        let bad = "00-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-b7ad6b7169203331-01";
        assert!(extract_traceparent_trace_id(bad).is_none());
    }

    #[test]
    fn traceparent_rejects_wrong_trace_id_length() {
        // 31 chars instead of 32 — header is malformed, ignore.
        let short = "00-0af7651916cd43dd8448eb211c8031-b7ad6b7169203331-01";
        assert!(extract_traceparent_trace_id(short).is_none());
    }

    #[test]
    fn max_body_bytes_defaults_to_two_mib_when_unset() {
        with_env("JWC_MAX_BODY_BYTES", None, || {
            assert_eq!(parse_max_body_bytes(), Some(DEFAULT_MAX_BODY_BYTES));
        });
    }

    #[test]
    fn max_body_bytes_zero_disables_the_cap() {
        with_env("JWC_MAX_BODY_BYTES", Some("0"), || {
            assert_eq!(parse_max_body_bytes(), None);
        });
    }

    #[test]
    fn max_body_bytes_custom_value_round_trips() {
        with_env("JWC_MAX_BODY_BYTES", Some("8388608"), || {
            assert_eq!(parse_max_body_bytes(), Some(8 * 1024 * 1024));
        });
    }

    #[test]
    fn shutdown_timeout_defaults_to_five_seconds() {
        with_env("JWC_SHUTDOWN_TIMEOUT", None, || {
            assert_eq!(parse_shutdown_timeout_secs(), 5);
        });
    }

    #[test]
    fn request_timeout_defaults_to_thirty_seconds() {
        let prev = std::env::var("JWC_REQUEST_TIMEOUT").ok();
        std::env::remove_var("JWC_REQUEST_TIMEOUT");
        assert_eq!(parse_request_timeout_secs(), 30);
        if let Some(v) = prev {
            std::env::set_var("JWC_REQUEST_TIMEOUT", v);
        }
    }

    #[test]
    fn request_timeout_zero_disables_the_watchdog() {
        let prev = std::env::var("JWC_REQUEST_TIMEOUT").ok();
        std::env::set_var("JWC_REQUEST_TIMEOUT", "0");
        // 0 keeps as 0 — unlike JWC_SHUTDOWN_TIMEOUT where 0 falls
        // back. Streaming projects opt out by setting this var
        // explicitly; we honour the choice.
        assert_eq!(parse_request_timeout_secs(), 0);
        match prev {
            Some(v) => std::env::set_var("JWC_REQUEST_TIMEOUT", v),
            None => std::env::remove_var("JWC_REQUEST_TIMEOUT"),
        }
    }

    #[test]
    fn request_timeout_custom_value_round_trips() {
        let prev = std::env::var("JWC_REQUEST_TIMEOUT").ok();
        std::env::set_var("JWC_REQUEST_TIMEOUT", "120");
        assert_eq!(parse_request_timeout_secs(), 120);
        match prev {
            Some(v) => std::env::set_var("JWC_REQUEST_TIMEOUT", v),
            None => std::env::remove_var("JWC_REQUEST_TIMEOUT"),
        }
    }

    #[test]
    fn shutdown_timeout_zero_falls_back_to_default() {
        with_env("JWC_SHUTDOWN_TIMEOUT", Some("0"), || {
            // A zero would mean "force-exit immediately", which silently
            // breaks every in-flight request. The parser drops invalid
            // values back to the safe default rather than honoring zero.
            assert_eq!(parse_shutdown_timeout_secs(), 5);
        });
    }
}

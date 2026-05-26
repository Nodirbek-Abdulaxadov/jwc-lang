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
        Path, State,
    },
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

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

#[derive(Clone)]
struct AppState {
    program: Arc<Program>,
    request_logging: bool,
    metrics: Arc<ServerMetrics>,
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

    let state = AppState {
        program: Arc::clone(&shared_program),
        request_logging,
        metrics: Arc::clone(&metrics),
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
            .await
            .map_err(|e| anyhow!("axum serve error: {e}"))?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

fn build_router(state: AppState) -> Router {
    let mut router: Router<AppState> = Router::new();

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

    router.fallback(handle_http_fallback).with_state(state)
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

    let program = Arc::clone(&state.program);
    let result = tokio::spawn(async move {
        runner::run_request_with_headers(&program, &method_str, &path, body_opt, header_map).await
    })
    .await;

    let elapsed = started.elapsed().as_micros() as u64;
    state.metrics.record_latency_us(elapsed);
    state.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);

    let response: Response = match result {
        Ok(Ok((status, body, content_type, extra_headers))) => {
            state.metrics.completed.fetch_add(1, Ordering::Relaxed);
            if state.request_logging {
                eprintln!("[JWC] {} {} -> {}", method, uri.path(), status);
            }
            let mut resp = Response::new(body.into());
            *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            // `html(...)` / future `text(...)` declare an explicit content-type
            // via the runtime envelope; everything else falls back to JSON for
            // backward compatibility with handlers that just `return obj`.
            let ct = content_type.as_deref().unwrap_or("application/json");
            if let Ok(value) = ct.parse() {
                resp.headers_mut().insert("content-type", value);
            } else {
                resp.headers_mut()
                    .insert("content-type", "application/json".parse().unwrap());
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
            resp.headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
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

    // Writer loop: vm-output queue → WS frames.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx_from_vm.recv().await {
            if ws_sink.send(Message::Text(msg)).await.is_err() {
                break;
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

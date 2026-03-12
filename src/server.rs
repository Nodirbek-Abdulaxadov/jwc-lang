use anyhow::Result;
use tiny_http::{Header, Response, Server};

use crate::ast::Program;
use crate::engine;
use crate::error_report;
use crate::runner;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct ServerMetrics {
    queue_depth: AtomicUsize,
    in_flight: AtomicUsize,
    busy_workers: AtomicUsize,
    total_requests: AtomicU64,
    completed_requests: AtomicU64,
    rejected_requests: AtomicU64,
    total_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
}

impl ServerMetrics {
    fn new() -> Self {
        Self {
            queue_depth: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            busy_workers: AtomicUsize::new(0),
            total_requests: AtomicU64::new(0),
            completed_requests: AtomicU64::new(0),
            rejected_requests: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            max_latency_us: AtomicU64::new(0),
        }
    }

    fn record_latency_us(&self, latency_us: u64) {
        self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);

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

fn parse_queue_capacity(worker_count: usize) -> usize {
    std::env::var("JWC_SERVER_QUEUE_CAPACITY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(worker_count.saturating_mul(64).max(64))
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

pub fn serve(program: &Program, port: u16, request_logging: bool) -> Result<()> {
    if std::env::var("DATABASE_URL").is_ok() || std::env::var("JWC_DATABASE_URL").is_ok() {
        engine::init_engine_from_env()?;
    }

    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr)
        .map_err(|e| anyhow::anyhow!("Failed to bind to {addr}: {e}"))?;

    println!("╔══════════════════════════════════════╗");
    println!("║         JWC Server started           ║");
    println!("╠══════════════════════════════════════╣");
    println!("║  http://{}            ║", addr);
    println!("║  Press Ctrl+C to stop                ║");
    println!("╚══════════════════════════════════════╝");
    println!();

    let content_type: Header = "Content-Type: application/json"
        .parse()
        .expect("valid header");
    let shared_program = Arc::new(program.clone());
    let metrics = Arc::new(ServerMetrics::new());
    let worker_count = parse_worker_count();
    let queue_capacity = parse_queue_capacity(worker_count);
    let metrics_enabled = parse_metrics_enabled();
    let metrics_interval = Duration::from_secs(parse_metrics_interval_secs());
    let (tx, rx) = mpsc::sync_channel::<tiny_http::Request>(queue_capacity);
    let shared_rx = Arc::new(Mutex::new(rx));

    if metrics_enabled {
        let metrics = Arc::clone(&metrics);
        thread::spawn(move || loop {
            thread::sleep(metrics_interval);

            let queue_depth = metrics.queue_depth.load(Ordering::Relaxed);
            let in_flight = metrics.in_flight.load(Ordering::Relaxed);
            let busy_workers = metrics.busy_workers.load(Ordering::Relaxed);
            let total = metrics.total_requests.load(Ordering::Relaxed);
            let completed = metrics.completed_requests.load(Ordering::Relaxed);
            let rejected = metrics.rejected_requests.load(Ordering::Relaxed);
            let total_latency_us = metrics.total_latency_us.load(Ordering::Relaxed);
            let max_latency_us = metrics.max_latency_us.load(Ordering::Relaxed);
            let avg_latency_us = if completed == 0 {
                0.0
            } else {
                total_latency_us as f64 / completed as f64
            };
            let avg_latency_ms = avg_latency_us / 1000.0;
            let max_latency_ms = max_latency_us as f64 / 1000.0;

            eprintln!(
                "[JWC-METRICS] queue_depth={} in_flight={} busy_workers={} total={} completed={} rejected={} avg_latency_ms={:.3} max_latency_ms={:.3}",
                queue_depth,
                in_flight,
                busy_workers,
                total,
                completed,
                rejected,
                avg_latency_ms,
                max_latency_ms
            );
        });
    }

    for _ in 0..worker_count {
        let content_type = content_type.clone();
        let program = Arc::clone(&shared_program);
        let rx = Arc::clone(&shared_rx);
        let metrics = Arc::clone(&metrics);

        thread::spawn(move || loop {
            let next_request = {
                let guard = rx.lock();
                match guard {
                    Ok(rx) => rx.recv(),
                    Err(_) => return,
                }
            };

            let mut request = match next_request {
                Ok(req) => req,
                Err(_) => return,
            };

            metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
            metrics.in_flight.fetch_add(1, Ordering::Relaxed);
            metrics.busy_workers.fetch_add(1, Ordering::Relaxed);
            let started_at = Instant::now();

            let method = request.method().to_string();
            let url = request.url().to_string();

            // Strip query string from path
            let path = url.split('?').next().unwrap_or(&url).to_string();

            // Read request body
            let mut body_bytes = Vec::new();
            let _ = std::io::Read::read_to_end(request.as_reader(), &mut body_bytes);
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();
            let body = if body_str.trim().is_empty() {
                None
            } else {
                Some(body_str)
            };

            // Dispatch to JWC route
            let (status, response_body) = runner::run_request(&program, &method, &path, body)
                .unwrap_or_else(|e| {
                    error_report::log_runtime_error(
                        &format!("HTTP {} {} failed", method, path),
                        &e,
                    );
                    let msg = error_report::to_single_line(&e).replace('"', "'");
                    (500, format!("{{\"error\":\"{msg}\"}}"))
                });

            if request_logging {
                eprintln!("[JWC] {} {} -> {}", method, path, status);
            }

            let response = Response::from_string(response_body)
                .with_status_code(status)
                .with_header(content_type.clone());

            let _ = request.respond(response);

            let latency_us = started_at.elapsed().as_micros() as u64;
            metrics.record_latency_us(latency_us);
            metrics.completed_requests.fetch_add(1, Ordering::Relaxed);
            metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
            metrics.busy_workers.fetch_sub(1, Ordering::Relaxed);
        });
    }

    for request in server.incoming_requests() {
        metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        if let Err(err) = tx.send(request) {
            let request = err.0;
            metrics.rejected_requests.fetch_add(1, Ordering::Relaxed);
            let response = Response::from_string("{\"error\":\"Server queue unavailable\"}")
                .with_status_code(503)
                .with_header(content_type.clone());
            let _ = request.respond(response);
            break;
        }
        metrics.queue_depth.fetch_add(1, Ordering::Relaxed);
    }

    Ok(())
}

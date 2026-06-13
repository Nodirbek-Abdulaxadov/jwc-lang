//! Observability surface — Prometheus `/metrics` lives in `server.rs`, the
//! per-request structured access log lives in `server.rs::log_request_line`,
//! and OTLP trace export lives here.
//!
//! Only `otlp` is in this module today; the access-log and metrics paths
//! pre-date the module and stay where the request handler can reach them
//! without an extra import hop. Future trace-shaped observability (custom
//! `tracing` layers, OTLP metric exporter) belongs here.

pub mod otlp;

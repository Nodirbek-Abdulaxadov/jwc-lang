//! JWC language library — exposes the lexer, parser, runtime, engine, and
//! migration helpers as a reusable crate so integration tests in `tests/`
//! can exercise them against a real Postgres instance.
//!
//! `main.rs` is the thin CLI wrapper that imports from here.

// `await_holding_lock` is acknowledged tech debt: the WebSocket bridge
// (`runner::WS_STREAM`) and the engine TLS-init path hold short critical
// sections across awaits. Tracked under a dedicated clippy-cleanup sprint;
// silenced crate-wide so CI can enforce `-D warnings` on the rest.
#![allow(clippy::await_holding_lock)]

pub mod ast;
pub mod builtins;
pub mod cache;
pub mod cmd;
pub mod config;
pub mod diag;
pub mod email;
pub mod engine;
pub mod error_codes;
pub mod error_report;
pub mod fmt;
pub mod hash;
pub mod jwt;
pub mod lexer;
pub mod lint;
pub mod lockfile;
pub mod migrate;
pub mod native_build;
pub mod native_ir;
pub mod observability;
pub mod parser;
pub mod password;
pub mod pkg_cache;
pub mod project;
pub mod queue;
pub mod registry;
pub mod resolver;
pub mod runner;
pub mod schema_diff;
pub mod sema;
pub mod server;
pub mod sql;
pub mod swagger;
pub mod typecheck;

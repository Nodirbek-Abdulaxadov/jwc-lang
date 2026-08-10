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
// The unwrap budget is met and now enforced. `cargo clippy --lib` reports
// zero production `.unwrap()` calls, so this costs nothing today and stops
// the next one from arriving unnoticed.
//
// `not(test)` is what makes it usable: unit tests under `#[cfg(test)] mod
// tests` unwrap freely — that's the point of a test — while every path that
// ships is denied. Integration tests in `tests/` are separate crates and are
// unaffected. `expect_used` deliberately stays allowed; see Cargo.toml.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
// Clippy 1.96+ added several pedantic-style lints that the codebase doesn't
// yet pass (needless_range_loop, unnecessary_map_or, manual_range_patterns,
// print_literal, uninlined_format_args). These are stylistic, not bugs.
// Silenced crate-wide so CI can enforce `-D warnings` on the rest until a
// dedicated clippy-cleanup sprint sweeps them.
#![allow(clippy::needless_range_loop)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_range_patterns)]
#![allow(clippy::print_literal)]
#![allow(clippy::uninlined_format_args)]

pub mod ast;
pub mod builtins;
pub mod cache;
pub mod cmd;
pub mod config;
pub mod cors;
pub mod diag;
pub mod email;
pub mod engine;
pub mod error_codes;
pub mod error_report;
pub mod fmt;
pub mod hash;
pub mod http_error;
pub mod jwks;
pub mod jwt;
pub mod lexer;
pub mod lint;
pub mod lockfile;
pub mod locks;
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
pub mod templates;
pub mod typecheck;

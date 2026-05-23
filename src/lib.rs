//! JWC language library — exposes the lexer, parser, runtime, engine, and
//! migration helpers as a reusable crate so integration tests in `tests/`
//! can exercise them against a real Postgres instance.
//!
//! `main.rs` is the thin CLI wrapper that imports from here.

pub mod ast;
pub mod cache;
pub mod cmd;
pub mod diag;
pub mod email;
pub mod engine;
pub mod error_report;
pub mod fmt;
pub mod jwt;
pub mod lexer;
pub mod lint;
pub mod lockfile;
pub mod migrate;
pub mod native_build;
pub mod parser;
pub mod password;
pub mod pkg_cache;
pub mod project;
pub mod queue;
pub mod registry;
pub mod resolver;
pub mod runner;
pub mod schema_diff;
pub mod server;
pub mod sql;

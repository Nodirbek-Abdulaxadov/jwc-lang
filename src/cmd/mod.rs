//! CLI subcommand implementations split out from `main.rs` for testability.

pub mod build;
pub mod check;
pub mod fmt;
pub mod lint;
pub mod migrate;
pub mod openapi;
pub mod pkg;
pub mod publish;
pub mod run;
pub mod serve;
pub mod swagger;
pub mod upgrade;
pub mod v1;

//! The `jwc` CLI.
//!
//! Every subcommand is a thin wrapper over a function in `cmd/`; the work
//! lives in the library so the integration tests can drive it directly.

use anyhow::Result;
use clap::{Parser, Subcommand};
use jwc::cmd;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "jwc",
    version,
    about = "JWC — a backend language with first-class routes, tables and views",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and check the sources under a path.
    Check {
        /// File or directory. Defaults to the current directory.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print diagnostics only; no success line.
        #[arg(long)]
        quiet: bool,
        /// Stop after the front-end: no schema model, no type checking.
        #[arg(long)]
        parse_only: bool,
    },
    /// Rewrite sources in canonical form.
    Fmt {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Report what would change and exit non-zero; write nothing.
        #[arg(long)]
        check: bool,
    },
    /// Emit the schema as Postgres DDL.
    ///
    /// Offline: never connects to a database. Deterministic: two runs on
    /// the same source are byte-identical.
    GenSql {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Prefix each statement with the `file:line` that produced it.
        #[arg(long)]
        explain: bool,
        /// Write to a file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print every query the program issues, with its SQL.
    Explain {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// SQL only: skip the raw-tracking line.
        #[arg(long)]
        sql: bool,
    },
    /// Print the resolved route table: method, path, middleware chain.
    Routes {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Run the program.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    /// Print the parsed AST. A debugging aid, not a stable format.
    Ast {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            path,
            quiet,
            parse_only,
        } => cmd::check(path, quiet, parse_only),
        Command::Fmt { path, check } => cmd::fmt(path, check),
        Command::GenSql { path, explain, out } => cmd::gen_sql(path, explain, out),
        Command::Explain { path, sql } => cmd::explain(path, sql),
        Command::Routes { path } => cmd::routes(path),
        Command::Serve { path, port } => cmd::serve(path, port),
        Command::Ast { path } => cmd::ast(path),
    }
}

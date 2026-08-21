//! The `jwc` CLI.
//!
//! Every subcommand is a thin wrapper over a function in `cmd/`; the work
//! lives in the library so the integration tests can drive it directly.

use anyhow::Result;
use clap::{Parser, Subcommand};
use jwc::cmd;
use std::path::PathBuf;
use std::process::ExitCode;

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
        /// Exit non-zero on any warning. The CI shape.
        #[arg(long)]
        deny_warnings: bool,
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
        /// Only the queries this function can reach, over the call graph.
        #[arg(long)]
        function: Option<String>,
        /// Only the queries a request to this route can reach, e.g.
        /// `--route "GET /api/v1/orgs/{org_id}/invoices"`.
        #[arg(long)]
        route: Option<String>,
        /// Also run `EXPLAIN` on each statement against `DATABASE_URL`.
        #[arg(long)]
        analyze: bool,
    },
    /// Store a registry API key in `~/.jwc/credentials.json`.
    Login {
        /// A `jwc_…` key, from the registry's web UI.
        #[arg(long)]
        token: String,
        #[arg(long, default_value_t = jwc::registry::registry_url())]
        registry: String,
    },
    /// Upload this package to the registry.
    Publish {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = jwc::registry::registry_url())]
        registry: String,
        /// Print what would be uploaded and stop.
        #[arg(long)]
        dry_run: bool,
    },
    /// Download a package and record it as a dependency.
    Add {
        /// `redis`, or `redis@0.1.0`.
        spec: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = jwc::registry::registry_url())]
        registry: String,
    },
    /// Run every `test` block.
    ///
    /// Each test runs in its own transaction and is rolled back, so the
    /// order is irrelevant.
    Test {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only tests whose name contains this.
        #[arg(long)]
        filter: Option<String>,
        /// Commit instead of rolling back. Leaves data behind.
        #[arg(long)]
        no_rollback: bool,
    },
    /// Run the language server, speaking LSP over stdio.
    Lsp,
    /// Emit an OpenAPI 3.1 document for the route table.
    ///
    /// Offline: derived from the typed signatures and the raise sets, never
    /// from a running server.
    Openapi {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Write to a file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// `info.title`. Defaults to the `database` name.
        #[arg(long)]
        title: Option<String>,
    },
    /// `check`, plus the whole-program lints that are advisory.
    Lint {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print every constraint each route can reach, with the status its
        /// violation produces.
        #[arg(long)]
        constraints: bool,
        /// Exit non-zero on any warning. The CI shape.
        #[arg(long)]
        deny_warnings: bool,
    },
    /// Print the resolved route table: method, path, middleware chain.
    Routes {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Compile the program to a native binary (AOT).
    ///
    /// Restored in 0.9.901. Coverage is the database-free tier; anything
    /// outside it is refused by name rather than silently dropped, and
    /// `jwc serve` runs the whole language.
    Build {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Optimised build.
        #[arg(long)]
        release: bool,
        /// Write the generated Rust and stop, without invoking cargo.
        #[arg(long)]
        emit_rust: bool,
    },
    /// Run the program.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Override the port the program declares with `serve(...)`.
        /// Without it the program's own value is used.
        #[arg(long)]
        port: Option<u16>,
        /// Start even when the database is missing a table or column the
        /// program reads. The default is to refuse and name it.
        #[arg(long)]
        skip_schema_check: bool,
        /// Development mode: `debug.dump` prints. Never in production —
        /// what it prints is request data.
        #[arg(long)]
        dev: bool,
    },
    /// Generate and apply schema migrations.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Print the parsed AST. A debugging aid, not a stable format.
    Ast {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Write the next migration: diff the sources against the last
    /// snapshot and emit an up/down pair plus a new snapshot.
    ///
    /// Offline: never connects to a database.
    New {
        /// Short name, e.g. `add_region`.
        name: String,
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Migrations directory. Defaults to `<path>/migrations`.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print each operation with the declaration that caused it.
        #[arg(long)]
        explain: bool,
        /// Print the files instead of writing them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply every pending migration, in order, under an advisory lock.
    Up {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Stop after this ordinal.
        #[arg(long)]
        to: Option<u32>,
    },
    /// Roll back applied migrations, newest first.
    ///
    /// Refuses a migration whose `down` carries an `-- irreversible:`
    /// marker.
    Down {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        dir: Option<PathBuf>,
        /// How many to roll back.
        #[arg(long, default_value_t = 1)]
        count: usize,
    },
    /// What is applied, what is pending, and what has drifted.
    Status {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Compare the constraint and index names the binary expects against
    /// the ones the database holds.
    Verify {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

/// `Err` from a subcommand is almost always the *program's* fault, not the
/// tool's: a type error, a missing migration, a database that refused a
/// statement. Returning it from `main` would hand it to `Termination`,
/// which formats an `anyhow::Error` with `Debug` — and with
/// `RUST_BACKTRACE=1` in the environment that prints this CLI's own stack
/// frames under the diagnostic, which reads as a compiler crash. The
/// answer is the message and its causes; the frames belong to panics.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `{:#}` is anyhow's cause chain on one line — the
            // `with_context` layers this crate adds are the diagnosis and
            // dropping them would be worse than the frames.
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            path,
            quiet,
            parse_only,
            deny_warnings,
        } => cmd::check(path, quiet, parse_only, deny_warnings),
        Command::Fmt { path, check } => cmd::fmt(path, check),
        Command::GenSql { path, explain, out } => cmd::gen_sql(path, explain, out),
        Command::Explain {
            path,
            sql,
            function,
            route,
            analyze,
        } => cmd::explain(path, sql, function, route, analyze),
        Command::Login { token, registry } => jwc::registry::login(token, registry),
        Command::Publish {
            path,
            registry,
            dry_run,
        } => jwc::registry::publish(path, registry, dry_run),
        Command::Add {
            spec,
            path,
            registry,
        } => jwc::registry::add(spec, path, registry),
        Command::Test {
            path,
            filter,
            no_rollback,
        } => cmd::test(path, filter, no_rollback),
        Command::Lsp => jwc::lsp::run(),
        Command::Openapi { path, out, title } => cmd::openapi(path, out, title),
        Command::Lint {
            path,
            constraints,
            deny_warnings,
        } => cmd::lint(path, constraints, deny_warnings),
        Command::Routes { path } => cmd::routes(path),
        Command::Serve {
            path,
            port,
            skip_schema_check,
            dev,
        } => cmd::serve(path, port, skip_schema_check, dev),
        Command::Build {
            path,
            release,
            emit_rust,
        } => cmd::build(path, release, emit_rust),
        Command::Migrate { command } => match command {
            MigrateCommand::New {
                name,
                path,
                dir,
                explain,
                dry_run,
            } => cmd::migrate_new(path, name, dir, explain, dry_run),
            MigrateCommand::Up { path, dir, to } => cmd::migrate_up(path, dir, to),
            MigrateCommand::Down { path, dir, count } => cmd::migrate_down(path, dir, count),
            MigrateCommand::Status { path, dir } => cmd::migrate_status(path, dir),
            MigrateCommand::Verify { path } => cmd::migrate_verify(path),
        },
        Command::Ast { path } => cmd::ast(path),
    }
}

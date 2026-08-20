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
    /// Run the program.
    Serve {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 8080)]
        port: u16,
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

fn main() -> Result<()> {
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

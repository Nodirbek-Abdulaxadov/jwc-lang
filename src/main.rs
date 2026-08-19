// Same unwrap guard as the library (see src/lib.rs). Binaries are separate
// crates, so the lib's crate-level attribute does not reach them.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use jwc::{cmd, error_report, project, runner, server, templates};

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "jwc",
    version,
    about = "JWC MVP CLI",
    long_version = long_version_string()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Long-form `jwc --version` output, including target triple, build
/// profile, rustc version, and git short hash (when the build was made
/// from a checkout). Cargo's `--version` short form (`jwc 0.4.3`) still
/// works; this is the verbose long form (`jwc --version --verbose` /
/// `-V`).
fn long_version_string() -> &'static str {
    // `concat!` builds a single static string at compile time so we hand
    // clap a `&'static str` without any runtime allocation. Each env!()
    // pulls a value emitted by build.rs (or, in CARGO_PKG_VERSION's case,
    // cargo's built-in).
    concat!(
        env!("CARGO_PKG_VERSION"),
        "\nbuild target: ",
        env!("JWC_BUILD_TARGET"),
        "\nbuild profile: ",
        env!("JWC_BUILD_PROFILE"),
        "\ngit commit:   ",
        env!("JWC_GIT_HASH"),
        "\n",
        env!("JWC_RUSTC_VERSION"),
    )
}

#[derive(Subcommand)]
enum Command {
    /// Create a new JWC project folder with jwcproj.json and main.jwc
    New {
        name: String,
        /// Starter template to scaffold from. Defaults to `empty` (the
        /// original `jwc new <name>` behaviour: a minimal main.jwc + manifest).
        /// `api` lays down a CRUD REST scaffold, `auth` adds JWT + middleware,
        /// `jobs` wires up a background-queue handler. See
        /// `docs/getting-started/templates.md` for the full layout.
        #[arg(long, value_enum)]
        template: Option<TemplateKindArg>,
    },
    /// Parse and validate a .jwc schema file
    Check {
        file: PathBuf,
        /// Skip the gradual static type checker (E018/E019/E020). Use during
        /// the transition release if a legacy program trips a false positive
        /// — file an issue first, this escape hatch is temporary.
        #[arg(long = "no-typecheck", action = ArgAction::SetTrue, default_value_t = false)]
        no_typecheck: bool,
    },
    /// Generate PostgreSQL CREATE TABLE SQL from entities
    GenSql { file: PathBuf },
    /// Run a JWC program from a .jwc file or project directory (defaults to current project)
    Run {
        path: Option<PathBuf>,
        /// Enable HTTP request logging when server starts from main()->serve()
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        request_logging: bool,
        /// Skip the gradual static type checker (E018/E019/E020). Use during
        /// the transition release if a legacy program trips a false positive.
        #[arg(long = "no-typecheck", action = ArgAction::SetTrue, default_value_t = false)]
        no_typecheck: bool,
    },
    /// Validate current project sources (searches jwcproj.json upward)
    Test {
        /// Treat any lint warning surfaced during the check as an error
        /// (CI-friendly gate). Matches `jwc build --deny-warnings`.
        #[arg(long = "deny-warnings", action = ArgAction::SetTrue, default_value_t = false)]
        deny_warnings: bool,
    },
    /// Run lint checks (validation + dead-code warnings) on the current project
    Lint {
        /// Emit warnings as one JSON array on stdout instead of human-readable
        /// lines. Each entry: {"code": "WNNN", "message": "..."}. Useful for
        /// editor / CI integration.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        json: bool,
        /// Print the description for a single diagnostic code from the
        /// catalog and exit, instead of linting. Accepts `WNNN` and
        /// `ENNN`. Example: `jwc lint --explain W004`.
        #[arg(long, value_name = "CODE")]
        explain: Option<String>,
        /// Print the entire diagnostic-code catalog (both W and E codes)
        /// as a JSON array and exit. Useful for editor integrations that
        /// want to render code-aware tooltips offline.
        #[arg(long = "list-codes", action = ArgAction::SetTrue, default_value_t = false)]
        list_codes: bool,
    },
    /// Bundle the project: copies JWC runtime + launcher into bin/{debug,release}.
    ///
    /// Pass --native to produce a real AOT-compiled binary via the embedded Rust
    /// toolchain. Native compilation is being rolled out incrementally; trivial
    /// programs work today; coverage is tracked in docs/spec/roadmap-0.9.x.md Phase 4.
    #[command(alias = "bundle")]
    Build {
        #[arg(long)]
        release: bool,
        /// Compile to a real native binary instead of bundling the interpreter.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        native: bool,
        /// Dump the generated Rust source the native pipeline would compile,
        /// without running cargo. Output: bin/<profile>/<app>.generated.rs.
        /// Useful for inspecting / debugging codegen. Requires --native.
        #[arg(long = "emit-rust-source", action = ArgAction::SetTrue, default_value_t = false)]
        emit_rust_source: bool,
        /// Cross-compile to a specific Rust target triple
        /// (e.g. `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`).
        /// The host's installed rustup toolchain must already provide the
        /// target — install via `rustup target add <triple>`. Requires
        /// --native.
        #[arg(long)]
        target: Option<String>,
        /// Fail the build when any lint warning fires (W001 unused fn,
        /// W002 unused middleware, etc.). Warnings are advisory by
        /// default — turn this on in CI to keep dead code out of `main`.
        #[arg(long = "deny-warnings", action = ArgAction::SetTrue, default_value_t = false)]
        deny_warnings: bool,
        /// Skip the gradual static type checker (E018/E019/E020). Use during
        /// the transition release if a legacy program trips a false positive.
        #[arg(long = "no-typecheck", action = ArgAction::SetTrue, default_value_t = false)]
        no_typecheck: bool,
    },
    /// Manage SQL migrations for Postgres
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Add a dependency to the project.
    ///
    /// Source flags (mutually exclusive): `--path`, `--git[ + --rev]`, or
    /// just a version requirement (defaults to the configured registry).
    Add {
        /// Package name as it appears in the manifest.
        pkg: String,
        /// Semver requirement (e.g. `^1.2`, `=0.4.0`). Required for
        /// registry/git sources unless `--path` is given.
        #[arg(long)]
        version: Option<String>,
        /// Local filesystem source. Relative to the project root.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Git URL.
        #[arg(long)]
        git: Option<String>,
        /// Git revision (commit/tag).
        #[arg(long)]
        rev: Option<String>,
    },
    /// Fetch all deps from the lockfile into `~/.jwc/registry/`.
    #[command(alias = "fetch")]
    Install,
    /// Re-resolve deps (optionally just one) within their semver ranges.
    Update {
        /// Restrict the update to a single package name. Omit to update all.
        pkg: Option<String>,
    },
    /// Remove a dependency from the manifest and lockfile.
    Remove { pkg: String },
    /// Print the resolved dependency tree.
    Tree,
    /// Front-end for the redesigned 1.0 language (docs/spec/v1/).
    ///
    /// The 1.0 vocabulary (`table` / `database` / `view` / `service`) is a
    /// different language from the one the other subcommands compile. It
    /// lives behind `jwc v1 …` until the v0.25.0 cutover, when it becomes
    /// the default and the old front-end is removed.
    V1 {
        #[command(subcommand)]
        cmd: V1Command,
    },
    /// Store a registry API key in `~/.jwc/credentials.json`.
    ///
    /// Generate the key at <https://registry-jwc.1kb.uz/#/keys> after
    /// signing in with Google. Required by `jwc publish`.
    Login {
        /// API key (`jwc_...`) issued by the registry.
        #[arg(long)]
        token: String,
        /// Override the registry base URL (default: registry-jwc.1kb.uz).
        #[arg(long)]
        registry: Option<String>,
    },
    /// Pack the current project (type=pkg) and upload to the registry.
    ///
    /// Reads `~/.jwc/credentials.json` (set via `jwc login`). Picks the
    /// version from `pkgVersion` (preferred) or `version` in the manifest.
    Publish,
    /// Run the deprecation codemod registry against `.jwc` sources.
    ///
    /// At v0.4.7 the registry is empty — nothing has been removed yet,
    /// so the command reports "no rules" and exits clean. Future
    /// versions ship rules (see `DEPRECATION.md`) that rewrite legacy
    /// syntax / flags as they're retired. `--dry-run` prints the diff
    /// without writing.
    Upgrade {
        /// Files or directories to walk. Defaults to the current project root.
        paths: Vec<PathBuf>,
        /// Print what would change without writing.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        dry_run: bool,
    },
    /// Generate `openapi.json` (OpenAPI 3.1) from the project's routes,
    /// classes, and entities. Drop the file alongside your repo or pipe
    /// to stdout for CI tooling.
    Swagger {
        /// Dump JSON to stdout instead of writing `openapi.json`.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        stdout: bool,
    },
    /// Generate an OpenAPI 3.0 JSON spec from the current project.
    ///
    /// v1 best-effort: per-route path/query params + requestBody for
    /// POST/PUT/PATCH, 200 response (referencing the handler return type
    /// when it matches a known model), 400 when `validate body` is used,
    /// 401 when an `Auth*` middleware is attached. WebSocket and SSE
    /// routes are skipped. Use `--out` to write to a file, `--pretty` to
    /// indent the JSON.
    Openapi {
        /// Project directory (defaults to the current directory).
        path: Option<PathBuf>,
        /// Output file. When omitted, the JSON is printed to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Pretty-print the JSON with indentation.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        pretty: bool,
    },
    /// Start a real HTTP server for a JWC project
    Serve {
        /// Project directory or jwcproj.json (defaults to current dir)
        path: Option<PathBuf>,
        /// Port to listen on (default: 8080)
        #[arg(long, short, default_value_t = 8080)]
        port: u16,
        /// Enable HTTP request logging
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        request_logging: bool,
        /// Watch .jwc files and restart the server on change
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        watch: bool,
    },
    /// Normalise and canonicalise `.jwc` source files.
    ///
    /// The formatter has two tiers: an AST round-trip renderer (used when
    /// the source is comment-free and parses cleanly) that emits canonical
    /// output, and a line-based normaliser (tabs → 4 spaces, strip
    /// trailing whitespace, collapse runs of 3+ blank lines, single
    /// trailing newline) used when comments are present or parsing fails.
    /// Both tiers are idempotent.
    Fmt {
        /// One or more files or directories to format. Defaults to the
        /// current directory; directories are walked recursively, skipping
        /// `.jwc-build`, `target`, `node_modules`, and `.git`.
        paths: Vec<PathBuf>,
        /// Do not write changes — exit non-zero if any file would be
        /// rewritten. Suitable for CI.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        check: bool,
        /// Write the formatted result to stdout instead of rewriting each
        /// file. Ignored when `--check` is also set (check wins).
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        stdout: bool,
    },
}

/// Clap value-enum mirror of [`templates::TemplateKind`]. Kept in a
/// separate type so the CLI surface owns its own derive (and so a
/// downstream rename in the library doesn't accidentally break the
/// stable CLI spelling).
#[derive(Clone, Copy, Debug, ValueEnum)]
enum TemplateKindArg {
    /// Minimal scaffold — same as omitting `--template`.
    Empty,
    /// CRUD REST API with one entity + migrations.
    Api,
    /// JWT auth with a middleware-protected `/me` route.
    Auth,
    /// Background-queue producer/handler scaffold.
    Jobs,
}

impl From<TemplateKindArg> for templates::TemplateKind {
    fn from(value: TemplateKindArg) -> Self {
        match value {
            TemplateKindArg::Empty => templates::TemplateKind::Empty,
            TemplateKindArg::Api => templates::TemplateKind::Api,
            TemplateKindArg::Auth => templates::TemplateKind::Auth,
            TemplateKindArg::Jobs => templates::TemplateKind::Jobs,
        }
    }
}

#[derive(Subcommand)]
enum V1Command {
    /// Parse the 1.0 sources under a path and report diagnostics.
    ///
    /// Parse-only today: name resolution lands in v0.23.0 and the runtime
    /// in v0.24.0.
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
    /// Rewrite 1.0 sources in canonical form.
    Fmt {
        /// File or directory. Defaults to the current directory.
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
        /// File or directory. Defaults to the current directory.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Prefix each statement with the `file:line` that produced it.
        #[arg(long)]
        explain: bool,
        /// Write to a file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
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
    /// Dump the parse tree of one file.
    Ast { path: PathBuf },
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Create new migration files
    #[command(alias = "add")]
    New { name: String },
    /// Apply pending migrations to Postgres
    #[command(alias = "apply")]
    Up {
        #[arg(long)]
        database_url: Option<String>,
        /// Print the SQL that would run without touching the database.
        /// Stored-checksum verification still happens — a tampered
        /// migration is reported even under --dry-run.
        #[arg(long)]
        dry_run: bool,
    },
    /// Rollback the most recent applied migration(s)
    Down {
        /// Number of migrations to roll back (default 1)
        #[arg(long, short, default_value_t = 1)]
        steps: usize,
        #[arg(long)]
        database_url: Option<String>,
        /// Print the rollback SQL without executing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// List every migration file in the project's `migrations/` dir
    /// (chronological order). Offline — does not touch the database.
    List,
    /// Show the applied / pending / sha-mismatch matrix against the
    /// database referenced by `--database-url` (or DATABASE_URL).
    Status {
        #[arg(long)]
        database_url: Option<String>,
    },
}

fn main() {
    let run_result = std::panic::catch_unwind(real_main);

    match run_result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            error_report::print_cli_error(&err);
            std::process::exit(1);
        }
        Err(panic_payload) => {
            let message = if let Some(msg) = panic_payload.downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = panic_payload.downcast_ref::<String>() {
                msg.clone()
            } else {
                "Unknown panic payload".to_string()
            };
            eprintln!("\nUnhandled panic: {message}");
            eprintln!("Tip: set RUST_BACKTRACE=1 to include panic backtrace details.");
            std::process::exit(101);
        }
    }
}

fn real_main() -> Result<()> {
    // The runner and migration engine are async (tokio_postgres under the
    // hood). The CLI itself stays synchronous so `server::serve` can keep
    // owning its own multi-threaded runtime; we only need a small
    // current-thread runtime for the handful of awaited calls below.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build tokio runtime: {e}"))?;

    if try_run_embedded_app(&rt)? {
        return Ok(());
    }

    let cli = Cli::parse();

    match cli.command {
        Command::New { name, template } => {
            // `Empty` (or no `--template`) preserves the byte-for-byte
            // legacy behaviour of `project::create_new_project`. Anything
            // else routes through the embedded-template materializer.
            let kind = template
                .map(templates::TemplateKind::from)
                .unwrap_or(templates::TemplateKind::Empty);
            cmd::check::new_project(&PathBuf::from(&name), &name, kind)?;
        }
        Command::Check { file, no_typecheck } => cmd::check::check(&file, !no_typecheck)?,
        Command::GenSql { file } => cmd::check::gen_sql(&file)?,
        Command::Run {
            path,
            request_logging,
            no_typecheck,
        } => cmd::run::run(&rt, path, request_logging, !no_typecheck)?,
        Command::Test { deny_warnings } => cmd::check::test(deny_warnings)?,
        Command::Lint {
            json,
            explain,
            list_codes,
        } => {
            if let Some(code) = explain {
                cmd::lint::explain(&code)?;
            } else if list_codes {
                cmd::lint::list_codes()?;
            } else {
                cmd::lint::run(json)?;
            }
        }
        Command::Build {
            release,
            native,
            emit_rust_source,
            target,
            deny_warnings,
            no_typecheck,
        } => cmd::build::run(
            release,
            native,
            emit_rust_source,
            target,
            deny_warnings,
            !no_typecheck,
        )?,
        Command::Migrate { command } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            project::load_dotenv(&root);

            match command {
                MigrateCommand::New { name } => cmd::migrate::new(&root, &name)?,
                MigrateCommand::Up {
                    database_url,
                    dry_run,
                } => rt.block_on(cmd::migrate::up(&root, database_url, dry_run))?,
                MigrateCommand::Down {
                    steps,
                    database_url,
                    dry_run,
                } => rt.block_on(cmd::migrate::down(&root, database_url, steps, dry_run))?,
                MigrateCommand::List => cmd::migrate::list(&root)?,
                MigrateCommand::Status { database_url } => {
                    rt.block_on(cmd::migrate::status(&root, database_url))?
                }
            }
        }
        Command::Add {
            pkg,
            version,
            path,
            git,
            rev,
        } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            cmd::pkg::add(
                &root,
                &pkg,
                version.as_deref(),
                path.as_deref(),
                git.as_deref(),
                rev.as_deref(),
            )?;
        }
        Command::Install => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            cmd::pkg::install(&root)?;
        }
        Command::Update { pkg } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            cmd::pkg::update(&root, pkg.as_deref())?;
        }
        Command::Remove { pkg } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            cmd::pkg::remove(&root, &pkg)?;
        }
        Command::Tree => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            cmd::pkg::tree(&root)?;
        }
        Command::V1 { cmd } => match cmd {
            V1Command::Check {
                path,
                quiet,
                parse_only,
            } => cmd::v1::check(path, quiet, parse_only)?,
            V1Command::Fmt { path, check } => cmd::v1::fmt(path, check)?,
            V1Command::GenSql { path, explain, out } => cmd::v1::gen_sql(path, explain, out)?,
            V1Command::Routes { path } => cmd::v1::routes(path)?,
            V1Command::Serve { path, port } => cmd::v1::serve(path, port)?,
            V1Command::Ast { path } => cmd::v1::ast(path)?,
        },
        Command::Login { token, registry } => cmd::publish::login(&token, registry.as_deref())?,
        Command::Publish => rt.block_on(cmd::publish::publish())?,
        Command::Upgrade { paths, dry_run } => cmd::upgrade::upgrade(paths, dry_run)?,
        Command::Swagger { stdout } => cmd::swagger::run(stdout)?,
        Command::Openapi { path, out, pretty } => cmd::openapi::run(path, out, pretty)?,
        Command::Fmt {
            paths,
            check,
            stdout,
        } => cmd::fmt::run(paths, check, stdout)?,
        Command::Serve {
            path,
            port,
            request_logging,
            watch,
        } => cmd::serve::run(path, port, request_logging, watch)?,
    }

    Ok(())
}

fn try_run_embedded_app(rt: &tokio::runtime::Runtime) -> Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() > 1 {
        return Ok(false);
    }

    let exe = std::env::current_exe()?;
    let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) else {
        return Ok(false);
    };

    // Only treat non-CLI app launchers as embedded apps.
    if stem.eq_ignore_ascii_case("jwc") {
        return Ok(false);
    }

    let meta_path = exe.with_file_name(format!("{stem}.jwcroot"));
    let root = if meta_path.is_file() {
        let root_str = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("Failed to read {}", meta_path.display()))?;
        PathBuf::from(root_str.trim())
    } else {
        let exe_dir = exe
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid executable path"))?
            .to_path_buf();
        project::find_project_root(&exe_dir)?
    };

    if !root.is_dir() {
        anyhow::bail!("Embedded app root does not exist: {}", root.display());
    }

    project::load_dotenv(&root);
    let loaded = project::load_project_from_root(&root)?;
    loaded.manifest.ensure_runnable()?;
    let result = rt.block_on(runner::run_main(&loaded.program))?;
    if !result.output.is_empty() {
        print!("{}", result.output);
    }
    if let Some(port) = result.serve_port {
        server::serve(&loaded.program, port, false)?;
    }

    Ok(true)
}

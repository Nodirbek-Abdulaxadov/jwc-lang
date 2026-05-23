use jwc::{cmd, error_report, native_build, parser, project, runner, server};

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "jwc", version, about = "JWC MVP CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new JWC project folder with jwcproj.json and main.jwc
    New { name: String },
    /// Parse and validate a .jwc schema file
    Check { file: PathBuf },
    /// Generate PostgreSQL CREATE TABLE SQL from entities
    GenSql { file: PathBuf },
    /// Run a JWC program from a .jwc file or project directory (defaults to current project)
    Run {
        path: Option<PathBuf>,
        /// Enable HTTP request logging when server starts from main()->serve()
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        request_logging: bool,
    },
    /// Validate current project sources (searches jwcproj.json upward)
    Test,
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
    /// programs work today, full coverage tracks Phase 4 in ROADMAP.md.
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
    /// Normalise whitespace in `.jwc` source files.
    ///
    /// v1 is a line-based formatter (tabs → 4 spaces, strip trailing
    /// whitespace, collapse runs of 3+ blank lines, single trailing
    /// newline). A token-stream-aware AST → source renderer is tracked in
    /// ROADMAP Phase 3.3.
    Fmt {
        /// File or directory to format. Defaults to the current directory;
        /// directories are walked recursively, skipping `.jwc-build`,
        /// `target`, `node_modules`, and `.git`.
        path: Option<PathBuf>,
        /// Do not write changes — exit non-zero if any file would be
        /// rewritten. Suitable for CI.
        #[arg(long, action = ArgAction::SetTrue, default_value_t = false)]
        check: bool,
    },
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
    },
    /// Rollback the most recent applied migration(s)
    Down {
        /// Number of migrations to roll back (default 1)
        #[arg(long, short, default_value_t = 1)]
        steps: usize,
        #[arg(long)]
        database_url: Option<String>,
    },
    /// List every migration file in the project's `migrations/` dir
    /// (chronological order). Offline — does not touch the database.
    List,
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
        Command::New { name } => cmd::check::new_project(&PathBuf::from(name))?,
        Command::Check { file } => cmd::check::check(&file)?,
        Command::GenSql { file } => cmd::check::gen_sql(&file)?,
        Command::Run {
            path,
            request_logging,
        } => {
            let target = path.unwrap_or(std::env::current_dir()?);

            if target.is_dir() {
                let root = project::find_project_root(&target)?;
                project::load_dotenv(&root);
                let loaded = project::load_project_from_root(&root)?;
                loaded.manifest.ensure_runnable()?;
                let _ = build_project_native_artifact(&root, &loaded.manifest.name, false)?;
                let result = rt.block_on(runner::run_main(&loaded.program))?;
                if !result.output.is_empty() {
                    print!("{}", result.output);
                }
                if let Some(port) = result.serve_port {
                    server::serve(&loaded.program, port, request_logging)?;
                }
            } else if target
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case(project::PROJECT_FILE))
                .unwrap_or(false)
            {
                let root = target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Invalid project file path"))?
                    .to_path_buf();
                project::load_dotenv(&root);
                let loaded = project::load_project_from_root(&root)?;
                loaded.manifest.ensure_runnable()?;
                let _ = build_project_native_artifact(&root, &loaded.manifest.name, false)?;
                let result = rt.block_on(runner::run_main(&loaded.program))?;
                if !result.output.is_empty() {
                    print!("{}", result.output);
                }
                if let Some(port) = result.serve_port {
                    server::serve(&loaded.program, port, request_logging)?;
                }
            } else {
                let source = read_source(&target)?;
                let program = parser::parse_program(&source)
                    .with_context(|| format!("Failed to parse {}", target.display()))?;
                parser::validate_program(&program)
                    .with_context(|| format!("Validation failed for {}", target.display()))?;
                let result = rt.block_on(runner::run_main(&program))?;
                if !result.output.is_empty() {
                    print!("{}", result.output);
                }
                if let Some(port) = result.serve_port {
                    server::serve(&program, port, request_logging)?;
                }
            }
        }
        Command::Test => cmd::check::test()?,
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
        } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            let loaded = project::load_project_from_root(&root)?;
            loaded.manifest.ensure_runnable()?;
            let profile = if release { "release" } else { "debug" };

            if emit_rust_source && !native {
                anyhow::bail!("--emit-rust-source requires --native");
            }
            if target.is_some() && !native {
                anyhow::bail!("--target requires --native");
            }

            if native {
                let app_name = sanitize_app_name(&loaded.manifest.name);
                if emit_rust_source {
                    let out =
                        native_build::emit_rust_source(&loaded.program, &root, &app_name, release)?;
                    println!("Emitted generated Rust source ({profile})");
                    println!("Project: {}", loaded.manifest.name);
                    println!("Source:  {}", out.display());
                    return Ok(());
                }
                let report = native_build::compile_with_target(
                    &loaded.program,
                    &root,
                    &app_name,
                    release,
                    target.as_deref(),
                )?;
                println!("Native build complete ({profile})");
                if let Some(t) = target.as_deref() {
                    println!("Target: {t}");
                }
                println!("Project: {}", loaded.manifest.name);
                println!("Binary:  {}", report.binary_path.display());
                println!("Workspace: {}", report.workspace.display());
            } else {
                let out_path =
                    build_project_native_artifact(&root, &loaded.manifest.name, release)?;
                println!("Bundled runtime + launcher ({profile})");
                println!("Project: {}", loaded.manifest.name);
                println!("Launcher: {}", out_path.display());
                println!("Note: this bundles the JWC runtime alongside your project.");
                println!(
                    "      For real AOT-compiled binaries, pass --native (Phase 4 — incremental)."
                );
            }
        }
        Command::Migrate { command } => {
            let cwd = std::env::current_dir()?;
            let root = project::find_project_root(&cwd)?;
            project::load_dotenv(&root);

            match command {
                MigrateCommand::New { name } => cmd::migrate::new(&root, &name)?,
                MigrateCommand::Up { database_url } => {
                    rt.block_on(cmd::migrate::up(&root, database_url))?
                }
                MigrateCommand::Down {
                    steps,
                    database_url,
                } => rt.block_on(cmd::migrate::down(&root, database_url, steps))?,
                MigrateCommand::List => cmd::migrate::list(&root)?,
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
        Command::Fmt { path, check } => {
            let target =
                path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let files = jwc::fmt::collect_jwc_files(&target).with_context(|| {
                format!("Failed to enumerate .jwc files under {}", target.display())
            })?;
            if files.is_empty() {
                eprintln!("No .jwc files found under {}", target.display());
                return Ok(());
            }
            let mut changed: Vec<PathBuf> = Vec::new();
            for file in &files {
                let outcome = jwc::fmt::format_file(file, check)
                    .with_context(|| format!("Failed to format {}", file.display()))?;
                if matches!(outcome, jwc::fmt::FormatOutcome::Changed) {
                    changed.push(file.clone());
                }
            }
            if check {
                if !changed.is_empty() {
                    eprintln!(
                        "jwc fmt --check: {} file(s) would be rewritten:",
                        changed.len()
                    );
                    for f in &changed {
                        eprintln!("  {}", f.display());
                    }
                    std::process::exit(1);
                } else {
                    println!("jwc fmt --check: {} file(s) already formatted", files.len());
                }
            } else {
                println!("jwc fmt: rewrote {}/{} file(s)", changed.len(), files.len());
            }
        }
        Command::Serve {
            path,
            port,
            request_logging,
            watch,
        } => {
            let target = path.clone().unwrap_or(std::env::current_dir()?);
            let root = if target.is_dir() {
                project::find_project_root(&target)?
            } else {
                target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("Invalid project path"))?
                    .to_path_buf()
            };

            if watch {
                run_serve_with_watch(&root, port, request_logging)?;
            } else {
                project::load_dotenv(&root);
                let loaded = project::load_project_from_root(&root)?;
                loaded.manifest.ensure_runnable()?;
                server::serve(&loaded.program, port, request_logging)?;
            }
        }
    }

    Ok(())
}

fn run_serve_with_watch(root: &PathBuf, port: u16, request_logging: bool) -> Result<()> {
    use notify::{event::EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::process::Command as SysCommand;
    use std::sync::mpsc;
    use std::time::Duration;

    let exe = std::env::current_exe()?;
    let (tx, rx) = mpsc::channel::<notify::Event>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })?;
    watcher.watch(root.as_path(), RecursiveMode::Recursive)?;

    println!("[watch] Watching {} for .jwc changes", root.display());

    loop {
        let mut cmd = SysCommand::new(&exe);
        cmd.arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg(root);
        if request_logging {
            cmd.arg("--request-logging");
        }
        let mut child = cmd
            .spawn()
            .with_context(|| "watch: failed to spawn child")?;
        println!("[watch] Server started (pid {})", child.id());

        // Drain any backlog so the first event after spawn isn't a stale one.
        while rx.try_recv().is_ok() {}

        // Wait until a .jwc change arrives.
        loop {
            let Ok(event) = rx.recv() else {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                continue;
            }
            if event.paths.iter().any(|p| is_jwc_path(p)) {
                break;
            }
        }

        println!("[watch] Change detected, restarting server...");
        let _ = child.kill();
        let _ = child.wait();

        // Debounce: drain rapid-fire follow-up events for ~250 ms.
        std::thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}
    }
}

fn is_jwc_path(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jwc"))
        .unwrap_or(false)
}

fn read_source(path: &std::path::Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

fn sanitize_app_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        }
    }
    if out.is_empty() {
        "app".to_string()
    } else {
        out
    }
}

#[cfg(not(windows))]
fn build_launcher_script() -> String {
    r#"#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SELF_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "$SELF_DIR/jwc-runtime" run "$ROOT_DIR" "$@"
"#
    .to_string()
}

fn build_project_native_artifact(
    root: &std::path::Path,
    manifest_name: &str,
    release: bool,
) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    let bin_dir = root.join("bin").join(profile);
    std::fs::create_dir_all(&bin_dir)?;

    let app_name = sanitize_app_name(manifest_name);
    let runtime_src = std::env::current_exe()?;

    #[cfg(windows)]
    {
        let out_path = bin_dir.join(format!("{app_name}.exe"));
        std::fs::copy(&runtime_src, &out_path).with_context(|| {
            format!(
                "Failed to copy runtime from {} to {}",
                runtime_src.display(),
                out_path.display()
            )
        })?;

        // Clean up legacy sidecar from older builds; executable now self-resolves project root.
        let root_meta = bin_dir.join(format!("{app_name}.jwcroot"));
        if root_meta.is_file() {
            let _ = std::fs::remove_file(&root_meta);
        }

        Ok(out_path)
    }

    #[cfg(not(windows))]
    {
        let out_path = bin_dir.join(&app_name);
        let script = build_launcher_script();
        std::fs::write(&out_path, script)?;

        let runtime_dst = bin_dir.join("jwc-runtime");
        std::fs::copy(&runtime_src, &runtime_dst).with_context(|| {
            format!(
                "Failed to copy runtime from {} to {}",
                runtime_src.display(),
                runtime_dst.display()
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&out_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&out_path, perms)?;

            let mut runtime_perms = std::fs::metadata(&runtime_dst)?.permissions();
            runtime_perms.set_mode(0o755);
            std::fs::set_permissions(&runtime_dst, runtime_perms)?;
        }

        Ok(out_path)
    }
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

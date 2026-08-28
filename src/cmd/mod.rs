//! The CLI's commands.
//!
//! These were `jwc v1 …` while the two languages coexisted. The v0.25.0
//! cutover removed the older one, so the prefix is gone and these are the
//! ordinary commands (ROADMAP §2).

use anyhow::{anyhow, bail, Result};
use jwc_v1_paths::collect_sources;
use std::path::{Path, PathBuf};

mod jwc_v1_paths {
    use std::path::{Path, PathBuf};

    /// Every `.jwc` file under `root`, or `root` itself when it is a file.
    /// Sorted, so diagnostics come out in a stable order.
    pub fn collect_sources(root: &Path) -> std::io::Result<Vec<PathBuf>> {
        if root.is_file() {
            return Ok(vec![root.to_path_buf()]);
        }
        let mut out = Vec::new();
        walk(root, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if p.is_dir() {
                walk(&p, out)?;
            } else if p.extension().and_then(|s| s.to_str()) == Some("jwc") {
                out.push(p);
            }
        }
        Ok(())
    }
}

/// `jwc v1 check <path>` — parse, resolve the schema, and type-check.
///
/// `--parse-only` stops after the front-end, which is what the parse corpus
/// exercises. The full pass adds the schema model (schema.md §11) and the
/// type checker (types.md, queries.md, writes.md).
/// The tokio runtime every subcommand blocks on.
///
/// `Runtime::new()` sizes the pool from `available_parallelism()`, which
/// on Linux reads the cgroup CPU limit — so a container capped at 2 cores
/// gets 2 threads, not the host's 96. That default is right, and
/// `JWC_SERVER_WORKERS` is for the cases it is not: a pinned thread count
/// for a benchmark, or 1 to make a scheduling bug reproducible.
///
/// The variable was registered, documented and printed in the boot table
/// since the registry was written, and read by nothing.
/// Stack per worker thread.
///
/// tokio's default is 2 MiB, and a JWC call frame is a chain of boxed
/// futures whose poll costs the whole chain's depth — so on the default a
/// recursion **18 deep** ran and **20 deep** printed `fatal runtime error:
/// stack overflow, aborting`, which is a process abort: every other
/// request on that server dies with it, from one request.
///
/// `exec::MAX_CALL_DEPTH` is the ceiling a program is supposed to hit
/// instead. This is what makes that number reachable rather than
/// aspirational. 64 MiB is address space, not memory — pages are committed
/// as they are touched, so a server that never recurses pays nothing for
/// it.
pub const WORKER_STACK_BYTES: usize = 64 * 1024 * 1024;

fn runtime() -> Result<tokio::runtime::Runtime> {
    let n = std::env::var("JWC_SERVER_WORKERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0);
    let mut b = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = n {
        b.worker_threads(n);
    }
    Ok(b.thread_stack_size(WORKER_STACK_BYTES)
        .enable_all()
        .build()?)
}

/// Run `f` on a worker thread rather than the caller's.
///
/// `block_on` polls the future on the *calling* thread, which for a CLI is
/// the process's main thread and whatever stack the linker gave it — 8 MiB
/// here, and not something this program chose. `jwc run` on a recursion
/// 100 deep aborted there for the same reason `serve` did on a worker.
/// Spawning first puts the work on a thread whose stack is
/// `WORKER_STACK_BYTES`, so `run` and `serve` have the same ceiling.
fn on_worker<F>(rt: &tokio::runtime::Runtime, f: F) -> Result<()>
where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    rt.block_on(async move { rt_join(tokio::spawn(f).await) })
}

fn rt_join(j: std::result::Result<Result<()>, tokio::task::JoinError>) -> Result<()> {
    match j {
        Ok(r) => r,
        Err(e) if e.is_panic() => bail!("the program panicked: {e}"),
        Err(e) => bail!("{e}"),
    }
}

pub fn check(path: PathBuf, quiet: bool, parse_only: bool, deny_warnings: bool) -> Result<()> {
    use crate::diag::Severity;

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }

    let mut errors = 0usize;
    let mut warnings = 0usize;
    for f in &ws.files {
        for d in &f.diags {
            match d.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
            }
            eprint!("{}", f.source.render(d));
        }
    }
    if errors > 0 {
        bail!("{errors} parse error{}", plural(errors));
    }

    if !parse_only {
        let built = crate::model::build(&ws);
        let symbols = crate::symbols::build(&ws, &built.model);
        let checked = crate::check::check(&ws, &symbols, &built.model);
        let wired = crate::wiring::wire(&ws, &symbols);
        let mut imports = crate::imports::check(&ws, &ws.packages);
        imports.extend(crate::imports::case_convention(&ws));
        imports.extend(crate::packages::check(&ws, &symbols));
        for (loc, d) in built
            .diags
            .iter()
            .chain(&symbols.diags)
            .chain(&checked.diags)
            .chain(&wired.diags)
            .chain(&imports)
        {
            match d.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
            }
            eprint!("{}", ws.render(*loc, d));
        }
    }

    if errors > 0 {
        bail!(
            "{errors} error{} in {} file{}",
            plural(errors),
            ws.files.len(),
            plural(ws.files.len())
        );
    }
    if deny_warnings && warnings > 0 {
        bail!("{warnings} warning{} (--deny-warnings)", plural(warnings));
    }
    if !quiet {
        println!(
            "ok — {} file{} checked, {warnings} warning{}",
            ws.files.len(),
            plural(ws.files.len()),
            plural(warnings)
        );
    }
    Ok(())
}

/// `jwc v1 fmt <path> [--check]` — canonical formatting.
///
/// `--check` reports which files would change and exits non-zero without
/// writing, which is the CI shape.
/// Name the comments a re-print would lose, and where they can live instead.
fn report_lost_comments(file: &Path, lost: &[String]) {
    eprintln!(
        "{}: not formatted — {} comment{} would be lost:",
        display_relative(file),
        lost.len(),
        plural(lost.len())
    );
    for c in lost {
        eprintln!("    {}", c.trim_end());
    }
    eprintln!(
        "  the printer re-emits from the AST, which carries a comment on a \
         declaration or a statement but not inside a record literal, a \
         `server {{ }}` body or an `insert` value list. Move it above the \
         statement and the file formats."
    );
}

pub fn fmt(paths: Vec<PathBuf>, check_only: bool, to_stdout: bool) -> Result<()> {
    let paths = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths
    };

    let mut files: Vec<PathBuf> = Vec::new();
    for p in &paths {
        files.extend(collect_sources(p)?);
    }
    // Two inputs can name the same file — `jwc fmt src src/app.jwc`. Left
    // as-is that rewrites it twice and counts it twice.
    files.sort();
    files.dedup();
    if files.is_empty() {
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        bail!("no .jwc files under {}", names.join(", "));
    }

    if to_stdout {
        // One input only. Concatenating two formatted files produces
        // something that is not a program, and a caller redirecting this
        // into a file would get exactly that.
        if files.len() != 1 {
            bail!(
                "--stdout formats one file; {} matched. Name the file, or \
                 drop --stdout to rewrite them in place.",
                files.len()
            );
        }
        if check_only {
            bail!("--check and --stdout ask for different things: one reports, one prints");
        }
        let parsed = crate::parse_file(&files[0])?;
        if parsed.has_errors() {
            eprint!("{}", parsed.render_all());
            bail!("source did not parse — nothing printed");
        }
        let printed = crate::fmt::format_program(&parsed.program);
        let lost = crate::fmt::comments_lost(&parsed.source.text, &printed);
        if !lost.is_empty() {
            report_lost_comments(&files[0], &lost);
            bail!(
                "formatting would drop {} comment{}",
                lost.len(),
                plural(lost.len())
            );
        }
        print!("{printed}");
        return Ok(());
    }

    let mut changed: Vec<PathBuf> = Vec::new();
    let mut failed = 0usize;
    let mut refused = 0usize;

    for f in &files {
        let parsed = crate::parse_file(f)?;
        if parsed.has_errors() {
            eprint!("{}", parsed.render_all());
            failed += 1;
            continue;
        }
        let printed = crate::fmt::format_program(&parsed.program);
        if printed == parsed.source.text {
            continue;
        }
        // Never trade a comment for a layout. The printer re-emits from the
        // AST, and the AST does not carry a comment written between the
        // fields of a record literal or the keys of `server { }` — so
        // formatting such a file used to delete the paragraph explaining
        // why the code is the way it is, with nothing said. Refusing the
        // file loses nothing and says which comment is in the way.
        let lost = crate::fmt::comments_lost(&parsed.source.text, &printed);
        if !lost.is_empty() {
            report_lost_comments(f, &lost);
            refused += 1;
            continue;
        }
        changed.push(f.clone());
        if !check_only {
            std::fs::write(f, &printed)?;
        }
    }

    // Two different failures, and reporting them as one told the reader the
    // wrong thing to go and look at.
    if failed > 0 && refused > 0 {
        bail!(
            "{failed} file{} did not parse, {refused} would lose a comment",
            plural(failed)
        );
    }
    if failed > 0 {
        bail!("{failed} file{} did not parse", plural(failed));
    }
    if refused > 0 {
        bail!(
            "{refused} file{} left alone rather than lose a comment",
            plural(refused)
        );
    }

    if check_only {
        if changed.is_empty() {
            println!("ok — {} file{} formatted", files.len(), plural(files.len()));
            return Ok(());
        }
        for c in &changed {
            println!("would reformat {}", display_relative(c));
        }
        bail!(
            "{} file{} need formatting",
            changed.len(),
            plural(changed.len())
        );
    }

    for c in &changed {
        println!("formatted {}", display_relative(c));
    }
    if changed.is_empty() {
        println!(
            "ok — {} file{} already formatted",
            files.len(),
            plural(files.len())
        );
    }
    Ok(())
}

/// `jwc v1 gen-sql <path>` — the schema as DDL.
///
/// Offline and deterministic: two runs on the same source are byte-identical
/// (schema.md §9). `--explain` prefixes each statement with the declaration
/// that caused it, which is the artefact the DBA test is read against.
pub fn gen_sql(path: PathBuf, explain: bool, out: Option<PathBuf>) -> Result<()> {
    use crate::diag::Severity;

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    if ws.has_parse_errors() {
        for e in ws.parse_errors() {
            eprint!("{e}");
        }
        bail!("source did not parse");
    }

    let built = crate::model::build(&ws);
    let mut errors = 0usize;
    for (loc, d) in &built.diags {
        if d.severity == Severity::Error {
            errors += 1;
        }
        eprint!("{}", ws.render(*loc, d));
    }
    if errors > 0 {
        bail!("{errors} schema error{}", plural(errors));
    }

    let statements = crate::ddl::emit(&built.model);
    let sql = crate::ddl::render(&ws, &statements, explain);
    match out {
        Some(p) => {
            std::fs::write(
                &p,
                format!(
                    "{sql}
"
                ),
            )?;
            println!("wrote {} ({} statements)", p.display(), statements.len());
        }
        None => println!("{sql}"),
    }
    Ok(())
}

/// `jwc explain [path]` — every query the program issues, with its SQL.
///
/// Offline unless `--analyze` is given. `--function` and `--route` narrow it
/// to what one entry point can reach, over the static call graph
/// (tooling.md §1).
pub fn explain(
    path: PathBuf,
    sql_only: bool,
    function: Option<String>,
    route: Option<String>,
    analyze: bool,
) -> Result<()> {
    use crate::diag::Severity;

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    if ws.has_parse_errors() {
        for e in ws.parse_errors() {
            eprint!("{e}");
        }
        bail!("source did not parse");
    }
    let built = crate::model::build(&ws);
    let sym = crate::symbols::build(&ws, &built.model);

    // Which owners to print. `None` is everything.
    let mut wanted = select_owners(&ws, &sym, function.as_deref(), route.as_deref())?;
    if let Some(w) = &mut wanted {
        expand_views(&ws, &sym, w);
    }
    let wanted = wanted;
    let show = |owner: &str| -> bool {
        wanted
            .as_ref()
            .is_none_or(|w| w.contains(&owner_key(owner)))
    };

    let mut queries = 0usize;
    let mut gaps = 0usize;
    let mut hatches = 0usize;

    if wanted.is_none() {
        for file in &ws.files {
            // writes.md §6.4 — the valve's usage count is the measurement of
            // which feature to add next, so it is printed rather than
            // assumed to be zero.
            for (i, line) in file.source.text.lines().enumerate() {
                if line.contains("raw(") && !line.trim_start().starts_with("--") {
                    hatches += 1;
                    println!(
                        "\x1b[1m{}:{}\x1b[0m  raw() — hand-written SQL, unchecked shape",
                        file.source.path.display(),
                        i + 1
                    );
                    println!("  {}\n", line.trim());
                }
            }
        }
    }

    let mut statements: Vec<String> = Vec::new();
    for file in &ws.files {
        for site in crate::query_sql::sites(&file.program) {
            if !show(&site.owner) {
                continue;
            }
            queries += 1;
            let (line, _) = file.source.line_col(site.select.span.start);
            println!(
                "\x1b[1m{}:{line}\x1b[0m  {}",
                file.source.path.display(),
                site.label
            );
            let plan = crate::query::plan(site.select, &sym);
            if let Some(d) = plan.diags.iter().find(|d| d.severity == Severity::Error) {
                println!("  rejected: {} {}", d.code, d.message);
                gaps += 1;
                continue;
            }
            if !sql_only {
                println!(
                    "  {}",
                    crate::query_sql::raw_state(&built.model, site.select, &plan)
                );
            }
            let mut c = crate::query_sql::Compiler::new(&built.model);
            match c.compile(site.select, &plan) {
                Some(compiled) => {
                    for line in compiled.sql.lines() {
                        println!("  {line}");
                    }
                    statements.push(compiled.sql.clone());
                }
                None => {
                    println!("  not compilable: {}", c.gap());
                    gaps += 1;
                }
            }
            println!();
        }
    }

    if analyze {
        analyze_statements(&statements)?;
    }

    println!("{queries} quer{}", if queries == 1 { "y" } else { "ies" });
    if gaps > 0 {
        println!("{gaps} not compiled");
    }
    if hatches > 0 {
        println!(
            "{hatches} raw() escape hatch{}",
            if hatches == 1 { "" } else { "es" }
        );
    }
    Ok(())
}

/// A selected query that reads a view runs that view's body too, so the
/// view is part of the answer to "what SQL does this route issue".
///
/// A fixed point rather than one pass: a view may select from a view
/// (queries.md §8), and the inner one is just as much part of the statement.
fn expand_views(
    ws: &crate::workspace::Workspace,
    sym: &crate::symbols::Symbols,
    wanted: &mut std::collections::BTreeSet<String>,
) {
    loop {
        let mut grew = false;
        for file in &ws.files {
            for site in crate::query_sql::sites(&file.program) {
                if !wanted.contains(&owner_key(&site.owner)) {
                    continue;
                }
                let plan = crate::query::plan(site.select, sym);
                let mut objects = Vec::new();
                plan.root.walk(&mut objects);
                let names: Vec<String> = objects
                    .iter()
                    .map(|n| n.object.clone())
                    .chain(plan.groups.iter().map(|g| g.object.clone()))
                    .collect();
                for name in names {
                    if sym.views.contains_key(&name) && wanted.insert(format!("view {name}")) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
}

/// `function f` and `f` are the same owner; everything else is its own
/// label. Call sites write the bare name, `sites()` writes the readable one.
fn owner_key(owner: &str) -> String {
    owner.strip_prefix("function ").unwrap_or(owner).to_string()
}

/// The owner keys `--function` / `--route` select, or `None` for all of them.
fn select_owners(
    ws: &crate::workspace::Workspace,
    sym: &crate::symbols::Symbols,
    function: Option<&str>,
    route: Option<&str>,
) -> Result<Option<std::collections::BTreeSet<String>>> {
    let _ = sym;
    if function.is_none() && route.is_none() {
        return Ok(None);
    }
    let bodies = crate::wiring::function_bodies(ws);
    let mut keys: std::collections::BTreeSet<String> = Default::default();

    if let Some(name) = function {
        let Some(body) = bodies.get(name) else {
            bail!(
                "no function `{name}`. This program declares:\n  {}",
                bodies.keys().cloned().collect::<Vec<_>>().join("\n  ")
            );
        };
        keys.insert(name.to_string());
        keys.extend(crate::wiring::reachable_from(&bodies, body));
    }

    if let Some(spec) = route {
        // `GET /api/v1/orgs/{org_id}` — the declared pattern, the same
        // string `request.route()` returns (routing.md §5.4).
        let (method, pattern) = spec
            .split_once(char::is_whitespace)
            .map(|(m, p)| (m.trim().to_uppercase(), p.trim().to_string()))
            .unwrap_or_else(|| ("GET".to_string(), spec.trim().to_string()));

        let mut found = false;
        for file in &ws.files {
            for d in &file.program.decls {
                let crate::ast::Decl::Routes(r) = d else {
                    continue;
                };
                for rt in &r.routes {
                    let full = crate::wiring::route_pattern(&r.prefix, &rt.suffix);
                    if rt.method.name.to_uppercase() != method || full != pattern {
                        continue;
                    }
                    found = true;
                    keys.insert(format!("route {method} {full}"));
                    keys.extend(crate::wiring::reachable_from(&bodies, &rt.body));
                }
            }
        }
        if !found {
            let built = crate::model::build(ws);
            let symbols = crate::symbols::build(ws, &built.model);
            let wired = crate::wiring::wire(ws, &symbols);
            let mut have: Vec<String> = wired
                .routes
                .iter()
                .map(|r| format!("{} {}", r.method, r.pattern))
                .collect();
            have.sort();
            bail!(
                "no route `{method} {pattern}`. This program serves:\n  {}",
                have.join("\n  ")
            );
        }
    }

    Ok(Some(keys))
}

/// `EXPLAIN` each statement against `DATABASE_URL` (tooling.md §1.4).
///
/// Parameters are bound as `NULL`. The plan *shape* is what is being read —
/// which index, which join — and binding a made-up value would give row
/// estimates for a row that does not exist.
fn analyze_statements(statements: &[String]) -> Result<()> {
    if statements.is_empty() {
        return Ok(());
    }
    let url = crate::engine::database_url_from_env()?;
    let rt = runtime()?;
    rt.block_on(async move {
        let client = crate::engine::connect_for_migrations(&url).await?;
        for sql in statements {
            println!("\x1b[1mEXPLAIN\x1b[0m");
            let n = highest_parameter(sql);
            let params: Vec<Option<String>> = vec![None; n];
            let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
                .iter()
                .map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync))
                .collect();
            match client.query(&format!("EXPLAIN {sql}"), &refs).await {
                Ok(rows) => {
                    for row in rows {
                        println!("  {}", row.get::<_, String>(0));
                    }
                }
                Err(e) => println!("  could not plan: {e}"),
            }
            println!();
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// The largest `$n` in a statement, which is how many parameters to bind.
fn highest_parameter(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut max = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                if let Ok(n) = sql[i + 1..j].parse::<usize>() {
                    max = max.max(n);
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    max
}

/// `jwc v1 routes <path>` — the resolved route table.
///
/// This is the artefact E0710 (duplicate route) and E0803 (unsatisfied
/// `requires`) are read against: method, path, and the middleware chain in
/// execution order (routing.md §8.2).
pub fn routes(path: PathBuf) -> Result<()> {
    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.has_parse_errors() {
        for e in ws.parse_errors() {
            eprint!("{e}");
        }
        bail!("source did not parse");
    }
    let built = crate::model::build(&ws);
    let symbols = crate::symbols::build(&ws, &built.model);
    let wired = crate::wiring::wire(&ws, &symbols);

    let mut rows: Vec<_> = wired.routes.iter().collect();
    rows.sort_by(|a, b| (&a.pattern, &a.method).cmp(&(&b.pattern, &b.method)));

    let width = rows.iter().map(|r| r.pattern.len()).max().unwrap_or(4);
    for r in &rows {
        let chain = if r.chain.is_empty() {
            "-".to_string()
        } else {
            r.chain.join(" → ")
        };
        // The upgrade is a GET on the wire, which is why the duplicate
        // check compares it as one — but printing `GET` here would hide
        // what the endpoint is.
        let method = if r.socket { "WS" } else { r.method.as_str() };
        println!("{method:<7} {:<width$}  {chain}", r.pattern, width = width);
        if !r.after.is_empty() {
            println!(
                "{:<7} {:<width$}  after: {}",
                "",
                "",
                r.after.join(" → "),
                width = width
            );
        }
    }
    println!("\n{} route{}", rows.len(), plural(rows.len()));
    Ok(())
}

/// `jwc v1 serve <path> --port N` — run the program.
/// `jwc run [path]` — run `main()` and exit.
///
/// Restored in 0.9.920. The v0.25.0 cutover deleted `jwc run` with the
/// rest of the 0.9 front-end, and nothing replaced it: `jwc serve` was the
/// only way to execute a program, so the smallest thing anyone writes
/// first — print a line — started an HTTP listener, and needed a Postgres
/// to do it.
///
/// A program with no `database` connects to nothing, exactly as in
/// `serve`. One that queries still needs `DATABASE_URL`, because the query
/// does.
pub fn run(path: PathBuf, dev: bool, request_logging: bool) -> Result<()> {
    crate::serve::set_request_logging(request_logging);
    let ws = crate::workspace::Workspace::load(&path)?;
    let program = std::sync::Arc::new(crate::serve::load(&ws)?);

    let Some(_) = program.functions.get("main") else {
        bail!(
            "nothing to run: this program declares no `main`.\n\
             \n\
             `jwc run` calls `main()`. Add one:\n\
             \n  \
             function main() {{\n      \
             console.writeln(\"hello\");\n  \
             }}\n\
             \n\
             A program that is only routes is started with `jwc serve`."
        );
    };

    crate::exec::set_dev_mode(dev);

    let needs_db = ws.files.iter().any(|f| {
        f.program
            .decls
            .iter()
            .any(|d| matches!(d, crate::ast::Decl::Database(_)))
    });

    let rt = runtime()?;
    // On a worker, not on the main thread: `block_on` polls on the caller,
    // and the main thread's stack is whatever the linker chose (8 MiB
    // here), not what this program configured. A recursion 100 deep
    // aborted `jwc run` there. See `WORKER_STACK_BYTES`.
    on_worker(&rt, async move {
        if needs_db {
            crate::engine::init_engine_from_env()?;
        }
        crate::redis_engine::init_from_env()?;
        // `declared_port` runs `main`, and `serve(n)` inside it records
        // the port rather than blocking — it is a declaration, not a
        // call that listens.
        //
        // So this used to run `main`, take the port, throw it away, and
        // exit: `jwc run app.jwc` on a program whose `main` is
        // `serve(8080)` printed nothing and returned 0. The help text
        // beside this command has always said the opposite — "a `main`
        // that calls `serve(...)` still starts a server, because that is
        // what the call means" — and the comment that used to sit here
        // claimed the program "has already blocked inside it and never
        // reaches here". Neither was true, and the first thing a reader
        // does with the hello-world is `jwc run` it.
        //
        // `main` deciding to serve is the difference between the two
        // commands: `jwc run` starts a listener only when the program
        // asked for one, and `jwc serve` starts one either way.
        if let Some(port) = crate::serve::declared_port(&program).await? {
            println!("{} routes", program.routes.len());
            crate::serve::serve(program, port).await?;
        }
        Ok(())
    })
}

pub fn serve(
    path: PathBuf,
    port: Option<u16>,
    skip_schema_check: bool,
    dev: bool,
    request_logging: bool,
    watch: bool,
) -> Result<()> {
    if watch {
        return serve_with_watch(&path, port, skip_schema_check, dev, request_logging);
    }
    crate::serve::set_request_logging(request_logging);
    let ws = crate::workspace::Workspace::load(&path)?;
    let program = std::sync::Arc::new(crate::serve::load(&ws)?);
    let snap = crate::snapshot::of(&crate::model::build(&ws).model);

    crate::exec::set_dev_mode(dev);

    // `jwt.rs` calls this "the boot fence" and it was never called, so a
    // typo in `JWC_DB_POOL_SIZE=twenty` was swallowed by an
    // `unwrap_or(64)` deeper in the call graph and the pool was quietly
    // the wrong size. Before the listener, so the failure is a refusal to
    // start rather than a surprise under load.
    crate::config::validate_or_bail()?;
    // `JWC_PRINT_CONFIG=1` — every registered variable, its value, and
    // where the value came from, with secrets redacted. The renderer has
    // shipped since the registry was written with no caller.
    if matches!(std::env::var("JWC_PRINT_CONFIG").as_deref(), Ok("1")) {
        print!("{}", crate::config::render(&crate::config::snapshot()));
    }

    // A program that declares no `database` has no tables, no queries and
    // nothing to connect to, and `serve` demanded `DATABASE_URL` from it
    // anyway. The first program anyone writes is that program, so the
    // first thing JWC said to them was that it needed a Postgres to print
    // a line.
    let needs_db = ws.files.iter().any(|f| {
        f.program
            .decls
            .iter()
            .any(|d| matches!(d, crate::ast::Decl::Database(_)))
    });

    let rt = runtime()?;
    rt.block_on(async move {
        if needs_db {
            crate::engine::init_engine_from_env()?;
        }
        // The `redis` package's surface (builtins.md §8) is dead without
        // this: the driver reads `JWC_REDIS_URL` here and nowhere else, so
        // until it was called, `redis.enabled()` answered `false` on a
        // binary built `--features redis` with the variable set, and every
        // other call raised.
        crate::redis_engine::init_from_env()?;
        // #33 — name the missing column at boot rather than wrapping
        // Postgres's 42703 in a 500 at request time. `information_schema`
        // is readable by every role, so this costs one query and no
        // privileges.
        if needs_db && !skip_schema_check {
            let url = crate::engine::database_url_from_env()?;
            let client = crate::engine::connect_for_migrations(&url).await?;
            let missing = crate::apply::check_live_schema(&client, &snap).await?;
            if !missing.is_empty() {
                for m in &missing {
                    eprintln!("error: {m}");
                }
                bail!(
                    "the database is behind the sources — run `jwc migrate up`                      ({} thing{} missing)",
                    missing.len(),
                    plural(missing.len())
                );
            }
        }
        if std::env::var("JWC_LOG_SQL").as_deref() == Ok("1") {
            // tooling.md §2.2 — the parameters in a logged statement are
            // the request's data. Said once at boot rather than on every
            // line, which is where a warning stops being read.
            eprintln!(
                "warning: JWC_LOG_SQL=1 — every statement and its bound \
                 parameters go to stderr. Those parameters are request data; \
                 this is a development switch."
            );
        }
        // `serve(port)` in `main()` is where the program says where it
        // listens, and until now nothing evaluated it: the listener took
        // the CLI default and a program asking for 3000 silently got 8080.
        // `main` is an ordinary body, so it runs on an ordinary Vm — which
        // is also what makes `serve(int(env("PORT") ?? "8080"))`, the form
        // the spec's own sample uses, mean anything.
        //
        // `main` runs either way. It used to run only when `--port` was
        // absent, because the only caller was the port lookup — so
        // `jwc serve --port 3000` skipped the program's initialisation
        // entirely, and a `main` that did anything besides call `serve`
        // did it or not depending on a flag about the port.
        let declared = crate::serve::declared_port(&program).await?;
        let port = port.or(declared).unwrap_or(8080);
        println!("{} routes", program.routes.len());
        crate::serve::serve(program, port).await
    })
}

/// `jwc serve --watch` — supervise a child `jwc serve` and replace it when
/// a `.jwc` file under the project changes.
///
/// A child process rather than an in-process reload. Reloading in place
/// would mean dropping a `Program` that the running handlers still borrow,
/// re-running `main`, and re-opening the pool — three things whose failure
/// modes are all "the old server is half gone". Killing a process has one.
///
/// `notify = "6"` has been a dependency the whole time, pulling `inotify`
/// into every build; until now nothing in `src/` used it.
fn serve_with_watch(
    root: &Path,
    port: Option<u16>,
    skip_schema_check: bool,
    dev: bool,
    request_logging: bool,
) -> Result<()> {
    use notify::{event::EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    // `jwc serve --watch app.jwc` watches the directory holding it: an
    // editor that writes through a temp file replaces the inode, and a
    // watch on the file itself would follow the deleted one.
    let dir = if root.is_file() {
        root.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        root.to_path_buf()
    };

    let exe = std::env::current_exe()?;
    let (tx, rx) = mpsc::channel::<notify::Event>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })?;
    watcher.watch(&dir, RecursiveMode::Recursive)?;
    println!("watching {} for .jwc changes", display_relative(&dir));

    loop {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("serve").arg(root);
        if let Some(p) = port {
            cmd.arg("--port").arg(p.to_string());
        }
        if skip_schema_check {
            cmd.arg("--skip-schema-check");
        }
        if dev {
            cmd.arg("--dev");
        }
        if request_logging {
            cmd.arg("--request-logging");
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("watch: could not start the server: {e}"))?;

        // Drain whatever arrived while the child was starting, so the
        // first edit after a restart is what triggers the next one.
        while rx.try_recv().is_ok() {}

        loop {
            let Ok(event) = rx.recv() else {
                // The watcher thread is gone — nothing will ever restart
                // the child again, so stop supervising and take it down
                // rather than leaving an unwatched server behind.
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

        println!("change detected — restarting");
        let _ = child.kill();
        let _ = child.wait();

        // One save is several filesystem events, and an editor's
        // write-temp-then-rename is several more. Without the debounce the
        // server restarts once per event and never finishes booting.
        std::thread::sleep(Duration::from_millis(250));
        while rx.try_recv().is_ok() {}
    }
}

/// Only `.jwc` sources restart the server. A `.env` change does not: the
/// child reads it at boot, and restarting on it would mean restarting
/// every time an editor touched a dotfile in the tree.
fn is_jwc_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jwc"))
}

/// `jwc v1 ast <file>` — the parse tree, for debugging the front-end and
/// for `tests/parse_corpus` triage.
pub fn ast(path: PathBuf) -> Result<()> {
    let parsed = crate::parse_file(&path)?;
    if parsed.has_errors() {
        eprint!("{}", parsed.render_all());
        bail!("{} did not parse", path.display());
    }
    println!("{:#?}", parsed.program);
    Ok(())
}

/// `jwc migrate new <name> [path]` — write the next migration.
///
/// Offline (migrations.md §1): the previous state comes from the last
/// `.snapshot.json` under `migrations/`, never from a database.
pub fn migrate_new(
    path: PathBuf,
    name: String,
    dir: Option<PathBuf>,
    explain: bool,
    dry_run: bool,
) -> Result<()> {
    use crate::diag::Severity;

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    if ws.has_parse_errors() {
        eprint!("{}", ws.parse_errors().join(""));
        bail!("{} did not parse", path.display());
    }

    let built = crate::model::build(&ws);
    let schema_errors = built
        .diags
        .iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .count();
    for (loc, d) in &built.diags {
        eprint!("{}", ws.render(*loc, d));
    }
    if schema_errors > 0 {
        bail!("{schema_errors} schema error{}", plural(schema_errors));
    }

    let dir = dir.unwrap_or_else(|| {
        let root = if path.is_file() {
            path.parent().unwrap_or(&path).to_path_buf()
        } else {
            path.clone()
        };
        root.join("migrations")
    });
    let prev = crate::snapshot::previous(&dir).map_err(anyhow::Error::msg)?;
    let ordinal = crate::snapshot::next_ordinal(&dir);
    let plan = crate::migrate::plan(&prev, &built.model, ordinal, &name);

    if explain {
        for e in &plan.explain {
            let where_ = match e.loc {
                Some(l) => ws.file_line(l),
                // A drop has no declaration; its cause is an absence.
                None => "(removed)".to_string(),
            };
            println!("{:>2}  {:<60}  {where_}", e.phase as u8, e.text);
        }
    }

    let mut errors = 0usize;
    for (loc, d) in &plan.diags {
        if d.severity == Severity::Error {
            errors += 1;
        }
        eprint!("{}", ws.render(*loc, d));
    }
    if errors > 0 {
        bail!("{errors} error{} — no migration written", plural(errors));
    }

    if plan.is_empty() {
        println!("no schema changes");
        return Ok(());
    }

    if dry_run {
        for f in &plan.files {
            println!("── {}.up.sql\n{}", f.stem, f.up);
            println!("── {}.down.sql\n{}", f.stem, f.down);
        }
        return Ok(());
    }

    std::fs::create_dir_all(&dir)?;
    for f in &plan.files {
        let up = dir.join(format!("{}.up.sql", f.stem));
        if up.exists() {
            bail!("{} already exists", display_relative(&up));
        }
        std::fs::write(&up, &f.up)?;
        std::fs::write(dir.join(format!("{}.down.sql", f.stem)), &f.down)?;
        if let Some(snap) = &f.snapshot {
            std::fs::write(dir.join(format!("{}.snapshot.json", f.stem)), snap)?;
        }
        println!("{}", display_relative(&up));
    }
    Ok(())
}

/// The migrations directory for a project path.
fn migrations_dir(path: &Path, dir: Option<PathBuf>) -> PathBuf {
    dir.unwrap_or_else(|| {
        let root = if path.is_file() {
            path.parent().unwrap_or(path).to_path_buf()
        } else {
            path.to_path_buf()
        };
        root.join("migrations")
    })
}

fn migration_client() -> Result<(tokio::runtime::Runtime, tokio_postgres::Client)> {
    let url = crate::engine::database_url_from_env()?;
    let rt = runtime()?;
    let client = rt.block_on(crate::engine::connect_for_migrations(&url))?;
    Ok((rt, client))
}

/// `jwc migrate up [path] [--to N]` — apply every pending migration.
pub fn migrate_up(path: PathBuf, dir: Option<PathBuf>, to: Option<u32>) -> Result<()> {
    let dir = migrations_dir(&path, dir);
    let (rt, client) = migration_client()?;
    let ran = rt.block_on(crate::apply::up(&client, &dir, to))?;
    if ran.is_empty() {
        println!("nothing to apply");
    }
    for name in ran {
        println!("applied {name}");
    }
    Ok(())
}

/// `jwc migrate down [path] [--count N]` — roll back, newest first.
pub fn migrate_down(path: PathBuf, dir: Option<PathBuf>, count: usize) -> Result<()> {
    let dir = migrations_dir(&path, dir);
    let (rt, client) = migration_client()?;
    let undone = rt.block_on(crate::apply::down(&client, &dir, count))?;
    if undone.is_empty() {
        println!("nothing to roll back");
    }
    for name in undone {
        println!("rolled back {name}");
    }
    Ok(())
}

/// `jwc migrate status [path]` — applied, pending, and drift.
pub fn migrate_status(path: PathBuf, dir: Option<PathBuf>) -> Result<()> {
    let dir = migrations_dir(&path, dir);
    let (rt, client) = migration_client()?;
    let st = rt.block_on(crate::apply::status(&client, &dir))?;
    for r in &st.applied {
        println!("applied  {}", r.name);
    }
    for p in &st.pending {
        println!("pending  {p}");
    }
    for d in &st.drift {
        eprintln!("drift    {d}");
    }
    println!(
        "{} applied, {} pending, {} drift",
        st.applied.len(),
        st.pending.len(),
        st.drift.len()
    );
    if !st.drift.is_empty() {
        bail!("{} drift finding{}", st.drift.len(), plural(st.drift.len()));
    }
    Ok(())
}

/// `jwc migrate list [path]` — the migration files on disk, in order.
///
/// Offline, unlike `migrate status`: it says what is *written*, not what
/// is applied, so it works with no `DATABASE_URL` and no reachable
/// database. That is the difference worth having — "what does this
/// checkout contain" is a question you ask before you have a database,
/// and `status` needs one to answer anything at all.
pub fn migrate_list(path: PathBuf, dir: Option<PathBuf>) -> Result<()> {
    let dir = migrations_dir(&path, dir);
    let files = crate::snapshot::list(&dir);
    if files.is_empty() {
        println!(
            "no migrations in {} — `jwc migrate new <name>` writes the first",
            display_relative(&dir)
        );
        return Ok(());
    }
    for m in &files {
        // A `down` is optional and its absence is the interesting fact:
        // it is what `migrate down` will refuse.
        let down = if m.down().exists() {
            ""
        } else {
            "   (no down)"
        };
        println!("{:>4}  {}{}", m.ordinal, m.stem, down);
    }
    println!("{} migration{}", files.len(), plural(files.len()));
    Ok(())
}

/// `jwc migrate verify [path]` — the names the binary expects against the
/// ones Postgres holds (#28).
pub fn migrate_verify(path: PathBuf) -> Result<()> {
    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.has_parse_errors() {
        eprint!("{}", ws.parse_errors().join(""));
        bail!("{} did not parse", path.display());
    }
    let snap = crate::snapshot::of(&crate::model::build(&ws).model);
    let (rt, client) = migration_client()?;
    let problems = rt.block_on(crate::apply::verify(&client, &snap))?;
    for p in &problems {
        eprintln!("{p}");
    }
    if problems.is_empty() {
        println!("ok — every constraint, index and view is present under its expected name");
        return Ok(());
    }
    bail!("{} problem{}", problems.len(), plural(problems.len()))
}

/// `jwc test [path]` — run every `test` block (testing.md §1.3).
///
/// Each test runs inside its own transaction, rolled back when it ends, so
/// the order is irrelevant and nothing a test writes outlives it (§2.1).
pub fn test(path: PathBuf, filter: Option<String>, no_rollback: bool) -> Result<()> {
    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    let program = std::sync::Arc::new(crate::serve::load(&ws)?);

    // Declaration order, which is also file order — `Workspace::load`
    // sorts, so two runs report the same sequence.
    let mut tests: Vec<(String, crate::ast::Block)> = Vec::new();
    for file in &ws.files {
        for d in &file.program.decls {
            if let crate::ast::Decl::Test(t) = d {
                if filter.as_ref().is_none_or(|f| t.name.contains(f.as_str())) {
                    tests.push((t.name.clone(), t.body.clone()));
                }
            }
        }
    }
    if tests.is_empty() {
        println!("no tests");
        return Ok(());
    }

    let rt = runtime()?;
    rt.block_on(async move {
        crate::engine::init_engine_from_env()?;
        // `jwc test` runs the same program bodies as `serve`, so a test
        // touching `redis.*` needs the same driver.
        crate::redis_engine::init_from_env()?;
        if no_rollback {
            eprintln!(
                "warning: --no-rollback — every test commits. {} will hold \
                 whatever they write.",
                crate::engine::scrub_database_url(
                    &crate::engine::database_url_from_env().unwrap_or_default()
                )
            );
        }

        let request = std::sync::Arc::new(crate::exec::Request {
            method: "TEST".into(),
            path: "/".into(),
            route: "/".into(),
            headers: Default::default(),
            query: Vec::new(),
            body: String::new(),
            peer_ip: "127.0.0.1".into(),
            client_ip: "127.0.0.1".into(),
            id: "test".into(),
        });

        let mut failed = 0usize;
        for (name, body) in &tests {
            let mut vm = crate::exec::Vm::new(&program, request.clone());
            match vm.in_scoped_transaction(body, !no_rollback).await {
                Ok(_) => println!("\x1b[32mok\x1b[0m    {name}"),
                Err(e) => {
                    failed += 1;
                    println!("\x1b[31mFAILED\x1b[0m {name}");
                    for line in describe_abort(&e).lines() {
                        println!("        {line}");
                    }
                }
            }
        }
        println!(
            "\n{} test{}, {failed} failed",
            tests.len(),
            plural(tests.len())
        );
        if failed > 0 {
            bail!("{failed} test{} failed", plural(failed));
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn describe_abort(a: &crate::exec::Abort) -> String {
    match a {
        crate::exec::Abort::Thrown(t) => format!("{}: {}", t.error, t.message()),
        crate::exec::Abort::Fault(e) => format!("{e}"),
    }
}

/// `jwc openapi [path] [--out f]` — an OpenAPI 3.1 document, derived from
/// the route table, the typed signatures and the raise sets (tooling.md §5).
///
/// Offline. Every part of the document already exists in the compiler; this
/// arranges them and infers nothing of its own.
/// `compact` writes one line. Indented is the default and `--pretty` names
/// it: 0.9 had the inversion — compact by default, `--pretty` to indent —
/// and flipping back would change the output of every caller that has been
/// running `jwc openapi` since. The choice the flag existed to give is
/// what matters, not which side of it is the default.
pub fn openapi(
    path: PathBuf,
    out: Option<PathBuf>,
    title: Option<String>,
    compact: bool,
) -> Result<()> {
    let doc = openapi_document(&path, title)?;
    let body = if compact {
        serde_json::to_string(&doc)?
    } else {
        serde_json::to_string_pretty(&doc)?
    };
    let text = format!("{body}\n");
    match out {
        Some(p) => {
            std::fs::write(&p, text)?;
            println!("{}", display_relative(&p));
        }
        None => print!("{text}"),
    }
    Ok(())
}

/// The OpenAPI document for a project, checked first.
///
/// `jwc openapi` and `jwc swagger` are the same document rendered two
/// ways, so they share this rather than each walking the workspace. A
/// second walk is how the JSON and the page would come to disagree — and
/// the 0.9 `jwc swagger` was exactly that: a whole second generator, 661
/// lines, kept in step with the first by hand.
pub fn openapi_document(path: &Path, title: Option<String>) -> Result<serde_json::Value> {
    use crate::diag::Severity;
    let path = path.to_path_buf();

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    if ws.has_parse_errors() {
        eprint!("{}", ws.parse_errors().join(""));
        bail!("source did not parse");
    }
    let built = crate::model::build(&ws);
    let symbols = crate::symbols::build(&ws, &built.model);
    // Two passes: the first infers the return type of every unannotated
    // function, the second hands each call site the shape its callee
    // actually produces. One pass cannot know the type of a function it has
    // not reached yet, and reordering would only move the problem.
    let first = crate::check::check(&ws, &symbols, &built.model);
    let checked = crate::check::check_with(&ws, &symbols, &built.model, &first.function_returns);
    let wired = crate::wiring::wire(&ws, &symbols);

    let errors = built
        .diags
        .iter()
        .chain(&symbols.diags)
        .chain(&checked.diags)
        .chain(&wired.diags)
        .filter(|(_, d)| d.severity == Severity::Error)
        .count();
    if errors > 0 {
        for (loc, d) in built
            .diags
            .iter()
            .chain(&symbols.diags)
            .chain(&checked.diags)
            .chain(&wired.diags)
        {
            if d.severity == Severity::Error {
                eprint!("{}", ws.render(*loc, d));
            }
        }
        bail!("{errors} error{} — no document written", plural(errors));
    }

    // Which declared errors each route can raise. errors.md §4.3 makes a
    // declared error's default status the answer whether or not an
    // `errorHandler` arm names it, so this is exactly the non-2xx set.
    let bodies = crate::wiring::function_bodies(&ws);
    let mut raises: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for file in &ws.files {
        for d in &file.program.decls {
            let crate::ast::Decl::Routes(r) = d else {
                continue;
            };
            for rt in &r.routes {
                let key = format!(
                    "{} {}",
                    rt.method.name.to_uppercase(),
                    crate::wiring::route_pattern(&r.prefix, &rt.suffix)
                );
                let mut set: Vec<String> = crate::wiring::raises_from(&symbols, &bodies, &rt.body)
                    .into_iter()
                    .collect();
                // Middleware runs before the handler and can answer on its
                // own, so what it raises the route can produce.
                for m in &rt.uses {
                    if let Some(b) = middleware_body(&ws, &m.name) {
                        set.extend(crate::wiring::raises_from(&symbols, &bodies, b));
                    }
                }
                for m in &r.uses {
                    if let Some(b) = middleware_body(&ws, &m.name) {
                        set.extend(crate::wiring::raises_from(&symbols, &bodies, b));
                    }
                }
                set.sort();
                set.dedup();
                raises.insert(key, set);
            }
        }
    }

    let title = title.unwrap_or_else(|| {
        built
            .model
            .database
            .clone()
            .unwrap_or_else(|| "JWC application".to_string())
    });
    Ok(crate::openapi::document(&crate::openapi::Input {
        title,
        version: "1.0.0".to_string(),
        sym: &symbols,
        wired: &wired,
        checked: &checked,
        raises,
    }))
}

/// `jwc swagger [path] [--port N] [--out FILE]` — the OpenAPI document,
/// rendered to read rather than to parse.
///
/// The 0.9 command of this name was a *second* OpenAPI generator whose
/// output went to `openapi.json`; `jwc openapi --out openapi.json` does
/// that today. What never existed is somewhere to read the API, so that
/// is what this is — over the same document, from the same generator.
pub fn swagger(
    path: PathBuf,
    port: u16,
    out: Option<PathBuf>,
    title: Option<String>,
) -> Result<()> {
    let doc = openapi_document(&path, title)?;

    if let Some(p) = out {
        // One file, no assets beside it: `--out` exists so the page can be
        // committed, mailed or published without a directory around it.
        std::fs::write(&p, crate::swagger::render(&doc))?;
        println!("{}", display_relative(&p));
        return Ok(());
    }

    runtime()?.block_on(crate::swagger::serve(doc, port))
}

fn middleware_body<'a>(
    ws: &'a crate::workspace::Workspace,
    name: &str,
) -> Option<&'a crate::ast::Block> {
    ws.files.iter().find_map(|f| {
        f.program.decls.iter().find_map(|d| match d {
            crate::ast::Decl::Middleware(m) if m.name.name == name => Some(&m.body),
            _ => None,
        })
    })
}

/// `jwc lint [path] [--constraints]` — `jwc check` plus the whole-program
/// lints that are advisory rather than definitional (tooling.md §4).
/// `jwc lint --list-codes` — every diagnostic the spec documents.
///
/// The table comes from `docs/spec/v1/*.md` by way of `build.rs`, so this
/// listing cannot fall behind the spec: there is no second copy to fall
/// behind with.
fn print_code_list() -> Result<()> {
    let all = crate::codes::DIAGNOSTIC_CATALOGUE;
    if all.is_empty() {
        bail!(
            "this binary carries no diagnostic catalogue — it was built \
             without `docs/spec/v1` beside it"
        );
    }
    let mut band = "";
    for (code, file, meaning) in all {
        // A blank line between bands. E02xx is names, E03xx types, E07xx
        // routing — the grouping is the useful part of a 200-row list.
        if &code[..3] != band {
            if !band.is_empty() {
                println!();
            }
            band = &code[..3];
        }
        println!("{code}  {meaning}  ({file})");
    }
    println!("\n{} diagnostics", all.len());
    Ok(())
}

/// `jwc lint --explain E0211` — one code, with the spec file that defines
/// it. A miss lists the code's band rather than nothing: `E02` is names,
/// so a mistyped `E0121` still lands the reader near the answer.
fn explain_code(code: &str) -> Result<()> {
    match crate::codes::lookup(code) {
        Some((code, file, meaning)) => {
            println!("{code}  {meaning}");
            println!("\nspec: docs/spec/v1/{file}");
            Ok(())
        }
        None => {
            let band = crate::codes::in_same_band(code);
            if band.is_empty() {
                bail!("no diagnostic {code} — `jwc lint --list-codes` has the whole set");
            }
            eprintln!("no diagnostic {code}. In the same band:");
            for (c, _, meaning) in &band {
                eprintln!("  {c}  {meaning}");
            }
            bail!("unknown diagnostic code");
        }
    }
}

/// `jwc lint --json` — every diagnostic as one JSON array on stdout.
///
/// The shape an editor or a CI annotation reads: absolute path, 1-based
/// line and column, severity, code, message, and the spec clause when the
/// diagnostic names one. Warnings included — filtering them here would
/// make the flag useless for the tools it exists for.
fn lint_json(ws: &crate::workspace::Workspace) -> Result<()> {
    use crate::diag::Severity;

    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut errors = 0usize;

    let mut push = |file: &crate::diag::SourceFile, d: &crate::diag::Diagnostic| {
        let (line, col) = file.line_col(d.span.start);
        let (end_line, end_col) = file.line_col(d.span.end);
        out.push(serde_json::json!({
            "file": file.path.display().to_string(),
            "line": line,
            "column": col,
            "end_line": end_line,
            "end_column": end_col,
            "severity": match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            "code": d.code,
            "message": d.message,
            "note": d.note,
            "spec": d.clause,
        }));
    };

    for f in &ws.files {
        for d in &f.diags {
            if d.severity == Severity::Error {
                errors += 1;
            }
            push(&f.source, d);
        }
    }

    // Parse errors make everything downstream meaningless, so the array
    // stops there rather than carrying diagnostics derived from a tree
    // that does not exist.
    if errors == 0 {
        let built = crate::model::build(ws);
        let symbols = crate::symbols::build(ws, &built.model);
        let checked = crate::check::check(ws, &symbols, &built.model);
        let wired = crate::wiring::wire(ws, &symbols);
        let mut imports = crate::imports::check(ws, &ws.packages);
        imports.extend(crate::imports::case_convention(ws));
        imports.extend(crate::packages::check(ws, &symbols));
        for (loc, d) in built
            .diags
            .iter()
            .chain(&symbols.diags)
            .chain(&checked.diags)
            .chain(&wired.diags)
            .chain(&imports)
        {
            if d.severity == Severity::Error {
                errors += 1;
            }
            if let Some(f) = ws.files.get(loc.file) {
                push(&f.source, d);
            }
        }
    }

    println!("{}", serde_json::to_string(&out)?);
    // Machine-readable output on stdout, the verdict in the exit code:
    // a CI step reads the array and the shell reads the status.
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

pub fn lint(
    path: PathBuf,
    constraints: bool,
    deny_warnings: bool,
    json: bool,
    list_codes: bool,
    explain: Option<String>,
) -> Result<()> {
    // Both read the catalogue and no sources, so they answer outside a
    // project — which is the point: you look a code up when you have one
    // in front of you, not when you have a checkout.
    if list_codes {
        return print_code_list();
    }
    if let Some(code) = explain {
        return explain_code(&code);
    }

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    if json {
        return lint_json(&ws);
    }
    // Everything `jwc check` would say, said first and once.
    check(path.clone(), true, false, false)?;

    let built = crate::model::build(&ws);
    let symbols = crate::symbols::build(&ws, &built.model);
    let wired = crate::wiring::wire(&ws, &symbols);
    let bodies = crate::wiring::function_bodies(&ws);

    // Which routes reach which table. A write is the only way a constraint
    // can be violated, so this bounds the statuses each route can produce
    // (errors.md §6.4).
    let mut reached: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut per_route: Vec<(String, Vec<(String, crate::wiring::WriteKind)>)> = Vec::new();
    for file in &ws.files {
        for d in &file.program.decls {
            let crate::ast::Decl::Routes(r) = d else {
                continue;
            };
            for rt in &r.routes {
                let label = format!(
                    "{} {}",
                    rt.method.name.to_uppercase(),
                    crate::wiring::route_pattern(&r.prefix, &rt.suffix)
                );
                let mut tables = crate::wiring::writes_in(&rt.body);
                for f in crate::wiring::reachable_from(&bodies, &rt.body) {
                    if let Some(b) = bodies.get(&f) {
                        tables.extend(crate::wiring::writes_in(b));
                    }
                }
                for (t, kind) in &tables {
                    // A `delete` cannot violate the target's own unique or
                    // check, so it does not count as reaching them.
                    if *kind != crate::wiring::WriteKind::Delete {
                        reached.entry(t.clone()).or_default().push(label.clone());
                    }
                }
                per_route.push((label, tables.into_iter().collect()));
            }
        }
    }
    per_route.sort();

    if constraints {
        for (label, tables) in &per_route {
            println!("\x1b[1m{label}\x1b[0m");
            if tables.is_empty() {
                println!("  (writes nothing — no constraint is reachable)");
                continue;
            }
            let mut rows: Vec<String> = Vec::new();
            for (path, kind) in tables {
                rows.extend(constraint_rows(&built.model, &symbols, path, *kind));
            }
            rows.sort();
            rows.dedup();
            if rows.is_empty() {
                println!("  (no constraint can be violated by what it writes)");
            }
            for line in rows {
                println!("  {line}");
            }
        }
        println!();
    }

    // W1302, once per constraint rather than once per route: the caret
    // belongs on the schema line, and a handler that reaches it did nothing
    // wrong (tooling.md §4.3.1).
    let mut warnings = 0usize;
    for t in &built.model.tables {
        let routes: Vec<String> = symbols
            .by_path
            .iter()
            .filter(|(_, name)| **name == t.declared)
            .flat_map(|(p, _)| reached.get(p).cloned().unwrap_or_default())
            .collect();
        if routes.is_empty() {
            continue;
        }
        let mut named: Vec<String> = routes;
        named.sort();
        named.dedup();
        let note = format!("reached from: {}", named.join(", "));

        let mut messageless: Vec<(&str, crate::workspace::Loc, &'static str)> = Vec::new();
        for u in &t.uniques {
            if u.message.is_none() {
                messageless.push((&u.name, u.loc, "unique"));
            }
        }
        for c in &t.checks {
            if c.message.is_none() {
                messageless.push((&c.name, c.loc, "check"));
            }
        }
        for (name, loc, kind) in messageless {
            warnings += 1;
            let d = crate::diag::Diagnostic::warning(
                "W1302",
                loc.span,
                format!("`{name}` carries no message, so violating it is a 500"),
            )
            .note(format!(
                "{note}\nadd `: \"…\"` to make it a declared error \
                 ({}); a pure invariant no request can violate is fine as it is",
                if kind == "unique" {
                    "Conflict, 409"
                } else {
                    "BadRequest, 400"
                }
            ))
            .clause("errors.md §6.2");
            eprint!("{}", ws.render(loc, &d));
        }
    }
    let _ = &wired;

    // errors.md §6.2 asks for the 500-producing set to be *enumerable*
    // rather than discovered in production. A constraint on a table no route
    // writes cannot produce a 500 through the API, so it is not a warning —
    // but leaving it out of the count entirely would make the set look
    // smaller than it is.
    let unreachable: usize = built
        .model
        .tables
        .iter()
        .filter(|t| {
            !symbols
                .by_path
                .iter()
                .any(|(p, name)| **name == t.declared && reached.contains_key(p))
        })
        .map(|t| {
            t.uniques.iter().filter(|u| u.message.is_none()).count()
                + t.checks.iter().filter(|c| c.message.is_none()).count()
        })
        .sum();
    if constraints && unreachable > 0 {
        println!(
            "{unreachable} more message-less constraint{} on tables no route writes — \
             not warned, but they are 500s the day one does",
            plural(unreachable)
        );
    }

    println!(
        "{} route{}, {warnings} warning{}",
        per_route.len(),
        plural(per_route.len()),
        plural(warnings)
    );
    if deny_warnings && warnings > 0 {
        bail!("{warnings} warning{} (--deny-warnings)", plural(warnings));
    }
    Ok(())
}

/// One line per constraint on a table, with the status its violation
/// produces (errors.md §6).
fn constraint_rows(
    model: &crate::model::SchemaModel,
    sym: &crate::symbols::Symbols,
    path: &str,
    kind: crate::wiring::WriteKind,
) -> Vec<String> {
    use crate::wiring::WriteKind;
    let Some(declared) = sym.by_path.get(path) else {
        return Vec::new();
    };
    let Some(t) = model.tables.iter().find(|t| &t.declared == declared) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let row = |name: &str, status: &str, what: String| format!("{name:<32} {status:<4} {what}");

    if kind == WriteKind::Delete {
        // A delete violates nothing on the row it removes. What it can trip
        // is a foreign key **pointing at** that row from elsewhere — and
        // only where the reference is not cascaded or nulled.
        for other in &model.tables {
            for f in &other.foreign_keys {
                if f.target_schema != t.schema_physical || f.target_table != t.physical {
                    continue;
                }
                if matches!(
                    f.on_delete,
                    Some(crate::ast::RefAction::Cascade) | Some(crate::ast::RefAction::SetNull)
                ) {
                    continue;
                }
                out.push(row(
                    &f.name,
                    "400",
                    format!(
                        "{}.{} still references this row",
                        other.schema, other.declared
                    ),
                ));
            }
        }
        return out;
    }

    for u in &t.uniques {
        match &u.message {
            // errors.md §6.1 — a unique is a conflict with existing state,
            // not a malformed request.
            Some(m) => out.push(row(&u.name, "409", format!("\"{m}\""))),
            None => out.push(row(&u.name, "500", "(no message — a fault)".into())),
        }
    }
    for c in &t.checks {
        match &c.message {
            Some(m) => out.push(row(&c.name, "400", format!("\"{m}\""))),
            None => out.push(row(&c.name, "500", "(no message — a fault)".into())),
        }
    }
    for f in &t.foreign_keys {
        // errors.md §6.3 — always 400, with a fixed message. An FK carries
        // no per-constraint message in 1.0 (DEFERRED-4).
        out.push(row(&f.name, "400", "referenced row does not exist".into()));
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn display_relative(p: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(|r| r.display().to_string()))
        .unwrap_or_else(|| p.display().to_string())
}

/// `jwc build [path] [--release] [--emit-rust]` — the native AOT backend.
///
/// Deleted at the v0.25.0 cutover and restored in 0.9.901. The runtime half
/// — the prelude the generated crate includes — came back unchanged; the
/// codegen is written against the 1.0 AST, because the old one named
/// declarations this language does not have.
pub fn build(path: PathBuf, release: bool, emit_rust: bool, target: Option<String>) -> Result<()> {
    // `jwc build` must not ship what `jwc check` refuses.
    //
    // It did. This function tested `has_parse_errors` and nothing else, so
    // every diagnostic past the parser — every type error, every unknown
    // name, every unresolved import — was skipped on the one path that
    // produces the production binary. A program that `jwc serve` refuses to
    // boot and `jwc check` exits 1 on built clean and ran, because codegen
    // does not need the types to be right in order to emit something that
    // happens to compile.
    //
    // Calling `check` rather than repeating its passes is the point: a
    // second list of analyses here is a second list to forget to extend,
    // which is how the first one ended up being just the parser.
    check(path.clone(), true, false, false)?;

    let ws = crate::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }

    let app = ws
        .manifest
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| "app".to_string());

    if emit_rust {
        let out = crate::native::emit_rust_source(&ws, &path, &app, release)?;
        println!("{}", out.display());
        return Ok(());
    }

    let report = crate::native::compile(&ws, &path, &app, release, target.as_deref())?;
    println!("{}", report.binary_path.display());
    Ok(())
}

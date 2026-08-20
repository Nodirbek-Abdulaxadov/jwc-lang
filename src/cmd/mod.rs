//! The CLI's commands.
//!
//! These were `jwc v1 …` while the two languages coexisted. The v0.25.0
//! cutover removed the older one, so the prefix is gone and these are the
//! ordinary commands (ROADMAP §2).

use anyhow::{bail, Result};
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
pub fn check(path: PathBuf, quiet: bool, parse_only: bool) -> Result<()> {
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
pub fn fmt(path: PathBuf, check_only: bool) -> Result<()> {
    let files = collect_sources(&path)?;
    if files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }

    let mut changed: Vec<PathBuf> = Vec::new();
    let mut failed = 0usize;

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
        changed.push(f.clone());
        if !check_only {
            std::fs::write(f, &printed)?;
        }
    }

    if failed > 0 {
        bail!("{failed} file{} did not parse", plural(failed));
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
        println!("ok — {} file{} already formatted", files.len(), plural(files.len()));
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
            std::fs::write(&p, format!("{sql}
"))?;
            println!("wrote {} ({} statements)", p.display(), statements.len());
        }
        None => println!("{sql}"),
    }
    Ok(())
}

/// `jwc v1 explain <path>` — every query the program issues.
///
/// Prints, per query: where it is, the SQL with bind placeholders, and
/// whether the result stays raw. This is the answer to #29 — generated SQL
/// used to be invisible, so nobody could tell a query that reads an index
/// from one that reads the table (queries.md §7.4).
pub fn explain(path: PathBuf, sql_only: bool) -> Result<()> {
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

    let mut queries = 0usize;
    let mut gaps = 0usize;
    let mut hatches = 0usize;
    for file in &ws.files {
        // writes.md §6.4 — the valve's usage count is the measurement of
        // which feature to add next, so it is printed rather than assumed
        // to be zero.
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
    for file in &ws.files {
        for site in crate::query_sql::sites(&file.program) {
            queries += 1;
            let (line, _) = file.source.line_col(site.select.span.start);
            println!(
                "\x1b[1m{}:{line}\x1b[0m  {}",
                file.source.path.display(),
                site.label
            );
            let plan = crate::query::plan(site.select, &sym);
            if let Some(d) = plan
                .diags
                .iter()
                .find(|d| d.severity == Severity::Error)
            {
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
                }
                None => {
                    println!("  not compilable: {}", c.gap());
                    gaps += 1;
                }
            }
            println!();
        }
    }
    println!("{queries} quer{}", if queries == 1 { "y" } else { "ies" });
    if gaps > 0 {
        println!("{gaps} not compiled");
    }
    if hatches > 0 {
        println!("{hatches} raw() escape hatch{}", if hatches == 1 { "" } else { "es" });
    }
    Ok(())
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
        println!(
            "{:<7} {:<width$}  {chain}",
            r.method,
            r.pattern,
            width = width
        );
        if !r.after.is_empty() {
            println!("{:<7} {:<width$}  after: {}", "", "", r.after.join(" → "), width = width);
        }
    }
    println!("\n{} route{}", rows.len(), plural(rows.len()));
    Ok(())
}

/// `jwc v1 serve <path> --port N` — run the program.
pub fn serve(path: PathBuf, port: u16) -> Result<()> {
    let ws = crate::workspace::Workspace::load(&path)?;
    let program = std::sync::Arc::new(crate::serve::load(&ws)?);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        crate::engine::init_engine_from_env()?;
        println!("{} routes", program.routes.len());
        crate::serve::serve(program, port).await
    })
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

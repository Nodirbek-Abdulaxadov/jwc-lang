//! `jwc v1 …` — the front-end for the redesigned language.
//!
//! Kept behind its own subcommand while the two languages coexist. The old
//! `jwc check` / `jwc fmt` still compile the 0.9.x language; `jwc v1 check`
//! and `jwc v1 fmt` compile the language specified in `docs/spec/v1/`.
//! At the v0.25.0 cutover the `v1` prefix disappears and these become the
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
    use crate::v1::diag::Severity;

    let ws = crate::v1::workspace::Workspace::load(&path)?;
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
        let built = crate::v1::model::build(&ws);
        let symbols = crate::v1::symbols::build(&ws, &built.model);
        let checked = crate::v1::check::check(&ws, &symbols, &built.model);
        for (loc, d) in built
            .diags
            .iter()
            .chain(&symbols.diags)
            .chain(&checked.diags)
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
        let parsed = crate::v1::parse_file(f)?;
        if parsed.has_errors() {
            eprint!("{}", parsed.render_all());
            failed += 1;
            continue;
        }
        let printed = crate::v1::fmt::format_program(&parsed.program);
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
    use crate::v1::diag::Severity;

    let ws = crate::v1::workspace::Workspace::load(&path)?;
    if ws.files.is_empty() {
        bail!("no .jwc files under {}", path.display());
    }
    if ws.has_parse_errors() {
        for e in ws.parse_errors() {
            eprint!("{e}");
        }
        bail!("source did not parse");
    }

    let built = crate::v1::model::build(&ws);
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

    let statements = crate::v1::ddl::emit(&built.model);
    let sql = crate::v1::ddl::render(&ws, &statements, explain);
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

/// `jwc v1 ast <file>` — the parse tree, for debugging the front-end and
/// for `tests/parse_corpus` triage.
pub fn ast(path: PathBuf) -> Result<()> {
    let parsed = crate::v1::parse_file(&path)?;
    if parsed.has_errors() {
        eprint!("{}", parsed.render_all());
        bail!("{} did not parse", path.display());
    }
    println!("{:#?}", parsed.program);
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

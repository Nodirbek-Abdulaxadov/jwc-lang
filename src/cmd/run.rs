//! `jwc run` command implementation split out from main.rs.
//!
//! Three accepted forms for the path argument (mirrors original behaviour):
//! - directory → treated as a project root via `project::find_project_root`
//! - `<x>.jwcproj` file → manifest path; root is its parent
//! - any other file → standalone `.jwc` source, parsed in isolation
//!
//! For the two project-shaped forms we also drop a bundled launcher into
//! `bin/<profile>/` as a side effect, matching the pre-extraction
//! behaviour where `jwc run` doubled as a quick `jwc build` for the dev
//! loop. Single-file mode skips that since there is no project to bundle.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::{cmd, parser, project, runner, server};

/// Drive `jwc run`. The handler picks the right load path based on the
/// argument shape, dispatches into `runner::run_main`, prints any
/// captured output, and — when the program called `serve(port)` —
/// hands off to `server::serve` to keep the process alive.
///
/// `typecheck` mirrors the `--no-typecheck` CLI flag: `true` to run the
/// gradual static type checker (the default), `false` to skip it during
/// the transition release.
pub fn run(
    rt: &tokio::runtime::Runtime,
    path: Option<PathBuf>,
    request_logging: bool,
    typecheck: bool,
) -> Result<()> {
    let target = path.unwrap_or(std::env::current_dir()?);

    if target.is_dir() {
        run_project(rt, &target, request_logging, typecheck)
    } else if is_manifest_path(&target) {
        let root = target
            .parent()
            .ok_or_else(|| anyhow!("Invalid project file path"))?
            .to_path_buf();
        run_project(rt, &root, request_logging, typecheck)
    } else {
        run_single_file(rt, &target, request_logging, typecheck)
    }
}

fn is_manifest_path(target: &Path) -> bool {
    target
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.eq_ignore_ascii_case(project::PROJECT_FILE))
        .unwrap_or(false)
}

fn run_project(
    rt: &tokio::runtime::Runtime,
    target_root: &Path,
    request_logging: bool,
    typecheck: bool,
) -> Result<()> {
    let root = project::find_project_root(target_root)?;
    project::load_dotenv(&root);
    let loaded = project::load_project_from_root_with(&root, project::LoadOpts { typecheck })?;
    loaded.manifest.ensure_runnable()?;
    let _ = cmd::build::project_native_artifact(&root, &loaded.manifest.name, false)?;
    let result = rt.block_on(runner::run_main(&loaded.program))?;
    if !result.output.is_empty() {
        print!("{}", result.output);
    }
    if let Some(port) = result.serve_port {
        server::serve(&loaded.program, port, request_logging)?;
    }
    Ok(())
}

fn run_single_file(
    rt: &tokio::runtime::Runtime,
    file: &Path,
    request_logging: bool,
    typecheck: bool,
) -> Result<()> {
    let source = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    let program = parser::parse_program(&source)
        .with_context(|| format!("Failed to parse {}", file.display()))?;
    parser::validate_program(&program)
        .with_context(|| format!("Validation failed for {}", file.display()))?;
    if typecheck {
        crate::typecheck::typecheck_program(&program)
            .with_context(|| format!("Type check failed for {}", file.display()))?;
    }
    let result = rt.block_on(runner::run_main(&program))?;
    if !result.output.is_empty() {
        print!("{}", result.output);
    }
    if let Some(port) = result.serve_port {
        server::serve(&program, port, request_logging)?;
    }
    Ok(())
}

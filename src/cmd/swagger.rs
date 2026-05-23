//! `jwc swagger` — generate an OpenAPI 3.1 document from the current project.
//!
//! Two output forms:
//! - default: write to `openapi.json` in the project root
//! - `--stdout`: dump JSON to stdout (CI / piping into other tools)
//!
//! The doc is regenerated from scratch every call — `openapi.json` is
//! treated as a build artifact, not source. Add it to `.gitignore` if
//! you don't want to commit it (most teams do commit it for `git diff`
//! review of API changes).

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::{project, swagger};

pub fn run(stdout: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = project::find_project_root(&cwd)
        .with_context(|| "jwc swagger must run inside a JWC project")?;
    let loaded = project::load_project_from_root(&root)?;

    let title = loaded.manifest.name.clone();
    let version = pick_version(&loaded.manifest);
    let doc = swagger::generate_openapi(&loaded.program, &title, &version);
    let pretty = serde_json::to_string_pretty(&doc)?;

    if stdout {
        println!("{pretty}");
        return Ok(());
    }
    let out: PathBuf = root.join("openapi.json");
    std::fs::write(&out, pretty + "\n").with_context(|| format!("write {}", out.display()))?;
    println!(
        "Wrote {} ({} paths)",
        out.display(),
        loaded.program.routes.len()
    );
    Ok(())
}

fn pick_version(m: &project::JwcProject) -> String {
    if let Some(v) = &m.pkg_version {
        if !v.is_empty() {
            return v.clone();
        }
    }
    if !m.version.is_empty() {
        return m.version.clone();
    }
    "0.0.0".to_string()
}

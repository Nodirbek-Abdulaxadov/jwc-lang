//! `jwc build` command implementation split out from main.rs.
//!
//! Two modes:
//! - default ("bundled launcher"): copies the embedded `jwc` runtime
//!   alongside the project so `./bin/<profile>/<app>[.exe]` resolves
//!   the project root and runs the interpreter against it. Same shape
//!   on Windows + Unix; Unix gets a `jwc-runtime` sidecar + a thin
//!   bash launcher so `chmod +x` works.
//! - `--native`: hands off to `native_build::compile_with_target`,
//!   which generates Rust source, invokes cargo, and emits an
//!   AOT-compiled binary under `bin/[<target>/]<profile>/`.
//!   `--emit-rust-source` short-circuits before cargo and dumps the
//!   generated source for inspection.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::{native_build, project};

/// Dispatch the Build command. `target` is the optional Rust target
/// triple (`--target x86_64-unknown-linux-musl`, ...).
pub fn run(
    release: bool,
    native: bool,
    emit_rust_source: bool,
    target: Option<String>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = project::find_project_root(&cwd)?;
    let loaded = project::load_project_from_root(&root)?;
    loaded.manifest.ensure_runnable()?;
    let profile = if release { "release" } else { "debug" };

    if emit_rust_source && !native {
        bail!("--emit-rust-source requires --native");
    }
    if target.is_some() && !native {
        bail!("--target requires --native");
    }

    if native {
        let app_name = sanitize_app_name(&loaded.manifest.name);
        if emit_rust_source {
            let out = native_build::emit_rust_source(&loaded.program, &root, &app_name, release)?;
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
        let out_path = project_native_artifact(&root, &loaded.manifest.name, release)?;
        println!("Bundled runtime + launcher ({profile})");
        println!("Project: {}", loaded.manifest.name);
        println!("Launcher: {}", out_path.display());
        println!("Note: this bundles the JWC runtime alongside your project.");
        println!("      For real AOT-compiled binaries, pass --native (Phase 4 — incremental).");
    }
    Ok(())
}

/// Slug-style normalisation for a manifest name → executable name.
/// Lowercased, ASCII alphanumeric + `_` + `-` only. Empty input
/// (or input made entirely of skipped chars) falls back to `"app"`.
pub fn sanitize_app_name(name: &str) -> String {
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

/// Drop a runtime + launcher into `bin/<profile>/`. Windows copies the
/// CLI exe as the launcher and clears any legacy `.jwcroot` sidecar
/// (executables now self-resolve the project root). Unix drops a thin
/// bash launcher + a sibling `jwc-runtime` copy and chmod +x's both.
pub fn project_native_artifact(root: &Path, manifest_name: &str, release: bool) -> Result<PathBuf> {
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
        // Legacy sidecar from older builds; executable now self-resolves.
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

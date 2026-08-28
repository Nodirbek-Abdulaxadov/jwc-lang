//! Two build-time inputs: version provenance, and the diagnostic
//! catalogue.
//!
//! **Provenance** — the target triple cargo built for, the rustc version,
//! the build profile, and the git short hash when available (falls back
//! to "unknown" outside a checkout: release tarballs, `cargo install`).
//! Consumed by `jwc --version --verbose`.
//!
//! **The catalogue** — every `E0000` / `W0000` row in `docs/spec/v1/*.md`,
//! extracted into a table `jwc lint --list-codes` and `--explain` read.
//! Generated rather than hand-written because the spec is the definition:
//! a second copy would be a second definition, and the one that drifts is
//! always the one nobody reads.
//!
//! No extra build-deps, no network. Provenance degrades to "unknown"
//! rather than failing the build.

use std::process::Command;

fn main() {
    // cargo always sets TARGET during the build script.
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=JWC_BUILD_TARGET={target}");

    // rustc version: shell out to whichever rustc cargo used.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=JWC_RUSTC_VERSION={rustc_version}");

    // git short hash. Tarball / non-git builds get "unknown".
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=JWC_GIT_HASH={git_hash}");

    // Build profile (debug / release).
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=JWC_BUILD_PROFILE={profile}");

    emit_diagnostic_catalogue();

    // Re-run only when HEAD changes or the build script itself does. We
    // intentionally don't depend on every source file here — this script
    // is fast and `cargo` already invalidates on source change for the
    // crate proper.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=docs/spec/v1");
}

/// Extract every diagnostic row from the normative spec into a sorted
/// `(code, spec file, meaning)` table.
///
/// The rows look like one of
///
/// ```text
/// | `E0211` | unknown name: not a column here, and not a local |
/// | `E0008` | a `routes` body | expected `route` or `socket` |
/// ```
///
/// so everything after the code cell is the meaning, joined with an
/// em-dash when the table splits it across columns.
///
/// A missing `docs/` directory is not a build failure: `cargo package`
/// ships `src/` and a consumer building from crates.io has no spec tree.
/// The table comes out empty and the two commands say so.
fn emit_diagnostic_catalogue() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let spec = std::path::Path::new("docs/spec/v1");

    let mut rows: Vec<(String, String, String)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(spec) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let file = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                let t = line.trim();
                if !t.starts_with("| `E") && !t.starts_with("| `W") {
                    continue;
                }
                let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
                let Some(first) = cells.first() else { continue };
                let code = first.trim_matches('`');
                let is_code = code.len() == 5
                    && (code.starts_with('E') || code.starts_with('W'))
                    && code[1..].chars().all(|c| c.is_ascii_digit());
                if !is_code {
                    continue;
                }
                let meaning = cells[1..].join(" — ");
                if meaning.is_empty() {
                    continue;
                }
                rows.push((code.to_string(), file.clone(), meaning));
            }
        }
    }

    // Sorted so the listing is stable across filesystems, and deduped so
    // a code documented in two tables (E0900 is, deliberately) is one row.
    rows.sort();
    rows.dedup_by(|a, b| a.0 == b.0);

    let mut src = String::from(
        "/// Every diagnostic the spec documents: `(code, spec file, meaning)`.\n\
         /// Generated by `build.rs` from `docs/spec/v1/*.md` — the spec is the\n\
         /// definition, so there is no second copy to drift.\n\
         pub static DIAGNOSTIC_CATALOGUE: &[(&str, &str, &str)] = &[\n",
    );
    for (code, file, meaning) in &rows {
        src.push_str(&format!("    ({:?}, {:?}, {:?}),\n", code, file, meaning));
    }
    src.push_str("];\n");
    std::fs::write(out.join("diagnostic_catalogue.rs"), src).expect("write catalogue");
}

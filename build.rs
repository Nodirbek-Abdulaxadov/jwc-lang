//! Build-time provenance for `jwc --version --verbose`.
//!
//! Emits three env vars consumed by `src/main.rs` to print rich version
//! info: the target triple cargo built for, the rustc version, and the
//! git commit short hash when available (falls back to "unknown" outside
//! a git checkout — release tarballs and `cargo install` users).
//!
//! Kept deliberately small: no extra build-deps, no network. All values
//! degrade to "unknown" rather than failing the build.

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

    // Re-run only when HEAD changes or the build script itself does. We
    // intentionally don't depend on every source file here — this script
    // is fast and `cargo` already invalidates on source change for the
    // crate proper.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}

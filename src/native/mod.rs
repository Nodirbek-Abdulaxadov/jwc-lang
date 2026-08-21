//! Native AOT: a `.jwc` project becomes a Rust crate, then a single binary.
//!
//! ## Why this exists again
//!
//! The v0.25.0 cutover deleted the whole backend — 5,149 lines of codegen
//! and 5,030 of prelude — and the roadmap entry that authorised it gave one
//! reason: a second implementation of the query compiler would have to move
//! in lockstep with the first, and every query change would need a
//! differential case to prove they agreed.
//!
//! That reason does not survive the 1.0 front-end. `query_sql` already
//! lowers a query to **a SQL string and a parameter list at compile time**
//! — `Built { sql, params, shape, … }` — so codegen embeds the very same
//! string the interpreter would send. There is no second query compiler to
//! keep in step, and no query semantics that can drift, because there is
//! only one.
//!
//! What codegen does own is the statement and expression tier: control
//! flow, calls, builders, and the shape of the generated handler. That is
//! the part written against the 1.0 AST rather than ported from the 0.9.x
//! one, which named `RouteDecl`, `MountDecl`, `ModelKind` and `validate
//! body` — none of which exist now.
//!
//! ## Layout
//!
//! * `prelude/` — the runtime the generated crate includes, restored
//!   unchanged from before the cutover. It references no AST type.
//! * `codegen.rs` — the 1.0 AST to Rust.
//! * this file — scaffolding: the manifest, the cargo invocation, and
//!   where the binary lands.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

mod codegen;

/// Codegen alone, for the test suite: no cargo, no filesystem.
pub fn codegen_for_test(ws: &crate::workspace::Workspace) -> Result<String> {
    Ok(codegen::generate(ws)?.source)
}

/// Where the generated crate is scaffolded, under the project root.
const BUILD_DIR_NAME: &str = ".jwc-build";

/// What a native build produced.
pub struct CompileReport {
    pub binary_path: PathBuf,
    pub workspace: PathBuf,
}

/// The runtime the generated crate includes. Split so a program that never
/// reaches for a dependency does not compile it: before the split every
/// hello-world built reqwest, hyper, h2 and rustls before the linker threw
/// them away, and LTO recovered the binary size but nothing recovered the
/// compile time.
pub const PRELUDE_BASE: &str = include_str!("prelude/base.rs.in");
pub const PRELUDE_DB: &str = include_str!("prelude/db.rs.in");
pub const PRELUDE_CRYPTO: &str = include_str!("prelude/crypto.rs.in");
pub const PRELUDE_REDIS: &str = include_str!("prelude/redis.rs.in");
pub const PRELUDE_WS: &str = include_str!("prelude/ws.rs.in");
pub const PRELUDE_HTTP: &str = include_str!("prelude/http.rs.in");
/// The built-ins 1.0 introduced, which the restored 0.9 runtime has no
/// counterpart for. Always included: they lean on nothing the base prelude
/// does not already carry.
pub const PRELUDE_V1: &str = include_str!("prelude/v1.rs.in");

fn find_cargo() -> Result<PathBuf> {
    if let Ok(path) = which_cargo() {
        return Ok(path);
    }
    if let Some(home) = dirs_home() {
        let candidate = home
            .join(".jwc")
            .join("toolchain")
            .join("bin")
            .join(cargo_exe_name());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("cargo not found on PATH"))
}

fn scaffold_workspace(
    root: &Path,
    app_name: &str,
    rust_src: &str,
    needs_db: bool,
    needs_http_client: bool,
    needs_crypto: bool,
    needs_redis: bool,
    needs_regex: bool,
) -> Result<PathBuf> {
    let workspace = root.join(BUILD_DIR_NAME);
    check_path_length(&workspace)?;
    let src_dir = workspace.join("src");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("Failed to create {}", src_dir.display()))?;

    let cargo_toml = workspace.join("Cargo.toml");
    std::fs::write(
        &cargo_toml,
        render_cargo_toml(
            app_name,
            needs_db,
            needs_http_client,
            needs_crypto,
            needs_redis,
            needs_regex,
        ),
    )
    .with_context(|| format!("Failed to write {}", cargo_toml.display()))?;

    let main_rs = src_dir.join("main.rs");
    std::fs::write(&main_rs, rust_src)
        .with_context(|| format!("Failed to write {}", main_rs.display()))?;

    let gitignore = workspace.join(".gitignore");
    if !gitignore.is_file() {
        let _ = std::fs::write(&gitignore, "target/\n");
    }

    Ok(workspace)
}

fn render_cargo_toml(
    app_name: &str,
    needs_db: bool,
    needs_http_client: bool,
    needs_crypto: bool,
    needs_redis: bool,
    needs_regex: bool,
) -> String {
    // `http_get` / `fetch_json` and their SSRF guards live in
    // `native_prelude_http.rs.in`, so `reqwest` is a dependency only of
    // programs that can reach it. Before the split the prelude carried them
    // unconditionally and every hello-world compiled reqwest → hyper → h2 →
    // tower → rustls before the linker discarded it: LTO recovered the
    // binary size, but nothing recovered the compile time.
    //
    // TODO(phase-10.6): the manifest is currently target-agnostic. Future
    // iterations may want to toggle reqwest's TLS backend per target
    // (e.g. `rustls-tls` for musl/static builds, `native-tls` for the
    // host glibc target) and possibly switch `panic = "abort"` on for
    // size-sensitive cross builds. Keep this single shared manifest as
    // long as the matrix is small — multiplying it per target is an
    // anti-feature.
    // `reqwest` and `url` follow the HTTP prelude: crypto pulls it in for the
    // JWKS fetch even when the program never calls `http_get`, so the
    // condition here must match the one in `codegen`.
    let needs_http = needs_http_client || needs_crypto;

    let mut deps = String::new();
    // Enumerated rather than `features = ["full"]`. The list is what the
    // prelude actually touches: `fs` for the `file.*` / `directory.*`
    // built-ins, `io-std` for `console.*`, `signal` for graceful shutdown,
    // `macros` for the emitted `#[tokio::main]`. Only `process` and
    // `parking_lot` fall out — a small win next to dropping reqwest, but it
    // keeps the manifest honest about what the generated crate uses.
    deps.push_str(
        "tokio = { version = \"1\", features = [\"rt\", \"rt-multi-thread\", \"macros\", \"net\", \"time\", \"sync\", \"io-util\", \"io-std\", \"fs\", \"signal\"] }\n",
    );
    deps.push_str("futures = \"0.3\"\n");
    deps.push_str("axum = { version = \"0.7\", features = [\"http2\"] }\n");
    // Already in the tree via tokio; named explicitly so the listener can
    // clear IPV6_V6ONLY. Neither `std` nor `tokio` exposes that option, and
    // Windows defaults it to on — a native build bound `[::]` and was
    // unreachable on 127.0.0.1, which is what every container health check
    // and load generator dials.
    deps.push_str("socket2 = \"0.5\"\n");
    if needs_http {
        deps.push_str(
            "reqwest = { version = \"0.12\", default-features = false, features = [\"rustls-tls\", \"json\"] }\n",
        );
    }
    // `uuid()` and `now()` are always-on built-ins; ship the supporting
    // crates unconditionally. Combined wire weight is ~50 KB stripped, far
    // below the noise floor of axum + reqwest + tokio.
    deps.push_str("uuid = { version = \"1\", features = [\"v4\"] }\n");
    deps.push_str("rand = \"0.8\"\n");
    deps.push_str("chrono = { version = \"0.4\", default-features = false, features = [\"clock\", \"std\"] }\n");
    // Hot-path V::Object payload is an FxHashMap (Phase A1 of PERF_PLAN.md):
    // O(1) lookup + fxhash, replacing BTreeMap's O(log n) + node alloc cost.
    deps.push_str("rustc-hash = \"2\"\n");
    // `serde_json` stays unconditional — the base prelude uses it for JSON
    // body validation and the `json_*` builtins, so it must be a direct dep
    // or every native build fails with `E0433: unresolved module or unlinked
    // crate`. `url` does not: its only use is the SSRF host-allowlist parse,
    // which moved into the HTTP prelude alongside `http_get`.
    deps.push_str("serde_json = \"1\"\n");
    if needs_http {
        deps.push_str("url = \"2\"\n");
    }
    // Only for programs that write a `pattern` rule in `validate body`.
    // Before this the rule compiled to an is-it-a-string check and the
    // regex was discarded, so `pattern(r"^https?://")` accepted
    // `javascript:` — a security boundary that existed in the source, held
    // in the interpreter, and silently did not hold in the shipped binary.
    if needs_regex {
        deps.push_str("regex = \"1\"\n");
    }
    if needs_db {
        // `with-chrono-0_4` plugs `chrono::DateTime` into tokio-postgres'
        // ToSql/FromSql so `jwc_param_timestamp` can bind directly to
        // `TIMESTAMPTZ` columns. Without the feature, the generated code
        // fails to compile with `the trait bound DateTime<Utc>: ToSql is
        // not satisfied`, and the only workaround (binding String + a
        // `$N::timestamptz` cast) trips a client-side WrongType check.
        deps.push_str("tokio-postgres = { version = \"0.7\", features = [\"with-chrono-0_4\"] }\n");
        deps.push_str("deadpool-postgres = \"0.14\"\n");
        // `db-postgres` plugs `Decimal` into tokio-postgres' ToSql/FromSql so
        // `jwc_param_numeric` can bind NUMERIC columns. Without it there is no
        // conversion for numeric in either direction.
        deps.push_str(
            "rust_decimal = { version = \"1\", default-features = false, features = [\"db-postgres\"] }\n",
        );
    }
    if needs_crypto {
        deps.push_str("sha2 = \"0.10\"\n");
        deps.push_str("sha1 = \"0.10\"\n");
        deps.push_str("md-5 = \"0.10\"\n");
        deps.push_str("hmac = \"0.12\"\n");
        deps.push_str("argon2 = { version = \"0.5\", features = [\"std\"] }\n");
        // JWT segments are base64url without padding.
        deps.push_str("base64 = \"0.22\"\n");
        // RS256 (OIDC) verification. Already in the tree via reqwest's
        // rustls-tls, but `ring::signature` is only nameable from a
        // direct dependency.
        deps.push_str("ring = \"0.17\"\n");
    }
    if needs_redis {
        // Must match the feature set in the compiler's own Cargo.toml, or
        // a program behaves differently under `jwc run` and `--native`.
        // `tokio-rustls-comp` + webpki roots so `rediss://` works in a
        // scratch container; `script` powers `redis_eval`.
        deps.push_str(
            "redis = { version = \"0.27\", default-features = false, features = [\"tokio-comp\", \"tokio-rustls-comp\", \"tls-rustls-webpki-roots\", \"script\", \"keep-alive\"] }\n",
        );
        deps.push_str(
            "deadpool-redis = { version = \"0.18\", default-features = false, features = [\"rt_tokio_1\"] }\n",
        );
    }
    // Phase A4 (PERF_PLAN.md): global allocator. mimalloc on Windows
    // sidesteps the notoriously slow `HeapAlloc` / `HeapFree` path that
    // dominates Vec / String / HashMap churn on a Windows host; on Linux
    // we fall back to the system allocator (glibc malloc is competitive
    // for our workload, and jemalloc adds 100+ KB to every binary).
    // The actual `#[global_allocator]` declaration lives in the prelude,
    // gated on `#[cfg(windows)]`.
    //
    // This MUST stay the last block: it opens a
    // `[target.'cfg(windows)'.dependencies]` table, so any crate pushed
    // after it lands under that table instead of `[dependencies]` and
    // silently vanishes on non-Windows targets — exactly how tokio-postgres
    // went missing on the Linux CI build.
    deps.push_str("[target.'cfg(windows)'.dependencies]\n");
    deps.push_str("mimalloc = { version = \"0.1\", default-features = false }\n");
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
publish = false

[[bin]]
name = "{name}"
path = "src/main.rs"

[dependencies]
{deps}
[profile.release]
# Phase A5 of PERF_PLAN.md. `opt-level = 3` (was "z") trades a slightly
# bigger binary for actual loop / inline optimization — release builds
# are perf-sensitive, not size-sensitive. `lto = "fat"` (was bare `true`,
# which is already fat — written explicitly so future readers don't
# downgrade to thin). `codegen-units = 1` keeps a single LLVM module
# so the linker sees everything for inlining.
# `panic = "abort"` is intentionally left off: the runtime relies on
# `catch_unwind` to power `try {{}} catch {{}}` and `transaction {{}}` —
# enabling abort would silently break both.
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
"#,
        name = app_name,
        deps = deps,
    )
}

fn invoke_cargo(
    cargo: &Path,
    workspace: &Path,
    app_name: &str,
    release: bool,
    target: Option<&str>,
) -> Result<PathBuf> {
    let mut cmd = Command::new(cargo);
    cmd.arg("build").current_dir(workspace);
    if release {
        cmd.arg("--release");
        // Phase A5 of PERF_PLAN.md: build with `-C target-cpu=native` so
        // LLVM emits instructions for the host's exact micro-architecture
        // (AVX2 / BMI2 / etc. when available) — meaningful on hot integer
        // and string paths. Restricted to release builds because:
        //   * debug binaries are rebuilt constantly and target-cpu=native
        //     defeats Cargo's incremental cache when machines differ;
        //   * cross-target builds use an explicit `--target` and must
        //     not be miscompiled for the *host* CPU.
        if target.is_none() {
            let existing = std::env::var("RUSTFLAGS").unwrap_or_default();
            let flag = "-C target-cpu=native";
            let combined = if existing.is_empty() {
                flag.to_string()
            } else if existing.contains("target-cpu") {
                // Honour the user's override — don't double-set.
                existing
            } else {
                format!("{} {}", existing, flag)
            };
            cmd.env("RUSTFLAGS", combined);
        }
    }
    if let Some(t) = target {
        cmd.arg("--target").arg(t);
    }
    cmd.arg("--bin").arg(app_name);

    let status = cmd
        .status()
        .with_context(|| format!("Failed to spawn cargo at {}", cargo.display()))?;
    if !status.success() {
        bail!("cargo build failed (exit {})", status.code().unwrap_or(-1));
    }

    let profile_dir = if release { "release" } else { "debug" };
    let exe = if target_is_windows(target) {
        format!("{app_name}.exe")
    } else {
        app_name.to_string()
    };
    // `CARGO_TARGET_DIR` (and its per-project `build.target-dir` twin) moves
    // cargo's output tree somewhere else entirely. Assuming `<ws>/target`
    // meant that anyone who exports it globally — a common setup, one shared
    // target dir across every Rust project — got a full successful compile
    // followed by "cargo reported success but binary not found", naming a
    // path that legitimately does not exist. Read the same variable cargo
    // read. Relative values resolve against the workspace, matching cargo.
    let target_root = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) if !dir.is_empty() => workspace.join(dir),
        _ => workspace.join("target"),
    };
    // With --target, cargo emits to <target-dir>/<triple>/<profile>/ instead
    // of <target-dir>/<profile>/.
    let mut bin = target_root;
    if let Some(t) = target {
        bin = bin.join(t);
    }
    let bin = bin.join(profile_dir).join(&exe);
    if !bin.is_file() {
        bail!(
            "cargo reported success but binary not found: {}",
            bin.display()
        );
    }
    Ok(bin)
}

fn copy_to_project_bin(
    root: &Path,
    src: &Path,
    release: bool,
    target: Option<&str>,
) -> Result<PathBuf> {
    let profile = if release { "release" } else { "debug" };
    // With --target, segregate the produced artifact under
    // bin/<target>/<profile>/ so multiple target builds can coexist.
    let bin_dir = if let Some(t) = target {
        root.join("bin").join(t).join(profile)
    } else {
        root.join("bin").join(profile)
    };
    std::fs::create_dir_all(&bin_dir)
        .with_context(|| format!("Failed to create {}", bin_dir.display()))?;
    let file_name = src
        .file_name()
        .ok_or_else(|| anyhow!("cargo output has no file name"))?;
    let dest = bin_dir.join(file_name);
    std::fs::copy(src, &dest)
        .with_context(|| format!("Failed to copy {} to {}", src.display(), dest.display()))?;
    Ok(dest)
}

fn which_cargo() -> Result<PathBuf> {
    let name = cargo_exe_name();
    let path_var = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH unset"))?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("cargo not in PATH"))
}

fn cargo_exe_name() -> &'static str {
    if cfg!(windows) {
        "cargo.exe"
    } else {
        "cargo"
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Fail early, and in terms of the actual problem, when the project sits too
/// deep for the generated cargo workspace to build on Windows.
///
/// Without this the build gets all the way to linking and dies with
///
/// ```text
/// error: linking with `link.exe` failed: exit code: 1104
/// LINK : fatal error LNK1104: cannot open file '...'
/// ```
///
/// which names a file, not a path length, and sends you looking for a
/// missing dependency or a corrupt toolchain. Someone benchmarking JWC lost
/// a build to exactly this and worked around it by moving the project to
/// `C:\Users\<name>\jb\`.
///
/// Non-Windows targets have no equivalent limit worth pre-checking (Linux
/// allows 4096), so this compiles to nothing there.
fn check_path_length(workspace: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let len = workspace.as_os_str().len();
        if len + MAX_GENERATED_SUFFIX > WINDOWS_MAX_PATH {
            let budget = WINDOWS_MAX_PATH.saturating_sub(MAX_GENERATED_SUFFIX);
            bail!(
                "project path is too long for a native build on Windows.\n\
                 \n  build workspace: {} ({len} chars)\n  \
                 budget:          {budget} chars\n\
                 \n\
                 cargo nests build artefacts about {MAX_GENERATED_SUFFIX} characters below \
                 this directory, which crosses Windows' {WINDOWS_MAX_PATH}-character MAX_PATH. \
                 The build would reach the link step and fail with LNK1104 naming a file \
                 rather than the path length.\n\
                 \n\
                 Move the project somewhere shorter (for example C:\\src\\{}), or enable \
                 long paths system-wide:\n  \
                 reg add HKLM\\SYSTEM\\CurrentControlSet\\Control\\FileSystem \
                 /v LongPathsEnabled /t REG_DWORD /d 1 /f",
                workspace.display(),
                workspace
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("myapp"),
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = workspace;
    }
    Ok(())
}

/// Cargo's target triples follow the `<arch>-<vendor>-<sys>-<env>?`
/// convention. The third segment ("sys") is enough to tell us whether
/// the produced executable will carry a `.exe` suffix.
fn target_is_windows(target: Option<&str>) -> bool {
    match target {
        Some(t) => t.contains("-windows-"),
        None => cfg!(windows),
    }
}

/// Lower the workspace, scaffold a crate around it and let cargo build it.
pub fn compile(
    ws: &crate::workspace::Workspace,
    root: &Path,
    app_name: &str,
    release: bool,
) -> Result<CompileReport> {
    let gen = codegen::generate(ws)?;

    let cargo = find_cargo().context(
        "`cargo` not found.\n\
         The native build generates a Rust crate and hands it to cargo, so a \
         Rust toolchain has to be on PATH — https://rustup.rs/.\n\
         `jwc serve` needs no toolchain and runs the whole language.",
    )?;

    let workspace = scaffold_workspace(
        root,
        app_name,
        &gen.source,
        gen.needs_db,
        gen.needs_http_client,
        gen.needs_crypto,
        gen.needs_redis,
        gen.needs_regex,
    )?;
    let bin = invoke_cargo(&cargo, &workspace, app_name, release, None)?;
    let binary_path = copy_to_project_bin(root, &bin, release, None)?;

    Ok(CompileReport {
        binary_path,
        workspace,
    })
}

/// Run codegen only and write the Rust it produced, so the generated source
/// can be read before cargo ever sees it.
pub fn emit_rust_source(
    ws: &crate::workspace::Workspace,
    root: &Path,
    app_name: &str,
    release: bool,
) -> Result<PathBuf> {
    let rust_src = codegen::generate(ws)?.source;
    let profile = if release { "release" } else { "debug" };
    let out_dir = root.join("bin").join(profile);
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create bin/{profile}"))?;
    let out_path = out_dir.join(format!("{app_name}.generated.rs"));
    std::fs::write(&out_path, rust_src).with_context(|| format!("write {}", out_path.display()))?;
    Ok(out_path)
}

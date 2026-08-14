//! Differential harness: same program, both backends, **compiled and run**.
//!
//! ## Why this exists
//!
//! `tests/native_parity.rs` and `tests/conformance.rs` check that codegen
//! *emits* the right call shapes — they string-match the generated Rust and
//! deliberately never invoke `cargo` on it. That leaves two blind spots, and
//! v0.9.5 shipped five bugs that lived in both of them:
//!
//!   1. **Emitted-but-wrong.** `badRequest({...})` emitted a perfectly
//!      well-formed `jwc_b_bad_request(...)` call on both backends. The call
//!      shape was identical; the *runtime behaviour* differed, because the
//!      interpreter stringified its argument and native did not. No
//!      substring assertion can see that.
//!
//!   2. **The golden value was one of the backends.** `native_parity.rs`
//!      states it outright: "we treat the interpreter's stdout as the source
//!      of truth". In all five bugs the interpreter was the side that was
//!      wrong, so a harness anchored to it would have certified the bug and
//!      moved on.
//!
//! So this suite does the two things those cannot:
//!
//!   * It **builds the emitted crate with cargo and runs the binary**, then
//!     drives real HTTP requests at it — and at `jwc run` — over a socket.
//!   * It compares both backends against **expectations declared in the
//!     fixture**, never against each other. Neither backend can vote. A case
//!     where both agree and both are wrong still fails, which is precisely
//!     the case that got shipped.
//!
//! ## Cost, and how to run it
//!
//! Each case shells out to `cargo` to compile a generated crate: roughly
//! 30s–2min apiece even with a warm shared target dir. That is too slow for
//! the default `cargo test`, so the suite is opt-in:
//!
//! ```bash
//! JWC_DIFFERENTIAL=1 cargo test --test differential
//! JWC_DIFFERENTIAL=1 cargo test --test differential -- case_redirect
//! ```
//!
//! Without the variable every case prints SKIPPED and passes — the same
//! convention `integration_db.rs` uses for a missing Docker daemon. A
//! SKIPPED line is **not** a pass; CI must set the variable.
//!
//! ## Adding a case
//!
//! Drop `<name>.jwc` and `<name>.expect.json` in `tests/differential/cases/`
//! and add a `#[test] fn case_<name>()`. The program must end in a `main()`
//! that reads `PORT` from the environment and calls `serve(port)` — the
//! harness assigns a fresh port per phase. Write the expectation from what
//! the language *should* do, not from what either backend currently prints.

use std::collections::HashMap;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value as Json;

/// Native builds shell out to cargo; running several at once thrashes the
/// shared target dir and the disk. Cases are cheap to serialise — they are
/// dominated by compile time either way.
static SERIAL: Mutex<()> = Mutex::new(());

const READY_TIMEOUT: Duration = Duration::from_secs(30);

// --- process plumbing --------------------------------------------------------

/// Kills the child on drop. Without this a failed assertion leaves a server
/// holding the port and every later case in the same run fails to bind.
struct ServerGuard {
    child: Child,
    label: &'static str,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerGuard {
    /// Drains whatever the child wrote before dying. Used only on the error
    /// path — a native binary that panics on boot says why on stderr, and
    /// without this the failure surfaces as a bare connection-refused.
    fn diagnostics(&mut self) -> String {
        let mut out = String::new();
        if let Some(mut err) = self.child.stderr.take() {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            if !buf.trim().is_empty() {
                out.push_str(&format!("\n--- {} stderr ---\n{}", self.label, buf.trim()));
            }
        }
        if let Some(mut sout) = self.child.stdout.take() {
            let mut buf = String::new();
            let _ = sout.read_to_string(&mut buf);
            if !buf.trim().is_empty() {
                out.push_str(&format!("\n--- {} stdout ---\n{}", self.label, buf.trim()));
            }
        }
        out
    }
}

/// Asks the OS for an unused port, then immediately releases it. There is an
/// unavoidable race between release and the server's bind; a fresh port per
/// phase keeps it small and avoids TIME_WAIT collisions between the two
/// phases of the same case.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

fn wait_until_ready(port: u16, guard: &mut ServerGuard) -> Result<(), String> {
    let label = guard.label;
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        // A server that already exited will never bind — fail fast with its
        // output rather than burning the full timeout.
        if let Ok(Some(status)) = guard.child.try_wait() {
            let why = guard.diagnostics();
            return Err(format!(
                "{label} exited before binding (status {status}){why}"
            ));
        }
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let why = guard.diagnostics();
    Err(format!(
        "{label} never bound port {port} within {READY_TIMEOUT:?}{why}"
    ))
}

// --- fixtures ----------------------------------------------------------------

struct Expectation {
    name: String,
    method: String,
    path: String,
    body: Option<String>,
    status: u16,
    /// Compared with `serde_json` equality so key order never decides a test.
    json: Option<Json>,
    /// Recursive subset match: every key named here must match, keys not named
    /// are ignored. For envelopes where one field is worth pinning (`code`,
    /// `status`) and another is formatting detail (`details`).
    json_subset: Option<Json>,
    /// Exact string equality, for responses that are not JSON.
    text: Option<String>,
    /// Header name (lowercased) -> exact expected value.
    headers: HashMap<String, String>,
}

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("differential")
        .join("cases")
}

fn load_expectations(case: &str) -> Vec<Expectation> {
    let path = cases_dir().join(format!("{case}.expect.json"));
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: Json =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let reqs = doc["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: `requests` must be an array", path.display()));

    reqs.iter()
        .map(|r| {
            let expect = &r["expect"];
            let headers = expect["headers"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| {
                            (
                                k.to_ascii_lowercase(),
                                v.as_str()
                                    .expect("header value must be a string")
                                    .to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Expectation {
                name: r["name"].as_str().unwrap_or("<unnamed>").to_string(),
                method: r["method"].as_str().unwrap_or("GET").to_string(),
                path: r["path"].as_str().expect("`path` is required").to_string(),
                body: r["body"].as_str().map(|s| s.to_string()),
                status: expect["status"].as_u64().expect("`status` is required") as u16,
                json: expect.get("json").filter(|v| !v.is_null()).cloned(),
                json_subset: expect.get("json_subset").filter(|v| !v.is_null()).cloned(),
                text: expect["text"].as_str().map(|s| s.to_string()),
                headers,
            }
        })
        .collect()
}

// --- driving one backend -----------------------------------------------------

struct Observed {
    status: u16,
    body: String,
    headers: HashMap<String, String>,
}

fn issue(port: u16, exp: &Expectation) -> Result<Observed, String> {
    let client = reqwest::blocking::Client::builder()
        // A redirect helper is under test here; following redirects would
        // hide the very status and Location header the case asserts on.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("build client: {e}"))?;

    let url = format!("http://127.0.0.1:{port}{}", exp.path);
    let method = reqwest::Method::from_bytes(exp.method.as_bytes())
        .map_err(|e| format!("bad method {}: {e}", exp.method))?;
    let mut req = client.request(method, &url);
    if let Some(b) = &exp.body {
        req = req
            .header("content-type", "application/json")
            .body(b.clone());
    }

    let resp = req
        .send()
        .map_err(|e| format!("{} {}: {e}", exp.method, url))?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or("<non-utf8>").to_string(),
            )
        })
        .collect();
    let body = resp.text().map_err(|e| format!("read body: {e}"))?;
    Ok(Observed {
        status,
        body,
        headers,
    })
}

/// Walks `want` against `actual`, recording a message per key that is absent
/// or unequal. Keys present in `actual` but not in `want` are ignored — that
/// is what makes this a subset match.
fn subset_diff(want: &Json, actual: &Json, path: String, out: &mut Vec<String>) {
    match (want, actual) {
        (Json::Object(w), Json::Object(a)) => {
            for (k, wv) in w {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match a.get(k) {
                    Some(av) => subset_diff(wv, av, child, out),
                    None => out.push(format!("`{child}` missing, expected {wv}")),
                }
            }
        }
        (w, a) if w != a => out.push(format!("`{path}` = {a}, expected {w}")),
        _ => {}
    }
}

/// Checks one response against the fixture. Returns one line per violation so
/// a single run reports every mismatch instead of stopping at the first.
fn compare(backend: &str, exp: &Expectation, got: &Observed) -> Vec<String> {
    let mut fails = Vec::new();
    let where_ = format!("[{backend}] {} ({} {})", exp.name, exp.method, exp.path);

    if got.status != exp.status {
        fails.push(format!(
            "{where_}: status {} != expected {}\n      body: {}",
            got.status, exp.status, got.body
        ));
    }

    if let Some(want) = &exp.json {
        match serde_json::from_str::<Json>(&got.body) {
            Ok(actual) => {
                if &actual != want {
                    fails.push(format!(
                        "{where_}: json mismatch\n      expected: {want}\n      actual:   {actual}"
                    ));
                }
            }
            Err(e) => fails.push(format!(
                "{where_}: expected JSON but body did not parse ({e})\n      body: {}",
                got.body
            )),
        }
    }

    if let Some(want) = &exp.json_subset {
        match serde_json::from_str::<Json>(&got.body) {
            Ok(actual) => {
                let mut missing = Vec::new();
                subset_diff(want, &actual, String::new(), &mut missing);
                for m in missing {
                    fails.push(format!("{where_}: {m}\n      body: {}", got.body));
                }
            }
            Err(e) => fails.push(format!(
                "{where_}: expected JSON but body did not parse ({e})\n      body: {}",
                got.body
            )),
        }
    }

    if let Some(want) = &exp.text {
        if &got.body != want {
            fails.push(format!(
                "{where_}: text mismatch\n      expected: {want:?}\n      actual:   {:?}",
                got.body
            ));
        }
    }

    for (k, want) in &exp.headers {
        match got.headers.get(k) {
            Some(actual) if actual == want => {}
            Some(actual) => fails.push(format!(
                "{where_}: header `{k}` = {actual:?}, expected {want:?}"
            )),
            None => fails.push(format!(
                "{where_}: header `{k}` missing (expected {want:?}); present: {:?}",
                {
                    let mut names: Vec<_> = got.headers.keys().cloned().collect();
                    names.sort();
                    names
                }
            )),
        }
    }

    fails
}

// --- the harness -------------------------------------------------------------

fn jwc_bin() -> &'static str {
    env!("CARGO_BIN_EXE_jwc")
}

fn cargo_available() -> bool {
    Command::new("cargo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Writes the fixture into a temp project. Returned dir must outlive the run.
fn stage_project(case: &str, dir: &Path) {
    let src = cases_dir().join(format!("{case}.jwc"));
    let program =
        std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));
    std::fs::write(
        dir.join("jwcproj.json"),
        format!("{{\n  \"name\": \"{case}\",\n  \"version\": \"0.0.0\"\n}}\n"),
    )
    .expect("write jwcproj.json");
    std::fs::write(dir.join("main.jwc"), program).expect("write main.jwc");
}

fn run_interpreter(dir: &Path, port: u16) -> Result<ServerGuard, String> {
    let child = Command::new(jwc_bin())
        .arg("run")
        .arg(".")
        .current_dir(dir)
        .env("PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn `jwc run`: {e}"))?;
    Ok(ServerGuard {
        child,
        label: "interpreter",
    })
}

fn build_native(case: &str, dir: &Path, shared_target: &Path) -> Result<PathBuf, String> {
    let out = Command::new(jwc_bin())
        .arg("build")
        .arg("--native")
        .current_dir(dir)
        // One shared target dir across every case: the generated crates pull
        // the same dependency set, so this turns N full dependency builds
        // into one. Without it the suite is both far slower and a real risk
        // of filling the disk.
        .env("CARGO_TARGET_DIR", shared_target)
        .output()
        .map_err(|e| format!("spawn `jwc build --native`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`jwc build --native` failed ({}):\n{}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim(),
        ));
    }
    let bin = dir.join("bin").join("debug").join(case);
    if !bin.exists() {
        return Err(format!("native binary missing at {}", bin.display()));
    }
    Ok(bin)
}

fn run_native(bin: &Path, port: u16) -> Result<ServerGuard, String> {
    let child = Command::new(bin)
        .env("PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", bin.display()))?;
    Ok(ServerGuard {
        child,
        label: "native",
    })
}

/// Drives every request in the fixture against one already-running backend.
fn collect(backend: &str, port: u16, expectations: &[Expectation]) -> Vec<String> {
    let mut fails = Vec::new();
    for exp in expectations {
        match issue(port, exp) {
            Ok(got) => fails.extend(compare(backend, exp, &got)),
            Err(e) => fails.push(format!("[{backend}] {}: request failed: {e}", exp.name)),
        }
    }
    fails
}

fn run_case(case: &str) {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());

    if std::env::var("JWC_DIFFERENTIAL").is_err() {
        eprintln!(
            "[differential] SKIPPED {case}: set JWC_DIFFERENTIAL=1 to run \
             (each case cargo-builds a generated crate)."
        );
        return;
    }
    if !cargo_available() {
        eprintln!("[differential] SKIPPED {case}: no cargo on PATH; --native cannot build.");
        return;
    }

    let expectations = load_expectations(case);
    assert!(
        !expectations.is_empty(),
        "{case}: fixture declares no requests"
    );

    let project = tempfile::tempdir().expect("tempdir");
    stage_project(case, project.path());
    // Shared across cases within a run: see build_native.
    let shared_target = std::env::temp_dir().join("jwc-differential-target");

    let mut failures = Vec::new();

    // ── interpreter ──────────────────────────────────────────────────────
    let interp_port = free_port();
    let mut interp = match run_interpreter(project.path(), interp_port) {
        Ok(g) => g,
        Err(e) => panic!("{case}: {e}"),
    };
    match wait_until_ready(interp_port, &mut interp) {
        Ok(()) => failures.extend(collect("interpreter", interp_port, &expectations)),
        Err(e) => failures.push(format!("[interpreter] {e}")),
    }
    drop(interp);

    // ── native ───────────────────────────────────────────────────────────
    match build_native(case, project.path(), &shared_target) {
        Ok(bin) => {
            let native_port = free_port();
            match run_native(&bin, native_port) {
                Ok(mut native) => {
                    match wait_until_ready(native_port, &mut native) {
                        Ok(()) => failures.extend(collect("native", native_port, &expectations)),
                        Err(e) => failures.push(format!("[native] {e}")),
                    }
                    drop(native);
                }
                Err(e) => failures.push(format!("[native] {e}")),
            }
        }
        Err(e) => failures.push(format!("[native] {e}")),
    }

    assert!(
        failures.is_empty(),
        "\n{case}: {} expectation(s) violated\n\n{}\n",
        failures.len(),
        failures.join("\n")
    );
}

// --- cases -------------------------------------------------------------------
//
// One test per fixture so `cargo test --test differential -- case_redirect`
// isolates a single ~1min build.

#[test]
fn case_error_helpers() {
    run_case("error_helpers");
}

#[test]
fn case_redirect() {
    run_case("redirect");
}

#[test]
fn case_len_shapes() {
    run_case("len_shapes");
}

#[test]
fn case_request_body() {
    run_case("request_body");
}

#[test]
fn case_validate_body() {
    run_case("validate_body");
}

#[test]
fn case_field_write() {
    run_case("field_write");
}

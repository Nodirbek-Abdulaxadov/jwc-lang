//! `jwc login` / `publish` / `add`, against a stub registry.
//!
//! The stub speaks the three endpoints `jwc-registry` serves, so the client
//! exercises its real HTTP path — multipart upload, Bearer auth, the
//! metadata request, the download, the checksum, the unpack. A stub also
//! makes the cases a real registry cannot produce on demand testable: a
//! tampered body, and an archive with a `..` in it.

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::routing::{get, post};
use axum::Json;
use serde_json::json;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Registry {
    /// The last upload: (name, version, bytes, bearer token).
    uploaded: Option<(String, String, Vec<u8>, String)>,
    /// What `add` will be served, and the sha256 the metadata advertises.
    serving: Option<(Vec<u8>, String)>,
}

type Shared = Arc<Mutex<Registry>>;

/// The single part's payload, without pulling in a multipart parser for
/// fifteen lines of framing: the bytes between the blank line that ends the
/// part headers and the closing boundary.
fn multipart_file(body: &[u8], boundary: &str) -> Vec<u8> {
    let marker = format!("\r\n--{boundary}");
    let start = match find(body, b"\r\n\r\n") {
        Some(i) => i + 4,
        None => return Vec::new(),
    };
    let end = find(&body[start..], marker.as_bytes())
        .map(|i| start + i)
        .unwrap_or(body.len());
    body[start..end].to_vec()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn upload(
    State(state): State<Shared>,
    AxPath((name, version)): AxPath<(String, String)>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Json<serde_json::Value> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let boundary = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("boundary=").nth(1))
        .unwrap_or_default()
        .to_string();
    let bytes = multipart_file(&body, &boundary);
    let sha = jwc::hash::sha256_hex_bytes(&bytes);
    let len = bytes.len();
    state.lock().expect("lock").uploaded = Some((name.clone(), version.clone(), bytes, token));
    Json(json!({ "name": name, "version": version, "sha256": sha, "size_bytes": len }))
}

async fn meta(
    State(state): State<Shared>,
    AxPath(name): AxPath<String>,
) -> Json<serde_json::Value> {
    let sha = state
        .lock()
        .expect("lock")
        .serving
        .as_ref()
        .map(|(_, s)| s.clone())
        .unwrap_or_default();
    Json(json!({
        "name": name,
        "owner_email": "someone@example.com",
        "versions": [
            { "version": "0.2.0", "sha256": sha, "size_bytes": 1, "uploaded_at": "2026-01-01T00:00:00Z" },
            { "version": "0.1.0", "sha256": sha, "size_bytes": 1, "uploaded_at": "2025-01-01T00:00:00Z" }
        ]
    }))
}

async fn download(State(state): State<Shared>) -> Vec<u8> {
    state
        .lock()
        .expect("lock")
        .serving
        .as_ref()
        .map(|(b, _)| b.clone())
        .unwrap_or_default()
}

struct Stub {
    port: u16,
    state: Shared,
    _rt: tokio::runtime::Runtime,
}

impl Stub {
    fn start() -> Stub {
        let state: Shared = Default::default();
        let app = axum::Router::new()
            .route("/api/v1/pkg/:name/:version", post(upload))
            .route("/api/v1/pkg/:name", get(meta))
            .route("/api/v1/pkg/:name/:version/download", get(download))
            .with_state(state.clone());

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        rt.spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Stub {
            port,
            state,
            _rt: rt,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn jwc(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run jwc")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn package(dir: &Path, name: &str, kind: &str) {
    std::fs::write(
        dir.join("jwcproj.json"),
        format!(r#"{{ "name": "{name}", "version": "0.1.0", "type": "{kind}" }}"#),
    )
    .expect("manifest");
    std::fs::write(dir.join("main.jwc"), format!("namespace {name};\n")).expect("source");
    std::fs::write(dir.join(".env"), "SECRET=hunter2").expect("env");
}

#[test]
fn publish_uploads_only_sources_and_add_brings_them_back() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    let pkg = tempfile::tempdir().expect("tempdir");
    package(pkg.path(), "demo", "pkg");

    // A dry run lists what would go, and nothing leaves.
    let dry = jwc(
        &[
            "publish",
            pkg.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
            "--dry-run",
        ],
        home.path(),
    );
    assert!(dry.status.success(), "{}", text(&dry));
    let listed = text(&dry);
    assert!(
        listed.contains("jwcproj.json") && listed.contains("main.jwc"),
        "{listed}"
    );
    // A package is source. Shipping whatever is in the directory is how a
    // `.env` reaches a registry.
    assert!(!listed.contains(".env"), "{listed}");

    // Not logged in yet.
    let out = jwc(
        &[
            "publish",
            pkg.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("not logged in"), "{}", text(&out));

    let login = jwc(
        &["login", "--token", "jwc_secret", "--registry", &stub.url()],
        home.path(),
    );
    assert!(login.status.success(), "{}", text(&login));
    // The file holds a bearer token.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.path().join(".jwc/credentials.json"))
            .expect("credentials")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "credentials are world-readable");
    }

    let out = jwc(
        &[
            "publish",
            pkg.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(out.status.success(), "{}", text(&out));

    let (name, version, bytes, token) = stub
        .state
        .lock()
        .expect("lock")
        .uploaded
        .clone()
        .expect("nothing was uploaded");
    assert_eq!((name.as_str(), version.as_str()), ("demo", "0.1.0"));
    assert_eq!(token, "Bearer jwc_secret");

    // Now serve that same archive back and install it into an app.
    let sha = jwc::hash::sha256_hex_bytes(&bytes);
    stub.state.lock().expect("lock").serving = Some((bytes, sha));

    let app = tempfile::tempdir().expect("tempdir");
    package(app.path(), "myapp", "app");
    let out = jwc(
        &[
            "add",
            "demo",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(out.status.success(), "{}", text(&out));
    // No version given, so the newest the registry lists.
    assert!(text(&out).contains("added demo 0.2.0"), "{}", text(&out));

    let vendored = app.path().join("jwc_packages/demo");
    assert!(
        vendored.join("main.jwc").is_file(),
        "sources were not unpacked"
    );
    assert!(vendored.join("jwcproj.json").is_file());

    // And the manifest records it, keeping everything else in the file.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(app.path().join("jwcproj.json")).expect("read"),
    )
    .expect("json");
    assert_eq!(manifest["dependencies"]["demo"], "^0.2.0");
    assert_eq!(
        manifest["name"], "myapp",
        "the rest of the manifest was lost"
    );
}

#[test]
fn a_tampered_download_is_refused() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");

    // The advertised sha256 comes from the *metadata* request; the bytes
    // come from the download. A client that trusted a checksum carried by
    // the same response would be verifying nothing.
    stub.state.lock().expect("lock").serving = Some((
        b"not a tarball".to_vec(),
        jwc::hash::sha256_hex_bytes(b"something else"),
    ));

    let app = tempfile::tempdir().expect("tempdir");
    package(app.path(), "myapp", "app");
    let out = jwc(
        &[
            "add",
            "demo",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(!out.status.success(), "a tampered body was accepted");
    assert!(text(&out).contains("checksum mismatch"), "{}", text(&out));
    assert!(
        !app.path().join("jwc_packages/demo/main.jwc").exists(),
        "it unpacked anyway"
    );
}

#[test]
fn an_archive_that_escapes_its_directory_is_refused() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");

    // The registry does not produce one of these. A registry is not the
    // only thing that can serve a `.tar.gz` over a URL.
    let mut tar = tar::Builder::new(Vec::new());
    let payload = b"namespace evil;\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(payload.len() as u64);
    header.set_mode(0o644);
    // `set_path` refuses a `..`, which is the point: an attacker writing
    // the archive is not using this library's front door. The name goes
    // into the header bytes directly.
    let name = b"../escaped.jwc";
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_cksum();
    tar.append(&header, &payload[..]).expect("append");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar.into_inner().expect("tar")).expect("gz");
    let bytes = gz.finish().expect("gz");
    let sha = jwc::hash::sha256_hex_bytes(&bytes);
    stub.state.lock().expect("lock").serving = Some((bytes, sha));

    let app = tempfile::tempdir().expect("tempdir");
    package(app.path(), "myapp", "app");
    let out = jwc(
        &[
            "add",
            "demo",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(!out.status.success(), "an escaping archive was unpacked");
    assert!(text(&out).contains("unsafe path"), "{}", text(&out));
    assert!(!app
        .path()
        .parent()
        .expect("parent")
        .join("escaped.jwc")
        .exists());
}

#[test]
fn only_a_package_is_published_and_only_under_a_name_a_program_can_import() {
    let home = tempfile::tempdir().expect("tempdir");

    let app = tempfile::tempdir().expect("tempdir");
    package(app.path(), "myapp", "app");
    let out = jwc(
        &["publish", app.path().to_str().expect("utf8")],
        home.path(),
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("is an application"), "{}", text(&out));

    // packages.md §1.2 — the registry's name rule allows a hyphen and
    // `import jwc-redis;` does not parse. A registry name is permanent, so
    // the refusal has to happen before it is taken.
    let pkg = tempfile::tempdir().expect("tempdir");
    package(pkg.path(), "jwc-redis", "pkg");
    let out = jwc(
        &["publish", pkg.path().to_str().expect("utf8")],
        home.path(),
    );
    assert!(!out.status.success());
    assert!(
        text(&out).contains("must also be an identifier"),
        "{}",
        text(&out)
    );
}

/// Build a `.tar.gz` holding one manifest and one source, and serve it.
fn serve_archive(stub: &Stub, manifest: &str) {
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |name: &str, body: &str| {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, name, body.as_bytes())
            .expect("append");
    };
    add("jwcproj.json", manifest);
    add("main.jwc", "namespace demo;\n");
    let bytes = {
        let raw = tar.into_inner().expect("tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        gz.write_all(&raw).expect("gz");
        gz.finish().expect("gz")
    };
    let sha = jwc::hash::sha256_hex_bytes(&bytes);
    stub.state.lock().expect("lock").serving = Some((bytes, sha));
}

fn app_with_deps(dir: &Path, deps: &str) {
    std::fs::write(
        dir.join("jwcproj.json"),
        format!(
            r#"{{ "name": "myapp", "version": "0.1.0", "type": "app", "dependencies": {deps} }}"#
        ),
    )
    .expect("manifest");
    std::fs::write(dir.join("main.jwc"), "namespace myapp;\n").expect("source");
}

/// `jwc install` is what a fresh clone needs: `jwc_packages/` is a build
/// artefact for most projects, so a checkout has the manifest and none of
/// the sources, and every package `import` fails on something that looks
/// correct in the file.
#[test]
fn install_fetches_what_the_manifest_declares_and_respects_the_range() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    serve_archive(
        &stub,
        r#"{ "name": "demo", "version": "0.1.0", "type": "pkg" }"#,
    );

    let app = tempfile::tempdir().expect("tempdir");
    app_with_deps(app.path(), r#"{ "demo": "^0.1.0" }"#);
    assert!(
        !app.path().join("jwc_packages/demo/main.jwc").exists(),
        "the fixture starts with nothing vendored"
    );

    let out = jwc(
        &[
            "install",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        app.path().join("jwc_packages/demo/main.jwc").exists(),
        "{}",
        text(&out)
    );
    // The stub serves 0.2.0 and 0.1.0, newest first. `^0.1.0` on a 0.x
    // version is `>=0.1.0, <0.2.0`, so taking the newest would be wrong.
    assert!(
        text(&out).contains("demo 0.1.0"),
        "the range was ignored: {}",
        text(&out)
    );
}

/// A second run must not re-download what is already there — that is the
/// difference between `install` and a loop of `add`, and the reason it is
/// safe to put in a build script.
#[test]
fn install_leaves_a_present_package_alone_unless_forced() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    serve_archive(
        &stub,
        r#"{ "name": "demo", "version": "0.1.0", "type": "pkg" }"#,
    );

    let app = tempfile::tempdir().expect("tempdir");
    app_with_deps(app.path(), r#"{ "demo": "^0.1.0" }"#);
    let args = [
        "install",
        app.path().to_str().expect("utf8"),
        "--registry",
        &stub.url(),
    ];
    assert!(jwc(&args, home.path()).status.success());

    // A file the archive does not carry: if the second run re-unpacked, it
    // would be gone, because unpacking clears the directory first.
    let marker = app.path().join("jwc_packages/demo/LOCAL");
    std::fs::write(&marker, "mine").expect("write");

    let again = jwc(&args, home.path());
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("already present"), "{}", text(&again));
    assert!(marker.exists(), "the second install re-downloaded");

    let forced = jwc(
        &[
            "install",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
            "--force",
        ],
        home.path(),
    );
    assert!(forced.status.success(), "{}", text(&forced));
    assert!(!marker.exists(), "--force did not re-download");
}

/// A vendored package's own manifest may declare dependencies, and its
/// imports resolve against them, so `install` follows them. The stub
/// serves the same archive under every name, so this also walks a cycle —
/// which must terminate rather than fetch forever.
#[test]
fn install_follows_a_packages_own_dependencies_without_looping() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    serve_archive(
        &stub,
        r#"{ "name": "demo", "version": "0.1.0", "type": "pkg", "dependencies": { "other": "^0.1.0" } }"#,
    );

    let app = tempfile::tempdir().expect("tempdir");
    app_with_deps(app.path(), r#"{ "demo": "^0.1.0" }"#);

    let out = jwc(
        &[
            "install",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        app.path().join("jwc_packages/demo/main.jwc").exists(),
        "{}",
        text(&out)
    );
    assert!(
        app.path().join("jwc_packages/other/main.jwc").exists(),
        "a package's own dependency was not followed: {}",
        text(&out)
    );
}

/// `install` must not silently take whatever shipped today when the
/// recorded range is a typo.
#[test]
fn an_unparseable_requirement_is_an_error_not_a_shrug() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    serve_archive(
        &stub,
        r#"{ "name": "demo", "version": "0.1.0", "type": "pkg" }"#,
    );

    let app = tempfile::tempdir().expect("tempdir");
    app_with_deps(app.path(), r#"{ "demo": "latest" }"#);

    let out = jwc(
        &[
            "install",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(
        !out.status.success(),
        "`latest` was accepted: {}",
        text(&out)
    );
    assert!(text(&out).contains("semver range"), "{}", text(&out));
    assert!(!app.path().join("jwc_packages/demo").exists());
}

/// `jwc update` moves within the recorded range and not past it. Crossing
/// a major is `jwc add name@version` — a change to the requirement, and
/// one that says so in the diff.
#[test]
fn update_moves_within_the_range_and_records_what_it_installed() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    serve_archive(
        &stub,
        r#"{ "name": "demo", "version": "0.1.0", "type": "pkg" }"#,
    );

    let app = tempfile::tempdir().expect("tempdir");
    app_with_deps(app.path(), r#"{ "demo": "^0.2.0" }"#);

    let out = jwc(
        &[
            "update",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("demo 0.2.0"), "{}", text(&out));

    let manifest = std::fs::read_to_string(app.path().join("jwcproj.json")).expect("manifest");
    assert!(
        manifest.contains("\"demo\": \"^0.2.0\""),
        "the manifest should record what was installed: {manifest}"
    );

    // A name that is not a dependency is a mistake worth naming.
    let bad = jwc(
        &[
            "update",
            app.path().to_str().expect("utf8"),
            "--package",
            "nosuch",
            "--registry",
            &stub.url(),
        ],
        home.path(),
    );
    assert!(!bad.status.success());
    assert!(text(&bad).contains("is not a dependency"), "{}", text(&bad));
}

/// `remove` and `tree` never touch the network.
#[test]
fn remove_and_tree_work_offline() {
    let stub = Stub::start();
    let home = tempfile::tempdir().expect("tempdir");
    serve_archive(
        &stub,
        r#"{ "name": "demo", "version": "0.1.0", "type": "pkg" }"#,
    );

    let app = tempfile::tempdir().expect("tempdir");
    app_with_deps(app.path(), r#"{ "demo": "^0.1.0", "other": "^0.1.0" }"#);

    // `tree` before anything is installed: it says so, which is the state
    // `install` exists to fix.
    let before = jwc(&["tree", app.path().to_str().expect("utf8")], home.path());
    assert!(before.status.success(), "{}", text(&before));
    assert!(text(&before).contains("not installed"), "{}", text(&before));

    assert!(jwc(
        &[
            "install",
            app.path().to_str().expect("utf8"),
            "--registry",
            &stub.url(),
        ],
        home.path(),
    )
    .status
    .success());

    let after = jwc(&["tree", app.path().to_str().expect("utf8")], home.path());
    assert!(after.status.success(), "{}", text(&after));
    assert!(text(&after).contains("myapp"), "{}", text(&after));
    assert!(text(&after).contains("demo 0.1.0"), "{}", text(&after));

    // No `--registry`, and the stub would answer if one were contacted.
    let rm = jwc(
        &["remove", "demo", app.path().to_str().expect("utf8")],
        home.path(),
    );
    assert!(rm.status.success(), "{}", text(&rm));
    assert!(
        !app.path().join("jwc_packages/demo").exists(),
        "the sources are still on disk"
    );
    let manifest = std::fs::read_to_string(app.path().join("jwcproj.json")).expect("manifest");
    assert!(!manifest.contains("\"demo\""), "{manifest}");
    assert!(
        manifest.contains("\"other\""),
        "it removed the wrong one: {manifest}"
    );

    let twice = jwc(
        &["remove", "demo", app.path().to_str().expect("utf8")],
        home.path(),
    );
    assert!(!twice.status.success());
    assert!(
        text(&twice).contains("is not a dependency"),
        "{}",
        text(&twice)
    );
}

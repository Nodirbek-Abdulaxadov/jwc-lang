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
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
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

async fn meta(State(state): State<Shared>, AxPath(name): AxPath<String>) -> Json<serde_json::Value> {
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
    assert!(listed.contains("jwcproj.json") && listed.contains("main.jwc"), "{listed}");
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
    assert!(vendored.join("main.jwc").is_file(), "sources were not unpacked");
    assert!(vendored.join("jwcproj.json").is_file());

    // And the manifest records it, keeping everything else in the file.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(app.path().join("jwcproj.json")).expect("read"))
            .expect("json");
    assert_eq!(manifest["dependencies"]["demo"], "^0.2.0");
    assert_eq!(manifest["name"], "myapp", "the rest of the manifest was lost");
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
    assert!(!app.path().parent().expect("parent").join("escaped.jwc").exists());
}

#[test]
fn only_a_package_is_published_and_only_under_a_name_a_program_can_import() {
    let home = tempfile::tempdir().expect("tempdir");

    let app = tempfile::tempdir().expect("tempdir");
    package(app.path(), "myapp", "app");
    let out = jwc(&["publish", app.path().to_str().expect("utf8")], home.path());
    assert!(!out.status.success());
    assert!(text(&out).contains("is an application"), "{}", text(&out));

    // packages.md §1.2 — the registry's name rule allows a hyphen and
    // `import jwc-redis;` does not parse. A registry name is permanent, so
    // the refusal has to happen before it is taken.
    let pkg = tempfile::tempdir().expect("tempdir");
    package(pkg.path(), "jwc-redis", "pkg");
    let out = jwc(&["publish", pkg.path().to_str().expect("utf8")], home.path());
    assert!(!out.status.success());
    assert!(text(&out).contains("must also be an identifier"), "{}", text(&out));
}

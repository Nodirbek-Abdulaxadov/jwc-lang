//! `static "/assets" from "public";` — routing.md §10.
//!
//! Two halves. The first is what the mount answers, through the real
//! `serve::handle`: statuses, headers, bytes. The second is what it refuses
//! to be declared as, through `wiring`.
//!
//! The traversal cases are the reason the module exists, so they are
//! written as URLs rather than as calls to the resolver: a rule that holds
//! for `assets::safe_relative` and not for the path that reaches it is not
//! a rule.

use jwc::exec::Response;
use jwc::serve::{self, Incoming};
use jwc::workspace::Workspace;
use std::collections::HashMap;
use std::sync::Arc;

/// A project with a `public/` tree, on disk, because a mount is resolved
/// against the filesystem and there is no way to fake that honestly.
struct Project {
    _dir: tempfile::TempDir,
    program: Arc<jwc::exec::Program>,
}

fn project(source: &str, files: &[(&str, &[u8])]) -> Project {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, body) in files {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, body).expect("write");
    }
    std::fs::write(dir.path().join("a.jwc"), source).expect("write");
    let ws = Workspace::load(dir.path()).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let program = Arc::new(serve::load(&ws).unwrap_or_else(|e| panic!("{e}")));
    Project { _dir: dir, program }
}

async fn get(p: &Project, path: &str) -> Response {
    call(p, "GET", path, &[]).await
}

async fn call(p: &Project, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    serve::handle(
        p.program.clone(),
        Incoming {
            method: method.to_string(),
            path: path.to_string(),
            query: Vec::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_lowercase(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            body: Vec::new(),
            peer_ip: "203.0.113.7".into(),
        },
    )
    .await
}

fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

const MOUNT: &str = "namespace s;\n\
                     static \"/assets\" from \"public\" cache 600;\n\
                     routes \"/assets\" {\n\
                     \x20   route GET \"declared\" { return json({ route: true }); }\n\
                     }\n";

/// A PNG header, a NUL and a high byte: nothing here survives a trip
/// through `String`, which is the point.
const BINARY: &[u8] = b"\x89PNG\r\n\x1a\n\x00\xff\xfeend";

fn tree() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("public/index.html", b"<!doctype html><h1>root</h1>" as &[u8]),
        ("public/app.js", b"console.log(1)"),
        ("public/logo.png", BINARY),
        ("public/sub/index.html", b"<i>sub</i>"),
        ("public/.env", b"SECRET=1"),
        ("secret.txt", b"NOT PUBLISHED"),
    ]
}

#[tokio::test]
async fn a_file_comes_back_with_its_type_its_validator_and_no_sniffing() {
    let p = project(MOUNT, &tree());
    let r = get(&p, "/assets/app.js").await;

    assert_eq!(r.status, 200);
    assert_eq!(r.bytes.as_deref(), Some(b"console.log(1)" as &[u8]));
    assert_eq!(header(&r, "content-type"), Some("text/javascript; charset=utf-8"));
    assert_eq!(header(&r, "cache-control"), Some("public, max-age=600"));
    assert_eq!(header(&r, "x-content-type-options"), Some("nosniff"));
    assert!(
        header(&r, "etag").is_some_and(|e| e.starts_with('"')),
        "a strong validator"
    );
}

#[tokio::test]
async fn bytes_that_are_not_text_survive_the_response() {
    let p = project(MOUNT, &tree());
    let r = get(&p, "/assets/logo.png").await;

    assert_eq!(r.status, 200);
    // The whole reason `Response` carries bytes: `String::from_utf8_lossy`
    // would have replaced `\xff` with U+FFFD and shipped a corrupt image
    // with a 200 on it.
    assert_eq!(r.bytes.as_deref(), Some(BINARY));
    assert_eq!(header(&r, "content-type"), Some("image/png"));
}

#[tokio::test]
async fn a_directory_answers_its_index_at_the_root_and_below() {
    let p = project(MOUNT, &tree());
    for path in ["/assets", "/assets/", "/assets/index.html"] {
        let r = get(&p, path).await;
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(
            r.bytes.as_deref(),
            Some(b"<!doctype html><h1>root</h1>" as &[u8]),
            "{path}"
        );
    }
    let r = get(&p, "/assets/sub").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.bytes.as_deref(), Some(b"<i>sub</i>" as &[u8]));
}

#[tokio::test]
async fn nothing_above_the_root_is_reachable_by_any_spelling() {
    let p = project(MOUNT, &tree());
    for path in [
        "/assets/../secret.txt",
        "/assets/%2e%2e/secret.txt",
        "/assets/..%2fsecret.txt",
        "/assets/%2E%2E%2Fsecret.txt",
        "/assets/sub/../../secret.txt",
        "/assets/....//secret.txt",
    ] {
        let r = get(&p, path).await;
        assert_eq!(r.status, 404, "{path} reached something");
        assert!(r.bytes.is_none(), "{path} answered with a file");
    }
}

#[tokio::test]
async fn a_dotfile_inside_the_root_is_not_published() {
    let p = project(MOUNT, &tree());
    let r = get(&p, "/assets/.env").await;
    assert_eq!(r.status, 404);
    assert!(r.bytes.is_none());
}

#[tokio::test]
async fn a_neighbouring_path_that_starts_the_same_is_not_the_mount() {
    let p = project(MOUNT, &tree());
    // `/assetsx` is not under `/assets`, so this is an ordinary miss and
    // never a lookup in the tree.
    assert_eq!(get(&p, "/assetsx/app.js").await.status, 404);
}

#[tokio::test]
async fn a_declared_route_wins_over_the_mount_it_sits_inside() {
    let p = project(MOUNT, &tree());
    let r = get(&p, "/assets/declared").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "{\"route\":true}");
    assert!(r.bytes.is_none(), "the route answered, not the mount");
}

#[tokio::test]
async fn a_mount_at_the_root_cannot_take_the_operational_paths_away() {
    let p = project(
        "namespace s;\nstatic \"/\" from \"public\";\n",
        &[
            ("public/index.html", b"<!doctype html>" as &[u8]),
            ("public/healthz", b"NOT THE PROBE"),
        ],
    );
    // config.md §4.0.2 promises these are reachable before reading anyone's
    // source. A file that happens to be named `healthz` does not change it.
    let r = get(&p, "/healthz").await;
    assert_eq!(r.status, 200);
    assert!(
        r.bytes.is_none(),
        "the probe answered, not a file that shares its name"
    );
    assert_eq!(get(&p, "/").await.status, 200);
}

#[tokio::test]
async fn a_matching_validator_is_a_304_with_no_body() {
    let p = project(MOUNT, &tree());
    let first = get(&p, "/assets/app.js").await;
    let tag = header(&first, "etag").expect("etag").to_string();

    for candidate in [tag.clone(), "*".to_string(), format!("W/{tag}")] {
        let r = call(&p, "GET", "/assets/app.js", &[("if-none-match", &candidate)]).await;
        assert_eq!(r.status, 304, "{candidate}");
        assert!(r.bytes.is_none() && r.body.is_empty(), "{candidate}");
        assert_eq!(header(&r, "etag"), Some(tag.as_str()), "{candidate}");
    }

    let r = call(&p, "GET", "/assets/app.js", &[("if-none-match", "\"other\"")]).await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn head_is_the_headers_of_the_get_with_the_length_and_no_body() {
    let p = project(MOUNT, &tree());
    let r = call(&p, "HEAD", "/assets/app.js", &[]).await;

    assert_eq!(r.status, 200);
    assert!(r.bytes.is_none() && r.body.is_empty());
    assert_eq!(header(&r, "content-length"), Some("14"));
    assert_eq!(header(&r, "content-type"), Some("text/javascript; charset=utf-8"));
}

#[tokio::test]
async fn a_write_to_a_mounted_path_is_a_method_error_not_a_missing_page() {
    let p = project(MOUNT, &tree());
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let r = call(&p, method, "/assets/app.js", &[]).await;
        assert_eq!(r.status, 405, "{method}");
        assert_eq!(header(&r, "allow"), Some("GET, HEAD"), "{method}");
    }
}

// ---------------------------------------------------------------- declaring

/// The diagnostics a bad mount produces, through the real `wiring` pass.
fn diagnose(source: &str, files: &[(&str, &[u8])]) -> Vec<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, body) in files {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, body).expect("write");
    }
    std::fs::write(dir.path().join("a.jwc"), source).expect("write");
    let ws = Workspace::load(dir.path()).expect("load");
    let built = jwc::model::build(&ws);
    let sym = jwc::symbols::build(&ws, &built.model);
    jwc::wiring::wire(&ws, &sym)
        .diags
        .into_iter()
        .map(|(_, d)| d.code.to_string())
        .collect()
}

#[tokio::test]
async fn a_mount_that_cannot_work_is_reported_when_the_program_is_checked() {
    let dir_only: &[(&str, &[u8])] = &[("public/a.txt", b"a")];

    // §10.1 — a prefix is literal.
    assert!(diagnose(
        "namespace s;\nstatic \"assets\" from \"public\";\n",
        dir_only
    )
    .contains(&"E0740".to_string()));
    assert!(diagnose(
        "namespace s;\nstatic \"/a/{id}\" from \"public\";\n",
        dir_only
    )
    .contains(&"E0740".to_string()));

    // §10.3 — the directory has to be there at check time, not at the first
    // request that misses.
    assert!(diagnose(
        "namespace s;\nstatic \"/a\" from \"nope\";\n",
        dir_only
    )
    .contains(&"E0741".to_string()));
    assert!(
        diagnose(
            "namespace s;\nstatic \"/a\" from \"public/a.txt\";\n",
            dir_only
        )
        .contains(&"E0741".to_string()),
        "a file is not a tree"
    );

    // §10.2 — one directory per prefix, and `/a/` is `/a`.
    assert!(diagnose(
        "namespace s;\nstatic \"/a\" from \"public\";\nstatic \"/a/\" from \"public\";\n",
        dir_only
    )
    .contains(&"E0742".to_string()));

    // §10.4 — a year is the ceiling.
    assert!(diagnose(
        "namespace s;\nstatic \"/a\" from \"public\" cache 99999999;\n",
        dir_only
    )
    .contains(&"E0743".to_string()));

    // §10.3 — `jwc build` embeds the tree, so it may not be one the project
    // does not own.
    assert!(diagnose(
        "namespace s;\nstatic \"/a\" from \"..\";\n",
        dir_only
    )
    .contains(&"E0744".to_string()));

    // And the shape that is fine stays fine.
    assert!(diagnose(
        "namespace s;\nstatic \"/a\" from \"public\" cache 3600;\n",
        dir_only
    )
    .is_empty());
}

#[test]
fn the_walk_that_fills_the_binary_publishes_what_the_request_path_would() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("public");
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    std::fs::create_dir_all(root.join(".git")).expect("mkdir");
    std::fs::write(root.join("app.js"), b"a").expect("write");
    std::fs::write(root.join("sub/b.css"), b"b").expect("write");
    std::fs::write(root.join(".env"), b"SECRET").expect("write");
    std::fs::write(root.join(".git/config"), b"c").expect("write");
    std::fs::write(dir.path().join("outside.txt"), b"o").expect("write");

    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path().join("outside.txt"), root.join("link.txt"))
        .expect("symlink");

    let names: Vec<String> = jwc::assets::walk(&root).into_iter().map(|(r, _)| r).collect();

    // `jwc serve` answers 404 for every one of these, so `jwc build` must
    // not put them in the binary — an unreachable copy of `.env` inside a
    // shipped artifact is still a copy of `.env`.
    assert_eq!(names, vec!["app.js".to_string(), "sub/b.css".to_string()]);
    assert!(!names.iter().any(|n| n.contains("link")), "{names:?}");
}

#[test]
fn the_walk_is_sorted_so_the_generated_table_is_reproducible() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for name in ["z.txt", "a.txt", "m.txt"] {
        std::fs::write(root.join(name), b"x").expect("write");
    }
    let names: Vec<String> = jwc::assets::walk(root).into_iter().map(|(r, _)| r).collect();
    assert_eq!(names, vec!["a.txt", "m.txt", "z.txt"]);
}

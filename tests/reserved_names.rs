//! A physical name that is a reserved SQL word, through every write path
//! (names.md §4.2, schema.md §4.2).
//!
//! `as "…"` exists so a program can keep the physical names a database
//! already has — the first thing a port off an older version needs. Two
//! things broke on it, and both needed a real statement to show:
//!
//!   * a foreign key naming a renamed table did not resolve, so
//!     `references App.s.Users` was `E0422` against a program that declares
//!     exactly that table;
//!   * `RETURNING` exposes the target under its own name, and the
//!     projection built `json_build_object('id', user.id)` unquoted, where
//!     `user` is the SQL `USER` function — "syntax error at or near `.`".
//!
//! Both were found porting MyWallet, whose tables are `user`, `wallet`,
//! `category` and `transaction`. Every read path already quoted, which is
//! why the select half was fine and the register endpoint was a 500.
//!
//! Requires Postgres. Set `JWC_V1_DATABASE_URL`. **A SKIPPED line is not a
//! pass.**

use jwc::serve::{self, Incoming};
use jwc::workspace::Workspace;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

async fn call(
    program: Arc<jwc::exec::Program>,
    method: &str,
    path: &str,
    body: &str,
) -> jwc::exec::Response {
    serve::handle(
        program,
        Incoming {
            method: method.to_string(),
            path: path.to_string(),
            query: Vec::new(),
            headers: HashMap::new(),
            body: body.as_bytes().to_vec(),
            peer_ip: "127.0.0.1".into(),
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reserved_physical_name_survives_insert_update_and_delete() {
    let Ok(url) = std::env::var("JWC_V1_DATABASE_URL") else {
        eprintln!(
            "SKIPPED a_reserved_physical_name_survives_insert_update_and_delete — \
             set JWC_V1_DATABASE_URL. A SKIPPED line is not a pass."
        );
        return;
    };

    let ws = Workspace::load(repo_root().join("tests/reserved_names")).expect("load");
    let built = jwc::model::build(&ws);
    assert!(
        !built.diags.iter().any(|(_, d)| d.code.starts_with('E')),
        "the fixture must compile — a foreign key to a renamed table is the \
         first half of this test:\n{:#?}",
        built.diags
    );

    let ddl = jwc::ddl::render(&ws, &jwc::ddl::emit(&built.model), false);
    let reset = format!("DROP SCHEMA IF EXISTS s CASCADE;\n{ddl}");
    let out = std::process::Command::new("psql")
        .arg(&url)
        .args(["-q", "-v", "ON_ERROR_STOP=1", "-c", &reset])
        .output()
        .expect("psql");
    assert!(
        out.status.success(),
        "could not prepare the database: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    jwc::engine::init_engine(&url).expect("engine");
    let program = Arc::new(serve::load(&ws).expect("the fixture must compile"));

    // INSERT … RETURNING — the statement that used to be a 500.
    let made = call(
        program.clone(),
        "POST",
        "/accounts",
        "{\"email\":\"a@b.uz\"}",
    )
    .await;
    assert_eq!(made.status, 201, "body was: {}", made.body);
    assert!(made.body.contains("\"email\":\"a@b.uz\""), "{}", made.body);

    // UPDATE … RETURNING.
    let renamed = call(
        program.clone(),
        "PATCH",
        "/accounts/1",
        "{\"email\":\"c@d.uz\"}",
    )
    .await;
    assert_eq!(renamed.status, 200, "body was: {}", renamed.body);
    assert!(
        renamed.body.contains("\"email\":\"c@d.uz\""),
        "{}",
        renamed.body
    );

    // DELETE … RETURNING.
    let gone = call(program.clone(), "DELETE", "/accounts/1", "").await;
    assert_eq!(gone.status, 200, "body was: {}", gone.body);

    // And the row is really gone, so the delete was not silently a no-op.
    let again = call(program, "DELETE", "/accounts/1", "").await;
    assert_eq!(again.status, 404, "body was: {}", again.body);
}

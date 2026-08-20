//! The `raw()` escape hatch, executed (writes.md §6).
//!
//! The sample has no `raw()` — zero is the healthy number, and
//! `jwc v1 explain` prints the count so it stays visible. That leaves the
//! valve with nothing exercising it, which is how a valve rusts shut. This
//! drives one through the real pipeline: a window function, which is
//! exactly what the construct exists for, since the query compiler has no
//! syntax for one and is not growing one.
//!
//! Requires Postgres. Set `JWC_V1_DATABASE_URL`. **A SKIPPED line is not a
//! pass.**

use jwc::v1::serve::{self, Incoming};
use jwc::v1::workspace::Workspace;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_raw_valve_runs() {
    let Ok(url) = std::env::var("JWC_V1_DATABASE_URL") else {
        eprintln!(
            "SKIPPED the_raw_valve_runs — set JWC_V1_DATABASE_URL. \
             A SKIPPED line is not a pass."
        );
        return;
    };

    let ws = Workspace::load(repo_root().join("tests/raw_hatch")).expect("load");
    let built = jwc::v1::model::build(&ws);
    let ddl = jwc::v1::ddl::render(&ws, &jwc::v1::ddl::emit(&built.model), false);
    let reset = format!(
        "DROP SCHEMA IF EXISTS s CASCADE;\n{ddl}\n\
         INSERT INTO s.notes (org_id, body) VALUES (1, 'a'), (1, 'b'), (2, 'c');"
    );
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

    let r = serve::handle(
        program,
        Incoming {
            method: "GET".into(),
            path: "/reports/1".into(),
            query: Vec::new(),
            headers: HashMap::new(),
            body: Vec::new(),
            peer_ip: "203.0.113.7".into(),
        },
    )
    .await;

    assert_eq!(r.status, 200, "{}", r.body);
    let rows: serde_json::Value = serde_json::from_str(&r.body).expect("json");
    let rows = rows.as_array().expect("an array");
    // Two of the three notes: the `{}` was bound, not interpolated, and it
    // scoped the query to org 1.
    assert_eq!(rows.len(), 2, "{}", r.body);
    assert_eq!(rows[0]["body"], "a", "{}", r.body);
    // The window function came through — the thing the compiler cannot
    // express and the valve exists for.
    assert_eq!(rows[0]["rn"], 1, "{}", r.body);
    assert_eq!(rows[1]["rn"], 2, "{}", r.body);
}

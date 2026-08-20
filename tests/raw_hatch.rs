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

use jwc::serve::{self, Incoming};
use jwc::workspace::Workspace;
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
    let built = jwc::model::build(&ws);
    let ddl = jwc::ddl::render(&ws, &jwc::ddl::emit(&built.model), false);
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

/// The db layer must not turn a wrong projection into an empty result.
///
/// Every statement this layer sends projects one text column — the query
/// compiler wraps in `json_agg(…)::text` / `row_to_json(…)::text`, and so
/// does `raw()`. Reading that column used to be
/// `try_get::<_, Option<String>>(0).unwrap_or(None)`, which meant a
/// projection that was *not* text reported **no rows**: 404 from
/// `Shape::First`, `[]` from `Shape::Rows`, both indistinguishable from an
/// empty table. A generator bug would have looked like missing data
/// everywhere it touched.
///
/// `SELECT 1` is the shortest statement with a non-text first column, so
/// it stands in for that bug without needing one.
#[tokio::test(flavor = "multi_thread")]
async fn a_non_text_first_column_is_a_fault_not_an_empty_result() {
    let Ok(url) = std::env::var("JWC_V1_DATABASE_URL") else {
        eprintln!(
            "SKIPPED a_non_text_first_column_is_a_fault_not_an_empty_result — set \
             JWC_V1_DATABASE_URL. A SKIPPED line is not a pass."
        );
        return;
    };
    jwc::engine::init_engine(&url).expect("engine");

    for shape in [jwc::sql::Shape::First, jwc::sql::Shape::Rows] {
        let r = jwc::db::run("SELECT 1", &[], shape).await;
        match r {
            Err(jwc::db::DbError::Other(e)) => {
                let text = format!("{e:#}");
                assert!(
                    text.contains("not text"),
                    "the fault does not say what went wrong: {text}"
                );
            }
            Ok(v) => panic!("{shape:?} reported {v:?} instead of failing"),
            Err(other) => panic!("{shape:?} gave the wrong error: {other:?}"),
        }
    }

    // And the shape it *is* built for still works, so the guard is not
    // simply rejecting everything.
    let r = jwc::db::run("SELECT 'x'::text", &[], jwc::sql::Shape::First)
        .await
        .expect("a text first column is what this layer sends");
    assert_eq!(r.as_deref(), Some("x"));
}

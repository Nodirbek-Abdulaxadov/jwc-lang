//! Regression tests for the native AOT `insert <var> into ...` code
//! generator.
//!
//! Production hit:
//!     thread 'tokio-rt-worker' panicked at src/main.rs:1381:29:
//!     execute failed: error serializing parameter 2
//!     SQL: INSERT INTO "api_call" ("path", "method", "status", ...) VALUES ($1, $2, $3, ...)
//!
//! Root cause: `helper_for_kind` boxed every JWC integer (including plain
//! `int` → Postgres `INT4`) as `i64`. tokio-postgres' `ToSql` impl for `i64`
//! only accepts `INT8`, so the prepared-statement bind raised
//! `WrongType { postgres: Int4, rust: "i64" }` — surfaced as "error
//! serializing parameter 2" because `ToSql(2)` is 0-indexed (param[2] = $3
//! = `status`). The error message had nothing to do with VARCHAR despite
//! position $2 being a varchar in both failing entities — that lined up
//! coincidentally with the third column being `int`.
//!
//! Fix: split `PgKind::Int` into `Smallint | Int | Bigint` so the codegen
//! picks `jwc_param_smallint` / `jwc_param_int` / `jwc_param_bigint` per
//! column, and emit the matching `i16` / `i32` / `i64` width.

use jwc::native_build::emit_rust_source;
use jwc::parser::{parse_program, validate_program};

fn emit(src: &str, app: &str) -> String {
    let program = parse_program(src).expect("parse");
    validate_program(&program).expect("validate");
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = emit_rust_source(&program, tmp.path(), app, false).expect("emit");
    std::fs::read_to_string(out).expect("read emitted")
}

/// Find the body of the `INSERT INTO "<table>" ...` block emitted into
/// `main()` and return it as a slice that includes the surrounding
/// `__params` literal and the `jwc_db_exec(...)` call.
fn insert_block_for<'a>(body: &'a str, table: &str) -> &'a str {
    let needle = format!("INSERT INTO \\\"{}\\\"", table);
    let pos = body
        .find(&needle)
        .unwrap_or_else(|| panic!("emitted source has no INSERT for `{}`:\n{body}", table));
    // The execute line is preceded by the `__params` vec literal; walk
    // backwards to the opening `let __params:` so the assertions can scan
    // the whole bind list.
    let start = body[..pos].rfind("let __params:").unwrap_or(pos);
    let after = body[pos..]
        .find(".await")
        .map(|n| pos + n)
        .unwrap_or(body.len());
    &body[start..after]
}

/// The original bug: `status int` was boxed as `i64`, blowing up at $3.
/// The fix means `int` MUST emit `jwc_param_int` and `bigint` MUST emit
/// `jwc_param_bigint`. We also pin the column-by-column helper choice so
/// a refactor that re-collapses the integer widths breaks this test loudly.
#[test]
fn insert_picks_width_specific_int_helper_per_column() {
    let body = emit(
        r#"
            dbcontext AppDb : Postgres;
            entity ApiCall of AppDb {
                id         bigint pk autoincrement;
                path       varchar(200);
                method     varchar(8);
                status     int;
                latency_ms int;
                ts         datetime;
            }
            function main() {
                setConnectionString();
                let row = new ApiCall();
                row.path = "/x";
                row.method = "GET";
                row.status = 200;
                row.latency_ms = 0;
                row.ts = now();
                insert row into AppDb.ApiCall;
            }
        "#,
        "insert_int_widths",
    );

    let block = insert_block_for(&body, "api_call");

    // path / method → varchar
    assert!(
        block.contains("jwc_param_str(jwc_get_field(__var, \"path\"))"),
        "expected `path` to use jwc_param_str:\n{block}"
    );
    assert!(
        block.contains("jwc_param_str(jwc_get_field(__var, \"method\"))"),
        "expected `method` to use jwc_param_str:\n{block}"
    );
    // status / latency_ms → int (Postgres INT4)
    assert!(
        block.contains("jwc_param_int(jwc_get_field(__var, \"status\"))"),
        "expected `status` to use jwc_param_int (was the broken column):\n{block}"
    );
    assert!(
        block.contains("jwc_param_int(jwc_get_field(__var, \"latency_ms\"))"),
        "expected `latency_ms` to use jwc_param_int:\n{block}"
    );
    // ts (datetime) → jwc_param_timestamp boxes a chrono::DateTime so
    // the bind matches Postgres `timestamptz` directly. Binding a String
    // tripped `WrongType { postgres: Timestamptz, rust: "String" }` —
    // even with a `$N::timestamptz` cast in the SQL.
    assert!(
        block.contains("jwc_param_timestamp(jwc_get_field(__var, \"ts\"))"),
        "expected `ts` to use jwc_param_timestamp:\n{block}"
    );

    // Regression guard: the broken codegen called `jwc_param_int` (which
    // boxed i64) for every integer, so a `jwc_param_bigint` call should
    // NOT show up for an `int` column.
    assert!(
        !block.contains("jwc_param_bigint(jwc_get_field(__var, \"status\"))"),
        "regression: `status` was boxed as bigint, the original bug shape:\n{block}"
    );

    // SQL is well-formed: distinct parameter slots, not all-$1.
    assert!(
        block.contains("VALUES ($1, $2, $3, $4, $5)"),
        "emitted SQL doesn't use distinct positional params:\n{block}"
    );
}

/// Independently verify `bigint` columns still box as i64 — otherwise the
/// fix would have only swung the bug the other way (INT8 column receiving
/// i32). The test exercises an explicit `bigint` column with a user-set
/// value so it actually appears in the INSERT param list.
#[test]
fn insert_uses_bigint_helper_for_bigint_column() {
    let body = emit(
        r#"
            dbcontext AppDb : Postgres;
            entity Counter of AppDb {
                id    int pk autoincrement;
                total bigint;
            }
            function main() {
                setConnectionString();
                let row = new Counter();
                row.total = 1;
                insert row into AppDb.Counter;
            }
        "#,
        "insert_bigint",
    );

    let block = insert_block_for(&body, "counter");
    assert!(
        block.contains("jwc_param_bigint(jwc_get_field(__var, \"total\"))"),
        "expected `total bigint` to use jwc_param_bigint:\n{block}"
    );
}

/// Each parameter expression in the bind list must reference its own
/// column — the originally-feared bug shape was "$1, $1, $1, ...".
/// Documenting the invariant here so a future refactor of `emit_db_insert`
/// can't reintroduce it.
#[test]
fn insert_emits_one_distinct_get_field_per_column() {
    let body = emit(
        r#"
            dbcontext AppDb : Postgres;
            entity Link of AppDb {
                code       varchar(8) pk;
                url        varchar(2048);
                hits       int;
                created_at datetime;
            }
            function main() {
                setConnectionString();
                let row = new Link();
                row.code = "abc";
                row.url = "https://example.com";
                row.hits = 0;
                row.created_at = now();
                insert row into AppDb.Link;
            }
        "#,
        "insert_link",
    );

    let block = insert_block_for(&body, "link");
    for col in ["code", "url", "hits", "created_at"] {
        let needle = format!("jwc_get_field(__var, \"{col}\")");
        let n = block.matches(&needle).count();
        assert_eq!(
            n, 1,
            "column `{col}` should bind exactly once in the INSERT \
             param list; saw {n}:\n{block}"
        );
    }
}

//! Differential tests for the query layer, against a real Postgres.
//!
//! Every other DB test in this repo asserts on the SQL *string* JWC builds.
//! That is not enough, and this suite exists because of what it missed:
//!
//! ```text
//! where Sale.amount > 2 and Sale.amount < 9
//!   ->  SELECT * FROM "sale" WHERE "amount" > $1
//! ```
//!
//! The second predicate had silently vanished. As a string that SQL looks
//! perfectly well-formed — nothing about it is obviously wrong until it runs
//! and returns rows it should have excluded. Only comparing *results* catches
//! that class of bug, and it is the worst class a query layer has: no error,
//! no log line, just wrong data.
//!
//! ## How it works
//!
//! Each case is a JWC query and a hand-written SQL statement that should mean
//! the same thing. Both run against the same seeded tables and their outputs
//! are compared. The hand-written SQL is the oracle: it is what the author
//! *meant*, so a mismatch is JWC not honouring the query as written.
//!
//! Cases carry `ordered: false` unless the query pins row order, in which case
//! the comparison respects it — otherwise the suite would be asserting an
//! ordering neither side promises.
//!
//! ## Running it
//!
//! Needs a Postgres. It takes one from, in order:
//!
//! 1. `JWC_TEST_DATABASE_URL`, then `DATABASE_URL`
//! 2. a `testcontainers` container, when a Docker daemon is reachable
//!
//! and skips with a notice when neither is available, so `cargo test` on a
//! machine with no database still passes. CI supplies (1) through a service
//! container — see `.github/workflows/ci.yml`.
//!
//! **A skip is not a pass.** If you changed anything under `runner::sql`,
//! `native_build`'s SQL emitters, or the `where` / `having` grammar, run this
//! against a real database before believing the change works.

use std::collections::HashMap;

use jwc::engine;
use jwc::parser::{parse_program, validate_program};
use jwc::runner;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Entities mirror `SCHEMA_SQL` exactly. `RegionInfo` exists so joins have a
/// second table; `to_snake_case` maps it to `region_info`.
const PROGRAM_HEADER: &str = r#"
dbcontext AppDb : Postgres;

entity Sale of AppDb {
    id int pk;
    region varchar(32);
    product varchar(32);
    amount int;
    discount int nullable;
    note varchar(64) nullable;
}

entity RegionInfo of AppDb {
    id int pk;
    name varchar(32);
    manager varchar(32);
}
"#;

const SCHEMA_SQL: &str = r#"
DROP TABLE IF EXISTS "sale";
DROP TABLE IF EXISTS "region_info";

CREATE TABLE "region_info" (
    id integer PRIMARY KEY,
    name varchar(32) NOT NULL,
    manager varchar(32) NOT NULL
);

CREATE TABLE "sale" (
    id integer PRIMARY KEY,
    region varchar(32) NOT NULL,
    product varchar(32) NOT NULL,
    amount integer NOT NULL,
    discount integer,
    note varchar(64)
);
"#;

/// Chosen so the assertions can actually fail:
///
/// - `amount` spans a range where `> 2 and < 9` keeps a strict subset, so a
///   dropped conjunct changes the row count.
/// - `region` repeats with different group sizes, and one group has exactly
///   the count a `having` boundary tests.
/// - `discount` and `note` carry NULLs, which behave differently from 0 / "".
/// - `note` mixes case so `like` and `ilike` disagree.
/// - two rows share an `amount` so ordering by it alone is ambiguous — every
///   ordered case therefore orders by something unique.
const SEED_SQL: &str = r#"
INSERT INTO "region_info" (id, name, manager) VALUES
    (1, 'north', 'ada'),
    (2, 'south', 'grace'),
    (3, 'east',  'alan');

INSERT INTO "sale" (id, region, product, amount, discount, note) VALUES
    (1, 'north', 'widget', 1,  NULL, 'First sale'),
    (2, 'north', 'widget', 5,  10,   'second sale'),
    (3, 'north', 'gadget', 8,  0,    NULL),
    (4, 'north', 'gadget', 12, 5,    'Bulk Order'),
    (5, 'south', 'widget', 3,  NULL, 'small'),
    (6, 'south', 'gizmo',  9,  20,   'SOUTH special'),
    (7, 'south', 'gizmo',  5,  NULL, NULL),
    (8, 'east',  'widget', 20, 15,   'east big'),
    (9, 'east',  'gadget', 2,  NULL, 'tiny');
"#;

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

struct Case {
    name: &'static str,
    /// Route body. Must end in `return json(<expr>);`.
    body: &'static str,
    /// SQL whose result must match. For row sets, produce the same JSON array
    /// shape JWC does; for scalars, a single value.
    oracle: String,
    /// True when the JWC query pins row order and the comparison must too.
    ordered: bool,
}

const ROWS: &str = "SELECT COALESCE(json_agg(row_to_json(r)), '[]')::text FROM";

fn cases() -> Vec<Case> {
    vec![
        // -- where: the shapes that hid the vanishing-conjunct bug -----------
        Case {
            name: "where two literal conjuncts",
            body: r#"return json(select Sale from AppDb.Sale where Sale.amount > 2 and Sale.amount < 9);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" > 2 AND "amount" < 9) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where three literal conjuncts",
            body: r#"return json(select Sale from AppDb.Sale where Sale.amount > 2 and Sale.amount < 12 and Sale.id != 6);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" > 2 AND "amount" < 12 AND "id" <> 6) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where literal disjuncts",
            body: r#"return json(select Sale from AppDb.Sale where Sale.amount == 1 or Sale.amount == 20);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" = 1 OR "amount" = 20) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where and binds tighter than or",
            body: r#"return json(select Sale from AppDb.Sale where Sale.region == "north" and Sale.amount > 4 or Sale.id == 9);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE ("region" = 'north' AND "amount" > 4) OR "id" = 9) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where parenthesised group",
            body: r#"return json(select Sale from AppDb.Sale where Sale.region == "north" and (Sale.amount < 2 or Sale.amount > 10));"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "region" = 'north' AND ("amount" < 2 OR "amount" > 10)) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where bound param and literal mixed",
            body: r#"
                let floor = 4;
                return json(select Sale from AppDb.Sale where Sale.amount > @floor and Sale.amount < 12);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" > 4 AND "amount" < 12) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where arithmetic on the right",
            body: r#"return json(select Sale from AppDb.Sale where Sale.amount > 2 + 3);"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" > 5) r"#),
            ordered: false,
        },
        // -- where: operators ------------------------------------------------
        Case {
            name: "where all comparison operators",
            body: r#"return json(select Sale from AppDb.Sale where Sale.amount >= 5 and Sale.amount <= 12 and Sale.id != 4);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" >= 5 AND "amount" <= 12 AND "id" <> 4) r"#
            ),
            ordered: false,
        },
        Case {
            name: "where like is case sensitive",
            body: r#"return json(select Sale from AppDb.Sale where Sale.note like "%sale%");"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "note" LIKE '%sale%') r"#),
            ordered: false,
        },
        Case {
            name: "where ilike is not",
            body: r#"return json(select Sale from AppDb.Sale where Sale.note ilike "%sale%");"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "note" ILIKE '%sale%') r"#),
            ordered: false,
        },
        Case {
            name: "where in literal list",
            body: r#"return json(select Sale from AppDb.Sale where Sale.id in (1, 4, 8));"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "id" IN (1, 4, 8)) r"#),
            ordered: false,
        },
        Case {
            name: "where in dynamic array",
            body: r#"
                let ids = [2, 5, 9];
                return json(select Sale from AppDb.Sale where Sale.id in (@ids));"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "id" = ANY(ARRAY[2,5,9])) r"#),
            ordered: false,
        },
        Case {
            name: "where between",
            body: r#"return json(select Sale from AppDb.Sale where Sale.amount between 3 and 9);"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" BETWEEN 3 AND 9) r"#),
            ordered: false,
        },
        Case {
            name: "where is null",
            body: r#"return json(select Sale from AppDb.Sale where Sale.discount is null);"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "discount" IS NULL) r"#),
            ordered: false,
        },
        Case {
            name: "where is not null combined with a literal conjunct",
            body: r#"return json(select Sale from AppDb.Sale where Sale.discount is not null and Sale.amount > 5);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "discount" IS NOT NULL AND "amount" > 5) r"#
            ),
            ordered: false,
        },
        Case {
            name: "optional predicate keeps the term when bound",
            body: r#"
                let want = "north";
                return json(select Sale from AppDb.Sale where Sale.region ==? @want and Sale.amount > 2);"#,
            oracle: format!(
                r#"{ROWS} (SELECT * FROM "sale" WHERE "region" = 'north' AND "amount" > 2) r"#
            ),
            ordered: false,
        },
        Case {
            name: "optional predicate drops the term when empty",
            body: r#"
                let want = "";
                return json(select Sale from AppDb.Sale where Sale.region ==? @want and Sale.amount > 2);"#,
            oracle: format!(r#"{ROWS} (SELECT * FROM "sale" WHERE "amount" > 2) r"#),
            ordered: false,
        },
        // -- shaping ---------------------------------------------------------
        Case {
            name: "projection",
            body: r#"return json(select Sale { id, region, amount } from AppDb.Sale where Sale.amount > 8);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "id", "region", "amount" FROM "sale" WHERE "amount" > 8) r"#
            ),
            ordered: false,
        },
        Case {
            name: "distinct over a projection",
            body: r#"return json(select distinct Sale { region } from AppDb.Sale);"#,
            oracle: format!(r#"{ROWS} (SELECT DISTINCT "region" FROM "sale") r"#),
            ordered: false,
        },
        Case {
            name: "distinct over two columns",
            body: r#"return json(select distinct Sale { region, product } from AppDb.Sale);"#,
            oracle: format!(r#"{ROWS} (SELECT DISTINCT "region", "product" FROM "sale") r"#),
            ordered: false,
        },
        Case {
            name: "orderby asc",
            body: r#"return json(select Sale { id, amount } from AppDb.Sale orderby id asc);"#,
            oracle: r#"SELECT COALESCE(json_agg(row_to_json(r) ORDER BY r.id ASC), '[]')::text
                       FROM (SELECT "id", "amount" FROM "sale") r"#
                .into(),
            ordered: true,
        },
        Case {
            name: "orderby desc with limit",
            body: r#"return json(select Sale { id, amount } from AppDb.Sale orderby amount desc limit 3);"#,
            oracle: r#"SELECT COALESCE(json_agg(row_to_json(r) ORDER BY r.amount DESC), '[]')::text
                       FROM (SELECT "id", "amount" FROM "sale" ORDER BY "amount" DESC LIMIT 3) r"#
                .into(),
            ordered: true,
        },
        Case {
            name: "limit and offset",
            body: r#"return json(select Sale { id } from AppDb.Sale orderby id asc limit 3 offset 4);"#,
            oracle: r#"SELECT COALESCE(json_agg(row_to_json(r) ORDER BY r.id ASC), '[]')::text
                       FROM (SELECT "id" FROM "sale" ORDER BY "id" ASC LIMIT 3 OFFSET 4) r"#
                .into(),
            ordered: true,
        },
        Case {
            name: "first returns one row",
            body: r#"return json(select Sale from AppDb.Sale where Sale.id == 4 first);"#,
            oracle: r#"SELECT row_to_json(r)::text FROM (SELECT * FROM "sale" WHERE "id" = 4) r"#
                .into(),
            ordered: false,
        },
        // -- aggregates ------------------------------------------------------
        Case {
            name: "scalar count",
            body: r#"return json(select count(*) from AppDb.Sale where Sale.amount > 4);"#,
            oracle: r#"SELECT count(*)::text FROM "sale" WHERE "amount" > 4"#.into(),
            ordered: false,
        },
        Case {
            name: "scalar sum",
            body: r#"return json(select sum(Sale.amount) from AppDb.Sale where Sale.region == "north");"#,
            oracle: r#"SELECT sum("amount")::text FROM "sale" WHERE "region" = 'north'"#.into(),
            ordered: false,
        },
        Case {
            name: "scalar min and max boundaries",
            body: r#"return json(select max(Sale.amount) from AppDb.Sale);"#,
            oracle: r#"SELECT max("amount")::text FROM "sale""#.into(),
            ordered: false,
        },
        Case {
            name: "scalar aggregate ignores nulls",
            body: r#"return json(select count(*) from AppDb.Sale where Sale.discount is null);"#,
            oracle: r#"SELECT count(*)::text FROM "sale" WHERE "discount" IS NULL"#.into(),
            ordered: false,
        },
        // -- group by / having ------------------------------------------------
        Case {
            name: "group by with aliased count",
            body: r#"return json(select Sale { region, total: count(*) } from AppDb.Sale group by region);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", count(*) AS "total" FROM "sale" GROUP BY "region") r"#
            ),
            ordered: false,
        },
        Case {
            name: "group by with sum and a where",
            body: r#"return json(select Sale { region, taken: sum(amount) } from AppDb.Sale where Sale.amount > 2 group by region);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", sum("amount") AS "taken" FROM "sale" WHERE "amount" > 2 GROUP BY "region") r"#
            ),
            ordered: false,
        },
        Case {
            name: "having on an aggregate",
            body: r#"return json(select Sale { region, total: count(*) } from AppDb.Sale group by region having count(*) > 2);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", count(*) AS "total" FROM "sale" GROUP BY "region" HAVING count(*) > 2) r"#
            ),
            ordered: false,
        },
        Case {
            name: "having on a projection alias",
            body: r#"return json(select Sale { region, total: count(*) } from AppDb.Sale group by region having total > 2);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", count(*) AS "total" FROM "sale" GROUP BY "region" HAVING count(*) > 2) r"#
            ),
            ordered: false,
        },
        Case {
            name: "having with two aggregate terms",
            body: r#"return json(select Sale { region, total: count(*), taken: sum(amount) } from AppDb.Sale group by region having count(*) > 1 and sum(Sale.amount) >= 26);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", count(*) AS "total", sum("amount") AS "taken"
                           FROM "sale" GROUP BY "region"
                           HAVING count(*) > 1 AND sum("amount") >= 26) r"#
            ),
            ordered: false,
        },
        Case {
            name: "having on a group key",
            body: r#"return json(select Sale { region, total: count(*) } from AppDb.Sale group by region having Sale.region != "east");"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", count(*) AS "total" FROM "sale" GROUP BY "region" HAVING "region" <> 'east') r"#
            ),
            ordered: false,
        },
        Case {
            name: "group by two columns",
            body: r#"return json(select Sale { region, product, total: count(*) } from AppDb.Sale group by region, product);"#,
            oracle: format!(
                r#"{ROWS} (SELECT "region", "product", count(*) AS "total" FROM "sale" GROUP BY "region", "product") r"#
            ),
            ordered: false,
        },
        // -- join --------------------------------------------------------------
        Case {
            name: "join surfacing a column from the joined entity",
            body: r#"return json(select Sale { id, amount, boss: RegionInfo.manager } from AppDb.Sale join RegionInfo on RegionInfo.name == Sale.region where Sale.amount > 8);"#,
            oracle: format!(
                r#"{ROWS} (SELECT s."id", s."amount", ri."manager" AS "boss"
                           FROM "sale" s JOIN "region_info" ri ON ri."name" = s."region"
                           WHERE s."amount" > 8) r"#
            ),
            ordered: false,
        },
        Case {
            name: "grouped aggregation across a join",
            body: r#"return json(select Sale { region, boss: RegionInfo.manager, total: count(*) } from AppDb.Sale join RegionInfo on RegionInfo.name == Sale.region group by Sale.region, RegionInfo.manager);"#,
            oracle: format!(
                r#"{ROWS} (SELECT s."region", ri."manager" AS "boss", count(*) AS "total"
                           FROM "sale" s JOIN "region_info" ri ON ri."name" = s."region"
                           GROUP BY s."region", ri."manager") r"#
            ),
            ordered: false,
        },
        Case {
            name: "join with two literal conjuncts in where",
            body: r#"return json(select Sale { id, boss: RegionInfo.manager } from AppDb.Sale join RegionInfo on RegionInfo.name == Sale.region where Sale.amount > 2 and Sale.amount < 12);"#,
            oracle: format!(
                r#"{ROWS} (SELECT s."id", ri."manager" AS "boss"
                           FROM "sale" s JOIN "region_info" ri ON ri."name" = s."region"
                           WHERE s."amount" > 2 AND s."amount" < 12) r"#
            ),
            ordered: false,
        },
    ]
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Postgres URL from the environment, or a testcontainer, or `None`.
///
/// The env var comes first so CI and local runs can point at a database that
/// is already up; testcontainers is the fallback for a developer with Docker
/// and nothing else.
fn database_url() -> Option<String> {
    for key in ["JWC_TEST_DATABASE_URL", "DATABASE_URL"] {
        if let Ok(url) = std::env::var(key) {
            if !url.trim().is_empty() {
                return Some(url);
            }
        }
    }
    container_url().map(|s| s.to_string())
}

fn container_url() -> Option<&'static str> {
    use std::sync::OnceLock;
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::testcontainers::runners::SyncRunner;
    use testcontainers_modules::testcontainers::Container;

    struct Shared {
        _c: Container<Postgres>,
        url: String,
    }
    static SHARED: OnceLock<Option<Shared>> = OnceLock::new();

    SHARED
        .get_or_init(|| {
            let c = Postgres::default()
                .with_user("postgres")
                .with_password("postgres")
                .with_db_name("postgres")
                .start()
                .ok()?;
            let port = c.get_host_port_ipv4(5432).ok()?;
            let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
            Some(Shared { _c: c, url })
        })
        .as_ref()
        .map(|s| s.url.as_str())
}

/// Seed once per process. `engine::ENGINE` is a process-wide `OnceLock`, and
/// every case here only reads, so there is nothing to isolate between them.
async fn ensure_seeded() -> Option<()> {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0); // 0 = untried, 1 = ready, 2 = unavailable

    match STATE.load(Ordering::SeqCst) {
        1 => return Some(()),
        2 => return None,
        _ => {}
    }

    let Some(url) = database_url() else {
        STATE.store(2, Ordering::SeqCst);
        return None;
    };
    // `init_engine` erroring means a sibling test already initialised the
    // process-wide pool, which is fine — it points at the same URL.
    let _ = engine::init_engine(&url);

    // Schema + seed are multi-statement, so they go through a direct client
    // rather than the pool's single-statement `exec`.
    let Ok(client) = engine::connect_for_migrations(&url).await else {
        STATE.store(2, Ordering::SeqCst);
        return None;
    };
    for stmt in [SCHEMA_SQL, SEED_SQL] {
        if let Err(e) = client.batch_execute(stmt).await {
            eprintln!("[query_differential] seed failed: {e}");
            STATE.store(2, Ordering::SeqCst);
            return None;
        }
    }
    STATE.store(1, Ordering::SeqCst);
    Some(())
}

fn skip_notice() {
    eprintln!(
        "[query_differential] SKIPPED: no Postgres. Set JWC_TEST_DATABASE_URL \
         (or DATABASE_URL), or make a Docker daemon reachable. A skip is not a pass — \
         run this before trusting a change to the query layer."
    );
}

/// Parse when possible and re-serialise canonically, so `{"a":1,"b":2}` and
/// `{"b":2,"a":1}` compare equal and unordered row sets do too.
fn normalize(raw: &str, ordered: bool) -> String {
    let trimmed = raw.trim();
    let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
        return trimmed.to_string();
    };
    fn canon(v: &Value, ordered: bool) -> String {
        match v {
            Value::Array(items) => {
                let mut parts: Vec<String> = items.iter().map(|i| canon(i, ordered)).collect();
                if !ordered {
                    parts.sort();
                }
                format!("[{}]", parts.join(","))
            }
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let body: Vec<String> = keys
                    .iter()
                    .map(|k| format!("{:?}:{}", k, canon(&map[k.as_str()], ordered)))
                    .collect();
                format!("{{{}}}", body.join(","))
            }
            // Postgres hands back numerics as strings in some paths and as
            // numbers in others; compare on the textual value so `5` and "5"
            // don't read as a difference in meaning.
            Value::Number(n) => n.to_string(),
            Value::String(s) => match s.parse::<f64>() {
                Ok(f) if s.trim() == f.to_string() => f.to_string(),
                _ => format!("{:?}", s),
            },
            other => other.to_string(),
        }
    }
    canon(&v, ordered)
}

/// Run one case's JWC query through the real runner and its oracle through
/// the engine, and compare.
async fn check(case: &Case) -> Result<(), String> {
    let src = format!(
        "{PROGRAM_HEADER}\nroute GET \"/q\" {{\n{}\n}}\n",
        case.body.trim()
    );
    let program = parse_program(&src).map_err(|e| format!("parse: {e:#}"))?;
    validate_program(&program).map_err(|e| format!("validate: {e:#}"))?;

    let (status, body) =
        runner::run_request_with_headers(&program, "GET", "/q", None, HashMap::new())
            .await
            .map(|(s, b, _, _)| (s, b))
            .map_err(|e| format!("run: {e:#}"))?;
    if status != 200 {
        return Err(format!("route returned {status}: {body}"));
    }

    let expected = engine::query_text(&case.oracle, &[])
        .await
        .map_err(|e| format!("oracle SQL failed: {e:#}"))?;

    let got_n = normalize(&body, case.ordered);
    let want_n = normalize(&expected, case.ordered);
    if got_n != want_n {
        return Err(format!(
            "results differ\n  jwc    : {got_n}\n  oracle : {want_n}\n  query  : {}\n  oracle sql: {}",
            case.body.trim(),
            case.oracle.trim()
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn jwc_queries_agree_with_hand_written_sql() {
    if ensure_seeded().await.is_none() {
        skip_notice();
        return;
    }

    let all = cases();
    let mut failures: Vec<String> = Vec::new();
    for case in &all {
        match check(case).await {
            Ok(()) => {}
            Err(why) => failures.push(format!("--- {} ---\n{why}", case.name)),
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} query cases disagree with hand-written SQL:\n\n{}",
        failures.len(),
        all.len(),
        failures.join("\n\n")
    );
    eprintln!("[query_differential] {} cases verified", all.len());
}

/// The suite is only worth anything if a wrong result actually fails it.
/// This runs a query against an oracle that means something else and asserts
/// the comparison rejects it — otherwise a normalisation bug could quietly
/// make every case pass.
#[tokio::test(flavor = "multi_thread")]
async fn the_comparison_rejects_a_wrong_result() {
    if ensure_seeded().await.is_none() {
        skip_notice();
        return;
    }

    let deliberately_wrong = Case {
        name: "control",
        body: r#"return json(select Sale from AppDb.Sale where Sale.amount > 2 and Sale.amount < 9);"#,
        // Missing the second conjunct — exactly the bug this suite exists for.
        oracle: r#"SELECT COALESCE(json_agg(row_to_json(r)), '[]')::text
                   FROM (SELECT * FROM "sale" WHERE "amount" > 2) r"#
            .into(),
        ordered: false,
    };

    let outcome = check(&deliberately_wrong).await;
    assert!(
        outcome.is_err(),
        "the differential check passed a query whose oracle drops a predicate — \
         it cannot detect the bug it was written for"
    );
}

//! Integration tests for the JWC DB stack against a real Postgres instance.
//!
//! Each test boots (or reuses) a Postgres container via `testcontainers` and
//! exercises `jwc::engine` / `jwc::migrate` / `jwc::runner` directly. Tests
//! gracefully skip with `eprintln!` when Docker is unavailable, so the suite
//! is safe to run on hosts without a daemon.
//!
//! ## Why a shared container + global mutex
//!
//! `jwc::engine::ENGINE` is a `OnceLock<JwcEngine>` — once initialised against
//! a URL, every subsequent call reuses that pool. We therefore boot one
//! container, set `DATABASE_URL` to point at it, and serialise tests with a
//! `Mutex<()>` so each one gets a fresh `public` schema. This mirrors how the
//! production runtime sees the world, while keeping isolation per test.
//!
//! ## Async note
//!
//! Phase 9 moved `engine` / `runner` / `migrate` to async APIs backed by
//! `deadpool-postgres` + `tokio-postgres`. Tests therefore run on a
//! `#[tokio::test(flavor = "multi_thread")]` runtime so the synchronous
//! `testcontainers` boot path can coexist with the async DB calls.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::bail;
use jwc::engine;
use jwc::migrate;
use jwc::parser::{parse_program, validate_program};
use jwc::runner;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::Container;

static SHARED: OnceLock<Option<SharedContainer>> = OnceLock::new();
static TEST_LOCK: Mutex<()> = Mutex::new(());

struct SharedContainer {
    /// Held to keep the Postgres container alive for the duration of the
    /// test process. Dropped at exit when the OnceLock is torn down.
    _container: Container<Postgres>,
    url: String,
}

/// Try to boot a shared Postgres container. Returns `None` when Docker is
/// unreachable from the test process (common on Windows when Docker Desktop's
/// named pipe permissions block native binaries) so individual tests can
/// skip rather than hard-fail.
fn shared_postgres_url() -> Option<&'static str> {
    SHARED
        .get_or_init(|| {
            let container = Postgres::default()
                .with_user("postgres")
                .with_password("postgres")
                .with_db_name("postgres")
                .start()
                .ok()?;
            let port = container.get_host_port_ipv4(5432).ok()?;
            let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
            // The engine pool reads DATABASE_URL once on init, so wire it now
            // for the migration helpers and any code path that defaults from env.
            std::env::set_var("DATABASE_URL", &url);
            Some(SharedContainer {
                _container: container,
                url,
            })
        })
        .as_ref()
        .map(|c| c.url.as_str())
}

/// Acquire the global test lock and reset the `public` schema. Returns `None`
/// when Docker is unavailable — callers then early-return as a graceful skip.
async fn fresh_schema() -> Option<(std::sync::MutexGuard<'static, ()>, &'static str)> {
    let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let url = shared_postgres_url()?;
    let client = engine::connect_for_migrations(url).await.ok()?;
    client
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .await
        .ok()?;
    Some((guard, url))
}

fn skip_notice(test_name: &str) {
    eprintln!(
        "[integration_db::{}] skipping: docker not reachable from this test process",
        test_name
    );
}

fn write_project(root: &std::path::Path, main_jwc: &str, manifest_name: &str) {
    std::fs::create_dir_all(root).unwrap();
    let manifest = format!(
        "{{\n    \"name\": \"{}\",\n    \"version\": \"1.0.0\",\n    \"dependencies\": []\n}}\n",
        manifest_name
    );
    std::fs::write(
        root.join(format!("{}.jwcproj", manifest_name)),
        manifest,
    )
    .unwrap();
    std::fs::write(root.join("main.jwc"), main_jwc).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_init_and_basic_query() {
    let Some((_g, url)) = fresh_schema().await else {
        skip_notice("engine_init_and_basic_query");
        return;
    };
    engine::init_engine(url).expect("init engine");
    let result = engine::query_text("SELECT 'hello'::text", &[])
        .await
        .expect("query");
    assert_eq!(result, "hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn transaction_commit_persists() {
    let Some((_g, url)) = fresh_schema().await else {
        skip_notice("transaction_commit_persists");
        return;
    };
    engine::init_engine(url).expect("init engine");

    engine::exec("CREATE TABLE tx_commit (id integer PRIMARY KEY);", &[])
        .await
        .expect("create table");

    // `with_tx` commits on Ok and rolls back on Err. A successful body
    // therefore exercises the commit path — equivalent to the old
    // `begin_tx()` + `tx.commit()` flow.
    engine::with_tx(|| async {
        engine::exec("INSERT INTO tx_commit VALUES (1);", &[]).await?;
        Ok(())
    })
    .await
    .expect("with_tx commit");

    let count = engine::query_text("SELECT COUNT(*)::text FROM tx_commit;", &[])
        .await
        .unwrap();
    assert_eq!(count, "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn transaction_rollback_on_drop_without_commit() {
    let Some((_g, url)) = fresh_schema().await else {
        skip_notice("transaction_rollback_on_drop_without_commit");
        return;
    };
    engine::init_engine(url).expect("init engine");

    engine::exec(
        "CREATE TABLE tx_rollback (id integer PRIMARY KEY);",
        &[],
    )
    .await
    .expect("create table");

    // The old `begin_tx()` returned a `TxGuard` whose `Drop` issued `ROLLBACK`
    // when not committed. The new async API expresses the same contract via
    // `with_tx`: any `Err` returned from the body triggers a rollback. We
    // force that branch with a sentinel error and assert the row was not
    // persisted.
    let res: anyhow::Result<()> = engine::with_tx(|| async {
        engine::exec("INSERT INTO tx_rollback VALUES (1);", &[]).await?;
        bail!("force rollback for test");
    })
    .await;
    assert!(res.is_err(), "with_tx body should have returned Err");

    let count = engine::query_text("SELECT COUNT(*)::text FROM tx_rollback;", &[])
        .await
        .unwrap();
    assert_eq!(
        count, "0",
        "row leaked despite Err-returning with_tx body — rollback didn't fire"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_up_then_down_roundtrip() {
    let Some((_g, url)) = fresh_schema().await else {
        skip_notice("migrate_up_then_down_roundtrip");
        return;
    };
    engine::init_engine(url).expect("init engine");

    let tmp = tempfile_dir("jwc_migrate_roundtrip");
    let project_root = tmp.join("proj");
    write_project(
        &project_root,
        r#"
            dbcontext AppDb : Postgres;
            entity Note of AppDb {
                id integer pk;
                title varchar(120);
            }
            function main() { print("ok"); }
        "#,
        "proj",
    );

    let migration = migrate::create_migration(&project_root, "init")
        .expect("create migration");
    assert!(migration.up_path.exists());

    let first = migrate::apply_pending_migrations(&project_root, Some(url.to_string()))
        .await
        .expect("apply first time");
    assert_eq!(first.applied, 1);
    assert_eq!(first.skipped, 0);

    let second = migrate::apply_pending_migrations(&project_root, Some(url.to_string()))
        .await
        .expect("apply second time");
    assert_eq!(
        second.applied, 0,
        "second up should be a no-op"
    );
    assert_eq!(second.skipped, 1);

    // The table must exist after `up`.
    let exists = engine::query_text("SELECT to_regclass('public.note')::text", &[])
        .await
        .unwrap();
    assert!(
        exists != "null" && !exists.is_empty(),
        "expected 'note' table after migrate up, got '{exists}'"
    );

    // Hand-write a usable down so the rollback step has something to run.
    let down_path = migration
        .up_path
        .to_string_lossy()
        .replace(".up.sql", ".down.sql");
    std::fs::write(&down_path, "DROP TABLE IF EXISTS note;\n").unwrap();

    let rolled = migrate::rollback_migrations(&project_root, Some(url.to_string()), 1)
        .await
        .expect("rollback");
    assert_eq!(rolled.rolled_back, 1);

    let after = engine::query_text("SELECT to_regclass('public.note')::text", &[])
        .await
        .unwrap();
    assert!(
        after.is_empty() || after == "null",
        "table 'note' still present after migrate down, got '{after}'"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn migration_advisory_lock_blocks_concurrent_runs() {
    let Some((_g, url)) = fresh_schema().await else {
        skip_notice("migration_advisory_lock_blocks_concurrent_runs");
        return;
    };
    engine::init_engine(url).expect("init engine");

    let tmp = tempfile_dir("jwc_migrate_lock");
    let project_root = tmp.join("proj");
    write_project(
        &project_root,
        r#"
            dbcontext AppDb : Postgres;
            entity Slow of AppDb {
                id integer pk;
            }
            function main() { print("ok"); }
        "#,
        "proj",
    );
    let _ = migrate::create_migration(&project_root, "slow").unwrap();

    // Hold the advisory lock manually from another connection, then try to
    // run the migration. The migrate helper must bail rather than block.
    let holder = engine::connect_for_migrations(url)
        .await
        .expect("holder client");
    holder
        .execute(
            "SELECT pg_advisory_lock($1);",
            &[&0x6a77632d6d6967_i64],
        )
        .await
        .expect("acquire lock");

    let err = migrate::apply_pending_migrations(&project_root, Some(url.to_string()))
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        err.contains("another migration run is in progress"),
        "expected advisory-lock error, got: {err}"
    );

    let _ = holder
        .execute("SELECT pg_advisory_unlock($1);", &[&0x6a77632d6d6967_i64])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn runner_run_request_full_crud() {
    let Some((_g, url)) = fresh_schema().await else {
        skip_notice("runner_run_request_full_crud");
        return;
    };
    engine::init_engine(url).expect("init engine");

    // Create the backing table directly — exercising the migration here would
    // duplicate `migrate_up_then_down_roundtrip` and slow the suite.
    engine::exec(
        r#"CREATE TABLE "user" (
            id integer PRIMARY KEY,
            name varchar(60) NOT NULL,
            email varchar(120) NOT NULL
        );"#,
        &[],
    )
    .await
    .unwrap();

    let src = r#"
        dbcontext AppDb : Postgres;

        entity User of AppDb {
            id int pk;
            name varchar(60);
            email varchar(120);
        }

        route POST "users" {
            let payload = body();
            let u = new User();
            u.id = payload.id;
            u.name = payload.name;
            u.email = payload.email;
            insert u into AppDb.User;
            return created(u);
        }

        route GET "users/{id}" {
            let id = path_param("id");
            let u = select User from AppDb.User where User.id == @id first;
            if (u == null) { return notFound(); }
            return json(u);
        }

        route DELETE "users/{id}" {
            let id = path_param("id");
            delete from AppDb.User where User.id == @id;
            return noContent();
        }
    "#;
    let program = parse_program(src).expect("parse");
    validate_program(&program).expect("validate");

    let (status_post, body_post) = runner::run_request(
        &program,
        "POST",
        "/users",
        Some(r#"{"id":1,"name":"Najim","email":"n@example.com"}"#.to_string()),
    )
    .await
    .expect("POST run");
    assert_eq!(status_post, 201);
    assert!(body_post.contains("\"name\":\"Najim\""));

    let (status_get, body_get) = runner::run_request(&program, "GET", "/users/1", None)
        .await
        .expect("GET run");
    assert_eq!(status_get, 200);
    assert!(body_get.contains("\"email\":\"n@example.com\""));

    let (status_del, _) = runner::run_request(&program, "DELETE", "/users/1", None)
        .await
        .expect("DELETE run");
    assert_eq!(status_del, 204);

    let (status_404, _) = runner::run_request(&program, "GET", "/users/1", None)
        .await
        .expect("GET after delete");
    assert_eq!(status_404, 404);
}

fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        Duration::from_millis(rand_suffix()).as_millis()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}

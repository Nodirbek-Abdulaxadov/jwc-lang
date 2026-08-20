//! The applier, against a real Postgres.
//!
//! Set `JWC_V1_DATABASE_URL` to a connection string for a database the
//! suite may **drop and recreate schemas in**. Without it every test here
//! prints SKIPPED and returns — and, as everywhere else in this repo,
//! **a SKIPPED line is not a pass**. Nothing about `up`, `down`, `status`
//! or `verify` is checkable without a database; a green run with no
//! variable set has checked nothing.

use jwc::{apply, migrate, model, snapshot, workspace::Workspace};
use std::path::{Path, PathBuf};
use tokio_postgres::Client;

fn url() -> Option<String> {
    std::env::var("JWC_V1_DATABASE_URL").ok()
}

macro_rules! db {
    ($name:literal) => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!(
                    "SKIPPED {} — set JWC_V1_DATABASE_URL. A SKIPPED line is not a pass.",
                    $name
                );
                return;
            }
        }
    };
}

async fn connect(url: &str) -> Client {
    jwc::engine::connect_for_migrations(url)
        .await
        .expect("connect")
}

/// A clean slate: every schema this suite creates, plus the bookkeeping
/// table, gone.
async fn reset(client: &Client) {
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS org CASCADE;
             DROP SCHEMA IF EXISTS billing CASCADE;
             DROP TABLE IF EXISTS public._jwc_migrations;",
        )
        .await
        .expect("reset");
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn model_of(text: &str, dir: &Path) -> model::SchemaModel {
    let p = dir.join("a.jwc");
    std::fs::write(&p, text).expect("write");
    let ws = Workspace::load(&p).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    model::build(&ws).model
}

/// Write a migration into `dir` the way `jwc migrate new` would.
fn write_migration(
    dir: &Path,
    prev: &snapshot::Snapshot,
    model: &model::SchemaModel,
    name: &str,
) -> snapshot::Snapshot {
    let ordinal = snapshot::next_ordinal(dir);
    let plan = migrate::plan(prev, model, ordinal, name);
    assert!(!plan.has_errors(), "{name}: the plan has errors");
    let mut last = prev.clone();
    for f in &plan.files {
        std::fs::write(dir.join(format!("{}.up.sql", f.stem)), &f.up).expect("write up");
        std::fs::write(dir.join(format!("{}.down.sql", f.stem)), &f.down).expect("write down");
        if let Some(s) = &f.snapshot {
            std::fs::write(dir.join(format!("{}.snapshot.json", f.stem)), s).expect("write snap");
            last = snapshot::Snapshot::from_json(s).expect("re-read");
        }
    }
    last
}

const V1: &str = r#"
namespace m;
database App : Postgres;
schema org of App;

enum Plan of App.org { free, pro }

--- Tenants.
table Orgs of App.org {
    id   bigint primary key identity;
    slug varchar(40) unique;
    plan Plan;
    name varchar(80)?;
    retired_at timestamptz?;

    unique (name) where retired_at == null : "faol nom bitta";
    index on (slug);
}

view OrgSummary of App.org {
    select O from App.org.Orgs as { id, slug };
}
"#;

const V2: &str = r#"
namespace m;
database App : Postgres;
schema org of App;

enum Plan of App.org { free, pro, enterprise }

--- Tenants, one per customer.
table Orgs of App.org {
    id     bigint primary key identity;
    slug   varchar(40) unique;
    plan   Plan;
    name   varchar(200)?;
    region varchar(20)?;
    retired_at timestamptz?;

    unique (name) where retired_at == null : "faol nom bitta";
    index on (slug);
}

view OrgSummary of App.org {
    select O from App.org.Orgs as { id, slug };
}
"#;

#[tokio::test]
async fn up_applies_everything_then_has_nothing_left_to_do() {
    let url = db!("up_applies_everything_then_has_nothing_left_to_do");
    let client = connect(&url).await;
    reset(&client).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");

    let src = tempfile::tempdir().expect("tempdir");
    let snap = write_migration(
        &dir,
        &snapshot::Snapshot::default(),
        &model_of(V1, src.path()),
        "initial",
    );
    write_migration(&dir, &snap, &model_of(V2, src.path()), "widen");

    let ran = apply::up(&client, &dir, None).await.expect("up");
    assert_eq!(
        ran,
        vec![
            "0001_initial".to_string(),
            "0002_widen_enum_values".to_string(),
            "0003_widen".to_string()
        ],
        "the enum file has to go first — a default in the ordinary file may \
         name the value it adds"
    );

    // Idempotent. Anything else and a redeploy re-runs the whole history.
    let again = apply::up(&client, &dir, None).await.expect("up twice");
    assert!(again.is_empty(), "{again:?}");

    let st = apply::status(&client, &dir).await.expect("status");
    assert_eq!(st.applied.len(), 3);
    assert!(st.pending.is_empty());
    assert!(st.drift.is_empty(), "{:?}", st.drift);

    // The database really is the shape the sources describe.
    let final_snap = snapshot::of(&model_of(V2, src.path()));
    let problems = apply::verify(&client, &final_snap).await.expect("verify");
    assert!(problems.is_empty(), "{problems:?}");
    let missing = apply::check_live_schema(&client, &final_snap)
        .await
        .expect("check");
    assert!(missing.is_empty(), "{missing:?}");
}

#[tokio::test]
async fn down_rolls_back_and_refuses_what_it_cannot_undo() {
    let url = db!("down_rolls_back_and_refuses_what_it_cannot_undo");
    let client = connect(&url).await;
    reset(&client).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = tempfile::tempdir().expect("tempdir");

    let snap = write_migration(
        &dir,
        &snapshot::Snapshot::default(),
        &model_of(V1, src.path()),
        "initial",
    );
    write_migration(&dir, &snap, &model_of(V2, src.path()), "widen");
    apply::up(&client, &dir, None).await.expect("up");

    // 0003_widen is reversible: it widens a column and adds a nullable one.
    let undone = apply::down(&client, &dir, 1).await.expect("down");
    assert_eq!(undone, vec!["0003_widen".to_string()]);
    let row = client
        .query_one(
            "SELECT character_maximum_length FROM information_schema.columns
              WHERE table_schema = 'org' AND table_name = 'orgs' AND column_name = 'name'",
            &[],
        )
        .await
        .expect("query");
    assert_eq!(row.get::<_, i32>(0), 80, "the down did not narrow it back");

    // 0002_widen_enum_values is not. Postgres cannot remove an enum value,
    // and the file says so rather than pretending.
    let err = apply::down(&client, &dir, 1)
        .await
        .expect_err("should refuse");
    let text = format!("{err}\n{}", err.chain().map(|c| c.to_string()).collect::<Vec<_>>().join("\n"));
    assert!(text.contains("cannot be rolled back"), "{text}");
    assert!(text.contains("enum value"), "{text}");
}

#[tokio::test]
async fn a_data_sidecar_runs_in_phase_three() {
    let url = db!("a_data_sidecar_runs_in_phase_three");
    let client = connect(&url).await;
    reset(&client).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = tempfile::tempdir().expect("tempdir");

    let snap = write_migration(
        &dir,
        &snapshot::Snapshot::default(),
        &model_of(V1, src.path()),
        "initial",
    );
    apply::up(&client, &dir, None).await.expect("up");
    client
        .batch_execute("INSERT INTO org.orgs (slug, plan) VALUES ('a', 'free')")
        .await
        .expect("seed");

    // Expand: add the column nullable, backfill it by hand, tighten later.
    // This is the shape migrations.md §7.2 exists for.
    write_migration(&dir, &snap, &model_of(V2, src.path()), "region");
    std::fs::write(
        dir.join("0003_region.data.sql"),
        "UPDATE org.orgs SET region = 'us' WHERE region IS NULL;\n",
    )
    .expect("sidecar");

    apply::up(&client, &dir, None).await.expect("up");
    let row = client
        .query_one("SELECT region FROM org.orgs WHERE slug = 'a'", &[])
        .await
        .expect("query");
    assert_eq!(
        row.get::<_, Option<String>>(0).as_deref(),
        Some("us"),
        "the sidecar did not run"
    );
}

#[tokio::test]
async fn an_edited_migration_is_drift_not_silence() {
    let url = db!("an_edited_migration_is_drift_not_silence");
    let client = connect(&url).await;
    reset(&client).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = tempfile::tempdir().expect("tempdir");

    write_migration(
        &dir,
        &snapshot::Snapshot::default(),
        &model_of(V1, src.path()),
        "initial",
    );
    apply::up(&client, &dir, None).await.expect("up");

    let p = dir.join("0001_initial.up.sql");
    let text = std::fs::read_to_string(&p).expect("read");
    std::fs::write(&p, format!("{text}\n-- someone edited this\n")).expect("write");

    let st = apply::status(&client, &dir).await.expect("status");
    assert_eq!(st.drift.len(), 1, "{:?}", st.drift);
    assert!(st.drift[0].contains("edited after it was applied"), "{:?}", st.drift);
}

#[tokio::test]
async fn verify_and_the_boot_check_name_what_is_missing() {
    let url = db!("verify_and_the_boot_check_name_what_is_missing");
    let client = connect(&url).await;
    reset(&client).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = tempfile::tempdir().expect("tempdir");

    write_migration(
        &dir,
        &snapshot::Snapshot::default(),
        &model_of(V1, src.path()),
        "initial",
    );
    apply::up(&client, &dir, None).await.expect("up");
    let snap = snapshot::of(&model_of(V1, src.path()));

    // A DBA drops an index by hand. The name is generated and therefore
    // predictable, which is the whole reason this is checkable (#28).
    client
        .batch_execute("DROP INDEX org.ix_orgs__slug")
        .await
        .expect("drop index");
    let problems = apply::verify(&client, &snap).await.expect("verify");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("ix_orgs__slug"), "{problems:?}");

    // A column the program reads is gone: #33 names it at boot rather than
    // wrapping PG's 42703 in a 500 at request time.
    client
        .batch_execute("DROP VIEW org.org_summary; ALTER TABLE org.orgs DROP COLUMN name;")
        .await
        .expect("drop column");
    let missing = apply::check_live_schema(&client, &snap).await.expect("check");
    assert!(
        missing.iter().any(|m| m.contains("org.orgs.name")),
        "{missing:?}"
    );
}

#[tokio::test]
async fn a_hand_edited_no_transaction_file_is_refused() {
    let url = db!("a_hand_edited_no_transaction_file_is_refused");
    let client = connect(&url).await;
    reset(&client).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");

    // E1101 — the generator never writes this, but the files are checked in
    // and editable, and this one has no transaction to roll a stray
    // statement back.
    std::fs::write(
        dir.join("0001_bad.up.sql"),
        format!(
            "{}\nCREATE SCHEMA IF NOT EXISTS org;\nDROP SCHEMA org;\n",
            migrate::NO_TRANSACTION
        ),
    )
    .expect("write");
    let err = apply::up(&client, &dir, None).await.expect_err("should refuse");
    let text = format!("{err}");
    assert!(text.contains("E1101"), "{text}");

    // And nothing ran: the schema the file would have created is absent.
    let row = client
        .query_one(
            "SELECT count(*) FROM information_schema.schemata WHERE schema_name = 'org'",
            &[],
        )
        .await
        .expect("query");
    assert_eq!(row.get::<_, i64>(0), 0, "a refused file still ran");
}

#[tokio::test]
async fn the_advisory_lock_is_held_while_a_migration_runs() {
    let url = db!("the_advisory_lock_is_held_while_a_migration_runs");
    let a = connect(&url).await;
    let b = connect(&url).await;
    apply::lock(&a).await.expect("lock");

    // A second session can see it — and, more to the point, cannot take it.
    let row = b
        .query_one(
            "SELECT pg_try_advisory_lock($1)",
            &[&apply::LOCK_KEY],
        )
        .await
        .expect("try lock");
    assert!(
        !row.get::<_, bool>(0),
        "two deploys could interleave half a migration each"
    );

    apply::unlock(&a).await.expect("unlock");
    let row = b
        .query_one("SELECT pg_try_advisory_lock($1)", &[&apply::LOCK_KEY])
        .await
        .expect("try lock");
    assert!(row.get::<_, bool>(0), "the lock was not released");
}

#[tokio::test]
async fn the_sample_migrates_from_nothing() {
    let url = db!("the_sample_migrates_from_nothing");
    let client = connect(&url).await;
    reset(&client).await;
    client
        .batch_execute("DROP SCHEMA IF EXISTS audit CASCADE; DROP SCHEMA IF EXISTS auth CASCADE;")
        .await
        .expect("reset");
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("migrations");
    std::fs::create_dir_all(&dir).expect("mkdir");

    // 13 tables, 5 views, every constraint class the language has.
    let ws = Workspace::load(repo_root().join("docs/spec/v1/sample")).expect("sample");
    let m = model::build(&ws).model;
    write_migration(&dir, &snapshot::Snapshot::default(), &m, "sample");
    apply::up(&client, &dir, None).await.expect("up");

    let snap = snapshot::of(&m);
    let problems = apply::verify(&client, &snap).await.expect("verify");
    assert!(problems.is_empty(), "{problems:?}");
}

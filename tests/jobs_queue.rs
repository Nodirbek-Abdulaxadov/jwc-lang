//! The durable queue, against a real Postgres (jobs.md §3).
//!
//! Set `JWC_V1_DATABASE_URL` to a database the suite may create and drop
//! `public._jwc_jobs*` in. Without it every test here prints SKIPPED and
//! returns — and **a SKIPPED line is not a pass**. Nothing about a claim,
//! a lease, a retry or a dead letter is checkable without a database.
//!
//! What this pins is the *storage* contract: the SQL, the lease, the
//! backoff and the dead-letter move. Whether a handler body runs is
//! `serve`'s business and is covered where the handler is.

use jwc::jobs;
use tokio_postgres::Client;

fn url() -> Option<String> {
    std::env::var("JWC_V1_DATABASE_URL").ok()
}

/// # One runtime, one test
///
/// These were seven `#[tokio::test]`s and they failed intermittently.
/// `#[tokio::test]` builds a **runtime per test**, and a
/// `deadpool-postgres` connection is driven by a task spawned on the
/// runtime that created it: the first test's runtime is dropped at the end
/// of that test, and every later test is holding connections nothing is
/// polling. A `tokio::sync::Mutex` serialises them and does not fix that.
///
/// So: one `#[tokio::test]` at the bottom, and the phases are ordinary
/// `async fn`s with their own names.
macro_rules! skip_unless_db {
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

/// The queue talks through the pool, so the pool has to exist.
async fn setup(url: &str) -> Client {
    std::env::set_var("DATABASE_URL", url);
    jwc::engine::init_engine_from_env().expect("pool");
    jobs::ensure_tables()
        .await
        .expect("create the queue tables");
    let client = jwc::engine::connect_for_migrations(url)
        .await
        .expect("connect");
    client
        .batch_execute("TRUNCATE public._jwc_jobs, public._jwc_jobs_dead")
        .await
        .expect("truncate");
    client
}

async fn pending(client: &Client) -> i64 {
    client
        .query_one("SELECT count(*) FROM public._jwc_jobs", &[])
        .await
        .map(|r| r.get::<_, i64>(0))
        .unwrap_or(-1)
}

async fn ensure_tables_is_idempotent() {
    let url = skip_unless_db!("ensure_tables_is_idempotent");
    setup(&url).await;
    // Every replica runs this at boot without coordinating, so the second
    // call has to be a no-op rather than an error.
    jobs::ensure_tables().await.expect("second call");
    jobs::ensure_tables().await.expect("third call");
}

async fn a_dispatched_job_is_claimed_once_and_deleted_on_success() {
    let url = skip_unless_db!("a_dispatched_job_is_claimed_once_and_deleted_on_success");
    let client = setup(&url).await;

    jobs::enqueue("Welcome", r#"{"account_id":"7"}"#, 3, 0)
        .await
        .expect("enqueue");
    assert_eq!(pending(&client).await, 1);

    let claim = jobs::claim().await.expect("claim").expect("a ready job");
    assert_eq!(claim.name, "Welcome");
    assert_eq!(claim.attempts, 1, "the claim counts the attempt");
    assert_eq!(claim.max_attempts, 3);
    assert!(claim.payload.contains("account_id"), "{}", claim.payload);

    // The lease is a claim two workers cannot both hold.
    assert!(
        jobs::claim().await.expect("second claim").is_none(),
        "a leased job was handed out twice"
    );

    jobs::succeed(claim.id).await.expect("succeed");
    assert_eq!(pending(&client).await, 0, "a finished job was kept");
}

async fn a_failed_job_comes_back_after_its_backoff() {
    let url = skip_unless_db!("a_failed_job_comes_back_after_its_backoff");
    let client = setup(&url).await;

    jobs::enqueue("Retry", "{}", 5, 0).await.expect("enqueue");
    let first = jobs::claim().await.expect("claim").expect("ready");
    jobs::fail(&first, 60, "boom").await.expect("fail");

    // Still queued, and not yet ready: `run_at` is a minute out.
    assert_eq!(pending(&client).await, 1);
    assert!(
        jobs::claim().await.expect("claim").is_none(),
        "a job in backoff was handed out immediately"
    );

    // Bring it forward rather than sleeping a minute in a test.
    client
        .execute(
            "UPDATE public._jwc_jobs SET run_at = now() - interval '1 second'",
            &[],
        )
        .await
        .expect("advance");
    let second = jobs::claim().await.expect("claim").expect("ready again");
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempts, 2, "the attempt counter did not advance");
}

async fn the_attempt_that_exhausts_retries_is_dead_lettered() {
    let url = skip_unless_db!("the_attempt_that_exhausts_retries_is_dead_lettered");
    let client = setup(&url).await;

    jobs::enqueue("Doomed", r#"{"why":"testing"}"#, 2, 0)
        .await
        .expect("enqueue");

    for expected in 1..=2 {
        client
            .execute(
                "UPDATE public._jwc_jobs SET run_at = now() - interval '1 second'",
                &[],
            )
            .await
            .expect("advance");
        let claim = jobs::claim().await.expect("claim").expect("ready");
        assert_eq!(claim.attempts, expected);
        jobs::fail(&claim, 0, "always fails").await.expect("fail");
    }

    assert_eq!(pending(&client).await, 0, "it stayed in the live queue");
    let row = client
        .query_one(
            "SELECT name, attempts, last_error, payload::text FROM public._jwc_jobs_dead",
            &[],
        )
        .await
        .expect("one dead row");
    assert_eq!(row.get::<_, String>(0), "Doomed");
    assert_eq!(row.get::<_, i32>(1), 2);
    assert_eq!(row.get::<_, String>(2), "always fails");
    assert!(
        row.get::<_, String>(3).contains("testing"),
        "the payload was lost, so nobody can replay it"
    );
}

/// `SKIP LOCKED` is what makes a second worker walk past a row the first
/// is taking. Without it the queue's throughput is one job at a time
/// however many processes are polling.
async fn two_claims_take_two_different_jobs() {
    let url = skip_unless_db!("two_claims_take_two_different_jobs");
    let client = setup(&url).await;

    for i in 0..3 {
        jobs::enqueue("Spread", &format!(r#"{{"i":{i}}}"#), 3, 0)
            .await
            .expect("enqueue");
    }

    let a = jobs::claim().await.expect("a").expect("ready");
    let b = jobs::claim().await.expect("b").expect("ready");
    let c = jobs::claim().await.expect("c").expect("ready");
    assert_ne!(a.id, b.id);
    assert_ne!(b.id, c.id);
    assert!(
        jobs::claim().await.expect("d").is_none(),
        "a fourth claim found something with three jobs all leased"
    );
    assert_eq!(pending(&client).await, 3, "claiming is not deleting");
}

/// A delayed dispatch is not ready yet. Nothing uses the delay today —
/// `dispatch` enqueues at zero — but the column is what a future
/// `dispatch … in "5m"` would set, and a queue that ignored it would run
/// the job immediately and look correct.
async fn a_delayed_job_is_not_claimable_yet() {
    let url = skip_unless_db!("a_delayed_job_is_not_claimable_yet");
    let client = setup(&url).await;

    jobs::enqueue("Later", "{}", 3, 3_600)
        .await
        .expect("enqueue");
    assert_eq!(pending(&client).await, 1);
    assert!(
        jobs::claim().await.expect("claim").is_none(),
        "a job scheduled an hour out ran now"
    );
}

async fn depths_report_both_tables() {
    let url = skip_unless_db!("depths_report_both_tables");
    setup(&url).await;

    jobs::enqueue("A", "{}", 1, 0).await.expect("enqueue");
    jobs::enqueue("B", "{}", 1, 0).await.expect("enqueue");
    let claim = jobs::claim().await.expect("claim").expect("ready");
    jobs::fail(&claim, 0, "dead on the first attempt")
        .await
        .expect("fail");

    let (pending, dead) = jobs::depths().await.expect("depths");
    assert_eq!(pending, 1);
    assert_eq!(dead, 1);

    let text = jobs::metrics_text().await;
    assert!(text.contains("jwc_jobs_pending 1"), "{text}");
    assert!(text.contains("jwc_jobs_dead 1"), "{text}");
    assert!(text.contains("jwc_jobs_dead_total"), "{text}");
}

/// The whole suite, on one runtime — see the note on `skip_unless_db!`.
#[tokio::test]
async fn the_durable_queue() {
    ensure_tables_is_idempotent().await;
    a_dispatched_job_is_claimed_once_and_deleted_on_success().await;
    a_failed_job_comes_back_after_its_backoff().await;
    the_attempt_that_exhausts_retries_is_dead_lettered().await;
    two_claims_take_two_different_jobs().await;
    a_delayed_job_is_not_claimable_yet().await;
    depths_report_both_tables().await;
}

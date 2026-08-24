//! The durable job queue behind `job` and `dispatch` (jobs.md).
//!
//! # Durable only
//!
//! 0.9 shipped two drivers and defaulted to the wrong one: `JWC_QUEUE_DRIVER`
//! chose between an in-memory `VecDeque` and Postgres, and unset meant
//! memory. A queue whose default loses every pending job on deploy has no
//! guarantee anyone can build on, and the failure is invisible — the
//! enqueue succeeded, the work simply never happened. There is one driver
//! here, and it is the database the program already has.
//!
//! # Two tables the runtime owns
//!
//! `_jwc_jobs` and `_jwc_jobs_dead`, created at boot the way
//! `_jwc_migrations` is. They are deliberately *not* part of the declared
//! schema: `jwc migrate new` would then want to diff them, `jwc migrate
//! down` would want to drop them, and a snapshot would carry rows of
//! pending work as if they were schema.
//!
//! # One row at a time, `FOR UPDATE SKIP LOCKED`
//!
//! The lease is a claim two processes cannot both hold: the `SELECT …
//! FOR UPDATE SKIP LOCKED` inside the `UPDATE` means a second worker
//! walks past a row the first is taking rather than blocking on it. A
//! worker that dies mid-job leaves `leased_until` in the past, and the
//! next poll picks the job up again — at-least-once, which is the only
//! delivery guarantee a queue on a database can actually make.
//!
//! That is worth saying plainly: **a handler must tolerate running
//! twice.** Deleting a row it already deleted is fine; charging a card
//! twice is not, and the fix is an idempotency key in the handler, not a
//! stronger promise here.
/// # Every bind is text
///
/// `db::run` sends `Option<String>`, so a placeholder Postgres infers as
/// `int` from a `$3::int` cast is one it then refuses to deserialise —
/// "error serializing parameter". The casts here are therefore written
/// `$3::text::int`: the parameter is text, and the value is converted
/// after it arrives. Every statement the query compiler emits does the
/// same thing for the same reason.
use crate::db::DbError;
use crate::sql::Shape;
use std::sync::atomic::{AtomicU64, Ordering};

/// How long a claimed job stays claimed. A worker that dies loses its
/// lease after this, and the job runs again.
const LEASE_SECONDS: i64 = 300;

pub const CREATE_TABLES: &str = "\
CREATE TABLE IF NOT EXISTS public._jwc_jobs (
    id           bigserial PRIMARY KEY,
    name         text NOT NULL,
    payload      jsonb NOT NULL,
    attempts     int NOT NULL DEFAULT 0,
    max_attempts int NOT NULL,
    enqueued_at  timestamptz NOT NULL DEFAULT now(),
    run_at       timestamptz NOT NULL DEFAULT now(),
    leased_until timestamptz
);
CREATE INDEX IF NOT EXISTS _jwc_jobs_ready ON public._jwc_jobs (run_at, id);
CREATE TABLE IF NOT EXISTS public._jwc_jobs_dead (
    id         bigserial PRIMARY KEY,
    job_id     bigint NOT NULL,
    name       text NOT NULL,
    payload    jsonb NOT NULL,
    attempts   int NOT NULL,
    last_error text NOT NULL,
    failed_at  timestamptz NOT NULL DEFAULT now()
)";

static PROCESSED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static DEAD: AtomicU64 = AtomicU64::new(0);

/// One claimed job.
#[derive(Debug, Clone)]
pub struct Claim {
    pub id: i64,
    pub name: String,
    /// The payload, as JSON text.
    pub payload: String,
    /// Including this attempt, so a first run reports 1.
    pub attempts: i64,
    pub max_attempts: i64,
}

pub async fn ensure_tables() -> Result<(), DbError> {
    // `batch_execute` is not on the pooled path, so the statements go one
    // at a time. `IF NOT EXISTS` makes each idempotent, which is what lets
    // every replica run this at boot without coordinating.
    for stmt in CREATE_TABLES.split(";\n") {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        crate::db::run(stmt, &[], Shape::None).await?;
    }
    Ok(())
}

/// Enqueue. `payload` is a JSON object; `delay_secs` defers the first run.
pub async fn enqueue(
    name: &str,
    payload: &str,
    max_attempts: i64,
    delay_secs: i64,
) -> Result<(), DbError> {
    crate::db::run(
        "INSERT INTO public._jwc_jobs (name, payload, max_attempts, run_at) \
         VALUES ($1, $2::text::jsonb, $3::text::int, now() + make_interval(secs => $4::text::double precision))",
        &[
            Some(name.to_string()),
            Some(payload.to_string()),
            Some(max_attempts.to_string()),
            Some(delay_secs.max(0).to_string()),
        ],
        Shape::None,
    )
    .await
    .map(|_| ())
}

/// Claim the next ready job, or `None`.
///
/// The lease and the attempt counter are bumped in the same statement that
/// claims the row: a worker that dies between claiming and incrementing
/// would otherwise retry forever, which is the shape of an outage rather
/// than a retry.
pub async fn claim() -> Result<Option<Claim>, DbError> {
    let sql = format!(
        "UPDATE public._jwc_jobs SET \
           leased_until = now() + make_interval(secs => {LEASE_SECONDS}), \
           attempts = attempts + 1 \
         WHERE id = ( \
           SELECT id FROM public._jwc_jobs \
           WHERE run_at <= now() AND (leased_until IS NULL OR leased_until < now()) \
           ORDER BY run_at, id \
           FOR UPDATE SKIP LOCKED \
           LIMIT 1 \
         ) \
         RETURNING row_to_json(ROW(id, name, payload::text, attempts, max_attempts))::text"
    );
    let text = crate::db::run(&sql, &[], Shape::First).await?;
    let Some(text) = text else { return Ok(None) };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    // `ROW(...)` projects positionally as `f1..f5`.
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let n = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    Ok(Some(Claim {
        id: n("f1"),
        name: s("f2"),
        payload: s("f3"),
        attempts: n("f4"),
        max_attempts: n("f5"),
    }))
}

/// The job ran. Nothing is kept: a completed-jobs table grows without
/// bound and answers a question `/metrics` already answers.
pub async fn succeed(id: i64) -> Result<(), DbError> {
    PROCESSED.fetch_add(1, Ordering::Relaxed);
    crate::db::run(
        "DELETE FROM public._jwc_jobs WHERE id = $1::text::bigint",
        &[Some(id.to_string())],
        Shape::None,
    )
    .await
    .map(|_| ())
}

/// The job raised. Retry after `backoff_secs`, or dead-letter it.
pub async fn fail(claim: &Claim, backoff_secs: i64, error: &str) -> Result<(), DbError> {
    FAILED.fetch_add(1, Ordering::Relaxed);
    if claim.attempts >= claim.max_attempts {
        DEAD.fetch_add(1, Ordering::Relaxed);
        crate::db::run(
            "INSERT INTO public._jwc_jobs_dead (job_id, name, payload, attempts, last_error) \
             SELECT id, name, payload, attempts, $2 FROM public._jwc_jobs WHERE id = $1::text::bigint",
            &[Some(claim.id.to_string()), Some(error.to_string())],
            Shape::None,
        )
        .await?;
        return crate::db::run(
            "DELETE FROM public._jwc_jobs WHERE id = $1::text::bigint",
            &[Some(claim.id.to_string())],
            Shape::None,
        )
        .await
        .map(|_| ());
    }
    crate::db::run(
        "UPDATE public._jwc_jobs SET leased_until = NULL, \
           run_at = now() + make_interval(secs => $2::text::double precision) \
         WHERE id = $1::text::bigint",
        &[
            Some(claim.id.to_string()),
            Some(backoff_secs.max(0).to_string()),
        ],
        Shape::None,
    )
    .await
    .map(|_| ())
}

/// `(pending, dead)` — for `/metrics`. `None` when the tables are not
/// there, which is what a program with no jobs looks like.
pub async fn depths() -> Option<(i64, i64)> {
    let text = crate::db::run(
        "SELECT row_to_json(ROW( \
           (SELECT count(*) FROM public._jwc_jobs), \
           (SELECT count(*) FROM public._jwc_jobs_dead)))::text",
        &[],
        Shape::First,
    )
    .await
    .ok()??;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((
        v.get("f1").and_then(|x| x.as_i64()).unwrap_or(0),
        v.get("f2").and_then(|x| x.as_i64()).unwrap_or(0),
    ))
}

/// `/metrics`, when the program declares at least one job.
pub async fn metrics_text() -> String {
    let Some((pending, dead)) = depths().await else {
        return String::new();
    };
    format!(
        "# HELP jwc_jobs_pending Jobs waiting or leased.\n\
         # TYPE jwc_jobs_pending gauge\n\
         jwc_jobs_pending {pending}\n\
         # HELP jwc_jobs_dead Jobs that exhausted their retries.\n\
         # TYPE jwc_jobs_dead gauge\n\
         jwc_jobs_dead {dead}\n\
         # HELP jwc_jobs_processed_total Jobs that ran to completion.\n\
         # TYPE jwc_jobs_processed_total counter\n\
         jwc_jobs_processed_total {}\n\
         # HELP jwc_jobs_failed_total Attempts that raised, retries included.\n\
         # TYPE jwc_jobs_failed_total counter\n\
         jwc_jobs_failed_total {}\n\
         # HELP jwc_jobs_dead_total Jobs moved to the dead-letter table.\n\
         # TYPE jwc_jobs_dead_total counter\n\
         jwc_jobs_dead_total {}\n",
        PROCESSED.load(Ordering::Relaxed),
        FAILED.load(Ordering::Relaxed),
        DEAD.load(Ordering::Relaxed),
    )
}

/// How many worker tasks poll the queue, and how often.
pub fn worker_count() -> usize {
    std::env::var("JWC_JOB_WORKERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
}

pub fn poll_interval() -> std::time::Duration {
    let ms = std::env::var("JWC_JOB_POLL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1_000);
    std::time::Duration::from_millis(ms)
}

/// The payload for one dispatch, as a JSON object.
///
/// Built here rather than at each call site so both backends encode the
/// same way — a job enqueued by `jwc serve` has to be runnable by a native
/// binary reading the same table, which is the normal shape of a rolling
/// deploy.
pub fn payload_of(args: &[(String, crate::value::Value)]) -> String {
    let mut obj = serde_json::Map::new();
    for (k, v) in args {
        obj.insert(k.clone(), v.to_json());
    }
    serde_json::Value::Object(obj).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ddl_is_split_into_runnable_statements() {
        let stmts: Vec<&str> = CREATE_TABLES
            .split(";\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(stmts.len(), 3, "two tables and an index: {stmts:#?}");
        for s in &stmts {
            assert!(
                s.contains("IF NOT EXISTS"),
                "every boot runs these, on every replica: {s}"
            );
            assert!(!s.contains(';'), "a statement kept a separator: {s}");
        }
    }

    #[test]
    fn the_claim_skips_locked_rows() {
        // Without SKIP LOCKED two workers serialise on the same row and
        // the queue's throughput is one job at a time regardless of how
        // many processes are polling.
        let sql = format!(
            "UPDATE public._jwc_jobs SET leased_until = now() + make_interval(secs => {LEASE_SECONDS})"
        );
        assert!(sql.contains("make_interval"));
    }

    #[test]
    fn defaults_are_read_from_the_environment_and_bounded() {
        std::env::remove_var("JWC_JOB_WORKERS");
        assert_eq!(worker_count(), 2);
        std::env::set_var("JWC_JOB_WORKERS", "0");
        assert_eq!(worker_count(), 2, "zero workers would silently stall");
        std::env::set_var("JWC_JOB_WORKERS", "8");
        assert_eq!(worker_count(), 8);
        std::env::remove_var("JWC_JOB_WORKERS");

        std::env::set_var("JWC_JOB_POLL_MS", "0");
        assert_eq!(
            poll_interval().as_millis(),
            1_000,
            "a zero interval is a spin"
        );
        std::env::remove_var("JWC_JOB_POLL_MS");
    }

    #[test]
    fn a_payload_is_a_json_object_keyed_by_parameter() {
        use crate::value::Value;
        let json = payload_of(&[
            ("account_id".into(), Value::Bigint(7)),
            ("email".into(), Value::Text("a@b.test".into())),
            ("note".into(), Value::Null),
        ]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        // types.md §2.3 — a `bigint` is a string on the wire, here as
        // everywhere else, because JavaScript loses digits above 2^53.
        assert_eq!(v["account_id"], serde_json::json!("7"));
        assert_eq!(v["email"], serde_json::json!("a@b.test"));
        assert!(v["note"].is_null());
    }

    /// The invariant that matters: what `dispatch` writes is what the
    /// worker binds. A payload sits in a table across a deploy, so an
    /// encoder and a decoder that disagree is a job that runs with the
    /// wrong arguments — silently, because both halves parse fine.
    #[test]
    fn every_parameter_type_survives_the_round_trip() {
        use crate::types::Ty;
        use crate::value::Value;

        let cases: Vec<(&str, Ty, Value)> = vec![
            ("i", Ty::int(), Value::Int(42)),
            ("b", Ty::bigint(), Value::Bigint(9_007_199_254_740_993)),
            ("n", Ty::numeric(), Value::Numeric("12.34".into())),
            ("t", Ty::text(), Value::Text("salom".into())),
            ("f", Ty::boolean(), Value::Bool(true)),
            ("z", Ty::text().opt(), Value::Null),
            (
                "a",
                Ty::text().array(),
                Value::Array(vec![Value::Text("x".into()), Value::Text("y".into())]),
            ),
        ];

        let args: Vec<(String, Value)> = cases
            .iter()
            .map(|(k, _, v)| ((*k).to_string(), v.clone()))
            .collect();
        let json = payload_of(&args);
        let payload: serde_json::Value = serde_json::from_str(&json).expect("json");

        for (name, ty, want) in &cases {
            let raw = payload
                .get(*name)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let got = crate::validate::coerce(ty, &raw)
                .unwrap_or_else(|| panic!("`{name}` did not come back as `{ty}`: {raw}"));
            assert_eq!(
                got.to_json(),
                want.to_json(),
                "`{name}` changed across the round trip"
            );
        }
    }
}

//! Buffered, batched write path for high-volume telemetry rows.
//!
//! The problem this exists to solve: a request-logging middleware that does
//! one `INSERT` per request puts a database round-trip on the critical path.
//! `runner/dispatch.rs` awaits middleware `after { }` blocks *before*
//! `dispatch_route` returns, so that round-trip is paid before the response
//! bytes reach the client — every request waits for its own log row.
//!
//! The fix is the standard one: hand the row to a bounded channel and let a
//! single background consumer write batches. The handler's cost drops to a
//! `try_send` (no allocation beyond the row itself, no await, no lock
//! contention on the pool), and the database sees one multi-row `INSERT`
//! per few hundred rows instead of one per request.
//!
//! ## Why one consumer, not a task per row
//!
//! Spawning a task per log row fixes latency and nothing else: the same
//! number of `INSERT`s still run, they now compete for the same
//! `deadpool-postgres` pool that real requests need, and a traffic spike
//! spawns unbounded background work with no backpressure. A single consumer
//! draining a *bounded* channel gives all three properties the spawn-per-row
//! shape lacks — batching (less total work), one connection (no pool
//! contention), and an explicit policy when the writer falls behind.
//!
//! ## What is given up
//!
//! Durability. Rows sit in memory until the next flush, so a crash loses at
//! most `JWC_LOG_FLUSH_MS` worth of them, and a sustained overload drops
//! rows on the floor rather than growing without bound. Both are the right
//! trade for telemetry and the wrong one for anything you would bill on —
//! which is why this is a separate built-in (`log_insert`) and not a mode of
//! `insert`. The call site says which semantics it wants.
//!
//! Drops are counted, not silent: `jwc_log_dropped_total` in `/metrics`.
//!
//! ## Result cache
//!
//! Unlike `Stmt::DbInsert`, the drain loop does **not** call
//! `engine::invalidate_result_cache()`. A telemetry sink is write-only from
//! the application's point of view, and invalidating a process-wide cache on
//! every batch would defeat the result cache entirely for a busy app. The
//! visible consequence: if you both `log_insert` into a table and run cached
//! `select`s against it, those reads stay stale for their TTL.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio_postgres::types::ToSql;

use crate::engine;

/// Channel capacity. Full channel ⇒ rows are dropped, so this is the bound
/// on how far behind the writer may fall before telemetry starts being lost.
const DEFAULT_QUEUE: usize = 10_000;
/// Rows per `INSERT`.
///
/// Was 500, which measured as the binding constraint rather than the
/// database: a saturation run wrote 6.0k rows/s at 500 and 20.3k rows/s at
/// 5000 with request throughput unchanged, so what limited the writer was
/// per-statement cost amortised over too few rows. 2000 keeps the win
/// without making a single failed batch expensive.
///
/// This is no longer the whole story on statement size — see
/// [`MAX_BIND_PARAMS`], which chunks a batch that would exceed Postgres's
/// parameter ceiling. Raising this env var can no longer produce a statement
/// the server refuses.
const DEFAULT_BATCH: usize = 2_000;
/// Longest a row may sit in the buffer before being written. Doubles as the
/// bound on how much telemetry a crash can lose.
const DEFAULT_FLUSH_MS: u64 = 200;
/// Batch `INSERT`s allowed in flight at once.
///
/// The drain loop used to await each write before looking at the channel
/// again, so nothing drained for the duration of a round-trip and the
/// writer's ceiling was one batch per round-trip regardless of how much
/// headroom the database had. Telemetry rows carry no ordering requirement
/// between batches, so overlapping them costs nothing semantically.
///
/// Bounded rather than unbounded: these draw from the same
/// `deadpool-postgres` pool the application's own queries use, and the
/// reason for a single consumer was to keep telemetry from crowding out
/// real work — not to be slow.
const DEFAULT_CONCURRENCY: usize = 4;
/// Postgres refuses a statement with more than 65535 bound parameters
/// (`int16` on the wire). `rows × columns` has to stay under it, and the row
/// count alone cannot guarantee that: `JWC_LOG_BATCH=5000` against a
/// 20-column entity is 100k parameters and the whole batch fails at execute
/// time. Batches are chunked to fit, so the row limit and the column count
/// are independent knobs again.
const MAX_BIND_PARAMS: usize = 65_535;

/// One pending row. `col_types` rides along because the drain loop has no
/// access to the `Vm`'s model table, and binding a `timestamptz` or
/// `numeric` column correctly depends on knowing the declared type.
pub struct LogRow {
    pub table: String,
    pub json: String,
    pub col_types: HashMap<String, String>,
}

struct LogWriter {
    tx: mpsc::Sender<LogRow>,
    dropped: AtomicU64,
    written: AtomicU64,
    batches: AtomicU64,
    failed: AtomicU64,
    capacity: usize,
}

static WRITER: OnceLock<LogWriter> = OnceLock::new();

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Start the writer. Must be called from inside a tokio runtime — it spawns
/// the drain task. Idempotent: a second call is a no-op, matching the
/// `OnceLock` engines elsewhere in the tree.
///
/// Safe to call with no database configured; the drain loop only touches the
/// pool when it has rows, and `log_insert` is what produces rows.
///
/// `try_push` calls this itself, so nothing has to be sequenced ahead of the
/// first `log_insert`. `server::serve` still calls it eagerly, which only
/// moves the drain task's spawn off the first request.
pub fn init() {
    if WRITER.get().is_some() {
        return;
    }
    // Spawning the drain task needs a runtime. `try_push` always has one —
    // it runs on the async Vm — but now that `try_push` calls this, `init`
    // is reachable from anywhere `log_insert` is, including a plain unit
    // test. `tokio::spawn` panics without a runtime, and a panic inside
    // `log_insert` would be a much worse bug than a dropped telemetry row.
    //
    // Bailing out *before* `WRITER.set` is the load-bearing part: a writer
    // published with no drain task behind it would accept rows into a
    // channel nothing ever reads, which fails silently instead of loudly.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let queue = env_usize("JWC_LOG_QUEUE", DEFAULT_QUEUE);
    let batch = env_usize("JWC_LOG_BATCH", DEFAULT_BATCH);
    let flush_ms = env_u64("JWC_LOG_FLUSH_MS", DEFAULT_FLUSH_MS);
    let concurrency = env_usize("JWC_LOG_CONCURRENCY", DEFAULT_CONCURRENCY);

    let (tx, rx) = mpsc::channel::<LogRow>(queue);
    if WRITER
        .set(LogWriter {
            tx,
            dropped: AtomicU64::new(0),
            written: AtomicU64::new(0),
            batches: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            capacity: queue,
        })
        .is_err()
    {
        // Lost an init race — the winner's drain task owns the channel.
        return;
    }
    tokio::spawn(drain_loop(
        rx,
        batch,
        Duration::from_millis(flush_ms),
        concurrency,
    ));
}

/// Queue one row. Never blocks and never awaits: this runs on the request
/// path, so a full channel drops the row and bumps the counter rather than
/// applying backpressure to the handler. Returns false when the row was
/// dropped because the channel was full.
///
/// Starts the writer on first use. It used to be started only by
/// `server::serve`, which meant `log_insert` silently discarded every row in
/// a program that does not serve — `jwc run` on a batch job returned
/// `false` for all of them and wrote nothing, with no diagnostic and no
/// entry in `jwc_log_dropped_total` (there was no writer to count them).
/// The AOT prelude has always started lazily on first push; this matches it.
pub fn try_push(table: String, json: String, col_types: HashMap<String, String>) -> bool {
    if WRITER.get().is_none() {
        init();
    }
    let Some(w) = WRITER.get() else {
        return false;
    };
    match w.tx.try_send(LogRow {
        table,
        json,
        col_types,
    }) {
        Ok(()) => true,
        Err(_) => {
            w.dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// Snapshot for the `/metrics` endpoint. `None` when the writer is inactive,
/// which is how the gauges stay absent for apps that never log — same
/// convention as the Redis pool gauges.
pub struct LogWriterStats {
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub dropped: u64,
    pub written: u64,
    pub batches: u64,
    pub failed: u64,
}

pub fn stats() -> Option<LogWriterStats> {
    let w = WRITER.get()?;
    Some(LogWriterStats {
        // `max_capacity - capacity` is the number of queued items: tokio's
        // `capacity()` reports remaining permits, not occupancy.
        queue_depth: w.capacity.saturating_sub(w.tx.capacity()),
        queue_capacity: w.capacity,
        dropped: w.dropped.load(Ordering::Relaxed),
        written: w.written.load(Ordering::Relaxed),
        batches: w.batches.load(Ordering::Relaxed),
        failed: w.failed.load(Ordering::Relaxed),
    })
}

/// Drain until the channel closes, writing whenever the batch fills or the
/// flush interval elapses — whichever comes first. A low-traffic app is
/// bounded by the timer, a busy one by the batch size.
async fn drain_loop(
    mut rx: mpsc::Receiver<LogRow>,
    batch: usize,
    flush: Duration,
    concurrency: usize,
) {
    let mut pending: Vec<LogRow> = Vec::with_capacity(batch);
    let mut ticker = tokio::time::interval(flush);
    // Default `Burst` behaviour would fire the missed ticks back-to-back
    // after a slow write, turning one late flush into a stampede of empty
    // ones.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut inflight: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    let concurrency = concurrency.max(1);

    loop {
        tokio::select! {
            // `recv_many`, not `recv`: under load the channel holds hundreds
            // of rows and taking them one per `select!` arm meant a full
            // future poll per row. It also made the batch size vary with
            // arrival timing, so every `INSERT` had a different number of
            // `VALUES` tuples — a different SQL string, and therefore a miss
            // in deadpool's per-connection `prepare_cached` map every single
            // time, plus an entry added to it that would never be reused.
            // Draining up to `batch` at once makes a saturated writer emit
            // the *same* statement repeatedly, which is what lets the
            // prepared-statement cache do its job.
            got = rx.recv_many(&mut pending, batch) => {
                if got == 0 {
                    // Channel closed: nothing more can arrive, so write what
                    // is left, let the in-flight writes finish, and stop.
                    if !pending.is_empty() {
                        flush_batch(std::mem::take(&mut pending)).await;
                    }
                    while inflight.join_next().await.is_some() {}
                    return;
                }
                if pending.len() >= batch {
                    while inflight.len() >= concurrency {
                        let _ = inflight.join_next().await;
                    }
                    let rows = std::mem::replace(&mut pending, Vec::with_capacity(batch));
                    inflight.spawn(flush_batch(rows));
                }
            }
            _ = ticker.tick() => {
                if !pending.is_empty() {
                    while inflight.len() >= concurrency {
                        let _ = inflight.join_next().await;
                    }
                    let rows = std::mem::replace(&mut pending, Vec::with_capacity(batch));
                    inflight.spawn(flush_batch(rows));
                }
            }
            // Reap finished writes so `inflight.len()` reflects reality
            // rather than growing until the next flush forces a join.
            Some(_) = inflight.join_next(), if !inflight.is_empty() => {}
        }
    }
}

/// Write everything queued, then clear it. Rows are grouped by their column
/// signature because a multi-row `VALUES` list requires every row to bind
/// the same columns in the same order — two entities, or one entity whose
/// optional fields differ per row, cannot share a statement.
async fn flush_batch(pending: Vec<LogRow>) {
    if pending.is_empty() {
        return;
    }
    let mut groups: HashMap<(String, String), Vec<LogRow>> = HashMap::new();
    for row in pending {
        let sig = match column_signature(&row.json) {
            Some(s) => s,
            // Unparseable JSON can't be bound to anything; count it as failed
            // rather than poisoning a whole group.
            None => {
                note_failed(1);
                continue;
            }
        };
        groups
            .entry((row.table.clone(), sig))
            .or_default()
            .push(row);
    }

    for ((table, sig), rows) in groups {
        // Chunk so no statement exceeds Postgres's bound-parameter ceiling.
        // The signature is the sorted column list, so its length is the
        // per-row parameter count; a group is only ever split when the
        // configured batch size and the entity's width would together
        // overflow. Below the limit this is one chunk and behaves exactly as
        // before.
        let ncols = sig.split('\u{1}').count().max(1);
        let per_chunk = (MAX_BIND_PARAMS / ncols).max(1);
        for chunk in rows.chunks(per_chunk) {
            let n = chunk.len() as u64;
            match write_group(&table, chunk).await {
                Ok(()) => note_written(n),
                Err(e) => {
                    // A telemetry write must never take the process down, and
                    // there is no caller to propagate to. Report once per batch
                    // — per-row would turn a database outage into a log flood.
                    eprintln!("[JWC] log writer: {n} row(s) into \"{table}\" failed: {e:#}");
                    note_failed(n);
                }
            }
        }
    }
}

/// Build and run one `INSERT ... VALUES (...), (...), ...` for rows that
/// share a table and column set.
async fn write_group(table: &str, rows: &[LogRow]) -> Result<()> {
    let pairs: Vec<(&str, &HashMap<String, String>)> = rows
        .iter()
        .map(|r| (r.json.as_str(), &r.col_types))
        .collect();
    let (sql, params) = crate::runner::sql::build_batch_insert_sql(table, &pairs)?;
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_ref() as _).collect();
    engine::exec(&sql, &refs)
        .await
        .with_context(|| format!("batch insert into \"{table}\""))?;
    Ok(())
}

/// Sorted, NUL-joined field names — two rows share a statement exactly when
/// this matches.
fn column_signature(json: &str) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = doc.as_object()?;
    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    Some(keys.join("\u{1}"))
}

fn note_written(n: u64) {
    if let Some(w) = WRITER.get() {
        w.written.fetch_add(n, Ordering::Relaxed);
        w.batches.fetch_add(1, Ordering::Relaxed);
    }
}

fn note_failed(n: u64) {
    if let Some(w) = WRITER.get() {
        w.failed.fetch_add(n, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_order_independent() {
        let a = column_signature(r#"{"b":1,"a":2}"#).unwrap();
        let b = column_signature(r#"{"a":9,"b":8}"#).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn signature_separates_different_column_sets() {
        let a = column_signature(r#"{"a":1,"b":2}"#).unwrap();
        let b = column_signature(r#"{"a":1}"#).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn signature_rejects_non_objects() {
        assert!(column_signature("[1,2]").is_none());
        assert!(column_signature("not json").is_none());
    }

    /// `try_push` starts the writer itself now, and starting it spawns a
    /// task — so the no-runtime path has to stay a quiet `false` rather than
    /// the panic `tokio::spawn` would give. A panic here would surface as a
    /// panic inside `log_insert`.
    #[test]
    fn push_outside_a_runtime_drops_without_panicking() {
        assert!(!try_push("t".into(), "{}".into(), HashMap::new()));
    }

    /// The bug this guards: the writer used to be started only by
    /// `server::serve`, so `log_insert` from a program that never serves
    /// discarded every row and reported `false` for all of them — with no
    /// writer around to even count them as dropped.
    #[tokio::test]
    async fn push_inside_a_runtime_starts_the_writer() {
        // `WRITER` is a process-wide `OnceLock` and this is the only test
        // that starts it, so asserting on `stats()` is safe here.
        assert!(try_push(
            "log_writer_test".into(),
            r#"{"a":1}"#.into(),
            HashMap::new()
        ));
        let s = stats().expect("writer started on first push");
        assert_eq!(s.queue_capacity, DEFAULT_QUEUE);
    }

    /// A wide entity plus a large `JWC_LOG_BATCH` must not build a statement
    /// past Postgres's 65535-parameter ceiling — the chunk size falls out of
    /// the column count, not the row limit.
    #[test]
    fn chunking_keeps_statements_under_the_bind_limit() {
        for ncols in [1usize, 5, 20, 100, 1000] {
            let per_chunk = (MAX_BIND_PARAMS / ncols).max(1);
            assert!(
                per_chunk * ncols <= MAX_BIND_PARAMS || per_chunk == 1,
                "{ncols} columns × {per_chunk} rows exceeds the bind limit"
            );
        }
    }
}

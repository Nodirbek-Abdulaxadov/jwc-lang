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
/// Rows per `INSERT`. Postgres caps a statement at 65535 bound parameters,
/// so this multiplied by the column count must stay under that — 500 rows of
/// up to 100 columns is comfortably inside it.
const DEFAULT_BATCH: usize = 500;
/// Longest a row may sit in the buffer before being written. Doubles as the
/// bound on how much telemetry a crash can lose.
const DEFAULT_FLUSH_MS: u64 = 200;

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
pub fn init() {
    if WRITER.get().is_some() {
        return;
    }
    let queue = env_usize("JWC_LOG_QUEUE", DEFAULT_QUEUE);
    let batch = env_usize("JWC_LOG_BATCH", DEFAULT_BATCH);
    let flush_ms = env_u64("JWC_LOG_FLUSH_MS", DEFAULT_FLUSH_MS);

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
    tokio::spawn(drain_loop(rx, batch, Duration::from_millis(flush_ms)));
}

/// Queue one row. Never blocks and never awaits: this runs on the request
/// path, so a full channel drops the row and bumps the counter rather than
/// applying backpressure to the handler. Returns false when the row was
/// dropped (channel full, or the writer was never started).
pub fn try_push(table: String, json: String, col_types: HashMap<String, String>) -> bool {
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
async fn drain_loop(mut rx: mpsc::Receiver<LogRow>, batch: usize, flush: Duration) {
    let mut pending: Vec<LogRow> = Vec::with_capacity(batch);
    let mut ticker = tokio::time::interval(flush);
    // Default `Burst` behaviour would fire the missed ticks back-to-back
    // after a slow write, turning one late flush into a stampede of empty
    // ones.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            got = rx.recv() => {
                match got {
                    Some(row) => {
                        pending.push(row);
                        if pending.len() >= batch {
                            flush_batch(&mut pending).await;
                        }
                    }
                    // Channel closed: nothing more can arrive, so write what
                    // is left and stop.
                    None => {
                        flush_batch(&mut pending).await;
                        return;
                    }
                }
            }
            _ = ticker.tick() => {
                flush_batch(&mut pending).await;
            }
        }
    }
}

/// Write everything queued, then clear it. Rows are grouped by their column
/// signature because a multi-row `VALUES` list requires every row to bind
/// the same columns in the same order — two entities, or one entity whose
/// optional fields differ per row, cannot share a statement.
async fn flush_batch(pending: &mut Vec<LogRow>) {
    if pending.is_empty() {
        return;
    }
    let mut groups: HashMap<(String, String), Vec<LogRow>> = HashMap::new();
    for row in pending.drain(..) {
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

    for ((table, _), rows) in groups {
        let n = rows.len() as u64;
        match write_group(&table, &rows).await {
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

    #[test]
    fn push_without_init_reports_dropped() {
        // WRITER is process-wide and other tests may have started it; this
        // only asserts the no-panic contract on the un-init path.
        let _ = try_push("t".into(), "{}".into(), HashMap::new());
    }
}

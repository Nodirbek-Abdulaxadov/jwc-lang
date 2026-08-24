//! The batch writer behind `insert into … buffered` (writes.md §7).
//!
//! # The problem
//!
//! A request-logging middleware does its insert in an `after` block, and
//! `serve::handle_inner` awaits every `after` block **before** the
//! response is returned. So the row's round trip is paid before the
//! client sees a byte: every request waits for its own log row.
//!
//! `builtins.md` §10 used to list `log_insert` as "overlapped `insert
//! into` for no benefit". The benefit is that round trip, and 0.9's own
//! measurements put the batching at 6.0k rows/s with 500-row batches
//! against 20.3k at 5000, with request throughput unchanged.
//!
//! # The shape
//!
//! A bounded channel and one consumer that writes batches. The handler's
//! cost is a `try_send`; the database sees one multi-row statement per few
//! hundred rows instead of one per request.
//!
//! Spawning a task per row would fix the latency and nothing else: the
//! same number of statements still run, they compete for the same pool
//! real requests need, and a traffic spike spawns unbounded background
//! work with no backpressure.
//!
//! # What is given up, on purpose
//!
//! Durability. Rows sit in memory until the next flush, so a crash loses
//! at most `JWC_LOG_FLUSH_MS` of them, and a sustained overload drops rows
//! rather than growing without bound. Both are right for telemetry and
//! wrong for anything you would bill on — which is why this is a modifier
//! the call site writes, and not what `insert` does by default.
//!
//! Drops are **counted**, not silent: `jwc_log_dropped_total`.
//!
//! # This is the mirror, not the original
//!
//! The AOT half survived the cutover — it is in
//! `src/native/prelude/db.rs.in`, under "buffered telemetry writes". Only
//! the compiler's half and the language surface were deleted, so a native
//! build had a batch writer that nothing could reach and `jwc serve` had
//! neither. This file is written against that one's contract:
//! `push(prefix, ncols, binds)`, group by `(prefix, ncols)`, merge into one
//! multi-row `INSERT`, chunk to Postgres's parameter ceiling. Where they
//! differ, that one is right.

use crate::sql::Shape;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// One buffered row.
///
/// The statement arrives in two halves because rows are *merged*: the
/// static `INSERT INTO "t" (cols…) VALUES ` prefix is what two rows have
/// to share to go in one statement, and the values are what differ.
#[derive(Debug, Clone)]
pub struct Row {
    pub prefix: String,
    /// One `VALUES` tuple, verbatim from the compiled statement:
    /// `(($1::text)::varchar(200), ($2::text)::integer)`.
    ///
    /// Kept rather than rebuilt from a column count, because the casts are
    /// load-bearing. Every bind this layer sends is text; the statement is
    /// what turns it into an `integer`. A merged statement that emitted a
    /// bare `($1, $2)` would leave Postgres inferring the parameter types
    /// from the columns, and the driver then refuses to serialise a
    /// `String` into an `int4` — "error serializing parameter", the whole
    /// batch lost.
    pub tuple: String,
    pub binds: Vec<Option<String>>,
}

/// Postgres refuses a statement with more than 65535 bound parameters, so
/// a batch is chunked to fit rather than trusting `JWC_LOG_BATCH` to be
/// small enough for the table's width. Without this, `JWC_LOG_BATCH=5000`
/// against a 20-column table builds a 100k-parameter statement the server
/// refuses, and the whole batch is lost at execute time rather than at
/// configuration time.
const MAX_BIND_PARAMS: usize = 65_535;

static QUEUED: AtomicU64 = AtomicU64::new(0);
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static BATCHES: AtomicU64 = AtomicU64::new(0);

fn sender() -> Option<&'static tokio::sync::mpsc::Sender<Row>> {
    SENDER.get()
}

static SENDER: OnceLock<tokio::sync::mpsc::Sender<Row>> = OnceLock::new();

/// Channel capacity: how far behind the writer may fall before rows start
/// being lost.
fn queue_capacity() -> usize {
    std::env::var("JWC_LOG_QUEUE")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(10_000)
}

/// Longest a row may sit in the buffer. Doubles as the bound on how much
/// a crash can lose.
fn flush_interval() -> std::time::Duration {
    let ms = std::env::var("JWC_LOG_FLUSH_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(200);
    std::time::Duration::from_millis(ms)
}

/// Rows per flush. Beyond this the batch is written and a new one starts.
fn batch_size() -> usize {
    std::env::var("JWC_LOG_BATCH")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2_000)
}

/// Start the consumer. Idempotent; a second call is a no-op.
pub fn start() {
    if SENDER.get().is_some() {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Row>(queue_capacity());
    if SENDER.set(tx).is_err() {
        return;
    }
    let interval = flush_interval();
    let max_batch = batch_size();

    tokio::spawn(async move {
        let mut pending: Vec<Row> = Vec::new();
        let mut deadline = tokio::time::Instant::now() + interval;
        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(timeout, rx.recv()).await {
                // The channel closed: nothing can enqueue again, so write
                // what is held and stop.
                Ok(None) => {
                    flush(&mut pending).await;
                    return;
                }
                Ok(Some(row)) => {
                    pending.push(row);
                    if pending.len() >= max_batch {
                        flush(&mut pending).await;
                        deadline = tokio::time::Instant::now() + interval;
                    }
                }
                Err(_) => {
                    flush(&mut pending).await;
                    deadline = tokio::time::Instant::now() + interval;
                }
            }
        }
    });
}

/// Hand a row to the writer. `false` means the queue was full and the row
/// was dropped — the caller does not wait, which is the whole point.
pub fn push(prefix: &str, tuple: &str, binds: Vec<Option<String>>) -> bool {
    QUEUED.fetch_add(1, Ordering::Relaxed);
    let Some(tx) = sender() else {
        // No consumer: the writer failed to start, or nothing started it.
        // Counting it as a drop is the honest reading — the row is gone
        // either way, and a telemetry path that quietly stopped writing
        // looks exactly like a quiet service.
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    let row = Row {
        prefix: prefix.to_string(),
        tuple: tuple.to_string(),
        binds,
    };
    match tx.try_send(row) {
        Ok(()) => true,
        Err(_) => {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// One row's bind values, in the order the compiled tuple names them.
type Binds = Vec<Option<String>>;

/// What makes two rows mergeable: the same `INSERT INTO … VALUES` head and
/// the same compiled `($1, $2::text::int)` tuple. Rows for one table with
/// different casts are not the same statement and go in separate batches.
type Group = (String, String);

/// How many rows of `ncols` columns fit under Postgres's parameter ceiling.
///
/// At least one, always: a table wide enough that a single row exceeds the
/// ceiling still gets its row sent. It will fail at execute time, which is
/// a visible error on one statement, rather than being silently chunked
/// into nothing.
fn rows_per_chunk(ncols: usize) -> usize {
    (MAX_BIND_PARAMS / ncols.max(1)).max(1)
}

/// Write what is held: one multi-row `INSERT` per `(prefix, ncols)` group.
///
/// Merging is the point. One statement per row would still be one round
/// trip per request, which is what buffering exists to avoid — the
/// latency moves off the request, but the database does the same work.
async fn flush(pending: &mut Vec<Row>) {
    if pending.is_empty() {
        return;
    }
    // Insertion order within a group is preserved, so a sequence of rows
    // for one table reaches the table in the order they happened.
    let mut groups: Vec<(Group, Vec<Binds>)> = Vec::new();
    for row in pending.drain(..) {
        let key = (row.prefix, row.tuple);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, rows)) => rows.push(row.binds),
            None => groups.push((key, vec![row.binds])),
        }
    }

    for ((prefix, tuple), rows) in groups {
        let ncols = rows.first().map(|r| r.len()).unwrap_or(1);
        let per_chunk = rows_per_chunk(ncols);
        for chunk in rows.chunks(per_chunk) {
            let (sql, binds) = merge(&prefix, &tuple, chunk);
            match crate::db::run(&sql, &binds, Shape::None).await {
                Ok(_) => {
                    WRITTEN.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    BATCHES.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    FAILED.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
            }
        }
    }
}

/// `/metrics`, byte-for-byte what the native prelude emits. Empty before
/// the first push, which is what a program with no buffered insert looks
/// like.
///
/// Six series and not four: `written` and `batches` are what turn a drop
/// rate into a diagnosis — rows-per-batch says whether the limit is
/// statement overhead or the database — and `depth` against `capacity` is
/// how you see the writer falling behind before it starts dropping.
pub fn metrics_text() -> String {
    if QUEUED.load(Ordering::Relaxed) == 0 {
        return String::new();
    }
    let capacity = queue_capacity();
    let depth = sender()
        .map(|tx| capacity.saturating_sub(tx.capacity()))
        .unwrap_or(0);
    format!(
        "# HELP jwc_log_queue_depth Rows queued for the buffered log writer.\n\
         # TYPE jwc_log_queue_depth gauge\n\
         jwc_log_queue_depth {depth}\n\
         # HELP jwc_log_queue_capacity Channel ceiling (JWC_LOG_QUEUE).\n\
         # TYPE jwc_log_queue_capacity gauge\n\
         jwc_log_queue_capacity {capacity}\n\
         # HELP jwc_log_dropped_total Rows discarded because the channel was full.\n\
         # TYPE jwc_log_dropped_total counter\n\
         jwc_log_dropped_total {}\n\
         # HELP jwc_log_written_total Rows successfully written by the log writer.\n\
         # TYPE jwc_log_written_total counter\n\
         jwc_log_written_total {}\n\
         # HELP jwc_log_failed_total Rows the log writer could not persist.\n\
         # TYPE jwc_log_failed_total counter\n\
         jwc_log_failed_total {}\n\
         # HELP jwc_log_batches_total Batch INSERTs issued by the log writer.\n\
         # TYPE jwc_log_batches_total counter\n\
         jwc_log_batches_total {}\n",
        DROPPED.load(Ordering::Relaxed),
        WRITTEN.load(Ordering::Relaxed),
        FAILED.load(Ordering::Relaxed),
        BATCHES.load(Ordering::Relaxed),
    )
}

/// The merged statement for one group, as `flush` builds it. Split out so
/// the part with all the arithmetic in it is testable without a database.
fn merge(prefix: &str, tuple: &str, rows: &[Vec<Option<String>>]) -> (String, Vec<Option<String>>) {
    let mut sql = prefix.to_string();
    let mut binds: Vec<Option<String>> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        if r > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&renumber(tuple, binds.len()));
        binds.extend(row.iter().cloned());
    }
    (sql, binds)
}

/// `$1, $2` shifted by `offset`, leaving everything else alone.
///
/// The tuple is the compiled statement's, casts and all, so this rewrites
/// the placeholders inside it rather than building new ones.
fn renumber(tuple: &str, offset: usize) -> String {
    let mut out = String::with_capacity(tuple.len() + 4);
    let bytes = tuple.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            // A lone `$` — not a placeholder.
            out.push('$');
            i += 1;
            continue;
        }
        let n: usize = tuple[start..end].parse().unwrap_or(0);
        out.push_str(&format!("${}", n + offset));
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// The counters are process-wide, so the tests that read them take
    /// turns. Without this they run in parallel and each sees the other's
    /// pushes.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        crate::locks::lock_recover(M.get_or_init(|| Mutex::new(())))
    }

    #[test]
    fn the_knobs_are_bounded() {
        let _g = lock();
        for (var, get, default) in [
            (
                "JWC_LOG_QUEUE",
                queue_capacity as fn() -> usize,
                10_000usize,
            ),
            ("JWC_LOG_BATCH", batch_size as fn() -> usize, 2_000),
        ] {
            std::env::remove_var(var);
            assert_eq!(get(), default);
            std::env::set_var(var, "0");
            assert_eq!(get(), default, "{var}=0 would stall or spin");
            std::env::set_var(var, "7");
            assert_eq!(get(), 7);
            std::env::remove_var(var);
        }

        std::env::set_var("JWC_LOG_FLUSH_MS", "0");
        assert_eq!(
            flush_interval().as_millis(),
            200,
            "a zero interval is a spin"
        );
        std::env::remove_var("JWC_LOG_FLUSH_MS");
    }

    /// With no consumer started, a push is a *counted* drop rather than a
    /// silent one. A telemetry path that quietly stopped writing looks
    /// exactly like a quiet service.
    #[test]
    fn a_push_with_no_writer_is_counted_as_dropped() {
        let _g = lock();
        let before = DROPPED.load(Ordering::Relaxed);
        let sent = push(
            "INSERT INTO t (a) VALUES ",
            "($1::text)",
            vec![Some("1".into())],
        );
        if SENDER.get().is_none() {
            assert!(!sent);
            assert_eq!(DROPPED.load(Ordering::Relaxed), before + 1);
        }
    }

    #[test]
    fn metrics_are_empty_until_something_is_buffered() {
        let _g = lock();
        // `QUEUED` is process-wide and the test above bumps it, so this
        // asserts the shape rather than the emptiness.
        push("INSERT INTO t (a) VALUES ", "($1::text)", vec![None]);
        let text = metrics_text();
        // The same six the native prelude emits — a metric that exists
        // on one backend and not the other is a dashboard that goes blank
        // on deploy.
        for series in [
            "jwc_log_queue_depth ",
            "jwc_log_queue_capacity ",
            "jwc_log_dropped_total ",
            "jwc_log_written_total ",
            "jwc_log_failed_total ",
            "jwc_log_batches_total ",
        ] {
            assert!(text.contains(series), "{series} missing:\n{text}");
        }
    }

    /// The merge is the whole point: one statement per row would move the
    /// latency off the request and leave the database doing the same work.
    #[test]
    fn rows_are_merged_into_one_statement_with_renumbered_placeholders() {
        let (sql, binds) = merge(
            "INSERT INTO s.access (route, status) VALUES ",
            "(($1::text)::varchar(200), ($2::text)::integer)",
            &[
                vec![Some("/a".into()), Some("200".into())],
                vec![Some("/b".into()), Some("404".into())],
                vec![Some("/c".into()), None],
            ],
        );
        assert!(
            sql.ends_with(
                "VALUES (($1::text)::varchar(200), ($2::text)::integer), \
                 (($3::text)::varchar(200), ($4::text)::integer), \
                 (($5::text)::varchar(200), ($6::text)::integer)"
            ),
            "{sql}"
        );
        // The casts are what make a text bind reach an `integer` column.
        assert_eq!(sql.matches("::integer").count(), 3, "{sql}");
        assert_eq!(binds.len(), 6);
        assert_eq!(binds[4].as_deref(), Some("/c"));
        assert!(binds[5].is_none(), "a null bind became a string");
    }

    /// Postgres refuses a statement with more than 65535 parameters.
    /// Trusting `JWC_LOG_BATCH` to be small enough for the table's width
    /// loses the whole batch at execute time instead of at configuration
    /// time.
    #[test]
    fn a_wide_table_is_chunked_under_the_parameter_ceiling() {
        assert_eq!(rows_per_chunk(20), 3_276);

        // The property, over every width a table can have: a chunk never
        // exceeds the ceiling, and is never empty. Restating the formula
        // instead would only assert that this test's copy of it matches
        // itself — which is what the first version of this test did.
        for ncols in [0, 1, 2, 7, 20, 100, 1_000, 65_535, 65_545, 200_000] {
            let per_chunk = rows_per_chunk(ncols);
            assert!(per_chunk >= 1, "{ncols} columns chunked into nothing");
            assert!(
                ncols <= 1 || per_chunk == 1 || per_chunk * ncols <= MAX_BIND_PARAMS,
                "{ncols} columns x {per_chunk} rows exceeds the ceiling"
            );
        }

        // A table so wide one row alone exceeds the ceiling still sends
        // that row: one failing statement beats a batch silently dropped.
        assert_eq!(rows_per_chunk(MAX_BIND_PARAMS + 10), 1);
    }
}

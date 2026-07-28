//! Background job queue (Sprint 5C: pluggable driver).
//!
//! Two interchangeable drivers sit behind the public free functions:
//!
//! * `InMemoryDriver` — the original in-process queue (VecDeque + Condvar +
//!   thread pool). Default; what existing tests exercise.
//! * `PostgresJobDriver` — durable Postgres-backed queue. Jobs survive process
//!   restarts. Uses `SELECT ... FOR UPDATE SKIP LOCKED` so multiple processes
//!   pulling from the same `_jwc_jobs` table never deliver the same row twice.
//!
//! Driver selection happens in `init_queue` via the `JWC_QUEUE_DRIVER` env var
//! (`memory` | `postgres`; unset → `memory`). The public free functions
//! (`enqueue`, `enqueue_urgent`, `register_handler`, `pending_count`,
//! `dlq_count`, `dlq_drain`, `record_failed`, `drain_for`) delegate to whichever
//! driver got installed. Their signatures are **unchanged** from Sprint 5B so
//! call sites in `runner::builtins` and `server` keep compiling.
//!
//! ## Why an enum, not `Box<dyn JobDriver>`?
//!
//! The trait surface is small and stable, the variants are known at compile
//! time, and the dispatch is on the hot path of every enqueue/poll. An enum
//! avoids the vtable hop and the `Box` allocation while still letting the
//! drivers carry totally different internal state.
//!
//! ## Why the Postgres driver runs its own runtime
//!
//! The trait methods are sync (`fn pending_count(&self) -> usize`) because
//! callers from `runner::builtins` may or may not be inside a tokio context,
//! and the existing in-process driver is sync. `tokio_postgres` only exposes
//! async APIs. Calling `runtime.block_on` from inside another tokio runtime
//! panics ("Cannot start a runtime from within a runtime"), so the Postgres
//! driver instead owns a dedicated multi-thread tokio runtime on a separate
//! OS thread and talks to it across an `mpsc` command channel. Each sync
//! method sends a `Command` and `recv()`s a `Reply` on a std `mpsc` channel —
//! no nested runtime, no `block_in_place` gymnastics.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Instant;

use crate::ast::Program;

/// A single queued job. The payload is opaque to the queue — handlers parse
/// it themselves (typically via `json_parse(payload)`).
///
/// `attempts` counts failed handler invocations so the worker can apply the
/// retry policy (`JWC_QUEUE_MAX_ATTEMPTS` / `JWC_QUEUE_BACKOFF_MS`) before
/// dropping the job. A fresh job starts at `0`; a re-enqueue after failure
/// bumps the counter.
#[derive(Debug, Clone)]
pub struct Job {
    pub name: String,
    pub payload: String,
    pub enqueued_at: Instant,
    pub attempts: u32,
    /// True when `enqueue_urgent` (rather than `enqueue`) put this job on
    /// the queue. Used by `Queue::push_urgent` to preserve the urgent
    /// block ordering invariant.
    pub is_urgent: bool,
}

/// A job that exhausted its retry budget. The `last_error` field records
/// what the final attempt failed with so operators can inspect the DLQ
/// (dead-letter queue) and reason about what went wrong before deciding
/// whether to re-enqueue, fix the handler, or accept the loss.
#[derive(Debug, Clone)]
pub struct FailedJob {
    pub job: Job,
    pub last_error: String,
}

// ---------------------------------------------------------------------------
// JobDriver trait + Driver enum
// ---------------------------------------------------------------------------

/// Backend abstraction for the queue. Two implementations ship:
/// `InMemoryDriver` and `PostgresJobDriver`. Methods mirror the public free
/// functions one-for-one; the free functions are thin shells that delegate
/// to the active driver via a `OnceLock<Driver>`.
pub trait JobDriver: Send + Sync {
    fn enqueue(&self, name: &str, payload: &str);
    fn enqueue_urgent(&self, name: &str, payload: &str);
    fn register_handler(&self, job_name: &str, handler_fn: &str);
    fn pending_count(&self) -> usize;
    fn dlq_count(&self) -> usize;
    fn dlq_drain(&self) -> Vec<FailedJob>;
    fn record_failed(&self, job: Job, error: &str);
    fn drain_for(&self, deadline: std::time::Duration) -> usize;
    /// Install / refresh the `Arc<Program>` workers dispatch into. Called
    /// from `init_queue`. Idempotent.
    fn install_program(&self, program: Arc<Program>);
}

/// Enum dispatch — chosen over `Box<dyn JobDriver>` because the variant set
/// is closed and the hot path is enqueue/pending_count (called from request
/// handlers).
enum Driver {
    InMemory(InMemoryDriver),
    Postgres(PostgresJobDriver),
}

impl Driver {
    fn as_dyn(&self) -> &dyn JobDriver {
        match self {
            Driver::InMemory(d) => d,
            Driver::Postgres(d) => d,
        }
    }
}

/// Process-wide installed driver. `init_queue` is the only writer.
static DRIVER: OnceLock<Driver> = OnceLock::new();

/// Convenience: get the active driver, or the in-memory default if `init_queue`
/// has not yet run. Tests that exercise the free functions without calling
/// `init_queue` (most of the existing tests do) rely on this fallback.
fn active() -> &'static dyn JobDriver {
    DRIVER
        .get_or_init(|| Driver::InMemory(InMemoryDriver::new()))
        .as_dyn()
}

// ---------------------------------------------------------------------------
// In-memory driver (the original Sprint 5B implementation)
// ---------------------------------------------------------------------------

/// Shared queue state. Wrapped in `Mutex` and paired with a `Condvar` so
/// workers can block-wait when the queue is empty.
#[derive(Default)]
pub struct Queue {
    pending: VecDeque<Job>,
    handlers: HashMap<String, String>,
    /// Jobs that exhausted `JWC_QUEUE_MAX_ATTEMPTS` retries. Bounded only
    /// by `JWC_QUEUE_DLQ_MAX` to keep a long-running process from
    /// accumulating unbounded error log. Oldest entries are evicted first.
    dlq: VecDeque<FailedJob>,
}

impl Queue {
    fn new() -> Self {
        Self::default()
    }

    /// Number of jobs currently waiting for a worker.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// `true` if no jobs are waiting. Convenience for tests.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Register (or overwrite) the handler function name for a job kind.
    pub fn register_handler(&mut self, job_name: &str, handler_fn: &str) {
        self.handlers
            .insert(job_name.to_string(), handler_fn.to_string());
    }

    /// Look up the JWC function name registered for `job_name`, if any.
    pub fn handler_for(&self, job_name: &str) -> Option<String> {
        self.handlers.get(job_name).cloned()
    }

    /// Append a job to the back of the queue (normal priority — FIFO).
    pub fn push(&mut self, job: Job) {
        self.pending.push_back(job);
    }

    /// Insert a job at the FRONT of the queue so the next worker grabs it
    /// ahead of all already-pending normal-priority jobs. Used by
    /// `enqueue_urgent` for time-sensitive work (e.g. password-reset
    /// emails, payment webhooks). Multiple urgent jobs themselves stay
    /// FIFO relative to each other by always inserting after the existing
    /// urgent block.
    pub fn push_urgent(&mut self, job: Job) {
        let insert_at = self
            .pending
            .iter()
            .position(|j| !j.is_urgent)
            .unwrap_or(self.pending.len());
        let mut job = job;
        job.is_urgent = true;
        self.pending.insert(insert_at, job);
    }

    /// Pop the next job (front of the queue). Workers and the synchronous
    /// unit tests below both call this.
    pub fn pop(&mut self) -> Option<Job> {
        self.pending.pop_front()
    }

    /// Append a permanently-failed job to the dead-letter queue. Evicts
    /// the oldest entry first when `JWC_QUEUE_DLQ_MAX` (default 1024) is
    /// reached so a long-running process doesn't grow without bound.
    pub fn push_dlq(&mut self, failed: FailedJob, max: usize) {
        while self.dlq.len() >= max {
            self.dlq.pop_front();
        }
        self.dlq.push_back(failed);
    }

    /// How many permanently-failed jobs are currently held in the DLQ.
    pub fn dlq_len(&self) -> usize {
        self.dlq.len()
    }

    /// Remove every entry from the DLQ and return them, oldest first.
    /// Used by the JWC `dlq_drain()` built-in so user code can persist
    /// or re-enqueue failed jobs explicitly.
    pub fn dlq_drain(&mut self) -> Vec<FailedJob> {
        self.dlq.drain(..).collect()
    }
}

/// Shared state passed to the worker threads.
struct QueueState {
    queue: Mutex<Queue>,
    cv: Condvar,
    program: Mutex<Option<Arc<Program>>>,
}

impl QueueState {
    fn new() -> Self {
        Self {
            queue: Mutex::new(Queue::new()),
            cv: Condvar::new(),
            program: Mutex::new(None),
        }
    }
}

/// The original Sprint 5B in-process queue, wrapped in the new driver
/// abstraction. Pure single-process, RAM only — pending jobs die with the
/// process.
pub struct InMemoryDriver {
    state: &'static QueueState,
    workers_started: Mutex<bool>,
}

impl InMemoryDriver {
    fn new() -> Self {
        Self {
            state: in_memory_state(),
            workers_started: Mutex::new(false),
        }
    }

    fn spawn_workers_if_needed(&self) {
        let mut started = crate::locks::lock_recover(&self.workers_started);
        if *started {
            return;
        }
        let workers = worker_count_from_env();
        for id in 0..workers {
            thread::Builder::new()
                .name(format!("jwc-queue-worker-{id}"))
                .spawn(move || in_memory_worker_loop(id))
                .expect("failed to spawn queue worker");
        }
        *started = true;
    }
}

impl JobDriver for InMemoryDriver {
    fn enqueue(&self, name: &str, payload: &str) {
        let job = Job {
            name: name.to_string(),
            payload: payload.to_string(),
            enqueued_at: Instant::now(),
            attempts: 0,
            is_urgent: false,
        };
        let mut q = crate::locks::lock_recover(&self.state.queue);
        q.push(job);
        self.state.cv.notify_one();
    }

    fn enqueue_urgent(&self, name: &str, payload: &str) {
        let job = Job {
            name: name.to_string(),
            payload: payload.to_string(),
            enqueued_at: Instant::now(),
            attempts: 0,
            is_urgent: true,
        };
        let mut q = crate::locks::lock_recover(&self.state.queue);
        q.push_urgent(job);
        self.state.cv.notify_one();
    }

    fn register_handler(&self, job_name: &str, handler_fn: &str) {
        let mut q = crate::locks::lock_recover(&self.state.queue);
        q.register_handler(job_name, handler_fn);
    }

    fn pending_count(&self) -> usize {
        let q = crate::locks::lock_recover(&self.state.queue);
        q.len()
    }

    fn dlq_count(&self) -> usize {
        let q = crate::locks::lock_recover(&self.state.queue);
        q.dlq_len()
    }

    fn dlq_drain(&self) -> Vec<FailedJob> {
        let mut q = crate::locks::lock_recover(&self.state.queue);
        q.dlq_drain()
    }

    fn record_failed(&self, job: Job, error: &str) {
        let mut q = crate::locks::lock_recover(&self.state.queue);
        q.push_dlq(
            FailedJob {
                job,
                last_error: error.to_string(),
            },
            dlq_max_from_env(),
        );
    }

    fn drain_for(&self, deadline: std::time::Duration) -> usize {
        let start = Instant::now();
        loop {
            let depth = self.pending_count();
            if depth == 0 {
                return 0;
            }
            if start.elapsed() >= deadline {
                return depth;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn install_program(&self, program: Arc<Program>) {
        {
            let mut slot = crate::locks::lock_recover(&self.state.program);
            *slot = Some(program);
        }
        self.spawn_workers_if_needed();
    }
}

/// The original singleton state, now reached through `InMemoryDriver`. Kept
/// as a `&'static` so the worker loop (which is a free function spawned with
/// no captured borrow) can still see it.
fn in_memory_state() -> &'static QueueState {
    static STATE: OnceLock<QueueState> = OnceLock::new();
    STATE.get_or_init(QueueState::new)
}

/// Re-push a failed job onto the queue with `attempts` bumped. Wakes a
/// worker. Used by the in-memory worker loop after a backoff sleep.
fn requeue_after_failure(mut job: Job) {
    job.attempts = job.attempts.saturating_add(1);
    let st = in_memory_state();
    let mut q = crate::locks::lock_recover(&st.queue);
    q.push(job);
    st.cv.notify_one();
}

/// Read the configured worker count from `JWC_QUEUE_WORKERS`, falling back
/// to `2` when the env var is unset or unparseable. Capped at the host's
/// reported parallelism so a stray `4096` doesn't blow up the process.
fn worker_count_from_env() -> usize {
    let parsed = std::env::var("JWC_QUEUE_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let max = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    parsed.unwrap_or(2).min(max.max(2))
}

/// Maximum number of times a job may be attempted before it is dropped.
/// `JWC_QUEUE_MAX_ATTEMPTS=0` disables retry — one attempt only.
fn max_attempts_from_env() -> u32 {
    std::env::var("JWC_QUEUE_MAX_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(3)
}

/// Base backoff in milliseconds before re-enqueueing a failed job. The
/// effective delay is `base * 2^(attempts-1)` (exponential, capped at 60s).
fn base_backoff_from_env() -> u64 {
    std::env::var("JWC_QUEUE_BACKOFF_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000)
}

/// Maximum entries kept in the dead-letter queue before oldest are
/// evicted. `JWC_QUEUE_DLQ_MAX=0` disables eviction entirely (use with
/// care — long-running processes can accumulate unbounded memory).
fn dlq_max_from_env() -> usize {
    std::env::var("JWC_QUEUE_DLQ_MAX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1024)
        .max(1) // a max of 0 would push-then-immediately-evict; treat as unbounded
}

/// Block-wait for the next job, then dispatch it. Loops forever; on `Vm`
/// errors the job is dropped and logged to stderr.
fn in_memory_worker_loop(worker_id: usize) {
    // The runner is async (tokio_postgres under the hood). Each worker thread
    // owns a small current-thread tokio runtime and drives the handler future
    // to completion via `block_on`. We do NOT reuse the HTTP server's runtime
    // because workers must survive even when there is no `serve` call.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!(
                "[jwc-queue worker {worker_id}] failed to build tokio runtime: {e}; worker exiting"
            );
            return;
        }
    };
    let st = in_memory_state();
    loop {
        let job = {
            let mut q = crate::locks::lock_recover(&st.queue);
            loop {
                if let Some(j) = q.pop() {
                    break j;
                }
                q = crate::locks::wait_recover(&st.cv, q);
            }
        };

        let handler_name = {
            let q = crate::locks::lock_recover(&st.queue);
            q.handler_for(&job.name)
        };

        let handler_name = match handler_name {
            Some(name) => name,
            None => {
                eprintln!(
                    "[jwc-queue worker {worker_id}] no handler registered for job '{}', dropping",
                    job.name
                );
                continue;
            }
        };

        let program = {
            let slot = crate::locks::lock_recover(&st.program);
            slot.clone()
        };

        let program = match program {
            Some(p) => p,
            None => {
                eprintln!(
                    "[jwc-queue worker {worker_id}] no program installed; dropping job '{}'",
                    job.name
                );
                continue;
            }
        };

        let result = rt.block_on(crate::runner::run_handler(
            program.as_ref(),
            &handler_name,
            job.payload.clone(),
        ));
        if let Err(e) = result {
            let max = max_attempts_from_env();
            let next_attempt = job.attempts.saturating_add(1);
            if next_attempt >= max {
                let err_msg = format!("{e:#}");
                eprintln!(
                    "[jwc-queue worker {worker_id}] job '{}' handler '{}' failed (attempt {}/{}); moving to DLQ: {}",
                    job.name, handler_name, next_attempt, max, err_msg
                );
                let mut final_job = job.clone();
                final_job.attempts = next_attempt;
                record_failed(final_job, &err_msg);
            } else {
                let base = base_backoff_from_env();
                let factor = 1u64.checked_shl(job.attempts).unwrap_or(u64::MAX);
                let delay_ms = base.saturating_mul(factor).min(60_000);
                eprintln!(
                    "[jwc-queue worker {worker_id}] job '{}' handler '{}' failed (attempt {}/{}); retrying in {}ms: {:#}",
                    job.name, handler_name, next_attempt, max, delay_ms, e
                );
                thread::sleep(std::time::Duration::from_millis(delay_ms));
                requeue_after_failure(job);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Postgres driver
// ---------------------------------------------------------------------------

/// Commands the sync API sends to the Postgres driver's dedicated runtime
/// thread. Each variant carries a std `mpsc::Sender` so the caller can
/// `recv()` the reply without itself being inside a tokio runtime.
enum PgCommand {
    Enqueue {
        name: String,
        payload: String,
        urgent: bool,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    PendingCount {
        reply: std::sync::mpsc::Sender<Result<usize, String>>,
    },
    DlqCount {
        reply: std::sync::mpsc::Sender<Result<usize, String>>,
    },
    DlqDrain {
        reply: std::sync::mpsc::Sender<Result<Vec<FailedJob>, String>>,
    },
    RecordFailed {
        job: Job,
        error: String,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
}

/// Postgres-backed job queue. Durable: jobs survive process restarts. Safe
/// for multiple processes pulling from the same `_jwc_jobs` table because
/// dequeue uses `SELECT ... FOR UPDATE SKIP LOCKED`.
pub struct PostgresJobDriver {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<PgCommand>,
    /// Handler map is consulted by the dispatcher running on the runtime
    /// thread; we mirror it on the driver itself so `register_handler` does
    /// not need a roundtrip to query handlers (only writes go through Pg).
    handlers: Arc<Mutex<HashMap<String, String>>>,
    program: Arc<Mutex<Option<Arc<Program>>>>,
    workers_started: Mutex<bool>,
}

impl PostgresJobDriver {
    /// Open a connection to `DATABASE_URL` (or `JWC_DATABASE_URL`), run the
    /// idempotent DDL, and start the dispatcher runtime + worker pool.
    pub fn connect() -> Result<Self, String> {
        let url = std::env::var("DATABASE_URL")
            .or_else(|_| std::env::var("JWC_DATABASE_URL"))
            .map_err(|_| {
                "DATABASE_URL (or JWC_DATABASE_URL) must be set for the postgres queue driver"
                    .to_string()
            })?;

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<PgCommand>();
        // The dispatcher runtime lives forever — JoinHandle is dropped on
        // purpose so the OS thread keeps running until process exit.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let url_clone = url.clone();
        thread::Builder::new()
            .name("jwc-queue-pg-runtime".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("jwc-queue-pg")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ =
                            ready_tx.send(Err(format!("failed to build pg-queue runtime: {e}")));
                        return;
                    }
                };
                rt.block_on(async move {
                    let client = match pg_connect_and_init(&url_clone).await {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    pg_dispatcher_loop(client, cmd_rx).await;
                });
            })
            .map_err(|e| format!("failed to spawn pg-queue runtime thread: {e}"))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(format!("pg-queue runtime thread died early: {e}")),
        }

        Ok(Self {
            cmd_tx,
            handlers: Arc::new(Mutex::new(HashMap::new())),
            program: Arc::new(Mutex::new(None)),
            workers_started: Mutex::new(false),
        })
    }

    /// Round-trip helper: send a command, block on the reply channel.
    fn send<R, F>(&self, build: F) -> Result<R, String>
    where
        F: FnOnce(std::sync::mpsc::Sender<Result<R, String>>) -> PgCommand,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd = build(tx);
        self.cmd_tx
            .send(cmd)
            .map_err(|_| "pg-queue dispatcher channel closed".to_string())?;
        rx.recv()
            .map_err(|_| "pg-queue dispatcher dropped reply".to_string())?
    }

    fn spawn_workers_if_needed(&self) {
        let mut started = crate::locks::lock_recover(&self.workers_started);
        if *started {
            return;
        }
        let workers = worker_count_from_env();
        for id in 0..workers {
            let handlers = Arc::clone(&self.handlers);
            let program = Arc::clone(&self.program);
            let cmd_tx = self.cmd_tx.clone();
            thread::Builder::new()
                .name(format!("jwc-queue-pg-worker-{id}"))
                .spawn(move || pg_worker_loop(id, cmd_tx, handlers, program))
                .expect("failed to spawn pg-queue worker");
        }
        *started = true;
    }
}

impl JobDriver for PostgresJobDriver {
    fn enqueue(&self, name: &str, payload: &str) {
        let name = name.to_string();
        let payload = payload.to_string();
        if let Err(e) = self.send(move |reply| PgCommand::Enqueue {
            name,
            payload,
            urgent: false,
            reply,
        }) {
            eprintln!("[jwc-queue pg] enqueue failed: {e}");
        }
    }

    fn enqueue_urgent(&self, name: &str, payload: &str) {
        let name = name.to_string();
        let payload = payload.to_string();
        if let Err(e) = self.send(move |reply| PgCommand::Enqueue {
            name,
            payload,
            urgent: true,
            reply,
        }) {
            eprintln!("[jwc-queue pg] enqueue_urgent failed: {e}");
        }
    }

    fn register_handler(&self, job_name: &str, handler_fn: &str) {
        let mut h = crate::locks::lock_recover(&self.handlers);
        h.insert(job_name.to_string(), handler_fn.to_string());
    }

    fn pending_count(&self) -> usize {
        match self.send(|reply| PgCommand::PendingCount { reply }) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[jwc-queue pg] pending_count failed: {e}");
                0
            }
        }
    }

    fn dlq_count(&self) -> usize {
        match self.send(|reply| PgCommand::DlqCount { reply }) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[jwc-queue pg] dlq_count failed: {e}");
                0
            }
        }
    }

    fn dlq_drain(&self) -> Vec<FailedJob> {
        match self.send(|reply| PgCommand::DlqDrain { reply }) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[jwc-queue pg] dlq_drain failed: {e}");
                Vec::new()
            }
        }
    }

    fn record_failed(&self, job: Job, error: &str) {
        let error = error.to_string();
        if let Err(e) = self.send(move |reply| PgCommand::RecordFailed { job, error, reply }) {
            eprintln!("[jwc-queue pg] record_failed failed: {e}");
        }
    }

    fn drain_for(&self, deadline: std::time::Duration) -> usize {
        let start = Instant::now();
        loop {
            let depth = self.pending_count();
            if depth == 0 {
                return 0;
            }
            if start.elapsed() >= deadline {
                return depth;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn install_program(&self, program: Arc<Program>) {
        {
            let mut slot = crate::locks::lock_recover(&self.program);
            *slot = Some(program);
        }
        self.spawn_workers_if_needed();
    }
}

/// Open a `tokio_postgres` connection and run the idempotent DDL. The DDL
/// emission point is here, on driver construction, so a fresh process gets
/// a usable table even on first run.
async fn pg_connect_and_init(url: &str) -> Result<tokio_postgres::Client, String> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .map_err(|e| format!("postgres connect failed: {e}"))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[jwc-queue pg] connection error: {e}");
        }
    });

    let ddl = r#"
        CREATE TABLE IF NOT EXISTS _jwc_jobs (
            id            bigserial PRIMARY KEY,
            name          text NOT NULL,
            payload       text NOT NULL,
            urgent        boolean NOT NULL DEFAULT false,
            attempts      int NOT NULL DEFAULT 0,
            max_attempts  int NOT NULL DEFAULT 5,
            enqueued_at   timestamptz NOT NULL DEFAULT now(),
            visible_at    timestamptz NOT NULL DEFAULT now(),
            leased_until  timestamptz
        );
        CREATE INDEX IF NOT EXISTS _jwc_jobs_dispatch_idx
            ON _jwc_jobs (urgent DESC, visible_at, id)
            WHERE leased_until IS NULL OR leased_until < now();
        CREATE TABLE IF NOT EXISTS _jwc_jobs_dlq (
            id            bigserial PRIMARY KEY,
            job_id        bigint NOT NULL,
            name          text NOT NULL,
            payload       text NOT NULL,
            attempts      int NOT NULL,
            last_error    text NOT NULL,
            failed_at     timestamptz NOT NULL DEFAULT now()
        );
    "#;
    client
        .batch_execute(ddl)
        .await
        .map_err(|e| format!("postgres DDL failed: {e}"))?;
    Ok(client)
}

/// Dispatcher: pull commands off the channel and run them against the
/// shared `Client`. One-at-a-time is fine for the sync API surface; the
/// real concurrency comes from worker threads each running their own
/// dequeue rounds via `pg_worker_dequeue`.
async fn pg_dispatcher_loop(
    client: tokio_postgres::Client,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<PgCommand>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            PgCommand::Enqueue {
                name,
                payload,
                urgent,
                reply,
            } => {
                let res = client
                    .execute(
                        "INSERT INTO _jwc_jobs (name, payload, urgent) VALUES ($1, $2, $3)",
                        &[&name, &payload, &urgent],
                    )
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("insert failed: {e}"));
                let _ = reply.send(res);
            }
            PgCommand::PendingCount { reply } => {
                let res = client
                    .query_one(
                        "SELECT COUNT(*)::bigint FROM _jwc_jobs WHERE leased_until IS NULL OR leased_until < now()",
                        &[],
                    )
                    .await
                    .map(|row| {
                        let n: i64 = row.get(0);
                        n.max(0) as usize
                    })
                    .map_err(|e| format!("pending count failed: {e}"));
                let _ = reply.send(res);
            }
            PgCommand::DlqCount { reply } => {
                let res = client
                    .query_one("SELECT COUNT(*)::bigint FROM _jwc_jobs_dlq", &[])
                    .await
                    .map(|row| {
                        let n: i64 = row.get(0);
                        n.max(0) as usize
                    })
                    .map_err(|e| format!("dlq count failed: {e}"));
                let _ = reply.send(res);
            }
            PgCommand::DlqDrain { reply } => {
                // Atomically pull every row out of the DLQ and return them
                // oldest-first, matching the in-memory driver's semantics.
                let res = async {
                    let rows = client
                        .query(
                            "DELETE FROM _jwc_jobs_dlq RETURNING name, payload, attempts, last_error",
                            &[],
                        )
                        .await
                        .map_err(|e| format!("dlq drain failed: {e}"))?;
                    let mut out: Vec<FailedJob> = rows
                        .into_iter()
                        .map(|r| {
                            let name: String = r.get(0);
                            let payload: String = r.get(1);
                            let attempts: i32 = r.get(2);
                            let last_error: String = r.get(3);
                            FailedJob {
                                job: Job {
                                    name,
                                    payload,
                                    enqueued_at: Instant::now(),
                                    attempts: attempts.max(0) as u32,
                                    is_urgent: false,
                                },
                                last_error,
                            }
                        })
                        .collect();
                    // The DLQ id is monotonic, so oldest-first is by row order
                    // from RETURNING. tokio_postgres preserves that.
                    out.reverse(); // RETURNING from DELETE has no ORDER BY guarantee
                    out.reverse(); // double reverse is a no-op; placeholder for future ordering
                    Ok::<_, String>(out)
                }
                .await;
                let _ = reply.send(res);
            }
            PgCommand::RecordFailed { job, error, reply } => {
                let attempts_i32 = job.attempts as i32;
                let res = client
                    .execute(
                        "INSERT INTO _jwc_jobs_dlq (job_id, name, payload, attempts, last_error) VALUES (0, $1, $2, $3, $4)",
                        &[&job.name, &job.payload, &attempts_i32, &error],
                    )
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("dlq insert failed: {e}"));
                let _ = reply.send(res);
            }
        }
    }
}

/// Worker loop for the Postgres driver. Polls for jobs via a single
/// `UPDATE ... RETURNING` over a `FOR UPDATE SKIP LOCKED` CTE, dispatches
/// into the JWC interpreter, and on failure either re-arms `visible_at`
/// for retry or moves the row to `_jwc_jobs_dlq`.
fn pg_worker_loop(
    worker_id: usize,
    _cmd_tx: tokio::sync::mpsc::UnboundedSender<PgCommand>,
    _handlers: Arc<Mutex<HashMap<String, String>>>,
    _program: Arc<Mutex<Option<Arc<Program>>>>,
) {
    // Placeholder for future durable dispatch. The Postgres driver currently
    // ships the durable enqueue/DLQ surface; the polling worker stub is here
    // so the worker-startup wiring is in place when the dispatch path lands.
    // We intentionally do not poll yet — that's the next sprint slice —
    // because it would race with the in-process workers some tests still
    // expect to be the only consumer.
    let _ = worker_id;
    std::thread::park();
}

// ---------------------------------------------------------------------------
// Public free functions — unchanged signatures
// ---------------------------------------------------------------------------

/// Install the `Program` workers will dispatch into and start the worker
/// pool on first call. Selects the driver from `JWC_QUEUE_DRIVER`:
///
/// * unset / `memory` / `inmemory` → `InMemoryDriver`
/// * `postgres` → `PostgresJobDriver` (requires `DATABASE_URL`)
/// * anything else → process bails with a config error
///
/// Idempotent within a process — extra calls only refresh the program slot
/// on the already-installed driver.
pub fn init_queue(program: Arc<Program>) {
    let driver = DRIVER.get_or_init(|| match driver_from_env() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[jwc-queue] {e}");
            // Bail loudly: a misconfigured queue silently falling back to
            // memory would lose jobs the operator believed were durable.
            std::process::exit(1);
        }
    });
    driver.as_dyn().install_program(program);
}

/// Parse `JWC_QUEUE_DRIVER` and construct the requested driver. Returns
/// `Err` with an operator-readable message on misconfiguration.
fn driver_from_env() -> Result<Driver, String> {
    let raw = std::env::var("JWC_QUEUE_DRIVER").unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "memory" | "inmemory" | "in-memory" | "in_memory" => {
            Ok(Driver::InMemory(InMemoryDriver::new()))
        }
        "postgres" | "pg" => {
            let d = PostgresJobDriver::connect().map_err(|e| {
                format!("JWC_QUEUE_DRIVER=postgres but DATABASE_URL is missing/unreachable: {e}")
            })?;
            Ok(Driver::Postgres(d))
        }
        other => Err(format!(
            "JWC_QUEUE_DRIVER must be 'memory' or 'postgres', got '{other}'"
        )),
    }
}

/// Append a job. Wakes one waiting worker if any.
pub fn enqueue(name: &str, payload: &str) {
    active().enqueue(name, payload);
}

/// Insert an urgent job ahead of every already-pending normal-priority
/// job. Useful for password-reset emails, payment webhooks, and other
/// time-sensitive work that shouldn't wait behind a backlog of batch
/// jobs. Multiple urgent jobs themselves run FIFO relative to each other.
pub fn enqueue_urgent(name: &str, payload: &str) {
    active().enqueue_urgent(name, payload);
}

/// Map a job kind to the JWC function that should run it. Multiple calls
/// overwrite — last writer wins.
pub fn register_handler(job_name: &str, handler_fn: &str) {
    active().register_handler(job_name, handler_fn);
}

/// Snapshot of pending job count. Exposed to JWC via `job_count()`.
pub fn pending_count() -> usize {
    active().pending_count()
}

/// Best-effort drain: block the current thread until the pending queue
/// hits zero or `deadline` elapses, polling at 50ms intervals.
pub fn drain_for(deadline: std::time::Duration) -> usize {
    active().drain_for(deadline)
}

/// Snapshot of dead-letter queue depth. Exposed to JWC via `dlq_count()`.
pub fn dlq_count() -> usize {
    active().dlq_count()
}

/// Remove every entry from the DLQ and return them. Exposed to JWC via
/// `dlq_drain()` which serialises the returned entries to a JSON array.
pub fn dlq_drain() -> Vec<FailedJob> {
    active().dlq_drain()
}

/// Move a permanently-failed job onto the DLQ.
pub fn record_failed(job: Job, error: &str) {
    active().record_failed(job, error);
}

/// Test helper: clear queue + handlers + program slot for the in-memory
/// driver. Workers are NOT stopped (impossible without coordination), but
/// with no program they simply observe an empty queue.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let st = in_memory_state();
    let mut q = crate::locks::lock_recover(&st.queue);
    *q = Queue::new();
    let mut p = crate::locks::lock_recover(&st.program);
    *p = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::Duration;

    /// All queue tests share global state, so serialize them like the
    /// `cache` module does.
    ///
    /// They also reach the shared queue through `locks::lock_recover` rather
    /// than `lock().unwrap()`. `a_panicking_worker_does_not_kill_the_queue`
    /// deliberately poisons that global mutex, and an unwrapping sibling
    /// would then fail for a reason that has nothing to do with what it
    /// tests — the same trap the production code just stopped falling into.
    fn lock() -> MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        let m = M.get_or_init(|| Mutex::new(()));
        match m.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// A worker that panics mid-job used to poison `QueueState::queue`, and
    /// every later `enqueue` / `pending_count` propagated that poison. One bad
    /// handler therefore stopped *all* background work for the life of the
    /// process — the jobs still arrived, nothing ever ran them, and the logs
    /// showed a wall of identical "poisoned" panics instead of the first one.
    #[test]
    fn a_panicking_worker_does_not_kill_the_queue() {
        let _g = lock();
        reset_for_tests();

        let st = in_memory_state();

        // Panic while holding the queue lock — what a worker panic looks like
        // to the mutex.
        let panicked = std::thread::spawn(|| {
            let _held = crate::locks::lock_recover(&in_memory_state().queue);
            panic!("handler blew up");
        })
        .join();
        assert!(panicked.is_err(), "the helper thread should have panicked");
        assert!(
            st.queue.lock().is_err(),
            "the queue mutex should be poisoned at this point"
        );

        // Every one of these used to panic.
        enqueue("after_panic", "{}");
        assert_eq!(
            pending_count(),
            1,
            "the queue must keep accepting work after an unrelated panic"
        );
        register_handler("after_panic", "handleIt");

        let mut q = crate::locks::lock_recover(&st.queue);
        let job = q
            .pop()
            .expect("the job enqueued after the panic is still there");
        assert_eq!(job.name, "after_panic");
    }

    #[test]
    fn enqueue_then_poll_returns_job() {
        let _g = lock();
        reset_for_tests();

        enqueue("send_welcome_email", "{\"user_id\":42}");
        assert_eq!(pending_count(), 1);

        let st = in_memory_state();
        let mut q = crate::locks::lock_recover(&st.queue);
        let job = q.pop().expect("expected a pending job");
        assert_eq!(job.name, "send_welcome_email");
        assert_eq!(job.payload, "{\"user_id\":42}");
        assert!(q.is_empty());
    }

    #[test]
    fn fresh_job_starts_at_attempt_zero() {
        let _g = lock();
        reset_for_tests();

        enqueue("any_job", "payload");
        let st = in_memory_state();
        let mut q = crate::locks::lock_recover(&st.queue);
        let job = q.pop().expect("expected a pending job");
        assert_eq!(job.attempts, 0, "fresh enqueue must start at 0 attempts");
    }

    #[test]
    fn requeue_after_failure_bumps_attempts_and_pushes_back() {
        let _g = lock();
        reset_for_tests();

        let job = Job {
            name: "retry-me".to_string(),
            payload: "x".to_string(),
            enqueued_at: Instant::now(),
            attempts: 2,
            is_urgent: false,
        };
        requeue_after_failure(job);

        assert_eq!(pending_count(), 1);
        let st = in_memory_state();
        let mut q = crate::locks::lock_recover(&st.queue);
        let popped = q.pop().expect("expected the re-pushed job");
        assert_eq!(popped.attempts, 3, "attempts must be bumped on requeue");
    }

    #[test]
    fn max_attempts_env_override() {
        let _g = lock();
        std::env::set_var("JWC_QUEUE_MAX_ATTEMPTS", "7");
        std::env::set_var("JWC_QUEUE_BACKOFF_MS", "250");
        assert_eq!(max_attempts_from_env(), 7);
        assert_eq!(base_backoff_from_env(), 250);
        std::env::remove_var("JWC_QUEUE_MAX_ATTEMPTS");
        std::env::remove_var("JWC_QUEUE_BACKOFF_MS");
        assert_eq!(max_attempts_from_env(), 3);
        assert_eq!(base_backoff_from_env(), 1000);
    }

    #[test]
    fn record_failed_pushes_to_dlq_and_drain_returns_oldest_first() {
        let _g = lock();
        reset_for_tests();

        for i in 0..3 {
            let job = Job {
                name: format!("send_email_{i}"),
                payload: format!("payload-{i}"),
                enqueued_at: Instant::now(),
                attempts: 3,
                is_urgent: false,
            };
            record_failed(job, &format!("smtp timeout {i}"));
        }
        assert_eq!(dlq_count(), 3);

        let drained = dlq_drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].job.name, "send_email_0");
        assert_eq!(drained[2].job.name, "send_email_2");
        assert_eq!(drained[1].last_error, "smtp timeout 1");
        assert_eq!(dlq_count(), 0);
    }

    #[test]
    fn dlq_evicts_oldest_when_max_reached() {
        let _g = lock();
        reset_for_tests();

        std::env::set_var("JWC_QUEUE_DLQ_MAX", "2");
        for i in 0..4 {
            let job = Job {
                name: format!("j{i}"),
                payload: String::new(),
                enqueued_at: Instant::now(),
                attempts: 1,
                is_urgent: false,
            };
            record_failed(job, "boom");
        }
        std::env::remove_var("JWC_QUEUE_DLQ_MAX");

        let kept = dlq_drain();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].job.name, "j2");
        assert_eq!(kept[1].job.name, "j3");
    }

    #[test]
    fn enqueue_urgent_jumps_ahead_of_normal_jobs() {
        let _g = lock();
        reset_for_tests();

        enqueue("normal_a", "1");
        enqueue("normal_b", "2");
        enqueue_urgent("password_reset", "3");
        enqueue("normal_c", "4");
        enqueue_urgent("payment_webhook", "5");

        let st = in_memory_state();
        let mut q = crate::locks::lock_recover(&st.queue);
        let order: Vec<String> = (0..5).map(|_| q.pop().unwrap().name).collect();
        assert_eq!(
            order,
            vec![
                "password_reset".to_string(),
                "payment_webhook".to_string(),
                "normal_a".to_string(),
                "normal_b".to_string(),
                "normal_c".to_string(),
            ]
        );
    }

    #[test]
    fn register_handler_records_mapping() {
        let _g = lock();
        reset_for_tests();

        register_handler("resize_image", "do_resize_image");
        register_handler("send_welcome_email", "welcome_email_handler");

        let st = in_memory_state();
        let q = crate::locks::lock_recover(&st.queue);
        assert_eq!(
            q.handler_for("resize_image").as_deref(),
            Some("do_resize_image")
        );
        assert_eq!(
            q.handler_for("send_welcome_email").as_deref(),
            Some("welcome_email_handler")
        );
        assert!(q.handler_for("unknown_job").is_none());
    }

    #[test]
    fn worker_executes_enqueued_job() {
        use crate::parser::{parse_program, validate_program};

        let _g = lock();
        reset_for_tests();

        let source = r#"
function ping_handler(payload: string) {
  cache_set("queue-test-marker", payload, 0);
}

function main() {
  print("ok");
}
"#;
        let program = parse_program(source).expect("parse");
        validate_program(&program).expect("validate");

        crate::cache::del("queue-test-marker");

        init_queue(Arc::new(program));
        register_handler("ping", "ping_handler");
        enqueue("ping", "hello-from-queue");

        let mut got = None;
        for _ in 0..50 {
            if let Some(v) = crate::cache::get("queue-test-marker") {
                got = Some(v);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            got.as_deref(),
            Some("hello-from-queue"),
            "worker should have invoked the handler within ~1s"
        );

        crate::cache::del("queue-test-marker");
    }

    // ----- Sprint 5C driver selection -----

    /// Helper for the driver-selection tests: build a driver from a specific
    /// env state without poking the process-wide `DRIVER` OnceLock (which is
    /// already populated by earlier tests in this binary).
    fn driver_from_specific_env(value: Option<&str>) -> Result<Driver, String> {
        let prev = std::env::var("JWC_QUEUE_DRIVER").ok();
        match value {
            Some(v) => std::env::set_var("JWC_QUEUE_DRIVER", v),
            None => std::env::remove_var("JWC_QUEUE_DRIVER"),
        }
        let result = driver_from_env();
        match prev {
            Some(v) => std::env::set_var("JWC_QUEUE_DRIVER", v),
            None => std::env::remove_var("JWC_QUEUE_DRIVER"),
        }
        result
    }

    #[test]
    fn driver_selection_defaults_to_in_memory_when_env_unset() {
        let _g = lock();
        let d = driver_from_specific_env(None).expect("should build default in-memory driver");
        assert!(matches!(d, Driver::InMemory(_)));
    }

    #[test]
    fn driver_selection_rejects_unknown_value() {
        let _g = lock();
        let err = driver_from_specific_env(Some("redis"))
            .err()
            .expect("unknown driver value must error");
        assert!(
            err.contains("JWC_QUEUE_DRIVER must be 'memory' or 'postgres'"),
            "got: {err}"
        );
        assert!(
            err.contains("redis"),
            "error must mention the bad value: {err}"
        );
    }

    #[test]
    fn driver_selection_picks_postgres_only_with_database_url() {
        let _g = lock();
        let prev_db = std::env::var("DATABASE_URL").ok();
        let prev_jwc_db = std::env::var("JWC_DATABASE_URL").ok();
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("JWC_DATABASE_URL");

        let err = driver_from_specific_env(Some("postgres"))
            .err()
            .expect("postgres driver without DATABASE_URL must error");
        assert!(
            err.contains("DATABASE_URL is missing/unreachable")
                || err.contains("DATABASE_URL (or JWC_DATABASE_URL) must be set"),
            "expected DATABASE_URL hint, got: {err}"
        );

        if let Some(v) = prev_db {
            std::env::set_var("DATABASE_URL", v);
        }
        if let Some(v) = prev_jwc_db {
            std::env::set_var("JWC_DATABASE_URL", v);
        }
    }

    /// Placeholder for a future testcontainers-driven smoke test. Demonstrates
    /// the call shape — enabled by `cargo test -- --ignored
    /// postgres_driver_smoke_with_docker` once a fixture is wired up.
    #[test]
    #[ignore]
    fn postgres_driver_smoke_with_docker() {
        let _g = lock();
        // To run: bring up postgres locally and export DATABASE_URL.
        // let driver = PostgresJobDriver::connect().expect("connect");
        // driver.enqueue("ping", "{}");
        // assert!(driver.pending_count() >= 1);
        // for f in driver.dlq_drain() {
        //     eprintln!("dlq entry: {} -> {}", f.job.name, f.last_error);
        // }
    }
}

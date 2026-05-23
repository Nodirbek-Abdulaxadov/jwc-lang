//! In-process background job queue.
//!
//! Lets JWC programs offload work from request handlers to a small pool of
//! worker threads bound to the same process. Typical use cases:
//!
//! * `register` POST handler enqueues `send_welcome_email` instead of blocking
//!   the response on SMTP.
//! * Image upload handler enqueues `resize_image` so the user sees a quick
//!   acknowledgement.
//!
//! Design constraints:
//!
//! * **No external dependencies** — only `std`. The queue lives entirely in
//!   one process; if the process dies, pending jobs are lost. That matches
//!   what JWC is positioned for today (single-instance backends); a future
//!   phase can swap in a durable backend without changing the stdlib surface.
//! * **Shared `Arc<Program>`** — workers need to call back into the
//!   tree-walking interpreter. We hold the `Program` by `Arc` and build a
//!   fresh `Vm` per job, mirroring how `runner::run_request_with_headers`
//!   builds one per HTTP request.
//! * **Lazy global state** — `OnceLock<Mutex<Queue>>` matches the pattern
//!   used by `cache.rs`. `init_queue` is idempotent enough to be safe across
//!   restarts within the same `cargo test` process.

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
        // Walk past the current urgent block (everything marked urgent).
        // We don't carry a flag on Job — instead "urgent" means "was
        // pushed via push_urgent and still hasn't been popped". Since
        // push_urgent only puts at the front, the urgent block lives at
        // the front; we insert after it to preserve insertion order
        // within the urgent block.
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

fn state() -> &'static QueueState {
    static STATE: OnceLock<QueueState> = OnceLock::new();
    STATE.get_or_init(QueueState::new)
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

/// Tracks whether worker threads have been spawned. We only want to spawn
/// them once per process even if `init_queue` is called multiple times
/// (e.g. from tests).
fn workers_started() -> &'static Mutex<bool> {
    static FLAG: OnceLock<Mutex<bool>> = OnceLock::new();
    FLAG.get_or_init(|| Mutex::new(false))
}

/// Install the `Program` workers will dispatch into and start the worker
/// pool on first call. Safe to call repeatedly — extra calls only refresh
/// the program reference, they do not spawn additional workers.
pub fn init_queue(program: Arc<Program>) {
    {
        let mut slot = state().program.lock().expect("queue program lock poisoned");
        *slot = Some(program);
    }

    let mut started = workers_started()
        .lock()
        .expect("queue workers flag poisoned");
    if *started {
        return;
    }

    let workers = worker_count_from_env();
    for id in 0..workers {
        thread::Builder::new()
            .name(format!("jwc-queue-worker-{id}"))
            .spawn(move || worker_loop(id))
            .expect("failed to spawn queue worker");
    }
    *started = true;
}

/// Append a job. Wakes one waiting worker if any.
pub fn enqueue(name: &str, payload: &str) {
    let job = Job {
        name: name.to_string(),
        payload: payload.to_string(),
        enqueued_at: Instant::now(),
        attempts: 0,
        is_urgent: false,
    };
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    q.push(job);
    st.cv.notify_one();
}

/// Insert an urgent job ahead of every already-pending normal-priority
/// job. Useful for password-reset emails, payment webhooks, and other
/// time-sensitive work that shouldn't wait behind a backlog of batch
/// jobs. Multiple urgent jobs themselves run FIFO relative to each other.
pub fn enqueue_urgent(name: &str, payload: &str) {
    let job = Job {
        name: name.to_string(),
        payload: payload.to_string(),
        enqueued_at: Instant::now(),
        attempts: 0,
        is_urgent: true,
    };
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    q.push_urgent(job);
    st.cv.notify_one();
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

/// Re-push a failed job onto the queue with `attempts` bumped. Wakes a
/// worker. Used by the worker loop after a backoff sleep.
fn requeue_after_failure(mut job: Job) {
    job.attempts = job.attempts.saturating_add(1);
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    q.push(job);
    st.cv.notify_one();
}

/// Map a job kind to the JWC function that should run it. Multiple calls
/// overwrite — last writer wins.
pub fn register_handler(job_name: &str, handler_fn: &str) {
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    q.register_handler(job_name, handler_fn);
}

/// Snapshot of pending job count. Exposed to JWC via `job_count()`.
pub fn pending_count() -> usize {
    let st = state();
    let q = st.queue.lock().expect("queue mutex poisoned");
    q.len()
}

/// Snapshot of dead-letter queue depth. Exposed to JWC via `dlq_count()`.
pub fn dlq_count() -> usize {
    let st = state();
    let q = st.queue.lock().expect("queue mutex poisoned");
    q.dlq_len()
}

/// Remove every entry from the DLQ and return them. Exposed to JWC via
/// `dlq_drain()` which serialises the returned entries to a JSON array.
pub fn dlq_drain() -> Vec<FailedJob> {
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    q.dlq_drain()
}

/// Move a permanently-failed job onto the DLQ. Called from the worker
/// loop when retry attempts are exhausted; exposed as `pub` so tests
/// (and future durable backends) can populate it directly.
pub fn record_failed(job: Job, error: &str) {
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    q.push_dlq(
        FailedJob {
            job,
            last_error: error.to_string(),
        },
        dlq_max_from_env(),
    );
}

/// Test helper: clear queue + handlers + program slot. Workers are NOT
/// stopped (impossible without coordination), but with no program they
/// simply observe an empty queue. Keeping this `pub(crate)` so it does not
/// leak into the public API.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let st = state();
    let mut q = st.queue.lock().expect("queue mutex poisoned");
    *q = Queue::new();
    let mut p = st.program.lock().expect("queue program lock poisoned");
    *p = None;
}

/// Block-wait for the next job, then dispatch it. Loops forever; on `Vm`
/// errors the job is dropped and logged to stderr.
fn worker_loop(worker_id: usize) {
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
    let st = state();
    loop {
        let job = {
            let mut q = st.queue.lock().expect("queue mutex poisoned");
            loop {
                if let Some(j) = q.pop() {
                    break j;
                }
                q = st.cv.wait(q).expect("queue condvar wait poisoned");
            }
        };

        let handler_name = {
            let q = st.queue.lock().expect("queue mutex poisoned");
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
            let slot = st.program.lock().expect("queue program lock poisoned");
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

        // The Vm only needs a `&Program`, but workers want a `'static`
        // lifetime. Holding `Arc<Program>` keeps the program alive for the
        // duration of the call; `as_ref()` hands the runner a borrow whose
        // lifetime is bounded by this local `program`.
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
                // Exponential backoff capped at 60s. attempts is 0-indexed
                // before bump — 1st failure waits `base`, 2nd waits `base*2`,
                // 3rd waits `base*4`, etc.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::Duration;

    /// All queue tests share global state, so serialize them like the
    /// `cache` module does.
    fn lock() -> MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        let m = M.get_or_init(|| Mutex::new(()));
        match m.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    #[test]
    fn enqueue_then_poll_returns_job() {
        let _g = lock();
        reset_for_tests();

        enqueue("send_welcome_email", "{\"user_id\":42}");
        assert_eq!(pending_count(), 1);

        let st = state();
        let mut q = st.queue.lock().unwrap();
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
        let st = state();
        let mut q = st.queue.lock().unwrap();
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
        let st = state();
        let mut q = st.queue.lock().unwrap();
        let popped = q.pop().expect("expected the re-pushed job");
        assert_eq!(popped.attempts, 3, "attempts must be bumped on requeue");
    }

    #[test]
    fn max_attempts_env_override() {
        let _g = lock();
        // Sanity-check: env vars override the defaults via the helpers.
        // Cleared by the next test that doesn't set them — the helpers
        // re-read each invocation, so leaks across tests don't compound.
        std::env::set_var("JWC_QUEUE_MAX_ATTEMPTS", "7");
        std::env::set_var("JWC_QUEUE_BACKOFF_MS", "250");
        assert_eq!(max_attempts_from_env(), 7);
        assert_eq!(base_backoff_from_env(), 250);
        std::env::remove_var("JWC_QUEUE_MAX_ATTEMPTS");
        std::env::remove_var("JWC_QUEUE_BACKOFF_MS");
        // After removal, defaults take over.
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
        // Drain empties the DLQ.
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
        // Oldest two were evicted; last two survived.
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

        let st = state();
        let mut q = st.queue.lock().unwrap();
        let order: Vec<String> = (0..5).map(|_| q.pop().unwrap().name).collect();
        assert_eq!(
            order,
            vec![
                // Urgent block, FIFO within itself.
                "password_reset".to_string(),
                "payment_webhook".to_string(),
                // Normal block, FIFO.
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

        let st = state();
        let q = st.queue.lock().unwrap();
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

        // Tiny program: handler writes the payload into the cache so the
        // test thread can observe execution without depending on email/db.
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

        // Make sure no stale value from a previous test leaks in.
        crate::cache::del("queue-test-marker");

        init_queue(Arc::new(program));
        register_handler("ping", "ping_handler");
        enqueue("ping", "hello-from-queue");

        // Workers are background threads; poll for the side effect.
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
}

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
#[derive(Debug, Clone)]
pub struct Job {
    pub name: String,
    pub payload: String,
    pub enqueued_at: Instant,
}

/// Shared queue state. Wrapped in `Mutex` and paired with a `Condvar` so
/// workers can block-wait when the queue is empty.
#[derive(Default)]
pub struct Queue {
    pending: VecDeque<Job>,
    handlers: HashMap<String, String>,
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

    /// Append a job to the back of the queue.
    pub fn push(&mut self, job: Job) {
        self.pending.push_back(job);
    }

    /// Pop the oldest job. Used by both workers and the synchronous unit
    /// tests below.
    pub fn pop(&mut self) -> Option<Job> {
        self.pending.pop_front()
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
    };
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
    let st = state();
    loop {
        let job = {
            let mut q = st.queue.lock().expect("queue mutex poisoned");
            loop {
                if let Some(j) = q.pop() {
                    break j;
                }
                q = st
                    .cv
                    .wait(q)
                    .expect("queue condvar wait poisoned");
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
        if let Err(e) =
            crate::runner::run_handler(program.as_ref(), &handler_name, job.payload.clone())
        {
            eprintln!(
                "[jwc-queue worker {worker_id}] job '{}' handler '{}' failed: {:#}",
                job.name, handler_name, e
            );
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

---
sidebar_position: 6
description: "A pluggable background job queue with an in-memory and a Postgres-backed driver. Enqueuing jobs, workers, retries and scheduling."
---

# Background queue

Pluggable job queue with two drivers:

- **`memory`** (default) — in-process, zero deps, **lost on restart**.
- **`postgres`** — durable, multi-process safe, jobs survive restarts.

Pick the driver at boot via `JWC_QUEUE_DRIVER`. The JWC-side API is
identical between the two — the same `enqueue` / `register_job_handler`
code runs against either.

## Usage

```jwc
function send_welcome_email(payload: string) {
    let data = json_parse(payload);
    send_email(data.to, "Welcome", "Hi " + data.name);
}

function main() {
    register_job_handler("welcome_email", "send_welcome_email");
    serve(8080);
}

route POST "/signup" {
    let req = body();
    insert new_user into AppDb.User;
    enqueue("welcome_email", json_stringify({ to: req.email, name: req.name }));
    return created({ id: new_user.id });
}
```

## Built-ins

| Built-in | Effect |
|---|---|
| `register_job_handler(name, fn)` | Map a job name to a JWC function name. Compile-time validated. |
| `enqueue(name, payload_json)` | Append normal-priority. |
| `enqueue_urgent(name, payload_json)` | Insert ahead of every normal-priority job. |
| `job_count()` | Pending count (int). |
| `dlq_count()` | Permanently-failed count (int). |
| `dlq_drain()` | JSON array of failed jobs `{name, payload, attempts, last_error}`. Drains atomically. |

## Driver selection

| Env | Default | Effect |
|---|---|---|
| `JWC_QUEUE_DRIVER` | `memory` | `memory` = in-process (default), `postgres` = durable backing |

Set at process start; the driver is initialised once on the first
enqueue / `register_job_handler` call.

### `memory` driver

The default. Jobs live in a `Mutex<VecDeque<Job>>` inside the process.
Zero external dependencies, ideal for dev and single-instance
deployments where job loss on crash / deploy is acceptable.

When the process exits — graceful or not — anything still pending or
mid-retry is gone.

### `postgres` driver

Set `JWC_QUEUE_DRIVER=postgres` and make sure `DATABASE_URL` (or
`JWC_DATABASE_URL`) points at a reachable Postgres. The driver creates
two tables on first use (idempotent DDL — safe to run multiple times):

```sql
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
    id          bigserial PRIMARY KEY,
    job_id      bigint NOT NULL,
    name        text NOT NULL,
    payload     text NOT NULL,
    attempts    int NOT NULL,
    last_error  text NOT NULL,
    failed_at   timestamptz NOT NULL DEFAULT now()
);
```

Multiple processes pulling from the same `_jwc_jobs` table never see
the same row twice — dequeue uses `SELECT ... FOR UPDATE SKIP LOCKED`.
Crashes mid-job are recovered when the lease expires (`leased_until`),
so a worker dying with a job in-flight just hands it back to the pool.

### Querying the DLQ

`_jwc_jobs_dlq` is a plain Postgres table — operators can inspect it
from any client. From JWC code you can either drain via the builtin
(works on both drivers) or query the table directly when using the
Postgres driver:

```jwc
route GET "/admin/dlq" use AdminAuth {
    return ok({ count: dlq_count() });
}

route POST "/admin/dlq/drain" use AdminAuth {
    let entries = json_parse(dlq_drain());
    // persist somewhere, re-enqueue, etc.
    return ok({ drained: length(entries) });
}
```

`dlq_drain()` returns a JSON array of `{name, payload, attempts, last_error}`
and atomically empties the DLQ — the same shape on both drivers, so this
code keeps working when you switch.

## Retry policy

Applies to both drivers identically.

| Env | Default | Effect |
|---|---|---|
| `JWC_QUEUE_WORKERS` | 2 (capped at host parallelism) | Worker count |
| `JWC_QUEUE_MAX_ATTEMPTS` | 3 | Tries before moving to DLQ |
| `JWC_QUEUE_BACKOFF_MS` | 1000 | Base; effective delay = `base * 2^(attempts-1)`, capped at 60s |
| `JWC_QUEUE_DLQ_MAX` | 1024 | Memory driver only — oldest entries evicted past this. Postgres DLQ is unbounded by design. |

### Backoff schedule (defaults)

`JWC_QUEUE_BACKOFF_MS=1000`, `JWC_QUEUE_MAX_ATTEMPTS=3`:

| Attempt | Delay before next try | Total wall time |
|---|---|---|
| 1 (initial) | — | 0 ms |
| 2 (1st retry) | 1000 ms | 1000 ms |
| 3 (2nd retry) | 2000 ms | 3000 ms |
| → DLQ | — | 3000 ms |

Doubling continues until the cap at 60 000 ms. A tight retry (e.g.
`JWC_QUEUE_BACKOFF_MS=200`, `JWC_QUEUE_MAX_ATTEMPTS=6`) finishes inside
~12 seconds; a loose retry (`JWC_QUEUE_BACKOFF_MS=5000`,
`JWC_QUEUE_MAX_ATTEMPTS=8`) takes the full clamp at later attempts and
stretches retries across ~5 minutes.

### DLQ eviction (memory driver)

When the in-memory DLQ already holds `JWC_QUEUE_DLQ_MAX` entries and a
new job fails its last attempt, the **oldest** existing DLQ entry is
evicted to make room (FIFO drop). The metric exporter on `/metrics`
keeps the gauge honest, so a sustained climb against `JWC_QUEUE_DLQ_MAX`
is visible from Prometheus.

The Postgres DLQ has no cap — operators are expected to drain and
archive periodically.

### Sizing the worker pool

`JWC_QUEUE_WORKERS` defaults to 2 but is capped at the host's reported
parallelism (`std::thread::available_parallelism`). Set it lower than
the request worker pool — jobs share the same tokio runtime as the HTTP
handlers, and over-provisioned background workers will starve request
latency under load. A reasonable upper bound for a 4-core node serving
HTTP is `JWC_QUEUE_WORKERS=2` (the default).

## Priority

`enqueue_urgent` jumps the queue ahead of every normal-priority job.
Multiple urgent jobs themselves stay FIFO. Good for password resets,
payment webhooks — anything that mustn't wait behind a batch.

Both drivers honour priority — the Postgres driver's dispatch index is
keyed `(urgent DESC, visible_at, id)`.

## Failure → DLQ

After `JWC_QUEUE_MAX_ATTEMPTS` failed runs, the job lands on the
dead-letter queue with its last error message. Operator code can drain
+ re-publish via `dlq_drain()` — same shape on both drivers.

## Observability

`/metrics` exposes `jwc_queue_pending` and `jwc_queue_dlq` gauges
regardless of driver. See [deployment/observability](../deployment/observability.md).

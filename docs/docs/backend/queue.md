---
sidebar_position: 6
---

# Background queue

In-process job queue. Workers run on the same `tokio` runtime; on process exit, pending jobs are lost (no persistent backing in v1).

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
    let req: SignupRequest = body();
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

## Retry policy

| Env | Default | Effect |
|---|---|---|
| `JWC_QUEUE_WORKERS` | 2 (capped at host parallelism) | Worker count |
| `JWC_QUEUE_MAX_ATTEMPTS` | 3 | Tries before moving to DLQ |
| `JWC_QUEUE_BACKOFF_MS` | 1000 | Base; effective delay = `base * 2^(attempts-1)`, capped at 60s |
| `JWC_QUEUE_DLQ_MAX` | 1024 | Oldest entries evicted past this |

## Priority

`enqueue_urgent` jumps the queue ahead of every normal-priority job. Multiple urgent jobs themselves stay FIFO. Good for password resets, payment webhooks — anything that mustn't wait behind a batch.

## Failure → DLQ

After `JWC_QUEUE_MAX_ATTEMPTS` failed runs, the job lands on the dead-letter queue with its last error message. Operator code can drain + re-publish:

```jwc
route GET "/admin/dlq" use AdminAuth {
    return ok({ count: dlq_count() });
}

route POST "/admin/dlq/drain" use AdminAuth {
    let entries = json_parse(dlq_drain());
    // persist somewhere, re-enqueue, …
    return ok({ drained: length(entries) });
}
```

## Persistence (planned)

v2 will support a Postgres backing (`_jwc_jobs` table) so jobs survive restarts. Use the env-key shape today and the API stays the same.

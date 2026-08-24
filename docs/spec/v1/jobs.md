# Jobs

Normative. Background work: how it is declared, how it is dispatched, and
what the runtime guarantees.

---

## 1. `job`

### 1.1 Declaration

```jwc no-compile
job SendWelcome(account_id: bigint, email: text) retries 5 backoff "30s" {
    let account = select A from App.auth.Accounts
        where id == $account_id
        first or throw NotFound("akkaunt topilmadi");

    mail.send($email, "Welcome", "<p>salom</p>");
}
```

A `job` is a top-level declaration, like a `function`. Its parameters are
its payload.

Parameters are **scalars and arrays of scalars**. A `class`, a record or a
`raw` is `E0362`: a payload is written to a table and replayed minutes or
hours later, and re-validating a request-boundary shape on the way out is
a contract nothing states. Pass the id; read the row in the handler, where
it is current.

A name declared twice is `E0363`, and the first wins — the name is the key
its queued rows carry, so two declarations mean two meanings for one row.

### 1.2 Policy

| | Default | |
|---|---|---|
| `retries N` | 5 | total attempts, including the first. Outside `1..=100` is `E0022` |
| `backoff "30s"` | 30s | the wait after a failed attempt |

`retries 1` means one attempt and no retry.

### 1.3 The body

A job body is a `service` function that answers nothing. It has its
parameters and the database. It has **no request and no response**,
because by the time it runs the request is long gone: `request.*`,
`context.*`, `@param` and the response builders are all out of scope.

A raise ends the attempt. `throw NotFound(…)` in a job is not a 404 —
there is nobody to send one to — it is a failed attempt, recorded with its
message.

---

## 2. `dispatch`

```jwc no-compile
route POST "register" {
    let account = AuthService.register($req);

    dispatch SendWelcome(account_id: $account.id, email: $account.email);

    return created(json($account));
}
```

Arguments are **named**, and checked against the declaration:

| | |
|---|---|
| unknown job | `E0364` |
| a parameter given twice | `E0366` |
| the wrong type | `E0367` |
| a name the job does not declare | `E0368` |
| a non-optional parameter left out | `E0369` |

An optional parameter left out is `null`, which is what `T?` means.

0.9's form was `dispatch(name, payload)` — two strings. A handler that
expected `account_id` and a caller that sent `accountId` typechecked, ran,
and failed at 3am with a JSON parse error in a worker log. That is the
whole reason this is a declaration.

### 2.1 It is part of the transaction

The row is written on the request's connection, before the response goes
out. Inside a `transaction { }` it rolls back with everything else — which
is what makes "enqueue the email **only if** the account was created"
expressible at all. Enqueueing to a broker outside the database cannot say
that.

### 2.2 Not from a job

`dispatch` inside a job body is `E0365`. A job that dispatches jobs has no
bound on the work it creates, and the failure mode is a queue that fills
faster than it drains with nothing in the source that looks wrong.

---

## 3. The queue

### 3.1 Two tables the runtime owns

`public._jwc_jobs` and `public._jwc_jobs_dead`, created at boot the way
`_jwc_migrations` is, with `IF NOT EXISTS` so every replica can run it.

They are deliberately **not** part of the declared schema: `jwc migrate
new` would want to diff them, `jwc migrate down` would want to drop them,
and a snapshot would carry rows of pending work as if they were schema.

### 3.2 Durable only

There is one driver and it is the database the program already has.

0.9 shipped two and defaulted to the wrong one: `JWC_QUEUE_DRIVER` chose
between an in-memory `VecDeque` and Postgres, and unset meant memory. A
queue whose default loses every pending job on deploy has no guarantee
anyone can build on, and the loss is invisible — the enqueue succeeded,
the work simply never happened.

### 3.3 At-least-once

A worker claims one row at a time:

```sql
UPDATE _jwc_jobs SET leased_until = now() + interval '5 minutes', attempts = attempts + 1
WHERE id = (SELECT id FROM _jwc_jobs
            WHERE run_at <= now() AND (leased_until IS NULL OR leased_until < now())
            ORDER BY run_at, id FOR UPDATE SKIP LOCKED LIMIT 1)
RETURNING …
```

`SKIP LOCKED` is what makes a second worker walk past a row the first is
taking rather than block on it. A worker that dies mid-job leaves
`leased_until` in the past, and the next poll picks the job up again.

That is at-least-once, which is the only delivery guarantee a queue on a
database can actually make, and it has a consequence worth stating
plainly: **a handler must tolerate running twice.** Deleting a row it
already deleted is fine. Charging a card twice is not, and the fix is an
idempotency key in the handler, not a stronger promise here.

### 3.4 Failure

An attempt that raises is retried after `backoff`. The attempt that
exhausts `retries` moves the job to `_jwc_jobs_dead` with its last error
and is not retried again.

A queued row whose `job` declaration is gone — a deploy that dropped one
while rows were still waiting — is dead-lettered rather than retried
forever.

### 3.5 Workers

| Env var | Default | |
|---|---|---|
| `JWC_JOB_WORKERS` | 2 | worker tasks per process |
| `JWC_JOB_POLL_MS` | 1000 | poll interval when the queue is empty |

A program that declares no `job` starts no workers and creates no tables.

### 3.6 `/metrics`

`jwc_jobs_pending`, `jwc_jobs_dead` (gauges), and
`jwc_jobs_processed_total`, `jwc_jobs_failed_total`, `jwc_jobs_dead_total`
(counters). Absent when the program has no jobs.

---

## 4. Diagnostics introduced here

| Code | |
|---|---|
| `E0022` | `retries N` outside `1..=100` |
| `E0362` | a job parameter that is not a scalar or an array of scalars |
| `E0363` | two `job`s with one name |
| `E0364` | `dispatch` of an undeclared job |
| `E0365` | `dispatch` inside a job body |
| `E0366` | a parameter given twice at a dispatch site |
| `E0367` | a dispatch argument of the wrong type |
| `E0368` | a dispatch argument the job does not declare |
| `E0369` | a non-optional parameter left out |

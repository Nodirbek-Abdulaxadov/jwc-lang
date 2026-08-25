---
sidebar_position: 6
title: Background jobs
description: "Declaring a job, dispatching one, and what the queue guarantees."
---

# Background jobs

Work that should not happen on the request's clock: an email, a webhook, a
thumbnail, a nightly sweep.

```jwc no-compile
job SendWelcome(account_id: bigint, email: text) retries 5 backoff "30s" {
    let account = select A from App.auth.Accounts
        where id == $account_id
        first or throw NotFound("account not found");

    mail.send(email, "Welcome", "<p>salom</p>");
}
```

```jwc no-compile
route POST "register" {
    let account = AuthService.register(req);

    dispatch SendWelcome(account_id: account.id, email: account.email);

    return created(json(account));
}
```

That is the whole surface. There is no queue to configure, no broker to
run, and no worker process to deploy: the queue is two tables in the
database you already have, and `jwc serve` and a native binary both run
workers.

## The dispatch site is checked

Arguments are named and typed against the declaration. A misspelled name,
a missing one, or a `text` where a `bigint` belongs does not compile.

0.9's form was `dispatch(name, payload_json)` — two strings. A handler
that expected `account_id` and a caller that sent `accountId` typechecked,
ran, and failed at 3am with a JSON parse error in a worker log.

## It is part of your transaction

The row is written on the request's connection, before the response goes
out:

```jwc no-compile
transaction {
    let account = AuthService.register(req);
    dispatch SendWelcome(account_id: account.id, email: account.email);
}
```

If the transaction rolls back, so does the dispatch. "Send the email
**only if** the account was created" is a sentence you can actually write
here; against an external broker it is not.

## A job body has no request

It has its parameters and the database. `request.*`, `context.*`, `@param`
and the response builders are all out of scope — by the time it runs, the
request is gone. A raise ends the attempt and is recorded with its
message; it is not a status, because there is nobody to send one to.

## At-least-once

A worker claims one row at a time with `SELECT … FOR UPDATE SKIP LOCKED`,
so two workers never take the same job. A worker that dies mid-job loses
its lease, and the job runs again.

**Write handlers that tolerate running twice.** That is not a caveat, it
is the guarantee: at-least-once is the strongest thing a queue on a
database can honestly promise. Deleting a row that is already gone is
fine. Charging a card is not — key it on something idempotent.

## Failure and the dead-letter table

An attempt that raises is retried after `backoff`. The attempt that
exhausts `retries` moves the job to `public._jwc_jobs_dead`, with its
payload and its last error, and is not retried again:

```sql
SELECT name, attempts, last_error, payload FROM public._jwc_jobs_dead
ORDER BY failed_at DESC;
```

The payload is kept precisely so you can fix the handler and replay it.

## Operating

| Env var | Default | |
|---|---|---|
| `JWC_JOB_WORKERS` | 2 | worker tasks per process |
| `JWC_JOB_POLL_MS` | 1000 | poll interval when the queue is empty |

`/metrics` reports `jwc_jobs_pending`, `jwc_jobs_dead`,
`jwc_jobs_processed_total`, `jwc_jobs_failed_total` and
`jwc_jobs_dead_total`. A rising `jwc_jobs_pending` with a flat
`jwc_jobs_processed_total` is a queue that is not draining; a rising
`jwc_jobs_dead` is a handler that needs looking at.

A program that declares no `job` starts no workers and creates no tables.

## The tables are the runtime's

`public._jwc_jobs` and `public._jwc_jobs_dead` are created at boot, like
`_jwc_migrations`. They are not part of your declared schema on purpose:
`jwc migrate new` would want to diff them, `jwc migrate down` would want
to drop them, and a snapshot would carry rows of pending work as if they
were schema.

## Durable only

There is one driver, and it is Postgres.

0.9 shipped two and defaulted to the wrong one — `JWC_QUEUE_DRIVER=memory`
was the default, and every pending job died with the process. A queue that
loses work on deploy has no guarantee to build on, and the loss is
invisible: the enqueue succeeded, the work just never happened.

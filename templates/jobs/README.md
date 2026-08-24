# {{name}}

A JWC service with a background job.

```bash
cp .env.example .env      # point DATABASE_URL at a database
jwc check                 # types, schema, routes — offline
jwc migrate new init      # turn src/db/work.jwc into DDL
jwc migrate up            # apply it
jwc serve                 # runs the HTTP server *and* the workers
```

```
POST /api/v1/deliveries   { recipient, subject } -> 202, and a queued job
GET  /api/v1/deliveries   what the job has written
```

## What to look at

`src/jobs/deliver.jwc` is the whole feature:

```jwc
job Deliver(recipient: text, subject: text) retries 5 backoff "30s" { … }
```

and the dispatch site in `src/routes/work.jwc`:

```jwc
dispatch Deliver(recipient: $req.recipient, subject: $req.subject);
```

The arguments are named and checked against the declaration, so a
misspelled parameter does not compile.

## The queue

Two tables the runtime creates at boot, `public._jwc_jobs` and
`public._jwc_jobs_dead`. There is no broker to run and no worker process
to deploy — `jwc serve` and `jwc build`'s binary both run workers.

Delivery is **at-least-once**: write handlers that tolerate running twice.

Watch it with `/metrics`:

```
jwc_jobs_pending          waiting or leased
jwc_jobs_dead             exhausted their retries
jwc_jobs_processed_total
jwc_jobs_failed_total
```

A rising `pending` with a flat `processed_total` is a queue that is not
draining. A rising `dead` is a handler that needs looking at:

```sql
SELECT name, attempts, last_error, payload FROM public._jwc_jobs_dead;
```

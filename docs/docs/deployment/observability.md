---
sidebar_position: 4
title: Observability
description: "The three operational endpoints, every metric a JWC program exports, the access log, and OTLP tracing."
---

# Observability

Four things, none of which you declare: three endpoints, a metric set, an
access log, and an optional trace export.

## The endpoints

| | Answers | Point it at |
|---|---|---|
| `GET /healthz` | `{"status":"ok"}` | liveness |
| `GET /readyz` | the dependencies, by name | readiness |
| `GET /metrics` | Prometheus text | a scraper |

Every JWC program answers all three, at these names, without declaring
them. A declared route wins over them — that is the one way to take one
away, and it has to be deliberate.

`/healthz` touches nothing. Putting a dependency behind a liveness probe
turns a database blip into a restart storm, and the restarts make the blip
worse.

`/readyz` round-trips every **configured** dependency and names the one
that failed. Redis is checked only when `JWC_REDIS_URL` is set, so a
program that does not use it does not start failing readiness.

## What `/metrics` exports

Only what applies. A program with no database exports no pool gauges; one
with no `job` exports no queue gauges. Everything is unlabelled — the
cardinality of a per-route label is a decision for your scraper, not one
JWC makes for you.

| Metric | Type | |
|---|---|---|
| `jwc_routes` | gauge | declared routes |
| `jwc_db_pool_size` | gauge | connections the pool holds |
| `jwc_db_pool_available` | gauge | idle and checkout-ready |
| `jwc_db_pool_max_size` | gauge | the ceiling, from `JWC_DB_POOL_SIZE` |
| `jwc_db_pool_waiting` | gauge | tasks blocked waiting for a connection |
| `jwc_redis_pool_size` | gauge | as above, when `JWC_REDIS_URL` is set |
| `jwc_redis_pool_available` | gauge | |
| `jwc_redis_pool_max_size` | gauge | |
| `jwc_redis_pool_waiting` | gauge | |
| `jwc_jobs_pending` | gauge | waiting or leased |
| `jwc_jobs_dead` | gauge | exhausted their retries |
| `jwc_jobs_processed_total` | counter | ran to completion |
| `jwc_jobs_failed_total` | counter | attempts that raised, retries included |
| `jwc_jobs_dead_total` | counter | moved to the dead-letter table |
| `jwc_log_queue_depth` | gauge | buffered log rows not yet written |
| `jwc_log_queue_capacity` | gauge | the buffer's ceiling |
| `jwc_log_written_total` | counter | rows written |
| `jwc_log_batches_total` | counter | batches flushed |
| `jwc_log_failed_total` | counter | writes that failed |
| `jwc_log_dropped_total` | counter | rows dropped because the buffer was full |

The two worth alerting on first are **`jwc_db_pool_waiting`** — above zero
for any length of time means the pool is the bottleneck, not the database
— and **`jwc_jobs_dead`**, because a job that exhausted its retries is
work that silently did not happen.

`jwc_log_dropped_total` above zero means the log buffer overflowed and
rows were thrown away. It is a counter for exactly that reason: the number
is the only trace of what was lost.

## The access log

Off by default. One line per answered request, on **stderr**:

```bash
jwc serve --request-logging          # the interpreter
JWC_REQUEST_LOG=1 ./bin/release/app    # a native binary has no flags
```

```
[jwc] GET /notes/17 -> 200 3.4ms rid=4bf92f3577b34da6a3ce929d0e0e4736
```

`JWC_LOG_FORMAT=json` makes each line one JSON object — `level`, `kind`,
`request_id`, `method`, `path`, `status`, `latency_us` — which is what a
cluster log collector wants.

Both backends format the line from one shared source file, so a pipeline
configured against `jwc serve` reads `jwc build` output unchanged.

### The request id

It is the caller's **W3C `traceparent` trace-id** when the request carries
a valid one, so a line here joins the trace the caller already started.
Without one it is a 16-hex-digit id from this process.

Either way it goes back as `x-request-id`, whether or not the log is on —
a client cannot turn the switch on, and correlating its report with a
server line is the point.

## OTLP tracing

```bash
JWC_OTLP_ENDPOINT=http://collector:4317
JWC_SERVICE_NAME=notes-api          # `service.name` on the spans
```

Empty — the default — disables the export entirely, and nothing is
started. There is no sampling knob: the collector is where sampling
belongs, because it is the only place that can see the whole trace.

An inbound `traceparent` reattaches to the caller's trace, which is the
same header the access log's request id comes from — so the id in a log
line and the trace id in your tracing backend are the same string, and
that is how you get from one to the other.

## What is not here

**No per-route metrics.** `jwc_http_requests_total{route="…"}` is not
exported, because the label's cardinality is bounded by your route table
today and by whatever a client sends the day someone adds a wildcard.
Derive it from the access log, where the path is data rather than a series
name.

**No log levels.** The access line and the operational lines are the whole
output. `debug.dump` prints under `--dev` and is refused in a request path
otherwise (`W1301` warns about one left in the source).

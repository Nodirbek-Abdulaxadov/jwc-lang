---
sidebar_position: 7
description: "The three observability surfaces JWC ships with: HTTP health endpoints, Prometheus metrics and structured request logs."
---

# Observability

JWC ships three observability surfaces out of the box: HTTP health /
readiness probes, a Prometheus scrape endpoint, and structured access
logs. **Distributed tracing (OTLP) is optional / advanced** — see
[deployment/otlp](./otlp.md). OTLP is an ops tool, not a JWC ergonomics
surface; default builds and runs don't enable it.

## Health probes

Two endpoints — both built-in unless the user's program declares its
own route at the same path:

| Path | What it does | Use it for |
|---|---|---|
| `/healthz` | Always 200 `{"status":"ok"}`. Process-alive only — no DB round-trip. | k8s `livenessProbe` |
| `/readyz` | Runs a real `engine::ping()` (pool checkout + `SELECT 1`) when a DB is configured; returns 200 `{"status":"ready","db":"ok"}` on success, 503 + error detail on failure. When no DB is configured, falls back to process-alive. | k8s `readinessProbe`, load-balancer health checks |

The distinction matters: a pod with a flaky DB should keep being
restarted (`liveness` = green) only as long as it can still serve other
work, but should stop receiving traffic (`readiness` = red) until the
DB comes back. The split here closes a real gap — earlier projects
hand-rolled `/healthz` with no DB check and stayed green through
database outages.

## Prometheus metrics

`/metrics` exposes counters and gauges in Prometheus text exposition
format (`text/plain; version=0.0.4`). Scrape with the standard
Prometheus annotation:

```yaml
prometheus.io/scrape: "true"
prometheus.io/port:   "8080"
prometheus.io/path:   "/metrics"
```

What's there:

| Metric | Type | Meaning |
|---|---|---|
| `jwc_requests_total{method,status}` | counter | Completed requests by method + status. |
| `jwc_requests_in_flight` | gauge | Requests currently being served. |
| `jwc_request_duration_ms_bucket` / `_sum` / `_count` | histogram | Per-request wall time in ms. |
| `jwc_errors_total{kind}` | counter | Handler errors by `JwcErrorKind`. |
| `jwc_queue_pending` | gauge | Jobs waiting on a worker (memory or postgres driver). |
| `jwc_queue_dlq` | gauge | DLQ depth (memory or postgres driver). |
| `jwc_db_pool_size` | gauge | Connections held by the deadpool-postgres pool right now. |
| `jwc_db_pool_available` | gauge | Idle connections immediately checkout-able. |
| `jwc_db_pool_max_size` | gauge | Configured pool ceiling (= `JWC_DB_POOL_SIZE`). |
| `jwc_db_pool_waiting` | gauge | Tasks queued for a pool slot — non-zero means contention. |

The four `jwc_db_pool_*` gauges are skipped (not emitted) when no DB
is configured — a missing gauge is a clearer signal than a misleading
zero. Wire them into the standard "pool saturation > 80% for 5 min"
alert in any non-trivial deployment.

## API docs (OpenAPI)

The server generates an OpenAPI spec **at request time from the running
routes**, so it can never drift from what the service actually serves:

| Path | What it does |
|---|---|
| `/openapi.json` | OpenAPI 3.0.3 document — paths from your routes, path/query params from handler signatures, request bodies from `validate body`, `400`/`401` responses inferred from validation + `Auth*` middleware, and `components.schemas` per entity/class. |
| `/docs` | Swagger UI that renders `/openapi.json`. |

Both are built-in unless your program declares its own route at the same
path, and both are off when `JWC_DISABLE_OPENAPI=1` (or `true`) — set that
for deployments that don't want the API surface advertised publicly.

The same document is available offline from the CLI — `jwc openapi`
(3.0.3, the form served at runtime) or `jwc swagger` (3.1) — for
contract tests, codegen, or committing a spec snapshot.

## Access logs

Default format is one line per request:

```
2026-06-13T10:32:17Z GET /api/users 200 12ms client=1.2.3.4 req_id=68a1b2c3deadbeef
```

Switch to newline-delimited JSON with `JWC_LOG_FORMAT=json`:

```json
{"ts":"2026-06-13T10:32:17Z","method":"GET","path":"/api/users","status":200,"dur_ms":12,"client":"1.2.3.4","request_id":"68a1b2c3deadbeef"}
```

Loki, Datadog, CloudWatch and the rest of the JSON-log world ingest
the second form without a parser config.

The `request_id` field on every line matches what `request_id()`
returns inside the handler — the same id is also echoed back as the
`x-request-id` response header. A single curl-to-log round-trip can be
correlated end-to-end without any extra wiring.

## W3C `traceparent` propagation

If the inbound request carries a `traceparent` header (W3C Trace
Context), JWC extracts the trace-id and stamps it into the access log
+ any spans the OTLP exporter emits. No env var, no opt-in — if your
upstream is already in the trace, JWC's logs and spans attach to the
same trace automatically.

A malformed `traceparent` is ignored (not a 4xx) — never refuse a
request over a broken upstream tracing header.

Outbound `traceparent` injection on runtime-initiated HTTP calls
(`http_get`, `http_post`, `fetch_json`) is on the Sprint 5C roadmap.

## See also

- [deployment/otlp](./otlp.md) — OpenTelemetry / Jaeger / Tempo recipe.
- [deployment/env-vars](./env-vars.md) — every `JWC_*` knob.
- [backend/middleware](../backend/middleware.md#response-phase-after---) — the `after { ... }` block + `response_status()` / `response_duration_ms()` for per-route metrics.
- [backend/queue](../backend/queue.md) — queue depth + DLQ gauges.

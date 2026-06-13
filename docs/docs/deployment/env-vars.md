---
sidebar_position: 5
---

# Environment variables

Every `JWC_*` env var the runtime reads, with its default and an
indication of which subsystem owns it. Group order matches the order a
production checklist usually walks: connect → harden → observe → tune.

## Database

| Var | Default | Notes |
|---|---|---|
| `DATABASE_URL` | _(unset)_ | Postgres connection string. Highest priority. Same shape `tokio-postgres` accepts. |
| `JWC_DATABASE_URL` | _(unset)_ | Same as `DATABASE_URL`. Honoured when the former is absent so JWC apps can coexist with non-JWC sidecars on the same env. |
| `PG_HOST` / `PG_PORT` / `PG_USER` / `PG_PASSWORD` / `PG_DATABASE` | `localhost` / `5432` / `postgres` / _(empty)_ / `postgres` | Per-field fallback when neither URL var is set. Useful for k8s secrets that ship fields separately. |
| `JWC_DB_POOL_SIZE` | `64` | Max pool size. Lower for memory-constrained pods. |
| `JWC_DB_TLS` | unset | `1` / `true` enables TLS to the DB. |
| `JWC_DB_TLS_INSECURE_SKIP_VERIFY` | unset | `1` / `true` skips cert verification. Dev only. |

## HTTP server hardening

| Var | Default | Notes |
|---|---|---|
| `JWC_MAX_BODY_BYTES` | `2097152` (2 MiB) | Request body cap. `0` disables for behind-LB setups. |
| `JWC_REAL_IP_HEADER` | `x-forwarded-for` | Header `client_ip()` reads. Flip to `cf-connecting-ip` behind Cloudflare. |
| `JWC_TRUSTED_PROXIES` | _(empty)_ | Comma-separated list of IP / prefix entries. Peeled off the chain right-to-left so the first untrusted entry wins. Empty ⇒ "trust no proxy", rightmost entry wins. |
| `JWC_SHUTDOWN_TIMEOUT` | `5` (seconds) | How long graceful shutdown waits for in-flight requests + queued jobs before the watchdog forces exit. |
| `JWC_REQUEST_TIMEOUT` | `30` (seconds) | Per-request budget. The watchdog returns 504 + `{"error":"request timed out after Ns"}` if the handler hasn't finished. `0` disables — use only on projects that genuinely need long-running responses. |

## Observability

| Var | Default | Notes |
|---|---|---|
| `JWC_LOG_FORMAT` | _(text)_ | `json` switches access + error logs to newline-delimited JSON. Loki / Datadog / CloudWatch ingest natively. |
| `JWC_SERVER_METRICS` | `false` | `1` / `true` enables a periodic `eprintln!` metrics line on top of `/metrics`. Mainly useful when no Prometheus is reachable. |
| `JWC_SERVER_METRICS_INTERVAL_SECS` | `10` | Interval for the above. |
| `JWC_OTLP_ENDPOINT` | _(unset)_ | OTLP HTTP collector URL (e.g. `http://localhost:4318/v1/traces`). When set, the server exports spans to that endpoint. Requires the binary to be built with `--features otlp`. |
| `JWC_SERVICE_NAME` | `jwc` | `service.name` resource attribute on every exported span — drives the service picker in Jaeger / Tempo / Honeycomb. Set per-deployment so pods don't blur into one entry. |

### How do I see my traces?

JWC speaks the W3C Trace Context spec and exports OTLP over HTTP, so any
OpenTelemetry-compatible backend works. Two common local setups:

- **Jaeger all-in-one** — `docker run -p 4318:4318 -p 16686:16686 jaegertracing/all-in-one:latest`, then run JWC with `JWC_OTLP_ENDPOINT=http://localhost:4318/v1/traces` and browse `http://localhost:16686`.
- **Grafana Tempo** — point `JWC_OTLP_ENDPOINT` at your Tempo HTTP receiver (default `:4318/v1/traces`) and query in Grafana via the Tempo datasource.

Inbound `traceparent` headers are honoured automatically — if your
upstream service is already in the trace, JWC's spans attach to the same
trace id without configuration. Outbound traceparent propagation on
runtime-initiated HTTP calls is on the Sprint 5C roadmap.

Builds without the `otlp` Cargo feature ignore `JWC_OTLP_ENDPOINT` and
print a one-line warning at startup so the misconfiguration is visible.

## Worker / queue

| Var | Default | Notes |
|---|---|---|
| `JWC_SERVER_WORKERS` | host CPUs | Tokio worker thread count. |
| `JWC_QUEUE_WORKERS` | `2` (capped to host CPUs) | Background job worker pool size. |
| `JWC_QUEUE_MAX_ATTEMPTS` | `3` | Per-job retry ceiling before sending to DLQ. |
| `JWC_QUEUE_BACKOFF_MS` | `1000` | Initial backoff between retries. Caps at 60s exponential. |
| `JWC_QUEUE_DLQ_MAX` | `1024` | DLQ ring size — oldest entries evicted on overflow. |

## Cache

| Var | Default | Notes |
|---|---|---|
| `JWC_QUERY_CACHE_TTL_SECS` | _(unset)_ | When set, enables a TTL result cache for `select` queries. |

## Email

| Var | Default | Notes |
|---|---|---|
| `JWC_SMTP_HOST` / `JWC_SMTP_PORT` / `JWC_SMTP_USER` / `JWC_SMTP_PASSWORD` / `JWC_SMTP_FROM` | _(unset)_ | `send_email()` looks here. All required when the builtin is called. |

## Native AOT toolchain

| Var | Default | Notes |
|---|---|---|
| `JWC_REGISTRY_URL` | `https://jwc-registry.1kb.uz/` | Package registry endpoint the resolver hits. |
| `JWC_TOKEN` | _(unset)_ | Bearer credential for publish/install against a private registry. |

Anything else surfacing in an error message but not listed here is a
documentation gap — please open a PR.

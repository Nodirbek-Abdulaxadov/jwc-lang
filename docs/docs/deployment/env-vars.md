---
sidebar_position: 5
description: "Every JWC_* environment variable the runtime reads, with its default, the subsystem that owns it and what changing it affects."
---

# Environment variables

Every `JWC_*` env var the runtime reads, with its default, owning
subsystem, and the release it landed in. The canonical source is
[`src/config.rs::REGISTRY`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/src/config.rs)
— the boot path walks the same table, prints the rendered values at
startup, and fails fast if a numeric var was set to a non-numeric
string. Group order matches the order a production checklist usually
walks: connect → harden → observe → tune.

The boot table is printed by default. Suppress it with
`JWC_PRINT_CONFIG=0` once the values are pinned in a deployment.

## Database

| Var | Default | Description | Since |
|---|---|---|---|
| `DATABASE_URL` | _(unset)_ | Postgres connection string. Highest priority. Same shape `tokio-postgres` accepts. | v0.1 |
| `JWC_DATABASE_URL` | _(unset)_ | Same as `DATABASE_URL`. Honoured when the former is absent so JWC apps can coexist with non-JWC sidecars on the same env. | v0.1 |
| `PG_HOST` / `PG_PORT` / `PG_USER` / `PG_PASSWORD` / `PG_DATABASE` | `localhost` / `5432` / `postgres` / _(empty)_ / `postgres` | Per-field fallback when neither URL var is set. Useful for k8s secrets that ship fields separately. | v0.1 |
| `JWC_DB_POOL_SIZE` | `64` | Max connections in the deadpool-postgres pool. Lower for memory-constrained pods. | v0.2 |
| `JWC_DB_TLS` | `false` | `1` / `true` enables TLS to the DB via `tokio-postgres-rustls`. | v0.3 |
| `JWC_DB_TLS_INSECURE_SKIP_VERIFY` | `false` | `1` / `true` skips cert verification. Dev only. | v0.3 |
| `JWC_DB_RETRY_MAX_ATTEMPTS` | `3` | Transient-error retry ceiling outside transactions (`57P01`, `40001`, etc.). | v0.4.6 |
| `JWC_DB_RETRY_BACKOFF_MS` | `100` | Base retry backoff in milliseconds; doubles per attempt. | v0.4.6 |
| `JWC_ADMIN_DB` | `postgres` | Admin DB used by `migrate` to create the target DB. | v0.4 |

Cross-link: [data/dbcontext](../data/dbcontext.md), [deployment/migrations](./migrations.md).

## Redis

Only read by binaries built with `--features redis`. A build without the
feature warns at boot if `JWC_REDIS_URL` is set, then carries on with the
`redis_*` built-ins failing — see [deployment/redis](./redis.md).

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_REDIS_URL` | _(unset)_ | Redis connection string; unset disables the `redis_*` built-ins. Use `rediss://` for TLS. No bare `REDIS_URL` fallback — see [deployment/redis](./redis.md). | v0.8.9 |
| `JWC_REDIS_POOL_SIZE` | `64` | Max connections in the deadpool-redis pool. `0` is ignored (a zero-size pool never hands out a connection). | v0.8.9 |
| `JWC_REDIS_RETRY_MAX_ATTEMPTS` | `3` | Transient-error retry ceiling (dropped connection, timeout, `LOADING`, cluster `MOVED`/`ASK`). `1` disables retries. | v0.8.9 |
| `JWC_REDIS_RETRY_BACKOFF_MS` | `100` | Base Redis retry backoff in milliseconds; doubles per attempt. | v0.8.9 |

Cross-link: [deployment/redis](./redis.md).

## HTTP server hardening

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_MAX_BODY_BYTES` | `2097152` (2 MiB) | Request body cap. `0` disables for behind-LB setups. | v0.3 |
| `JWC_REAL_IP_HEADER` | `x-forwarded-for` | Header `client_ip()` reads. Flip to `cf-connecting-ip` behind Cloudflare. | v0.4 |
| `JWC_TRUSTED_PROXIES` | _(empty)_ | Comma-separated list of IP / prefix entries. Peeled off the chain right-to-left so the first untrusted entry wins. Empty ⇒ "trust no proxy", rightmost entry wins. | v0.4 |
| `JWC_HTTP_ALLOWLIST` | _(unset)_ | CSV host allowlist for outbound `http_get` / `http_post` / `fetch_json`. Empty ⇒ no restriction (default). Closes the SSRF gap for production. | v0.4.8 |
| `JWC_SHUTDOWN_TIMEOUT` | `5` (seconds) | How long graceful shutdown waits for in-flight requests + queued jobs before the watchdog forces exit. | v0.4 |
| `JWC_REQUEST_TIMEOUT` | `30` (seconds) | Per-request budget. The watchdog returns 504 + `{"error":"request timed out after Ns"}` if the handler hasn't finished. `0` disables — use only on projects that genuinely need long-running responses. | v0.4 |

Cross-link: [backend/middleware](../backend/middleware.md), [stdlib/http](../stdlib/http.md), [security](./../security/index.md).

## Observability

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_LOG_FORMAT` | `text` | `json` switches access + error logs to newline-delimited JSON. Loki / Datadog / CloudWatch ingest natively. | v0.4 |
| `JWC_SERVER_METRICS` | `false` | `1` / `true` enables a periodic `eprintln!` metrics line on top of `/metrics`. Mainly useful when no Prometheus is reachable. | v0.4 |
| `JWC_SERVER_METRICS_INTERVAL_SECS` | `10` | Interval for the above. | v0.4 |
| `JWC_OTLP_ENDPOINT` | _(unset)_ | OTLP HTTP collector URL (e.g. `http://localhost:4318/v1/traces`). When set, the server exports spans to that endpoint. Requires the binary to be built with `--features otlp`. | v0.4.6 |
| `JWC_SERVICE_NAME` | `jwc` | `service.name` resource attribute on every exported span — drives the service picker in Jaeger / Tempo / Honeycomb. Set per-deployment so pods don't blur into one entry. | v0.4.6 |
| `JWC_PRINT_CONFIG` | `true` | Print the rendered config table at server boot. Set off (`0`) to suppress once values are pinned in a deployment. | v0.4.6 |
| `JWC_DISABLE_OPENAPI` | `false` | `1` / `true` turns off the built-in `/openapi.json` + `/docs` endpoints — for deployments that don't want the API surface advertised publicly. | v0.6.0 |

Cross-link: [deployment/observability](./observability.md), [deployment/otlp](./otlp.md).

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

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_SERVER_WORKERS` | `0` (host CPUs) | Tokio worker thread count. `0` = `available_parallelism()`. | v0.4 |
| `JWC_QUEUE_DRIVER` | `memory` | Background queue backend: `memory` (default, in-process, lost on restart) or `postgres` (durable, `_jwc_jobs` + `_jwc_jobs_dlq` tables, multi-process safe via `SELECT ... FOR UPDATE SKIP LOCKED`). | v0.4.6 |
| `JWC_QUEUE_WORKERS` | `0` → 2 (capped at host CPUs) | Background job worker pool size. | v0.4 |
| `JWC_QUEUE_MAX_ATTEMPTS` | `3` | Per-job retry ceiling before sending to DLQ. | v0.4 |
| `JWC_QUEUE_BACKOFF_MS` | `1000` | Initial backoff between retries. Caps at 60s exponential. | v0.4 |
| `JWC_QUEUE_DLQ_MAX` | `1024` | DLQ ring size — oldest entries evicted on overflow. Memory driver only; the Postgres driver has no cap. | v0.4 |

Cross-link: [backend/queue](../backend/queue.md).

## Cache

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_QUERY_CACHE_TTL_SECS` | `0` (off) | When set > 0, enables a TTL result cache for `select` queries. | v0.3 |

## Email

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_SMTP_HOST` / `JWC_SMTP_PORT` / `JWC_SMTP_USER` / `JWC_SMTP_PASSWORD` / `JWC_SMTP_FROM` | _(unset)_ / `587` / _(unset)_ / _(unset)_ / _(unset)_ | `send_email()` looks here. All required when the builtin is called. `JWC_SMTP_PASSWORD` is redacted from the boot config table. | v0.3 |
| `JWC_SMTP_TLS` | `starttls` | TLS mode: `starttls`, `tls`, or `none`. | v0.4 |

Cross-link: [stdlib/email](../stdlib/email.md).

## Registry / packaging

| Var | Default | Description | Since |
|---|---|---|---|
| `JWC_REGISTRY_URL` | `https://registry-jwc.1kb.uz/` | Package registry endpoint the resolver hits. | v0.3 |
| `JWC_REGISTRY_TOKEN` | _(unset)_ | Bearer credential for publish/install against a private registry. Redacted from the boot config table. | v0.3 |
| `JWC_HOME` | _(empty → platform default)_ | Override the per-user data dir (default `%LOCALAPPDATA%\jwc` / `~/.jwc`). | v0.4 |

Cross-link: [packages/publish](../packages/publish.md).

## Secrets redaction

The boot config table masks any var whose name contains `PASSWORD`,
`SECRET`, `TOKEN`, `KEY`, `JWT`, or `DATABASE_URL` (case-insensitive
substring). The mask is purely a display concern — the runtime still
reads the real value when it needs it.

`DATABASE_URL` itself (no `JWC_` prefix) is intentionally not in the
registry: it is documented separately and never echoed to logs.

Anything else surfacing in an error message but not listed here is a
documentation gap — please open a PR.

---
sidebar_position: 8
---

# Distributed tracing (OTLP)

JWC exports spans via OTLP HTTP — point it at any
OpenTelemetry-compatible backend (Jaeger, Tempo, Honeycomb, Datadog
agent, etc.) and you get per-request traces with handler / DB / queue
spans inside.

## Build the binary with `otlp`

Tracing is behind a Cargo feature so the default build doesn't pull
the OpenTelemetry crate tree into projects that don't need it.

```bash
cargo build --release --features otlp
```

Or from source via `cargo install`:

```bash
cargo install --path . --features otlp
```

A build without the `otlp` feature ignores `JWC_OTLP_ENDPOINT` at
runtime and prints a one-line warning at startup — the
misconfiguration is visible, not silent.

## Configure

| Env | Default | Notes |
|---|---|---|
| `JWC_OTLP_ENDPOINT` | _(unset)_ | OTLP HTTP receiver URL, e.g. `http://localhost:4318/v1/traces`. Empty / unset disables export. |
| `JWC_SERVICE_NAME` | `jwc` | `service.name` resource attribute on every exported span. Set per-deployment so multiple services don't blur into one entry in the trace UI. |

Both vars are in the v0.4.6 release. See [env-vars](./env-vars.md#observability).

## Local recipe — Jaeger all-in-one

The fastest "see your first trace" loop:

```yaml
# docker-compose.yml
services:
  jaeger:
    image: jaegertracing/all-in-one:latest
    ports:
      - "4318:4318"     # OTLP HTTP collector
      - "16686:16686"   # Jaeger UI
```

```bash
docker compose up -d jaeger
JWC_OTLP_ENDPOINT=http://localhost:4318/v1/traces \
JWC_SERVICE_NAME=my-app \
  jwc run
```

Browse `http://localhost:16686`, pick `my-app` from the service
dropdown, click "Find Traces". One per HTTP request, with handler and
DB spans nested inside.

## Grafana Tempo

Point `JWC_OTLP_ENDPOINT` at your Tempo HTTP receiver (default port
`4318`, path `/v1/traces`) and add the Tempo datasource in Grafana.
Same OTLP wire — no JWC-side difference.

## Inbound `traceparent`

JWC honours W3C Trace Context on every request without configuration.
If your upstream service is already in the trace, JWC's spans attach
to the same trace-id automatically. See
[deployment/observability](./observability.md#w3c-traceparent-propagation).

Outbound traceparent injection on `http_get` / `http_post` /
`fetch_json` is on the Sprint 5C roadmap — for now, traces fan out
through inbound headers only.

## What spans are emitted

| Span | When |
|---|---|
| `http.request` | One per inbound request, parent of everything else. Attributes: `http.method`, `http.route`, `http.status_code`, `request_id`. |
| `db.query` | One per `select` / `insert` / `update` / `delete`. Attributes: `db.system=postgresql`, `db.statement` (parameterised SQL, not the bound values). |
| `queue.dispatch` | One per job pulled off the queue, child of nothing (background work). |

The exporter batches spans on a background task — a slow collector
won't slow down request handling.

## See also

- [deployment/observability](./observability.md) — `/healthz`, `/readyz`, `/metrics`, access logs.
- [deployment/env-vars](./env-vars.md) — full env var catalog.

---
sidebar_position: 5
title: "Configuration"
description: "The server block, the environment variables the runtime reads, and the three operational endpoints every JWC program answers."
---

# Configuration

## The `server` block

```jwc
namespace app;

server {
    max_body_bytes  = 65536;
    max_page_size   = 100;
    request_timeout = "15s";
    cursor_secret   = env("CURSOR_SECRET");
    trusted_proxies = ["10.0.0.0/8", "172.16.0.0/12"];
    strict_slash    = true;

    cors {
        origins = ["https://app.example.com"];
        methods = ["GET", "POST"];
    }
}

function main() { serve(8080); }
```

| Key | Default | What it does |
|---|---|---|
| `bind` | `0.0.0.0` | listen address |
| `max_body_bytes` | 1 MiB | a larger body is a 413, refused before middleware |
| `max_page_size` | 100 | the ceiling a `page … size n` is clamped to |
| `cursor_secret` | — | **required** if any query pages; signs the cursors |
| `request_timeout` | none | per-request wall clock |
| `header_timeout` | none | how long the request line and headers may take |
| `trusted_proxies` | empty | CIDRs `request.client_ip()` peels off the forwarded chain |
| `strict_slash` | false | whether `/a/` and `/a` are the same route |
| `cors { }` | absent | absent means **no** CORS headers at all |
| `tls { }` | absent | terminate TLS in-process |

An unknown key is `E1206`, not a silent no-op: a misspelled setting that
does nothing is worse than one that refuses to start.

CORS being absent by default is deliberate. A browser refusing a
cross-origin call is the correct default, and a header emitted "just in
case" is a policy nobody wrote.

## Environment

The database URL is never declared in source — it comes from the
environment, because it differs per deployment and belongs in one:

| Variable | |
|---|---|
| `DATABASE_URL` | Postgres connection string |
| `JWC_DB_POOL_SIZE` | pool ceiling, default 64 |
| `JWC_REDIS_URL` | enables the `redis.*` surface |
| `JWC_LOG_SQL` | log every statement with its binds and timing |
| `JWC_DEV` | `debug.dump` prints |
| `JWC_BIND_HOST` | overrides `server { bind }` |
| `JWC_SHUTDOWN_TIMEOUT` | seconds to drain on SIGINT, default 5 |

A `.env` file next to the project — or next to the binary — is read at
startup.

## The three operational endpoints

Every JWC program answers these, at these names, without declaring them:

### `GET /healthz`

```json
{"status":"ok"}
```

Liveness. Touches nothing. A process that answers this is one the
supervisor should not kill — wiring a dependency in here is the classic
way to turn a database blip into a restart storm.

### `GET /readyz`

```json
{"status":"ready"}
```

Readiness: every configured dependency, actually round-tripped. When one
is down it is a 503, and the body names which:

```json
{"status":"unready","failed":["db_unreachable"]}
```

A probe that only says "not ready" sends the operator to the logs of a pod
that is already out of rotation.

Redis is checked only when it is configured. A deployment that never set
`JWC_REDIS_URL` does not start failing its probe because the runtime grew
a Redis driver.

### `GET /metrics`

Prometheus text format, gauges only:

```
jwc_db_pool_size 4
jwc_db_pool_available 4
jwc_db_pool_max_size 64
jwc_db_pool_waiting 0
jwc_routes 29
```

`available` pinned at 0 while `waiting` climbs is the leak signature.

## A declared route wins

A program that writes its own `/metrics` keeps it. A wildcard that
happens to span the name does not — jwc-shortener declares `/{code}` for
its redirects, which matched `/readyz` too, so every pod stayed out of
rotation and nothing in the source mentioned `/readyz` for an operator to
find. A pattern nobody aimed at these three does not take them away.

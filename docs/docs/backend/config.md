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

The table below is **generated from `src/config.rs`** — the same registry
the runtime reads at boot — so it cannot describe a variable the compiler
does not have, or miss one it does.

<!-- generated:env-table -->
| Variable | Default | |
|---|---|---|
| `JWC_DATABASE_URL` | — | Postgres connection string (overrides DATABASE_URL). |
| `JWC_DB_POOL_SIZE` | `64` | Max connections in the deadpool-postgres pool. |
| `JWC_DB_TLS` | `false` | Connect to Postgres over TLS via tokio-postgres-rustls. |
| `JWC_DB_TLS_INSECURE_SKIP_VERIFY` | `false` | Skip cert verification (dev only — never set in prod). |
| `JWC_QUERY_CACHE_TTL_SECS` | `0` | Result-cache TTL; 0 disables caching. |
| `JWC_DB_RETRY_MAX_ATTEMPTS` | `3` | Transient-error retry ceiling (outside transactions). |
| `JWC_DB_RETRY_BACKOFF_MS` | `100` | Base retry backoff (ms); doubles each attempt. |
| `JWC_REDIS_URL` | — | Redis connection string; empty disables the redis_* built-ins. Use rediss:// for TLS. |
| `JWC_REDIS_POOL_SIZE` | `64` | Max connections in the deadpool-redis pool. |
| `JWC_REDIS_RETRY_MAX_ATTEMPTS` | `3` | Transient-error retry ceiling for Redis commands. |
| `JWC_REDIS_RETRY_BACKOFF_MS` | `100` | Base Redis retry backoff (ms); doubles each attempt. |
| `JWC_LOG_QUEUE` | `10000` | Channel capacity for log_insert; rows are dropped once full. |
| `JWC_LOG_BATCH` | `2000` | Rows per batched INSERT from the log writer. |
| `JWC_LOG_FLUSH_MS` | `200` | Longest a log_insert row waits before being written (ms). |
| `JWC_LOG_CONCURRENCY` | `4` | Batch INSERTs the log writer keeps in flight at once. |
| `JWC_SERVER_WORKERS` | `0` | Tokio worker threads. 0 or unset = one per available core, which in a container means the *cgroup* limit, not the host. |
| `JWC_REQUEST_LOG` | `0` | One access line per answered request, on stderr. `jwc serve --request-logging` sets it; a native binary has no flags, so this is how `jwc build` output is turned on. |
| `JWC_LOG_FORMAT` | `text` | Access-log shape: `text` or `json`. Read only when JWC_REQUEST_LOG is on. |
| `JWC_MAX_BODY_BYTES` | `2097152` | Request body cap (bytes); 0 disables. |
| `JWC_SHUTDOWN_TIMEOUT` | `5` | Graceful shutdown budget before force-exit. |
| `JWC_DEBUG_ERRORS` | `0` | Return the full error text on a 500 instead of a generic message. Local debugging only. |
| `JWC_CORS_ORIGINS` | — | Comma-separated allowed origins, or `*`. Empty disables CORS. |
| `JWC_CORS_METHODS` | `GET,POST,PUT,PATCH,DELETE,OPTIONS` | Methods echoed in the preflight response. |
| `JWC_CORS_HEADERS` | `content-type,authorization` | Request headers the browser may send cross-origin. |
| `JWC_CORS_EXPOSE_HEADERS` | `x-request-id` | Response headers readable by cross-origin JS. |
| `JWC_CORS_CREDENTIALS` | `0` | Allow cookies / Authorization cross-origin. Incompatible with `*`. |
| `JWC_CORS_MAX_AGE` | `86400` | Seconds a browser may cache the preflight result. |
| `JWC_REAL_IP_HEADER` | `x-forwarded-for` | Header name parsed by the request_ip() builtin. |
| `JWC_TRUSTED_PROXIES` | — | Comma-separated IPs/prefixes peeled off X-F-F. |
| `JWC_PRINT_CONFIG` | `true` | Print this table at boot, with secrets redacted. |
| `JWC_BIND_HOST` | — | Native builds only: override the listen address (`server { bind }` in the source). |
| `JWC_DEV` | `false` | Development mode: `debug.dump` prints. Never in production — it prints request data. |
| `JWC_HTTP_TIMEOUT_SECS` | `10` | Whole-request ceiling for outbound `http.*` calls. |
| `JWC_LOG_SQL` | `false` | Print every SQL statement the program issues, with its parameters. |
| `JWC_OTLP_ENDPOINT` | — | OTLP collector URL; empty disables tracing export. |
| `JWC_SERVICE_NAME` | `jwc` | `service.name` on exported traces. |
| `JWC_REGISTRY` | — | Package registry base URL; empty uses the default registry. |
| `JWC_REQUEST_BODY` | `null` | Native builds only: what `request.body()` answers outside a request. |
| `JWC_JOB_WORKERS` | `2` | Worker tasks polling the job queue. 0 = none in this process; another deployment of the same sources drains it. |
| `JWC_JOB_POLL_MS` | `1000` | How often a worker polls an empty queue, in milliseconds. |
| `JWC_SMTP_HOST` | — | SMTP server hostname. |
| `JWC_SMTP_PORT` | `587` | SMTP server port. |
| `JWC_SMTP_USER` | — | SMTP auth username. |
| `JWC_SMTP_PASSWORD` | — | SMTP auth password / app token. |
| `JWC_SMTP_FROM` | — | Default From: header for outbound mail. |
| `JWC_SMTP_TLS` | `starttls` | TLS mode: starttls \| tls \| none. |
| `JWC_CACHE_MAX_ENTRIES` | `10000` | Entry ceiling for the process-local `cache.*` store. |
| `JWC_HOME` | — | Where `jwc login` keeps its credentials. Default `~/.jwc`. |
| `JWC_HTTP_ALLOWLIST` | — | Comma-separated host allowlist for http_get/http_post/fetch_json; empty = no restriction. |
| `JWC_HTTP_BLOCK_PRIVATE` | `false` | Block loopback/private/link-local outbound hosts (incl. cloud metadata). |
| `JWC_JWT_LEEWAY_SECS` | `0` | Clock-skew tolerance applied to jwt_verify's exp/nbf checks. |
| `JWC_JWT_EXPECTED_ISS` | — | Require this 'iss' claim in jwt_verify; empty = not checked. |
| `JWC_JWT_EXPECTED_AUD` | — | Require this value in jwt_verify's 'aud' claim; empty = not checked. |
| `JWC_JWT_JWKS_TTL_SECS` | `300` | How long a fetched JWKS key set stays cached. |
| `JWC_JWT_JWKS_MIN_REFETCH_SECS` | `60` | Floor between forced JWKS refetches on an unknown 'kid' (DoS guard). |
<!-- /generated:env-table -->

## The `.env` file

A `.env` beside the project is read at startup, by `jwc serve`, `jwc run`,
`jwc migrate` and every other subcommand that works in a project. A binary
from `jwc build` reads one too — from its working directory, then from
beside the executable and up to three levels above it, so a deployed
`bin/release/app` finds the project's file.

```bash
# .env
DATABASE_URL=postgres://postgres@localhost:5432/app
export PORT=8080          # a line copied out of a shell works
CURSOR_SECRET="a long random string"
```

The rules are deliberately dull:

| | |
|---|---|
| `KEY=VALUE`, one per line | a leading `export ` is tolerated |
| `#` at the start of a line | a comment; `#` inside a value is part of the value |
| `'...'` or `"..."` around a value | stripped; nothing inside is interpreted |
| `$OTHER` inside a value | **not** expanded — a password containing `$` is a password |
| a line that is not `KEY=VALUE` | **warned about**, not silently skipped |
| a variable already in the environment | **wins** — the file never overwrites it |

The last row is what makes the file safe in production: a container's
configuration, or anything you `export`ed yourself, is untouched by a
`.env` that happens to be in the directory.

If `DATABASE_URL` is absent, the `PG_HOST` / `PG_PORT` / `PG_USER` /
`PG_PASSWORD` / `PG_DATABASE` block is assembled into one — all five, or
none: a half-filled block is a half-written configuration, and guessing
the rest is how a program connects to the wrong database.

Both backends run the same parser, pasted into the generated crate at
build time, so a file that works under `jwc serve` works in the binary.

### Setting one without a file

A `.env` is the convenient form, not the only one. The environment always
wins over the file, so these override it:

```bash
# bash / zsh — Linux, macOS, WSL, Git Bash
export DATABASE_URL=postgres://postgres@localhost:5432/app
jwc serve .

# or for one command only
DATABASE_URL=postgres://postgres@localhost:5432/app jwc serve .
```

```powershell
# PowerShell — Windows
$env:DATABASE_URL = "postgres://postgres@localhost:5432/app"
jwc serve .
```

```bat
:: cmd.exe — Windows
set DATABASE_URL=postgres://postgres@localhost:5432/app
jwc serve .
```

`export` is a **bash** word, not a JWC one. PowerShell rejects it, which
is why the `.env` file tolerates the prefix but never requires it — a file
written without `export` is read identically by both shells.

### Seeing what was read

```bash
JWC_PRINT_CONFIG=1 jwc serve .
```

prints every registered variable, its value, and whether the value came
from the environment or the default — with anything whose name looks like
a secret replaced by `*** (redacted)`:

```
ENV VAR                          SOURCE   VALUE                            ERROR
JWC_DATABASE_URL                 env      *** (redacted)
JWC_DB_POOL_SIZE                 default  64
JWC_SMTP_PASSWORD                env      *** (redacted)
```

A value that does not parse **stops the boot**, before the listener opens:

```
Error: config: 1 env var(s) failed to parse:
  JWC_DB_POOL_SIZE: invalid usize 'twenty': invalid digit found in string
```

That is deliberate. The alternative is what used to happen: the bad value
was swallowed by a default deeper in the code, the pool was quietly the
wrong size, and nothing said so.

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

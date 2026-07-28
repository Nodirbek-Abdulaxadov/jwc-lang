---
sidebar_position: 1
description: "Declare a database connection with dbcontext and point it at Postgres. Connection strings, environment variables and pool configuration."
---

# dbcontext

A `dbcontext` declares a database connection. JWC supports **Postgres** only in v1.

```jwc
dbcontext AppDb: Postgres;
```

The driver name is part of the declaration — there is no block form and no
`driver = "..."` setting. Entities then name the context they belong to:

```jwc
dbcontext AppDb: Postgres;

entity Note of AppDb {
    id    int pk autoincrement;
    title varchar(200);
}
```

The connection URL is read at runtime from env (in order of precedence):

1. `JWC_DATABASE_URL`
2. `DATABASE_URL`
3. The dotenv file (`.env` in project root) — same two variable names

Format: `postgres://user:password@host:5432/dbname` (`postgres-native-tls` is enabled when `JWC_DB_TLS=1`).

## Multiple contexts

```jwc
dbcontext AppDb: Postgres;
dbcontext AnalyticsDb: Postgres;
```

Each context is a separate connection pool keyed by `JWC_DATABASE_URL_<NAME>` (uppercase). Defaults: if `JWC_DATABASE_URL_ANALYTICSDB` is unset, falls back to `JWC_DATABASE_URL`.

## Pool tuning

| Env | Default | Effect |
|---|---|---|
| `JWC_DB_POOL_SIZE` | based on CPU count | Max active connections per context |
| `JWC_DB_MIN_IDLE` | 1 | Keep this many warm |
| `JWC_DB_MAX_LIFETIME_SECS` | 1800 (30 min) | Recycle connections older than this |
| `JWC_DB_IDLE_TIMEOUT_SECS` | 600 (10 min) | Close idle connections after this |
| `JWC_DB_CONNECTION_TIMEOUT_SECS` | 5 | Bail if checkout takes longer |

## TLS

```bash
export JWC_DB_TLS=1
export JWC_DB_TLS_INSECURE_SKIP_VERIFY=1   # only for self-signed dev certs
```

Both the runtime pool and `jwc migrate` honour the same flags.

## Caching

In-memory TTL cache for SELECT results:

```bash
export JWC_QUERY_CACHE_TTL_SECS=60
```

Cache keys are the SQL shape + bound params; `update`/`insert`/`delete` against the table invalidate every cached row for that table.

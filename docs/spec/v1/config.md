# config.md — `database init()`, `server { }`, environment

Normative. Closes gap **#39** and the configuration half of **#15**.

---

## 1. Three places, one rule each

| Where | What belongs there |
|---|---|
| `DATABASE_URL` / env | **secrets and per-deployment values** |
| `database App : Postgres { init() { … } }` | **connection pool behaviour** |
| `server { … }` | **HTTP listener behaviour** |

Nothing that differs between staging and production is written in source.

---

## 2. `database`

```jwc
database App : Postgres {
    init() {
        pool_size         = int(env("DB_POOL") ?? "20");
        statement_timeout = "10s";
        tls               = env("DB_TLS") == "1";
    }
}
```

2.1 `App` is the name that qualifies schemas in source (`App.auth.Accounts`).
It is **not** the database name — that comes from `DATABASE_URL`, always.
Declaring a database name would put an environment value in source
(names §4.5).

2.2 `: Postgres` names the driver. It is the only value in 1.0; a second one
would require a dialect abstraction, which is a declared non-goal
(ROADMAP §8).

2.3 `init()` runs once at boot, before any connection is opened. It may call
`env()` and the coercions and nothing else — no queries, no I/O
(`E1201`).

2.4 Keys:

| Key | Type | Default |
|---|---|---|
| `pool_size` | `int` | 20 |
| `pool_timeout` | duration | `5s` |
| `statement_timeout` | duration | `10s` |
| `connect_timeout` | duration | `5s` |
| `tls` | `boolean` | false |
| `tls_root_cert` | `text?` | none |
| `application_name` | `text` | the project name |

An unknown key is `E1202`. Duration values are strings: `10s`, `500ms`,
`2m`.

2.5 More than one `database` declaration is `E1203`. Multi-database is a
non-goal.

---

## 3. `server { }` (#39)

```jwc
server {
    max_body_bytes  = 1048576;
    request_timeout = "30s";
    header_timeout  = "10s";
    max_page_size   = 200;
    strict_slash    = true;
    cursor_secret   = env("CURSOR_SECRET");
    trusted_proxies = ["10.0.0.0/8", "172.16.0.0/12"];

    cors {
        origins     = ["https://app.example.com"];
        methods     = ["GET", "POST", "PATCH", "DELETE"];
        headers     = ["authorization", "content-type"];
        credentials = true;
        max_age     = "600s";
    }

    tls {
        cert = env("TLS_CERT_PATH");
        key  = env("TLS_KEY_PATH");
    }
}
```

3.1 At most one `server` block (`E1204`). It is optional; every key has a
default.

3.2 Keys:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `max_body_bytes` | `int` | 1048576 | over → 413 before middleware (routing §5.1) |
| `request_timeout` | duration | `30s` | whole request |
| `header_timeout` | duration | `10s` | request line + headers |
| `max_page_size` | `int` | 100 | ceiling for `page … size` (queries §9.2) |
| `strict_slash` | `boolean` | true | `/x/` → 308 → `/x` |
| `cursor_secret` | `text` | — | HMAC key for keyset cursors; **required** if any query uses `page` (`E1205`) |
| `trusted_proxies` | `inet[]` | `[]` | see §3.3 |
| `shutdown_grace` | duration | `20s` | drain window on SIGTERM |

3.3 **`trusted_proxies` is the whole of the `client_ip` rule** (#15).
Empty (the default) means `X-Forwarded-For` is ignored and
`request.client_ip()` returns the socket peer. Non-empty means the header is
walked from the right past addresses in the set. There is no third mode and
no "trust one hop" heuristic: both known failure modes — a spoofable rate
limiter and a self-DoS when every request shares a proxy IP — come from
guessing.

3.4 `cors` — when present, `OPTIONS` is answered automatically for every
declared route. When absent, no CORS headers are emitted at all.

3.5 `tls` — when present the listener is HTTPS. Absent means plain HTTP,
which is correct behind a terminating proxy.

**Not implemented.** The listener is HTTP-only, and a declared `tls { }`
makes `jwc serve` **refuse to boot** rather than serve plain text under a
name that says otherwise. That is the one misconfiguration an operator
cannot see for themselves: the listener answers, and every byte is in the
clear. Terminate at a proxy and leave the block out.

3.6 `header_timeout` is likewise **not enforced**, and a declared one
refuses to boot for the same reason: reading the request line and headers
belongs to the HTTP server, and `axum::serve` does not expose the knob. Set
it on the proxy in front. The default in the table is what the runtime would
use if it could, not a promise it keeps.

3.7 `request_timeout` **is** enforced, around the whole of `handle`. Past it
the answer is 504 and the handler's task is dropped, which releases whatever
it held: a request that has already lost its client is a connection and a
pool slot nobody is waiting on.

3.8 `shutdown_grace` is the drain window on SIGTERM *and* on Ctrl-C. A
server that handles only one of them drops in-flight requests on whichever
it missed.

---

## 4. Environment variables read by the runtime

| Variable | Used by |
|---|---|
| `DATABASE_URL` / `JWC_DATABASE_URL` | connection |
| `PORT` | only if the program reads it: `serve(int(env("PORT") ?? "8080"))` |
| `JWC_LOG_LEVEL` | `error` / `warn` / `info` / `debug` |
| `JWC_LOG_FORMAT` | `json` (default) / `text` |
| `JWC_LOG_SQL` | `1` logs every statement with timing (queries §7.4) |
| `JWC_REDIS_URL` | the `redis` package |

`env(k)` returns `text?`. The environment is snapshotted at boot; changing a
variable at runtime has no effect.

---

## 5. Secrets never appear in output

`env()` values used as `secret`, `key`, `password`, `token` or `cursor_secret`
are redacted in `jwc config --print`, in logs, and in error messages. The
redaction list is by key name, and the check is on the **variable name**, not
the value.

---

## 6. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1201` | I/O or query inside `init()` |
| `E1202` | unknown `init()` key |
| `E1203` | more than one `database` |
| `E1204` | more than one `server` block |
| `E1205` | `page` used with no `cursor_secret` |

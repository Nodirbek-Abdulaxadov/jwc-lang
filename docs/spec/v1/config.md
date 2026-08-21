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
| `bind` | `text` | `"0.0.0.0"` | the address the listener binds |
| `cursor_secret` | `text` | — | HMAC key for keyset cursors; **required** if any query uses `page` (`E1205`) |
| `trusted_proxies` | `inet[]` | `[]` | see §3.3 |
| `shutdown_grace` | duration | `20s` | drain window on SIGTERM |

3.2.1 `bind` takes an IP address, not a hostname. The default answers on
every interface, which is what a container publishing a port expects; a
development machine that should not be answering its own network writes
`bind = "127.0.0.1"`. A value that does not parse as an address stops the
server rather than falling back to the default — the fallback would put the
listener on every interface, which is the opposite of what writing the key
was reaching for, and nothing outside the process would show it.

3.2.2 The **port** is not a `server { }` key: it is the argument of
`serve(port)` in `main()` (builtins §2). `main` is evaluated at boot, so the
argument is an expression and not a literal —
`serve(int(env("PORT") ?? "8080"))` is the ordinary form. A `main` that
raises stops the boot and says so rather than listening somewhere nobody
asked for; a program with no `main` listens on 8080.

`jwc serve --port N` overrides the program's own value, for a local run that
needs a different port than the one the program ships with. Without the flag
the program decides.

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

`cert` and `key` are paths to PEM files, read **at boot**. A block whose
paths do not resolve, name a missing file, or hold a key that does not go
with the certificate stops the server. It does not fall back to plain HTTP:
that is the one misconfiguration an operator cannot see for themselves,
because the listener answers either way and every byte would be in the
clear. The listener advertises `h2` and `http/1.1` over ALPN, so an HTTPS
client gets the same HTTP/2 an HTTP/1.1 client would negotiate down from.

3.6 `header_timeout` bounds the request line and the headers. It is
separate from `request_timeout` because it has to be: that clock starts in
the handler, and a client dribbling headers a byte at a time never reaches
one. Past the deadline the connection is closed. It applies to HTTP/1;
HTTP/2 has frame-level limits of its own and takes no equivalent setting.

3.7 `request_timeout` **is** enforced, around the whole of `handle`. Past it
the answer is 504 and the handler's task is dropped, which releases whatever
it held: a request that has already lost its client is a connection and a
pool slot nobody is waiting on.

3.8 `shutdown_grace` is the drain window on SIGTERM *and* on Ctrl-C. A
server that handles only one of them drops in-flight requests on whichever
it missed.

---

## 4. Operational endpoints

4.0.1 Three paths are served by the runtime, at fixed names, on `GET`:

| Path | Answers | Touches |
|---|---|---|
| `/healthz` | `200 {"status":"ok"}` | nothing |
| `/readyz` | `200 {"status":"ready"}` or `503 {"status":"unready","failed":[…]}` | every configured dependency |
| `/metrics` | Prometheus text (`text/plain; version=0.0.4`) | nothing |

4.0.2 They are **not declarable**, and that is deliberate. An operator needs
them at a known path before reading anyone's source, and a liveness probe
that depends on the application having remembered to write one is a
deployment that restarts for the wrong reasons.

4.0.3 A **declared route wins**. The built-in answers only when routing
found nothing, so a program that writes its own `/metrics` keeps it.
Shadowing a declared route with a built-in would remove someone's endpoint
in a point release, and the symptom is a dashboard that goes blank.

4.0.4 `/healthz` touches no dependency. Liveness answers "should the
supervisor kill this process", and wiring a database check into it turns a
connection blip into a restart storm across every replica at once.

4.0.5 `/readyz` round-trips each configured dependency — Postgres always,
Redis **only when `JWC_REDIS_URL` is set**, so a deployment that never used
Redis does not start failing its probe because the runtime grew a driver.
The body names which one failed: a probe that only says "not ready" sends
the operator to the logs of a pod that is already out of rotation.

4.0.6 `/metrics` exposes gauges, not counters. `jwc_db_pool_size`,
`_available`, `_max_size` and `_waiting` (and the `jwc_redis_pool_*` set
when Redis is configured) are what a connection leak looks like from
outside: `available` pinned at zero while `waiting` climbs. Per-request
counters would need bookkeeping on the hot path and are what a real
metrics exporter is for.

---

## 5. Environment variables read by the runtime

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

## 6. Secrets never appear in output

`env()` values used as `secret`, `key`, `password`, `token` or `cursor_secret`
are redacted in `jwc config --print`, in logs, and in error messages. The
redaction list is by key name, and the check is on the **variable name**, not
the value.

---

## 7. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1201` | I/O or query inside `init()` |
| `E1202` | unknown `init()` key |
| `E1203` | more than one `database` |
| `E1204` | more than one `server` block |
| `E1205` | `page` used with no `cursor_secret` |
| `E1206` | unknown `server { }` key, or unknown key inside its `cors` / `tls` block |

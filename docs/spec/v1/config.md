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

Three sub-blocks: `cors { }` (§3.4), `tls { }` (§3.5) and `headers { }`
(§3.9).

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

3.4a `origins = ["*"]` together with `credentials = true` is `E1207`.

A browser refuses the literal pair — `Access-Control-Allow-Origin: *` is
invalid on a credentialed request — but a server that answers `*` by
*reflecting* the caller's origin satisfies the browser and defeats the
check. Reflecting is what a wildcard means here, so the pair is refused at
compile time: it would let any site on the internet read this API's
authenticated responses. List the origins.

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

3.9 **`headers { }` — the security headers on every response.**

```jwc
server {
    headers {
        hsts                    = "max-age=31536000; includeSubDomains";
        content_security_policy = "default-src 'none'";
        frame_options           = "SAMEORIGIN";
    }
}
```

| Key | Default | |
|---|---|---|
| `nosniff` | **`true`** | `X-Content-Type-Options: nosniff` |
| `frame_options` | **`"DENY"`** | `X-Frame-Options` |
| `referrer_policy` | **`"strict-origin-when-cross-origin"`** | `Referrer-Policy` |
| `hsts` | `""` | `Strict-Transport-Security` |
| `content_security_policy` | `""` | `Content-Security-Policy` |
| `permissions_policy` | `""` | `Permissions-Policy` |

An **empty string is how a default is turned off**, so there is no second
spelling for "do not send this one". An unknown key is `E1206`.

3.9.1 Three are on by default because there is no deployment they are wrong
for. `nosniff` stops a browser second-guessing a content type, and guessing
is never better. `X-Frame-Options: DENY` is clickjacking, and a program that
wants to be framed can say so where one that does not should not have had to
know the header exists. `Referrer-Policy` is already the current browser
default, so setting it changes nothing for them and fixes the older ones.

3.9.2 Three are **off** until asked for, because a wrong value is worse than
no value. An HSTS `max-age` sent by mistake pins the domain to HTTPS in every
browser that saw it, for that long, and cannot be withdrawn — that is not a
default anyone gets to choose for someone else's domain. There is no CSP
that is right for every page, and a default that breaks a page teaches
authors to delete the header rather than to write a policy. `Permissions-Policy`
is the same, and its feature list keeps moving.

3.9.3 The headers go on **every** answer, including the ones no response
builder made: a 413 refused before the chain, a preflight, a 404, a static
asset, a fault. A header the program set itself — `with { … }`,
`response.set_header` — **wins**: an author who wrote one meant that
response, and a default that overwrote it would be one that cannot be
escaped.

3.9.4 Both backends send the same set in the same order. `jwc build` bakes
the table by calling the same function `jwc serve` calls, so the two cannot
grow separate opinions about what is on.

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

4.0.3 A **declared route wins**, where declared means the path was written
down: a program whose source contains `routes "/metrics"` keeps it.
Shadowing that with a built-in would remove someone's endpoint in a point
release, and the symptom is a dashboard that goes blank.

A route that matches one of these names only through a **path parameter**
does not win — the built-in answers and the route does not run. `/{code}`
spans one segment and therefore spans `/readyz`, and a redirect service
that declares it has not written a readiness probe; it has written a
redirect. §4.0.2 promises an operator these three paths without reading
the source, and a pattern nobody aimed at them must not take that away.

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

## 5.1 The `.env` file

A `.env` in the project directory is read into the process environment
before anything reads a variable. `jwc build` binaries read one too: the
working directory first, then beside the executable and up to three levels
above it.

`KEY=VALUE` per line; a leading `export ` is tolerated; `#` begins a
comment only at the start of a line; a value may be wrapped in `'` or `"`
and nothing inside is interpreted — no `$VAR` expansion and no escapes. A
line that is not `KEY=VALUE` is reported on stderr rather than skipped.

**A variable already present in the environment is never overwritten.**
The file is a convenience for a developer's machine; a deployment that
exports its own configuration is unaffected by one that happens to be
lying in the directory.

## 5.2 `PG_*`

When neither `DATABASE_URL` nor `JWC_DATABASE_URL` is set, `PG_USER`,
`PG_PASSWORD`, `PG_HOST`, `PG_PORT` and `PG_DATABASE` are assembled into
one. All five are required.

Both are one implementation — `src/dotenv_core.rs.in`, which the CLI
includes and codegen pastes into the generated crate — because until
0.9.927 the native backend assembled `PG_*` and read `.env`, and the
interpreter did neither: the same file worked in a built binary and failed
under `jwc serve`.

## 6. Secrets never appear in output

`env()` values used as `secret`, `key`, `password`, `token` or `cursor_secret`
are redacted in `jwc config --print`, in logs, and in error messages. The
redaction list is by key name, and the check is on the **variable name**, not
the value.

---

## 6a. Runtime ceilings

Fixed numbers, not settings: a program that reaches one has a defect, and a
knob would only move where the defect appears. Both backends carry the same
values — `jwc build` emits the interpreter's constants rather than its own,
so a binary and `jwc serve` fail at the same place.

| | | |
|---|---|---|
| turns in one `while` | 10 000 000 | a loop whose condition never goes false; the error names the loop |
| JWC calls on one stack | 128 | recursion with no base case; the error names the function |
| nesting in one expression | 512 | four times the call ceiling, so a runaway recursion reports the call and not the nesting |
| turns between yields | 1024 | not a ceiling — see below |

### 6a.1 Why a loop yields

Everything a JWC loop body awaits is **ready**, and awaiting a ready future
does not hand the scheduler a turn. A loop that never finishes therefore
never returns `Pending`, and `request_timeout` — a `tokio::time::timeout`
around the handler — never gets to fire.

Measured before this was fixed: `request_timeout = "3s"` around
`while (true) { i += 1; }` did not fire at all, the client gave up at twenty
seconds, and the worker thread stayed at 100% after it had disconnected,
because nothing had cancelled the task. A handful of such requests is the
whole server.

Both loops yield every 1024 turns. `request_timeout` is a bound on
**compute**, not only on I/O, and it is accurate to well under a
millisecond.

### 6a.2 Why there is a call ceiling

A JWC call frame is a chain of boxed futures, and polling it costs the whole
chain's depth in machine stack. Without a ceiling the *stack* ran out first:
on tokio's default 2 MiB worker stack `jwc serve` answered a recursion 18
deep and died at 20 with `fatal runtime error: stack overflow, aborting` —
a process abort, which takes every other request in flight with it.

So the runtime gives its threads a 64 MiB stack (address space, committed as
touched) and 128 frames is what a program reaches first. Reaching it is a
fault: 500, the generic envelope, and the function named in the log beside
the request id.

`jwc build` boxes the calls in a cycle, which is also what makes a recursive
function compile at all — a generated `async fn` that calls itself is
`E0733` in rustc, so before this every program with a recursive function ran
under `jwc serve` and could not be built.

---

## 7. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1201` | I/O or query inside `init()` |
| `E1202` | unknown `init()` key |
| `E1203` | more than one `database` |
| `E1204` | more than one `server` block |
| `E1205` | `page` used with no `cursor_secret` |
| `E1206` | unknown `server { }` key, or unknown key inside its `cors` / `tls` / `headers` block |
| `E1207` | `cors { origins = ["*"] }` together with `credentials = true` |

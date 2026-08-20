# JWC

**Write web backends without hand-coding CRUD, without fighting an ORM,
native-fast.**

JWC is a small, Postgres-first backend language. Tables compile straight to
DDL, queries are part of the language and compile to SQL you can read, and
routes are declarations rather than a framework's callbacks. What you would
hand-write across a controller, a service, a repository, a request DTO, a
response DTO and a mapper is a table plus the handlers that use it.

```jwc
namespace notes;

database App : Postgres;
schema app of App;

table Notes of App.app {
    id         bigint primary key identity;
    title      varchar(200);
    body       text;
    created_at timestamptz default now();
}

class NoteInput {
    title varchar(200) required, minLength(1);
    body  text required;
}

service NoteService {
    function list() {
        return select N from App.app.Notes
            as { id, title, created_at }
            orderby created_at desc, id desc
            limit 50;
    }

    function create(req: NoteInput) {
        return insert into App.app.Notes { ...$req }
            as { id, title, body, created_at };
    }
}

routes "/notes" {
    route GET "" {
        return json(NoteService.list());
    }

    route POST "" {
        let req = request.body() as NoteInput;

        return created(json(NoteService.create($req)));
    }
}
```

`jwc gen-sql` turns the `table` into `CREATE TABLE`. `jwc explain` shows the
SQL each query compiles to. `jwc migrate new` writes the migration from one
schema to the next. `jwc serve` runs it.

---

## Status

**Pre-1.0, and the language changed.** v0.25.0 replaced the 0.9.x grammar
with the one specified in [`docs/spec/v1/`](docs/spec/v1/) and removed the
old front-end. `entity`, `dbcontext`, `with`, `via`, `validate body`,
`new … from`, `patch`, `group`, `mount` and `dome` are gone; the compiler
names their replacement rather than accepting them.

If you are running a 0.9.x binary, its documentation is archived under
[`docs/archive-0.9/`](docs/archive-0.9/). It describes a language this
compiler no longer compiles.

Every release through **v0.29.0** is in. What works today, against a real
Postgres:

| | |
|---|---|
| **Schema** | tables, views, enums, constraints, indexes, triggers → deterministic DDL |
| **Queries** | joins (`as one` / `as many` / `as group`), projections, aggregates, keyset pagination, `exists`, and `raw(…)` as the escape hatch |
| **Views** | real `CREATE VIEW`; a bounded page over one takes its keys first |
| **Routes** | path/query parameters, middleware chains, typed `context`, an error model checked at compile time |
| **Runtime** | an interpreter on hyper + tokio; every value is a bind parameter |
| **Migrations** | snapshot, diff, ten-phase emission, declared renames, `up` / `down` / `status` / `verify` |
| **Tests** | `test` blocks, each in its own transaction, rolled back whatever happens |
| **Packages** | `jwc login` / `publish` / `add`; a checksum from a separate request; what a package may declare is a closed list |
| **Config** | a `server { }` block: body limits, timeouts, CORS, trusted proxies, TLS, bind address |
| **Operations** | `/healthz`, `/readyz`, `/metrics` at fixed paths — no declaration needed |
| **Redis** | `redis.*` over a pooled driver behind the `redis` Cargo feature, with an atomic `rate_limit` |

[`ROADMAP.md`](ROADMAP.md) is the source of truth for what counts as done,
partial, and **non-goal**. Next is **v1.0.0-rc.1**: the conformance corpus
blocking in CI, an external review, and migrating a pilot application.

---

## Install

```bash
git clone https://github.com/just-web-code/jwc-lang
cd jwc-lang
cargo build --release
./target/release/jwc --help
```

Redis support is a Cargo feature, off by default, so the standard build
pulls in no Redis dependency:

```bash
cargo build --release --features redis
```

---

## CLI

| Command | What it does |
|---|---|
| `jwc check [path]` | parse, resolve names, type-check, and check the wiring |
| `jwc fmt [path] [--check]` | rewrite in canonical form; `--check` is the CI shape |
| `jwc gen-sql [path] [--explain]` | the schema as Postgres DDL, deterministic and offline |
| `jwc explain [path]` | every query the program issues, with its SQL |
| `jwc login --token jwc_…` | store a registry key in `~/.jwc/credentials.json` |
| `jwc publish [path]` | upload this package's manifest and sources to the registry |
| `jwc add <name>[@version]` | download a package, verify it, and record the dependency |
| `jwc test [path] [--filter s]` | run every `test` block — each in its own transaction, rolled back |
| `jwc lsp` | the language server, over stdio: diagnostics, hover-to-SQL, go-to-definition, completion, signature help |
| `jwc openapi [path] [--out f]` | an OpenAPI 3.1 document, derived from the route table and the typed signatures |
| `jwc lint [path] [--constraints]` | `check`, plus every constraint each route can reach and the status its violation produces |
| `jwc routes [path]` | the resolved route table: method, path, middleware chain |
| `jwc migrate new <name> [path]` | diff the sources against the last snapshot and write the next migration — offline |
| `jwc migrate up [path]` | apply every pending migration, in order, under an advisory lock |
| `jwc migrate down [path]` | roll back, newest first; refuses what it cannot undo |
| `jwc migrate status [path]` | applied, pending, and drift |
| `jwc migrate verify [path]` | every constraint and index present under the name the binary expects |
| `jwc serve [path] --port N` | run it. `--skip-schema-check` to boot without the live-schema check, `--dev` to enable `debug.dump` |
| `jwc ast [path]` | the parsed AST — a debugging aid, not a stable format |

An expected failure — a type error, a migration that cannot be reversed —
exits 1 with the message and its causes. It does not print a stack trace;
`RUST_BACKTRACE` governs panics, and a program you wrote being wrong is not
one.

### Environment

| Variable | Read by |
|---|---|
| `DATABASE_URL` / `JWC_DATABASE_URL` | the connection |
| `JWC_LOG_SQL=1` | logs every statement with its timing |
| `JWC_REDIS_URL` | the `redis` package surface; unset means `redis.enabled()` is false |

Everything else a program needs it asks for itself, through `env()` inside
`init()` or a `server { }` key — the environment is not a second
configuration surface.

---

## Running it

```jwc
server {
    max_body_bytes  = 1048576;
    request_timeout = "30s";
    header_timeout  = "10s";
    bind            = "0.0.0.0";
    trusted_proxies = ["10.0.0.0/8"];

    cors { origins = ["https://app.example.com"]; }

    tls { cert = env("TLS_CERT_PATH"); key = env("TLS_KEY_PATH"); }
}
```

Every key is enforced or the server refuses to boot — there is no third
option. A `tls { }` whose paths do not resolve stops the process rather than
falling back to plain HTTP, because that fallback is the one
misconfiguration nothing outside the process can see: the listener answers
either way, and every byte is in the clear. A misspelled key is `E1206`, for
the same reason: `trusted_proxie` leaves the proxy list empty, and a rate
limiter keyed on `client_ip()` then collapses into one shared bucket.

Three paths are served without being declared:

| Path | Answers | Touches |
|---|---|---|
| `/healthz` | `200` | nothing — liveness must not restart a pod over a database blip |
| `/readyz` | `200`, or `503` naming what failed | every configured dependency |
| `/metrics` | Prometheus text | nothing |

A route you declare at one of those paths wins. `/metrics` carries the pool
gauges: `jwc_db_pool_available` at zero while `jwc_db_pool_waiting` climbs
is what a leaked connection looks like from outside, and it is a shape RSS
cannot show.

---

## The specification

[`docs/spec/v1/`](docs/spec/v1/) is normative — grammar, name resolution,
the type lattice, schema emission, queries, writes, routing, middleware, the
error model, migrations, builtins, packages, testing, tooling, security and
configuration. Where this README and the spec disagree, the spec is right.

[`docs/spec/v1/sample/`](docs/spec/v1/sample/) is a complete application in
the language: 13 tables, 5 views, 26 endpoints, authentication, billing and
a webhook. It is what the compiler is tested against, and
`spec-coverage.json` maps every construct it uses to the clause that defines
it — checked by a test, so it cannot drift quietly.

---

## Two properties worth knowing

**Every value is a bind parameter.** Nothing is interpolated, ever.
Parameters are bound as text and cast in SQL — `($1::text)::bigint`, never
`$1::bigint` — so one binding path covers every type and there is no
position in an emitted statement a caller's string can reach.

**A query result is raw until you project it.** `select N from …` with no
`as { }` produces one JSON value that Postgres builds and the runtime never
parses; it goes to the response as text. Adding `as { … }` opts into a
record whose fields you can read. `jwc explain` prints which of the two each
query is, so the promise is checkable rather than assumed.

---

## Building

```bash
cargo build                                        # debug
cargo test                                         # everything that needs no server
cargo clippy --workspace --all-targets -- -D warnings
```

Several suites are opt-in on a real dependency, and **a SKIPPED line is not
a pass** — a suite that skips has verified nothing:

```bash
export JWC_V1_DATABASE_URL=postgres://…            # a database it may drop schemas in
export JWC_V1_PG=postgres://…                      # same server, for the psql-driven goldens
export JWC_TEST_REDIS_URL=redis://127.0.0.1:6379   # flushed between tests — never a real cache
export CURSOR_SECRET=ci-cursor-secret

cargo test --features redis --test http_golden --test hardening
cargo test --test migrate_apply --test migrate_golden --test migrate_roundtrip
cargo test --test jwc_test --test sql_golden --test ddl_golden --test raw_hatch
cargo test --features redis --test integration_redis
cargo test --test serve_listener                   # needs a socket and openssl
```

`.github/workflows/ci.yml` runs all of them with the services attached, and
`hardening.rs::every_test_suite_is_named_in_ci` fails if a suite exists that
no job names — the omission it exists to catch had left seven suites
running nowhere.

The soak harness is [`soak/`](soak/): cycles of sustained load with a
graceful restart between each, recording RSS and the pool gauges, and
`analyze.py` renders PASS/FAIL against the exit criteria.

---

## Licence

**Undecided, deliberately.** There is no `LICENSE` file: `Cargo.toml` and
`deny.toml` both record the crate as workspace-private until a licence
decision lands, and it is `publish = false`. Until that changes, no
licence is granted — assume all rights reserved rather than inferring one
from a sibling component.

The VS Code extension under [`vscode-extension/`](vscode-extension/)
ships its own MIT `LICENSE`, and covers only itself.

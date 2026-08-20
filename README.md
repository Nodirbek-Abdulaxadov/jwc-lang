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

What works today, against a real Postgres:

| | |
|---|---|
| **Schema** | tables, views, enums, constraints, indexes, triggers → deterministic DDL |
| **Queries** | joins (`as one` / `as many` / `as group`), projections, aggregates, keyset pagination, `exists` |
| **Views** | real `CREATE VIEW`; a bounded page over one takes its keys first |
| **Routes** | path/query parameters, middleware chains, typed `context`, an error model checked at compile time |
| **Runtime** | an interpreter on axum + tokio; every value is a bind parameter |
| **Migrations** | snapshot, diff, ten-phase emission, declared renames, `up` / `down` / `status` / `verify` |
| **Tooling** | `explain` per route or function, `lint --constraints`, OpenAPI 3.1, a language server |

Not yet: the test framework and packages (v0.28.0), hardening (v0.29.0). See [`ROADMAP.md`](ROADMAP.md) — it is the
source of truth for what counts as done, partial, and **non-goal**.

---

## Install

```bash
git clone https://github.com/just-web-code/jwc-lang
cd jwc-lang
cargo build --release
./target/release/jwc --help
```

---

## CLI

| Command | What it does |
|---|---|
| `jwc check [path]` | parse, resolve names, type-check, and check the wiring |
| `jwc fmt [path] [--check]` | rewrite in canonical form; `--check` is the CI shape |
| `jwc gen-sql [path] [--explain]` | the schema as Postgres DDL, deterministic and offline |
| `jwc explain [path]` | every query the program issues, with its SQL |
| `jwc lsp` | the language server, over stdio: diagnostics, hover-to-SQL, go-to-definition, completion, signature help |
| `jwc openapi [path] [--out f]` | an OpenAPI 3.1 document, derived from the route table and the typed signatures |
| `jwc lint [path] [--constraints]` | `check`, plus every constraint each route can reach and the status its violation produces |
| `jwc routes [path]` | the resolved route table: method, path, middleware chain |
| `jwc migrate new <name> [path]` | diff the sources against the last snapshot and write the next migration — offline |
| `jwc migrate up [path]` | apply every pending migration, in order, under an advisory lock |
| `jwc migrate down [path]` | roll back, newest first; refuses what it cannot undo |
| `jwc migrate status [path]` | applied, pending, and drift |
| `jwc migrate verify [path]` | every constraint and index present under the name the binary expects |
| `jwc serve [path] --port N` | run it |
| `jwc ast [path]` | the parsed AST — a debugging aid, not a stable format |

`DATABASE_URL` points the runtime at Postgres. `JWC_LOG_SQL=1` logs every
statement.

---

## The specification

[`docs/spec/v1/`](docs/spec/v1/) is normative — grammar, name resolution,
the type lattice, schema emission, queries, writes, routing, middleware, the
error model, migrations, builtins and configuration. Where this README and
the spec disagree, the spec is right.

[`docs/spec/v1/sample/`](docs/spec/v1/sample/) is a complete application in
the language: 13 tables, 5 views, 25 endpoints, authentication, billing and
a webhook. It is what the compiler is tested against.

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
cargo build                 # debug
cargo test                  # the suite
cargo clippy --all-targets  # lints
```

Some suites need a Postgres and are opt-in:

```bash
JWC_V1_DATABASE_URL=postgres://…  cargo test --test http_golden
JWC_V1_PG='-h 127.0.0.1 -p 5432 -U postgres' cargo test --test sql_golden
```

They print `SKIPPED` without it. **A SKIPPED line is not a pass.**

---

## Licence

See [LICENSE](LICENSE).

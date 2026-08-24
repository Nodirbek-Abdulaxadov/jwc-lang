---
slug: /
sidebar_position: 1
sidebar_label: Introduction
title: "A backend-first language for SQL-native APIs"
description: "JWC is a Postgres-first backend language: tables compile straight to SQL and queries are part of the language, so there is no ORM, no DTO mapping and no repository boilerplate."
---

# JWC

<p align="center">
  <img src="/img/logo.png" alt="JWC" width="150" />
</p>

**Write web backends without hand-coding CRUD and without fighting an ORM.**

JWC is a small, Postgres-first backend language. Tables compile straight to
SQL and queries are part of the language, so there is no ORM layer, no DTO
mapping and no repository boilerplate. What you would hand-write across a
controller, a service, a repository, a request DTO, a response DTO and a
mapper profile is a table plus the handlers that use it.

```jwc
namespace app;

database App : Postgres;
schema notes of App;

table Notes of App.notes {
    id         bigint primary key identity;
    title      varchar(200);
    body       text;
    created_at timestamptz default now();
}

class NewNote {
    title varchar(200) required, minLength(1);
    body  text?;
}

routes "/notes" {
    route GET "" {
        return json(select N from App.notes.Notes
            as { id, title, body, created_at }
            orderby created_at desc
            limit 100);
    }

    route POST "" {
        let req = request.body() as NewNote;

        return created(json(insert into App.notes.Notes { ...$req }
            as { id, title, body, created_at }));
    }
}

function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

`jwc serve` boots a Postgres-backed HTTP server for those routes, with
validation and JSON in and out handled for you. `jwc migrate new` diffs the
tables against the last snapshot and writes the DDL.

## What the code above is doing

Five things are worth naming, because each one replaces a layer you would
otherwise write and maintain:

**The table is the schema.** `table Notes of App.notes` is the DDL. There is
no separate migration language and no model-to-table mapping to keep in
sync — `jwc migrate new` diffs the declaration against the last snapshot.

**`class` is the request boundary.** `request.body() as NewNote` parses,
validates and whitelists in one step. A key the class does not declare is
dropped, so a caller cannot set a column by naming it. A failure is a 400
carrying every field error at once, not the first one.

**`as { … }` is the response boundary.** A query with no projection returns
an opaque value the compiler will not let you read a field of. Naming the
fields is what makes them readable — and a column marked `private` can never
appear in one.

**Queries are checked against the schema.** A typo in a column name, a
comparison between mismatched types, or an aggregate without its `group by`
is a compile error with a line number, not a 500 at three in the morning.

**Errors have one shape.** `throw NotFound("…")` is 404 with a JSON envelope;
so is a unique-constraint violation, and it carries the message you wrote on
the constraint. Routes do not build error responses by hand.

## Why JWC

- **No ORM, no mapping.** Tables compile to SQL directly. No change tracker,
  no lazy loading, no repository pattern, no DTO duplication.
- **Postgres-honest.** Every statement the compiler emits is plain SQL you
  can read with `jwc explain`. Parameters are bound, never interpolated.
- **The unsafe things are hard to reach.** `private` columns cannot leave the
  process, raw SQL is one named escape hatch that `jwc explain` counts, and a
  keyset cursor is signed because it is a predicate the client hands back.
- **One language.** Routes, tables, queries, validation, auth, migrations.

## Where JWC fits — and where it does not

**Fits well**

- CRUD-heavy services: admin backends, internal tools, line-of-business APIs.
- Postgres-only stacks where you already write SQL by hand or use a thin
  layer like sqlc or PostgREST.
- Teams that want one engineer to ship a service end to end.

**Does not fit, by design**

- Rich-domain code with deep object graphs, polymorphism or change-tracking
  semantics.
- Multi-database portability. Postgres is the only driver and that is a
  deliberate non-goal.
- Anything needing a large package ecosystem.

## What is here

| Section | What it covers |
|---|---|
| [Getting started](./getting-started/install) | Install, a first project, the layout, editor setup |
| [Tutorial](./tutorial/) | A link bin: four endpoints, a real database |
| [Language](./language/syntax) | Vocabulary, types, control flow, functions |
| [Data](./data/schema) | Tables, queries, writes, migrations |
| [Backend](./backend/routing) | Routes, middleware, errors, validation, `server { }` |
| [Standard library](./stdlib/builtins) | Every built-in, by group |
| [Packages](./packages/index.md) | The manifest, imports, publishing |
| [CLI](./cli/index.md) | Every `jwc` subcommand |
| [Deployment](./deployment/index.md) | Docker, the native build, probes |
| [Security](./security) | What the language enforces, and what it does not |
| [Reference](./reference/removed) | The 0.9.x → 1.0 map, and what 1.0 does not have |

## Status

The 1.0 vocabulary is what this compiler implements. The 0.9.x language —
`dbcontext`, `entity`, `dome`, `validate body` — was removed at the cutover
and does not compile; every removed keyword has a diagnostic naming its
replacement. If you are reading code written against it, see
[what changed](./reference/removed).

The normative specification lives in
[`docs/spec/v1/`](https://github.com/just-web-code/jwc-lang/tree/main/docs/spec/v1)
in the repository. Where this site and the spec disagree, the spec is right —
tell us, because it means a page here is wrong.

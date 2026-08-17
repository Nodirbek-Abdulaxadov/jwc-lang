---
slug: /
sidebar_position: 1
sidebar_label: Introduction
title: "A backend-first language for SQL-native APIs"
description: "JWC is a Postgres-first backend language: entities compile straight to SQL and queries are part of the language, so there is no ORM, no DTO mapping and no repository boilerplate."
---

# JWC

<p align="center">
  <img src="/img/logo.png" alt="JWC" width="150" />
</p>

**Write web backends without hand-coding CRUD, without fighting an ORM, native-fast.**

JWC is a small, Postgres-first backend language. Entities compile straight
to SQL and queries are part of the language, so there's no ORM layer, no
DTO mapping, and no repository boilerplate to maintain. What you'd
hand-write across a controller, a service, a repository, a request DTO, a
response DTO, and an AutoMapper profile is an entity plus the handlers
that use it.

```jwc
dbcontext AppDb: Postgres;

entity Note of AppDb {
    id         int pk autoincrement;
    title      varchar(200);
    body       text;
    created_at datetime;
}

route GET "/notes" {
    return json(select Note from AppDb.Note orderby Note.created_at desc);
}

route POST "/notes" {
    validate body {
        title: required, minLength(1), maxLength(200);
    }
    let req = body();
    let n = new Note();
    n.title      = req.title;
    n.body       = req.body;
    n.created_at = now();
    insert n into AppDb.Note;
    return created(json(n));
}
```

`jwc run` boots a Postgres-backed HTTP server serving those routes, with
validation and JSON in/out handled for you. `jwc migrate new` diffs the
entities against the last migration and writes the DDL; `jwc build
--native` produces a single static binary.

Routes are written, not generated — `jwc new --template api` scaffolds a
working CRUD set you can edit.

## Why JWC

- **No ORM, no mapping** — entities compile to SQL directly. No
  EF/Hibernate change-tracker, no AutoMapper, no repository pattern, no
  DTO duplication.
- **Postgres-honest** — every query the compiler emits is plain SQL you
  can read in `jwc gen-sql`. No N+1, no lazy-loading surprises, no
  hidden fetch plans.
- **One language, one binary** — routes, entities, validation, JWT
  auth, background jobs, migrations. `jwc build --native` → one static
  binary.
- **Fast enough** — close to `rust-axum`, ahead of `go-fiber` on JSON
  paths. Performance is a *consequence* of the design, not the headline.

## Where JWC fits — and where it doesn't

**Fits well:**

- CRUD-heavy services (admin backends, internal tools, line-of-business
  APIs, prototype-to-prod webapps).
- Postgres-only stacks where you already write SQL by hand or use a
  thin layer like Dapper / sqlc / PostgREST.
- Teams that want one engineer to ship a service end-to-end without
  juggling a half-dozen frameworks.

**Does NOT fit (by design):**

- Rich-domain code with deep object graphs, polymorphism, or
  change-tracking semantics (EF Core / Hibernate territory).
- Multi-database portability — Postgres is the only supported driver
  and that's a deliberate non-goal until 1.0.
- Performance-critical hot paths where the last 10% of an axum number
  matters — use axum.
- Anything that needs a mature ecosystem (1000s of packages). JWC's
  package count is small and curated.

## Building with an AI agent?

Hand it [**AI agent guide**](./reference/ai-agent-guide) — the whole language
in one self-contained file: every declaration, statement and built-in, the
native-build rules, and the mistakes agents actually make. Nothing else needs
to be in its context.

## What's here

| Section | What it covers |
|---|---|
| [AI agent guide](./reference/ai-agent-guide) | The whole language in one file, written to be pasted into an agent's context |
| [Getting started](./getting-started/install) | Install, first project, templates, editor setup |
| [Tutorial — zero to deployed CRUD](./tutorial/zero-to-crud) | 15-minute end-to-end walkthrough |
| [Language](./language/syntax) | Types, variables, functions, control flow, async |
| [Data](./data/dbcontext) | Entities, select / insert / update / delete, migrations, transactions |
| [Backend](./backend/routes) | Routes, middleware, validation, error handler, websockets, background queue |
| [Standard library](./stdlib/strings) | String, array, JSON, HTTP, JWT, hashing |
| [CLI](./cli/overview) | Every `jwc` subcommand + flag |
| [Deployment](./deployment/native-build) | Native AOT, [Docker](./deployment/docker), musl static, k8s migrate init-container |
| [Reference](./reference/builtins) | Built-ins reference + numbered diagnostic codes (`Wxxx` / `Exxx`) |
| [Security](./security/) | SSRF allowlist, JWT validation, secrets redaction, trusted-proxy chain |

## Status

Production-ready for the maintainer's own workload; external pilots TBD.

- **Interpreter** — stable (`jwc run`, `jwc serve`).
- **Native AOT** — stable for the documented surface
  ([`aot-scope`](https://github.com/just-web-code/jwc-lang/blob/main/docs/spec/aot-scope.md)).
- **Query layer (join + projection + eager-load + grouped aggregation +
  optional/dynamic filters)** — ✅ shipped (Phase 11, v0.6.x). Native AOT
  mirrors the query surface; `jwt_sign`/`jwt_verify` + a couple of query forms
  stay interpreter-only natively. See
  [`ROADMAP.md`](https://github.com/just-web-code/jwc-lang/blob/main/ROADMAP.md).
- **LLVM IR backend, cross-target native matrix, WASM, self-hosting,
  multi-database driver, SSE v2** — **declared Non-goals** (see ROADMAP
  Non-goals section).

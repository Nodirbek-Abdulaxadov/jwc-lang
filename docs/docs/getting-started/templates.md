---
sidebar_position: 5
title: Templates
description: "The four starter trees jwc new can scaffold: empty, api, auth and jobs — what each contains and which to pick."
---

# Templates

`jwc new` scaffolds one of four trees. They are not samples to read and
throw away: each one checks, migrates, runs, and is meant to be edited in
place.

```bash
jwc new myapp                      # empty (the default)
jwc new myapp --template api
jwc new myapp --template auth
jwc new myapp --template jobs
jwc new myapp --template api --path ./services/notes   # where to put it
```

Every template writes the same five things beside the sources:
`jwcproj.json`, `.env.example`, `.gitignore`, `README.md`, and `src/`.

| Template | Tables | Routes | Pick it when |
|---|---|---|---|
| `empty` | none | 1 | you want the smallest thing that runs |
| `api` | 1 | 5 | you are building CRUD over a table |
| `auth` | 2 | 4 | you need accounts, passwords and sessions |
| `jobs` | 2 | 2 | you have work that must not happen in a request |

The first four commands are the same in all four:

```bash
cp .env.example .env      # then point DATABASE_URL at a database
jwc check                 # types, schema, routes — offline, no database
jwc migrate new init      # turn the schema into DDL
jwc migrate up            # apply it
jwc serve                 # run
```

`jwc check` needs no database and no network: the schema is in the source,
so the queries are checked against it without connecting to anything.

---

## `empty` — one file

```
src/app.jwc
```

A `database`, a `schema`, a `server { }` block, one route and `main()`.
No tables, so `jwc migrate new init` produces an empty migration and
`jwc serve` answers immediately.

This is the one to start from when you know what you are building. The
other three are `empty` plus a feature.

## `api` — CRUD over one table

```
src/app.jwc          the database, its schemas, server { }, main()
src/db/notes.jwc     the table
src/dto/notes.jwc    the request bodies, with their validation
src/services/notes.jwc   the queries
src/routes/notes.jwc     the five routes
```

```
GET    /api/v1/notes           list, keyset-paginated  (?cursor=…)
POST   /api/v1/notes           create
GET    /api/v1/notes/{id}      read
PATCH  /api/v1/notes/{id}      partial update
DELETE /api/v1/notes/{id}      delete
```

The list route is **keyset-paginated**, not offset-paginated: the order is
total (`created_at desc, id desc`), there is an index that matches it, and
the cursor is signed. `CURSOR_SECRET` in `.env` is what signs it — a
cursor is a position in someone else's data, and an unsigned one is an
invitation to edit it.

The layout is the one the other templates use too, and it is worth
copying: `db/` declares, `dto/` validates, `services/` queries, `routes/`
does nothing but call a service and return what it answered.

## `auth` — accounts, passwords, sessions

```
src/db/auth.jwc          accounts + sessions
src/dto/auth.jwc         register / login / update bodies
src/middleware/auth.jwc  the Bearer check
src/services/auth.jwc    register, login, me
src/routes/auth.jwc      the four routes
```

```
POST  /api/v1/auth/register    { email, password, display_name } -> 201
POST  /api/v1/auth/login       { email, password }               -> { token, expires_in }
GET   /api/v1/me               Authorization: Bearer <token>
PATCH /api/v1/me               { display_name }
```

Three things in it are the reason to read it before writing your own:

- **Passwords are Argon2id** (`hash.password` / `hash.verify`), and the
  column is `private`, so it cannot reach a response through a projection
  that does not name it.
- **Login is constant-work.** An unknown address is verified against a
  dummy hash rather than returning early, so the two failure branches cost
  the same and the timing does not say which accounts exist.
- **The middleware throws.** It does not return a response; it raises
  `Unauthorized`, and the error model turns that into a 401 once, in one
  place.

Set `JWT_SECRET` in `.env`. The template will not start without one.

## `jobs` — work outside the request

```
src/db/work.jwc       the deliveries table
src/jobs/deliver.jwc  the job
src/routes/work.jwc   dispatch it, and read what it wrote
```

```
POST /api/v1/deliveries   { recipient, subject } -> 202, and a queued job
GET  /api/v1/deliveries   what the job has written
```

```jwc no-compile
job Deliver(recipient: text, subject: text) retries 5 backoff "30s" { … }
```

`jwc serve` runs the HTTP server **and** the workers in one process. The
queue is Postgres-backed (`_jwc_jobs`, `_jwc_jobs_dead`, created at boot),
so a restart does not lose queued work.

Delivery is **at-least-once**: a worker that dies mid-job loses its lease
and the job comes back. The template's job body is written to tolerate
running twice, and yours has to be too.

---

## What `--template` does not do

It picks a starting tree, not a mode. Nothing in a project remembers which
template made it, and there is no command that adds a template's feature to
an existing project — `auth` on top of `api` is two files copied by hand,
which is the honest amount of work it is.

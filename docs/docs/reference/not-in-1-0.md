---
sidebar_position: 2
title: "What 1.0 does not have"
description: "Server-Sent Events, sequences, generated columns and a module system are not in 1.0. What exists, what does not, and what to do instead."
---

# What 1.0 does not have

This page exists so a decision to depend on JWC can be made with the whole
picture. Everything below is a dated omission, not a non-goal, and each row
says what 1.0 does instead.

:::note

Earlier versions of this page listed background jobs, WebSocket, an
in-process cache and outbound email as absent. They were, when it was
written; they were implemented across 0.9.902–0.9.910 and the page did not
keep up. If you were told JWC could not do one of these, check the table
below rather than your memory of it.

:::

## Not declarable

| | What to do instead |
|---|---|
| **Server-Sent Events** | a [`socket`](../backend/sockets), or long-polling. 0.9 parsed `route SSE "…"` end to end and dispatched it to a stub, so a program could declare one, pass every check and serve nothing — which is worse than not having it |
| **Sequences as a declared object** | a counter table plus `update … first` |
| **Generated columns** | compute in application code |
| **`jwc upgrade`** (a 0.9 → 1.0 codemod) | the diagnostics. Every removed construct has one that names its replacement — see [what 0.9.x had](./removed) |

## What it does have

The four things this page used to list as missing:

| | |
|---|---|
| **Background jobs** | `job Name(args) retries N backoff "30s" { … }` and `dispatch Name(…)`, over a durable Postgres queue with retries and a dead-letter table — [jobs](../backend/jobs) |
| **WebSocket** | `socket "path" { on open / on message (m) / on close }` — [sockets](../backend/sockets) |
| **In-process cache** | `cache.get` / `cache.set` / `cache.del`, bounded with eviction and `/metrics` counters. Behind more than one replica use `redis.*` instead: each replica has its own in-process cache, so a rate limiter built on one admits N times the limit |
| **Outbound email** | `mail.send(to, subject, html)` over SMTP, configured with `JWC_SMTP_*`. It raises when unconfigured rather than pretending to send |

Also present, and sometimes assumed missing: `redis.*`, `insert … buffered`
for writes a request should not wait for, and the native AOT backend
(`jwc build`), which produces a single binary that answers byte-identically
to `jwc serve`.

## Deferred inside the language

| | 1.0's answer |
|---|---|
| navigating into a `jsonb` value | a `jsonb` column reads as `Raw` — it splices into a response and cannot be read field-wise |
| an aggregate and an `as many` collection in one query | `E0532`, with the two-query rewrite printed |
| subqueries, CTEs, window functions, full-text | `where exists` / `not exists`, and the `raw(…)` escape hatch for the rest |
| a multi-row `insert` statement | `for (x in xs) { insert into … }` inside a `transaction`. (`insert … buffered` does batch rows into one multi-row statement, but that is the writer's doing, not something the language exposes) |
| a real module and visibility system | a flat declaration space; `import` is a checked dependency declaration that does not scope |
| typed client SDKs | `jwc openapi` |

The full list, with the reasoning for each, is
[`DEFERRED.md`](https://github.com/just-web-code/jwc-lang/blob/main/docs/spec/v1/DEFERRED.md).

## Not planned

- **A second database backend.** JWC is Postgres-first, and that is what
  makes the query language able to mean one thing.
- **An ORM, a repository layer, DTO mapping.** They are what the
  language exists to remove.
- **A general-purpose language.** JWC writes HTTP backends over Postgres.
  A program that needs more than that should call out to something that
  does more.

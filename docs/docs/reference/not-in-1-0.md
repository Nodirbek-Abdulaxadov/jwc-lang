---
sidebar_position: 2
title: "What 1.0 does not have"
description: "Background jobs, WebSocket, SSE and an in-process cache are not in the 1.0 vocabulary. What exists, what was deleted, and what to do instead."
---

# What 1.0 does not have

This page exists so a decision to depend on JWC can be made with the
whole picture. Everything below is a dated omission, not a non-goal, and
each row says what 1.0 does instead.

## Not declarable

| | What to do instead |
|---|---|
| **Background jobs, a durable queue, a DLQ** | a `jobs` table, a worker process that claims with `update … first` (which always emits `FOR UPDATE`), and whatever schedules it |
| **WebSocket, SSE** | a separate service. The vocabulary has no way to declare one, so there is nothing to route |
| **An in-process cache** | `redis.*` — and behind more than one replica an in-process cache is the wrong answer anyway, because each replica has its own |
| **Outbound email and other provider I/O** | a package: `import mail; mail.send(…)`. Provider shape is not language shape |
| **Sequences as a declared object** | a counter table plus `update … first` |
| **Generated columns** | compute in application code |

The reason for the first two is the same: `design.md` never covered
them, and guessing a vocabulary means writing it twice. They are tracked
as `DEFERRED-16`.

## Where the runtime code stands

0.9.x had some of these. The v0.25.0 cutover deleted 73 source files
along with the 0.9.x front-end, and they did not all come back:

| | |
|---|---|
| durable queue, DLQ, `dispatch` | **deleted**. 1,352 lines, recoverable from git history and nowhere else |
| WebSocket / SSE | runtime restored, unreachable — nothing can declare one |
| in-process cache | runtime restored, unreachable — `cache.*` is not a built-in |
| native AOT backend | **restored** and covered; `jwc build` is it |

The specification said "the 0.9.x runtime code is retained but
unreachable" until 0.9.903. For the queue that was not true, and this
table is the correction.

## Deferred inside the language

| | 1.0's answer |
|---|---|
| navigating into a `jsonb` value | a `jsonb` column reads as `Raw` — it splices into a response and cannot be read field-wise |
| an aggregate and an `as many` collection in one query | `E0532`, with the two-query rewrite printed |
| subqueries, CTEs, window functions, full-text | `where exists` / `not exists`, and the `raw(…)` escape hatch for the rest |
| multi-row `insert` | `for (x in xs) { insert into … }` inside a `transaction` |
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

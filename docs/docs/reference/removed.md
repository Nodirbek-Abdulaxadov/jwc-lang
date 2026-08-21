---
sidebar_position: 1
title: "What 0.9.x had that 1.0 does not"
description: "The vocabulary the 1.0 cutover replaced, what each construct became, and the diagnostic that says so."
---

# From 0.9.x to 1.0

The 1.0 vocabulary replaced most of the 0.9.x one. Nothing here is a
deprecation with a grace period: 0.9.x had no users outside this
repository, so the constructs were removed and each one has a diagnostic
that names its replacement.

Running `jwc check` on 0.9.x source is the fastest way to see it — the
compiler says what each construct became.

## Declarations

| 0.9.x | 1.0 |
|---|---|
| `dbcontext AppDb : Postgres;` | `database App : Postgres;` + `schema s of App;` |
| `entity User of AppDb { … }` | `table Users of App.s { … }` |
| `dome UserService { … }` | `service UserService { … }` |
| `route GET "/x" { … }` at top level | `routes "/x" { route GET "" { … } }` |
| `class R { email string; }` | `class R { email varchar(255) required; }` |

## Columns

| 0.9.x | 1.0 |
|---|---|
| `id int pk autoincrement` | `id int primary key identity` |
| `createdAt datetime` | `created_at timestamptz` |
| `payload json` | `payload jsonb` |
| `dueDate datetime nullable` | `due_date timestamptz?` |
| `camelCase` names | `snake_case`, with `as "camelCase"` to keep the physical name |

## Queries and writes

| 0.9.x | 1.0 |
|---|---|
| `select User from AppDb.User where User.id == @id first` | `select U from App.s.Users where id == $id as { … } first` |
| `select X with rel from …` | an explicit `join … as one` / `as many` |
| `new X(); x.f = v; insert x into AppDb.X` | `insert into App.s.X { f = $v } as { … }` |
| `update x in AppDb.X` | `update App.s.X set … where …` |
| `limit @n offset @off` | `page after $cursor size $n` |
| `select count(*)` | `as { total: count(id) }` |

## Routes and handlers

| 0.9.x | 1.0 |
|---|---|
| `validate body { … }` | rules on the `class`, applied by `request.body() as C` |
| `body()` | `request.body() as C` |
| `path_param("id")` | `@id`, with `{id: bigint}` in the pattern |
| `query_param("q")` | `request.query("q")` |
| `context("userId")` / `setContext(…)` | `context.user_id` |
| `header(name)` | `request.header(name)` |
| `try { } catch (e) { }` | `or throw`, postfix `catch`, `errorHandler` |
| `hash_password`, `jwt_sign`, `now()` | `hash.password`, `jwt.sign`, `date.now()` |

## Comments

`//` is not a comment in 1.0. Use `--` for a line comment and `---` for a
doc comment.

## Still deferred

These are specified and not yet implemented. They are tracked in
[`DEFERRED.md`](https://github.com/just-web-code/jwc-lang/blob/main/docs/spec/v1/DEFERRED.md),
and the compiler names them when it meets one rather than accepting them
quietly:

- navigation into a `jsonb` value (`DEFERRED-6`)
- an aggregate and an `as many` collection in one query (`DEFERRED-12`)

## The parts that came back

The v0.25.0 cutover removed the native AOT backend along with the 0.9.x
front-end. It was restored in 0.9.901–0.9.903 and now covers the
language: `jwc build` produces a single binary, and it is checked against
`jwc serve` by running the same requests against both and comparing the
responses byte for byte.

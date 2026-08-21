---
sidebar_position: 3
title: Project structure
description: "How a JWC project is laid out, how namespaces map to paths, and what the manifest carries."
---

# Project structure

A project is a `jwcproj.json` and a tree of `.jwc` files. Every file in the
tree is compiled together; there is no build order to declare.

## The manifest

```json
{
  "name": "shop",
  "type": "app",
  "version": "0.1.0",
  "entry": "src/app.jwc",
  "dependencies": {
    "redis": "^0.2.0"
  }
}
```

- `type` is `"app"` (deployed) or `"pkg"` (imported). Anything else is read
  as an app, because the package rules only *restrict*.
- `dependencies` keys are what `import <name>;` can resolve to.

## Namespaces follow the path

A file's `namespace` must match its location, with `src/` stripped:

| File | Namespace |
|---|---|
| `src/app.jwc` | `app` |
| `src/db/auth.jwc` | `db.auth` |
| `src/routes/orgs.jwc` | `routes.orgs` |

A mismatch is `W0102`. This is the whole rule — there is no module
resolution beyond it.

## Reaching across namespaces

`import` brings a namespace or a package into scope:

```jwc no-compile
namespace routes.orgs;

import dto.org;              -- a namespace in this program
import middleware.auth;
import services.org;
import redis;                -- a dependency in the manifest
```

A name that is both a local namespace and a dependency is `E0203` — rename
one, because there is no precedence rule. An import nothing uses is `W0103`.

## A layout that scales

The specification's own sample uses this shape, and so does every service in
the ecosystem:

```
jwcproj.json
src/
  app.jwc              database, schemas, server { }, errors, main()
  db/                  tables, one file per schema
  dto/                 classes — the request boundary
  middleware/          auth, rate limiting, audit
  services/            the logic: one service per aggregate
  routes/              thin — parse, call a service, respond
  views/               database views
tests/
  *_test.jwc           `jwc test`
migrations/
  0001_init.up.sql     generated; read before applying
```

Nothing enforces this. It holds up because of two rules that *are* enforced:
a package may not declare `routes` (`E1502`), and a route body that grows
logic has nowhere to put it except a service.

## Where things may be declared

| Declaration | App | Package |
|---|---|---|
| `service`, `middleware`, `class`, `error`, `function`, `test` | yes | yes |
| `enum` without `of` | yes | yes |
| `database`, `schema`, `table`, `view`, `enum … of` | yes | **no** (`E1501`) |
| `routes`, `errorHandler` | yes | **no** (`E1502`) |

A package that declared a table would bring DDL with it, so installing a
dependency would mean applying someone else's schema change to your
database. There is no version of that which is safe.

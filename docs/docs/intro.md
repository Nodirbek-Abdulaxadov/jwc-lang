---
slug: /
sidebar_position: 1
title: Introduction
---

# JWC

**JWC** (Just Web Code) is a small backend-first language for building API +
database applications. The compiler is written in Rust and ships a single CLI
that parses, validates, runs, and bundles your project.

```jwc title="users-api.jwc"
dbcontext AppDb : Postgres;

entity User of AppDb {
    id uuid pk;
    email varchar(120);
    name varchar(60);
}

route POST "users" {
    validate body {
        email: required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
        name:  required, minLength(2), maxLength(60);
    }
    let req = body();
    let u = new User();
    u.id = uuid();
    u.email = req.email;
    u.name = req.name;
    insert u into AppDb.User;
    return created(u);
}

route GET "users/{id}" {
    let u = select User from AppDb.User where User.id == @id first;
    if (u == null) { return notFound(); }
    return json(u);
}
```

The above is a working CRUD endpoint, complete with:

- Compile-time entity / column / FK validation
- Parameterized SQL (no injection)
- Body validation (regex via raw strings)
- Typed JSON response handling

## Why JWC

- **SQL is a first-class statement**, not a string. `select`, `insert`,
  `update`, `delete` are real syntax; columns and tables are checked at
  compile time.
- **Routes, middleware, validation, transactions, and background jobs are
  language features** — no framework on top of a general-purpose language.
- **One binary** — `jwc` is the compiler, runner, server, migrator, linter,
  and bundler.
- **Real stack out of the box** — HTTP/2 + WebSocket via axum, JWT,
  Argon2 password hashing, in-memory cache, SMTP email, in-process queue,
  LSP server.

## Where to next

- **Getting started** — install the CLI and create your first project.
- **Language tour** — entities, routes, middleware, transactions.
- **Standard library** — what's available out of the box.
- **Database guide** — query syntax, navigation, migrations.

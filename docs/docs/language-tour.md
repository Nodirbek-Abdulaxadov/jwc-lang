---
title: Language tour (10 min)
sidebar_position: 99
---

# A 10-minute tour

Everything in JWC is either a top-level declaration or a function. The
declarations you'll meet:

- `dbcontext Name : Driver;` — names a database connection
- `entity Name of Ctx { ... }` — table + model
- `class Name { ... }` — DTO / view model (no table)
- `route METHOD "path" { ... }` — HTTP or WS endpoint
- `function name(params): T { ... }` — plain function
- `middleware Name { ... }` — request preprocessor
- `errorHandler (e) { ... }` — global catch-all
- `dome Name { ... }` — static-class namespace

Everything else is a statement or an expression.

## Smallest possible app

```jwc
function main() {
    serve(8080);
}
route GET "ping" { return json({ pong: true }); }
```

Run: `jwc run`. Curl: `curl localhost:8080/ping` → `{"pong":true}`.

## Adding a database

```jwc
dbcontext AppDb : Postgres;

entity Note of AppDb {
    id uuid pk;
    title varchar(120);
    created_at datetime;
}

route POST "notes" {
    validate body {
        title: required, minLength(1), maxLength(120);
    }
    let req = body();
    let n = new Note();
    n.id = uuid();
    n.title = req.title;
    n.created_at = now();
    insert n into AppDb.Note;
    return created(n);
}

route GET "notes" {
    return json(select Note from AppDb.Note orderby Note.created_at desc);
}
```

Run `jwc migrate new init && jwc migrate up`, then `jwc serve`.

## Auth in 15 lines

```jwc
middleware AuthMw {
    let token = header("authorization");
    if (token == null) { return unauthorized(); }
    try {
        let claims = jwt_verify(token, env("JWT_SECRET"));
        setContext("userId", claims.sub);
    } catch (e) { return unauthorized(); }
}

route POST "login" {
    let req = body();
    let u = select User from AppDb.User
        where User.username == @req.username first;
    if (u == null) { return unauthorized(); }
    if (!verify_password(req.password, u.password_hash)) {
        return unauthorized();
    }
    return json({ token: jwt_sign({ sub: u.id }, env("JWT_SECRET")) });
}

route GET "me" use AuthMw {
    let id = context("userId");
    return json(select User { id, username, email }
        from AppDb.User where User.id == @id first);
}
```

## Background jobs in 5 lines

```jwc
function emailJob(payload) {
    let p = json_parse(payload);
    send_email(p.to, p.subject, p.body);
}
register_job_handler("send_email", "emailJob");

// somewhere in a route:
enqueue("send_email", json_stringify({
    to: u.email,
    subject: "Welcome",
    body: "<p>Hi " + u.name + "</p>"
}));
```

## WebSocket in 10 lines

```jwc
route WS "/chat" {
    while (true) {
        let msg = ws_recv();
        if (msg == null) { break; }
        ws_send(json_stringify({ at: now(), echo: msg }));
    }
}
```

## What you can't do (yet)

- Async runtime — the interpreter is sync; HTTP is async via
  `spawn_blocking`. Real cooperative concurrency lands with the
  upcoming async rewrite.
- Multi-driver dbcontext — Postgres only today (Redis/Clickhouse/SQLite
  are on the roadmap).
- LLVM AOT compile — `jwc build` bundles the runtime; native code
  generation is the long-term Phase 4 goal.
- Package manager — `jwcproj.json::dependencies` exists but is not
  resolved yet.

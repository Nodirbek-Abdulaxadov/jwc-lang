---
sidebar_position: 2
title: Hello world
description: "A table, a route, a migration and a running server — the whole loop in one file."
---

# Hello world

The smallest useful JWC program is a table, a route over it, and `main`.

## The project

```bash
mkdir hello && cd hello
mkdir src
```

`jwcproj.json`:

```json
{
  "name": "hello",
  "type": "app",
  "version": "0.1.0",
  "entry": "src/app.jwc"
}
```

`src/app.jwc`:

```jwc
namespace app;

database App : Postgres;
schema hello of App;

table Greetings of App.hello {
    id      bigint primary key identity;
    who     varchar(80);
    said_at timestamptz default now();
}

class NewGreeting {
    who varchar(80) required, minLength(1);
}

routes "/greetings" {
    route GET "" {
        return json(select G from App.hello.Greetings
            as { id, who, said_at }
            orderby said_at desc, id desc
            limit 50);
    }

    route POST "" {
        let req = request.body() as NewGreeting;

        return created(json(insert into App.hello.Greetings { ...$req }
            as { id, who, said_at }));
    }
}

function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

The namespace has to match the path: `src/app.jwc` is `namespace app;`. A
mismatch is `W0102`.

## Create the schema

```bash
export DATABASE_URL=postgres://jwc:jwc@localhost:5432/app

jwc migrate new init .     # writes migrations/0001_init.{up,down}.sql
jwc migrate up .           # applies it
jwc migrate verify .       # every constraint and index is where it should be
```

Read `migrations/0001_init.up.sql` before applying it. It is ordinary DDL,
generated so you can review it — not a black box.

## Run it

```bash
jwc serve .
# 2 routes
# listening on http://0.0.0.0:8080
```

```bash
curl -X POST localhost:8080/greetings \
  -H 'content-type: application/json' -d '{"who":"dunyo"}'
# {"id":1,"who":"dunyo","said_at":"2026-08-21T09:30:00.000000+00:00"}

curl localhost:8080/greetings
# [{"id":1,"who":"dunyo","said_at":"…"}]
```

Send a bad body and you get every problem at once, not the first:

```bash
curl -X POST localhost:8080/greetings \
  -H 'content-type: application/json' -d '{"who":""}'
# 400
# {"error":"validation_failed","fields":[
#   {"path":"who","rule":"minLength","limit":1,"message":"who kamida 1 belgidan iborat bo'lishi kerak"}]}
```

## Three endpoints you did not write

The runtime serves these at fixed names, so an operator can reach them
without reading your source:

| Path | Answers |
|---|---|
| `/healthz` | `{"status":"ok"}` — liveness; touches nothing |
| `/readyz` | round-trips every configured dependency, and names the one that failed |
| `/metrics` | Prometheus gauges for the connection pools |

A route you declare at one of those paths wins. A wildcard that merely spans
one does not — see [config](../backend/config).

## Next

[Project structure](./project-structure) — how this grows past one file.

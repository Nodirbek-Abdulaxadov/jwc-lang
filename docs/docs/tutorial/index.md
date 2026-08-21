---
slug: /tutorial
sidebar_position: 1
title: "Tutorial — build a link bin"
description: "One file, four endpoints, a real database. Tables, validation, a service, keyset pagination and the error model, in the order you would actually write them."
---

# Tutorial: a link bin

A shareable list of links. Four endpoints, one file, and every response
below is copied from a run rather than typed out.

```bash
mkdir linkbin && cd linkbin
```

```json title="jwcproj.json"
{ "name": "linkbin", "version": "0.1.0", "entry": "src/app.jwc" }
```

## The schema

```jwc no-compile
namespace app;

database App : Postgres;
schema links of App;

server {
    max_body_bytes = 8192;
    cursor_secret  = env("CURSOR_SECRET");
}

table Bins of App.links {
    id         bigint primary key identity;
    slug       varchar(16) unique : "bu slug band";
    title      varchar(200);
    created_at timestamptz default now();

    index on (created_at);
}

table Entries of App.links {
    id         bigint primary key identity;
    bin_id     bigint;
    url        varchar(2000);
    note       varchar(500)?;
    created_at timestamptz default now();

    foreign key (bin_id) references App.links.Bins (id) on delete cascade;

    index on (bin_id, created_at);
}
```

Two things are already decided here. The message on `slug unique` is what
a client sees when two bins collide — so there is no read-then-insert,
and no race in it. And `on delete cascade` means deleting a bin deletes
its entries without a transaction that walks the tree by hand.

## What a request may send

```jwc no-compile
class NewBin {
    title varchar(200) required, minLength(1), maxLength(200);
}

class NewEntry {
    url  varchar(2000) required, pattern(r"^https?://");
    note varchar(500)?;
}
```

`pattern(r"^https?://")` is the rule that keeps `javascript:` out. It is
on the shape, so every endpoint that takes a `NewEntry` gets it.

## The queries

```jwc no-compile
service BinService {
    function create(req: NewBin) {
        return insert into App.links.Bins {
            slug  = crypto.token(6),
            title = $req.title
        } as { id, slug, title, created_at };
    }

    function by_slug(slug: text) {
        return select B from App.links.Bins
            where slug == $slug
            as { id, slug, title, created_at }
            first;
    }

    function add(bin_id: bigint, req: NewEntry) {
        return insert into App.links.Entries {
            bin_id = $bin_id,
            url    = $req.url,
            note   = $req.note
        } as { id, url, note, created_at };
    }

    function entries(bin_id: bigint, cursor: text?, size: int) {
        return select E from App.links.Entries
            where bin_id == $bin_id
            as { id, url, note, created_at }
            orderby created_at asc, id asc
            page after $cursor size $size;
    }
}
```

`by_slug` returns `Record?` — `first` may find nothing, and the type says
so. The route is where that becomes a 404.

## The endpoints

```jwc no-compile
routes "/bins" {
    route POST "" {
        let req = request.body() as NewBin;

        return created(json(BinService.create($req)));
    }

    route GET "{slug}" {
        let bin = BinService.by_slug(@slug) or throw NotFound("bunday to'plam yo'q");

        return json($bin);
    }

    route GET "{slug}/entries" {
        let bin = BinService.by_slug(@slug) or throw NotFound("bunday to'plam yo'q");

        return json(BinService.entries(
            $bin.id,
            request.query("cursor"),
            int(request.query("size") ?? "20")
        ));
    }

    route POST "{slug}/entries" {
        let bin = BinService.by_slug(@slug) or throw NotFound("bunday to'plam yo'q");
        let req = request.body() as NewEntry;

        return created(json(BinService.add($bin.id, $req)));
    }
}

function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

`or throw NotFound(…)` is the line that does the work. It turns the
`Record?` into a `Record`, so `$bin.id` on the next line type-checks —
and it is the only place a missing bin is handled, once, rather than at
every field read.

## Run it

```bash
createdb linkbin
export DATABASE_URL=postgres://localhost/linkbin
export CURSOR_SECRET=$(openssl rand -hex 32)

jwc migrate new init .
jwc migrate up .
jwc serve .
```

```
4 routes
listening on http://0.0.0.0:8080
```

## Try it

```bash
curl -X POST -H 'content-type: application/json' \
     -d '{"title":"Reading list"}' localhost:8080/bins
```

```json
{"id":"1","slug":"eQkc15HW","title":"Reading list","created_at":"2026-08-21T12:47:00.756755+00:00"}
```

The id is a **string**. A `bigint` exceeds what JSON numbers represent
exactly, and silently losing the low bits of an id is worse than a quoted
one (`types.md §2.3`).

```bash
curl -X POST -H 'content-type: application/json' \
     -d '{"url":"https://jwc.1kb.uz","note":"the docs"}' \
     localhost:8080/bins/eQkc15HW/entries
```

```json
{"id":"1","url":"https://jwc.1kb.uz","note":"the docs","created_at":"2026-08-21T12:47:00.775859+00:00"}
```

The rule holds:

```bash
curl -X POST -H 'content-type: application/json' \
     -d '{"url":"javascript:alert(1)"}' \
     localhost:8080/bins/eQkc15HW/entries
```

```json
{"error":"validation_failed","fields":[{"path":"url","rule":"pattern","message":"url shakli mos emas"}]}
```

The list pages:

```bash
curl localhost:8080/bins/eQkc15HW/entries
```

```json
{"items":[{"id":"1","url":"https://jwc.1kb.uz","note":"the docs","created_at":"2026-08-21T12:47:00.775859+00:00"}],"next":null,"has_more":false}
```

`next` is null because there is no next page. When there is one, hand it
back as `?cursor=…`.

And a bin that does not exist:

```bash
curl -i localhost:8080/bins/nope
```

```
HTTP/1.1 404 Not Found
content-type: application/json; charset=utf-8

{"error":"bunday to'plam yo'q"}
```

## What the compiler already knows

```bash
jwc routes .      # method, path, middleware chain
jwc explain .     # every query, with the SQL it lowers to
jwc openapi .     # OpenAPI 3.1, from the types the checker inferred
```

## Then

- put it behind auth — [Middleware](../backend/middleware.md)
- ship it — [Deployment](../deployment/index.md)
- read what the rules actually are — the
  [specification](https://github.com/just-web-code/jwc-lang/tree/main/docs/spec/v1)

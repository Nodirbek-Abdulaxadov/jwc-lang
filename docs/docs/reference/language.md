---
sidebar_position: 0
title: "The language"
description: "The whole of JWC on one page, explained: every declaration, every type, every clause, and the reason each one is the way it is. Written to be read by a person and pasted into an agent."
---

# The JWC language

One page, the whole language, with the reasons.

There are two other ways in and this is neither of them. The
[tutorial](../tutorial/index.md) builds one service step by step. The
[AI agent guide](./ai-agent-guide.md) is the same material compressed to
tables, for pasting into a context window. This page is for the case in
between: you want to **understand** the language, in order, in one sitting
— and you want an agent to be able to read the same page and get it right.

Written against **JWC 1.0** (`0.9.9xx` pre-releases); check with
`jwc --version`. Every ```` ```jwc ```` block here is compiled by the test
suite, so an example that does not work is a failing build rather than a
page you cannot trust. Blocks marked ```` ```jwc no-compile ```` are
fragments shown for shape.

The normative text is [`docs/spec/v1/`](https://github.com/just-web-code/jwc-lang/tree/main/docs/spec/v1).
Where this page and the spec disagree, the spec wins and this page has a bug.

---

## 1. What JWC is

JWC is a language for HTTP backends over Postgres. A program declares
**tables**, **queries**, **routes** and **middleware**, and the compiler
produces the server, the SQL and the migrations.

It is not a general-purpose language with a web framework bolted on. There
is no ORM, no model layer, no `SELECT *` resolved at run time. That is a
narrowing, and it buys three things:

**The schema is in the source.** `jwc check` type-checks every query
against every table with no database running. A column you renamed breaks
the build, not the 3 a.m. page.

**Every query's shape is known at compile time.** The response body's keys
and their types are decided by the projection you wrote, so the OpenAPI
document, the migration and the answer cannot disagree about what a row is.

**One way to do each thing.** There is one error channel, one loop, one
recovery form, one pagination scheme. A language with two of anything has
a codebase that uses both.

The cost is real and worth stating: no lambdas, no user-defined generics,
no `async`/`await` in the surface language, no ad-hoc SQL beyond a
`raw()` hatch, and no runtime reflection. If you need those, you need a
different language for that part.

---

## 2. Reading the language

Five lexical facts do most of the damage when they are unknown:

| | |
|---|---|
| Line comment | `--`, **not** `//`. `//` lexes as two divisions |
| Doc comment | `---`, attaches to the next declaration, reaches Postgres as `COMMENT ON` |
| Logical operators | `and`, `or`, `not`. `&&` and `\|\|` do not exist |
| Strings | `"…"` with escapes, `r"…"` raw. A newline **inside** either is an error — use `\n` |
| Terminator | `;` after every statement and declaration |

Integers are plain: `42`. No underscores, no hex, no exponent.

There are **no reserved words**. `route`, `key`, `size`, `text` and `date`
are all legal column names; a word is a keyword only in the position where
the grammar expects that keyword. This is why the schema can name a column
after whatever the business calls it, rather than after whatever the
language left over.

### 2.1 The three sigils

| | |
|---|---|
| `$name` | a local, a parameter, a `let` binding |
| `@name` | a path parameter, declared in the route pattern |
| bare `name` | **inside a query clause**: a column. Everywhere else: the local |

`$` is *required* inside a query clause and optional outside one. That
looks like noise until you see what it prevents:

```jwc no-compile
where email == $email     -- the column, compared to your variable
where email == email      -- the column, compared to itself: always true
```

Both compile. They are different queries, not a typo and an error — so the
language makes you say which you meant, in the one place where it cannot
guess.

Everywhere else `$` is optional, and this compiles with or without it:

```jwc
namespace intro;

service Greeter {
    function hello(who: text) -> text {
        let name = who;
        return "salom, " + $name;
    }
}
```

---

## 3. A whole program

Before the parts, the shape. This is a complete service: it checks, it
migrates, it serves.

```jwc
namespace app;

database App : Postgres {
    init() {
        pool_size         = int(env("DB_POOL") ?? "20");
        statement_timeout = "10s";
    }
}

schema notes of App;

server {
    max_body_bytes  = 262144;
    request_timeout = "30s";
    -- Required by any query that uses `page`: a cursor is a
    -- client-supplied predicate, and unsigned it is a second filter
    -- nobody checked.
    cursor_secret   = env("CURSOR_SECRET");
}

--- One note. The doc comment above reaches Postgres as a `COMMENT ON`.
table Notes of App.notes {
    id         bigint primary key identity;
    title      varchar(200) minLength(1) : "sarlavha bo'sh bo'lmasin";
    body       text;
    archived   boolean default false;
    created_at timestamptz default now();

    index on (created_at, id);
}

class NoteCreate {
    title varchar(200) required, minLength(1);
    body  text required;
}

service NoteService {
    function list(size: int) {
        return select N from App.notes.Notes
            where archived == false
            as { id, title, created_at }
            orderby created_at desc, id desc
            page size $size max 100;
    }

    function get(id: bigint) {
        return select N from App.notes.Notes
            where id == $id
            as { id, title, body, created_at }
            first or throw NotFound("bunday eslatma yo'q");
    }

    function create(req: NoteCreate) {
        return insert into App.notes.Notes { ...$req } as { id, title, created_at };
    }
}

routes "/api/v1/notes" {
    route GET "" {
        return json(NoteService.list(20));
    }

    route POST "" {
        let req = request.body() as NoteCreate;
        return created(json(NoteService.create($req)));
    }

    route GET "{id: bigint}" {
        return json(NoteService.get(@id));
    }
}

function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

Read the layering off it, because it is the one structural rule the
language actually enforces socially rather than syntactically:

- a **route** reads the request, calls **one** service function, returns;
- a **service** holds the queries and the business logic;
- a **middleware** holds what every route in a group needs;
- a **table** holds the shape of the data, and the migrations come from it.

A route with a `where` clause in it will compile. It will also be the file
nobody can review a year from now.

---

## 4. Files, namespaces, projects

A project is a directory with a `jwcproj.json` and `.jwc` files under
`src/`:

```
jwcproj.json          { "name": "app", "type": "app", "version": "0.1.0" }
src/app.jwc           namespace app;
src/db/notes.jwc      namespace db.notes;
src/routes/notes.jwc  namespace routes.notes;
migrations/           generated — do not hand-write
public/               served by `static "/" from "public";`
```

One `namespace` per file, first line, and it must match the path under
`src/` — `src/db/notes.jwc` is `namespace db.notes;`. A mismatch is
`W0102`.

`import a.b;` declares that this file depends on that namespace. It does
**not** restrict visibility: the declaration space is flat and global, and
two declarations with the same name anywhere in the program is an error
wherever they are. The import exists so the dependency is written down and
so a package's names resolve — for a package it is required, and without
it the names do not resolve at all (`E0202`).

---

## 5. Types

| JWC | Postgres | On the wire |
|---|---|---|
| `smallint`, `int` | same | number |
| `bigint` | same | **string** |
| `numeric(p, s)` | `numeric` | **string** |
| `text`, `varchar(n)` | same | string |
| `boolean` | same | boolean |
| `timestamptz`, `date`, `time` | same | RFC 3339 / ISO string |
| `interval` | same | ISO 8601 duration, `P30D`, `PT10S` |
| `uuid` | same | string |
| `json` | `jsonb` | the value |
| `inet` | same | string |
| `T[]` | array | array |
| `T?` | nullable | value or `null` |

`bigint` and `numeric` are **JSON strings**, and this surprises people
once each. JavaScript's number loses digits above 2^53, so an id sent as a
number is an id that silently changes; and money in a float is money that
is wrong in the third year. A string is the only representation that
survives both.

### 5.1 Null is a type, not a value

`T?` is the only nullable form. A `T` is never null, and reading a field of
a `T?` is `E0320` — not a warning, an error.

Three shapes narrow it, and there is no `?.`:

```jwc
namespace nulls;

class Row { id bigint required; }

service S {
    function guard(r: Row?) {
        if ($r == null) { throw NotFound("yo'q"); }
        return $r.id;
    }

    function positive(r: Row?) {
        if ($r != null) { return $r.id; }
        return 0;
    }

    function branch(r: Row?) {
        if ($r == null) { return 0; } else { return $r.id; }
    }
}
```

`x ?? fallback` supplies a default and fires **only** on null — which is
what makes the standard boot line work, because `env` answers null and not
`""` for an unset variable:

```jwc no-compile
serve(int(env("PORT") ?? "8080"));
```

### 5.2 `Raw` and `Record`

A query **without** a projection answers `Raw`: a JSON fragment Postgres
assembled, forwarded to the response without ever being parsed. It is the
fastest path and it is opaque — reading a field of it is a compile error.

A query **with** `as { … }` answers a `Record`: named fields, each with a
type, each readable.

Choose by what happens next. If the rows go straight out of an endpoint,
`Raw` never allocates them twice. If your code reads `row.total`, project.

`json.parse` and a `jsonb` column are also `Raw`, for the same reason: the
language will not pretend to know the shape of a document it did not
declare. To get typed fields out of JSON, declare a `class` and let
validation produce it.

---

## 6. The schema

### 6.1 `table`

```jwc
namespace shop;

database App : Postgres { init() { pool_size = 4; } }
schema shop of App;

enum OrderState of App.shop { pending, paid, shipped, cancelled }

--- One customer order.
table Orders of App.shop as "order" {
    id       bigint primary key identity;
    customer varchar(80) minLength(2) : "mijoz ismi juda qisqa";
    total    numeric(12, 2) min(0) : "summa manfiy bo'lolmaydi";
    state    OrderState default OrderState.pending;
    note     text?;
    placed   timestamptz default now();

    index on (customer, placed);
    unique (customer, placed) : "bu mijozda shu vaqtda buyurtma bor";
}
```

A column is `name type modifiers;` — that order, always. The modifiers are
the constraints, and each may carry `: "message"`, which is the sentence a
violation produces. `as "order"` gives the physical table a different name
from the JWC one, which is how a port keeps the rows a live deployment
already has.

`primary key identity` is the ordinary surrogate key. `T?` is how a column
becomes nullable — the same spelling as everywhere else, so there is no
second nullability notion to learn.

A single-column `unique` is a modifier on the column; a multi-column one is
a table-level `unique (a, b)`. Adding `where …` to it makes it a *partial*
unique index instead of a constraint, because Postgres has no partial
unique constraint — which is how "one active subscription per org" is
expressed without a trigger.

Constraints go **on the column**, not in the route, and that is the whole
point: the same `minLength(2)` becomes a Postgres `CHECK` and a 400 from
the request boundary, and a violation from either direction produces the
message you wrote once.

### 6.2 `enum` and `view`

`enum E of App.s { a, b }` is a Postgres enum type; members are written
`E.a`. Comparing an enum to a bare string is an error rather than a
coercion — the type exists precisely so `"pnding"` cannot be a state.

`view V of App.s { … }` is a `CREATE VIEW` over a query, queried like a
table and never written to.

### 6.3 Migrations come from the schema

You do not hand-write migrations. `jwc migrate new <name>` diffs the tables
in the source against the last snapshot and writes the SQL:

```bash
jwc migrate new init      # writes migrations/<ts>_init.{up,down}.sql + a snapshot
jwc migrate up            # apply
jwc migrate status        # applied / pending / drift
jwc migrate down 1        # undo the last one
jwc migrate verify        # does the live database match the snapshot?
```

The generated file is ordinary DDL in a fixed phase order — drop views,
then types, then tables and columns, then data, then constraints, then
indexes, then triggers, then comments, then re-create views, then the
destructive changes last. **Read it before applying it.** It is generated,
not sacred, and a diff that proposes to drop a column is telling you
something about the change you just made.

`status` reports **drift** separately from *pending*: a migration applied
to this database that has no file in this checkout is a different problem
from a file that has not been applied, and merging them is how a deploy
goes out on a schema nobody has.

---

## 7. Queries

The clause order is fixed. Out of order is `E0501`, not a rearrangement:

```
select <binder> from <Qualified.Table>
    { <left|inner> join … on … as <one|many|group> }
    [ where … ]
    [ group by … ] [ having … ]
    [ as { … } ]              -- the projection
    [ orderby … ]
    [ page … | limit … ]
    [ first ]
```

A `select` answers **many rows**. `first` is what makes it one — and one
that may be absent, so it answers `T?` and the caller has to say what an
absent row means.

```jwc
namespace reports;

database App : Postgres { init() { pool_size = 4; } }
schema shop of App;

table Orders of App.shop {
    id       bigint primary key identity;
    customer varchar(80);
    total    numeric(12, 2);
    placed   timestamptz default now();
}

service Reports {
    --- Many rows, bounded.
    function big(floor: numeric) {
        return select O from App.shop.Orders
            where total > $floor
            as { id, customer, total }
            orderby total desc
            limit 50;
    }

    --- One row, or an answer to what "no row" means.
    function one(id: bigint) {
        return select O from App.shop.Orders
            where id == $id
            as { id, customer, total }
            first or throw NotFound("buyurtma yo'q");
    }

    --- An aggregate over the whole table is one row, so it needs `first`.
    function totals() {
        return select O from App.shop.Orders
            as { orders: count(O.id), spend: sum(O.total) }
            first;
    }

    --- Grouped: many rows again, one per group, and no `first`.
    function per_customer() {
        return select O from App.shop.Orders
            group by O.customer
            as { customer, orders: count(O.id), spend: sum(O.total) };
    }

    --- The last day. `timestamptz - interval` is an ordinary operator.
    function recent() {
        return select O from App.shop.Orders
            where placed > date.now() - date.hours(24)
            as { count: count(O.id) }
            first;
    }
}
```

Four things that catch people:

**The binder is mandatory.** `select from App.shop.Orders` does not parse.
The binder is what a join has to name, and having it always present means a
query does not change shape when a join is added.

**`as many` is a join result, not a select clause.** `as one`, `as many`
and `as group` say how a *joined* table folds into each row. On the select
itself, the answer is bounded by `limit` or `page`. An unbounded `as many`
warns (`W0501`) because it is a fan-out nobody counted.

**Only `left join` and `inner join` exist.** `right`, `full` and `cross`
are not grammar. A right join is a left join with the tables written the
other way round, and a cross join is a mistake often enough that making it
say so is worth the inconvenience.

**`count` answers `int`; `sum` widens to `numeric`.** Which means a summed
column comes back as a JSON *string* — see §5. If the number is a counter
rather than money, `int($row.total ?? "0")` narrows it back, and the `??`
is not decoration: `sum` over no rows is null.

### 7.1 Pagination is keyset

```jwc no-compile
orderby created_at desc, id desc
page after $cursor size $size max 100
```

There is no `offset`. Offset re-reads and re-sorts everything it skips, and
it silently repeats and drops rows when the underlying data changes between
pages — which it always does.

The cursor is HMAC-signed with `cursor_secret`, because a cursor is a
predicate the client hands back, and an unsigned one is a filter nobody
checked. The order has to be **total** (a tiebreaker column, usually the
key) with an index that matches it, or paging skips rows for a reason no
log will show.

### 7.2 `exists` and `raw`

`exists` asks the question without fetching the row. And when a query is
genuinely not expressible, there is one hatch:

```jwc no-compile
let n = raw("SELECT count(*) FROM public.thing WHERE kind = {}", $kind);
```

`{}` placeholders are bound as parameters — nothing is interpolated, so
there is no path by which a caller's value reaches the statement as text.
`raw` answers `Raw`. With no placeholders, write `raw(sql)` with no
argument list at all: an empty array still counts as an argument.

Reach for it rarely. Every `raw` is a query `jwc check` cannot verify
against the schema, and `jwc explain` will show it to you unresolved.

---

## 8. Writes

```jwc
namespace stock;

database App : Postgres { init() { pool_size = 4; } }
schema shop of App;

table Items of App.shop {
    id    bigint primary key identity;
    name  varchar(80);
    stock int default 0;
}

class ItemNew { name varchar(80) required; }

service Stock {
    function add(req: ItemNew) {
        return insert into App.shop.Items { ...$req } as { id, name };
    }

    --- The atomic increment: bare `stock` is the column's current value,
    --- `$by` is the parameter. One statement, no read-modify-write, so two
    --- concurrent calls are two increments and never a lost update.
    function bump(id: bigint, by: int) {
        return update App.shop.Items
            set stock = stock + $by
            where id == $id
            as { id, stock }
            first or throw NotFound("mahsulot yo'q");
    }

    function drop(id: bigint) {
        delete from App.shop.Items where id == $id;
    }
}
```

`...$req` spreads a validated class into the write, so the boundary shape
and the insert cannot drift apart.

`on conflict do nothing` and `on conflict do update set …` are the upsert
forms. `do nothing` makes a collision a *missing row* rather than a raised
error, which turns a retry loop into an ordinary `if`:

```jwc no-compile
let made = insert into App.shop.Items { ...$req }
    on conflict do nothing as { id, name };
if (made == null) { … }
```

`insert … buffered` hands the row to a batch writer and returns without
waiting — for telemetry, where the round trip in front of every response
costs more than the row is worth. Rows are dropped rather than queued
without bound if the writer falls behind, and `jwc_log_dropped_total`
counts them. It is `E0612` inside a `transaction { }`, because the row is
written later on another connection and a rollback would not take it back.

### 8.1 `transaction { }`

```jwc no-compile
transaction {
    let org = insert into App.org.Orgs { ...$req } as { id, slug };
    insert into App.org.Members { org_id = $org.id, account_id = $owner };
    return $org;
}
```

The block commits when it completes or returns, and rolls back when a
`throw` or a fault escapes it. A postfix `catch` inside uses a savepoint,
so recovering from one statement does not throw away the rest.

---

## 9. Routes

```jwc no-compile
routes "/api/v1/orgs/{org_id: bigint}" use RequireAuth, Audit {
    route GET "" { … }
    route PATCH "" use RequireOrgAdmin { … }
}
```

The prefix and the suffix concatenate. A path parameter's **type is in the
pattern** and it is read with `@org_id` — parsed before any middleware
runs, so a malformed value is a 400 that names the parameter rather than a
500 from Postgres, and a middleware binder can be typed at all.

`use` on the block applies to every route in it; `use` on a route adds to
that chain. There is no bare top-level `route`.

Precedence is fixed, not registration order: a **literal segment beats a
parameter segment**, so `/orgs/new` wins over `/orgs/{id}` whichever is
declared first. Two routes with the same method and resolved path is a hard
error naming both sites — last-wins does not exist, because file ordering
is not a language feature.

### 9.1 Responses

`json`, `created`, `accepted`, `noContent`, `badRequest`, `unauthorized`,
`forbidden`, `notFound`, `conflict`, `tooManyRequests`, `internalError`,
`statusCode`, `redirect`, `content`, `text`, `html`.

```jwc no-compile
return json($org);
return created(json($org)) with { "Location": "/api/v1/orgs/" + string.of($org.id) };
return text("hello");                       -- text/plain
return html("<h1>hi</h1>");                 -- text/html
return content("application/xml", $body);   -- anything else
return noContent();
return redirect(302, "/dashboard");          -- a path on this service
return redirectExternal(302, $link.url);    -- anywhere, and named so
```

`redirect` refuses a target that leaves this service — a scheme, an
authority, a protocol-relative `//host`. `redirectExternal` is the same
builder without the restriction. Two of them because an open redirect is a
phishing primitive for most services and the entire product for a
shortener, and the language cannot tell which you are.

`with { … }` adds headers; a key the builder already set is *replaced*,
not appended, because a repeated `Content-Type` is a malformed message that
different clients resolve differently.

`cookie(name, value, opts)` chains and may repeat. A cookie is `HttpOnly`
and `SameSite=Lax` unless the author says otherwise, and `http_only: false`
is the opt-out — a default that is wrong is a defect in every program that
did not think about it.

Every response also carries `X-Content-Type-Options: nosniff`,
`X-Frame-Options: DENY` and `Referrer-Policy`; HSTS, a CSP and a
Permissions-Policy are available and off until asked for:

```jwc no-compile
server {
    headers {
        hsts                    = "max-age=31536000; includeSubDomains";
        content_security_policy = "default-src 'none'";
    }
}
```

An empty string turns one off. A header the route set itself wins.

### 9.2 Static files

```jwc no-compile
static "/" from "public" cache 3600;
```

A mount, not a route: no body, no middleware, no path parameters. Under
`jwc serve` the directory is read per request, so an edit shows on the next
refresh; under `jwc build` the tree is walked at compile time and embedded
in the binary, which then needs no directory beside it.

One sharp edge worth knowing before you design around it: **a route is
answered before a mount**, and a `{slot}` route is a route. So a catch-all
like `/{code}` takes `/robots.txt` away from a mount at `/`, and the fixed
crawler names have to be literal routes. (`/healthz` and friends are safe —
a slot does not take an operational name.)

### 9.3 Sockets

```jwc no-compile
routes "/live" use RequireAuth {
    socket "rooms/{room: text}" use RequireMember {
        on open    { socket.send("joined " + @room); }
        on message (text) { socket.send("echo: " + $text); }
        on close   { -- released here, whatever ended the connection
        }
    }
}
```

Three optional blocks, each at most once, and at least one required — a
socket with none would accept the upgrade and then do nothing. There is no
receive loop to write, because an unbounded loop in a handler is the thing
this language does not have.

---

## 10. Middleware

```jwc no-compile
middleware RequireAuth provides account_id: bigint {
    let header = request.header("Authorization") or throw Unauthorized("token kerak");
    let claims = jwt.verify(string.strip_prefix($header, "Bearer "), $secret)
        or throw Unauthorized("token yaroqsiz");
    context.account_id = bigint($claims.sub);
}

middleware RequireOrgMember(@org_id: bigint) requires RequireAuth provides org_id: bigint {
    …
}
```

`provides` declares what it writes into `context`. A route that reads
`context.x` with no middleware providing `x` **fails to compile** — which
is the whole reason `context` is not just a map.

`requires` declares ordering, and a route listing them out of order fails
to compile too.

**Middleware throws; it does not return an error response.** Returning one
is `W0801`: `throw Unauthorized(…)` becomes a 401 in one place, and the
error handler is where the shape of an error response is decided.

Falling off the end is how a middleware lets the chain continue. A bare
`return;` is **not** that — it stops the chain and answers 204 — so since
0.9.942 it is `E0812`. Write `return noContent();` if you meant the
response, `throw` if you meant to reject, and an `if` around the work if
you meant "carry on".

An `after { … }` block runs on the way out, in reverse order, for **every**
outcome. It sees `response.status()` and the duration, may add headers, and
may not change the status or raise — by then there is no handler left to
catch anything.

---

## 11. Errors

```jwc no-compile
error PaymentDeclined(code: text) = 402 : "to'lov rad etildi";
```

Errors propagate on their own. A function's **raise set** is inferred over
the call graph, and exhaustiveness is checked once, at the app boundary:

```jwc no-compile
errorHandler (e) {
    catch PaymentDeclined (err) { return statusCode(402, { code: err.code }); }
    catch (err) { return internalError("kutilmagan xato"); }
}
```

`= <status>` on the declaration is what makes an arm optional: an error
that already knows its status needs no arm at all.

`NotFound`, `BadRequest`, `Unauthorized`, `Forbidden`, `Conflict`,
`TooManyRequests` and friends are predeclared with their statuses.

There is **one** local recovery form, and it is postfix:

```jwc no-compile
let payment = insert into App.billing.Payments { ...$req } as { id }
    catch Conflict (err) { return { status: "duplicate" }; };
```

The block must diverge — `return`, `throw`, `break` or `continue`. There is
no `try { } catch { }` statement, and that is deliberate: a second error
channel is how half a codebase ends up checking return values and the other
half catching.

`or throw` is the shape you will write most:

```jwc no-compile
let org = select … first or throw NotFound("topilmadi");
```

A **fault** — a runtime impossibility rather than a declared error — is a
500 with a generic body, and the detail goes to the log beside the request
id. Internal detail does not reach the client unless `JWC_DEBUG_ERRORS` is
set.

---

## 12. Statements

```jwc
namespace flow;

const LIMIT = 100;

service Loops {
    function demo(n: int) {
        let total = 0;
        let i = 0;

        while (i < $n) {
            i += 1;
            if (i == 3) { continue; }
            if (i > LIMIT) { break; }
            total += i;
        }

        for (x in [1, 2, 3]) {
            total += x;
        }

        let cfg = { a: 1, b: { c: 2 } };
        cfg.b.c = 20;
        cfg.fresh = 30;          -- assigning a field that does not exist adds it

        return $total;
    }
}
```

`let`, `if` / `else`, `while`, `for … in`, `break`, `continue`, `return`,
`throw`, assignment (`=`, `+=`, `-=`, `*=`, `/=`), field assignment,
`const`, `transaction { }`, `dispatch`, `assert`.

There is no `async` or `await`. The runtime is asynchronous and the surface
language is not: every call is written as though it were synchronous, which
is possible because there is no way to hold a future as a value.

### 12.1 The ceilings

Fixed numbers, identical on both backends, because a program that reaches
one has a defect and a knob would only move where the defect appears
(config.md §6a):

| | | |
|---|---|---|
| turns in one `while` | 10 000 000 | errors, naming the loop |
| JWC calls deep | 128 | errors, naming the function |
| expression nesting | 512 | above the call ceiling, so a recursion reports the call |

`for` has no ceiling: it is bounded by its array. Both loops hand the
scheduler a turn every 1024 iterations, and that is not a detail — without
it a loop that never finishes never yields, `request_timeout` never fires,
and one request owns a worker thread until the ceiling. With it,
`request_timeout` is a bound on **compute**, not only on I/O.

The call ceiling has a stack behind it: the runtime gives its threads 64
MiB so 128 frames is reachable. Without that the *machine* stack ran out
first and the process aborted, which took every other in-flight request
with it.

---

## 13. Classes and validation

Validation lives on the **class** and on the **column** — never in the
route:

```jwc
namespace signup;

class Signup {
    email    varchar(255) required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
    password varchar(200) required, minLength(10);
    age      int?         min(13);
}
```

`request.body() as Signup` runs every rule and answers 400 with the failing
paths **before** the handler body runs. The same rules written on a column
become a Postgres `CHECK`, and a violation there is promoted back to the
message you wrote.

That is one rule with two enforcement points and one sentence, rather than
a validator, a constraint and an error string that drift.

---

## 14. Background work

```jwc no-compile
job SendWelcome(account_id: bigint, email: text) retries 5 backoff "30s" {
    let account = select A from App.auth.Accounts
        where id == $account_id
        first or throw NotFound("akkaunt topilmadi");

    mail.send($email, "Welcome", "<p>salom</p>");
}
```

Dispatched from anywhere:

```jwc no-compile
dispatch SendWelcome(account_id: $account.id, email: $account.email);
```

The queue is a Postgres table, so a dispatch inside a `transaction { }`
commits with it and a rollback un-dispatches it. That is the property a
separate broker cannot give you without a two-phase dance.

Parameters are **scalars and arrays of scalars** — no class, no record.
A payload is written to a table and replayed minutes later; pass the id and
read the row in the handler, where it is current.

An attempt that raises is retried after `backoff`; the attempt that
exhausts `retries` moves the job to a dead-letter table with its last
error.

---

## 15. Tests

```jwc no-compile
test "an org gets an owner membership" {
    let org = OrgService.create(NewOrg { name: "Acme", slug: "acme" }, 1);
    let members = select M from App.org.Members where org_id == $org.id as { role };
    assert array.len($members) == 1;
}

test "two active subscriptions are refused" {
    assert fails Conflict {
        SubscriptionService.start($org_id);
    } with "bu tashkilotda faol obuna allaqachon bor";
}
```

A test body is a **service** body — it may write, call service functions
and open a transaction, because a test that cannot do what a service does
cannot test one. `jwc test` runs each in a transaction and rolls it back.

`assert fails <Error> { … }` requires *that* error type. The type is
mandatory: `assert fails { … }` alone passes when a typo makes the block
raise something unrelated, which is the assertion testing itself.

`with "<message>"` pins the message too — which for a constraint is the
only checkable artefact tying a schema rule to the sentence a user reads.

---

## 16. Built-ins

Every built-in is namespaced except the coercions. The generated list is
[the stdlib page](../stdlib/builtins.md); the shape to remember:

**Coercions** — `int`, `bigint`, `numeric`, `boolean`, `uuid`,
`timestamptz`, `enum(E, v)`, `env(name)`. A value that is not a number is a
**400**, not a plausible-looking `0`.

**Text** — `string.of`, `len`, `lower`, `upper`, `trim`, `replace`,
`slice`, `split`, `split_csv`, `join`, `contains`, `starts_with`,
`ends_with`, `strip_prefix`, `pad_left`, `pad_right`, `matches`,
`escape_html`, `escape_url`.

`html(body)` sends its argument verbatim — that is what it is for — so
`string.escape_html` is what makes a value safe to put in one. Nothing
escapes automatically: there is no template engine here, and a builder that
escaped on its own could not emit markup on purpose.

**Arrays** — `array.len`, `is_empty`, `first`, `last`, `contains`,
`pluck`, `sum`, `sum_product`, `min`, `max`, `sorted`. The field-taking
ones (`array.sum(rows, "amount")`) exist because there are no lambdas.
Most of the time the aggregate belongs in the query.

**Dates** — `date.now`, `today`, `days`, `hours`, `minutes`, `seconds`,
`parse`, `format`. Intervals compose with timestamps, in a query or out of
one.

**Hashing and tokens** — `hash.password`, `verify`, `sha256`, `sha1`,
`md5`, `hmac_sha256`, `hmac_verify`, `crypto.token`,
`crypto.constant_time_eq`, `jwt.sign`, `jwt.verify`, `jwt.verify_jwks`.

`hash.password` for a secret a **human** chose; `hash.sha256` for a
high-entropy token the **server** generated. A salted KDF cannot serve
`where token_hash == $h`, because every call produces a different string.
`sha1` and `md5` are for reading a checksum someone else produced.

**Request** — `request.body() as C`, `header`, `query`, `query_all`,
`method`, `path`, `route`, `id`, `client_ip`, `peer_ip`, `raw_body`.
`client_ip` walks `X-Forwarded-For` only through the `trusted_proxies` you
configured; with none, it is the socket peer.

**Response** — the constructors from §9.1, plus inside `after`:
`response.status()`, `duration_ms()`, `duration_us()`, `set_header`,
`add_header`.

**HTTP** — `http.get`, `post`, `json`, `status`. A non-2xx is an *answer*,
not an error; what raises is the request never happening. Outbound requests
are gated by `JWC_HTTP_ALLOWLIST` and `JWC_HTTP_BLOCK_PRIVATE`, and
redirects are not followed — a redirect is how an allowlisted host walks
you to one that is not.

**JSON** — `json.parse` (answers `Raw`, raises `BadRequest` on bad input),
`json.stringify`.

**Cache** — `cache.get/set/del/clear`, process-local and always available.
Two replicas do not share it, so a rate limiter keyed here counts per pod.

**Console**, for `jwc run` — `console.write` (no newline), `writeln`,
`error`, `read` (null at EOF).

**Mail** — `mail.send`, `mail.enabled`. Raises when no relay is configured
rather than answering null, because a password-reset route that returned
200 and sent nothing is how that was found.

**Debug** — `debug.dump(v)` returns its argument unchanged and prints
nothing outside `JWC_DEV`.

`redis.*` is a **package**, not part of the language: `import redis;` plus
`redis` in `dependencies`. Every name but `redis.enabled()` raises when no
server is configured — a rate limiter built on a call that quietly answered
null would allow everything.

---

## 17. The runtime

`server { … }` sets the listener's behaviour:

| | | |
|---|---|---|
| `max_body_bytes` | 1048576 | over → 413, before any middleware |
| `request_timeout` | `30s` | the whole request |
| `header_timeout` | `10s` | the request line and headers |
| `max_page_size` | 100 | the ceiling for `page … size` |
| `strict_slash` | true | `/x/` → 308 → `/x` |
| `bind` | `0.0.0.0` | the address |
| `trusted_proxies` | `[]` | whose `X-Forwarded-For` may be believed |
| `cursor_secret` | — | required by any `page` query (`E1205`) |
| `shutdown_grace` | `20s` | the drain window on SIGTERM |
| `cors { … }`, `tls { … }` | — | sub-blocks |

An unknown key is `E1206`, not a setting that silently does nothing.

Everything operational is an environment variable, because it belongs to
the deployment and not to the source: `DATABASE_URL`, `PORT`,
`JWC_REDIS_URL`, `JWC_REQUEST_LOG`, `JWC_LOG_FORMAT`, `JWC_SERVER_WORKERS`,
`JWC_CACHE_MAX_ENTRIES`, `JWC_JOB_WORKERS`, and the rest. Booting with
`JWC_PRINT_CONFIG=1` prints every registered variable with its current
value and where that value came from, secrets redacted by **name** —
`SECRET`, `TOKEN`, `KEY`, `PASSWORD`, `DATABASE_URL` — so the table is safe
to paste into an issue.

Three endpoints exist in every program and are **not declarable**:

| | |
|---|---|
| `/healthz` | the process is up |
| `/readyz` | Postgres (and Redis, when configured) answered |
| `/metrics` | Prometheus text |

Not declarable on purpose: an operator needs those three paths without
reading your source, and a route with a `{slot}` does not take them away.

---

## 18. Packages

A package is a JWC library: a manifest with `"type": "pkg"`, a namespace,
and `public` functions. Consumers add it to `dependencies` and `import` it.

A package name must be an **identifier** — `redis`, not `jwc-redis`. A
hyphen cannot be written in an `import`, so a hyphenated name is a package
that can be published and never used.

Ownership is first-publisher-wins and a name is permanent, so check before
you publish.

---

## 19. The two backends

| | |
|---|---|
| `jwc serve` | the interpreter — the whole language, instant start |
| `jwc build` | ahead-of-time to a native binary, via generated Rust and cargo |

They are held to **byte-identical** responses — same status, same headers,
same body — and that is enforced by tests rather than intended. Anything
`jwc build` cannot lower it **refuses by name**; a binary that quietly
dropped a query would be far worse than one that will not build.

Use `serve` in development and `build` when startup time or memory matters.
`jwc build` needs a Rust toolchain, because that is what it hands the
generated crate to. It runs `jwc check` first and builds nothing if that
fails.

---

## 20. The commands

```bash
jwc new myapp --template api   # empty | api | auth | jobs
jwc check                    # types, schema, routes — offline, no database
jwc fmt                      # canonical form; --check for CI
jwc lint --deny-warnings     # whole-program advisory lints
jwc lint --explain E0211       # what a diagnostic means
jwc routes                   # the resolved route table
jwc explain                  # every query, with the SQL it becomes
jwc openapi                  # OpenAPI 3.1, from the typed signatures
jwc migrate new init         # diff the schema, write up/down SQL
jwc migrate up               # apply
jwc test                     # every `test` block, each rolled back
jwc run                      # call main() — console programs
jwc serve --watch            # run, restart on change
jwc build --release          # a native binary
```

`check`, `fmt`, `lint`, `routes`, `explain`, `openapi` and `migrate new`
are all **offline**: the schema is in the source, so none of them needs a
database or a network.

---

## 21. What is deliberately absent

Knowing what is *not* here saves more time than any feature list:

| | |
|---|---|
| `//` comments | `--`. `//` is division, twice |
| `&&`, `\|\|` | `and`, `or` |
| `try` / `catch` statements | `or throw`, or postfix `catch` on one expression |
| `async` / `await` | the runtime is async; the language is not |
| lambdas, first-class functions | hence `array.sum(rows, "amount")` |
| `offset` pagination | keyset `page after $cursor` |
| `SELECT *` | project with `as { … }`, or take `Raw` |
| `x?.y` | narrow with `if (x == null)`, or `??` |
| `right` / `full` / `cross join` | `left` and `inner` |
| multi-line string literals | `\n`, or a file under a `static` mount |
| hand-written migrations | `jwc migrate new` |
| an ORM, a model layer | the table declaration is the model |

---

## 22. The mistakes that actually happen

**`//` instead of `--`.** It lexes as two divisions and the error surfaces
somewhere else entirely.

**Logic in a route.** Read the request, call one service function, return.

**Returning an error response from a middleware.** `throw` instead; the
error model turns it into a status in one place.

**A bare `return;` in a middleware's request phase.** It answered 204 and
stopped the chain, silently. `E0812` since 0.9.942.

**Forgetting the binder.** `select O from App.s.T` — the `O` is required.

**Forgetting `$` inside a query clause.** `where email == email` is always
true and compiles.

**Reading a field of a `T?`.** `first` answers `T?`; use `or throw` or
narrow.

**Expecting `SELECT *`.** Without a projection you get `Raw`, which is
opaque by design.

**Forgetting `first` on an aggregate.** A projection with no `group by`
still answers an array without it.

**`int($row.sum_column)` on an empty table.** `sum` over no rows is null.
`?? "0"`.

**Hand-writing a migration.** `jwc migrate new` diffs the schema. Read the
SQL it writes.

**Guessing a built-in's name.** `jwc check` answers with a suggestion, not
a runtime failure.

---

## Where to go next

- [Tutorial](../tutorial/index.md) — one service, built up step by step
- [AI agent guide](./ai-agent-guide.md) — the same language, compressed for a context window
- [Error codes](./error-codes.md) — every diagnostic, generated from the spec
- [Built-ins](../stdlib/builtins.md) — the full list, generated from the compiler
- [`docs/spec/v1/`](https://github.com/just-web-code/jwc-lang/tree/main/docs/spec/v1) — the normative text

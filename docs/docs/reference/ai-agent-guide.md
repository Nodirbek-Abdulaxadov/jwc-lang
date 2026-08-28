---
sidebar_position: 0
title: "AI agent guide"
description: "The whole JWC language in one file, written to be handed to a coding agent: every declaration, every statement, every built-in, and the mistakes agents actually make."
---

# JWC for AI agents

**This page is the language in one file.** Paste it into an agent's
context, save it as `JWC.md` beside your code, or point a `CLAUDE.md` /
`AGENTS.md` / `.cursorrules` at it.

Written against **JWC 1.0** (`0.9.9xx` pre-releases). Check with
`jwc --version`.

Every ```` ```jwc ```` block below is compiled by the test suite. If an
example here does not work, that is a bug in this page and it fails CI.

> **Human readers:** this is a dense reference. The narrative docs are
> friendlier — start at [Hello world](../getting-started/hello-world.md).

---

## The shape of it

JWC is a backend language. A program declares **tables**, **queries**,
**routes** and **middleware**; the compiler produces a server, the SQL,
and the migrations.

Two things follow from that, and most agent mistakes come from missing
them:

1. **The schema is in the source.** `jwc check` type-checks every query
   against every table without a database. There is no ORM, no model
   layer, no `SELECT *` at runtime — the query's shape is known at
   compile time.
2. **Routes contain no logic.** A route reads the request, calls one
   service function, and returns. Business logic is in `service`;
   cross-cutting concerns are in `middleware`.

---

## Lexical rules — read these first

| | |
|---|---|
| Line comment | `--`, **not** `//`. `//` is division, twice. |
| Doc comment | `---`, attaches to the next declaration, reaches Postgres as `COMMENT ON` |
| Logical operators | `and`, `or`, `not`. `&&` and `\|\|` do not exist; `!` is `not` |
| Strings | `"…"` with escapes, `r"…"` raw (no escapes but `\"`) |
| Newline in a string | an **error**. Use `\n` |
| Integers | `42`. No underscores, no hex, no exponent |
| Statement terminator | `;` after every statement and declaration |

There are **no reserved words**. `route`, `key`, `size`, `text`, `date`
are all legal column names — a word is a keyword only where the grammar
expects that keyword.

### Sigils

| | |
|---|---|
| `$name` | a local, parameter, or `let` binding |
| `@name` | a path parameter, from the route pattern |
| bare `name` | **inside a query clause**: a column. Elsewhere: the local |

`$` is **required inside query clauses** and optional everywhere else.
This compiles:

```jwc
namespace ex1;

service Greeter {
    function hello(who: text) {
        let name = who;
        return "salom, " + $name;
    }
}
```

Inside a `where` / `set` / `as` clause the distinction is load-bearing:
`email` is the *column*, `$email` is your variable, and writing the wrong
one is a different query, not an error.

---

## A whole program

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
    -- Required by any query that uses `page`: the cursor is a
    -- client-supplied predicate, and unsigned it is a second filter
    -- nobody checked.
    cursor_secret   = env("CURSOR_SECRET");
}

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
        let items = NoteService.list(20);
        return json($items);
    }

    route POST "" {
        let req = request.body() as NoteCreate;
        let note = NoteService.create($req);
        return created(json($note));
    }

    route GET "{id: bigint}" {
        return json(NoteService.get(@id));
    }
}

function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

That is a complete, checkable, runnable service. `jwc check` verifies
every query against the table, `jwc migrate new init` turns the table into
DDL, `jwc serve` runs it.

---

## Declarations

| Keyword | |
|---|---|
| `namespace a.b;` | one per file; must match the path under `src/` |
| `import a.b;` | brings a namespace's declarations into scope |
| `database App : Postgres { init() { … } }` | the connection |
| `schema s of App;` | a Postgres schema |
| `table T of App.s { … }` | a table |
| `view V of App.s { … }` | a view over a query |
| `enum E of App.s { a, b }` | a Postgres enum type |
| `class C { … }` | a request/response shape, with validation |
| `error E(msg: text) = 404 : "…";` | an error type and its default status |
| `service S { function f(…) { … } }` | queries and business logic |
| `middleware M { … }` | runs before a route |
| `routes "/prefix" use M { route … }` | the route table |
| `errorHandler (e) { catch … }` | one per program, at the app boundary |
| `job J(a: text) retries 5 backoff "30s" { … }` | background work |
| `server { … }` | limits |
| `function main() { … }` | the entry point |
| `test "name" { … }` | a test, rolled back |
| `const NAME = 5;` | a compile-time constant |
| `static "/assets" from "public";` | serve files |

### Namespaces must match the path

`src/db/notes.jwc` is `namespace db.notes;`. A mismatch is `W0102`.
The declaration space is **flat and global** — `import` does not restrict
visibility, it just documents the dependency. Two declarations with one
name anywhere in the program is an error.

---

## Types

| JWC | Postgres |
|---|---|
| `bigint`, `int`, `smallint` | same |
| `numeric(p, s)` | `numeric` |
| `text`, `varchar(n)` | same |
| `boolean` | same |
| `timestamptz`, `date`, `time` | same |
| `uuid` | same |
| `json` | `jsonb` |
| `T[]` | array |
| `T?` | nullable |

**`T?` is the only nullable form. A `T` is never null.** Reading a field
of a `T?` is `E0320`. Three shapes narrow it:

```jwc
namespace ex2;

class Row { id bigint required; }

service S {
    function a(r: Row?) {
        if ($r == null) { throw NotFound("yo'q"); }
        return $r.id;
    }

    function b(r: Row?) {
        if ($r != null) { return $r.id; }
        return 0;
    }

    function c(r: Row?) {
        if ($r == null) { return 0; } else { return $r.id; }
    }
}
```

`x ?? default` supplies a fallback. `x?.y` does not exist — the narrowing
above is the intended shape.

**`bigint` is a string on the wire**, in every response, because
JavaScript loses digits above 2^53.

### `Raw` versus `Record`

A query **without** `as { … }` returns `Raw`: a JSON fragment Postgres
built, forwarded to the response with no parsing. It is fast and it is
opaque — reading a field of it is a compile error.

A query **with** `as { … }` returns a `Record` with named, typed fields.

Use `Raw` when the answer goes straight out; use a projection when the
code reads it.

---

## Queries

The clause order is fixed. Writing them out of order is `E0501`:

```
select <binder> from <Qualified.Table>
    { <left|inner> join … on … as <one|many|group> }
    [ where … ]
    [ group by … ] [ having … ]
    [ as { … } ]          -- the projection
    [ orderby … ]
    [ page … | limit … ]
    [ first ]
```

A `select` answers **many rows by default**. `first` is what makes it one
— and one that may be absent, so it answers `T?`.

```jwc
namespace ex3;

database App : Postgres { init() { pool_size = 4; } }
schema shop of App;

table Orders of App.shop {
    id       bigint primary key identity;
    customer varchar(80);
    total    numeric(12, 2);
    placed   timestamptz default now();
}

service Reports {
    function big(floor: numeric) {
        return select O from App.shop.Orders
            where total > $floor
            as { id, customer, total }
            orderby total desc
            limit 50;
    }

    function one(id: bigint) {
        return select O from App.shop.Orders
            where id == $id
            as { id, customer, total }
            first or throw NotFound("buyurtma yo'q");
    }

    --- Aggregates + `group by`, and no `as many` in the same query:
    --- the grouping already fixes the cardinality (queries.md §6.2).
    function per_customer() {
        return select O from App.shop.Orders
            group by O.customer
            as { customer, orders: count(O.id), spend: sum(O.total) };
    }
}
```

- **The binder is mandatory.** `select from App.shop.Orders` does not parse.
- `first` answers `T?` — use `or throw NotFound(…)` unless a null answer
  is genuinely what the route means.
- **`as many` is a join result, not a select clause.** `as one`,
  `as many` and `as group` say how a joined table folds into each row
  (`as many N` also takes its own `orderby`, and warns `W0501` when it is
  unbounded). On the `select` itself, `limit` or `page` bounds the answer.
- Only `left join` and `inner join` exist. `right`, `full` and `cross` are
  not grammar.

### Pagination is keyset, not offset

```jwc no-compile
page after $cursor size $size max 100
```

The cursor is signed with `CURSOR_SECRET`, because a cursor is a position
in someone else's data. The order has to be **total** — `created_at desc,
id desc`, with an index that matches — or paging skips and repeats rows.

---

## Writes

```jwc
namespace ex4;

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

`...$req` spreads a validated class into the write. In `set`, a bare
`stock` is the *column's current value* and `$by` is your variable — so
`set stock = stock + $by` is the atomic increment, not a read-modify-write.

---

## Routes

```jwc no-compile
routes "/api/v1/orgs/{org_id: bigint}" use RequireAuth, Audit {
    route GET "" { … }
    route PATCH "" use RequireOrgAdmin { … }
}
```

- The path parameter's **type is in the pattern**, and it is read with
  `@org_id`.
- `use` on the `routes` block applies to every route in it; `use` on a
  `route` adds to that chain.
- A `route` lives inside a `routes` block. There is no bare top-level
  `route`.
- The prefix and the suffix concatenate: `"/api/v1/notes"` + `"{id: bigint}"`.

### Responses

`json`, `created`, `accepted`, `noContent`, `badRequest`, `unauthorized`,
`forbidden`, `notFound`, `conflict`, `tooManyRequests`, `internalError`,
`statusCode`, `redirect`, `content`, `text`, `html`.

```jwc no-compile
return json($org);
return created(json($org)) with { Location: "/api/v1/orgs/" + string.of($org.id) };
return text("hello");                 -- text/plain
return html("<h1>hi</h1>");           -- text/html
return noContent();
```

---

## Middleware

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

- `provides` declares what it writes into `context`. A route reading
  `context.x` without a middleware that provides `x` **fails to compile**.
- `requires` declares the ordering. A route that lists them out of order
  fails to compile.
- **Middleware throws; it does not return an error response.** Returning
  one is `W0801`. `throw Unauthorized(…)` becomes a 401 in one place.
- An `after { … }` block runs on the way out, in reverse order, for
  **every** outcome, and may add headers but not change the status.

---

## Errors

```jwc no-compile
error PaymentDeclined(code: text) = 402 : "to'lov rad etildi";
```

Errors propagate automatically. A function's **raise set** is inferred
over the call graph; exhaustiveness is checked once, at the app boundary.

```jwc no-compile
errorHandler (e) {
    catch PaymentDeclined (err) { return statusCode(402, { code: err.code }); }
    catch (err) { return internalError("kutilmagan xato"); }
}
```

`= <status>` is what makes an arm **optional** — a declared error with a
default status needs no arm at all.

**There is one local recovery form**, and it is postfix:

```jwc no-compile
let payment = insert into App.billing.Payments { ...$req } as { id }
    catch Conflict (err) { return { status: "duplicate" }; };
```

The block must diverge — `return`, `throw`, `break` or `continue`. There
is **no `try { } catch { }` statement**; it was rejected deliberately so
the language does not grow a second error channel.

`or throw` is the common shape:

```jwc no-compile
let org = select … first or throw NotFound("topilmadi");
```

---

## Statements

```jwc
namespace ex5;

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
        cfg.fresh = 30;

        return $total;
    }
}
```

`let`, `if` / `else`, `while`, `for … in`, `break`, `continue`, `return`,
`throw`, assignment (`=`, `+=`, `-=`, `*=`, `/=`), field assignment
(`o.a.b = v`, which creates the key if absent), `const`, `transaction { }`,
`dispatch`, `assert`.

Three ceilings, the same on both backends (config.md §6a):

| | |
|---|---|
| 10 000 000 turns in one `while` | a runaway loop errors, naming the loop |
| 128 JWC calls deep | a recursion with no base case errors, naming the function |
| 512 levels of expression nesting | above the call ceiling, so a recursion reports the call |

There is no ceiling on `for`: it is bounded by its array. Both loops hand
the scheduler a turn every 1024 iterations, which is what makes
`request_timeout` a bound on compute and not only on I/O.

---

## Built-ins

The full list is [the stdlib page](../stdlib/builtins.md), which is
generated from the compiler's own registry. The shape to remember:
**every built-in is namespaced** except the coercions.

**Coercions** — `int`, `bigint`, `numeric`, `boolean`, `uuid`,
`timestamptz`, `enum(E, v)`, `env(name)`.

`env` answers **null** when unset, not `""`, which is what makes
`int(env("PORT") ?? "8080")` work — `??` only fires on null.

**Text** — `string.of`, `string.len`, `string.lower`, `string.upper`,
`string.trim`, `string.replace`, `string.slice`, `string.split`,
`string.split_csv`, `string.join`, `string.contains`,
`string.starts_with`, `string.ends_with`, `string.strip_prefix`,
`string.pad_left`, `string.pad_right`, `string.matches`.

**Arrays** — `array.len`, `array.is_empty`, `array.first`, `array.last`,
`array.contains`, `array.pluck`, `array.sum`, `array.sum_product`,
`array.min`, `array.max`, `array.sorted`.

The field-taking ones (`array.sum(rows, "amount")`) exist because JWC has
no lambdas — a function is not a first-class value here. Most of the time
the aggregate belongs in the query instead.

**Dates** — `date.now`, `date.today`, `date.days`, `date.hours`,
`date.minutes`, `date.seconds`, `date.parse`, `date.format`. Intervals
compose with timestamps inside a query:
`where created_at > date.now() - date.hours(24)`.

**Hashing, tokens, JWT** — `hash.password`, `hash.verify`, `hash.sha256`,
`hash.sha1`, `hash.md5`, `hash.hmac_sha256`, `hash.hmac_verify`,
`crypto.token`, `crypto.constant_time_eq`, `jwt.sign`, `jwt.verify`,
`jwt.verify_jwks`.

**`hash.password` for secrets a human chose; `hash.sha256` for
high-entropy tokens the server generated.** A salted KDF cannot serve
`where token_hash == $h`, because every call produces a different string.
`hash.sha1` and `hash.md5` are for reading a checksum someone else
produced — never for a password.

**Request** — `request.body() as C`, `request.header`, `request.query`,
`request.query_all`, `request.method`, `request.path`, `request.route`,
`request.id`, `request.client_ip`, `request.peer_ip`,
`request.raw_body`.

**Response** — the constructors listed above, plus, inside an `after`
block: `response.status()`, `response.duration_ms()`,
`response.duration_us()`, `response.set_header(k, v)`,
`response.add_header(k, v)`.

**JSON** — `json.parse` (answers `Raw`, raises `BadRequest` on bad
input), `json.stringify`. To get typed fields out of JSON, declare a
`class` and let validation do it.

**Console**, for `jwc run` — `console.write` (no newline),
`console.writeln`, `console.error`, `console.read` (null at EOF).

**Other** — `http.get/post/json/status`, `cache.get/set/del/clear`,
`mail.send`, `mail.enabled`, `debug.dump`, `raw`.

`redis.*` is a **package**, not part of the language: `import redis;` and
declare the dependency.

## Validation

Validation lives on the **class** and on the **column**, not in the route:

```jwc
namespace ex6;

class Signup {
    email    varchar(255) required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
    password varchar(200) required, minLength(10);
    age      int?         min(13);
}
```

`request.body() as Signup` runs every rule and answers 400 with the
messages before the handler body runs. The same rules on a column become
a Postgres `CHECK`, and a violation there is promoted to the message you
wrote.

---

## The two backends

| | |
|---|---|
| `jwc serve` | the interpreter — the whole language |
| `jwc build` | AOT to a native binary via Rust |

They are held to **byte-identical** responses. Anything `jwc build`
cannot lower it **refuses by name** rather than dropping silently.

---

## The commands

```bash
jwc new myapp --template api    # empty | api | auth | jobs
jwc check                     # types, schema, routes — offline
jwc fmt                       # canonical form
jwc lint --deny-warnings      # + advisory whole-program lints
jwc lint --explain E0211        # what a code means
jwc migrate new init          # diff the schema, write up/down SQL
jwc migrate up                # apply
jwc routes                    # the resolved route table
jwc explain                   # every query, with its SQL
jwc openapi                   # OpenAPI 3.1
jwc test                      # every `test` block, each rolled back
jwc serve --watch             # run, restart on change
jwc build --release           # a native binary
```

---

## Mistakes agents actually make

**Writing `//` for a comment.** It is `--`. `//` lexes as two divisions
and the error appears somewhere else entirely.

**Writing `&&` / `||` / `!`.** Use `and`, `or`, `not`. (`!` does work as a
prefix; `&&` and `||` do not exist at all.)

**Putting logic in a route.** Read the request, call one service function,
return. Everything else belongs in `service` or `middleware`.

**Returning an error response from middleware.** `throw Unauthorized(…)`.
The error model turns it into a 401 in one place; returning a response
gets `W0801` and bypasses the handler.

**Forgetting the binder.** `select O from App.s.T` — the `O` is required.

**Forgetting `$` inside a query clause.** In `where email == $email` the
bare `email` is the column and `$email` is your variable. Swapping them
compiles to a different query, and `where email == email` is always true.

**Reading a field of a `T?`.** `first` answers `T?`. Use `or throw`, or
one of the three narrowing shapes.

**Expecting `SELECT *`.** Without `as { … }` you get `Raw`, which is
opaque. Name the fields you read.

**Reaching for `try` / `catch` as a statement.** It does not exist. Use
`or throw`, or postfix `catch E (err) { … }` where the block diverges.

**Expecting `async` / `await`.** The runtime is async; the language is
not. Every call is written as if synchronous.

**Adding an `offset` for pagination.** Use `page after $cursor size $size`.

**Hand-writing SQL migrations.** `jwc migrate new <name>` diffs the
schema against the last snapshot and writes them. Read the generated SQL
before applying it — it is ordinary DDL, not a black box.

**Guessing a built-in's name.** Run `jwc check`; an unknown name is
`E0204` with a suggestion, not a runtime failure.

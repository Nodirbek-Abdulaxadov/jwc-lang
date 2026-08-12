---
sidebar_position: 0
title: "AI agent guide"
description: "The whole JWC language in one file, written to be handed to an AI coding agent: every declaration, statement, built-in and native-build rule, plus the mistakes agents actually make."
---

# JWC for AI agents

**This page is the entire language in one file.** It exists to be pasted into
an agent's context, saved as `JWC.md` next to your code, or referenced from a
`CLAUDE.md` / `AGENTS.md` / `.cursorrules`. Everything an agent needs to write
a working JWC service is here; no other page is required reading.

Written against **JWC 0.9.x**. Check your version with `jwc --version`.

> **Human readers:** this is a reference dump, deliberately dense and
> example-first. The narrative docs are friendlier — start at
> [Hello world](../getting-started/hello-world.md).

---

## 0. Orientation

JWC is a **backend-only, Postgres-only** language. One `.jwc` file can hold
your schema, your HTTP routes, your validation, your auth and your background
jobs. There is no ORM, no DTO layer, no repository pattern — entities compile
straight to SQL and queries are syntax, not a library.

```jwc
dbcontext AppDb: Postgres;

entity Note of AppDb {
    id         int pk autoincrement;
    title      varchar(200);
    body       text;
    created_at datetime;
}

route GET "/notes" {
    return json(select Note from AppDb.Note orderby Note.created_at desc);
}

function main() {
    setConnectionString();
    serve(8080);
}
```

`jwc run` boots that as an HTTP server against Postgres.

**What JWC is not.** No classes-with-methods, no inheritance, no generics, no
first-class functions (no lambdas, no callbacks — a function is not a value),
no `null`-safe navigation operator, no imports *within* a project, no
databases other than Postgres. If a task needs any of those, say so rather
than inventing syntax; **JWC has no syntax you cannot find on this page.**

---

## 1. Rules an agent must not break

1. **Never invent a built-in.** The complete list is [§11](#11-built-in-functions).
   `jwc check` does **not** catch an unknown function name — it fails at
   runtime instead. See [§17](#17-gotchas-ranked-by-how-often-agents-hit-them).
2. **`return { status: 400, ... }` does not set an HTTP status.** A plain
   object is a *body*. Use `statusCode(400, body)` or a helper like
   `badRequest(...)`. ([§9.4](#94-responses))
3. **A bare string return gets `application/json`.** For any other type use
   `response(body, "text/plain")` / `text(...)` / `html(...)`.
4. **`main()` must call `serve(port)`** or the program exits without serving.
5. **`for` takes no parentheses:** `for x in items { }`, never `for (x in items)`.
6. **Statements end in `;`** — including every line inside `validate body { }`.
7. **`after { }` goes after the middleware's closing brace**, not inside it.
8. **Entity name → table name is snake_case:** `entity ApiCall` is table
   `api_call`. Write `raw_sql` against the snake_case name.
9. Before claiming done, run `jwc check`, `jwc lint`, **and actually exercise
   the routes** — see the checklist in [§19](#19-before-you-say-its-done).

---

## 2. Project layout and CLI

A project is a `jwcproj.json` manifest plus one or more `.jwc` files:

```
myapp/
├── jwcproj.json        # manifest (name, version, entry)
├── main.jwc            # any number of .jwc files
├── views.jwc
├── migrations/         # generated SQL
└── .env                # PG_* or JWC_DATABASE_URL
```

**Every `.jwc` file in the project is merged into one flat program.** There is
no per-file import; a function declared in `views.jwc` is callable from
`main.jwc` with no ceremony. A parse error in *any* file fails the whole load.
(`namespace` / `import` exist for *packages* — [§14](#14-namespaces-packages-mount-group).)

| Command | What it does |
|---|---|
| `jwc new <name> [--template api\|auth\|jobs]` | scaffold a project |
| `jwc check <file>` | parse + validate one file |
| `jwc lint` | validate the project + dead-code warnings |
| `jwc fmt [paths] [--check]` | canonical formatting |
| `jwc run [path]` | run `main()`, then serve if it called `serve()` |
| `jwc serve [path] --port N --watch` | always serve; `--watch` reloads |
| `jwc gen-sql <file>` | print the `CREATE TABLE` DDL |
| `jwc migrate new <name>` / `up` / `down` / `status` | migrations |
| `jwc build` | bundle launcher + runtime |
| `jwc build --native [--release]` | AOT → one static binary ([§15](#15-native-aot-build)) |
| `jwc openapi` / `jwc swagger` | generate an OpenAPI spec from the routes |
| `jwc add / install / update / remove / tree` | packages |
| `jwc login` / `jwc publish` | registry |

---

## 3. Lexical basics

```jwc
// Line comments only — there is no /* block */ form.

function main() {
    let s = "double quoted";        // strings are double-quoted
    let r = r"^raw\s+regex$";       // raw string — no escape processing
    let t = `interpolated ${s}!`;   // backtick template, ${expr} holes
    let n = 42;                     // int
    let f = 3.14;                   // float
    let b = true;                   // bool: true / false
    let z = null;                   // null
}
```

**Reserved keywords:** `dbcontext entity class route function dome let const
return print if else while break continue and or namespace import true false
null validate try catch middleware use async await transaction savepoint
errorHandler for in public private mount group`.

Identifiers are case-sensitive, but **built-in and middleware names are
matched case-insensitively** — `statusCode` and `status_code` are the same
function, `use RateLimit` finds `middleware ratelimit`.

---

## 4. Declarations

Everything below is a **top-level** form. Order does not matter.

```jwc no-compile
dbcontext AppDb: Postgres;                  // exactly one per project, usually

entity User of AppDb { ... }                // a table
class Dto { ... }                           // a shape with no table

function name(a: int, b: text): text { ... }
async function fetchAll(): text { ... }

route GET "/path" use Mw1, Mw2 { ... }      // inline body
route GET "/path" -> handlerFn;             // delegate to a function

middleware Auth { ... }                     // request phase
after { ... }                               // optional response phase

errorHandler (e) { ... }                    // one per project

namespace mypkg;                            // package files only
import otherpkg;
public function shared() { ... }            // default is private
mount otherpkg at "/prefix";
group "/api/v1" use Auth { ... }
```

`const NAME = expr;` declares a compile-time constant at top level.

---

## 5. Types

### 5.1 Runtime values

`int`, `float`, `text`/`string`, `bool`, `null`, arrays, objects, and entity
instances. Typing is **gradual**: annotations on function params and returns
are checked (errors E018–E020), locals are inferred and unannotated.

### 5.2 Entity column types

Only these are valid in an `entity` body:

| JWC | Postgres |
|---|---|
| `int` | `integer` |
| `int(min,max)` | `integer` + a `CHECK` range constraint |
| `bigint` | `bigint` |
| `decimal(p,s)` | `numeric(p,s)` — precision + scale are **required** |
| `text` | `text` |
| `text(n)` / `varchar(n)` | `varchar(n)` |
| `varchar` | `varchar` |
| `bool` | `boolean` |
| `uuid` | `uuid` |
| `datetime` / `timestamp` | `timestamptz` |
| `json` | `jsonb` |
| `bytea` | `bytea` |

Anything else is a compile error. There is no `float` *column* type — use
`decimal(p,s)`.

### 5.3 Field modifiers

Written after the type, space-separated, in any order, terminated by `;`:

`pk`, `autoincrement` (aliases `auto_increment`, `serial`), `unique`,
`index` (alias `indexed`), `nullable` (alias `null`),
`references Entity.column [on delete cascade|restrict|set null]`.

```jwc
entity Post of AppDb {
    id        int pk autoincrement;
    author_id int references User.id on delete cascade;
    slug      varchar(80) unique index;
    body      text nullable;
    tags      json;
    created_at datetime;
}
```

**Table name = snake_case of the entity name.** `entity ApiCall` → `api_call`.

---

## 6. Statements

```jwc
let x = 1;                 // declare
x = 2;                     // assign
obj.field = 3;             // field assign
print(expr);               // stdout

if (cond) { } else { }     // parens REQUIRED around the condition
while (cond) { }
for x in iterable { }      // NO parens
break; continue;

return;  return expr;

try { } catch (e) { }
try { } catch (e: DbError.UniqueViolation) { }   // typed catch, §12
                                                 // exactly ONE catch per try

transaction { }            // BEGIN/COMMIT, rollback on error
savepoint name { }         // nested boundary, only inside a transaction

validate body { }          // §10
```

`if`/`while` need parentheses; `for` must not have them. This asymmetry is the
single most common parse error.

---

## 7. Expressions and operators

| Category | Operators |
|---|---|
| Arithmetic | `+ - * / %`, unary `-` |
| Comparison | `== != < <= > >=` |
| Logic | `and` `or` `!` — **words, not `&&` / `\|\|`** |
| Ternary | `cond ? a : b` |
| Null-coalesce | `a ?? b` |
| Access | `obj.field` — arrays have **no** `[i]` syntax, see below |
| Literals | `[1, 2, 3]`, `{ key: value, other: 2 }` |
| Entity | `new Entity()` |
| Await | `await expr` (inside `async function`) |

`+` on two strings concatenates. Mixing a string and a number in `+` coerces
the number to text.

Arrays are references, and `push(arr, v)` **mutates in place** as well as
returning the array — `push(a, 2);` and `let b = push(a, 2);` do the same
thing to `a`, and `b` is not a copy. There is no index syntax: reach elements
with `first(arr)`, `last(arr)`, `take(arr, n)`, or `for x in arr`.

```jwc
let label = count > 0 ? "some" : "none";
let name  = req.name ?? "anonymous";
if (a == 1 and b != 2) { }
if (!ok or retries > 3) { }
```

---

## 8. The database layer

### 8.1 Select

```jwc no-compile
select [distinct] Entity [{ projection }] [with rel, ...] from CTX.Table
    [join Other on Other.col == Entity.col]
    [where COND [and|or COND ...]]
    [group by Entity.col, ...]
    [having AGG(col) OP value]
    [orderby Entity.col [asc|desc]]
    [limit N] [offset N]
    [first]
```

Note the shape: the **projection and `with` come before `from`**, everything
else after. The head is an entity name, `*`, or a scalar aggregate.

```jwc
function examples(id: int) {
    // whole rows
    let all = select User from AppDb.User;

    // exactly one row (or null) — NOT a one-element list
    let one = select User from AppDb.User where User.id == @id first;

    // scalars — count(*), sum, avg, min, max
    let n     = select count(*) from AppDb.User;
    let total = select sum(Order.total) from AppDb.Order where Order.paid == true;

    // projection: braces, not a comma list. `alias: expr` renames.
    let cols = select User { id, email } from AppDb.User;

    // eager-load declared navigations (dotted = two levels)
    let posts = select Post with author, comments from AppDb.Post;

    // join + grouped aggregate
    let grouped = select Order { user_id, n: count(*) } from AppDb.Order
        group by Order.user_id
        having count(*) > 3;
}
```

A `group by` **must** come with a projection naming the grouped columns —
without one the compiler would emit `SELECT t.*`, which Postgres rejects.

Navigations are declared on the entity, and are what `with` loads:

```jwc
entity Post of AppDb {
    id       int pk autoincrement;
    authorId int references User.id;
    comments: List<Comment> via Comment.postId orderby createdAt desc;
    tags:     List<Tag> via PostTag(postId, tagId);   // many-to-many
}
```

**`@name` binds a variable as a SQL parameter.** Always use it for anything
user-supplied — that is what makes the query injection-safe.

### 8.2 Where operators

`== != < <= > >=`, `like`, `ilike`, `in (a, b, c)`, `between a and b`,
`is null`, `is not null`.

Append `?` to make a predicate **optional** — it is dropped at runtime when
the bound value is `null` or `""`, so one static query serves an optional
filter:

```jwc
function byStatus() {
    let q = query_param("status");
    return select Order from AppDb.Order where Order.status ==? @q;
}
```

### 8.3 Mutations

```jwc
let u = new User();
u.email = req.email;
u.created_at = now();
insert u into AppDb.User;        // `u` is refreshed with generated columns

u.email = "new@example.com";
update u in AppDb.User;          // by primary key

delete u from AppDb.User;                              // by primary key
delete from AppDb.User where User.id == @id;           // by predicate
update AppDb.User set status = "archived" where User.id == @id;
```

### 8.4 Transactions

```jwc
transaction {
    insert a into AppDb.Account;
    savepoint transfer {
        update b in AppDb.Account;
    }
}
```

A literal `transaction { transaction { } }` is rejected (E016); `savepoint`
outside a transaction is E017.

### 8.5 Escape hatch

```jwc
let params = "[\"" + code + "\"]";                   // JSON array of params
let url = raw_sql("SELECT url FROM \"link\" WHERE code = $1", params);
```

`raw_sql` routes to a **query** when the statement starts with `SELECT` or
`WITH`, and to **exec** otherwise (returning the affected-row count). An
`UPDATE ... RETURNING` therefore gives you the row count, not the row — wrap
it in a CTE if you want the value:

```jwc no-compile
raw_sql("WITH b AS (UPDATE \"link\" SET hits = hits + 1 WHERE code = $1 RETURNING url) SELECT url FROM b", params)
```

### 8.6 Telemetry writes

```jwc
log_insert("ApiCall", row);
```

Hands the row to a background writer that batches a couple of thousand into
one multi-row `INSERT`, so the request does not wait for a database
round-trip. Use it for logs and metrics; use `insert` when the caller must
see the write. The first argument must be a **string literal** naming the
entity (E023).

Rows are dropped rather than queued without bound if the writer falls
behind. `/metrics` publishes `jwc_log_dropped_total`,
`jwc_log_written_total`, `jwc_log_batches_total`, `jwc_log_failed_total`,
`jwc_log_queue_depth` and `jwc_log_queue_capacity` — identically on both
backends. If you see drops, `written ÷ batches` is the number to look at
first: a low rows-per-batch means the writer is paying statement overhead
too often, and `JWC_LOG_BATCH` / `JWC_LOG_CONCURRENCY` are the knobs.

One caveat that is not a bug: under `jwc run` the runtime is
single-threaded, so a tight `main()` loop that never awaits starves the
drain task and fills the channel. Rows logged from route handlers are
unaffected — `serve` runs a multi-threaded runtime.

---

## 9. HTTP

### 9.1 Routes

```jwc no-compile
route GET  "/users"        { ... }          // leading slash optional
route POST "users"         { ... }          // same path as above
route GET  "/users/{id}"   { let id = path_param("id"); ... }
route GET  "/x" use Auth, RateLimit { ... } // middleware, in order
route GET  "/y" -> listUsers;               // delegate to a function
route WS   "/socket"       { ... }          // WebSocket
```

Methods: `GET POST PUT DELETE PATCH WS SSE`. A handler function's parameters
are filled from the path param of the same name, then the query param, then
`null`.

**Routes match in declaration order**, so a catch-all like `route GET "{code}"`
must be declared *after* every literal route it would otherwise swallow.

### 9.2 Reading the request

`path_param(name)`, `query_param(name[, default])`, `body()`, `header(name)`,
`client_ip()`, `request_id()`, `request_path()`, `request_method()`.

Inside an `after { }` block you also get `response_status()`,
`response_duration_ms()` and `response_duration_us()`. Prefer the
microsecond one for latency telemetry: a route that answers in under a
millisecond records `0` through `response_duration_ms`, and every
percentile computed from a column of zeros is also zero.

`body()` returns the parsed JSON body; read fields with `.`:

```jwc
let req = body();
let email = req.email;
```

### 9.3 Middleware

```jwc
middleware RateLimit {
    if (!redis.rate_limit("rl:" + client_ip(), 60, 60)) {
        return statusCode(429, { error: "rate limited" });
    }
}

middleware Metrics {
}
after {
    let row = new ApiCall();
    row.path       = request_path();
    row.status     = response_status();
    row.latency_ms = response_duration_ms();
    row.ts         = now();
    log_insert("ApiCall", row);
}
```

- The **request-phase** body runs before the handler. Returning any non-null
  value short-circuits the request with that value as the response; returning
  nothing lets it through.
- The **`after { }` block** — written after the middleware's closing brace —
  runs on the way out, in **reverse** middleware order, and runs **even when
  an earlier middleware short-circuited**, so a throttled request still
  reaches your metrics block. `response_status()` and `response_duration_ms()`
  are non-null only here.
- `setContext(key, value)` in a middleware, `context(key)` in the handler,
  is how you pass a value down (e.g. an authenticated user id).

### 9.4 Responses

| Helper | Status |
|---|---|
| `json(v)` / `ok(v?)` | 200 |
| `created(v)` | 201 |
| `noContent()` | 204 |
| `badRequest(v?)` | 400 |
| `unauthorized(v?)` | 401 |
| `forbidden(v?)` | 403 |
| `notFound(v?)` | 404 |
| `internalError(v?)` | 500 |
| `statusCode(code, body_or_headers?)` | anything |
| `text(v)` | 200, `text/plain; charset=utf-8` |
| `html(v)` | 200, `text/html; charset=utf-8` |
| `response(body, mime)` (alias `raw`) | 200, your mime |

Every helper has both `camelCase` and `snake_case` spellings.

**Returning a bare value works and defaults to 200 + `application/json`.** That
default is why `return { status: 429, error: "..." }` sends **HTTP 200** with a
body that merely mentions 429 — use `statusCode`. And why a hand-built XML or
SVG string needs `response(s, "application/xml")` rather than a bare `return s`.

A redirect is `statusCode(302, { Location: url })` — a 3xx with an object body
is read as a header map.

---

## 10. Validation

```jwc no-compile
route POST "/signup" {
    validate body {
        email:    required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
        password: required, minLength(8), maxLength(200);
        age:      min(18), max(120);
    }
    let req = body();
    ...
}
```

Rules: `required`, `minLength(n)`, `maxLength(n)`, `min(n)`, `max(n)`,
`pattern(regex)`. **Each field line ends with `;`.**

Semantics worth knowing:

- A **missing or null** field passes every rule except `required`. Only
  `required` checks presence.
- Only the **first** failing rule per field is reported.
- A wrong-typed value fails: `{"name": 5}` against `minLength(3)` reports
  `minLength(3): not a string`.

On failure the request short-circuits with **HTTP 400** and this envelope:

```json
{
  "code": "validation_failed",
  "details": { "email": "pattern(^[^@]+@[^@]+\\.[^@]+$)" },
  "error": "Request body failed validation",
  "status": 400
}
```

Every runtime error uses the same shape — `error`, `status`, `code`, and
`details` when there is structured detail.

---

## 11. Built-in functions

Complete surface as of 0.9.x. `name/N` is the accepted argument count;
`[interp-only]` means `jwc build --native` rejects the call.

**Strings** — `length/1`, `lower/1`, `upper/1`, `trim/1`, `contains/2`,
`starts_with/2`, `ends_with/2`, `replace/3`, `split/2`, `substring/3`,
`take/2`, `first/1`, `last/1`, `len/1` [interp-only]

**JSON** — `json_parse/1`, `json_stringify/1`, `set_json_field/3` [interp-only]

**HTTP request** — `path_param/1`, `query_param/1..2`, `body/0`, `header/1`,
`client_ip/0`, `request_id/0`, `response_status/0`, `response_duration_ms/0`,
`response_duration_us/0`, `request_path/0`, `request_method/0`,
`request_body/0` [interp-only]

**HTTP response** — `json/1`, `json_unchecked/1`, `text/1`, `html/1`,
`response/2` (alias `raw`), `ok/0..1`, `created/1`, `not_found/0..1`,
`no_content/0`, `unauthorized/0..1`, `forbidden/0..1`, `internal_error/0..1`,
`status_code/1..2`, `bad_request/0..1` — plus the camelCase spellings
`notFound`, `noContent`, `internalError`, `statusCode`, `badRequest`

**Database** — `setConnectionString/0..1`, `log_insert/2`, `raw_sql/1..2`,
`db_query/1` [interp-only], `set_connection_string/0..1` [interp-only]

**WebSocket** — `ws_send/1`, `ws_recv/0`, `ws_close/0`

**Async I/O** — `sleep_ms/1`, `http_get/1..2`, `fetch_json/1`,
`http_post/1..3` [interp-only]

**Console** — `console.write/1`, `console.writeln/1`, `console.error/1`,
`console.read/0`

**Files** — `file.read/1`, `file.write/2`, `file.append/2`, `file.exists/1`,
`file.delete/1`, `file.copy/2`, `file.move/2`, `file.size/1`, `file.lines/1`,
`directory.list/1`, `directory.create/1`, `directory.exists/1`,
`directory.delete/1`

**Environment + coercion** — `env/1`, `int/1`, `random_int/1..2`, `serve/0..1`

**Time + ids** — `now/0`, `uuid/0`, `unix_timestamp/0`

**Cache (in-process)** — `cache_get/1`, `cache_set/3`, `cache_del/1`,
`cache_clear/0`

**Redis (shared)** — `redis_get/1`, `redis_set/3`, `redis_del/1`,
`redis_exists/1`, `redis_incr/1`, `redis_expire/2`, `redis_eval/3`,
`redis_ping/0`, `redis_enabled/0`

**Arrays** — `range/1..3`, `push/2` (alias `append`), `join/2`

**Hashing + crypto** — `sha256/1`, `sha1/1`, `md5/1`, `hmac_sha256/2`,
`jwt_sign/2`, `jwt_verify/2`, `jwt_verify_jwks/2`, `hash_password/1`,
`verify_password/2`

**Email** — `send_email/3` [interp-only]

**Request context** — `context/1`, `setContext/2` (alias `set_context`),
`dispatch/2` [interp-only]

**Background jobs** (all [interp-only]) — `register_job_handler/2`,
`enqueue/2`, `enqueue_urgent/2`, `job_count/0`, `dlq_count/0`, `dlq_drain/0`

There is nothing else. Source of truth:
[`reference/builtins`](./builtins.md), generated from
`src/builtins.rs::BUILTIN_DEFS`.

---

## 12. Errors

```jwc
errorHandler (e) {
    return internalError(e.message);
}

function save(u: text) {
    try {
        insert u into AppDb.User;
    } catch (e: DbError.UniqueViolation) {
        return badRequest({ error: "email already registered" });
    }
}
```

**A `try` takes exactly one `catch`.** There is no chained
`catch (a) { } catch (b) { }` — nest to handle several kinds:

```jwc
function save(u: text) {
    try {
        try {
            insert u into AppDb.User;
        } catch (e: DbError.UniqueViolation) {
            return badRequest({ error: "email already registered" });
        }
    } catch (e) {
        return internalError(e.message);
    }
}
```

Catchable kinds (a bare parent catches all its subtypes):

- `Error`
- `DbError` · `.UniqueViolation` `.ForeignKeyViolation` `.NotNullViolation`
  `.CheckViolation` `.SerializationFailure` `.DeadlockDetected`
  `.ConnectionFailure`
- `HttpError` · `.NotFound` `.Unauthorized` `.Forbidden` `.BadGateway`
- `JwtError` · `.InvalidSignature` `.Expired`
- `IoError` · `.NotFound` `.PermissionDenied` `.AlreadyExists`
- `RedisError` · `.ConnectionFailure` `.TimedOut` `.NoScript` `.LoadingError`
- `ValidationError`, `TimeoutError`

An unknown kind is a compile error with a "did you mean" hint. Uncaught errors
become a 500 in the standard envelope, with the detail in the server log
against the `x-request-id` header (set `JWC_DEBUG_ERRORS=1` to put it in the
response while developing).

---

## 13. Async

```jwc
async function loadProfile(id: text): text {
    let user = await http_get("https://api.example.com/u/" + id);
    return user;
}
```

`await` is only valid inside an `async function`. Awaiting yields to the
scheduler, so concurrent requests do not block each other. Route bodies are
already async — you do not need `async` on a route to call an async builtin.

---

## 14. Namespaces, packages, mount, group

Within one project, everything is one flat namespace and no imports are
needed. Namespaces exist for **packages**:

```jwc no-compile
// inside the package
namespace mylib;
public function greet(name: text): text { return "hi " + name; }
private function helper() { }

// inside the consuming app
import mylib;
let s = mylib.greet("world");
```

Declarations are **private by default**; a cross-namespace call to a private
declaration is E021.

```jwc no-compile
mount authpkg at "/auth";        // activate a package's routes under a prefix

group "/api/v1" use Auth {       // shared prefix + middleware for inner routes
    route GET "/me" { ... }      // → /api/v1/me, with Auth
}
```

Dependencies live in `jwcproj.json` and are pinned in `jwcproj.lock`:

```bash
jwc add redis                 # from the registry
jwc add mylib --path ../mylib # local path
jwc install
```

The **`redis` package** wraps the `redis_*` builtins with a friendlier API and
falls back to the in-process cache when no Redis is configured:
`redis.get/set/del/incr/expire/exists`, `redis.get_json/set_json`,
`redis.rate_limit(key, limit, window_secs)`, `redis.available()`.

---

## 15. Native AOT build

```bash
jwc build --native --release      # → bin/release/<app>, one static binary
```

Linux x86_64 (and musl) only — a cross-target matrix is an explicit non-goal.
Codegen emits Rust and shells out to `cargo`, so the first build takes ~1
minute.

Rules that only apply here:

1. **Every built-in marked `[interp-only]` in [§11](#11-built-in-functions) is
   rejected.** Most importantly: the whole **background-job API**
   (`register_job_handler`, `enqueue`, …), `http_post`, `send_email`,
   `db_query`, `dispatch`. If a native binary is the target, do not reach for
   those. `log_insert` covers the common "write it later" case natively.
2. **`main()` must call `serve(port)`.** The generated binary serves only what
   `main()` starts; there is no `jwc serve` wrapper.
3. **`jwc check` is not enough.** It accepts calls that native codegen
   rejects. Run the real `jwc build --native` before claiming a native app
   works.
4. `GET /metrics` is served automatically (Prometheus text format: Postgres
   pool, Redis pool, buffered-writer series) unless the program declares its
   own `/metrics` route.

Behaviour is otherwise identical to the interpreter, and that parity is
tested. If you find a difference, it is a compiler bug — report it rather than
working around it.

---

## 16. Configuration

Postgres comes from `JWC_DATABASE_URL` / `DATABASE_URL`, or the `PG_*` set:

```env
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=myapp
```

Call `setConnectionString()` at the top of `main()` to load them.

Frequently needed vars:

| Var | Default | Purpose |
|---|---|---|
| `JWC_DATABASE_URL` | — | full Postgres URL |
| `JWC_REDIS_URL` | unset | Redis; unset means the `redis_*` builtins are disabled, not broken |
| `JWC_LOG_QUEUE` / `JWC_LOG_BATCH` / `JWC_LOG_FLUSH_MS` / `JWC_LOG_CONCURRENCY` | 10000 / 2000 / 200 / 4 | `log_insert` writer |
| `JWC_HTTP_ALLOWLIST` | unset | CSV host allowlist for outbound HTTP (SSRF guard) |
| `JWC_DEBUG_ERRORS` | false | put error detail in the response body |
| `JWC_PRINT_CONFIG` | false | print the resolved config table at boot |

Full table: [`deployment/env-vars`](../deployment/env-vars.md).

Built-in endpoints: `/healthz` (liveness), `/readyz` (pings the DB and, when
configured, Redis), `/metrics` (Prometheus).

---

## 17. Gotchas, ranked by how often agents hit them

These are real failures observed from agents writing JWC, not hypotheticals.

**1. Inventing a built-in, and `jwc check` saying OK.** `toInt` (it's `int`),
`query` (it's `query_param`), `parseInt`, `console.log`, `len` on a native
build. The type checker only validates *arity* for names it recognises; an
unrecognised name is assumed to be a user function and passes. It then fails
with `Unknown function: toInt` at runtime, or `E022` at native build time.
**Check §11 before writing any call you have not already used.**

**2. `return { status: 429, ... }`.** Sends HTTP 200. Use `statusCode(429, ...)`.

**3. Returning a hand-built XML/SVG/CSV string.** Gets `application/json`. Use
`response(body, mime)`.

**4. `for (x in items)`.** Parse error — `for` takes no parens, while `if` and
`while` require them.

**5. `after { }` inside the middleware braces.** Parse error at `after`. It
goes after the closing `}`:

```jwc
middleware M {
}
after {
}
```

**6. Missing `;` in `validate body`.** Every field line needs one.

**7. `route GET "/x" middleware [A, B]`.** The keyword is `use`, and the list
is bare: `route GET "/x" use A, B`.

**8. `&&` / `||`.** JWC uses the words `and` / `or`.

**9. Forgetting `serve(port)` in `main()`** — under `jwc run` the program
prints and exits; the native binary does the same.

**10. Writing `raw_sql` against the entity name.** The table is snake_case:
`entity ApiCall` → `"api_call"`.

**11. Expecting `UPDATE ... RETURNING` from `raw_sql` to return the row.** It
returns the affected-row count unless the statement starts with `SELECT` or
`WITH`. Wrap it in a CTE.

**12. Reaching for a lambda.** There are no first-class functions, so there is
no `map`/`filter`/`sort` taking a callback, and no `remember(key, ttl, fn)`
cache-aside helper. Write the loop.

**13. Assuming `select ... first` returns a list.** It returns the row itself
(or null). Without `first` you get a list.

**14. Interpolating user input into SQL.** Use `@var` binding, or `$1`
parameters with `raw_sql`.

**15. Chaining `catch` clauses.** One `catch` per `try`; nest for more.

**16. Writing a projection as a comma list.** It is
`select User { id, email } from ...`, in braces, and the projection and any
`with` come **before** `from`.

**17. `/* block comments */`.** Not a thing. `//` only, anywhere.

**18. Declaring a catch-all before the routes it shadows.** Matching is
first-match in declaration order and there is no specificity ranking, so a
`route GET "{code}"` written above `route GET "/late"` answers `/late`
itself — with no warning, and a perfectly valid-looking wrong response.
Catch-alls go last.

---

## 18. A complete service

A URL shortener with rate limiting, buffered request logging, validation and a
redirect. `jwc add redis` first — the `RateLimit` middleware uses that package.

```jwc
import redis;

dbcontext AppDb: Postgres;

entity Link of AppDb {
    code       varchar(16) pk;
    url        text;
    hits       int;
    created_at datetime;
}

entity ApiCall of AppDb {
    id         int pk autoincrement;
    path       text;
    method     varchar(8);
    status     int;
    latency_ms int;
    ts         datetime;
}

// ---- middleware -------------------------------------------------------

middleware RateLimit {
    if (!redis.rate_limit("rl:" + client_ip(), 60, 60)) {
        return statusCode(429, { error: "rate limited" });
    }
}

middleware Metrics {
}
after {
    let row = new ApiCall();
    row.path       = request_path() ?? "unknown";
    row.method     = request_method() ?? "GET";
    row.status     = response_status();
    row.latency_ms = response_duration_ms();
    row.ts         = now();
    log_insert("ApiCall", row);
}

// ---- routes -----------------------------------------------------------

route POST "/api/links" use RateLimit, Metrics {
    validate body {
        url: required, pattern(r"^https?://");
    }
    let req = body();

    let l = new Link();
    l.code       = substring(uuid(), 0, 7);
    l.url        = req.url;
    l.hits       = 0;
    l.created_at = now();
    insert l into AppDb.Link;

    return created({ code: l.code, url: l.url });
}

route GET "/api/links/{code}" use Metrics {
    let code = path_param("code");
    let link = select Link from AppDb.Link where Link.code == @code first;
    if (link == null) {
        return notFound({ error: "no such link" });
    }
    return ok({ code: link.code, url: link.url, hits: link.hits });
}

// Catch-all: declared LAST so it does not swallow the routes above.
route GET "{code}" use RateLimit, Metrics {
    let code = path_param("code");
    let params = "[\"" + code + "\"]";
    let url = raw_sql(
        "WITH b AS (UPDATE \"link\" SET hits = hits + 1 WHERE code = $1 RETURNING url) SELECT url FROM b",
        params
    );
    if (url == null or url == "") {
        return notFound({ error: "no such link" });
    }
    return statusCode(302, { Location: url });
}

errorHandler (e) {
    return internalError(e.message);
}

function main() {
    setConnectionString();
    serve(8080);
}
```

```bash
jwc check main.jwc && jwc lint
jwc migrate new init && jwc migrate up
jwc run
```

---

## 19. Before you say it's done

- [ ] `jwc check <file>` passes — catches parse and schema errors.
- [ ] `jwc lint` is clean — catches dead code and unused middleware.
- [ ] `jwc fmt --check` passes.
- [ ] **Every built-in you called appears in [§11](#11-built-in-functions).**
      `jwc check` will not catch a typo here.
- [ ] The service actually starts, and you have **called each route** and
      checked the **status code** as well as the body — a 200 carrying an
      error payload is the failure mode this language makes easiest.
- [ ] If the target is a native binary: `jwc build --native` succeeds, and no
      `[interp-only]` built-in is used.
- [ ] Migrations generated and applied for every schema change
      (`jwc migrate new <name>` diffs against the last one).

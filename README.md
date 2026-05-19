# JWC

JWC is a small backend-focused language for building API + database applications with simple syntax.

This README is a quick, practical guide.

## Quick Start

1. Create a project:

```bash
jwc new myapp
cd myapp
```

2. Create `.env` in project root:

```env
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=myapp
```

3. Create database (once):

```bash
createdb myapp
```

4. Create and run migrations:

```bash
jwc migrate add init-db
jwc migrate up
```

5. Run app:

```bash
jwc run
```

Server default: `http://0.0.0.0:8080`

## Minimal Example

```jwc
dbcontext AppDbContext : Postgres;

entity Brand of AppDbContext {
    id int pk;
    name varchar(255);
}

function getAllBrands() {
    return select BrandEntity from AppDbContext.BrandEntity;
}

route GET "api/brands" {
    return json(getAllBrands());
}

function main() {
    setConnectionString(`postgresql://${env("PG_USER")}:${env("PG_PASSWORD")}@${env("PG_HOST")}:${env("PG_PORT")}/${env("PG_DATABASE")}`);
    serve(8080);
}
```

## OOP-Style Grouping (dome)

JWC now supports static-class style function grouping with `dome`.

- Functions declared inside a `dome` are not global.
- They must be called via `DomeName.functionName(...)`.

Example:

```jwc
dome BrandService {
    function getAll() {
        return select BrandEntity from AppDbContext.BrandEntity;
    }
}

function main() {
    let brands = BrandService.getAll();
    print(brands);
}
```

## DTO / View Models (class)

Besides DB `entity`, JWC supports non-persistent model declarations:

- `class Name { ... }`

These are useful for DTO/View modeling and typed parameters, while SQL generation remains scoped to `entity` declarations only.

When a function parameter or return type is annotated with a known `class`/`entity` type, JWC now validates JSON payloads automatically:

- `body()` values are parsed/validated automatically for typed params.
- `select ...` JSON results are also validated when passed/returned as typed models.

Practical example:

```jwc
class BrandCreateRequest {
    id int;
    name string;
}

dome BrandService {
    function createBrand(data: BrandCreateRequest): BrandEntity {
        let brand = new BrandEntity();
        brand.id = data.id;
        brand.name = data.name;
        insert brand into AppDbContext.BrandEntity;
        return brand;
    }
}

route POST "api/brands" {
    // body() is validated/mapped against BrandCreateRequest automatically
    let createdBrand = BrandService.createBrand(body());
    return created(createdBrand);
}
```

## CLI

```bash
jwc --help
```

Main commands:

- `jwc new <name>`: Create a new project
- `jwc run [path]`: Run `main()` from project/file
- `jwc run [path] --request-logging`: Enable per-request console logs
- `jwc serve [path] --port 8080`: Start HTTP server directly
- `jwc serve [path] --request-logging`: Enable per-request console logs
- `jwc serve [path] --watch`: Watch `.jwc` files and restart on change
- `jwc test`: Validate project
- `jwc lint`: Validate + emit dead-code warnings (unused functions, unused middleware)
- `jwc check <file>`: Parse/validate one file
- `jwc gen-sql <file>`: Generate PostgreSQL schema SQL from entities
- `jwc migrate add <name>`: Alias for creating migration files
- `jwc migrate new <name>`: Create migration files
- `jwc migrate up`: Apply pending migrations
- `jwc migrate down [--steps N]`: Rollback the most recent applied migration(s)

Request logging is disabled by default.

## Query and Path Parameters

```jwc
route GET "users/{id}" {
    let id = path_param("id");
    let limit = query_param("limit", "20");  // 2nd arg is a default
    return json("user=" + id + ",limit=" + limit);
}
```

- `path_param(name)` reads a `{name}` placeholder from the route path.
- `query_param(name)` reads `?name=...` from the URL. Returns `null` if missing,
  or the provided default when a 2nd argument is given.

### Typed handler routes

When you bind a handler with `route GET "..." -> fn;`, JWC matches the
handler's typed parameters against the route's path placeholders (and
query string, as a fallback), and coerces each value to the declared type:

```jwc
function getUser(id: int) {
    return "user=" + id;
}

route GET "users/{id}" -> getUser;
```

## Validating Request Bodies

Use a `validate body { ... }` block inside a route or handler. Supported rules:
`required`, `minLength(n)`, `maxLength(n)`, `min(n)`, `max(n)`, `pattern("regex")`.

```jwc
route POST "users" {
    validate body {
        name: required, minLength(2), maxLength(60);
        age: min(0), max(150);
    }
    let payload = body();
    return created(payload);
}
```

On failure, JWC short-circuits and returns:

```json
{ "errors": { "name": "required" } }
```

with HTTP status **400**.

## Background jobs

Run work off the request path with a tiny in-process queue:

```jwc
function sendWelcome(payload_json) {
    let user = json_parse(payload_json);
    send_email(user.email, "Welcome!", "<p>Hi " + user.name + "</p>");
}

route POST "register" {
    let req = body();
    let u = new User();
    u.id = uuid();
    u.email = req.email;
    u.name = req.name;
    insert u into AppDb.User;

    register_job_handler("welcome", "sendWelcome");
    enqueue("welcome", json_stringify({ name: req.name, email: req.email }));

    return created({ ok: true });
}
```

- `register_job_handler(name, fn_name)` wires a queue name to a JWC function;
  the compiler verifies the function exists.
- `enqueue(name, payload_json)` returns immediately and pops the job onto
  one of the queue's worker threads (default 2, `JWC_QUEUE_WORKERS` to tune).
- `job_count()` reports pending size for health checks.
- The queue lives for the lifetime of `serve(...)`; no persistence yet —
  jobs in flight are lost on process restart.

## Editor support

`cargo build --bin jwc-lsp` produces a Language Server. Point your editor at it
to get:

- Live diagnostics from `parse_program` / `validate_program` / `lint_program`.
- Hover info on entities, classes, and functions.

It speaks stdio LSP; standard tower-lsp setup applies.

## Global error handler

Declare one top-level `errorHandler` to convert any uncaught route error
into a structured response:

```jwc
errorHandler (e) {
    return internalError(e.message);
}
```

- The handler runs after a route body or `-> fn` handler returns `Err`.
- `e` is bound to `{ "message": "...", "causes": [...] }` JSON.
- Without a handler, runtime errors still surface as HTTP 500.

## Raw string literals

Backslashes inside regular `"..."` strings are escape sequences (`\n`, `\t`,
`\"`). For regex patterns and Windows paths, the raw form is friendlier —
backslashes are kept verbatim:

```jwc
validate body {
    email: pattern(r"^[^@]+@[^@]+\.[^@]+$");
}
```

## Object & boolean ergonomics

```jwc
// JSON object literal — strings, numbers, bools, nested arrays/objects
// are all embedded raw (no double-encoding).
return json({ items: posts, total: count, healthy: true });

// Unary `!` for boolean negation
if (!verify_password(req.password, user.password_hash)) {
    return unauthorized();
}

// `now()` returns current UTC time as an RFC 3339 string.
// `unix_timestamp()` returns the same instant as an int.
let created_at = now();
let secs = unix_timestamp();

// `where ... == @req.username` reaches into the request payload directly,
// no temporary `let` needed:
let u = select User from db.Users where User.username == @req.username first;
```

## Strings, arrays, iteration

```jwc
lower("HELLO")           // "hello"
upper("najim")           // "NAJIM"
trim("  hi  ")           // "hi"
replace("a-b-c", "-", "/")  // "a/b/c"
contains("hello world", "world")  // true
starts_with("hello", "he")        // true
ends_with("hello", "lo")          // true
length("hello")          // 5
split("a,b,c", ",")      // "[\"a\",\"b\",\"c\"]"
first("[1,2,3]")         // 1
last("[1,2,3]")          // 3
length("[1,2,3]")        // 3
length({ a: 1, b: 2 })   // 2 — key count on an object literal

let xs = select Post from db.Posts where Post.draft == false;
for p in xs {
    print(p.title);
    if (p.id == "x") { break; }
}

let parsed = json_parse("{\"a\":1}");
let back   = json_stringify({ a: 1 });
```

- `for VAR in EXPR { ... }` iterates a JSON array. `EXPR` can be a `select`
  result, a `body()` payload, a literal `"[ ... ]"` — anything that parses
  into a JSON array. `break` / `continue` / `return` all work inside.
- `contains` works on substrings, JSON-array elements, and JSON-object keys.
- `json_parse` lifts a JSON string into the runtime's untagged Value;
  `json_stringify` does the reverse.

## HTTP Client

```jwc
let res = http_get("https://api.example.com/users");
print(res);                          // { "status": 200, "body": { ... } }

let posted = http_post(
    "https://api.example.com/users",
    "{\"name\":\"Najim\"}",
    "{\"x-api-key\":\"abc\"}"
);
```

- Returns `{ "status": N, "body": <JSON or string> }`.
- 2nd arg of `http_post` is the request body; pass `null` for empty.
- 3rd arg (optional) is a JSON object of headers.

## Password hashing (Argon2id)

```jwc
let hash = hash_password("hunter2");          // PHC-format string
let ok   = verify_password("hunter2", hash);  // → true
let bad  = verify_password("wrong", hash);    // → false
```

- `hash_password` uses Argon2id with a freshly generated random salt — each
  call returns a different hash even for the same input.
- `verify_password` returns a bool; it throws only if the stored hash is
  malformed, so wrap it in `try` only when reading untrusted hash storage.

## JWT (HS256)

```jwc
let token = jwt_sign("{\"sub\":\"u-1\",\"exp\":9999999999}", "secret");
let claims = jwt_verify(token, "secret");      // returns payload JSON
```

- Algorithm: `HS256`. Tokens from other algorithms are rejected.
- `jwt_verify` returns the decoded payload on success and throws on
  signature mismatch / expired secret / malformed token — pair it with
  `try { ... } catch (e) { return unauthorized(); }`.

## Cache (in-memory, TTL)

A dependency-free process-wide string cache. Useful for JWT validation
results, per-user query results, short-lived rate-limit counters, etc.

```jwc
cache_set("user:42", userJson, 60);   // expires in 60 seconds
let hit = cache_get("user:42");        // → string, or null when missing/expired
cache_del("user:42");                  // remove a single key
cache_clear();                         // drop everything
```

- `cache_set(key, value, ttl_secs)` — `ttl_secs == 0` means "no expiration".
- `cache_get(key)` returns the cached string, or `null` if the key is
  missing or its TTL has already elapsed (expired entries are evicted
  lazily on read).
- `cache_del(key)` / `cache_clear()` return `void`.
- The store lives for the lifetime of the process — it does not persist
  across restarts. Use a real cache (Redis, etc.) if you need durability.

No environment variables are required.

## Email (SMTP)

```jwc
send_email(
    "alice@example.com",
    "Welcome to Acme",
    "<h1>Hello</h1><p>Confirm your address:</p><a href=\"...\">link</a>"
);
```

`send_email(to, subject, body_html)` performs a real SMTP send through a
cached `SmtpTransport`. The body is sent as `Content-Type: text/html;
charset=utf-8`.

Required environment variables:

| Env var              | Default     | Purpose                                              |
|----------------------|-------------|------------------------------------------------------|
| `JWC_SMTP_HOST`      | (required)  | SMTP server hostname, e.g. `smtp.gmail.com`          |
| `JWC_SMTP_PORT`      | `587`       | Server port                                          |
| `JWC_SMTP_USER`      | (required)  | SMTP login username                                  |
| `JWC_SMTP_PASSWORD`  | (required)  | SMTP login password / app token                      |
| `JWC_SMTP_FROM`      | (required)  | `Display Name <addr@host>` formatted From            |
| `JWC_SMTP_TLS`       | `starttls`  | `starttls` (default) \| `tls` (implicit/465) \| `none` |

The transport is built on the first call and reused thereafter. Missing
`JWC_SMTP_HOST` produces a clean `send_email: JWC_SMTP_HOST is required`
error so the caller can `try/catch` it. TLS uses `rustls` (no OpenSSL
dependency).

## SQL Clauses

`select` supports `where` (with `and`/`or`/parens), `orderby`, `limit`, `offset`, and `first`:

```jwc
function listAdults(country, min) {
    return select User from db.Users
        where (User.age >= @min and User.country == @country)
           or User.is_admin == true
        orderby User.created_at desc
        limit 20 offset 0;
}

function findOne(id) {
    return select User from db.Users where User.id == @id first;
}

function searchByEmail(prefix) {
    return select User from db.Users where User.email like @prefix;
}

function admins() {
    return select User from db.Users where User.role in ("admin", "owner");
}

function totalIn(country) {
    return select count(*) from db.Users where User.country == @country;
}
```

- Compound `where`: `and`/`or` with parentheses; `and` binds tighter than `or`.
- Operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `like`, `ilike`, `in (...)`,
  `between @a and @b`, `is null`, `is not null`.
- `orderby <field> [asc|desc]` — default direction is ascending.
- `limit N` / `offset N` accept integer literals or `@param` references.
- `first` forces `LIMIT 1` and returns a single row instead of an array.
- Aggregations: `select count(*)`, `select sum|avg|min|max(Entity.col) from ...`.
- Projection: `select User { name, email } from ...` — emits only the named
  columns. Every name is checked against the entity's declared fields at
  compile time. Combines with `with rel` (relations are added on top of the
  picked columns).

## Entity relations (navigation + auto-JOIN)

Declare navigation properties on the entity and pull them in with `select ... with`:

```jwc
entity User of AppDb {
    id uuid pk;
    name varchar(60);
    posts: List<Post> via Post.user_id;       // one-to-many
    profile: Profile via Profile.user_id;     // one-to-one
}

entity Post of AppDb {
    id uuid pk;
    user_id uuid references User.id on delete cascade;
    title varchar(200);
}

function getUserWithPosts(id) {
    return select User with posts, profile from AppDb.User
        where User.id == @id first;
}
```

- `List<T> via T.fk` materialises into a JSON array via a correlated
  `json_agg(...)` subquery (empty array when there are no children).
- `T via T.fk` materialises into a single nested object (or `null`).
- The navigation name, the target entity, and the FK column on the target are
  all checked at compile time.

## Transactions

Wrap a sequence of DML statements in `transaction { ... }` to run them
atomically — an uncaught error rolls back, success commits.

```jwc
transaction {
    insert user into AppDb.User;
    insert profile into AppDb.Profile;
}
```

Nested transactions are not supported; queries inside the block route through
the held connection automatically.

## Raw SQL escape hatch

```jwc
let rows = raw_sql(
    "SELECT json_agg(row_to_json(t))::text FROM users t WHERE created_at > $1",
    "[\"2026-01-01T00:00:00Z\"]"
);
let n_changed = raw_sql("DELETE FROM logs WHERE level = $1", "[\"debug\"]");
```

- Second argument is a JSON array of bound parameters — fully parameterized.
- `SELECT`/`WITH` shape returns the first column as text; other shapes return
  the affected row count.

## Async / Await (forward-compatible syntax)

```jwc
async function fetchUser(id: uuid): User {
    let row = await select User from db.Users where User.id == @id first;
    return row;
}
```

The parser accepts `async function` and `await expr` so existing code can adopt
the syntax now, but JWC still executes everything synchronously today — there
is no real future or scheduler. The actual non-blocking runtime (`tokio` +
`hyper`) is tracked as the next major effort, after Phase 2 ships.

## Middleware

Declare reusable request-time logic with `middleware Name { ... }` and attach
it to a route using `use`:

```jwc
middleware AuthMw {
    let token = header("authorization");
    if (token == null) {
        return unauthorized();
    }
    setContext("userId", verifyJwt(token));
}

route GET "api/me" use AuthMw {
    return json({ id: context("userId") });
}
```

- A middleware that `return`s a value short-circuits the request with that
  body and status (derived from the `status` JSON field, default 200).
- `header(name)` (case-insensitive) reads inbound headers.
- `setContext(key, value)` / `context(key)` share per-request state between
  middleware and the handler.
- Multiple middlewares: `route GET "..." use AuthMw, RateLimitMw { ... }`.

## Type System

Built-in types recognised in function signatures and JSON body validation:

| Type | Notes |
|------|-------|
| `string`, `int`, `bigint`, `double`, `decimal`, `bool` | primitives |
| `uuid` | RFC 4122 textual form (`8-4-4-4-12`) |
| `datetime` | ISO 8601 string (year-month-day prefix is validated) |
| `json` | any valid JSON value |
| `T?` / `Optional<T>` | nullable — `null` accepted |
| `List<T>` | JSON array; every element must match `T` |
| Custom `class` / `entity` names | runtime JSON schema check |

```jwc
function createUser(id: uuid, joined: datetime, tags: List<string>): User? {
    ...
}
```

## Error Handling

Wrap fallible statements in `try { ... } catch (var) { ... }`. The catch
variable is bound to a JSON object `{ "message": "...", "causes": [...] }`
which you can pass to `internalError(e)` or inspect via field access.

```jwc
try {
    insert car into db.Cars;
} catch (e) {
    return internalError(e);
}
```

> Typed catch (`catch (e: DbError)`) parses but currently matches all errors —
> first-class error types come with Phase 2.1.

## Foreign Keys

Declare relations directly on an entity field using `references EntityName.column`.
JWC emits the matching `FOREIGN KEY ... REFERENCES ...` constraint when generating
schema SQL and validates the reference at compile time.

```jwc
entity User of AppDbContext {
    id uuid pk;
    email varchar(120);
}

entity Post of AppDbContext {
    id uuid pk;
    title varchar(200);
    author_id uuid references User.id on delete cascade;
}
```

Supported actions: `on delete cascade`, `on delete restrict`, `on delete set null`.
If omitted, the database default (`NO ACTION`) applies.

> Navigation properties and an auto-JOIN `select User with posts ...` syntax
> are tracked for a follow-up iteration.

## Compile-Time DB Validation

JWC validates dbcontext and entity usage at compile-time:

- `entity X of AppDbContext { ... }` binds entity to a specific dbcontext.
- If multiple dbcontexts are declared, `of <DbContextName>` is required for entities.
- `select/insert/update/delete` must use a known dbcontext.
- `select Entity from Ctx.Table` checks entity-context compatibility.
- Unknown or mismatched table/entity references fail validation early.
- `where Entity.col == ...` and `orderby Entity.col` check that `col` is a real
  column on the entity. Misspelled columns fail before the server starts.

Example:

```jwc
dbcontext AppDbContext : Postgres;
entity TodoEntity of AppDbContext {
    id uuid pk;
    title varchar(200);
}

function getAll() {
    return select TodoEntity from AppDbContext.TodoEntity;
}
```

## Example Project: testapp

The repository includes a ready example at `examples/testapp`.

```bash
cd examples/testapp
jwc test
jwc migrate up
jwc run
```

Optional request logs:

```bash
jwc run --request-logging
```

## Supported Drivers

PostgreSQL is currently the only supported `dbcontext` driver. Multi-driver
support (Redis, Clickhouse, etc.) is on the Phase 2 roadmap.

```jwc
dbcontext AppDbContext : Postgres;   // only this works today
```

## Database Runtime

JWC now uses:

- PostgreSQL driver-based execution (no per-query `psql` subprocess)
- Connection pool
- Parameterized SQL execution
- Query-shape compilation cache
- Optional result cache with TTL

## Useful Environment Variables

Database:

- `DATABASE_URL` or `JWC_DATABASE_URL`

DB engine tuning:

- `JWC_DB_POOL_SIZE` (default `16`)
- `JWC_DB_MIN_IDLE` (optional — keeps at least N idle connections warm)
- `JWC_DB_MAX_LIFETIME_SECS` (default `1800` / 30 min, `0` disables)
- `JWC_DB_IDLE_TIMEOUT_SECS` (default `600` / 10 min, `0` disables)
- `JWC_DB_CONNECTION_TIMEOUT_SECS` (default `5`)
- `JWC_QUERY_CACHE_TTL_SECS` (optional, enables result cache when `> 0`)

TLS:

- `JWC_DB_TLS` — set to `1` / `true` / `yes` / `on` (case-insensitive) to
  open Postgres connections over TLS via `native-tls`. Required for hosted
  deployments that enforce SSL (Heroku Postgres, AWS RDS with `rds.force_ssl`,
  Supabase, etc.). Default is off — connections use `NoTls`.
- `JWC_DB_TLS_INSECURE_SKIP_VERIFY` — set to `1` / `true` to disable
  certificate and hostname verification. **Development only** — never enable
  this in production. The flag has no effect unless `JWC_DB_TLS` is also on.

Both the runtime pool and the `jwc migrate up` / `down` CLI honour these
flags, so a single env setting covers app traffic and schema migrations.

Server tuning:

- `JWC_SERVER_WORKERS` (default: CPU parallelism, min 2)
- `JWC_SERVER_QUEUE_CAPACITY` (default: workers x 64, min 64)
- `JWC_SERVER_METRICS` (`false` by default; set to `true` to enable)
- `JWC_SERVER_METRICS_INTERVAL_SECS` (default `10`)

## Install / Reinstall

Windows:

```powershell
./install.ps1 -Release
```

Linux/macOS:

```bash
./install.sh --release
```

After install, open a new terminal if `jwc` is not found immediately.

## Build (Bundle) — Debug/Release

`jwc build` (alias: `jwc bundle`) packages your project together with the JWC
runtime into `bin/{debug,release}`. This is **not** native AOT compilation yet —
the launcher invokes the embedded runtime to execute your `.jwc` sources.
A real native compiler is on Phase 4 of `ROADMAP.md`.

The build uses your current machine OS/architecture automatically.

Windows (PowerShell):

```powershell
./build.ps1 -Debug
./build.ps1 -Release
```

Windows (cmd):

```bat
build.cmd --debug
build.cmd --release
```

Linux/macOS:

```bash
./build.sh --debug
./build.sh --release
```

Output binaries:

- Windows debug: `target/debug/jwc.exe`
- Windows release: `target/release/jwc.exe`
- Linux/macOS debug: `target/debug/jwc`
- Linux/macOS release: `target/release/jwc`

Project-level native artifacts:

- `jwc build` now generates a native project launcher inside `bin/debug`.
- `jwc build --release` generates it inside `bin/release`.
- On Windows this output is a real `.exe` (for example: `bin/debug/myapp.exe`).
- `jwc run` on a project also refreshes the debug launcher automatically.

This keeps interpreter-style development flow (`jwc run`) and also gives compiled native artifacts for distribution.

## Notes

- `.env` is loaded automatically from project root.
- `jwc run -- test` is not the same as `jwc test`; use `jwc test` for project validation.
- If `jwc run` fails with `os error 10048`, port `8080` is already in use. Stop the process using that port, or run on another port: `jwc serve --port 8081`.

## Error Handling

JWC CLI now prints detailed errors in a `try/catch`-style format:

- Top-level message
- Full cause chain (`Caused by[0]`, `Caused by[1]`, ...)
- Optional backtrace hint

Example:

```bash
jwc check missing-file.jwc
```

Output shape:

```text
Unhandled JWC error:
    Message: Failed to read missing-file.jwc
    Caused by[0]: The system cannot find the file specified. (os error 2)
```

For runtime HTTP errors, JWC logs detailed error chain to console and returns a safe JSON error response.

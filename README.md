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

## JWT (HS256)

```jwc
let token = jwt_sign("{\"sub\":\"u-1\",\"exp\":9999999999}", "secret");
let claims = jwt_verify(token, "secret");      // returns payload JSON
```

- Algorithm: `HS256`. Tokens from other algorithms are rejected.
- `jwt_verify` returns the decoded payload on success and throws on
  signature mismatch / expired secret / malformed token — pair it with
  `try { ... } catch (e) { return unauthorized(); }`.

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
```

- Compound `where`: `and`/`or` with parentheses; `and` binds tighter than `or`.
- `orderby <field> [asc|desc]` — default direction is ascending.
- `limit N` / `offset N` accept integer literals or `@param` references.
- `first` forces `LIMIT 1` and returns a single row instead of an array.

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
- `JWC_QUERY_CACHE_TTL_SECS` (optional, enables result cache when `> 0`)

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

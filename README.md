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
- `jwc test`: Validate project
- `jwc check <file>`: Parse/validate one file
- `jwc gen-sql <file>`: Generate PostgreSQL schema SQL from entities
- `jwc migrate add <name>`: Alias for creating migration files
- `jwc migrate new <name>`: Create migration files
- `jwc migrate up`: Apply pending migrations

Request logging is disabled by default.

## Compile-Time DB Validation

JWC validates dbcontext and entity usage at compile-time:

- `entity X of AppDbContext { ... }` binds entity to a specific dbcontext.
- If multiple dbcontexts are declared, `of <DbContextName>` is required for entities.
- `select/insert/update/delete` must use a known dbcontext.
- `select Entity from Ctx.Table` checks entity-context compatibility.
- Unknown or mismatched table/entity references fail validation early.

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

## Native Build (Debug/Release)

Build uses your current machine OS/architecture automatically.

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

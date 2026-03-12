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
entity Brand {
    id int pk;
    name varchar(255);
}

dbcontext AppDbContext : Postgres;

function getAllBrands() {
    return select Brand from AppDbContext.Brands;
}

route GET "api/brands" {
    return json(getAllBrands());
}

function main() {
    setConnectionString(`postgresql://${env("PG_USER")}:${env("PG_PASSWORD")}@${env("PG_HOST")}:${env("PG_PORT")}/${env("PG_DATABASE")}`);
    serve(8080);
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

## Notes

- `.env` is loaded automatically from project root.
- `jwc run -- test` is not the same as `jwc test`; use `jwc test` for project validation.

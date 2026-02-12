# JWC (Just Web Code) — prototype

JWC is a small Rust CLI + parser/interpreter prototype.

One `.jwc` file can contain:

- DB DSL: `context`, `entity`, `select` → validate + generate Postgres SQL
- Runtime code: `function main() { ... }` → `jwc run`
- Web: `route get "/" { ... }` (and controllers) → `jwc serve`

If you want the deeper design notes, see `jwcv1.md`.

## Install (Windows)

Requirements:

- Rust stable: https://rustup.rs/
- If you see `link.exe not found`, install Visual Studio Build Tools (C++ workload).

Two easy ways to run:

1) Repo launcher (no manual rebuilds):

```powershell
.\jwc.cmd --help
.\jwc.cmd run .\examples\hello.jwc
```

2) Install to PATH:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& .\install.ps1
jwc --help
```

If you **don’t want Rust installed**, download/build a `jwc.exe` once and then install it to PATH:

```powershell
# install.ps1 can copy an existing binary (no cargo needed)
& .\install.ps1 -ExePath "C:\path\to\jwc.exe"
```

## Quick start

Run a program:

```powershell
jwc run .\examples\hello.jwc
```

Most commands accept either a single `.jwc` file, or a **project folder** that contains `jwcproj.json` (manifest) or `main.jwc` (single-file project).

You can also use a csproj-like project manifest: `jwcproj.json` (recommended).

Start the server:

```powershell
cd .\examples\mini_v1
jwc serve --port 3000
```

Generate schema SQL:

```powershell
jwc gen-sql .\examples\sample.jwc
```

## Database migrations (Postgres)

EF-style workflow:

```powershell
# 1) preview SQL (current schema vs local snapshot)
jwc migrate-plan .\path\to\app.jwc

# 2) add migration (writes migrations\*.sql and updates migrations\schema.snapshot.json)
jwc migrate-add .\path\to\app.jwc --name "init"

# 3) apply pending local migrations to DB
jwc migrate-apply .\path\to\app.jwc --db-url "postgres://..."

# 4) see DB history
jwc migrate-status --db-url "postgres://..."
```

### Project manifest (csproj-like)

If you pass a `jwcproj.json`, JWC will load the listed files/dirs and also pick up config/migrations settings from the manifest.

Example: `examples/featured_v1/jwcproj.json`

```json
{
	"files": ["DbContext.jwc", "App.jwc"],
	"dirs": ["features"],
	"config": "config.json",
	"migrationsDir": "migrations"
}
```

DB URL config (highest priority first):

1) `--db-url "postgres://..."`
2) `JWC_DATABASE_URL` environment variable
3) A `context`/`dbcontext` with a url string in the `.jwc` file

Example:

```jwc
context AppDb : Postgres "postgres://postgres:postgres@localhost:5432/postgres";
```

If multiple contexts include URLs, use `--dbcontext <Name>`.

Optional config file: `config.json` (preferred) or `jwc.toml`

`config.json` example:

```json
{
	"ConnectionStrings": {
		"DefaultConnection": "postgres://..."
	},
	"DbContext": "AppDb",
	"MigrationsDir": "migrations"
}
```

```toml
database_url = "postgres://..."
dbcontext = "AppDb"
migrations_dir = "migrations"
```

## Web server (minimal)

Define routes:

```jwc
route get "/" { return "hi"; }
route post "/todos" { return [201, "ok"]; }
```

Compiled DB query keywords (runtime compiles to SQL):

```jwc
let all = select Todo;
let one = select Todo where id = id;
let created = insert Todo set title = body, done = false;
let updated = update Todo set title = body where id = id;
let deleted = delete Todo where id = id;
```

IActionResult-style helpers:

- `Ok([body])`
- `Json(body)`
- `Content(body[, contentType])`
- `Created(body)` / `Created(location, body)`
- `NotFound([body])`
- `BadRequest([body])`
- `NoContent()`
- `StatusCode(code[, body[, contentType]])`

JSON helpers:

- `ToJson(value)` (alias: `JsonSerialize(value)`)
- `Json(...)` auto-serializes values and preserves already-valid JSON strings

Return values:

- `"text"` → HTTP 200
- `[status, body]` → custom status
- `[status, body, content_type]` → custom status + custom content type

## Commands

Tip: `jwc --help` shows the full list and flags.

- `check <file.jwc>`
- `new <dir> [--name <project-name>] [--template minimal|api]`
- `run <file.jwc>`
- `serve [project-dir|project.jwcproj.json] --port <n>`
- `gen-sql <file.jwc>`
- `gen-query-sql <file.jwc>`
- `diff-sql <old.jwc> <new.jwc>`
- `migrate-plan <file.jwc> [--config <path>]`
- `migrate-add <file.jwc> --name <migration-name> [--config <path>]`
- `migrate-apply <file.jwc> [--config <path>] [--db-url <url>] [--dbcontext <name>]`
- `migrate-status [file.jwc] [--config <path>] [--db-url <url>] [--dbcontext <name>] [--limit <n>]`

## Examples

- `examples/hello.jwc`
- `examples/todo_api.jwc`
- `examples/sample.jwc`
- `examples/migrate_old.jwc`, `examples/migrate_new.jwc`

## Dev

```powershell
cargo test
```

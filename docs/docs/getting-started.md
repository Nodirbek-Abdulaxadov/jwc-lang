---
sidebar_position: 2
title: Getting started
---

# Getting started

## Install

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
```

### Windows (PowerShell)

```powershell
iwr -useb https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.ps1 | iex
```

Both scripts pull the most recent tagged release from GitHub Releases
and place `jwc` + `jwc-lsp` into your user-local install directory
(`~/.jwc/bin` on Linux/macOS, `%LOCALAPPDATA%\jwc\bin` on Windows),
plus add it to your `PATH`.

Confirm:

```bash
jwc --help
```

### VS Code extension

Syntax highlighting, snippets, and LSP-powered diagnostics. Install
from the Marketplace:

```bash
code --install-extension jwc-extension.jwc-lang
```

Or grab the latest `.vsix` from the
[GitHub Releases](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/releases)
page and install via *Extensions → ⋯ → Install from VSIX...*.

The extension auto-discovers `jwc-lsp` on `PATH` or under `~/.jwc/bin`.
Override with the `jwc.lspPath` setting if you've installed it
elsewhere.

### Pin a version or mirror

| Env var | What it does |
|---|---|
| `JWC_VERSION=v0.2.0` | install a specific release tag instead of latest |
| `JWC_INSTALL_DIR=/opt/jwc/bin` | install to a custom directory |
| `JWC_DOWNLOAD_BASE=https://mirror/...` | pull artifacts from a self-hosted mirror (e.g. MinIO) instead of GitHub Releases |

### Build from source

When you want bleeding-edge `main`, clone and run the source installer:

```bash
git clone https://github.com/Nodirbek-Abdulaxadov/jwc-lang
cd jwc-lang
./install-from-source.sh         # Linux / macOS
./install-from-source.ps1        # Windows PowerShell
```

## Create a project

```bash
jwc new myapp
cd myapp
```

Result:

```
myapp/
├── myapp.jwcproj
└── main.jwc
```

`main.jwc` already has:

```jwc
function main() {
    print("Hello from JWC");
}
```

Run it:

```bash
jwc run
```

## Bring up a database

JWC currently targets **PostgreSQL**. Create a `.env` in the project root:

```env
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=myapp
```

The CLI loads `.env` automatically and assembles `DATABASE_URL` from the
`PG_*` vars if not already set.

> The database itself is created on demand. The first time you run
> `jwc migrate up`, JWC connects to the admin database (usually
> `postgres`) and issues `CREATE DATABASE` if `PG_DATABASE` doesn't
> exist yet — no `createdb` needed. Set `JWC_ADMIN_DB` if your admin
> role lives somewhere other than `postgres`.

### `setConnectionString(...)` forms

`setConnectionString` is the language-level "wire me to the database"
call. Three legal shapes, pick whichever reads best:

```jwc
// 1. No args — pull from env. `.env` is auto-loaded; reads
//    DATABASE_URL if present, otherwise assembles one from
//    PG_HOST / PG_PORT / PG_USER / PG_PASSWORD / PG_DATABASE.
setConnectionString();

// 2. Structured — explicit, source-readable. Port defaults to 5432.
setConnectionString({
    host:     "localhost",
    port:     5432,
    user:     "postgres",
    password: env("PG_PASSWORD"),
    database: "myapp"
});

// 3. Raw URL — drop-in for an existing connection string.
setConnectionString("postgresql://postgres:secret@localhost:5432/myapp");
```

## Add an entity and a route

Edit `main.jwc`:

```jwc
dbcontext AppDb : Postgres;

entity Note of AppDb {
    id uuid pk;
    title varchar(120);
}

route POST "notes" {
    let req = body();
    let n = new Note();
    n.id = uuid();
    n.title = req.title;
    insert n into AppDb.Note;
    return created(n);
}

route GET "notes" {
    return json(select Note from AppDb.Note);
}

function main() {
    setConnectionString();   // reads DATABASE_URL or PG_* from the loaded .env
    serve(8080);
}
```

Generate the first migration and apply it:

```bash
jwc migrate add init
jwc migrate up
```

Boot the server:

```bash
jwc run
```

Test:

```bash
curl -X POST http://localhost:8080/notes \
  -H 'content-type: application/json' \
  -d '{"title": "first"}'
curl http://localhost:8080/notes
```

## Useful CLI commands

| Command | What it does |
|---|---|
| `jwc new <name>` | Create a new project |
| `jwc run [path]` | Run `main()` from a project / file |
| `jwc run [path] --request-logging` | Run with per-request console logs |
| `jwc serve [--watch]` | Start the HTTP server; `--watch` restarts on `.jwc` change |
| `jwc serve [path] --request-logging` | Serve with per-request console logs |
| `jwc check <file>` | Parse + validate a single file |
| `jwc test` | Validate the whole project |
| `jwc lint` | Validate + emit warnings (unused fn, unused middleware) |
| `jwc gen-sql <file>` | Emit Postgres DDL from entities |
| `jwc migrate new <name>` (alias: `add`) | Create a new migration |
| `jwc migrate up` | Apply pending migrations |
| `jwc migrate down --steps N` | Roll back the most recent N migrations |
| `jwc build [--release]` | Bundle runtime + launcher into `bin/{debug,release}` |

Request logging is off by default.

## Build & bundle

`jwc build` (alias: `jwc bundle`) packages your project together with the JWC
runtime into `bin/{debug,release}`. This is **not** native AOT compilation
yet — the launcher invokes the embedded runtime to execute your `.jwc`
sources. A real native compiler is on Phase 4 of `ROADMAP.md`.

The build uses your current machine OS / architecture automatically.

```bash
# Linux / macOS
./build.sh --debug
./build.sh --release
```

```powershell
# Windows (PowerShell)
./build.ps1 -Debug
./build.ps1 -Release
```

```bat
:: Windows (cmd)
build.cmd --debug
build.cmd --release
```

Output binaries:

| Platform | Debug | Release |
|---|---|---|
| Windows | `target/debug/jwc.exe` | `target/release/jwc.exe` |
| Linux / macOS | `target/debug/jwc` | `target/release/jwc` |

Project-level native artifacts:

- `jwc build` generates a native project launcher inside `bin/debug`.
- `jwc build --release` generates it inside `bin/release`.
- On Windows the launcher is a real `.exe` (e.g. `bin/debug/myapp.exe`).
- `jwc run` on a project also refreshes the debug launcher automatically.

This keeps the interpreter-style dev flow (`jwc run`) and still gives
compiled native artifacts for distribution.

## Server tuning

The HTTP server is built on axum + tokio. Tune the workload via env vars:

| Env var | Default | Purpose |
|---|---|---|
| `JWC_SERVER_WORKERS` | CPU parallelism, min 2 | tokio worker threads |
| `JWC_SERVER_QUEUE_CAPACITY` | `workers × 64`, min 64 | inbound request queue cap |
| `JWC_SERVER_METRICS` | `false` | enable lightweight metrics dump |
| `JWC_SERVER_METRICS_INTERVAL_SECS` | `10` | metrics emission interval |

## Notes

- `.env` is loaded automatically from the project root.
- `jwc run -- test` is not the same as `jwc test`; use `jwc test` for
  project validation.
- If `jwc run` fails with `os error 10048`, port `8080` is already in use.
  Stop the offending process or pick another port:
  `jwc serve --port 8081`.

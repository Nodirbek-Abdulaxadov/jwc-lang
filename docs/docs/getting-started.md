---
sidebar_position: 2
title: Getting started
---

# Getting started

## Install

Clone the language repo and build the CLI:

```bash
git clone https://github.com/Nodirbek-Abdulaxadov/jwc-lang
cd jwc-lang
./install.sh   # Linux / macOS
./install.ps1  # Windows PowerShell
```

This places a `jwc` binary on your `PATH` (release profile). Confirm:

```bash
jwc --help
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
    setConnectionString(`postgresql://${env("PG_USER")}:${env("PG_PASSWORD")}@${env("PG_HOST")}:${env("PG_PORT")}/${env("PG_DATABASE")}`);
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
| `jwc serve [--watch]` | Start the HTTP server; `--watch` restarts on `.jwc` change |
| `jwc check <file>` | Parse + validate a single file |
| `jwc test` | Validate the whole project |
| `jwc lint` | Validate + emit warnings (unused fn, unused middleware) |
| `jwc gen-sql <file>` | Emit Postgres DDL from entities |
| `jwc migrate new <name>` | Create a new migration |
| `jwc migrate up` | Apply pending migrations |
| `jwc migrate down --steps N` | Roll back the most recent N migrations |
| `jwc build [--release]` | Bundle runtime + launcher into `bin/{debug,release}` |

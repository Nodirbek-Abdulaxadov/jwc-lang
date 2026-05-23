---
sidebar_position: 1
---

# Overview

Every subcommand in one place. `jwc --help` and `jwc <sub> --help` print the up-to-date version of this.

## Project lifecycle

| Command | Effect |
|---|---|
| `jwc new <name>` | Scaffold `name/name.jwcproj` + `name/main.jwc`. |
| `jwc check <file.jwc>` | Parse + validate a single file. Prints `OK`. |
| `jwc gen-sql <file.jwc>` | Emit Postgres DDL for entities in `<file.jwc>`. |
| `jwc test` | Load + validate the whole project. |
| `jwc lint` | Run lint warnings (see [Lint codes](./lint)). |
| `jwc fmt [path]` | Format `.jwc` files; `--check` for CI (non-zero on diff). |

## Run

| Command | Effect |
|---|---|
| `jwc run [path]` | Run interpreter against project / file. |
| `jwc serve [path]` | Same, but always starts the HTTP server. `--port`, `--request-logging`, `--watch`. |

## Build

| Command | Effect |
|---|---|
| `jwc build` | Bundle interpreter + launcher into `bin/{debug,release}/`. |
| `jwc build --native` | Real AOT — generates Rust source, runs cargo. |
| `jwc build --native --emit-rust-source` | Dump the generated Rust to `bin/<profile>/<app>.generated.rs` and skip cargo. |
| `jwc build --native --target <triple>` | Cross-compile. Triple must be in the supported list (`linux-gnu/musl`, `darwin`, `windows-msvc`, …). |

## Database

| Command | Effect |
|---|---|
| `jwc migrate new <name>` | Scaffold `migrations/<ts>_<name>.{up,down}.sql` using the entity diff. |
| `jwc migrate up` | Apply pending. `--database-url <url>` to override env. |
| `jwc migrate down --steps N` | Roll back the most recent N. |
| `jwc migrate list` | Offline — print every migration file in chronological order. |

## Packages

| Command | Effect |
|---|---|
| `jwc add <name>` | Add a dep. `--version <semver>`, `--path <dir>`, `--git <url> --rev <rev>`. |
| `jwc install` | Fetch every dep from the lockfile. |
| `jwc update [<name>]` | Re-resolve within current ranges. |
| `jwc remove <name>` | Drop from manifest + lockfile. |
| `jwc tree` | Print the resolved dep tree. |
| `jwc login --token jwc_...` | Store the registry token in `~/.jwc/credentials.json`. `--registry <url>` overrides. |
| `jwc publish` | Pack + upload the current project (`type=pkg` only). |

## Diagnostic helpers

```bash
jwc lint --json              # editor / CI integration: one JSON array per warning
jwc lint --explain W004      # print the catalog description for a code
jwc lint --list-codes        # full W + E catalog as JSON, no project load
```

## Env that every command honours

| Env | Effect |
|---|---|
| `JWC_DATABASE_URL` / `DATABASE_URL` | Postgres URL for `migrate` and runtime |
| `JWC_DB_TLS=1` | Connect over TLS |
| `JWC_DB_POOL_SIZE`, `JWC_DB_MIN_IDLE`, … | Pool tuning ([Dbcontext](../data/dbcontext)) |
| `JWC_QUERY_CACHE_TTL_SECS` | Enable in-memory SELECT cache |
| `JWC_QUEUE_WORKERS`, `JWC_QUEUE_MAX_ATTEMPTS`, … | Queue tuning ([Queue](../backend/queue)) |
| `JWC_SERVER_WORKERS` | Legacy — ignored on the async stack |
| `JWC_REGISTRY_URL` | Override registry for `add` / `install` / `publish` |
| `JWC_REGISTRY_TOKEN` | Override credential file for `publish` (CI use) |
| `RUST_LOG` | Tracing filter (e.g. `info,jwc=debug,tower_http=info`) |

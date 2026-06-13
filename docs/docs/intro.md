---
slug: /
sidebar_position: 1
---

# JWC

**Backend-first programming language with HTTP routes, entities, and SQL as first-class language constructs.**

```jwc
dbcontext AppDb { driver = "postgres"; }

entity User {
    id: int pk;
    name: string;
    email: string;
}

route GET "/users" {
    let users = select User from AppDb.User orderby User.id;
    return json(users);
}

function main() { serve(8080); }
```

`jwc run` → server up on `:8080`. That's the whole app.

## What's here

| Section | What it covers |
|---|---|
| [Getting started](./getting-started/install) | Install, hello world, project layout |
| [Language](./language/syntax) | Types, variables, functions, control flow, async |
| [Data](./data/dbcontext) | Entities, select / insert / update / delete, migrations, transactions |
| [Backend](./backend/routes) | Routes, middleware, validation, error handler, websockets, queue |
| [Standard library](./stdlib/strings) | String, array, JSON, HTTP, JWT, hashing, email, cache |
| [Packages](./packages/manifest) | Manifest, dependencies, registry, `jwc publish` |
| [CLI](./cli/overview) | Every `jwc` subcommand + flag |
| [Deployment](./deployment/native-build) | Bundled launcher, native AOT, [Docker](./deployment/docker) (official `ghcr.io/.../jwc` image), k8s migrate init-container, observability, OTLP |
| [Reference](./reference/builtins) | Built-ins reference + numbered diagnostic codes (`Wxxx` / `Exxx`) |
| [Security](./security/) | SSRF allowlist, JWT validation, secrets redaction, trusted-proxy chain |

## Status

- **Interpreter** — production-ready (`jwc run`, `jwc serve`).
- **Native AOT** — partial. Most programs compile via `jwc build --native` (Rust-source path).
- **LLVM IR backend** — skeleton only ([Sprint 13](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/ROADMAP.md)).
- **Registry** — live at [`registry-jwc.1kb.uz`](https://registry-jwc.1kb.uz).

See [`ROADMAP.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/ROADMAP.md) for the full per-feature status.

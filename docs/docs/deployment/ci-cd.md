---
sidebar_position: 3
title: "CI and CD"
description: "The checks that need no database, the ones that do, and a rollout that runs migrations before the new code."
---

# CI and CD

## What needs a database, and what does not

Most of it does not, and that is the useful fact:

| Command | Database | What it catches |
|---|---|---|
| `jwc check --deny-warnings` | no | types, names, schema, routes |
| `jwc fmt --check` | no | formatting drift |
| `jwc lint --deny-warnings` | no | the advisory whole-program lints |
| `jwc openapi --compact` | no | that the route table still renders |
| `jwc build --release` | no | that the AOT backend can lower every construct |
| `jwc test` | **yes** | `test` blocks, each in a rolled-back transaction |
| `jwc migrate up` | **yes** | that the migrations apply |
| `jwc migrate verify` | **yes** | that the constraint and index names match |

The schema is in the source, so the queries are checked against it without
connecting to anything. That is what puts the first five in a pre-commit
hook and keeps the pipeline's fast stage genuinely fast.

## A GitHub Actions workflow

```yaml
name: ci
on: [push, pull_request]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: |
          curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | bash
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - run: jwc check --deny-warnings
      - run: jwc fmt --check
      - run: jwc lint --deny-warnings

  test:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_PASSWORD: postgres
        options: >-
          --health-cmd pg_isready --health-interval 5s
          --health-timeout 5s --health-retries 10
        ports: ["5432:5432"]
    env:
      DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
      CURSOR_SECRET: ci-only-not-a-secret
    steps:
      - uses: actions/checkout@v4
      - run: |
          curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | bash
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - run: jwc migrate up
      - run: jwc migrate verify
      - run: jwc test
```

`jwc test` runs each `test` block in its own transaction and rolls it
back, so the order does not matter and the service container does not need
resetting between them.

### Annotating a pull request

`jwc lint --json` prints every diagnostic — warnings included — as one
array on stdout, with the verdict in the exit code:

```json
[{"file":"src/app.jwc","line":12,"column":9,"end_line":12,"end_column":17,
  "severity":"warning","code":"W0104","message":"…","note":null,
  "spec":"names.md §3.3"}]
```

`file`, `line` and `column` are what an annotation action wants. Pipe it
through `jq` into whatever your platform's annotation format is.

## Rollout

The order is the whole content of a JWC deploy:

1. `jwc migrate up` — before the new code takes traffic. It holds a
   Postgres advisory lock, so concurrent starts do not double-apply.
2. Start the new version.
3. `jwc migrate verify` in the readiness gate — it names any constraint or
   index the binary expects and the database does not have.

Migrations are **forward-only in practice**: `jwc migrate down` exists and
refuses anything whose `down` carries an `-- irreversible:` marker, which
is most schema changes that drop something. Plan a rollout so the previous
version can run against the new schema, and you never need it.

### Which image

There is one. A JWC binary is the compiler, the migrator and the server —
`jwc migrate up` and `jwc serve` are the same executable — so the init
container and the pod run the same image and there is no second artefact
to keep in step.

If you deploy a `jwc build` binary instead, that binary serves but does
not migrate. Keep a `jwc` in the migration step, pinned to the version the
binary was built with.

## Pinning the toolchain

The install script takes a version:

```bash
curl -fsSL …/install.sh | JWC_VERSION=v0.9.936 bash
```

Pin it in CI. A pipeline that silently follows the latest release will one
day fail on a diagnostic that is new and correct, in a pull request that
did not touch the code it is about.

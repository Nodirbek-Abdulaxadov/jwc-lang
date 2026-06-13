---
sidebar_position: 1
---

# Zero to deployed CRUD in 15 minutes

A walkthrough that takes you from `jwc --version` to a running JWC service
backed by Postgres, deployed inside Docker, with `migrate up` already
applied and CRUD endpoints answering bombardier traffic. The whole thing
clocks in at ~15 minutes if Postgres is already running locally.

By the end you will have:

- A JWC project scaffolded from the `api` template.
- An `Item` entity with auto-applied schema migration.
- Five REST endpoints (list / get / create / update / delete) answering
  JSON.
- A Docker image built from the official `ghcr.io/.../jwc:` base.
- Notes on next steps — auth template, jobs template, observability,
  package registry.

---

## 0. Prerequisites

| You need | Why |
|---|---|
| `jwc` v0.4.7+ | The CLI itself — install via the one-liner in [Install](../getting-started/install.md). |
| Postgres 13+ | The entity layer SQL-generates against it. Anything `psql`-reachable works. |
| Docker | For the deploy step. Skip if you only want `jwc run`. |

Verify:

```bash
jwc --version
psql -V
docker --version
```

---

## 1. Scaffold the project (1 minute)

```bash
jwc new myapi --template api
cd myapi
```

You get:

```
myapi/
├── myapi.jwcproj             # manifest (name, type=app, version)
├── main.jwc                  # 5 routes + serve(8080) in main()
├── src/
│   ├── AppDbContext.jwc      # dbcontext AppDb : Postgres;
│   └── Item.jwc              # entity Item of AppDb { id, name, createdAt }
├── migrations/
│   ├── <ts>_init.up.sql      # CREATE TABLE item (...)
│   └── <ts>_init.down.sql    # DROP TABLE item
├── .env.example              # DATABASE_URL placeholder
├── .gitignore
└── README.md
```

Open `main.jwc` and skim — every route is a thin wrapper over a
`select` / `insert` / `update` / `delete` against `AppDb.Item`. No
framework boilerplate; the route IS the handler.

---

## 2. Wire up Postgres (2 minutes)

```bash
cp .env.example .env
# Edit .env so DATABASE_URL points at your local Postgres:
# DATABASE_URL=postgres://myapi:myapi@localhost:5432/myapi
```

Create the database (any reachable Postgres works — Docker, Homebrew,
managed, doesn't matter):

```bash
createdb myapi
# or via psql -c "CREATE DATABASE myapi;"
```

Apply migrations:

```bash
jwc migrate up
# Migrations applied: 1
# Already applied: 0
# Total found: 1
```

Sanity-check the schema landed:

```bash
psql myapi -c "\d item"
# Table "public.item"
# ... id (bigint) | name (varchar) | createdAt (timestamptz) ...
```

---

## 3. Run it (1 minute)

```bash
jwc run
# JWC config:
#   DATABASE_URL          env   postgres://... (redacted)
#   JWC_PORT              default  8080
#   ...
# [JWC] listening on http://0.0.0.0:8080
```

In another terminal:

```bash
# Empty list
curl http://localhost:8080/items
# []

# Create
curl -X POST http://localhost:8080/items \
     -H 'content-type: application/json' \
     -d '{"name":"hello"}'
# {"id":1,"name":"hello","createdAt":"2026-06-13T18:42:01Z"}

# Get
curl http://localhost:8080/items/1
# {"id":1,"name":"hello","createdAt":"2026-06-13T18:42:01Z"}

# Update
curl -X PUT http://localhost:8080/items/1 \
     -H 'content-type: application/json' \
     -d '{"name":"hello-edited"}'
# {"id":1,"name":"hello-edited", ...}

# Delete
curl -X DELETE http://localhost:8080/items/1
# 204 (No Content)
```

---

## 4. Build the native binary (1 minute)

The interpreter (`jwc run`) is great for iteration. For production you
want the AOT-compiled binary — single file, no JWC runtime needed:

```bash
jwc build --native --release
# Compiling axum ...
# Native build complete (release)
# Binary:  bin/release/myapi
```

It's a regular ELF / Mach-O / PE binary. Run it directly:

```bash
./bin/release/myapi
# [JWC] listening on http://0.0.0.0:8080
```

---

## 5. Containerize (3 minutes)

JWC ships an official Docker base image:

```bash
# Dockerfile
cat > Dockerfile <<'EOF'
# Stage 1: build the native binary inside the JWC base.
FROM ghcr.io/nodirbek-abdulaxadov/jwc:0.4.7 AS build
WORKDIR /src
COPY . .
RUN jwc build --native --release

# Stage 2: minimal runtime image carrying ONLY the compiled binary.
FROM debian:bookworm-slim
RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /src/bin/release/myapi /usr/local/bin/myapi
ENTRYPOINT ["/usr/local/bin/myapi"]
EXPOSE 8080
EOF

docker build -t myapi:0.1 .
```

Run it:

```bash
docker run --rm -p 8080:8080 \
    -e DATABASE_URL='postgres://...@host.docker.internal:5432/myapi' \
    myapi:0.1
```

Hit it from a third terminal:

```bash
curl http://localhost:8080/items
```

---

## 6. Deploy to k8s — migrate as init-container (3 minutes)

The cleanest production pattern is to run `jwc migrate up` as an
init-container before the app container starts. The base JWC image
includes the `migrate` subcommand:

```yaml
# myapi.yaml
apiVersion: apps/v1
kind: Deployment
metadata: { name: myapi }
spec:
  replicas: 3
  selector: { matchLabels: { app: myapi } }
  template:
    metadata: { labels: { app: myapi } }
    spec:
      initContainers:
        - name: migrate
          # The official JWC image. Carries `jwc migrate`.
          image: ghcr.io/nodirbek-abdulaxadov/jwc:0.4.7
          args: ["migrate", "up"]
          env:
            - name: DATABASE_URL
              valueFrom: { secretKeyRef: { name: myapi-db, key: url } }
          # Mount the migrations/ dir so init container sees the SQL files.
          # In real life ship them inside your own app image — see Note below.
      containers:
        - name: app
          image: registry.example.com/myapi:0.1
          ports: [ { containerPort: 8080 } ]
          env:
            - name: DATABASE_URL
              valueFrom: { secretKeyRef: { name: myapi-db, key: url } }
```

> **Why init-container?** Multiple replicas need their migrations applied
> exactly once at rollout. JWC's `migrate up` takes a Postgres advisory
> lock (`pg_advisory_lock("jwc-mig")`), so even if k8s schedules every
> replica's init-container in parallel, only one acquires the lock and
> the others wait. Documented in
> [Migrations](../deployment/migrations.md) and verified for
> testcontainer rollouts.

For the production wiring (Recipe 3 + 4 with the migrations baked into
the app image), see [Docker deployment](../deployment/docker.md).

---

## 7. Verify with bombardier (1 minute)

```bash
go install github.com/codesenberg/bombardier@latest
bombardier -c 200 -d 10s http://localhost:8080/items
# Statistics        Avg      Stdev        Max
#   Reqs/sec     14523.18    1287.45   18420.83
#   Latency       13.76ms     8.40ms   102.40ms
```

That's the `/json-large`-shape — list serialise into JSON. Reasonable
for a multi-row CRUD path on a 3-field entity.

---

## 8. Where to go next

| Want | Read |
|---|---|
| JWT login + middleware-guarded routes | [`jwc new myapp --template auth`](../getting-started/templates.md) |
| Background email / image / webhook handlers | [`jwc new myapp --template jobs`](../getting-started/templates.md) |
| OTLP traces to Jaeger / Tempo | [Observability + OTLP](../deployment/otlp.md) |
| Pull packages from the registry | [`jwc add jwc-...`](../getting-started/install.md), full ecosystem map in [`docs/spec/ecosystem.md`](../../spec/ecosystem.md) |
| Editor support (LSP) | [Editor setup](../getting-started/editor-setup.md) |
| Static binary for Alpine / distroless | [musl static](../deployment/musl-static.md) |

---

## Troubleshooting

**`DATABASE_URL is required for db access`**
The `jwc run` boot reads `DATABASE_URL` (or `JWC_DATABASE_URL`).
Put it in `.env` or `export` it before launching.

**`migration X was modified after it was applied`**
The SHA-256 checksum recorded at `migrate up` time doesn't match the
on-disk file. Either restore the original or roll forward — see
[Migrations](../deployment/migrations.md).

**`json(): argument is not valid JSON`**
You're handing `json()` a `Value::Str` that doesn't parse. Either fix
the payload, switch to `text()` for plain text, or use `json_unchecked()`
if you've already validated the string yourself.

**`error[E021]: cannot reference private function 'foo' across namespaces`**
Mark the function `public` in its declaration. The visibility rule is
documented in [`docs/spec/visibility.md`](../../spec/visibility.md).

---

**Total wall-clock**: ~15 minutes start to finish on a machine that has
Docker + Postgres warmed up. The slowest single step is `jwc build
--native --release` — first run pulls + compiles axum/tokio + the rest
of the runtime crate set. Subsequent builds incremental-compile and
finish in 2-3 seconds.

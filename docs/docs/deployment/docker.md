---
sidebar_position: 2
---

# Docker

JWC ships two official multi-arch (`linux/amd64`, `linux/arm64`) images on GHCR, rebuilt and re-tagged on every `vX.Y.Z` release:

| Image | Base | Contents | Use it for |
|---|---|---|---|
| `ghcr.io/nodirbek-abdulaxadov/jwc:<version>` | `debian:bookworm-slim` | `jwc` CLI | Build stages, `jwc migrate up` init-containers, CI |
| `ghcr.io/nodirbek-abdulaxadov/jwc-runtime:<version>` | `gcr.io/distroless/cc-debian12:nonroot` | libc + ca-certs only | Final stage for a compiled `jwc-app` native binary |

Both publish SBOM + provenance attestations.

`:latest` exists too, but **pin to an exact `:0.4.8`-style tag in production** — these images are part of your supply chain.

---

## Recipe 1 — Verify the install

```bash
docker pull ghcr.io/nodirbek-abdulaxadov/jwc:0.4.8
docker run --rm ghcr.io/nodirbek-abdulaxadov/jwc:0.4.8 --version
# jwc 0.4.8
```

The default `ENTRYPOINT` is `/usr/local/bin/jwc`, so anything you pass becomes a subcommand:

```bash
docker run --rm -v "$PWD:/work" ghcr.io/nodirbek-abdulaxadov/jwc:0.4.8 check examples/testapp/main.jwc
docker run --rm -v "$PWD:/work" ghcr.io/nodirbek-abdulaxadov/jwc:0.4.8 lint
```

---

## Recipe 2 — App image (multi-stage, native AOT)

Compile your app to a static-ish native binary, then ship it inside the minimal `jwc-runtime` image. This is the recommended production layout.

```dockerfile
# syntax=docker/dockerfile:1.7

# Stage 1: pull a known-good jwc CLI.
FROM ghcr.io/nodirbek-abdulaxadov/jwc:0.4.8 AS jwc-bin

# Stage 2: build the native app binary.
FROM debian:bookworm-slim AS app-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates gcc libc6-dev pkg-config libssl-dev curl \
    && rm -rf /var/lib/apt/lists/*

# Rust toolchain — required by `jwc build --native`, which shells out to cargo.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH=/root/.cargo/bin:$PATH

COPY --from=jwc-bin /usr/local/bin/jwc /usr/local/bin/jwc

WORKDIR /app
COPY . .
RUN jwc build --native --release

# Stage 3: minimal runtime — just the binary on distroless.
FROM ghcr.io/nodirbek-abdulaxadov/jwc-runtime:0.4.8
COPY --from=app-builder /app/bin/release/my-api /app/my-api
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["/app/my-api"]
```

Resulting image: ~30 MB (distroless base + the 2-5 MB native binary).

---

## Recipe 3 — Kubernetes: migrate-as-init-container

This is the **production deploy pattern** the readiness plan calls out. The app pod's native binary never imports the migration code path; instead, an init-container runs `jwc migrate up` against the same `DATABASE_URL`, exits 0, and only *then* the app container starts.

`jwc migrate up` takes a Postgres advisory lock (`MIGRATION_LOCK_KEY = "jwc-mig"`), so multiple replicas rolling out simultaneously serialise safely without deadlocking — see [`migrations.md`](./migrations).

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-jwc-app
spec:
  replicas: 3
  selector:
    matchLabels: { app: my-jwc-app }
  template:
    metadata:
      labels: { app: my-jwc-app }
    spec:
      initContainers:
        - name: migrate
          image: ghcr.io/nodirbek-abdulaxadov/jwc:0.4.8
          args: ["migrate", "up"]
          workingDir: /work
          env:
            - name: DATABASE_URL
              valueFrom:
                secretKeyRef: { name: app-secrets, key: database-url }
          volumeMounts:
            - name: app-src
              mountPath: /work
              readOnly: true
      containers:
        - name: app
          image: registry.example.com/my-jwc-app:0.4.8   # your Recipe-2 image
          ports:
            - containerPort: 8080
          env:
            - name: DATABASE_URL
              valueFrom:
                secretKeyRef: { name: app-secrets, key: database-url }
            - name: JWT_SECRET
              valueFrom:
                secretKeyRef: { name: app-secrets, key: jwt-secret }
            - name: RUST_LOG
              value: "info,jwc=info,tower_http=info"
          readinessProbe:
            httpGet: { path: /healthz, port: 8080 }
            initialDelaySeconds: 2
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
            initialDelaySeconds: 10
      volumes:
        - name: app-src
          configMap:
            name: my-jwc-app-src   # mounts the .jwc project (or use a PVC / git-sync sidecar)
```

Notes:

- The init-container needs the `.jwc` project tree visible — mount it from a `ConfigMap` (small projects), `PersistentVolumeClaim`, or a `git-sync` sidecar (preferred for >1 file).
- If your migrations are pre-generated SQL files (`migrations/*.up.sql`), only that directory needs to be mounted.
- Set the same `JWC_DB_TLS*` env vars on the init-container as the app — the migrate command uses the same pool layer.

---

## Recipe 4 — Build a local image

For development you can build either Dockerfile from the repo root:

```bash
# Full CLI image
docker build -t my-jwc:dev -f Dockerfile .

# Minimal runtime base
docker build -t my-jwc-runtime:dev -f Dockerfile.slim .

# Multi-arch local build (requires buildx)
docker buildx build --platform linux/amd64,linux/arm64 -t my-jwc:dev -f Dockerfile .
```

The `Dockerfile` uses BuildKit cache mounts for `~/.cargo/{git,registry}` and `target/`, so a second incremental build of the same source takes seconds.

---

## Environment variables

The runtime reads the same env vars as the bare binary — `DATABASE_URL`, `JWC_DB_TLS*`, `JWT_SECRET`, `RUST_LOG`, etc. The full list is in [`env-vars.md`](./env-vars).

## Image trade-offs at a glance

|  | `jwc` image | `jwc-runtime` image |
|---|---|---|
| Base | `debian:bookworm-slim` | `gcr.io/distroless/cc-debian12:nonroot` |
| Size | ~80 MB | ~25 MB |
| Shell | yes (`/bin/sh`) | no |
| Package manager | yes (`apt`) | no |
| Includes `jwc` CLI | yes | **no** |
| Use case | migrate / build / debug | run a compiled native app |

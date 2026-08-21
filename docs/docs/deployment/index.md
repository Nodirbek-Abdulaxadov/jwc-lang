---
sidebar_position: 1
title: "Deployment"
description: "Docker, the native build, migrations at rollout, and the probes to point a supervisor at."
---

# Deployment

A JWC program needs Postgres and, if it uses `redis.*`, Redis. Nothing
else — no application server, no reverse proxy requirement, no runtime to
install beside it.

## Two shapes

### Ship the compiler and the sources

```dockerfile
FROM debian:trixie-slim AS fetch
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates && rm -rf /var/lib/apt/lists/*
ARG JWC_VERSION=0.9.9
RUN curl -fsSL https://github.com/just-web-code/jwc-lang/releases/download/v${JWC_VERSION}/jwc-v${JWC_VERSION}-x86_64-linux.tar.gz \
      | tar -xz -C /usr/local/bin \
    && chmod +x /usr/local/bin/jwc

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates wget && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=fetch /usr/local/bin/jwc /usr/local/bin/jwc
COPY jwcproj.json /app/jwcproj.json
COPY src /app/src
COPY migrations /app/migrations
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s \
    CMD wget -q -O- http://127.0.0.1:8080/healthz || exit 1
CMD ["jwc", "serve", "/app"]
```

No build stage and no Rust toolchain. The same binary runs the migrations
in an init container and serves in the pod.

### Ship a native binary

```dockerfile
FROM rust:slim AS build
# … fetch jwc, then:
RUN jwc build /src --release

FROM debian:trixie-slim
COPY --from=build /src/bin/release/app /usr/local/bin/app
CMD ["app"]
```

Slower to build, smaller to run, and no compiler in the runtime image.

Which to pick is a build-time-versus-image-size trade, not a
capability one: the two backends answer identically.

## Migrations at rollout

Run `jwc migrate up` **before** the new pods take traffic — an init
container, a pre-deploy job, whatever the platform calls it. It takes an
advisory lock, so several replicas starting at once do not both apply the
same migration.

Then, in the readiness gate:

```bash
jwc migrate verify .
```

which names any constraint or index the binary expects and the database
does not have.

## Probes

| | Point it at |
|---|---|
| liveness | `GET /healthz` |
| readiness | `GET /readyz` |
| scrape | `GET /metrics` |

`/healthz` touches nothing, deliberately: putting a dependency behind a
liveness probe turns a database blip into a restart storm.

`/readyz` round-trips every configured dependency and names the one that
failed. Redis is only checked when `JWC_REDIS_URL` is set.

## Behind a proxy

```jwc no-compile
server {
    trusted_proxies = ["10.0.0.0/8", "172.16.0.0/12"];
}
```

`request.client_ip()` walks `X-Forwarded-For` right to left, peeling off
entries that match a trusted CIDR, and returns the first that does not.
With no trust list the rightmost entry wins — the one the nearest proxy
appended, which is the only one a client cannot forge.

## Shutting down

SIGINT drains: the listener stops accepting, inflight requests finish, and
after `JWC_SHUTDOWN_TIMEOUT` seconds (default 5) the process exits
whatever is left. Set the platform's grace period above it.

## What to set

| | |
|---|---|
| `DATABASE_URL` | required |
| `JWT_SECRET` | if the program signs tokens |
| `CURSOR_SECRET` | if any query pages |
| `JWC_REDIS_URL` | to enable `redis.*` |
| `JWC_DB_POOL_SIZE` | default 64; lower it on memory-constrained nodes |
| `PORT` | only if `main` reads it |

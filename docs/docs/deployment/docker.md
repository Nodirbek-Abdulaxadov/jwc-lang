---
sidebar_position: 2
---

# Docker

Production deploys ship a single AOT binary in a `debian-slim` base. Minimal Dockerfile:

```dockerfile
# --- builder ---
FROM rust:1.83-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Install jwc CLI
RUN curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
ENV PATH=/root/.jwc/bin:$PATH

COPY . .
RUN jwc build --native --release

# --- runtime ---
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/bin/release/my-api /usr/local/bin/my-api
EXPOSE 8080
ENV RUST_LOG=info
CMD ["my-api"]
```

Image size: ~80 MB (debian-slim + 2-3 MB binary + ca-certs).

## docker-compose (full stack)

```yaml
services:
  postgres:
    image: postgres:17-alpine
    environment:
      POSTGRES_USER: jwc
      POSTGRES_PASSWORD: jwc
      POSTGRES_DB: app
    volumes: [ pg:/var/lib/postgresql/data ]

  app:
    build: .
    depends_on: [ postgres ]
    environment:
      JWC_DATABASE_URL: postgres://jwc:jwc@postgres:5432/app
      JWT_SECRET: ${JWT_SECRET:?required}
    ports: [ "8080:8080" ]

volumes: { pg: }
```

## Healthcheck

The interpreter doesn't ship a `/healthz` by default — add one to your app:

```jwc
route GET "/healthz" {
    return json({ status: "ok" });
}
```

Then in the Dockerfile:

```dockerfile
HEALTHCHECK --interval=30s --timeout=3s \
    CMD wget -q -O- http://127.0.0.1:8080/healthz || exit 1
```

## Logs

JWC uses `tracing`. `RUST_LOG=info,jwc=debug,tower_http=info` is a sensible default. Output goes to stdout — docker / k8s / journald all collect it.

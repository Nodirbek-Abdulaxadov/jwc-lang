# syntax=docker/dockerfile:1.7
#
# Official JWC builder + CLI image.
#
# This image contains the `jwc` CLI and is intended for:
#   - App build stages (multi-stage `FROM ghcr.io/.../jwc:<ver> AS jwc-bin`).
#   - k8s migration init-containers (`jwc migrate up`).
#   - Local dev / CI that needs a hermetic `jwc` binary.
#
# For the minimal app-runtime image (no CLI, just a base for your compiled
# `jwc-app` binary), use `Dockerfile.slim` instead.
#
# Rust version: pinned to the latest stable slim image. The host glibc
# (currently 2.40 on bookworm-slim) dictates which `debian:*-slim` runtime
# stages can consume the produced binary. Bumping the builder Rust version
# is fine; bumping the *base* (e.g. trixie) requires re-checking the runtime
# stage matches or downstreams will break on `GLIBC_2.4x not found`.

ARG RUST_VERSION=1.92
ARG DEBIAN_VERSION=bookworm

# ---------- builder ----------
FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS builder

# pkg-config + libssl-dev: needed for any transitive `openssl-sys` crate.
# We pull `tokio-postgres-rustls` so OpenSSL isn't *required* at runtime,
# but the builder still wants the headers if a transitive dep flips on.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Copy the full workspace. Build cache mounts keep cargo registry/git/target
# warm across CI runs.
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin jwc \
    && cp /src/target/release/jwc /usr/local/bin/jwc \
    && strip /usr/local/bin/jwc

# ---------- runtime ----------
FROM debian:${DEBIAN_VERSION}-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 jwc \
    && useradd  --system --uid 1000 --gid jwc --no-create-home jwc

COPY --from=builder /usr/local/bin/jwc /usr/local/bin/jwc

USER jwc:jwc
WORKDIR /work

LABEL org.opencontainers.image.title="jwc" \
      org.opencontainers.image.description="JWC language compiler + runtime CLI" \
      org.opencontainers.image.source="https://github.com/Nodirbek-Abdulaxadov/jwc-lang" \
      org.opencontainers.image.url="https://github.com/Nodirbek-Abdulaxadov/jwc-lang" \
      org.opencontainers.image.documentation="https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/docs/docs/deployment/docker.md" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.vendor="Nodirbek Abdulaxadov"

ENTRYPOINT ["/usr/local/bin/jwc"]
CMD ["--help"]

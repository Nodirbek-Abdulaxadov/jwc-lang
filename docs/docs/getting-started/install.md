---
sidebar_position: 1
title: Install
description: "Install the jwc compiler from a release archive, and check it against a real Postgres."
---

# Install

JWC ships as one binary. It needs a Postgres to talk to; it does not need a
Rust toolchain, a package manager, or a runtime installed alongside it.

## From a release archive

```bash
VERSION=0.9.9
curl -fsSL https://github.com/just-web-code/jwc-lang/releases/download/v${VERSION}/jwc-v${VERSION}-x86_64-linux.tar.gz \
  | sudo tar -xz -C /usr/local/bin
sudo chmod +x /usr/local/bin/jwc
jwc --version
```

Archives are published for `x86_64-linux`, `aarch64-linux`, `x86_64-macos`
and `aarch64-macos`. Each release also publishes a `.sha256` next to the
archive; check it before extracting if you are scripting the install.

The Linux builds are dynamically linked against glibc 2.34 or newer. On an
older distribution, build from source.

## From source

```bash
git clone https://github.com/just-web-code/jwc-lang
cd jwc-lang
cargo build --release --features redis
./target/release/jwc --version
```

`--features redis` is what makes the `redis` package's built-ins real. Without
it `redis.enabled()` answers `false` and every other name in that namespace
raises — see [packages](../packages/).

## What else you need

**Postgres.** Any supported version; the compiler emits standard DDL and
standard SQL. There is no other driver and there is not going to be one.

```bash
docker run -d --name jwc-pg -p 5432:5432 \
  -e POSTGRES_PASSWORD=jwc -e POSTGRES_USER=jwc -e POSTGRES_DB=app \
  postgres:17-alpine
```

**Redis**, only if you use the `redis` package — a rate limiter or a shared
cache. It is optional and the compiler tells you when it is missing rather
than silently degrading.

## Check the install

```bash
jwc --version
jwc check .          # type-check a project without touching the database
```

`jwc check` needs no connection. Everything that talks to Postgres —
`serve`, `migrate`, `test` — reads `DATABASE_URL` (or `JWC_DATABASE_URL`).

## Next

[Hello world](./hello-world) is a running service in about twenty lines.

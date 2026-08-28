---
sidebar_position: 1
title: Install
description: "Install the jwc compiler with the one-line installer on Linux or Windows, and check it against a real Postgres."
---

# Install

JWC ships as one binary. It needs a Postgres to talk to; it does not need a
Rust toolchain, a package manager, or a runtime installed alongside it.

## Linux and macOS

```bash
curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | bash
```

## Windows

```powershell
iwr -useb https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.ps1 | iex
```

Both resolve the latest release themselves, verify the published `.sha256`
against what they downloaded, and refuse to install on a mismatch. The
Windows script installs to `%LOCALAPPDATA%\jwc\bin`.

```bash
jwc --version
```

### Pinning a version

```bash
curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | JWC_VERSION=v0.9.943 bash
```

```powershell
$env:JWC_VERSION = 'v0.9.943'
iwr -useb https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.ps1 | iex
```

The variable goes before `bash`, not before `curl` — `bash` is what reads it.

### Other knobs

| | |
|---|---|
| `JWC_INSTALL_DIR` | where to put the binary |
| `JWC_MUSL=1` | Linux only: fetch the static musl build instead of the glibc one |
| `JWC_DOWNLOAD_BASE` | fetch from a mirror instead of GitHub Releases |

## What is published

| Platform | Archive |
|---|---|
| x86_64 Linux (glibc) | `jwc-vX.Y.Z-x86_64-linux.tar.gz` |
| x86_64 Linux (static) | `jwc-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` |
| aarch64 Linux (glibc) | `jwc-vX.Y.Z-aarch64-linux.tar.gz` |
| aarch64 Linux (static) | `jwc-vX.Y.Z-aarch64-unknown-linux-musl.tar.gz` |
| x86_64 macOS | `jwc-vX.Y.Z-x86_64-macos.tar.gz` |
| aarch64 macOS | `jwc-vX.Y.Z-aarch64-macos.tar.gz` |
| x86_64 Windows | `jwc-vX.Y.Z-x86_64-windows.zip` |

Each has a `.sha256` beside it.

macOS ships as of **v0.9.923**. Before that there was no darwin build at
all — not removed at any point, simply never in the release matrix, while
this page claimed archives for it. On an older release, build from source.

The glibc Linux builds are linked against glibc 2.35, which covers Ubuntu
22.04, Debian 12, RHEL 9 and Amazon Linux 2023. On anything older, or on
Alpine and distroless images, use the musl archive: it is fully static and
carries no libc dependency. `install.sh` retries with it automatically when
a glibc mismatch is what stopped the binary from starting.

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
jwc check          # type-check a project without touching the database
```

`jwc check` needs no connection. Everything that talks to Postgres —
`serve`, `migrate`, `test` — reads `DATABASE_URL` (or `JWC_DATABASE_URL`).

## Next

[Hello world](./hello-world) is a running service in about twenty lines.

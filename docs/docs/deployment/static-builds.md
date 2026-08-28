---
sidebar_position: 5
title: "Static builds (musl)"
description: "The fully-static Linux binary: when you need it, how to install it, and how to build one from a program."
---

# Static builds (musl)

Two Linux builds are published for each architecture: a glibc one and a
**musl** one that links libc statically and depends on nothing on the host.

| | Depends on |
|---|---|
| `jwc-vX.Y.Z-x86_64-linux.tar.gz` | glibc 2.35 or newer |
| `jwc-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` | nothing |
| `jwc-vX.Y.Z-aarch64-linux.tar.gz` | glibc 2.35 or newer |
| `jwc-vX.Y.Z-aarch64-unknown-linux-musl.tar.gz` | nothing |

## When you need it

The glibc build is fine on Ubuntu 22.04, Debian 12, RHEL 9 and Amazon
Linux 2023 — the release is built on glibc 2.35 for exactly that reason.
Take the musl one when the host is **older than that**, or has no glibc at
all:

- Alpine, and anything else built on musl
- `FROM scratch` and distroless images
- an older enterprise distro (CentOS 7, Ubuntu 20.04)
- a shell on Android

The symptom that sends you here is unambiguous:

```
jwc: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found
```

## Installing it

```bash
curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | JWC_MUSL=1 bash
```

The variable goes before `bash`, not before `curl` — `bash` is what reads
it. Without it the script starts on glibc and falls back to musl if the
host cannot run the glibc binary.

## A static binary from your own program

`jwc build` hands the generated crate to cargo, so the target triple is
cargo's:

```bash
rustup target add x86_64-unknown-linux-musl
jwc build . --release --target x86_64-unknown-linux-musl
```

The binary lands under `bin/<triple>/<profile>/`, so several targets
coexist:

```
bin/x86_64-unknown-linux-musl/release/app
```

`--target` requires the toolchain to have that target already; `jwc build`
does not install it for you, and the error cargo produces when it is
missing names the `rustup target add` line to run.

### One dependency needs saying out loud

Database TLS goes through `native-tls` → OpenSSL, and musl has no system
OpenSSL. The build vendors and compiles OpenSSL from source for the musl
target — which is why a musl build is **slower** than a glibc one, and why
it needs a C compiler present. That is a build-time cost only; the binary
still depends on nothing.

## A `FROM scratch` image

```dockerfile
FROM rust:slim AS build
RUN rustup target add x86_64-unknown-linux-musl \
 && apt-get update && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*
# … fetch jwc, then:
RUN jwc build /src --release --target x86_64-unknown-linux-musl

FROM scratch
COPY --from=build /src/bin/x86_64-unknown-linux-musl/release/app /app
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
EXPOSE 8080
ENTRYPOINT ["/app"]
```

Copy the CA bundle. A `scratch` image has no certificates, and without
them every outbound TLS connection fails — Postgres over TLS, `http.*`,
and a JWKS fetch — with an error that says the certificate could not be
verified rather than that there were none to verify with.

There is no shell in the image, so a `HEALTHCHECK` using `wget` or `curl`
will not work. Use the platform's HTTP probe against `/healthz` instead of
a container-level check.

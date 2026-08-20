---
sidebar_position: 3
description: "A fully static x86_64-unknown-linux-musl build of jwc, for distroless and scratch containers with no libc to link against."
---

# Static binary (musl)

JWC ships a fully static `x86_64-unknown-linux-musl` build alongside the
glibc binary on every tagged release. The musl binary has **no dynamic
library dependencies** — it runs unchanged on Alpine, distroless,
`scratch`-based images, and on long-lived hosts whose glibc is too old to
ABI-match the standard release binary.

Asset name (release v0.9.2 as the working example):

```
jwc-v0.9.2-x86_64-unknown-linux-musl.tar.gz
jwc-v0.9.2-x86_64-unknown-linux-musl.tar.gz.sha256
```

## When to use musl

- **Alpine** images (no glibc; the default Linux tarball will not run).
- **distroless** / `gcr.io/distroless/static` and `scratch` images.
- Hosts pinned to a glibc older than the GitHub Actions `ubuntu-latest`
  runner that produced the standard release binary (manifests as
  `GLIBC_2.XX not found` at startup).
- "I don't want to care about libc compatibility at all" — the musl
  binary is the safest single artifact you can ship into an unknown
  Linux environment.

## When NOT to use musl

- On a normal glibc host, the glibc binary is the better default. musl's
  allocator and a handful of syscall paths can be marginally slower under
  heavy load; for JWC's workload (HTTP + Postgres) this is rarely
  measurable, but if you have the choice, glibc is the safer pick.
- If you already build a custom container with the glibc binary baked in
  and have no portability problem to solve, there is no reason to switch.

## Verify the binary really is static

```bash
file jwc
# jwc: ELF 64-bit LSB pie executable, x86-64, ..., statically linked, ...
ldd jwc
# not a dynamic executable
```

If `file` says "dynamically linked" or `ldd` lists shared objects, you
grabbed the wrong artifact — re-download the `*-unknown-linux-musl.tar.gz`
asset, not the plain `*-linux.tar.gz` one. Both architectures publish the
pair: `x86_64-unknown-linux-musl` / `x86_64-linux`, and
`aarch64-unknown-linux-musl` / `aarch64-linux`. `install.sh` picks the musl
one for the host architecture when `JWC_MUSL=1` is set.

## Verify the checksum

Every release publishes a sibling `.sha256` next to the tarball:

```bash
curl -fLO https://github.com/just-web-code/jwc-lang/releases/download/v0.9.2/jwc-v0.9.2-x86_64-unknown-linux-musl.tar.gz
curl -fLO https://github.com/just-web-code/jwc-lang/releases/download/v0.9.2/jwc-v0.9.2-x86_64-unknown-linux-musl.tar.gz.sha256
sha256sum -c jwc-v0.9.2-x86_64-unknown-linux-musl.tar.gz.sha256
```

`install.sh` does this automatically when a `.sha256` asset is published.

## Cross-build locally

You don't need GitHub Actions to reproduce the musl binary — any Linux
box with a Rust toolchain and `musl-tools` can build it:

```bash
# one-time setup
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools      # Debian/Ubuntu
# or: apk add musl-dev gcc               # Alpine
# or: pacman -S musl                     # Arch

# build
cargo build --release --target x86_64-unknown-linux-musl --bin jwc --bin jwc-lsp

# strip for size (optional; CI does this)
strip target/x86_64-unknown-linux-musl/release/jwc

# confirm
file   target/x86_64-unknown-linux-musl/release/jwc
ldd    target/x86_64-unknown-linux-musl/release/jwc   # "not a dynamic executable"
```

On macOS or Windows the easiest path is the Docker `messense/rust-musl-cross:x86_64-musl`
image — `cargo` runs inside the container with all musl headers and
linkers preinstalled.

## Container usage

`FROM scratch` works because the binary has no runtime dependencies:

```dockerfile
FROM scratch
COPY jwc /jwc
COPY ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
EXPOSE 8080
ENTRYPOINT ["/jwc"]
```

The CA bundle is the only thing you actually need to copy in for TLS to
work; without it, outbound HTTPS (and Postgres-over-TLS) fails at handshake.
Alpine and `gcr.io/distroless/static` already ship one — `scratch` does
not.

See also the [Docker deployment guide](docker.md) for the standard
debian-slim image; the musl static binary is the alternative for the
Alpine / distroless / scratch path.

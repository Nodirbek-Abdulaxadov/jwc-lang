---
sidebar_position: 1
description: "Install the JWC toolchain on Linux (x86_64 or arm64) and Windows with a single command, or build it from source on any platform Rust supports. Ships the jwc compiler and the jwc-lsp language server."
---

# Install

## Supported platforms

| OS | Architecture | Prebuilt binary | Install with |
|---|---|---|---|
| Linux | x86_64 | ✅ | the one-liner below |
| Linux | arm64 / aarch64 | ✅ | the one-liner below |
| Windows | x86_64 | ✅ | the PowerShell one-liner below |
| macOS | Intel or Apple Silicon | ❌ | [build from source](#build-from-source) or [Docker](#docker) |
| Windows | ARM | ❌ | [build from source](#build-from-source) |
| anything else | — | ❌ | [build from source](#build-from-source) |

Both Linux architectures also publish a fully-static **musl** build that runs
unchanged on Alpine, distroless and old-glibc hosts — set `JWC_MUSL=1` before
running the installer. See [musl static builds](../deployment/musl-static.md).

:::note macOS has no prebuilt binary
There is no `jwc` download for macOS on either architecture — the installer
will stop with `Unsupported platform: darwin-arm64`. Build from source or use
the Docker image; both work on macOS. Prebuilt macOS binaries are a
[declared non-goal](https://github.com/just-web-code/jwc-lang/blob/main/ROADMAP.md)
for now, not an oversight.
:::

## One-liner

PowerShell (Windows x86_64):

```powershell
iex "& { $(irm https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.ps1) }"
```

Bash (Linux x86_64 / arm64):

```bash
curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | bash
```

Drops `jwc` and `jwc-lsp` into `%LOCALAPPDATA%\jwc\bin` (Windows) or
`~/.jwc/bin` (Linux) and prints how to add it to `PATH`. The architecture is
detected from `uname -m`; pass `JWC_VERSION=v0.9.6` to pin a specific release
or `JWC_INSTALL_DIR=/opt/jwc/bin` to install elsewhere.

## Docker

Needs no toolchain, and covers macOS and every other platform. The images are
multi-arch (`linux/amd64` and `linux/arm64`):

```bash
docker run --rm -it ghcr.io/just-web-code/jwc:latest --help
```

`ghcr.io/just-web-code/jwc-runtime` is the minimal distroless base for shipping
JWC apps built with `jwc build --native`.

## Build from source

Works anywhere Rust does, including macOS and Windows ARM. Requires the
[Rust toolchain](https://rustup.rs/) (1.83+).

```bash
git clone https://github.com/just-web-code/jwc-lang
cd jwc-lang
cargo install --path .         # → ~/.cargo/bin/jwc
```

## Verify

```bash
jwc --version    # → jwc 0.9.6 (or newer), plus build target and commit
jwc --help
```

After install, scaffold a project with [`jwc new --template ...`](./templates.md) — the `api`, `auth`, and `jobs` templates give you a working starter in one command.

## Update to the latest release

The CLI tracks its own version. Re-run the install one-liner above; existing config in `~/.jwc/` (credentials, registry cache) is preserved.

## What you also need (per feature)

| Want | Need |
|---|---|
| Run interpreter | Just `jwc` |
| Postgres-backed apps | A running Postgres (any 13+) reachable via `DATABASE_URL` / `JWC_DATABASE_URL` |
| Native AOT (`jwc build --native`) | [Rust toolchain](https://rustup.rs/) on `PATH` |
| Cross-compile (`--target`) | The matching `rustup target add <triple>` |
| `jwc publish` | An API key from [registry-jwc.1kb.uz](https://registry-jwc.1kb.uz/#/keys) (Google login) |
| Editor support | [JWC Language extension](https://marketplace.visualstudio.com/items?itemName=jwc-extension.jwc-lang) on VS Code Marketplace (or build the `.vsix` from `vscode-extension/`) |

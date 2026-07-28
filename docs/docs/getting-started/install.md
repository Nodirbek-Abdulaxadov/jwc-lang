---
sidebar_position: 1
---

# Install

## Windows / Linux / macOS — one-liner

PowerShell:

```powershell
iex "& { $(irm https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.ps1) }"
```

Bash:

```bash
curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
```

Drops the `jwc` binary into `%LOCALAPPDATA%\jwc\bin` (Windows) or `~/.jwc/bin` (Unix) and prints how to add it to `PATH`.

## Verify

```bash
jwc --version    # → jwc 0.4.9 (or newer)
jwc --help
```

After install, scaffold a project with [`jwc new --template ...`](./templates.md) — the `api`, `auth`, and `jobs` templates give you a working starter in one command.

## Build from source

Requires the [Rust toolchain](https://rustup.rs/) (1.83+).

```bash
git clone https://github.com/Nodirbek-Abdulaxadov/jwc-lang
cd jwc-lang
cargo install --path .         # → ~/.cargo/bin/jwc
```

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

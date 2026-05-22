# JWC Language for VS Code

Syntax highlighting, snippets, and LSP-powered diagnostics for [JWC (Just Web Code)](https://jwc.1kb.uz).

## Features

- Syntax highlighting for `.jwc` files (including the package keywords
  `namespace`, `import`, `mount`, `group`, `public`, `private`).
- Snippets for routes, entities, queries, CRUD scaffolds, and package
  pieces (`namespace`, `import`, `mount`, `group`, `pub-fn`, `priv-fn`,
  `pub-middleware`).
- Diagnostics (parse + validate + lint) via `jwc-lsp`.
- Hover info on entities / classes / functions.

## Requirements

Install the `jwc` toolchain (which ships `jwc-lsp`):

```bash
curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
```

The extension auto-discovers `jwc-lsp` on `PATH` or under `~/.jwc/bin`. Override via the `jwc.lspPath` setting.

## Settings

- `jwc.lspPath` — explicit path to the `jwc-lsp` binary.
- `jwc.trace.server` — `off` / `messages` / `verbose` LSP trace.

## Commands

- `JWC: Restart Language Server`
- `JWC: Show Language Server Output`

## Build from source

```bash
npm install
npm run compile
npm run package
```

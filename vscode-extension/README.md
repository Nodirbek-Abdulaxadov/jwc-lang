<p align="center">
  <img src="https://raw.githubusercontent.com/just-web-code/jwc-lang/main/vscode-extension/icon.png" alt="JWC" width="128">
</p>

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

### Navigation: Go to Definition

Press `F12` (or `Ctrl+click`) on any top-level `entity`, `class`, `function`,
`middleware`, or `dbcontext` reference. The LSP resolves the identifier
against the per-document symbol index built on each save and jumps to the
declaration site.

### Refactoring: Rename Symbol

Press `F2` on a top-level symbol to rename it. The new name is validated
against `^[A-Za-z_][A-Za-z0-9_]*$` and rejected if it collides with an
existing top-level declaration in the same document. Every reference is
rewritten in a single workspace edit; matches inside comments and string
literals are skipped.

### Smart Completion

Completion is context-aware:

- Typing inside `catch (e: ...)` lists the JWC error kinds (`DbError`,
  `HttpError.NotFound`, `ValidationError`, ...).
- Typing after `use ` on a `route` / `group` lists the declared middleware
  names.
- Anywhere else, completion offers JWC keywords + built-in functions
  (`json`, `body`, `now`, ...) + user-defined functions from the
  current document.

## Requirements

Install the `jwc` toolchain (which ships `jwc-lsp`):

```bash
curl -fsSL https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.sh | bash
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

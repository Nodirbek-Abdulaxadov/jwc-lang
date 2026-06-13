# Changelog

All notable changes to the **JWC Language** VS Code extension are documented
here. The extension version tracks the JWC compiler version it ships against.

## 0.4.7 — First marketplace release

- First marketplace release of the JWC extension for the editor.
- LSP-backed diagnostics, hover, and document symbols via `jwc-lsp`.
- Syntax highlighting via a TextMate grammar (`syntaxes/jwc.tmLanguage.json`).
- Snippets for routes, entities, queries, CRUD scaffolds, packages
  (`namespace`, `import`, `mount`, `group`, `pub-fn`, `priv-fn`,
  `pub-middleware`).
- Commands: `JWC: Restart Language Server`, `JWC: Show Language Server Output`.
- Settings: `jwc.lspPath`, `jwc.trace.server`.
- Auto-discovers `jwc-lsp` on `PATH` and under `~/.jwc/bin`.

> Advanced LSP features — go-to-definition, rename, completion — are
> landing in parallel under Phase 8D and will appear in a follow-up
> release (target: 0.4.8).

## 0.4.2 — Pre-marketplace internal build

- Snippets for the package-manager syntax (`namespace`, `import`, `mount`,
  `group`, `pub-*`, `priv-*`).
- Hover info on entities, classes, and functions.

## 0.4.1 — Pre-marketplace internal build

- Initial TypeScript scaffolding, LSP client wiring, TextMate grammar.

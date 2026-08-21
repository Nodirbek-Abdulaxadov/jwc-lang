---
sidebar_position: 4
title: Editor setup
description: "Syntax highlighting, diagnostics and formatting through the JWC language server."
---

# Editor setup

## VS Code

The extension in
[`vscode-extension/`](https://github.com/just-web-code/jwc-lang/tree/main/vscode-extension)
carries the grammar and starts the language server. Build and install it
locally:

```bash
cd vscode-extension
npm install && npx vsce package
code --install-extension jwc-*.vsix
```

It expects `jwc` on `PATH`.

## The language server

Any LSP-capable editor can talk to it:

```bash
jwc lsp
```

It speaks over stdio and provides:

- **diagnostics** — the same codes `jwc check` prints, as you type;
- **hover** — the type of a name, and the clause a diagnostic cites;
- **go to definition** — for tables, classes, services and functions;
- **formatting** — the same output as `jwc fmt`.

### Neovim

```lua
vim.lsp.start({
  name = 'jwc',
  cmd = { 'jwc', 'lsp' },
  root_dir = vim.fs.dirname(vim.fs.find({ 'jwcproj.json' }, { upward = true })[1]),
})
```

## Formatting

`jwc fmt` is not configurable, on purpose:

```bash
jwc fmt .            # rewrite in place
jwc fmt --check .    # non-zero if anything would change — for CI
```

The formatter is idempotent and its output is what the corpus is pinned
against, so a diff in CI means a real change and never a style argument.

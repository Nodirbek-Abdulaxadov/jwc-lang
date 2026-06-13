---
sidebar_position: 4
---

# Editor setup

The official editor support for `.jwc` ships as a VS Code extension that
points at the bundled `jwc-lsp` language server (errors, hovers,
go-to-definition).

## VS Code

Install [JWC Language](https://marketplace.visualstudio.com/items?itemName=Nodirbek-Abdulaxadov.jwc-lang)
from the Marketplace. The extension auto-launches `jwc-lsp` if it is on
your `PATH`; otherwise point it at the binary in extension settings.

## Format on save

`jwc fmt` is the canonical formatter — see the
[`jwc fmt` reference](../reference/fmt.md) for the full rules. Wire it
into save-time in VS Code by dropping the snippet below into the
project's `.vscode/settings.json`:

```json
{
    "editor.formatOnSave": true,
    "[jwc]": {
        "editor.defaultFormatter": "Nodirbek-Abdulaxadov.jwc-lang"
    }
}
```

If you prefer a shell hook (works in any editor that lets you run a
command on save), use:

```bash
jwc fmt path/to/file.jwc
```

The formatter is idempotent and safe to run repeatedly — files that are
already canonical are left untouched.

### CI gate

Add a check step to your pipeline so untouched-by-the-formatter files
fail the build:

```bash
jwc fmt --check .
```

Exit code 1 with a list of paths on stderr means at least one file
would be rewritten.

## Other editors

Any editor that speaks LSP can drive `jwc-lsp` directly. Point it at
the binary on `PATH` and use stdio transport — no extra arguments.

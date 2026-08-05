# Changelog

All notable changes to the **JWC Language** VS Code extension are documented
here. The extension version tracks the JWC compiler version it ships against.

## 0.8.7 — Console and filesystem built-ins

Editor support for the sixteen new built-ins the compiler gained in this
release: `console.write` / `console.error` / `console.read`, the `file.*`
family (`read`, `write`, `append`, `exists`, `delete`, `copy`, `move`,
`size`, `lines`) and `directory.*` (`list`, `create`, `exists`, `delete`).

- **Completion** comes for free — the language server reads the
  compiler's own builtin table, so the new names and their arities appear
  without an extension-side list to maintain.
- **Syntax highlighting** did need the list: the grammar carries a
  hardcoded builtin pattern. The dotted names are placed ahead of the
  bare ones in the alternation, since a `file` alternative would
  otherwise win the leftmost match over `file.read`.
- **Typed catch** now offers the new `IoError` kind and its
  `.NotFound` / `.PermissionDenied` / `.AlreadyExists` subtypes.

## 0.8.5 — A logo of its own

The extension ships a new icon: a teal hummingbird, replacing the purple
one. It is the same mark the documentation site now uses for its favicon,
navbar and social card — all four are generated from a single master
(`vscode-extension/logo-source.png`) by `tools/gen-logo-assets.py`, which
exists so the set stays in step. The gallery banner moves off the purple
it was matched to.

Nothing about the editor experience changes in this release. The compiler
side is a correctness release: SQL parameters are bound from the column's
declared type rather than the value's Rust shape — so
`where User.id == @id` with an `int` primary key stops answering 500 —
and wrong-arity builtin calls are now rejected at `jwc check` (E022)
instead of compiling to a no-op that answers 200. That last one is a
breaking change; see the root `CHANGELOG.md`.

## 0.8.0 — Back on the right extension, and the query-layer release

**If you were stuck on 0.4.7, this is why.** Every release from 0.5.2
onward was published under a second identifier —
`Nodirbek-Abdulaxadov.jwc-language` — while the extension you installed is
`jwc-extension.jwc-lang`. `vsce publish` creates a new extension when the
identifier changes, prints "Published", and exits 0, so the pipeline stayed
green for two months while the marketplace listing never moved. Publishing
now goes back to `jwc-extension.jwc-lang`, and CI fails if the identifier
ever drifts again.

That means this update carries everything from 0.4.8 through 0.8.0 at once.
The editor-facing highlights, all of which landed in 0.7.0:

- Diagnostics resolve against the **whole project**, not one file. Before,
  a project that `jwc lint` accepts showed 12 phantom problems because
  entities in `Data/` were invisible to routes in another file.
- **Format-on-save works.** The capability was never advertised, so the
  request came back `-32601 Method not found`.
- **Go-to-definition and rename work across files.** Rename used to resolve
  a symbol declared elsewhere and then edit only the current file.
- `async` / `public` / `private` on dome members; the shipped "Async
  Function" snippet no longer expands into a parse error.
- The `transaction` snippet is fixed, and the extension's own manifest no
  longer puts a warning in your Problems panel.
- The extension tells you when the installed `jwc-lsp` is behind it.

The compiler side of 0.8.0 is a query-layer release — a `where` clause
could silently drop an `and` / `or` term, `having` accepts aggregates, and
`select distinct` exists. See the root `CHANGELOG.md`.

## 0.7.0 — The Problems panel tells the truth

Everything below was reported from real projects, where the extension's
diagnostics disagreed with the compiler that ships alongside it.

- **Diagnostics are project-wide.** The server used to parse the open file
  alone, but a JWC project is one flat namespace — entities in
  `Data/`, middleware in `Infrastructure/`, routes in a third file — so
  every cross-file reference came back "unknown". On a project that
  `jwc lint` and `jwc run` both accept, the panel showed 12 problems,
  including the same middleware flagged as both "declared but never
  attached to a route" and "unknown". It now resolves against the merged
  project, with open editor buffers substituted so diagnostics don't lag a
  save behind. A sibling file that doesn't currently parse is skipped
  rather than blanking out the file in front of you.
- **Warnings land on the file that declares the symbol**, and validation
  errors are anchored at their real line instead of line 1. One error used
  to be republished on all 20 files in the project.
- **Format-on-save works.** `textDocument/formatting` was never
  advertised, so the request came back `-32601 Method not found` and the
  editor silently did nothing.
- **Go-to-definition and rename work across files.** Rename previously
  resolved a symbol declared in a sibling file and then edited only the
  current one.
- **`async` and `public` / `private` on dome members.** The shipped
  "Async Function" snippet expanded into a parse error inside a dome,
  which is where business logic lives.
- **The `transaction` snippet is fixed.** It proposed
  `transaction <Name> { … }`; the statement takes no name, so accepting the
  completion broke the file.
- **No more self-inflicted warning.** `activationEvents: ["onLanguage:jwc"]`
  is generated by VS Code from the `contributes.languages` entry, so
  declaring it put a warning in the user's Problems panel — from the
  extension's own manifest.
- **Version-skew warning.** The extension updates from the marketplace,
  `jwc-lsp` only moves when someone reruns the installer. When the binary
  is behind, the extension now says so instead of letting old diagnostics
  flag valid code. Patch differences are ignored; a local build never nags.

Every shipped snippet is now parsed by `tests/snippets_parse.rs` in CI, so
they can't drift from the grammar again.

## 0.5.2 — Marketplace publish fixes

> **Superseded — see 0.8.0.** The rename below is what quietly split this
> extension in two. The "already taken" name belonged to *this* extension
> under the `jwc-extension` publisher, not to a stranger, so renaming around
> it published to a new listing instead of updating the existing one. No
> release between here and 0.8.0 ever reached anyone.

- Extension id renamed to `jwc-language` and the display name made unique;
  the publish step had failed with "display name is taken".
- `package-lock.json` synced with `package.json`.

No editor-facing changes.

## 0.4.8 — Go-to-definition, rename, completion

- Go-to-definition and rename via `jwc-lsp` (Phase 8D), as promised in the
  0.4.7 note below.
- Context-aware completion, with `.`, `:` and space as trigger characters.

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

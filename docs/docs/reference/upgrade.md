---
sidebar_position: 5
description: "jwc upgrade runs automated codemods over your project to migrate off deprecated syntax and builtins. Available migrations and how to apply them."
---

# `jwc upgrade` — deprecation codemod

`jwc upgrade` runs a registry of automated migrations over your project's
`.jwc` source files. Each rule rewrites legacy syntax / flags that a future
JWC version removes — so when you bump the toolchain, your project keeps
compiling.

## Status at v0.4.8

The rule registry is **empty** at v0.4.8. Nothing has been removed yet, so
`jwc upgrade` reports "no rules registered" and exits clean. The command
ships now so the CLI shape is stable and rules can land in v0.5 / v0.6
without breaking your `jwc upgrade && jwc test && jwc publish` workflow.

The first scheduled rule will retire `--no-typecheck` (see
[`DEPRECATION.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/DEPRECATION.md))
in v0.6.0. When that lands, `jwc upgrade` will scan for build / CI scripts
that pass `--no-typecheck` and rewrite them (or leave a clear failure when
the flag is in a non-recognised template).

## Usage

```bash
# Apply every applicable rule in place.
jwc upgrade

# Show what would change without writing.
jwc upgrade --dry-run

# Target specific files / dirs (defaults to the current project root).
jwc upgrade src/ tests/legacy.jwc
```

## What it walks

- Recursively visits every `.jwc` file under each input path.
- Skips `.jwc-build/`, `target/`, `node_modules/`, `.git/`, `bin/`, `obj/`.
- Default input (no positional arg) is the directory that owns the closest
  `*.jwcproj` upward from `cwd`.

## What each rule outputs

Every rule has a stable `id` (`no-typecheck-removed`, etc.) printed
alongside the file path when the rewrite fires. `--dry-run` prints the
same lines + a `(dry-run: not written)` marker.

```
[no-typecheck-removed] src/main.jwc
  (dry-run: not written)
jwc upgrade --dry-run: 1/47 file(s) would change.
```

## How rules work (for contributors)

A rule implements the `UpgradeRule` trait in `src/cmd/upgrade.rs`:

```rust
pub trait UpgradeRule {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn apply_to_file(&self, path: &Path, src: &str) -> Option<String>;
}
```

`apply_to_file` returns `Some(new_text)` when the rule rewrote the file or
`None` when nothing applied. Rules run in order; each rule sees the
post-previous-rule text so chains compose cleanly.

Register a new rule:

1. Implement the trait in `src/cmd/upgrade.rs` (or a sibling module).
2. Add it to the `rules()` Vec in `src/cmd/upgrade.rs`.
3. Update the registry test (`rules_registry_is_empty_at_v047`) to
   `rules_registry_has_<n>_rules_at_v<version>`.
4. Add a fixture under `tests/upgrade_fixtures/` + a parameterised
   conformance-style test.
5. Mention the rule in `DEPRECATION.md` with the version that introduces it.

## Roadmap

| Version | Rule | What it does |
|---|---|---|
| v0.6.0 | `no-typecheck-removed` | Drops `--no-typecheck` from build/run/check invocations once the gradual checker stabilises. |
| TBD | `legacy-error-codes` | If we rename any E-codes (E018, E019, ...) those PR notes will list the rule that rewrites references. |

When you ship a 1.0 deprecation pass, the rule for it should land here
*before* the removal — users run `jwc upgrade && jwc test` against the
last working version, then bump to the new version.

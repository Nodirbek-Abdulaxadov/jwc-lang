# `jwc fmt`

Canonicalise the whitespace and shape of `.jwc` source files.

The formatter has two tiers and picks one per file based on a coarse
comment heuristic:

| Tier         | When it runs                                       | What it does                                                                                              |
| ------------ | -------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| AST renderer | source has no `//` or `/*` AND parses cleanly      | Round-trips through the AST and re-emits canonical, opinionated output. Asserts a single house style.     |
| Line-based   | source contains `//` or `/*`, OR parsing fails    | Normalises whitespace only; preserves every existing byte the rules below leave alone.                   |

Both tiers are **idempotent**: `format(format(src)) == format(src)`.

## CLI

```text
jwc fmt [PATHS...] [--check] [--stdout]
```

- `PATHS` — zero or more files / directories. Defaults to the current
  working directory. Directories are walked recursively, skipping
  `.jwc-build`, `target`, `node_modules`, and `.git`.
- `--check` — do not write anything. Exit code 0 if every file is already
  canonical; exit code 1 (with a list of paths on stderr) if any file
  would be rewritten. The recommended CI mode.
- `--stdout` — read each input and write the formatted result to stdout
  instead of rewriting the file on disk. Ignored when `--check` is also
  set (check wins).

### Examples

```bash
# Rewrite every .jwc file under the current project (default).
jwc fmt

# Format a single file.
jwc fmt src/main.jwc

# Format a curated set in CI without touching disk.
jwc fmt --check examples templates

# Pipe a single file through the formatter for diffing.
jwc fmt --stdout src/main.jwc | diff src/main.jwc -
```

## Comment-preservation policy

The parser drops lexical comments, so re-emitting from the AST would
silently delete every `// ...` and `/* ... */` block. The formatter
avoids this by routing any file that contains either delimiter through
the line-based tier — which only touches whitespace and never moves or
rewrites tokens.

The detector is intentionally coarse (a substring scan): false positives
(e.g. `//` inside a string literal) are safe — they just opt the file
out of the AST tier; false negatives would lose code, so the gate errs
toward preservation.

### Skipping individual files

Add `// FMT: skip` as the **first line** of a `.jwc` file to opt it out
of the idempotency test harness. The CLI still touches the file on
demand; the marker only excludes it from the test fixture corpus.

## Line-based rules

When the line-based tier runs:

1. Tabs are expanded to four spaces.
2. Trailing whitespace at end of each line is stripped (including stray `\r`).
3. Three or more consecutive blank lines collapse to two.
4. The file ends with exactly one trailing newline.

## AST renderer rules

When the AST tier runs, the output follows this house style:

- Indentation: 4 spaces, never tabs.
- Braces: K&R-style on the same line as the introducing keyword.
- One declaration per line; statements terminated with `;`.
- Declaration order in a single file:
  1. `using` imports
  2. `const` bindings
  3. `dbcontext`
  4. `entity` / `class`
  5. `mount`
  6. `middleware`
  7. `function`
  8. `route`
  9. top-level `error catch (…)` handler
- Within each group, the source order from the merged `Program` is
  preserved.
- Composite expressions appearing as operands are parenthesised so a
  reparse yields the same AST shape.

## CI integration

The CI workflow runs `cargo run --bin jwc -- fmt --check examples templates`
as a soft gate during the rollout (`continue-on-error: true`). The
intent is to surface drift without blocking unrelated changes; the
gate will be required once the bundled fixtures are fully reformatted.

## Programmatic API

The library crate re-exposes the underlying entry points:

```rust
use jwc::fmt::{format_source, format_program, is_formatted, has_comments};

let canonical: String = format_source(my_src);
let dirty: bool = !is_formatted(my_src);
```

`format_program(&Program)` is the AST-only entry point — useful when
you've already parsed once and want to skip the comment heuristic.

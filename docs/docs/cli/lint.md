---
sidebar_position: 2
description: "The jwc lint warning catalog. Every Wxxx code, what triggers it and how to fix it. Run jwc lint --list-codes for the machine-readable version."
---

# Lint codes

Run `jwc lint --list-codes` for the live machine-readable catalog.

## Warnings (`W001` – `W006`)

| Code | Meaning |
|---|---|
| `W001` | function declared but never called |
| `W002` | middleware declared but never attached to a route |
| `W003` | function body is empty (returns `null` silently) |
| `W004` | single-row select on PK is missing `first` (returns array instead of row) |
| `W005` | user-declared function shadows a built-in name |
| `W006` | unreachable statement after top-level `return` |

Warnings don't fail the build. Use `--json` to surface them in CI.

## Errors (`E001` – `E010`)

These are **compile errors** raised by the validator / runtime. They abort the load.

| Code | Meaning |
|---|---|
| `E001` | unknown dbcontext referenced in `select` / `insert` / `update` / `delete` |
| `E002` | unknown entity referenced in `select` / `insert` / `update` / `delete` |
| `E003` | entity / dbcontext mismatch on select or mutation |
| `E004` | unknown column in `WHERE` / `ORDER BY` / projection / `GROUP BY` |
| `E005` | duplicate route declaration (same method + path) |
| `E006` | navigation property references an unknown entity / column |
| `E007` | `validate body` block has no fields |
| `E008` | unknown catch type — must be one of `Error`, `DbError`, `HttpError`, `ValidationError`, `TimeoutError` |
| `E009` | `HAVING` used without `GROUP BY` |
| `E010` | `register_job_handler` references a function that doesn't exist |

Errors are tagged with the `[Exxx]` prefix in the message so editor/CI parsers can match them by code.

## Editor integration

```bash
jwc lint --json
# → [{"code":"W004","message":"single-row `select User` on PK `id` is missing `first` — ..."}]
```

```bash
jwc lint --explain W004
# → W004: single-row select on PK is missing `first` (returns array instead of row)
```

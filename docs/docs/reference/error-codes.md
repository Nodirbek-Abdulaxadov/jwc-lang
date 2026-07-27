---
sidebar_position: 2
---

# Error & warning codes

JWC ships a numbered diagnostic catalog. **`Wxxx`** codes are lint
warnings (non-fatal); **`Exxx`** codes are validator / parser errors
(fatal). The single source of truth is
[`src/error_codes.rs`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/src/error_codes.rs)
— this page mirrors it and keeps the "why this exists" prose.

## Lint warnings (`W001`–)

| Code | Description |
|---|---|
| `W001` | Function declared but never called. |
| `W002` | Middleware declared but never attached to a route. |
| `W003` | Function body is empty (returns null silently). |
| `W004` | Single-row `select` on PK is missing `first` (returns array instead of row). |
| `W005` | User-declared function shadows a built-in name. |
| `W006` | Unreachable statement after top-level `return`. |

## Validator errors (`E001`–`E021`)

### Select / mutation referent errors

| Code | Description |
|---|---|
| `E001` | Unknown dbcontext referenced in select / insert / update / delete. |
| `E002` | Unknown entity referenced in select / insert / update / delete. |
| `E003` | Entity / dbcontext mismatch on select or mutation. |
| `E004` | Unknown column in WHERE / ORDER BY / projection / GROUP BY. |
| `E006` | Navigation property references an unknown entity / column. |
| `E009` | `HAVING` used without `GROUP BY`. |

### Routes / handlers

| Code | Description |
|---|---|
| `E005` | Duplicate route declaration (same method + path). |
| `E007` | `validate body` block has no fields. |
| `E008` | Unknown `catch` type — must be one of the known `JwcErrorKind` names. |
| `E010` | `register_job_handler` references a function that doesn't exist. |
| `E014` | Route handler references a function that doesn't exist. |

### Bulk mutations

| Code | Description |
|---|---|
| `E011` | Atomic `update CTX.Table set ...` requires a `where` clause (would touch every row). |
| `E012` | Atomic `update` SET list must contain at least one assignment. |
| `E013` | Bulk `delete from CTX.Table` requires a `where` clause (would truncate the table). |

### E016 / E017 — transactions & savepoints

| Code | Description |
|---|---|
| `E016` | Literal nested `transaction { ... }` rejected (use `savepoint` instead). |
| `E017` | `savepoint <name> { ... }` declared outside an enclosing `transaction` block. |

**Why E016 exists.** Postgres' `BEGIN` inside a `BEGIN` silently
becomes a `SAVEPOINT`, so a literal nested `transaction { ... }` would
*look* like a fresh transaction but actually behave like a savepoint —
the outer error path would never see the inner failure as a full
rollback. We refuse the ambiguity at compile time. Fix: either remove
the nesting (one block does both pieces of work), or use the explicit
`savepoint` form so the partial-rollback intent is visible.

**Why E017 exists.** A bare `savepoint name { ... }` outside a
transaction has no surrounding `BEGIN` to attach to — Postgres would
reject it at runtime with `SAVEPOINT can only be used in transaction
blocks`. Catching it in the runner gives a clearer message + the
correct fix:

```jwc no-compile
transaction {
    savepoint try_charge { ... }
}
```

See [data/transactions](../data/transactions.md#savepoints--partial-rollback).

### E018 / E019 / E020 — function signature checks

Sprint 7 closed the typed call-site gap that let arity / type errors
slip past the validator.

| Code | Description |
|---|---|
| `E018` | Return type mismatch: function body returns a value incompatible with the declared return type. |
| `E019` | Wrong number of arguments at a user-function call site. |
| `E020` | Argument type at a user-function call site does not match the declared parameter type. |

**Why these exist.** Until Sprint 7, calling `addUser("ali", 30)`
against `function addUser(name: string)` was a runtime panic deep in
the eval loop. The validator now walks every call site, matches it
against the declared signature, and fails fast with a span pointing at
the offending argument. The same pass catches return-type drift in the
function body (`function foo(): int { return "x"; }`).

Typical fix: align the call site with the signature, or relax the
parameter type to `any` if the caller really is polymorphic.

### E015 / E021 — namespace integrity

| Code | Description |
|---|---|
| `E015` | Duplicate function declaration in the same project namespace. |
| `E021` | Private function called from outside its declaring namespace. |

**Why E021 exists.** JWC supports a `private` visibility marker on
functions; the validator enforces it across all `.jwc` files in the
project so a refactor that moves a function out of its module doesn't
silently turn an internal helper into part of the public surface.

## Looking up a code programmatically

`jwc lint --json` emits each diagnostic with its code so editors and CI
can build their own UI. `src/error_codes.rs` exposes `lookup_warning` /
`lookup_error` for in-process consumers; both return the same
descriptions you see in the tables above.

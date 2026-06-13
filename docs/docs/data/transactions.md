---
sidebar_position: 5
---

# Transactions

```jwc
transaction {
    insert u into AppDb.User;

    let invite = new Invite();
    invite.user_id = u.id;
    invite.token   = uuid();
    insert invite into AppDb.Invite;
}
```

Every DB op inside the block runs on a single connection in `BEGIN ... COMMIT`. Implementation detail: a `TxGuard` `Drop` performs `ROLLBACK` if the block exits abnormally (panic, early `return`, uncaught `throw`).

## Rollback

There's no explicit `rollback` keyword. Either:

- Throw / return from the block — the `TxGuard` rolls back automatically.
- Wrap in `try`:

  ```jwc
  try {
      transaction { ... }
  } catch (e) {
      // already rolled back by Drop
      return internalError({ error: e.message });
  }
  ```

## Savepoints — partial rollback

A literal nested `transaction { ... }` inside an existing block is a
compile-time error (E016) — Postgres would silently SAVEPOINT it, which
hides the intent. For the genuine "let me roll back part of this
transaction without losing the rest" case, use the **savepoint** form:

```jwc
transaction {
    insert order into AppDb.Order;

    savepoint try_charge {
        insert charge into AppDb.Charge;
        if (charge.status == "failed") {
            throw "card declined";   // rolls back just this savepoint
        }
    }

    // outer transaction is still healthy: order persists,
    // charge row was rolled back, control resumes here.
    insert audit into AppDb.AuditLog;
}
```

Semantics:

- `savepoint <name> { body }` emits `SAVEPOINT <name>` before `body` runs.
- Clean exit: `RELEASE SAVEPOINT <name>` — the inner work folds into the surrounding transaction.
- Exception / early `return` inside `body`: `ROLLBACK TO SAVEPOINT <name>` first, then the error propagates so a surrounding `try` can catch it. The outer transaction is **not** aborted.
- `<name>` must be a valid SQL identifier (`[A-Za-z_][A-Za-z0-9_]*`); the parser validates that at compile time.
- A `savepoint { ... }` block outside an enclosing `transaction { ... }` is a runtime error (**E017**: "`savepoint` is only valid inside a `transaction` block").
- Native AOT (`jwc build --native`) currently rejects projects that use savepoints — interpreter only for now.

The error catalog: see [reference/error-codes](../reference/error-codes.md#e016--e017--transactions--savepoints).

## Nesting (without savepoints)

A literal `transaction { ... }` directly inside another `transaction { ... }`
is rejected at validation with **E016** — use a savepoint instead, or
restructure the work to one block. The earlier "silent SAVEPOINT" behaviour
was surprising enough that we surface it as a compile-time error now.

## Read-only

No syntax for `BEGIN TRANSACTION READ ONLY` yet. If you need it, drop to `raw_sql` for the BEGIN.

## When you actually need it

Postgres reads in autocommit mode are already a read-only snapshot — you don't need `transaction { ... }` for a simple SELECT. Reach for it when **two or more mutations** must succeed or fail together (user + invite, payment + order, etc).

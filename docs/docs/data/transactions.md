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

## Nesting

Transactions don't nest in v1. A second `transaction { ... }` inside an existing block is a runtime error (it would silently SAVEPOINT today, which is surprising). Just structure the work as a single block.

## Read-only

No syntax for `BEGIN TRANSACTION READ ONLY` yet. If you need it, drop to `raw_sql` for the BEGIN.

## When you actually need it

Postgres reads in autocommit mode are already a read-only snapshot — you don't need `transaction { ... }` for a simple SELECT. Reach for it when **two or more mutations** must succeed or fail together (user + invite, payment + order, etc).

---
sidebar_position: 3
title: "Writes — insert, update, delete, transactions"
description: "Writes in JWC are statements, not row objects. Partial updates, conflict handling and transactions, with the race each rule exists to close."
---

# Writes

There is no row object to load, mutate and save. A write is a statement,
and the value it returns is the projection it asks for.

## Insert

```jwc no-compile
insert into App.notes.Notes {
    org_id = $org_id,
    title  = $req.title,
    body   = $req.body
} as { id, title, created_at }
```

`as { }` makes it `RETURNING`. Without it the statement returns nothing
and the expression's value is null.

### Spread

```jwc no-compile
insert into App.auth.Accounts {
    ...$req except (password),
    password_hash = $password_hash
} as { id, email, display_name }
```

`...$req` spreads the fields the value **carries**. An absent field is
omitted from the column list entirely, so the column's default applies —
which is different from sending null, and deliberately so.

### Conflicts

```jwc no-compile
insert into App.tasks.TaskLabels {
    task_id  = $task_id,
    label_id = $label_id
} on conflict (task_id, label_id) do nothing
```

This is what makes "attach this label" idempotent. Reading first and then
inserting is a race: two requests both read no row, and both insert.

The conflict target must be a real unique constraint, and on a table with
more than one it has to be named.

### Buffered

```jwc no-compile
insert into App.audit.Events {
    actor = context.account_id,
    route = request.route()
} buffered
```

`buffered` hands the row to a background writer and returns immediately.
The writer holds rows briefly, then sends one multi-row `INSERT` per
statement shape — merging is the point, because one statement per row
would take the wait off the request and leave the database doing the same
work.

This is what makes a logging `after` block worth writing. An `after` block
is awaited before the response goes out, so an ordinary insert there puts
a database round trip in front of every response.

The buffer is bounded, and a full buffer **drops** the row rather than
queueing it — the right trade for an audit trail and the wrong one for a
ledger. `/metrics` carries `jwc_log_dropped_total` alongside
`jwc_log_written_total`, `jwc_log_batches_total` and the queue depth, so
falling behind is visible before rows start disappearing.

Three things a buffered insert cannot do, because it answers before the
row exists and is not part of your transaction:

| | |
|---|---|
| `as { … }` | there is no row to project yet |
| `on conflict` | a resolution nobody observes is a row silently not written |
| inside `transaction { }` | it is written later, on another connection — a rollback would not take it back |

Each is a compile error, not a surprise at runtime.

## Update

```jwc no-compile
update App.notes.Notes
    set title = $req.title
    where id == $id
    as { id, title, updated_at }
    first
```

### Expressions the database evaluates

```jwc no-compile
update App.billing.Counters
    set value = value + 1
    where name == "invoice"
    as { value }
    first
```

`value + 1` reads the row it is writing, so it belongs in the database.
Computing it here would need a read first, and two callers doing that both
read the same number and both write the same result — one increment lost.
The rule is mechanical: an expression over the row's own columns is
emitted as SQL, everything else is bound.

### Partial updates

```jwc no-compile
update App.notes.Notes
    set title =? $req.title, body =? $req.body
    where id == $id
    as { id, title, body }
    first
```

`=?` skips the assignment when the value is absent. A `PATCH` that sends
only `title` leaves `body` alone — without a read first, and therefore
without the window between the read and the write in which someone else's
change disappears.

`set ...$req` is the same thing over every field of a class at once.

Absent and null are different here. `"body": null` sets the column to
null; omitting `body` leaves it. The whole `=?` and spread design rests on
keeping those apart.

### Locking

`update … first` lowers to a locked row selection — `FOR UPDATE LIMIT 1`
inside the statement. Without it two concurrent callers select the same
row and both write it.

## Delete

```jwc no-compile
delete from App.notes.Notes where id == $id
```

If the schema declares `on delete cascade`, children go with it. Walking
the tree by hand is what a schema without foreign keys forces.

## Transactions

```jwc no-compile
service WorkspaceService {
    function create(owner_id: int, req: CreateWorkspaceRequest) {
        transaction {
            let ws = insert into App.org.Workspaces {
                name = $req.name, owner_id = $owner_id
            } as { id, name };

            insert into App.org.Members {
                workspace_id = $ws.id, user_id = $owner_id, role = "owner"
            };

            return ws;
        }
    }
}
```

`BEGIN`, run, `COMMIT` — or `ROLLBACK` if anything leaves by an error.
Returning from inside the block **commits**: a `return` is a normal exit.

The connection is pinned for the block. Without the pin the `BEGIN` lands
on one pooled connection and the statements on whichever others the pool
hands out, so the block commits nothing and rolls back nothing.

A `transaction` belongs to a `service`, not to a route or a middleware
(`E0621`): one spanning a whole request holds a connection for the whole
request, including the parts that do no database work.

## Constraint violations are declared errors

A unique violation on a constraint that carries a message becomes
`Conflict` — a `409` with that message. A check or not-null violation
becomes `BadRequest`. A constraint with no message stays a fault, which is
a 500, because it means something nobody anticipated.

That is what makes the read-then-insert unnecessary: the friendly message
is on the constraint, and the constraint is what actually enforces it.

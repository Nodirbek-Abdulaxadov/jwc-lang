# writes.md — `insert`, `update`, `delete`, and `transaction`

Normative. Closes gaps **#6**, **#7**, **#21**, **#43**, and **G7** /
**G8** from error-model.md.

---

## 1. Shared rules

1.1 A write statement targets exactly one table, always fully qualified.
Writing to a `view` is `E0601`.

1.2 A write is a **statement** and an **expression**. As an expression its
value is its projection (types §5.3); as a statement its value is discarded.
A write with no projection has type `Void` and may not be assigned.

1.3 Column names in the object literal / `set` clause are the *declared*
column names of the target table. A name with no column is `E0602`.

1.4 `private` and `server` columns may be written by an **explicit** entry,
never by a spread (types §9.4).

---

## 2. `insert`

```jwc
insert into App.auth.Accounts {
    ...$req,
    password_hash = $password_hash
} as { id, email, display_name, created_at };
```

2.1 Always exactly one row. Bulk insert is `for (line in $req.lines) { … }`,
which emits one statement per element in the enclosing transaction. A
multi-row form is `DEFERRED-8`.

2.2 `as { }` is the `RETURNING` list; the result is a `Record` (not `T?` —
an insert that returns no row is impossible without `on conflict`).

2.3 `on conflict (cols) do nothing` makes the result `Record?`. This is the
form that fixes the sample's webhook TOCTOU (G8): select-then-insert is a
race, `on conflict do nothing` is not.

```jwc
let payment = insert into App.billing.Payments { ...$req, provider = "stripe" }
    on conflict (provider_ref) do nothing
    as { id };

if ($payment == null) { return { status: "duplicate" }; }
```

2.4 `on conflict (cols) do update set …` is the upsert. `cols` must name a
declared unique constraint or unique index (`E0603`).

2.5 Omitting `on conflict`'s column list is legal only when the table has
exactly one unique constraint (`E0604`).

---

## 3. `update`

```jwc
update App.org.Members
    set role = $req.role
    where org_id == $org_id and account_id == $account_id
    as { org_id, account_id, role }
    first;
```

3.1 With no `first`, the statement updates every matching row; the
projection is `Record[]`.

3.2 With `first`, exactly one row is updated and the projection is
`Record?`.

3.3 `set col =? expr` skips the assignment when `expr` is null. `set ...x`
applies the field-wise rule of types §9.2.

3.4 An `update` with no `where` is `E0605`. There is no accidental
whole-table update. `where true` is the explicit opt-in.

3.5 Empty spread — see types §9.5. The statement is skipped and the
projection reads the current row.

---

## 4. `update … first` and `delete … first` lowering (#43)

`first` on a write has no direct SQL form; it lowers to a locked row
selection:

```sql
UPDATE app_billing.subscriptions t
   SET status = $1, canceled_at = $2
 WHERE t.ctid = (
        SELECT s.ctid FROM app_billing.subscriptions s
         WHERE s.org_id = $3 AND s.status <> 'canceled'
         ORDER BY s.id
         FOR UPDATE
         LIMIT 1)
RETURNING …;
```

- `FOR UPDATE` is **always** emitted. Two concurrent `cancel(org_id)` calls
  serialise; the second sees the already-canceled row and matches nothing.
- `SKIP LOCKED` is not available in 1.0. Work-claiming is `DEFERRED-9`.
- The same determinism rule as `select … first` applies: `orderby` is
  required unless the `where` provably selects at most one row
  (queries §5.2, `E0520`).

---

## 5. `delete` (#6)

```jwc
delete from App.org.Invites
    where id == @invite_id and org_id == @org_id
    as { id }
    first;
```

5.1 `delete` takes a projection. With `first` the result is `Record?`, which
is what makes "404 if it did not exist" writable:

```jwc no-compile
let gone = delete from App.org.Invites where … as { id } first
    or throw NotFound("taklifnoma topilmadi");
```

5.2 Without a projection the result is `Void`. There is no row count in
1.0; the projection is strictly more informative and does not tempt anyone
into `if (n == 0)`.

5.3 A `delete` with no `where` is `E0605`.

---

## 6. `raw` escape hatch

```jwc no-compile
let rows = raw("select … from … where x = {}", $x) as { id, total };
```

6.1 `raw(sql, args…)` is the only way to write SQL by hand. `{}` are
positional bind placeholders; the arguments are bound, never interpolated.
A `{}` count mismatch is `E0610`.

6.2 `raw` is **forbidden inside a `view`** (`E0611`) — a view is a
snapshotted object and a hand-written body cannot be diffed.

6.3 `raw` results are `Raw` unless annotated with `as { }`, in which case the
annotation is trusted and unchecked. This is the one unchecked boundary in
the language and `jwc lint` lists every occurrence.

6.4 `raw` exists so that CTEs, window functions, recursive queries and
full-text search are reachable without the query compiler growing them
(ROADMAP §7). It is a valve, and its usage count is the measurement of which
feature to add next.

---

## 7. `transaction` (G7)

```jwc
transaction {
    let org = insert into App.org.Orgs { ...$req } as { id, slug, name, created_at };
    insert into App.org.Members { org_id = $org.id, account_id = $owner_id, role = MemberRole.owner };
    return $org;
}
```

### 7.1 Semantics

| Event | Outcome |
|---|---|
| block completes, or `return` inside it | **COMMIT**, then the value is returned |
| a `throw` or a fault escapes the block | **ROLLBACK**, then the error propagates |
| a postfix `catch` handles an error inside | `SAVEPOINT` / `ROLLBACK TO` (errors §7) |

`return` inside a `transaction` commits and returns from the **enclosing
function**, not just the block. There is no other reading; the sample
depends on it in three places.

### 7.2 The errorHandler runs *outside* the transaction

The rollback happens first, the connection is released, and only then does
`errorHandler` run — on a fresh connection. This is the fix for G7: an
`errorHandler` arm that logs to `App.audit.Events` would otherwise issue
statements on a connection Postgres has already put in `25P02` and turn
every 404 into a 500.

### 7.3 Nesting is a compile error (E13)

A `transaction` block whose call graph reaches another `transaction` block is
`E0620`, reported at the inner site with the call path. Detecting it
statically is possible because there are no function values (types §1).

### 7.4 Scope

A `transaction` may appear in a `service` function. It may not appear in a
`route`, a `middleware`, an `after` block, or an `errorHandler` arm
(`E0621`) — those are HTTP-shaped, and a transaction spanning middleware
would hold a connection across the whole request.

---

## 8. Constraint violations become errors (#30)

A write that violates a constraint carrying a message becomes a declared
error; one that violates a message-less constraint is a fault. The full
mapping is errors §6, and the mechanism — matching the violated constraint by
its generated name — is schema §8.

Because the raise set of a function includes the constraints of the tables it
writes (errors §3), a route that inserts into `Payments` *knows at compile
time* that it can raise the `provider_ref` conflict, and `errorHandler`
exhaustiveness covers it.

---

## 9. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0601` | write targets a view |
| `E0602` | unknown column in a write |
| `E0603` | `on conflict` columns are not a unique constraint |
| `E0604` | `on conflict` without columns on a multi-unique table |
| `E0605` | `update`/`delete` with no `where` |
| `E0610` | `raw` placeholder/argument count mismatch |
| `E0611` | `raw` inside a view |
| `E0620` | nested transaction |
| `E0621` | `transaction` outside a service |

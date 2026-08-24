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

3.6 A `set` expression that reads the row's **own columns** is emitted as
SQL, not evaluated in the process:

```jwc no-compile
set value = value + 1
```

becomes `SET value = (value + 1)`. Evaluating it here would need a read
first, and two callers doing that both read the same number — the race
§2.3 is about, in the one place where it is easiest to write by accident.
Everything else in a `set` is still a bind parameter.

---

## 4. `update … first` and `delete … first` lowering (#43)

`first` on a write has no direct SQL form; it lowers to a locked row
selection:

```sql
UPDATE billing.subscriptions t
   SET status = $1, canceled_at = $2
 WHERE t.ctid = (
        SELECT s.ctid FROM billing.subscriptions s
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

Each `{}` expands to `($n::text)` — the same "bound as text, cast in SQL"
rule the compiler follows everywhere (queries §7.3). Hand-written SQL
carries no type information to derive the cast from, so the **author**
writes it:

```jwc no-compile
raw("select … where org_id = ({})::bigint", $org_id)
```

Without the cast Postgres infers the column's type for the parameter and
refuses the text, which is an error at the first call rather than a wrong
answer later.

6.2 `raw` is **forbidden inside a `view`** (`E0611`) — a view is a
snapshotted object and a hand-written body cannot be diffed.

6.3 `raw` results are `Raw` unless annotated with `as { }`, in which case the
annotation is trusted and unchecked. This is the one unchecked boundary in
the language and `jwc v1 explain` lists every occurrence with a count
(queries §7.4).

The SQL itself must be a **literal** (`E0610`): a computed string cannot
have its placeholders counted, and counting them is the only thing standing
between this construct and interpolation.

6.4 `raw` exists so that CTEs, window functions, recursive queries and
full-text search are reachable without the query compiler growing them
(ROADMAP §7). It is a valve, and its usage count is the measurement of which
feature to add next.

---

## 7. `buffered`

### 7.1 The form

```jwc no-compile
middleware AccessLog {
    after {
        insert into App.audit.Requests {
            route  = request.route(),
            status = response.status(),
            micros = response.duration_us()
        } buffered;
    }
}
```

`buffered` hands the row to a batch writer and returns. The statement is
the one the query compiler produced for that `insert` — `buffered` changes
who sends it and when, not what is sent.

### 7.2 Rules

| | |
|---|---|
| `as { … }` | `E0614` — it answers before the row exists |
| inside `transaction { }` | `E0612` — the row is written later, on another connection; a rollback would not take it back |
| `on conflict` | `E0613` — a resolution nobody observes is a row silently not written |
| its raise set | **empty**. There is no caller left to raise to; a constraint the row violates is counted, not thrown |

That last row is what makes a logging `after` block possible at all: an
ordinary insert there is `E0811`, because an `after` block runs once the
response is decided and has no handler behind it.

### 7.3 Why it exists

An `after` block is awaited **before** the response is returned
(middleware §5.1). So an ordinary insert there puts a database round trip
in front of every response: every request waits for its own log row.

`builtins.md` §10 listed `log_insert` as "overlapped `insert into` for no
benefit". The benefit is that round trip. 0.9's own measurements put the
batching at 6.0k rows/s with 500-row batches against 20.3k at 5000, with
request throughput unchanged.

### 7.4 What is given up

**Durability.** Rows sit in memory until the next flush, so a crash loses
at most `JWC_LOG_FLUSH_MS` of them, and a sustained overload drops rows
rather than growing without bound. Drops are counted, not silent:
`jwc_log_dropped_total`.

Both are right for telemetry and wrong for anything you would bill on,
which is why this is a word the call site writes and not what `insert`
does by default.

### 7.5 The writer

Rows are grouped by statement and merged into one multi-row `INSERT`,
chunked to Postgres's 65 535-parameter ceiling. One statement per row
would move the latency off the request and leave the database doing the
same work.

| Env var | Default | |
|---|---|---|
| `JWC_LOG_QUEUE` | 10000 | channel capacity; full means rows are dropped |
| `JWC_LOG_BATCH` | 2000 | rows per statement |
| `JWC_LOG_FLUSH_MS` | 200 | longest a row waits, and the crash-loss bound |

`/metrics`: `jwc_log_queue_depth`, `jwc_log_queue_capacity`,
`jwc_log_dropped_total`, `jwc_log_written_total`, `jwc_log_failed_total`,
`jwc_log_batches_total`. Depth against capacity is how the writer falling
behind is visible before it starts dropping.

---

## 8. `transaction` (G7)

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

## 9. Constraint violations become errors (#30)

A write that violates a constraint carrying a message becomes a declared
error; one that violates a message-less constraint is a fault. The full
mapping is errors §6, and the mechanism — matching the violated constraint by
its generated name — is schema §8.

Because the raise set of a function includes the constraints of the tables it
writes (errors §3), a route that inserts into `Payments` *knows at compile
time* that it can raise the `provider_ref` conflict, and `errorHandler`
exhaustiveness covers it.

---

## 10. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0601` | write targets a view |
| `E0614` | `as { … }` on a buffered insert |
| `E0612` | `buffered` inside a `transaction { }` |
| `E0613` | `on conflict` on a buffered insert |
| `E0602` | unknown column in a write |
| `E0603` | `on conflict` columns are not a unique constraint |
| `E0604` | `on conflict` without columns on a multi-unique table |
| `E0605` | `update`/`delete` with no `where` |
| `E0606` | value is not assignable to the column it is written to |
| `E0610` | `raw` placeholder/argument count mismatch |
| `E0611` | `raw` inside a view |
| `E0620` | nested transaction |
| `E0621` | `transaction` outside a service |

---
sidebar_position: 4
description: "Writing rows in JWC: insert, update and delete, atomic update ... set, and how values are bound to their declared column types."
---

# insert / update / delete

## insert

```jwc
let u = new User();
u.name  = "ali";
u.email = "a@b.com";
insert u into AppDb.User;
```

`new <Entity>()` constructs an empty row. Field assignments populate it. The `insert` statement INSERTs every assigned column; auto / default columns are filled by the DB.

Body literal short-cut for HTTP routes:

```jwc
route POST "/users" {
    let req = body();
    let u = new User();
    u.name  = req.name;
    u.email = req.email;
    insert u into AppDb.User;
    return created(u);
}
```

## update

Update an in-memory row by its primary key:

```jwc
let u = first(select User from AppDb.User where User.id == @id);
u.name = "vali";
update u in AppDb.User;
```

Only the **dirty** fields are SET (tracked by assignment). Updating without changing anything is a clear error — guards against accidental no-ops.

Composite PK / ad-hoc tables fall back to a `WHERE id = ...` clause; declare `pk on (...)` to make this explicit.

### Atomic `update CTX.Table set ...`

The whole-row form above does a read, then a write. Under concurrent requests two readers can each see the old value and overwrite each other — a classic lost-update race. For counters and increment patterns, use the **atomic** form instead:

```jwc
update AppDb.Link set hits = hits + 1 where Link.code == @code;
```

This compiles to a single SQL `UPDATE` — no preceding read, no race window. A bare identifier on the RHS that names a column on the entity (`hits`) stays inline in the SQL, so `hits + 1` runs server-side and the increment is genuinely atomic.

Rules:

- `where` is **required** (mirrors `delete from` — a missing predicate would touch every row).
- The SET list must have at least one `col = expr` pair.
- Column names in the SET list and the WHERE clause are validated against the entity at compile time.
- Anything other than a column reference or column arithmetic (calls, literals, `body().field`, …) is evaluated host-side once and bound as a parameter.

## delete

```jwc
let u = first(select User from AppDb.User where User.email == @email);
delete u from AppDb.User;
```

Or **bulk** delete without a binding:

```jwc
delete from AppDb.Session where Session.expires_at < @now;
```

`where` is **required** on bulk delete (no accidental table truncations).

## raw_sql

Escape hatch:

```jwc no-compile
let count_str = raw_sql(
    "SELECT count(*)::text FROM users WHERE created_at > $1",
    json_stringify([@since])
);
// SELECT returns the text-cast result; mutations return affected rows.
```

Use sparingly — the typed `select` / `insert` paths get compile-time column checks; raw SQL doesn't.

---
sidebar_position: 4
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
    let body: CreateUserRequest = body();
    let u = new User();
    u.name  = body.name;
    u.email = body.email;
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

```jwc
let count_str = raw_sql(
    "SELECT count(*)::text FROM users WHERE created_at > $1",
    json_stringify([@since])
);
// SELECT returns the text-cast result; mutations return affected rows.
```

Use sparingly — the typed `select` / `insert` paths get compile-time column checks; raw SQL doesn't.

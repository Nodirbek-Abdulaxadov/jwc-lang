---
sidebar_position: 2
---

# Entities

Entities map to tables.

```jwc
entity User of AppDb {
    id:    int pk auto;        // primary key, auto-increment
    name:  string;
    email: string;
    age:   int?;               // nullable
    created_at: datetime default now;
}
```

`of <dbcontext>` ties the entity to a connection. If omitted, falls back to the single dbcontext in the program (error if there are multiple).

## Field modifiers

| Modifier | Meaning |
|---|---|
| `pk` | Primary key. Composite supported: `pk on (col_a, col_b)` at entity level. |
| `auto` | Auto-increment (Postgres `IDENTITY`). |
| `unique` | UNIQUE constraint at the column level. |
| `default <expr>` | Column default. `now` becomes `NOW()`. |
| `?` (suffix on type) | Nullable. |

## Composite primary key

```jwc
entity Membership of AppDb {
    user_id: uuid;
    role_id: uuid;
    pk on (user_id, role_id);
}
```

## Foreign keys

```jwc
entity Post of AppDb {
    id: int pk auto;
    user_id: uuid references User.id on delete cascade;
    title: string;
}
```

Supported actions: `cascade`, `restrict`, `set null`.

## Navigation properties

Declare in-language relations so projections can fetch them in one query:

```jwc
entity User of AppDb {
    id: uuid pk;
    posts: List<Post> via Post.user_id;       // one-to-many
    profile: Profile via Profile.user_id;     // one-to-one
}
```

Then in a select:

```jwc
let users = select User with posts, profile from AppDb.User;
```

This emits one correlated `json_agg(...)` subquery per nav — no N+1.

## Generated SQL

```bash
jwc gen-sql models/User.jwc     # → CREATE TABLE "users" (...);
```

Use [`jwc migrate new`](./migrations) to capture schema changes as migration files instead of dumping raw DDL.

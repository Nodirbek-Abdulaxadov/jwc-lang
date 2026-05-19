---
title: Database & queries
sidebar_position: 4
---

# Database & queries

JWC supports PostgreSQL today. Connection pool, query cache, migrations,
and transactions are built in.

## Select

```jwc
let xs = select User from AppDb.User
    where (User.age >= @min and User.country == @country)
       or User.is_admin == true
    orderby User.created_at desc
    limit 20 offset 0;

let one = select User from AppDb.User where User.id == @id first;
```

Supported clauses: `where`, `orderby [asc|desc]`, `limit N`, `offset N`,
`first`. `first` forces `LIMIT 1` and returns a single row.

## Operators

`==`, `!=`, `<`, `<=`, `>`, `>=`, `like`, `ilike`, `in (@a, @b, ...)`,
`between @a and @b`, `is null`, `is not null`.

## Projection

```jwc
let safe = select User { id, username, email } from AppDb.User
    where User.id == @id first;
```

Only the named columns hit the database. Columns are validated against
the entity at compile time.

## Aggregations

```jwc
let total  = select count(*)      from AppDb.User where User.active == true;
let avgAge = select avg(User.age) from AppDb.User;
let max_id = select max(User.id)  from AppDb.User;
```

`count(*)`, `sum`, `avg`, `min`, `max`. Results parse to `int` / `float` /
`string` based on the SQL response.

## Insert, update, delete

```jwc
let u = new User();
u.id    = uuid();
u.email = req.email;
insert u into AppDb.User;          // returns the row, populates u

u.email = "new@example.com";
update u in AppDb.User;            // SET only the modified column(s)

delete u from AppDb.User;          // WHERE on the declared PK

delete from AppDb.User where User.active == false;  // bulk, where required
```

- `update` honours **dirty-field tracking** — only fields assigned via
  `var.field = ...` since the variable was loaded land in the `SET` list.
- PK columns are picked from the entity's `pk` markers (composite PKs
  supported). Ad-hoc tables fall back to `id`.

## Transactions

```jwc
transaction {
    insert user into AppDb.User;
    insert profile into AppDb.Profile;
    if (something_bad) {
        return internalError("rolled back");
    }
}
```

- Thread-local pooled connection; all SQL inside routes through it.
- An uncaught error rolls back automatically (RAII guard).
- Nested transactions are rejected.

## Navigation / eager-loaded JOINs

```jwc
entity User of AppDb {
    id uuid pk;
    posts: List<Post> via Post.author_id;
}

let user = select User with posts from AppDb.User
    where User.id == @id first;
```

JWC emits correlated `json_agg(row_to_json(c))` subqueries and embeds the
result directly. `user.posts` is a JSON array string ready to return.

## Raw SQL escape hatch

```jwc
let rows = raw_sql(
    "SELECT json_agg(row_to_json(t))::text FROM users t WHERE created_at > $1",
    "[\"2026-01-01T00:00:00Z\"]"
);
let n_changed = raw_sql("DELETE FROM logs WHERE level = $1", "[\"debug\"]");
```

Second argument is a JSON array of bound parameters — always parameterized.
`SELECT`/`WITH` shape returns text; mutations return affected row count.

## Migrations

```bash
jwc migrate new init
jwc migrate up
jwc migrate down --steps 1
```

- `migrate new` parses your existing `migrations/` history and emits only
  the **diff** (`ALTER TABLE ... ADD COLUMN`, `DROP COLUMN`, etc.) — not
  the full schema each time.
- A session advisory lock (`pg_try_advisory_lock`) serialises concurrent
  `migrate up`/`down` invocations.
- Migrations run inside their own transaction; `_jwc_migrations` records
  the applied history.

## Connection pool tuning

| env | default | notes |
|---|---|---|
| `JWC_DB_POOL_SIZE` | 64 | maximum connections |
| `JWC_DB_MIN_IDLE` | 8 | minimum warm connections kept open |
| `JWC_DB_MAX_LIFETIME_SECS` | 1800 | recycle after this many seconds (`0` disables) |
| `JWC_DB_IDLE_TIMEOUT_SECS` | 600 | close idle conns after this (`0` disables) |
| `JWC_DB_CONNECTION_TIMEOUT_SECS` | 5 | abort waiting after this |
| `JWC_QUERY_CACHE_TTL_SECS` | unset | enable result-cache TTL when `> 0` |
| `JWC_DB_TLS` | unset | set to `1` / `true` for TLS (`native-tls`) |
| `JWC_DB_TLS_INSECURE_SKIP_VERIFY` | unset | dev only — accept self-signed |

Both the runtime pool and the `jwc migrate up` / `down` CLI honour these
flags, so a single env setting covers app traffic and schema migrations.

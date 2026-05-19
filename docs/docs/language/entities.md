---
title: Entities, classes, types
sidebar_position: 2
---

# Entities, classes & types

## Entities

```jwc
dbcontext AppDb : Postgres;

entity User of AppDb {
    id uuid pk;
    email varchar(120);
    age int(0, 150);
    created_at datetime;
    deleted_at datetime nullable;
}
```

- `pk` marks the primary key — `update` / `delete var in/from CTX.Table`
  uses these columns for `WHERE` (composite PKs supported).
- `nullable` allows NULL.
- `int(min, max)` emits a Postgres CHECK constraint.
- Field type validation runs at compile time (`Unknown type 'weirdtype'`).

### Foreign keys

```jwc
entity Post of AppDb {
    id uuid pk;
    author_id uuid references User.id on delete cascade;
    title varchar(200);
}
```

Supported `on delete` actions: `cascade`, `restrict`, `set null`. The
validator confirms the target entity + column exist.

### Navigation properties

```jwc
entity User of AppDb {
    id uuid pk;
    posts:   List<Post> via Post.author_id;       // one-to-many
    profile: Profile    via Profile.user_id;      // one-to-one
}
```

Use them in queries with `select ... with ...`:

```jwc
let user = select User with posts, profile from AppDb.User
    where User.id == @id first;
```

JWC emits correlated `json_agg(...)` subqueries and embeds the nested
shapes directly in the response JSON.

## Classes (DTOs / view models)

```jwc
class RegisterReq {
    username string;
    email    string;
    password string;
}

function register(req: RegisterReq) {
    // req.username / req.email / req.password are checked at compile time
}
```

Classes don't generate tables, but JSON validation runs on:

- Typed function params (`req: RegisterReq`)
- Typed return types (`function getUser(...): User?`)
- Body parsing — `body()` decoded via the param's declared type

## Type system

| Type | Notes |
|------|-------|
| `string`, `int`, `bigint`, `double`, `decimal`, `bool` | primitives |
| `uuid` | RFC 4122 hex form (`8-4-4-4-12`) |
| `datetime` | ISO 8601 (`YYYY-MM-DDTHH:MM:SS[.ms]Z`) |
| `json` | any valid JSON |
| `T?` / `Optional<T>` | nullable — `null` accepted |
| `List<T>` | JSON array, each element checked against `T` |

## Dome (static-class style)

```jwc
dome BrandService {
    function getAll() {
        return select Brand from AppDb.Brand;
    }
}

route GET "brands" {
    return json(BrandService.getAll());
}
```

Functions inside `dome` are only reachable via `Dome.fn(...)`.

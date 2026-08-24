---
sidebar_position: 2
title: Types
description: "Scalars, optionality, Raw versus Record, and why bigint is a string on the wire."
---

# Types

## Scalars

| JWC | Postgres | On the wire |
|---|---|---|
| `smallint`, `int` | `smallint`, `integer` | number |
| `bigint` | `bigint` | **string** |
| `numeric` | `numeric` | **string** |
| `boolean` | `boolean` | boolean |
| `varchar(n)`, `text` | same | string |
| `timestamptz` | `timestamptz` | RFC 3339, UTC |
| `date`, `time`, `interval` | same | string |
| `uuid`, `inet`, `jsonb` | same | string / JSON |
| `T[]` | `T[]` | array |

**`bigint` and `numeric` are strings in JSON, and that is deliberate.**
JavaScript loses integer precision above 2^53, and a float has no business
holding money. If a client reads an id as a number, declare the column
`int` — that is what a 0.9.x `int pk autoincrement` was, and keeping it
keeps the wire format.

## Optionality

`T?` is the only nullable form. A `T` is never null.

```jwc no-compile
let org = select O from App.org.Orgs
    where id == $id
    as { id, name }
    first or throw NotFound("topilmadi");
```

`first` answers `T?`, and reading a field of a `T?` is `E0320`. The
`or throw` is what narrows it — a route that answers `200 null` almost
always meant 404.

`??` supplies a default; `x?.y` does not exist, because the narrowing above
is the intended shape.

## `Raw` and `Record`

A query with **no** `as { … }` projection returns `Raw` — a JSON fragment
Postgres built, forwarded to the response with zero parsing. Reading a field
of it is a compile error.

```jwc no-compile
-- Raw: fast, forwarded whole, opaque
return json(select O from App.org.Orgs where id == $id first);

-- Record: named fields, readable, checked
let org = select O from App.org.Orgs
    where id == $id
    as { id, name, created_at }
    first or throw NotFound("topilmadi");
return json({ id: $org.id, label: $org.name });
```

`as { … }` is the only way to get a `Record`. It is also the response
boundary: a `private` column may not appear in one, so a password hash
cannot leave the process by accident.

`jwc explain` prints every place a `Raw` is lost to a projection, so the
cost is visible.

## Classes are the request boundary

```jwc no-compile
class OrgCreate {
    slug varchar(40) required, pattern(r"^[a-z0-9-]{3,40}$");
    name varchar(120) required, minLength(2);
    note varchar(500)?;
}
```

`request.body() as OrgCreate` parses, validates and **whitelists**: a key
the class does not declare is dropped, so a caller cannot set a column by
naming it in the body. Every failure is reported at once, as a 400 with a
`fields` array.

A class is input only. It never describes a response — that is `as { … }`.

## Coercions

`int(x)`, `bigint(x)`, `numeric(x)`, `boolean(x)`, `uuid(x)`,
`timestamptz(x)`, `enum(E, x)`. Each raises on a value it cannot read; the
failure class depends on where the value came from, which the compiler knows
statically.

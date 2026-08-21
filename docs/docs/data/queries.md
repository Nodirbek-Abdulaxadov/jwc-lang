---
sidebar_position: 2
title: "Queries — select, joins, projections"
description: "Queries are part of the language, not strings. Joins declare their shape, projections are the response boundary, and pagination is keyset."
---

# Queries

A query is a language construct, checked against the schema at compile
time and lowered to one SQL statement. There is no query builder, no
string interpolation and no `N+1`, because a nested collection is a join
rather than a loop.

```jwc no-compile
select <binder> from <qualified-source>
    { <join> }
    [ where <expr> ]
    [ group by <cols> ]
    [ having <expr> ]
    [ as { <projection> } ]
    [ orderby <keys> ]
    [ page … | limit <expr> ]
    [ first ]
```

The clause order is fixed. Writing them out of order is `E0501`, which
names the expected position, and `jwc fmt` normalises to it.

## The binder, and why it exists

```jwc no-compile
select N from App.notes.Notes where org_id == $org_id
```

`N` binds the row. A bare name in a clause is a **column**; `$name` is a
local. That is the whole of the rule that makes

```jwc no-compile
where org_id == $org_id
```

mean what it looks like. In SQL, `WHERE org_id = org_id` is a tautology
that matches every row; here the two sides cannot be confused, because
they are spelled differently.

## Projections are the response boundary

```jwc no-compile
as { id, title, created_at }
```

A query **with** `as { }` produces a record: the runtime parses it so
fields can be read, and rebuilds it in projection order, because that
order is what the response promises.

A query **without** `as { }` produces `Raw` — the text Postgres produced,
forwarded to the response without ever being parsed. That is the
performance promise, and it is also why a `private` column cannot leak:
a projection is the only way to name columns, and `private` columns
cannot be named.

## Joins say what they produce

```jwc no-compile
select O from App.org.Orgs
    left join App.org.Members M on M.org_id == O.id as many members orderby joined_at asc limit 200
    left join App.auth.Accounts A on A.id == M.account_id as one account under members
    as { id, name, members: { role, account: { id, email } } }
```

Three results, and every join picks one:

| | Meaning |
|---|---|
| `as one <name>` | at most one row, nested as an object. No match is **null**, not an object of nulls. |
| `as many <name>` | a collection, nested as an array. Takes its own `orderby` and `limit`. |
| `as group` | contributes to filtering and aggregates, and nothing to the shape. |

A join with no `as` clause is `E0535`. Only `left` and `inner` exist:
`right`, `full` and `cross` invert the projection tree, and swapping the
sides gives the same rows with a shape a reader can follow.

`under` names the parent when the `on` clause is ambiguous about it.

## Aggregates

```jwc no-compile
select I from App.billing.Invoices
    group by org_id
    as { org_id, invoice_count: count(id), total: sum(amount) }
```

Every non-aggregate field must be in the `group by` — aliasing one does
not group it. `count(x)` counts non-null `x`; `count.distinct(x)` is the
form to reach for under two bare joins, where fan-out would otherwise
count the other join's rows too. `count(x where pred)` lowers to
`count(x) FILTER (WHERE pred)`.

A **whole-table aggregate** — a projection that is nothing but
aggregates, with no `group by` — always produces exactly one row, so
`first` on it needs no `orderby`.

## Cardinality

`first` takes one row. The checker requires the query to be
**deterministic** about which one: either an `orderby`, or a `where` on a
unique constraint. A `first` with neither returns whatever the planner
happened to produce, and that answer changes under load.

## Pagination is keyset

```jwc no-compile
select N from App.notes.Notes
    where org_id == $org_id
    as { id, title, created_at }
    orderby created_at asc, id asc
    page after $cursor size $size
```

The answer is `{ items, next, has_more }`. Follow `next` until it is
null; with no next page there is no cursor, so a caller that loops on it
cannot loop forever.

`offset` does not exist. It re-scans every row before the page, and it
drifts: a row inserted while a client is scrolling shifts everything
after it, so the client sees a row twice or not at all.

The cursor is HMAC-signed with `server { cursor_secret }`. It is a
predicate the client hands back; unsigned, it is a second filter nobody
checked. A cursor that does not verify is a `400` — it is client input,
and the only honest answer is that it is not a cursor this server issued.

## Views

A `view` is a named query, and a query can select from it exactly as it
selects from a table.

```jwc no-compile
view OrgList of App.org {
    select O from App.org.Orgs
        left join App.org.Members M on M.org_id == O.id
            as many members orderby account_id asc limit 200
        as { id, slug, name, members: { account_id, role } }
}
```

## The escape hatch

`raw(sql, params)` runs SQL the language cannot express. It is deliberate,
it is greppable, and its result is `Raw` — the runtime does not pretend to
know the shape.

```jwc no-compile
raw("SELECT count(*) FROM notes.notes WHERE org_id = {}", [$org_id])
```

## Seeing the SQL

```bash
JWC_LOG_SQL=1 jwc serve .        # every statement, with its binds and timing
jwc explain .                    # the statement each query lowers to
```

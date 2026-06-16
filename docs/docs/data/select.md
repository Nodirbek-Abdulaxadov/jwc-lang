---
sidebar_position: 3
---

# select

`select` is a language construct, not a string. The compiler validates column names, paramref types, and the entity it selects from.

## Basic

```jwc
let users = select User from AppDb.User;
```

Returns a JSON array of objects. Empty list when no rows.

## Single row

Two equivalent shapes — pick the one that reads better at the call
site.

**`first(...)` — function form:**

```jwc
let me = first(select User from AppDb.User where User.id == @uid);
```

`first(...)` collapses a list to its first element (or `null`).
Forgetting it is so common that lint W004 flags single-row PK lookups
that lack it.

**`select ... first` — trailing keyword:**

```jwc
let me = select User from AppDb.User where User.id == @uid first;
```

The trailing `first` keyword is parsed as part of the select expression
itself — same result as `first(...)`, no outer call needed.
Particularly nice in middleware / route bodies where the `select` lives
inside a longer expression chain. Either form is fine; the project's
existing style usually wins.

## where

```jwc
select User from AppDb.User where User.email == @email;
select User from AppDb.User where User.age > @min and User.email like @pattern;
select User from AppDb.User where User.id in (@a, @b, @c);
select User from AppDb.User where User.age between @low and @high;
select User from AppDb.User where User.deleted_at is null;
```

Supported operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `like`, `ilike`, `in (...)`, `between ... and ...`, `is null`, `is not null`. Combine with `and` / `or` and parentheses; `and` binds tighter than `or`.

## order by / limit / offset

```jwc
select User from AppDb.User
    orderby User.created_at desc
    limit 20
    offset @skip;
```

## Projection

Subset of columns:

```jwc
let names = select User { id, name } from AppDb.User;
```

The compiler verifies `id` and `name` exist on `User`.

## Aggregates

```jwc
let total      = select count(*) from AppDb.User;
let max_age    = select max(User.age) from AppDb.User;
let avg_score  = select avg(Post.score) from AppDb.Post where Post.published == true;
```

## group by / having — not usable yet

`group by` and `having` are accepted by the parser, but the feature is **not
functional end-to-end**:

- A whole-entity `select Post ... group by Post.user_id` emits `SELECT t.* ...
  GROUP BY ...`, which Postgres rejects (`column "t.id" must appear in the
  GROUP BY clause`).
- `having` only parses a plain column comparison — an aggregate such as
  `having count(*) > 10` is a parse error.
- Arbitrary-shape aggregate projection (`select { user_id, total: count(*) }`)
  is not implemented; the projection list accepts plain column names only.

For grouped aggregation today, drop to [`raw_sql`](../reference/builtins.md)
(wrap the rows in `json_agg(...)::text` and `json_parse` the result). Scalar
aggregates over the whole query — `select count(*) from ...` etc. (see above) —
do work.

## with — eager nav loading

```jwc
let users = select User with posts, profile from AppDb.User;
```

Each nav becomes a `json_agg(...)` subquery. Validated against the entity's declared navs.

## Parameters

`@name` references a bound variable from the surrounding scope:

```jwc
function findByEmail(email: string): User? {
    return first(select User from AppDb.User where User.email == @email);
}
```

Bindings are real parameter values, not string-interpolated SQL — there's no injection vector.

## See also: atomic `update ... set`

`select` returns rows; the most common follow-up is to mutate one. If
you're tempted to write the classic "read, change, write back":

```jwc
let link = first(select Link from AppDb.Link where Link.code == @code);
link.hits = link.hits + 1;
update link in AppDb.Link;          // lost-update under concurrency!
```

…use the **atomic** form instead, documented in
[insert / update / delete](./mutations.md#atomic-update-ctxtable-set-):

```jwc
update AppDb.Link set hits = hits + 1 where Link.code == @code;
```

A single round-trip, no read-modify-write race. Two concurrent requests
will each see their increment land. The general rule: when the new
value of a column is a pure function of the old one (counter, version
bump, status transition), reach for `update ... set ...`; reserve
whole-row `update u in ...` for cases where the new value comes from
user input that you've already read end-to-end.

## Joins, projection, aggregation (coming in Phase 11)

The current `select` surface is single-entity. Cross-entity joins,
arbitrary-shape projection (`select { id, title, author.email }`), and
scalar aggregation (`count`, `sum`, `avg`, `min`, `max`) land in **Phase
11 — Query Layer** before v1.0. Joinsiz the "no ORM pain" promise is
half-finished; once Phase 11 ships, raw_sql fallback is no longer the
default escape hatch for cross-table reads.

Until then: use `select` per entity and assemble the response shape in
JWC code, or drop to raw SQL via the engine helpers if the join is
unavoidable.

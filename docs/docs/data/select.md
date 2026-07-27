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

```jwc no-compile
select User from AppDb.User where User.email == @email;
select User from AppDb.User where User.age > @min and User.email like @pattern;
select User from AppDb.User where User.id in (@a, @b, @c);
select User from AppDb.User where User.age between @low and @high;
select User from AppDb.User where User.deleted_at is null;
```

Supported operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `like`, `ilike`, `in (...)`, `between ... and ...`, `is null`, `is not null`. Combine with `and` / `or` and parentheses; `and` binds tighter than `or`.

### Optional predicate (`op?`)

Suffix any comparison operator with `?` to make the term **conditional**: if
the bound value is `null` or an empty string at runtime, the predicate is
dropped (no filter). One static query then serves every filter combination —
no in-code branching:

```jwc no-compile
// @status / @priority empty -> that filter is skipped, all rows match
select Task from AppDb.Task
    where Task.projectId == @projectId
      and Task.status ==? @status
      and Task.priority ==? @priority
    orderby Task.position asc;
```

### Dynamic in-list

`in (@arr)` with a single runtime array binds the whole array and emits
`= ANY($1)` — the list length is dynamic, decided at call time:

```jwc
delete from AppDb.Column where Column.boardId in (@boardIds);  // boardIds: array
```

A fixed `in (@a, @b, @c)` keeps the literal `IN ($1, $2, $3)` shape.

## order by / limit / offset

```jwc no-compile
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

## group by / having

Grouped aggregation is a first-class projection. The select list mixes plain
group-key columns with **aliased aggregate terms** (`alias: count(*)` etc.) —
the alias becomes the JSON key in each result row:

```jwc
let by_status = select Task { status, total: count(*) }
    from AppDb.Task
    where Task.projectId == @projectId
    group by status
    orderby status asc;
// -> [ { "status": "todo", "total": 3 }, { "status": "done", "total": 7 } ]
```

- Aggregate functions in the projection: `count(*)`, `sum(col)`, `avg(col)`,
  `min(col)`, `max(col)`.
- `group by col [, ...]` — group keys (also `group by Entity.col` under a join).
- `having <cond>` — post-aggregation filter.

A grouped `select` **must** use the aliased projection form — a bare
`select Task ... group by ...` (no projection) would emit `SELECT t.*` which
Postgres rejects under `GROUP BY`. Name the columns you group by.

### having

`having` filters **after** grouping, so it can name three things: a `group by`
key, an aggregate call, or an aggregate alias from the projection.

```jwc
let busy = select Task { status, total: count(*), effort: sum(hours) }
    from AppDb.Task
    group by status
    having count(*) > 2 and sum(Task.hours) >= 40;
```

Referring to the alias works too, and reads better when the projection
already names the aggregate:

```jwc
let busy = select Task { status, total: count(*) }
    from AppDb.Task
    group by status
    having total > 2;
```

Anything else is a compile error (`E010`). A plain column that is neither a
group key nor an aggregate can't be evaluated at that point — Postgres would
reject it with *"column must appear in the GROUP BY clause or be used in an
aggregate function"* — so `jwc check` catches it instead of the database.

`having` without a `group by` is also an error (`E009`).

Scalar aggregates over the whole query (no `group by`) keep the function form:

```jwc
let total   = select count(*) from AppDb.User;
let max_age = select max(User.age) from AppDb.User;
```

## with — eager nav loading

`with` eager-loads declared navigation properties (see
[entities](./entities.md#navigation-properties)) as nested JSON, in a single
query — no N+1, no manual assembly:

```jwc
let users = select User with posts, profile from AppDb.User;
```

Each nav becomes a correlated `json_agg(...)` / `row_to_json(...)` subquery,
validated against the entity's declared navs. All nav kinds are supported:

- **has-many** (`posts: List<Post> via Post.userId`) → array.
- **belongs-to / has-one** (`author: User via authorId`) → nested object.
- **many-to-many** (`labels: List<Label> via TaskLabel(taskId, labelId)`) →
  array, joined through the link table.
- **nav projection** hides columns (`assignees: List<User> { id, name }` never
  leaks `passwordHash`).
- **nav ordering** comes from the declaration (`... via Post.userId orderby
  createdAt desc`).

**Two-level nesting** with a dotted nav — load an aggregate root, its children,
and their grandchildren in one statement:

```jwc
let project = select Project with boards.columns
    from AppDb.Project where Project.id == @id first;
// -> { id, name, boards: [ { id, name, columns: [ { id, name }, ... ] } ] }
```

## join — explicit cross-entity queries

Equi-join another entity in the same dbcontext. Columns are qualified by
entity name; the projection can surface a joined column under an alias, and
aggregation can group across the join:

```jwc
let by_column = select Task { columnId, columnName: Column.name, total: count(*) }
    from AppDb.Task
    join Column on Column.id == Task.columnId
    where Task.projectId == @projectId
    group by Task.columnId, Column.name
    orderby Task.columnId asc;
```

`join Entity on a == b` chains for multiple joins. This is what replaces the
`raw_sql` escape hatch for cross-table aggregation — a task-tracker dogfood
brought its stats endpoints to **0 raw_sql** this way. Only inner equi-joins
are supported today (`LEFT`/outer and non-equi `on` are post-1.0).

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

## Query Layer status

The full Query Layer (joins, eager-load, grouped aggregation, projection,
optional/dynamic filters) shipped across v0.5.0–v0.6.1 — **Phase 11 is done**.
`raw_sql` is no longer the default escape hatch for cross-table reads.

Native AOT (`jwc build --native`) mirrors this query surface: nav eager-load,
grouped aggregation, joins, and `==?` all codegen the same SQL. Two query
forms remain interpreter-only on the native path: a dynamic in-list (`= ANY`)
and a `where` on a *joined* entity's column. They run fine under
`jwc run` / `jwc serve`.

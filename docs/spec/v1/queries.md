# queries.md — `select`, joins, projections, aggregates, pagination, views

Normative. Closes gaps **#1**, **#3**, **#4**, **#5**, **#8**, **#11**,
**#18**, **#24**, **#29**, **#40**, **#43**, **#44**, and **N6**, **N12**.

Clauses marked **[0.25]** are specified here but implemented in the query
compiler release; nothing else may assume a different meaning in the
meantime.

---

## 1. Shape

```
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

Clause order is fixed and is the order above. Writing them out of order is
`E0501`, which names the expected position. A fixed order is what lets a
reader scan the same shape in every query, and it is what `jwc fmt`
normalises to.

---

## 2. Sources and bindings

2.1 The source is always fully qualified: `App.billing.Invoices`. Both
tables and views are sources; they are the same kind of thing to a query.

2.2 The binder is mandatory (names §5.4). `select I from App.billing.Invoices`
binds `I`.

2.3 Referring to a source that is not a `table` or `view` is `E0502`.

---

## 3. `where`

3.1 The predicate is an expression over columns of the bindings in scope,
`$locals`, `@path_params`, and literals (names §5.3).

3.2 `==?` — the **optional predicate**. `where status ==? $status` emits the
comparison only when `$status` is non-null; when null the predicate is
dropped entirely (not `IS NULL`). The operand must be `T?` (`E0503`
otherwise, since a non-null operand makes it a plain `==`).

3.3 `in (…)` accepts a literal list or a single array-typed operand:

```jwc no-compile
where status in (InvoiceStatus.open, InvoiceStatus.paid)
where status in ($statuses)                 -- $statuses : InvoiceStatus[]
```

The array form lowers to `= ANY($n)`, one bind parameter, never string
interpolation. `?status=open,paid` becomes an array by
`string.split_csv(...)` in code (builtins §4).

3.4 `like` / `ilike` take a pattern operand. The operand is **always** a
bind parameter; there is no way to concatenate a user string into a pattern
position in emitted SQL.

3.5 `exists (select …)` / `not exists (select …)` **[0.25]** — the way a
parent is filtered by its children (#8). The inner select's `where` may
reference the outer bindings.

---

## 4. Joins

### 4.1 Kinds

Only `left` and `inner`. `right`, `full` and `cross` are not grammar: they
make the *driving* binding nullable or multiply it, which inverts the
projection tree (types §6.3). Swap the sides and use `left`.

### 4.2 Alias (#1)

```
left join App.auth.Accounts inviter on inviter.id == Invites.invited_by as one inviter
```

The optional identifier after the joined table is the binding name; it
defaults to the table's declared name. A repeated binding name is `E0212`
(names §5.4), which is what makes self-joins expressible.

### 4.3 What a join produces — `as one` / `as many` / `as group`

```
left join App.org.Members M   on M.org_id == O.id  as many members orderby joined_at asc
left join App.auth.Accounts A on A.id == M.account_id as one account
left join App.billing.Invoices I on I.org_id == O.id as group
```

- `as one x` — at most one related row. `x : Record?` under `left`,
  `Record` under `inner` (types §6.3).
- `as many x` — `x : Record[]`, empty array when there are none. Never
  null. An `orderby` is **required** (§4.6).
- `as group` — the join contributes to filtering and to aggregates and
  produces no field (§6.2).

**Every join says which.** A join with no `as` clause is `E0535`. It used to
mean the third mode by omission, which made "I meant to aggregate" and "I
forgot the projection" the same syntax; a keyword costs one word and tells
the reader — and the planner — which one was meant.

### 4.4 The attachment tree is declared, not inferred (N12)

A query is a **tree**, not a list: `OrgWithMembers` joins members to the org
and accounts to the members, and the projection nests the same way.

A join's `as one` / `as many` result attaches to the binding its `on` clause
references **other than the binding being joined**. If the `on` clause
references more than one such binding, the attachment is ambiguous and is
`E0510`, naming the candidates. The fix is to write the parent explicitly:

```
left join App.auth.Accounts A on A.id == M.account_id as one account under members
```

`under` names either the binding alias (`M`) or the field its join produces
(`members`) — the two are names for the same node, and a reader thinks in
whichever the surrounding code uses. An alias wins on a collision. Naming
neither is `E0511`.

`under` is required only when `E0510` fires; the sample's `OrgWithMembers`
does not need it, and now says so by construction rather than by accident of
clause order.

### 4.5 `as one` with no match (#3)

`a : Record?` is **null**, not a record of nulls. `$row.a.name` without
narrowing is `E0320` (types §6.4). In JSON, an unmatched `as one` field
serialises as `null`.

### 4.6 Ordering and limiting inside `as many` (#5)

```
left join App.billing.Payments Y on … as many payments orderby created_at desc limit 20
```

The `orderby`/`limit` bind to the child collection, not the outer query — and
so does their **name resolution**: inside a join result's own `orderby` and
`limit`, an unqualified name is a column of that joined table. `as many lines
orderby id asc` orders the lines by *their* id.

**`orderby` is required on `as many`** (`E0536`). This is the same rule
`first` has (§5.2) applied to a collection: without a stated order the
elements come back in whatever order the plan produced, and that changes with
the data, the statistics and the Postgres version. `limit` stays optional;
an unbounded collection with no natural bound is `W0501`.

### 4.7 Filtering a collection — `where` on the join (#8)

```
left join App.org.Members M on M.org_id == O.id where M.role == MemberRole.admin
    as many admins orderby joined_at asc
```

A `where` written **on the join clause** filters that collection. It is not
the query's `where`: it never removes a driving row, so an org with no admins
comes back with `admins: []` rather than disappearing.

The query's own `where` filters driving rows, as always. The two read
differently because they are different questions — "orgs that have an admin"
is `where exists (…)` (§3.5), not this.

---

## 5. `first`, ordering, determinism (#43)

5.1 `first` makes the result `T?` and appends `LIMIT 1`.

5.2 **`first` requires a deterministic result.** `first` is `E0520` unless
either:

- the query carries an `orderby`; **or**
- the compiler proves the `where` clause selects at most one row — i.e. it
  constrains every column of a declared primary key or unique constraint by
  equality, and (for a partial unique index) the predicate is implied.

A partial unique index counts when the query's `where` **implies** its
predicate. `where org_id == $org_id and status != SubscriptionStatus.canceled
first` is covered by `unique (org_id) where status != SubscriptionStatus.canceled`;
`where org_id == $org_id first` alone is not. Implication is checked
syntactically on the canonical predicate form (schema §4.3) — conjunct
containment, not a solver.

5.2.1 **Views inherit uniqueness.** A view projects a column of its driving
table under its own name; that column keeps the driving table's primary-key
and unique constraints for the purposes of this rule. `select V from
App.org.OrgWithMembers where id == $org_id first` is therefore accepted:
`id` is `Orgs.id`, a primary key. A column projected under a *different*
name (`org_id: id`) keeps them too, tracked through the alias; a column
projected from an expression does not.

5.3 `orderby` keys are column references, projection aliases, or expressions
over them. `nulls first` / `nulls last` are accepted and emitted verbatim;
Postgres's default (`nulls last` for `asc`) applies otherwise.

5.4 `orderby` on a nested `as one` field (`orderby org.name asc`) is legal
**[0.25]** and lowers to a join order key. `orderby` on an `as many` field is
`E0521` — there is no single value to sort by.

---

## 6. Projections and aggregates

### 6.1 `as { }` is the SELECT list

```jwc no-compile
as {
    id,
    slug,
    org_id: id,                       -- alias: expression
    plan: { id, code, name },         -- nested, from an `as one` binding
    lines: { id, description }        -- nested, from an `as many` binding
}
```

- a bare `ident` projects that column under its own name;
- `alias: expr` projects an expression;
- `alias: { … }` projects a nested shape and requires `alias` to be a join
  result name from §4.3.

**A bare name in a projection is a column of the driving binding.** Not of
"whichever binding has it": a joined table reaches the projection through its
own nested shape, so resolving across every binding would make `id`
ambiguous in every joined query. To project a joined table's column at the
top level, qualify it — `owner_id: M.account_id`. The same rule applies to a
bare name on the right of `alias:`, which is why `org_id: id` means the
driving table's `id`.

A projection field naming a `private` column is `E0410` (schema §3.1).

### 6.2 Bare joins and aggregates (#4)

A join with no `as` result exists to be aggregated:

```jwc
select O from App.org.Orgs
    left join App.billing.Invoices on Invoices.org_id == O.id
    group by O.id
    as {
        org_id: id,
        invoice_count: count(Invoices.id),
        paid_cents:    sum(Invoices.amount_cents where Invoices.status == InvoiceStatus.paid)
    }
```

Rules:

- Aggregates are legal **only** inside a projection of a query that has a
  `group by`, or that has exactly one binding and no non-aggregate
  projection fields. Elsewhere: `E0530`.
- Every non-aggregate projection field must appear in `group by`
  (`E0531`) — the same rule Postgres has, checked earlier and reported
  against the source line.
- Combining a bare join (aggregation) with an `as many` result in the same
  query is `E0532`, with the two-query rewrite printed. This is the half of
  #4 that ROADMAP §7 keeps as an error deliberately: whether the `as many`
  lateral survives grouping is a real design question and a silently
  multiplied `count` is not an acceptable answer to it.
- Fan-out over multiple bare joins duplicates rows. `count(distinct x)` is
  spelled `count.distinct(x)` and is available; a `count(x)` under two bare
  joins is `W0502` pointing at it.

### 6.3 The aggregate filter

`count(x where pred)` lowers to `count(x) FILTER (WHERE pred)`. The
`where` inside a call is grammar (`call_args`) and is legal only in an
aggregate call (`E0533`).

`FILTER`, not `count(CASE WHEN pred THEN x END)`: the two compute the same
number, but one says what was meant and the planner reads it as a filter.

`count(x)` → `int`, never null. `min`/`max` → `T?`. `sum` widens and `avg`
is `numeric?` — see types §6.3, and note that a summed column changes from
a JSON number to a JSON string, because `bigint` and `numeric` both are.

`having` compares an aggregate, so the literal on the other side is cast to
what the **aggregate** returns, not to the column's type:
`HAVING count(t1.id) > ($1::text)::bigint`.

### 6.4 What an aggregate emits **[0.25.c]**

```sql
count(t1.id)
count(DISTINCT t1.id)
count(t1.id) FILTER (WHERE t1.status = ($1::text)::billing.invoice_status)
(sum(t1.amount) FILTER (WHERE …))::text
```

The parentheses around a filtered aggregate before a cast are load-bearing:
`sum(x) FILTER (WHERE p)::text` casts `p`, not the sum.

A bare (`as group`) join is emitted into `FROM` like any other join and
contributes no projection field. `group by` and `having` follow the outer
`where`, in that order.

---

## 7. Result representation

### 7.1 Raw by default

A query with no `as { }` produces `Raw` (types §5.3): the row is serialised
by Postgres and forwarded to the response without being parsed.

### 7.2 What "raw" actually emits

Raw is a **compiled projection**, not `row_to_json(t)`:

- `bigint` and `numeric` columns are cast to text so the wire form matches
  the record path (types §2.3);
- `private` columns are excluded (schema §3.1);
- physical `as "name"` overrides are applied.

So the emitted form is

```sql
SELECT json_build_object('id', o.id::text, 'slug', o.slug, …) FROM …
```

not `row_to_json`. The fast path is "one JSON value comes back from
Postgres and is never parsed by the application" — it was never "whatever
`row_to_json` happens to do".

`json`, not `jsonb`: `jsonb` normalises an object by sorting its keys, and
the projection order **is** the JSON key order (§6.1). A response whose
fields come back alphabetised is not the shape the author wrote.

### 7.3 How a join lowers **[0.25.b]**

The emitted SQL for each join mode is fixed, because the obvious alternative
is wrong in a way that returns data rather than an error.

**`as one` under `left join`** is a plain `LEFT JOIN` and a guard on the
child's primary key:

```sql
CASE WHEN t1.id IS NULL THEN NULL ELSE json_build_object('id', t1.id::text, …) END
```

Without the guard an unmatched row projects `{"id": null, "name": null}` —
an object of nulls where §4.5 says null. The guard is the primary key
because it is the one column that is NOT NULL when the row exists and NULL
exactly when the join found nothing. Under `inner join` the guard is
omitted: the join cannot miss, and a comparison per row that can only ever
be false is not free.

**`as many`** is a `LEFT JOIN LATERAL` whose subquery carries the
collection's own `where`, `orderby` and `limit`, aggregated outside them:

```sql
LEFT JOIN LATERAL (
    SELECT coalesce(json_agg(c.j), '[]'::json) AS data
      FROM (SELECT json_build_object(…) AS j
      FROM billing.payments t2
      WHERE t2.invoice_id = t0.id
      ORDER BY t2.created_at DESC
      LIMIT ($1::text)::int) c
) t2_agg ON true
```

Not `json_agg(… ORDER BY …)` over a plain join. That form can order a
collection but cannot bound one — `LIMIT` at that level bounds the *page* —
and two collections projected side by side multiply each other's rows, so
three notes and two tags report six of each. Two laterals are independent.

The `coalesce` appears twice for two different nulls: `json_agg` over no
rows is NULL, and the lateral itself contributes NULL when nothing matched.
A collection is `[]` in both cases (types §6.2).

**`as group`** contributes to `FROM` and `WHERE` and projects nothing
(§6.2).

Aliases are `t0`, `t1`, … assigned in tree order, never derived from the
binding name: a binding may be called `user` or `order`, and a self-join
gives two bindings the same table. Parameters are numbered in emission
order and bound as text with the cast in the SQL — `($1::text)::bigint`,
never `$1::bigint`, which makes Postgres infer `bigint` for the parameter
and then refuse the text the runtime sends.

`tests/sql_golden/` holds the reviewed SQL for every query in the sample and
in six focused cases; it is the artefact this section is read against.

### 7.4 `jwc explain`

`jwc explain [--sql] [--raw]` prints, per query in the program: the source
location, the emitted SQL with bind placeholders, and `raw preserved` or
`raw lost here: <construct>`. `JWC_LOG_SQL=1` logs the same SQL at runtime
with timings. Together these are the answer to #29; the dev-only
`/__jwc/queries` endpoint is `DEFERRED-7`.

---

## 8. Views

### 8.1 A view is a named projection

```jwc
view MemberAccess of App.org {
    select M from App.org.Members
        left join App.org.Orgs on Orgs.id == M.org_id as one org
        as { org_id, account_id, role, org: { id, slug, name } }
}
```

The body **must** carry `as { }` (`E0540`). That is what makes a view a
`Record` source (types §5.3) and closes #17's contradiction: the sample's
auth gate reads `access.role` off a view, which is legal because a view is a
projection by construction.

A view body may not carry `where`, `first`, `limit`, `page`, or `orderby`
(`E0541`) — those belong to the query that selects *from* the view. It may
carry `group by`/`having`.

### 8.2 Materialisation **[0.25]**

A `view` is a real `CREATE VIEW`. Selecting from it composes.

### 8.3 The two-stage rewrite (#44) **[0.25]**

When a query against a view (or an inline query) has an `as many` child
**and** an `orderby`/`limit`/`page` on the driving table, the compiler emits
a two-stage form:

```sql
WITH page AS (
  SELECT i.id FROM billing.invoices i
   WHERE i.org_id = $1 ORDER BY i.issued_at DESC LIMIT $2
)
SELECT … FROM page JOIN billing.invoices i USING (id)
     LEFT JOIN LATERAL (…) lines ON true …
```

Children are aggregated only for the page. If the pushdown cannot be proven
— e.g. the `orderby` key is derived from a child — it is `E0542` with the
rewrite spelled out, never a silent O(table) plan.

### 8.4 Views and migrations (#24)

A view is a snapshotted object with a dependency edge to every table and
column it names. An `ALTER` that a view blocks is emitted as
`DROP VIEW … / ALTER … / CREATE VIEW …` in dependency order (migrations §4).

---

## 9. Pagination (#11, #40)

### 9.1 `limit`

`limit n` truncates. It is honest and it cannot reach page 2. It stays for
"top N" queries.

### 9.2 `page`

```
page [ after <cursor> ] size <n> [ max <m> ]
```

- requires an `orderby` whose keys, with the table's primary key appended,
  are a total order (`E0550` otherwise);
- `after $cursor` accepts the opaque cursor from a previous page, or `null`
  for the first page;
- `size` is clamped to `max` when given, else to `server { max_page_size }`
  (config §3);
- lowers to a keyset predicate on the ordering tuple, **not** `OFFSET`.

### 9.3 The envelope

A `page` query produces

```json
{ "items": [ … ], "next": "<opaque cursor>", "has_more": true }
```

as a `Record{items: Raw[]|Record[], next: text?, has_more: boolean}`. The
`items` element type is whatever the query produced, so raw survives the
envelope by §5.4 of types.md — that is the whole reason raw composition is a
text splice.

The cursor is base64url of the ordering tuple plus a version byte and an HMAC
over `server { cursor_secret }`. A tampered cursor is `BadRequest` 400, not a
500.

---

## 10. `transaction` and query execution

Covered in writes §7. A `select` inside a `transaction` block runs on the
transaction's connection.

---

## 11. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0501` | query clause out of order |
| `E0502` | source is not a table or view |
| `E0503` | `==?` on a non-nullable operand |
| `E0510` | ambiguous join attachment — add `under <binding>` |
| `E0520` | `first` without `orderby` on a non-unique predicate |
| `E0521` | `orderby` on an `as many` field |
| `E0530` | aggregate outside a grouped projection |
| `E0531` | non-aggregate projection field missing from `group by` |
| `E0532` | bare-join aggregation combined with `as many` |
| `E0533` | aggregate filter on a non-aggregate call |
| `E0540` | view body has no projection |
| `E0541` | view body carries a per-query clause |
| `E0542` | pagination pushdown cannot be proven |
| `E0550` | `page` without a total order |
| `W0501` | unbounded `as many` |
| `W0502` | `count` under fan-out — did you mean `count.distinct`? |

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

### 4.3 `as one` / `as many`

```
left join App.org.Members  on Members.org_id == Orgs.id           as many members
left join App.auth.Accounts on Accounts.id == Members.account_id  as one account
```

- `as one x` — at most one related row. `x : Record?` under `left`,
  `Record` under `inner` (types §6.3).
- `as many x` — `x : Record[]`, empty array when there are none. Never null.
- A join with **no** `as` result is a **bare join** (§6.2): it contributes
  to filtering and to aggregates and produces no field.

### 4.4 The attachment tree is declared, not inferred (N12)

A join's `as one` / `as many` result attaches to the binding its `on`
clause references **other than the binding being joined**. If the `on` clause
references more than one such binding, the attachment is ambiguous and is
`E0510`, naming the candidates. The fix is to write the parent explicitly:

```
left join App.auth.Accounts on Accounts.id == Members.account_id as one account under members
```

`under <binding>` is the disambiguator. It is required only when `E0510`
fires; the sample's `OrgWithMembers` does not need it, and now says so by
construction rather than by accident of clause order.

### 4.5 `as one` with no match (#3)

`a : Record?` is **null**, not a record of nulls. `$row.a.name` without
narrowing is `E0320` (types §6.4). In JSON, an unmatched `as one` field
serialises as `null`.

### 4.6 Ordering and limiting inside `as many` (#5)

```
left join App.billing.Payments on … as many payments orderby created_at desc limit 20
```

The `orderby`/`limit` bind to the child collection, not the outer query.
Unbounded `as many` on a collection with no natural bound is `W0501`.

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

`count(x)` → `int`, never null. `sum`/`min`/`max`/`avg` → `T?`
(types §6.3).

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
SELECT jsonb_build_object('id', o.id::text, 'slug', o.slug, …) FROM …
```

not `row_to_json`. The fast path is "one JSON value comes back from
Postgres and is never parsed by the application" — it was never "whatever
`row_to_json` happens to do".

### 7.3 `jwc explain`

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

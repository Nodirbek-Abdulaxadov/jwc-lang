# JWC — redesigned language, decisions from the design session

This supersedes ROADMAP.md's vocabulary. The old language had
`entity`/`dbcontext`/`with`/`via`/nav-properties/`validate body`/`new X from Y`/
`patch`/`group`/`mount`/`dome`. All removed.

## North star
Postgres-first backend language. No ORM. Two acceptance tests decide every
syntax question:
- **DBA test** — a database engineer who has never seen JWC reads a schema
  file and can state exactly what DDL it produces, without asking.
- **Developer test** — someone who does not know SQL writes a working route,
  and the SQL produced is SQL they could have written by hand.

## Vocabulary
- `database App : Postgres { init() { pool_size = ...; } }` — `init()` is
  RUNTIME config only. DB name comes from DATABASE_URL, never declared.
- `schema auth of App;`
- `table Accounts of App.auth { ... }`
- `view IssueList of App.billing { select ... }`
- `enum Priority { low, high }` — varchar + CHECK
- `enum Priority of App.billing { ... }` — real `CREATE TYPE`
- `class Register { ... }` — INPUT shapes only (request bodies)
- `service BillingService { function subscribe(...) { } }` — domain ops
- `middleware RequireAuth { ... }` / `after { }` blocks
- `routes "/api/v1/issues/{id}" use RequireAuth, Audit { route GET "" { } }`
  — cannot nest. Full path is written literally; no prefix rewriting.
- `namespace services.billing;` + `import app;` — file/module naming

## Naming style
- PascalCase: table, view, class, service, enum, middleware (things you
  declare a shape of)
- snake_case: columns, functions, locals, params, enum values, JSON keys
- Physical name = snake_case of the declared name, NO pluralisation.
  Override: `table Accounts of App.auth as "tbl_user_accounts"`, and
  `created_at timestamptz as "createdAt"`.

## Schema declaration
```
table Issues of App.billing {
    id          bigint primary key identity;
    title       varchar(200) minLength(1);      -- becomes a CHECK constraint
    body        text?;                          -- `?` = nullable; NOT NULL is default
    status      IssueStatus default IssueStatus.open;
    assignee_id bigint?;
    created_by  bigint server;                  -- in responses, never from body
    password    varchar(255) private;           -- never in responses, never from body

    primary key (a, b);
    foreign key (org_id) references App.org.Orgs (id) on delete cascade;
    foreign key (org_id, assignee_id) references App.org.Members (org_id, account_id);
    unique (org_id, key) : "message shown as 400 on violation";
    unique (org_id) where status != IssueStatus.canceled : "...";
    index on (project_id, status);
    index on (due_at) where status == InvoiceStatus.open;
    check (status == closed or closed_at == null) : "message";
}
```
Doc comments (`--- text`) become `COMMENT ON TABLE/COLUMN`.

## Queries
- Default result is RAW (row_to_json from PG, forwarded to the response with
  zero parsing). Reading a field of a raw value is a COMPILE ERROR.
- `as { id, title, alias: column }` — projection; becomes the SELECT list.
  This is the ONLY way to get a record.
- `as <Table>` was designed then DROPPED — zero uses in a full sample app.
- Joins are written in the query, never declared on the table:
  `left join Categories on Categories.id == Todos.category_id as one category`
  `left join Labels on ... as many labels`  (lateral + json_agg)
  Cardinality comes from `one`/`many`; nesting shape from the projection.
- `where col ==? value` — predicate dropped when value is absent
- `first`, `limit`, `orderby a desc, b asc`, `group by`
- `count(x)`, `count(x where pred)`, `sum`, `min`, `max` — SQL aggregates,
  only valid inside a query

## Writes
- `insert into App.x.T { ...req, col = expr } as { id, email }`
  — always one row; `as { }` is the RETURNING list
- `update App.x.T set ...req where ... first` — returns 0..N rows
- `update App.x.T set col =? maybe_null` — skip the SET when absent
- `delete from App.x.T where ...`
- `new T { }` / `patch x from y` were designed then REMOVED (ORM lifecycle)
- Load-modify-save is NOT expressible. Intentional.

## Input
- `let req = request.body() as Register;` — validates, 400 on failure
- Rules on class fields: `required`, `minLength`, `pattern(r"...")`, `min`, `max`
- `...req` spread — exact name matching only, no case conversion. The class
  is the whitelist, so spread cannot reach `private`/`server` columns.

## Errors
- Services throw: `throw NotFound("...")`, `Conflict`, `Forbidden`,
  `BadRequest`, `ConstraintViolation`
- One global `errorHandler (e) { catch NotFound (err) { ... } catch (err) { ... } }`
- Routes contain NO error handling. Route = middleware + service call + `json(...)`.
- Constraint violations with a message become 400 automatically.
- `or throw`: `select ... first or throw NotFound("...")` — proposed, replaces
  the bind/null-check/throw ritual.

## Builtins — namespaced by WHERE THEY RUN
- `string.replace`, `array.sum`, `hash.password`, `hash.verify`,
  `jwt.sign`, `jwt.verify`, `date.now()`, `request.body/header/query/raw_body/
  client_ip/path/method`, `context.get/set`, `redis.rate_limit`
- bare (language verbs): `json`, `created`, `notFound`, `badRequest`,
  `unauthorized`, `forbidden`, `noContent`, `statusCode`, `env`, `int`
- SQL aggregates bare but only inside queries
- `date.now()` = app clock; column `default now()` = Postgres clock. These
  are different clocks and the difference matters (billing periods).

## Style
Name every intermediate value with `let`. One operation per line. Do not nest
calls. Routes are 3-4 lines.

## Known-invented, never validated
`??`, `context.get/set` (untyped key-value — the weakest point),
`count(x where ...)` spelling, `random_token`, `days()`, `next_invoice_number`,
`verify_signature`, `send_email`, `log_insert` (overlaps `insert into`),
`transaction { }` + `return` semantics (commit or rollback? undefined),
`test` blocks with `assert fails { }`, `seed.*`.

## Sample app
`saas/` in this directory: 4 schemas (auth, org, billing, audit), 11 tables,
5 views, 4 services, 25 endpoints, ~1100 lines. Read it as ground truth for
how the syntax composes.

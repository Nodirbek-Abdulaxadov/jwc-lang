# tooling.md — explain, logging, lint, OpenAPI, the language server

Normative. Closes gaps **#29**, **#30**, **#31**, and the tooling half of
**N5**.

The two acceptance tests (ROADMAP §1) are *the DBA reads the generated DDL
and agrees* and *the backend engineer reads the generated SQL and agrees*.
Neither is checkable if the SQL is only visible by running the program
against a database. Everything in this document exists to make the compiler's
output readable without deploying it.

---

## 1. `jwc explain`

1.1 `jwc explain [path]` prints every query the program issues, in
declaration order, with the SQL it compiles to. It is **offline**: no
database is opened unless `--analyze` is given.

1.2 Three ways to narrow it. Without one, every site is printed:

| Flag | Selects |
|---|---|
| `--function <Service.fn>` or `--function <fn>` | the queries in that function, and in everything it calls |
| `--route "<METHOD> <pattern>"` | the queries a request to that route can reach |
| `--sql` | SQL only: no raw-tracking line, no header |

`--route` takes the **declared** pattern, the same string `request.route()`
returns (routing §5.4) — `GET /api/v1/orgs/{org_id}/invoices`, not a
concrete path. A method and pattern that name no route is an error listing
the routes that exist, never an empty success.

1.3 Reachability is the static call graph, the same one the raise sets are
computed over (errors §3). There are no function values (types §1), so it is
exact: a route's queries are its own, plus those of every function reachable
from it.

1.4 `--analyze` connects to `DATABASE_URL` and runs `EXPLAIN` on each
statement, printing the plan under the SQL. Parameters are bound as `NULL`
of the right type — the plan shape is what is being read, not the row
estimates for a particular value. A site the compiler refused is skipped
with its diagnostic, never silently.

1.5 Every site prints, above its SQL, whether the result is `Raw` or a
`Record` (types §5.1), because the promise "a projection is parsed and a raw
result is not" is otherwise only checkable by running the program.

1.6 `raw()` sites are listed first, with a count. writes §6.4 makes the
valve's usage count the measurement of which feature to add next, so it is
printed rather than assumed to be zero.

---

## 2. `JWC_LOG_SQL`

2.1 `JWC_LOG_SQL=1` makes `jwc serve` write one line per statement to
stderr:

```
[sql] 2.14ms 5 rows  SELECT t0.id AS id, … FROM billing.invoices t0 …  $1='42' $2=null
```

Duration, row count, the statement, then every bound parameter with its
index and value. All four, because each answers a different question and
three of them are useless alone.

2.2 Parameter values are printed. This is a **development** switch: the
statement's parameters are the request's data, and a log that redacts them
cannot be used to reproduce anything. `jwc serve` prints a warning line at
boot when it is set, naming the risk once rather than on every request.

2.3 `null` is printed as `null`, never as an empty string — the distinction
is the whole subject of `==?` (queries §4.4).

---

## 3. `debug.dump`

3.1 `debug.dump(x)` writes `x` to stderr and returns it, so it can be
wrapped around a subexpression without restructuring the code.

3.2 It accepts a **`Raw`** value, which nothing else does (types §5.1). That
is its reason to exist: the one place a raw result's shape can be inspected
is where the shape is in question.

3.3 It runs only under `jwc serve --dev`. Anywhere else the call is a
no-op that returns its argument unchanged — not an error, because a
half-deployed debug statement should not take an endpoint down.

3.4 A program containing `debug.dump` produces **`W1301`**. It compiles; the
warning is what stops the call reaching production unnoticed, and
`jwc check` in CI with warnings denied is what enforces it.

---

## 4. `jwc lint --constraints` (#30)

4.1 `jwc lint` runs `jwc check` and adds whole-program lints that are
advisory rather than definitional.

4.2 `--constraints` prints, for every route, each constraint reachable from
it and the status code its violation produces:

```
POST /api/v1/orgs
  uq_orgs__slug            409  "bu manzil band"
  ck_orgs__name__minlen    400  "nom juda qisqa"
  fk_members__account_id    -   (no message — 500)
```

The mapping is errors §6: a violated constraint carrying a message becomes
the declared error whose default status it has; one without a message is a
fault, and a fault is a 500.

4.3 A `unique` or foreign key reachable from a route and carrying **no
message** is **`W1302`**. It is not an error — a constraint can be a pure
invariant that no request should ever be able to violate — but the common
case is an oversight, and its cost is that a user-visible conflict arrives
as an unexplained 500.

4.4 `--deny-warnings` makes any warning a non-zero exit, which is the CI
shape.

---

## 5. `jwc openapi` (#31)

5.1 `jwc openapi [path]` prints an **OpenAPI 3.1** document to stdout, or to
`--out <file>`. Offline.

5.2 The document is derived, never authored:

| OpenAPI | Comes from |
|---|---|
| `paths` | the resolved route table (routing §5) |
| path parameters | the typed path parameters, with their JSON Schema type (routing §3.1) |
| `requestBody` | the `class` a route validates its body against |
| `200` response schema | the route's return type: a `class`, a view's projection, or a query's projection |
| other responses | the route's raise set, one per declared error, with its default status (errors §4.3) |
| `components.schemas` | every `class` and every `view` the paths reference |

5.3 A route whose response is `Raw` has no schema. It is emitted with a
`content` of `application/json` and no `schema`, which is the truthful
statement: the compiler did not check that shape either (types §5.1).

5.4 Scalar mapping follows types §2.3 — the wire form, not the Postgres
form. `bigint` and `numeric` are `{"type": "string"}` because that is what
the runtime sends.

---

## 6. The language server

6.1 `jwc lsp` speaks LSP over stdio.

6.2 Supported requests:

| Request | Behaviour |
|---|---|
| `textDocument/publishDiagnostics` | every diagnostic `jwc check` produces, on open and on change |
| `textDocument/hover` | over a `select`/`insert`/`update`/`delete`: **the generated SQL**, with `$n` placeholders and the join strategy. Over a name: its resolved type |
| `textDocument/definition` | tables, views, classes, enums, errors, services, functions, middleware |
| `textDocument/completion` | after `.`: the fields of the base's type; at statement position: the visible names |
| `textDocument/signatureHelp` | inside a call: the callee's parameters, from the typed service boundary (types §1) |

6.3 The server is **stateless between requests** beyond the document store:
each request re-runs the pipeline over the open documents. A language server
that caches a half-built model is a language server that reports a
diagnostic the compiler does not.

6.4 Hover over a query is the same string `jwc explain` prints for that site.
One compiler, one answer.

---

## 7. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `W1301` | `debug.dump` in the program |
| `W1302` | a `unique` or foreign key reachable from a route carries no message |

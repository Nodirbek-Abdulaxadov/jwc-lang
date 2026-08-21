# Changelog

All notable changes to JWC are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [0.9.903] — three defects a real port found — 2026-08-21

Porting task-tracker — a 0.9.x board API with 36 source files, m2m
labels and assignees, an audit feed and three grouped aggregates — to
1.0 turned up three defects in the compiler. Each is fixed with the
corpus case that would have caught it.

### A grouped column could not be aliased

```jwc
group by T.column_id, C.name
as { column_id: T.column_id, column_name: C.name, total: count(T.id) }
```

`E0531: column_name is neither aggregated nor grouped` — against a
column that is plainly in the `group by`. `group by` collects the column
name from either spelling, bare or qualified, but the alias map that
maps a projection alias back to its column read only the bare one. So
`as { name: C.name }` passed by coincidence (the alias equals the column
name) and `as { column_name: C.name }` did not.

### A record could not be written to a `jsonb` column

types.md §5.6 says a `jsonb` value written from code takes any `Record`,
array, scalar or `Raw` — it is the one column type whose shape is not
the schema's business. The lattice did not have that rule, so an audit
payload could only be written as a pre-encoded string.

### The native backend refused `=?` and `page`

Both are lowered now.

**`=?`** — which columns an `update` sets is a run-time fact, so every
combination is compiled and a mask picks one. Two optional assignments
is four statements; the cap is eight (256), which is far past anything a
PATCH endpoint writes. The all-absent combination sets nothing and falls
back to selecting the row as it stands, exactly as `exec::run_update`
does. Each value is evaluated once, before the branch: an `=?` whose
value calls `date.now()` must not be called to test for presence and
again to bind.

**`page`** — the cursor codec, the envelope and the HMAC are transcribed
from `cursor.rs` and `exec::page_envelope`. `server { cursor_secret }`
is emitted as the *expression*, not the value `jwc build` happened to
read: it is almost always `env("CURSOR_SECRET")`, and baking that in
would sign every deployment's cursors with the builder's secret.

### `...` spread in an `update`, and `with { … }`

Both were refused; both are lowered now, and with them all three
applications in the ecosystem — jwc-shortener, MyWallet and task-tracker
— build natively and answer `jwc serve` byte for byte.

**The spread.** Which columns `set ...$req` writes is the fields the
value actually carries. *Which fields it could carry* is the source's
declared type, and the AST says that outright in the two places a spread
source comes from: a typed function parameter, and
`let x = request.body() as C`. No type inference — codegen reads the
declaration and enumerates from there, the same mask the `=?` case uses.

The presence test is different, though, and it matters: `=?` skips when
the value is null, a spread skips when the key is **absent**. types.md
§6.5 keeps the two apart and §9.2 relies on it — a body that sends
`"note": null` clears the column, one that omits `note` leaves it. So
the prelude grew `jwc_has_field`, which `jwc_get_field` cannot answer
because it returns null for both.

**`with { … }`** replaces a header of the same name rather than appending
(routing.md §6.2). A builder has already stamped `content-type`, and two
of them is a malformed message (RFC 9110 §8.3) that clients resolve
inconsistently. `content_type` is its own field on the response object
and `jwc_to_response` reads it before the header map, so a
`with { "Content-Type": … }` that only landed in the map would lose to
the builder's — it is copied across.

### Also in the native backend

- `jwt.sign(claims, secret, ttl_minutes)` — the prelude had the 0.9
  two-argument form, which silently dropped the TTL.
- `context.<key>` and `@param` were emitted as bare `&str` where the
  built-in wanted a `V`.

## [0.9.902] — the native build answers what `jwc serve` answers — 2026-08-21

0.9.901 brought the native AOT backend back but covered only the
database-free tier: routes, control flow, expressions, and the built-ins
the restored prelude implements. Everything else was refused by name.
This closes the rest of it, and the acceptance test is not "it compiles"
— it is that the generated binary and `jwc serve` return **byte-identical**
responses, header for header, over a program that exercises every piece.

### What the pass now lowers

| | 0.9.901 | 0.9.902 |
|---|---|---|
| `select` | ✅ | ✅ |
| `insert` / `update` / `delete` | ❌ | ✅ |
| `transaction { }` | ❌ | ✅ |
| `middleware`, `requires`, `provides`, `after { }` | ❌ | ✅ |
| `service` | ❌ | ✅ |
| `throw`, `or throw`, postfix `catch` | ❌ | ✅ |
| `request.body() as <Class>` | ❌ | ✅ |
| typed path parameters | ❌ | ✅ |
| `/healthz`, `/readyz`, `/metrics` | ❌ | ✅ |
| `view` | ❌ | ❌ |
| `page after $c size $n` | ❌ | ❌ |
| `...` spread in a write, `=?` | ❌ | ❌ |

The last three are still refused by name, and the message says which
construct and that `jwc serve` runs it. A binary that quietly dropped a
query would be a far worse outcome than one that will not build.

### Errors are a `Result`, not a panic

A JWC `throw` now travels the way Rust travels errors: a generated
function returns `Result<V, JwcThrown>` and every call site propagates
with `?`. The alternative — unwinding across `.await` and catching at the
route boundary — needs `UnwindSafe` futures and poisons whatever lock or
pooled connection was held at the point of the throw.

Panics stay what they were: `Abort::Fault`, the 500. They are now caught
at the route boundary, so a fault answers 500 instead of dropping the
connection — the one failure a client cannot tell apart from the server
being gone.

### Where the two backends had drifted

Restoring the 0.9 prelude verbatim was the right call for 5,030 lines of
working runtime, but it carried 0.9's answers to questions 1.0 answers
differently. Each of these was a wire-visible difference between `jwc
serve` and the binary built from the same source:

- Every JSON response was `application/json`; the interpreter emits
  `application/json; charset=utf-8`. **Every** response differed.
- An unmatched path returned a four-key envelope, and a known path under
  the wrong verb returned 405 with `Allow`. 1.0 returns
  `{"error":"not found"}` and 404 for both.
- `notFound("gone")` served the four bytes `gone` as `text/plain`;
  the interpreter serves `{"error":"gone"}`.
- `created(json($row))` wrapped the response object as a body instead of
  re-statusing it, so the body was the marker object and the status 200.
- `noContent()` announced `text/plain; charset=utf-8` on a 204.
- `/metrics` reported two gauges under different `HELP` text and omitted
  `jwc_db_pool_max_size`, `jwc_db_pool_waiting` and `jwc_routes`.
- `internalError()` took an argument and echoed it.

All of them now come from one place per question, and the differential
run is what says so.

### The parts that had no native half at all

- **Typed path parameters.** `{id: bigint}` was matched as text and the
  type discarded, so `/notes/abc` reached the query layer and became a
  500. routing.md §3.2 makes it a 400 *before* middleware, with a body
  naming the parameter and the type — which is what it is now.
- **`request.route()`** returned the request path, so a rate-limit key
  bucketed by every distinct id instead of by route.
- **Class validation.** `validate.rs` is now mirrored in the prelude and
  driven by a table emitted from the same `ClassSym`s the checker built.
  A rule the checker accepted is a rule the binary enforces; a second,
  hand-written description of a class is what let `pattern(r"^https?://")`
  accept `javascript:` in an earlier backend.
- **Constraint violations.** A unique violation panicked into a 500.
  errors.md §6 makes a constraint carrying a message a declared error —
  `Conflict` for 23505, `BadRequest` for a check or not-null — and one
  without a message stays a fault.

### One bug worth naming

Every INSERT bound `null` for every column. The builder marks each INSERT
parameter `Bind::Expr` over a placeholder expression and
`exec::run_insert` supplies the values positionally, bypassing
`bind_params` entirely; deriving them from the placeholder — which is
`ExprKind::Null` — bound null for each. Postgres reported it as a
not-null violation on a column the program had plainly set.

### What running the real application found

The differential above uses a program written to exercise the tier. Then
jwc-shortener — 10 routes, three HTML pages, an SVG, Swagger, Redis rate
limiting — was built and diffed the same way, and found three more:

- **`crypto.token`, `string.of`, `string.slice`, `string.strip_prefix`,
  `date.hours`.** The restored prelude predates the 1.0 vocabulary, so it
  had no counterpart for the built-ins 1.0 introduced. `jwc build` refused
  on the first one and named it, which is the right failure — but it is
  still a refusal. All of `builtins.md` §2–§8 is now implemented, except
  `date.add`, which `exec_call.rs` has no arm for either: `jwc serve` does
  not run that one, and the message says so.
- **`redis.*`.** 1.0 spells the Redis surface as a built-in namespace with
  `get`/`set`/`del`/`incr`/`expire`/`rate_limit`/`enabled`. The prelude had
  the 0.9 `redis_*` names, no `rate_limit`, and answered rather than
  faulting when no server was configured — which would let a rate limiter
  allow everything.
- **The router took the first match, not the most specific.** The
  interpreter scores candidates by literal-segment count. jwc-shortener
  declares `/{code}` for its redirects beside `/docs`, `/openapi.json`,
  `/robots.txt`, `/sitemap.xml` and `/og.svg`; the native binary gave all
  five to the redirect handler and answered 404.
- **`env(name)` answered `""` for an unset variable, not null.** `??` only
  fires on null, so `env("PUBLIC_BASE_URL") ?? "https://1kb.uz"` produced
  the empty string and the short links came out as `/abc1234` with no host.

### Which prelude a program gets

Read off the prelude sources rather than a hand-kept list: codegen records
every prelude function the program reached and asks each prelude file
whether it defines it. A crate with no `pattern` rule anywhere does not
compile the regex engine; one with no query does not compile
tokio-postgres.

## [0.9.901] — `as "…"` was unusable — 2026-08-21

Two defects, both found porting MyWallet — a JWC backend written against
0.9.x — to the 1.0 vocabulary. Both are on `as "physical_name"`, which
exists so a program can keep the names a database already has, and which
is therefore the first thing a port off an older version reaches for.
MyWallet's four tables are `user`, `wallet`, `category` and `transaction`.

### A foreign key could not name a renamed table

The target's physical name was derived from the **reference** —
`references App.public.Users` → `users` — instead of from the target,
which had renamed itself to `user`. So the key did not resolve, and the
diagnostic named a table the source never wrote:

```
error[E0422]: `public.users` is not a declared table
   = help: every foreign key target must be declared in this program
```

against a program that declares exactly that table. Resolution now happens
after every table is known, keyed on the declared name.

jwc-shortener did not show this: it uses `as "…"` on both its tables and
neither is a foreign-key target.

### `RETURNING` did not quote a reserved physical name

`RETURNING` exposes the target under its own name, and the projection was
built against it unquoted:

```sql
INSERT INTO public."user" (…) RETURNING json_build_object('id', user.id)::text
                                                          ^^^^ the USER function
ERROR:  syntax error at or near "."
```

`user` there is the SQL `USER` function and the parser stops at the dot.
Every read path already went through `quote_ident`; only this one did not,
so the failure needed a write, a `RETURNING` projection and a reserved
physical name all at once — which is an ordinary combination in a ported
schema, and which made `POST /auth/register` a 500 that type-checked
clean.

Both are covered by `tests/reserved_names`, which drives insert, update
and delete against a real Postgres through a table named `user` that
another table points at.

## [0.9.901] — the native backend comes back — 2026-08-21

### What was deleted, and by whom

The v0.25.0 cutover (`60cc971`) removed 73 source files, and among them the
whole native AOT backend: 5,149 lines of codegen and 5,030 of prelude, plus
the background queue, the in-process cache, WebSocket/SSE and the mail
sender. The ROADMAP section that authorised it was written the day before
in the same hand. Neither the plan nor the deletion was put to the
maintainer, and neither was the maintainer's to discover afterwards.

The stated reason was that a second implementation of the query compiler
would have to move in lockstep with the first. **That reason does not
survive the 1.0 front-end.** `query_sql` already lowers a query to a SQL
string and a parameter list at compile time, so codegen embeds the very
string the interpreter sends: there is no second query compiler, and no
query semantics that can drift.

The roadmap also promised `jwc build --native` would answer `E0910` naming
the reason and the release it returns in. It did not: `build` was not a
subcommand at all, so the answer was clap's `unrecognized subcommand`.

### `jwc build` is back

* **The prelude returns unchanged** — 5,030 lines across base, db, crypto,
  redis, ws and http. It references no AST type, so it needed no port.
* **The codegen is new**, written against the 1.0 AST. The old one named
  `RouteDecl` with a bare path, `MountDecl`, `ModelKind` and `validate
  body`; none of those exist now.
* `jwc build --emit-rust` writes the generated source and stops, so what
  cargo is about to compile can be read first.

Verified end to end: a program with routes, a free function, `??`, a `for`
loop with `continue`, and `string.upper` generates Rust, compiles to a
42 MB binary, serves HTTP, and answers **byte-for-byte what `jwc serve`
answers** on every route.

### `serve(port)` means the same thing on both backends

The first cut of the generated `main` read `PORT` from the environment.
The interpreter evaluates `main` and takes the argument of `serve(…)`
(config.md §3.2.2), so a program that hardcodes its port would have been
served on two different ports depending on the backend. The generated
`main` now runs the program's own `main`, and `serve(n)` records the port.

### Coverage, stated rather than implied

This pass lowers the database-free tier: routes, control flow,
expressions, and the built-ins the prelude implements. Tables, views,
services, middleware, queries, `transaction`, `with { }`, postfix `catch`
and `request.body() as C` are **refused by name**, with the construct
printed and a pointer to `jwc serve`, which runs the whole language. A
native binary that silently dropped a query would be a far worse outcome
than one that will not build.

The 1.0 built-ins the prelude predates — `string.of`, `array.sum`,
`date.*`, `crypto.token`, `content`, `redirect` and the rest — are listed
in `PRELUDE_GAPS` and refused individually by name. That list is a
worklist, not a shrug.

### Still to come back

`queue.rs` (1,352 lines), `cache.rs` (177), `email.rs` (180),
`log_writer.rs` (466), `swagger.rs` (661), `templates.rs` (416) and the
package resolver, lockfile and registry client (~830). All of it is in git
at `60cc971^` and none of it is lost.

## [0.9.9] — porting a real app to 1.0 — 2026-08-21

Seven defects, all found by porting jwc-shortener — a service that has been
in production since long before the cutover — from the 0.9.x vocabulary to
1.0. Each one stopped the port dead, and each is now pinned by a test that
fails without its fix.

### The language could not answer anything but JSON

`routing.md` §6.1 said so outright: "There is no bare-string response
body." That rules out a landing page, `robots.txt`, `sitemap.xml` and an
OpenGraph card — five of jwc-shortener's routes. The documented workaround,
`statusCode(200, $html) with { "Content-Type": "text/html" }`, produced a
response with **two** `content-type` headers — the builder's
`application/json` and the author's — around a body that was still
JSON-encoded, so a browser was handed `"<h1>…</h1>"`, quotes included.

- **`content(mime, body)`** (routing.md §6.5) sends `body` verbatim under
  `mime`. The media type is a string literal, so framing can never depend
  on a runtime value and `jwc openapi` can name it; `text/*` gains
  `charset=utf-8`. It composes with the other builders, because a response
  is a value: `statusCode(404, content("text/html", $page))`.
- **`with { }` now replaces** a header the builder already set, matched
  case-insensitively, instead of appending a second one. Two `Content-Type`
  headers is a malformed message (RFC 9110 §8.3) that clients resolve
  differently.
- New: `E0735` (media type is not a literal), `E0736` (body is not `text`).

### `serve(port)` was never evaluated

`main()` was parsed, checked for arity, and then dropped. The listener took
the CLI default, so a program asking for 3000 silently got 8080 — and
`serve(int(env("PORT") ?? "8080"))`, the form this spec's own sample uses,
could not work at all. `main` now runs at boot on an ordinary Vm, which is
what makes the argument an expression rather than a decoration.
`jwc serve --port N` overrides it (config.md §3.2.2).

### `break` and `continue` did not exist

`errors.md` §7.2 is normative that a postfix `catch` block must "`return`,
`throw`, `break` or `continue`", and `E1020`'s help text says the same — so
a reader who did what the diagnostic told them got the diagnostic again.
Neither statement was in the grammar, the AST, or the parser. Both are
implemented now, which is what makes a retry-on-conflict loop expressible:
the handler has to stay inside the loop, and `return`/`throw` leave the
function. `E0813` outside a `for` body.

### Whole-table aggregates were rejected

`queries.md` §6.2 allows an aggregate projection in "a query that has a
`group by`, **or that has exactly one binding and no non-aggregate
projection fields**". Only the first half was implemented, so
`as { total: count(A.id) }` — asking a table how many rows it has — was
`E0530`. Such a query answers exactly one row for any table, empty
included, so it also needs no `orderby` under `first` and its type is `T`,
not `T?`: requiring an `or throw` on a branch that cannot be taken is how a
real null check learns to be ignored.

### `timestamptz - interval` faulted

types.md §12.2 specifies `timestamptz - timestamptz → interval` and
`timestamptz - interval → timestamptz`. `+` carried its timestamptz
overload from the start; `-` fell through to the numeric path and faulted
with "arithmetic is not defined here". The checker allowed both, so
`date.now() - date.hours(24)` compiled and answered 500 — and that is how a
query asks for "the last day".

### A long `+` chain was nesting

1.0 has no multi-line string literal (names.md §2.3, §2.4), so a page is
built from its own lines. Evaluating that chain by recursion spent one
`MAX_DEPTH` level per term, and jwc-shortener's landing page is 360 of
them: it compiled, served, and answered 500 with "expression nesting is too
deep". A left-leaning chain is a loop wearing a tree's shape, and is now
folded as one.

### A wildcard route swallowed the operational endpoints

config.md §4.0.3 — "a declared route wins" — was implemented as "anything
that matched wins", and the two differ when the match came from a path
parameter. jwc-shortener declares `/{code}` for its redirects; it spans one
segment, so it spanned `/readyz` and `/metrics` too, and the readiness probe
answered `404 {"error":"bunday havola yo'q"}`. Every pod would have stayed
out of rotation, and nothing in the source names `/readyz` for an operator
to go looking at.

A route reaching one of the three names **only through a path parameter**
no longer wins; a literally declared `routes "/metrics"` still does, which
is the half of §4.0.3 that was already right. §4.0.2 promises an operator
these paths without reading the source, and a pattern nobody aimed at them
must not take that away.

### Also

- `response.duration_ms()` / `response.duration_us()` in an `after` block
  (builtins.md §7). An `after` block exists to observe the response and how
  long it took is half of that; without it the only honest thing a
  telemetry row could say about latency was nothing. jwc-shortener wrote a
  hardcoded zero into 1.48M rows and every percentile from them was a zero.

### Known, not fixed

- `raw(sql, …) as { … }` (writes.md §6.3) does not parse. Both examples in
  that clause are fenced `no-compile`, which is why no test caught it.
- `jwc add` produces a project that cannot compile: it vendors sources into
  `jwc_packages/<name>/` **and** records the dependency, and the workspace
  walker then loads those sources as ordinary program files — so `import
  <name>` is both a local namespace and a package, which is `E0203`.

## [0.9.8] — `migrate down` — 2026-08-21

`jwc migrate down` could not roll back an ordinary schema, and the error
it printed described none of the three reasons why. Found by installing
the v0.9.7 release and running it against a real Postgres; each fault is
now pinned by a test that fails without its fix.

### Fixed

- **A rollback drops tables in dependency order.** The drops came out in
  the diff's order, which is alphabetical, so `auth.accounts` preceded
  the `org.members` and `org.invites` holding foreign keys into it and
  Postgres refused. `DROP TABLE` is now ordered so a table goes before
  everything it references — Kahn's algorithm over the foreign keys of
  the tables this migration drops, always taking the lowest-index ready
  node so two runs stay byte-identical (§10.1). A foreign-key cycle has
  no valid `DROP TABLE` order at all; its members keep their original
  order and Postgres reports it, rather than the generator inventing a
  sequence that cannot work.

- **A trigger's function is dropped after the trigger.** Phase 9 emits
  `DROP FUNCTION` and `DROP TABLE` into one bucket and the function came
  first, so Postgres refused to drop it while the trigger three
  statements below still referenced it. Any schema with an
  `on update now()` column was affected; the sample application has one.

- **A failed migration reports its own error.** A statement failing
  inside the migration's transaction leaves the connection in an aborted
  transaction, where the `pg_advisory_unlock` on the way out answers
  `current transaction is aborted, commands ignored until end of
  transaction block`. That was propagated with `?`, so it replaced the
  real diagnosis — the same text for every possible cause, naming
  neither the statement nor the dependency. The unlock is now
  best-effort on the failure path and the original error survives. This
  is what had been hiding the two faults above.

- **The release body carries one copy of its notes.** `release.yml` set
  `generate_release_notes` on a step that runs once per target, and the
  action appends the generated notes each time it runs against a release
  that already exists. The v0.9.7 body has four copies of "What's
  Changed"; one leg of the matrix asks for them now.

### Testing

`the_sample_migrates_from_nothing` applied the conformance corpus and
verified it, then stopped — it never rolled back, and that is the gap all
three faults shipped through. It now takes the sample back down and
asserts every schema is empty. Two further tests cover the function
ordering and the error masking directly.

## [0.9.7] — the 1.0 language, implemented — 2026-08-20

**BREAKING: every 0.9.x program stops compiling.** v0.25.0 replaced the
grammar with the one in `docs/spec/v1/` and deleted the old front-end.
`entity`, `dbcontext`, `with`, `via`, `validate body`, `new … from`,
`patch`, `group`, `mount` and `dome` are gone; the compiler names the
replacement rather than accepting them. There is no codemod — the shapes
do not map one-to-one, which is why the redesign happened. 0.9.x
documentation is archived under `docs/archive-0.9/`, and a 0.9.6 binary
still runs 0.9.6 programs.

> `SEMVER.md` calls a patch bump "nothing a user-written program can
> observe a behavioural change from", and this is the opposite of that.
> The number is the maintainer's call and it is `0.9.7`; the break is
> written down here so nobody meets it by surprise.

The section this replaces opened with **"No code changes. The language
design for 1.0 is now in the repository."** That was true when it was
written and stopped being true eleven releases ago. v0.20.0 through
v0.29.0 built the language that design describes, and none of it was
recorded here — a changelog that says "no code changes" over a rewritten
compiler is worse than no changelog, because it is read and believed.

Releases below in order. `ROADMAP.md` carries the done-criteria each was
held to and the reasoning behind the calls; this is the summary.

### v0.20.0 — the specification

56 unanswered semantic questions, answered or marked `DEFERRED` with what
1.0 does instead. Seventeen normative documents under `docs/spec/v1/`, a
~1100-line sample application, and `spec-coverage.json` mapping every
construct the sample uses to the clause that defines it.

### v0.21.0 — the vocabulary

Lexer, AST, parser and formatter for the new grammar. No reserved words:
`route`, `key`, `max`, `date` and `int` are all legal identifiers, because
a reserved-word list would forbid the specification's own examples.

### v0.22.0 — deterministic DDL

Five DDL object classes emitted in a fixed order, with generated constraint
and index names derived from canonical predicate text — so `a and b` and
`b and a` produce the same name and therefore no spurious migration.

### v0.23.0 — the type checker

The `Raw` / `Record` lattice, `T?` propagation, flow narrowing, and name
resolution over a flat declaration space where `import` is checked but does
not scope.

### v0.24.0 — the runtime

Routing, middleware chains with `after` blocks, the error model with typed
`throw` and a compile-time raise set, and single-table CRUD.

### v0.25.0 — the query compiler

The largest release: alias and join trees, `as one` / `as many` laterals,
aggregate modes, view compilation with two-stage pushdown, raw tracking,
keyset pagination, `exists`, and the `raw(…)` escape hatch. **The 0.9.x
front-end was deleted at the cutover** — the two lived side by side for
four releases so the old suite stayed green, and that reason expired the
moment the new one could run the sample.

### v0.26.0 — migrations

Snapshot, diff, ten-phase emission, declared renames, and the applier:
`up` / `down` / `status` / `verify`, under an advisory lock. Destructive
statements emit `-- irreversible` and stop rather than promising a
reversal that the dropped data makes impossible. A property test runs
random edit sequences and asserts a migrated database equals a created one.

### v0.27.0 — tooling

`jwc explain` per route or function, `JWC_LOG_SQL`, `debug.dump`,
`jwc lint --constraints`, `jwc openapi`, and a language server with
hover-to-SQL.

### v0.28.0 — tests and packages

`test` blocks, each inside its own transaction, rolled back whether it
passed, failed or faulted — which is the whole of the isolation model and
what makes the order irrelevant. `jwc login` / `publish` / `add`, with the
downloaded archive verified against a checksum from a **separate** request,
and a closed list of what a package may declare: no `table`, because
installing a dependency must never apply someone else's schema change to
your database.

### v0.29.0 — hardening

Hash builtins split by purpose, rate-limit keys on both IP and identity,
the `server { }` block, and a threat model. The finding was a **timing
oracle in `login`**: an unknown address returned before reaching Argon2id
at 2.4 ms against 415.8 ms for a known one — 172×, under a code comment
asserting the two were indistinguishable. Both branches verify now, the
miss against a decoy hash, at 410.9 ms and 414.8 ms.

---

### After v0.29.0 — the fix pass in this release

Everything below came out of running things that had never been run.

#### Fixed — silent wrong answers

- **`db::run_on` swallowed a column-type error.** `try_get::<_,
  Option<String>>(0).unwrap_or(None)` turned a projection that was wrong
  into *no rows*: 404 from `Shape::First`, `[]` from `Shape::Rows`, both
  indistinguishable from an empty table. A generator bug would have looked
  like missing data everywhere it touched.
- **`redis.rate_limit()` returned `true` unconditionally**, and
  `redis.enabled()` `false`; `get` / `set` / `del` / `incr` / `expire` were
  not implemented at all, so they typechecked and faulted at request time.
  A rate limiter written against the documented API admitted every request
  and nothing said so. The driver in `src/redis_engine.rs` was complete —
  **nothing ever called `init_from_env`**, so it was dead code. The
  sample's own `RateLimit` middleware had therefore never limited anything.
- **The diagnostic printer panicked on the file it was describing.** A
  non-ASCII character produced a one-byte span landing *inside* it, and
  `SourceFile::line_col` sliced there — so the compiler crashed while
  rendering the error it had just produced, mojibake included.

#### Added

- **`/healthz`, `/readyz`, `/metrics`** (config.md §4), at fixed paths, not
  declarable. v1 had served none of them since the cutover: no liveness
  probe, no readiness probe, and no way to see the pool —
  `engine::pool_status()` existed and nothing exposed it, which is why the
  soak's zero-pool-leaks criterion had never been checked. A declared route
  still wins.
- **`server { tls { … } }` and `header_timeout` are enforced** rather than
  refusing to boot. Both were hidden under `axum::serve`; writing out the
  accept loop over `hyper-util` got both and cost no new dependency.
- **`server { bind }`** — the listener address was hardcoded to `0.0.0.0`,
  so a development machine had no way to stay off its own network.
- **`E1206`** — an unknown `server { }` key. `init()` has had `E1202` for
  this since config.md was written; the server block had nothing, and
  `trusted_proxie` passed `jwc check` clean while leaving `client_ip()`
  reporting the proxy for every request.

#### Fixed — tests that never ran

Pointed at a real Postgres for the first time, **21 tests across 7 suites
failed**, none of them in the code under test: three suites composed their
psql command as `<uri> -d <db>`, which psql reads as a whole new connection
target and so fell back to the default unix socket; two shared one database
with no mutex; two more shared a scratch-database pair; and `http_golden`
asserted a route count written down as 25 against a sample that had grown
to 26.

Underneath that, **seven suites were named in no CI job at all** —
`hardening` among them — and four more ran only without the database they
need. `every_test_suite_is_named_in_ci` and
`the_spec_coverage_map_is_current` now check both claims against the
repository; the second found its own instance immediately.

#### Fixed — the soak

Closed once as "cannot run in this environment". The harness had five bugs,
each of the never-executed kind: `--format=json` without `-p r` is not
JSON; the readiness probe was `curl --fail` on a path the sample does not
declare; a port already in use satisfied that probe; `kill -TERM` on an
exited child ended the run under `set -e`; and absent latency percentiles
read as 0.00 forever. `analyze.py` also required pandas and never checked
the pool criterion.

It runs now. Eight cycles with a graceful restart between each, against the
sample on real Postgres and Redis: **480,051 requests, 480,051 2xx, zero
lost**, RSS drift 3.2%, pool waiting 0. That is twelve minutes, not
twenty-four hours, and too short for a slow leak — but the criterion is no
longer *unmeasured*.

## [0.9.6] — A harness that can fail both backends at once

0.9.5 fixed five interpreter/native divergences that an outside user found by
running into them. This release is about why *we* did not find them, and the
answer is that the parity tests could not have.

### Added

**`tests/differential.rs` — both backends, compiled and run.** Every existing
parity test string-matches the *emitted Rust source* and deliberately never
invokes cargo on it. That leaves two blind spots, and all five of the 0.9.5
bugs lived in both:

* **The call shape was right and the behaviour was wrong.**
  `badRequest({...})` emitted a well-formed `jwc_b_bad_request(...)` on both
  backends. No substring assertion can see the difference.
* **The golden value was one of the backends.** `native_parity.rs` says it
  outright: "we treat the interpreter's stdout as the source of truth". In
  all five bugs the interpreter was the wrong side, so a harness anchored to
  it would have certified the bug and moved on.

The new suite cargo-builds the generated crate, runs the binary, and drives
real HTTP at it and at `jwc run`. Both are compared against expectations
declared in the fixture — neither backend votes, so a case where both agree
and both are wrong still fails. It is opt-in (`JWC_DIFFERENTIAL=1`) because
each case shells out to cargo.

It found a new divergence on its first run, and gave the two defects TODO.md
had been carrying a place to live. Six cases ship: `error_helpers`, `redirect`,
`len_shapes`, `request_body`, `validate_body`, `field_write`.

### Fixed

**Native field assignment crashed on a row read back from the database.**
Read-modify-write — the shape every REST update handler is written in — blew up
under `--native` and worked under `jwc run`:

```jwc
let existing = select Todo from AppDb.Todo where Todo.id == @id first;
existing.title = req.title;   // HTTP 500 natively, fine under `jwc run`
update existing in AppDb.Todo;
```

`select ... first` yields `V::RawJson`. `jwc_get_field` was taught to parse
that on access, but `jwc_set_field` kept its two `Object` / `Record` arms and
`panic!`d on everything else — so reads started working while writes kept
failing, which is a worse state than both being broken. TODO.md reports it on
0.8.7 as a worker panic that dropped the connection; on 0.9.x it is caught and
answers 500. `tests/differential/cases/field_write.*` covers it and was
verified to fail without the fix.

**Native error helpers did not wrap a string argument.** `notFound("gone")`
returned the bare bytes `gone` as `text/plain` under `--native` and
`{"error":"gone"}` as `application/json` under `jwc run` — same helper, same
argument, a body no client can parse the same way twice. The same held for
`badRequest`, `internalError`, `unauthorized` and `forbidden`. Object
arguments already agreed, which is exactly why it survived 0.9.5's review:
the divergence only appears with a string, and a string is the spelling most
of the docs use. Native now goes through a shared `error_envelope` that
mirrors the interpreter's `error_response`, including the no-argument
defaults.

**`len()` was rejected by the native backend.** It carried its own registry
row with `native: false` while `length` — the identical interpreter body —
was `native: true`. The third instance of one built-in split across two
`BuiltinDef` rows, after `setConnectionString` in 0.9.5. `len` is now an
alias, and CLAUDE.md documents the pattern so the next one does not happen.

**`length()` counted characters natively, elements in the interpreter.** A
string that parses as a JSON array or object counts its elements under
`jwc run` and its characters under `--native`. This one was live in shipped
code, not merely unimplemented: `length(request_body())` returned the field
count on one backend and the byte-ish length on the other.

**`request_body()` had no native implementation.** Programs using it ran
under `jwc run` and failed to compile under `--native`. Implemented, keeping
it distinct from `body()` — this is the raw string, `body()` is the parsed
value — including the contract that an absent body yields the literal string
`"null"`.

**`jwc build --native` ignored `CARGO_TARGET_DIR`.** The binary path was
hardcoded to `<workspace>/target`, so anyone exporting the variable globally
— a single shared target dir across projects is a common setup — got a full
successful compile followed by `cargo reported success but binary not found`,
naming a path that legitimately did not exist. Found by the new harness,
which sets it to share one target dir across cases.

**A non-native built-in was reported as a typo.** `len(xs)` failed with
`unknown function — did you mean \`env\`?`. It was neither unknown nor
anything like `env`; it was a documented built-in with no native
implementation. Registry-known names that carry `native: false` now say so.

### Added — arm64 Linux

`jwc` now ships prebuilt for **aarch64 Linux**, glibc and musl, alongside the
existing x86_64 Linux and Windows builds. Raspberry Pi, Ampere, Graviton and
Android shells stop dead-ending on:

```
Unsupported platform: linux-aarch64.
```

The Docker images are multi-arch again (`linux/amd64` + `linux/arm64`).

This is a deliberate change to a **Non-goal**. `ROADMAP.md` refused a
cross-target matrix on the grounds that *"Linux x86_64 (glibc + musl) +
Docker amd64/arm64 is enough"* — but the Docker arm64 leg had been dropped
because building it under QEMU emulation hung for 30+ minutes. The policy
pointed at an escape hatch that did not exist, so arm64 users had no path at
all: no binary, no image. Both now exist, built on native ARM runners rather
than emulated. Windows-ARM, macOS-ARM and FreeBSD remain non-goals.

Two details that were wrong independently of architecture:

* `install.sh` told you to run `./install-from-source.sh` after failing.
  You reach that message by piping the script from `curl`, so there is no
  such file on disk — the advice could not be followed. It now gives the
  clone first, and a `docker run` line that needs no toolchain.
* The Docker images labelled `org.opencontainers.image.source` with the
  repository's pre-move URL. It is derived from the running repo now.

The manifest merge verifies both architectures are present and fails the job
otherwise, so a silently amd64-only image cannot ship again.

### Fixed — the Linux binaries required a very new glibc

v0.9.6's glibc builds require **GLIBC_2.39**, so they install cleanly and then
refuse to start:

```
jwc: /lib/aarch64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found
```

Found on an arm64 Android shell, but it was never an arm64 problem — the
shipped `x86_64-linux` binary needs 2.39 too. That is Ubuntu 24.04's glibc, so
the release did not run on Ubuntu 22.04 (2.35), Debian 12 (2.36), RHEL 9 or
Amazon Linux 2023 (2.34) on either architecture. The cause was `ubuntu-latest`
moving to 24.04: a glibc binary runs on its build glibc or newer, never older.

Two changes:

* **Releases build glibc targets on the oldest supported runner** —
  `ubuntu-22.04` and `ubuntu-22.04-arm`, glibc 2.35, covering all of the above.
  The musl targets stay on the current image; they link libc statically, so
  the runner's version is irrelevant. The matrix carries a comment saying not
  to "upgrade" the pins, because doing so silently drops distributions.

* **The installer no longer leaves a binary that cannot run.** It executes
  `jwc --version` after installing and, on failure, re-installs the static
  musl build automatically. This is a smoke test rather than an
  `ldd --version` comparison on purpose: a minimum-glibc constant would have
  to be kept in sync with the release runner and would drift silently, while
  asking the binary whether it runs cannot.

The fallback works against the existing v0.9.6 assets, so affected hosts are
fixed by re-running the installer — no new release required.

### Fixed — the installer 403'd on mobile networks

Resolving "latest" called `api.github.com`, which allows unauthenticated
clients **60 requests per hour per IP**. Carriers put thousands of subscribers
behind one NAT address, so the budget is routinely already spent and the
install dies with:

```
Resolving latest release tag for just-web-code/jwc-lang...
curl: (22) The requested URL returned error: 403
```

The same command works from a home network minutes later, which makes it look
like a broken release rather than a shared quota. Reported from an Android
shell on 4G while Windows and WSL on the same account succeeded.

Both installers now follow the `/releases/latest` redirect on github.com,
which resolves the same tag and is not part of that budget. The API remains a
fallback and picks up `GITHUB_TOKEN` when set (5000/hour). If resolution still
fails, the error names the rate limit and shows how to pin `JWC_VERSION`
instead of just saying "failed".

### Fixed — the multi-arch Docker merge (0.9.6 tag)

The `v0.9.6` tag built both architectures for both images and then failed to
publish `jwc`:

```
ERROR: ghcr.io/just-web-code/jwc@sha256:1ee0569e…: not found
```

The merge job selected its digests with `pattern: digests-<image>-*`. One image
is named `jwc` and the other `jwc-runtime`, so `digests-jwc-*` matched
`digests-jwc-runtime-amd64` too: the `jwc` merge collected four digests, two of
them belonging to a different repository, and the registry rejected them.
`jwc-runtime` published fine because its prefix happens to be unique — which is
how the bug hid in a green job next to a red one.

Digests are now downloaded by exact artifact name, and a count check fails with
a legible message instead of a registry 404 that names nothing.

Binaries were unaffected: `Release jwc binaries` published all five targets,
aarch64 included.

### Known — the GHCR packages are private

`docker pull ghcr.io/just-web-code/jwc` is denied for anonymous users; a
GitHub PAT with `read:packages` is required. The docs said Docker was the
no-toolchain escape hatch for macOS and other unsupported platforms, which was
wrong as written — they now show the `docker login` step, and `install.sh` no
longer suggests a `docker run` that would fail. Making the packages public
would remove the step; that is a repository setting, not a code change.

### Changed — canonical repository

Every repository URL, clone command, `raw.githubusercontent.com` install
one-liner and `ghcr.io` image reference now points at **`just-web-code`**.
`install.sh` and `install.ps1` resolved releases from the pre-move
`Nodirbek-Abdulaxadov` owner while the workflows published to the new one, so
the installer and the release pipeline were aimed at different repositories —
the aarch64 assets added above would have landed somewhere the installer never
looked.

Historical mentions are deliberately left alone: the workflow comments
explaining *why* the namespace is resolved at run time, and the changelog
entries recording the old VS Code publisher ID, are the reason those
workarounds exist.

### Fixed — `--target` allowlist had drifted

`aarch64-unknown-linux-musl` was missing from `KNOWN_TARGETS`, so an arm64 host
could install `jwc` and still be refused the static app build its x86_64
counterpart gets. Added, with a note in the source that the two lists must not
drift again.

The docs claimed the allowlist as a flat "supported triples" list.
`aarch64-apple-darwin` is on it and exercised by nothing — no darwin binary is
published, so there is no macOS `jwc` to invoke it from without building the
compiler from source first, and cross-linking to darwin from Linux needs a
macOS SDK. It stays accepted; the docs now say plainly which triples CI
actually covers and which one does not.

### Verified, not changed

TODO.md's `validate body` entry — `pattern(...)` not enforced against a
present, non-matching value, and a validation failure answering HTTP 200 in the
pre-0.7.0 envelope — was fixed in 0.9.5 but had never been regression-tested.
`tests/differential/cases/validate_body.*` now asserts the status line *and*
the envelope shape on both backends across `pattern`, `minLength`, `required`
and the accepted case. That entry asked for exactly this test; it exists now.

### Known

Four issues from the 0.6.3 → 0.8.8 migration remain open in TODO.md, three of
them interpreter-side: `raw_sql` reading only a text first column, unqualified
calls into a dependency namespace, Windows binding `[::]` without clearing
`IPV6_V6ONLY`, and `return { status: N, ... }` answering 200.

Eleven built-ins remain interpreter-only, so `--native` is still not a
superset of `jwc run`: `dispatch`, `http_post`, `send_email`, `db_query`,
`set_json_field`, and the job queue (`register_job_handler`, `enqueue`,
`enqueue_urgent`, `job_count`, `dlq_count`, `dlq_drain`). They now fail with
an accurate message instead of a misleading one.

The 0.9.5 "Known" items still stand: `jwc check` accepts calls to functions
that do not exist, and `first(rows).value` is a parse error.

## [0.9.5] — What the interpreter got wrong

From an outside user's first real project. Every fix here is a case where
`jwc run` behaved differently from `jwc build --native`, and the interpreter
was the one that was wrong — which matters more than the direction suggests,
because the interpreter is what runs on a machine with no Rust toolchain.

### Fixed

**`badRequest(obj)` and `internalError(obj)` double-encoded an object body.**
Both stringified their argument unconditionally, so `badRequest({ got: "x" })`
came back as `{"error":"{\"got\":\"x\"}"}` — an object JSON-encoded and
then stuffed into a string field, forcing the client to parse the body twice.
`notFound`, `unauthorized`, `forbidden` and `ok` all went through
`error_response` and were correct, and native was correct for every one of
them.

**`statusCode(302, { Location: url })` did not redirect.** The redirect branch
matched on `Value::Str` holding JSON, which is what object literals used to
evaluate to; they now build a `Value::Record`, so nothing matched and the call
fell through to the body path — a 302 status line, no `Location` header, and
the header map served as the response body. Native was unaffected.

**`take(xs, n)` rejected arrays on both backends.** `first` and `last` have
always accepted either, and the reference groups all three together, so
`take(rows, 5)` — the obvious pagination shape — read as a mistake in the
caller's code. A string still slices by character.

**`set_connection_string` and `setConnectionString` disagreed about native
support.** They were two registry rows, the snake_case one marked
`native: false`, so one spelling of one built-in compiled and the other was
rejected as an unknown function with a "did you mean" pointing at the name
the author had effectively already written. It is now an alias, like
`setContext` / `set_context`.

**A bare `setConnectionString()` broke the native build.** The Postgres
prelude was gated on declaring a dbcontext or entity, so a program that
called the built-in without declaring one emitted a crate referencing
`jwc_b_setConnectionString` without defining it. The build then failed inside
*generated* Rust with `error[E0425]: cannot find function` — an internal
symbol the author never wrote. The prelude is now gated on use as well as on
declarations.

### Known

`jwc check` still accepts a call to a function that does not exist; it fails
at runtime with `Unknown function`. `typecheck::check_call` returns `Ok(())`
for any name that is neither a built-in nor a known user function, and it
cannot simply reject them: the same `None` means "ambiguous across
namespaces", and `jwc check <file>` sees one file of a multi-file project.
The fix belongs in `lint.rs` as a warning, where the whole project is loaded.

## [0.9.4] — The log writer's ceiling

A saturation benchmark reported the buffered writer persisting **46%** of
offered rows at 106k req/s. Chasing that turned up five separate faults,
four of them silent.

### Fixed

**`log_insert` wrote nothing at all outside `jwc serve`.** The writer was
started only by `server::serve`, so a program that does not serve — a batch
job under `jwc run` — got `false` from every call and wrote zero rows, with
no writer around to even count them as dropped. It now starts on first push,
matching the AOT prelude, which always did.

**The writer's ceiling was one batch per database round-trip.** The drain
loop awaited each `INSERT` before looking at the channel again, so for the
whole round-trip nothing drained and arrivals had only the channel to sit
in. Up to `JWC_LOG_CONCURRENCY` (default 4) batches now overlap. Measured on
a saturation harness, 20 rows per request against local Postgres:

| | rows/s | of offered |
|---|---|---|
| batch 500, serial (0.9.3) | 50,413 | 11% |
| batch 2000, serial | 85,997 | 19% |
| **batch 2000, 4 in flight** | **178,820** | **74%** |

**A varying batch size defeated the prepared-statement cache.** The loop took
one row per `select!` poll, so the number of `VALUES` tuples tracked arrival
timing — a different SQL string every time, a miss in deadpool's
`prepare_cached` map every time, and an entry added that nothing would reuse.
`recv_many` drains up to a full batch at once, so a saturated writer emits
the same statement repeatedly.

**`JWC_LOG_BATCH` could build a statement Postgres refuses.** Rows × columns
has to stay under 65535 bound parameters and the row count alone cannot
guarantee it: 5000 rows of a 20-column entity is 100k parameters and the
whole batch failed at execute time. Batches are now chunked to fit, so the
row limit and the entity's width are independent again. The default rises
500 → 2000.

**A failed batch killed the native writer permanently.** `jwc_db_exec`
panics on error, and the AOT drain loop is a spawned task with no guard above
it, so one bad statement ended the writer for the life of the process and
every later `log_insert` silently dropped. Telemetry writes now use a
non-panicking exec and count failures.

**Native `/metrics` published three log-writer series to the interpreter's
six.** `jwc_log_written_total`, `jwc_log_batches_total` and
`jwc_log_failed_total` were interpreter-only — and `written ÷ batches` is
exactly the number that says whether the limit is statement overhead or the
database, so diagnosing a native build meant re-running it on the
interpreter.

### Added

**`response_duration_us()`.** `response_duration_ms()` cannot resolve a
handler that answers in under a millisecond: a shortener logging 1.48M
requests recorded min 0, max 1, mean 0.00, and every percentile built on that
column was zero. The value was measured all along — the unit was too coarse.

**`migrate new` generated an unapplicable `ADD COLUMN`.** A NOT NULL column
added to a table that already has rows needs a backfill default, or Postgres
refuses with `column "x" of relation "t" contains null values`. The migration
generated cleanly and only failed on the machine with production data — the
one place you least want to be hand-editing SQL. It now carries the type's
zero and drops the default on the next statement, so the migrated schema
still matches what `gen-sql` emits for a fresh database. Verified against a
900k-row table.

**A path-length pre-check on Windows native builds.** cargo nests build
artefacts ~140 characters below the workspace, which crosses `MAX_PATH` for a
project in a deep directory. The build reached the link step and died with
`LNK1104` naming a file rather than the path length. It now fails up front
with the budget, the measured length, and both fixes.

## [0.9.3] — The rest of the native-parity gaps

Everything here is `--native` catching up to the interpreter. Each one
returned a well-formed response with the wrong contents, status, or header,
which is why they read as application bugs rather than compiler faults.

### Fixed

**`validate body` failures were served as HTTP 200.** Codegen returned a
bare `{error, fields, status: 400}` object; `jwc_to_response` has no reason
to treat that as anything but a plain JSON value, so the status line said
`200 OK` while the body claimed 400. Any client branching on `res.ok` read
a rejected signup as a successful one. Native now answers with the shared
envelope through `make_response(400, …)` — `{code, details, error, status}`,
byte for byte what `http_error::validation_failed` produces.

The per-rule messages moved with it. Native emitted `minLength 3` where the
interpreter writes `minLength(3)`, reported *every* failing rule per field
where `run_validation_rules` breaks on the first, and silently skipped type
errors: `{"name": 5}` passed `minLength(3)` and `{"age": "abc"}` passed
`min(18)`, both because the emitted check only looked at the arm it wanted.

**A short-circuiting middleware skipped the after-chain.** `dispatch.rs`
breaks out of the request-phase loop on the first middleware that answers,
then runs the after-chain over every declared middleware anyway. Native's
`return __mw` jumped straight out of `route_N_inner` — the same class of bug
as the route-body `return` fixed in 0.9.2, one layer up. jwc-shortener
served 92,675 rate-limited requests and wrote zero `api_call` rows, so
throttling was invisible in the analytics. The short-circuit response is now
parked and stands in for the route body, which also means `response_status()`
inside `after { }` reads the 429 rather than a handler status that never
happened. Routes with no after-block anywhere in the chain keep the flat
early `return` and pay nothing.

**Bare string and null responses got the wrong content-type.** `server.rs`
defaults every response without an explicit type to `application/json`;
native guessed `text/plain; charset=utf-8` for a `V::Str`. A handler that
hand-builds a JSON document and returns the string — jwc-shortener's
`/openapi.json` — served the right bytes under the wrong header, and Swagger
UI refused the spec from the native binary only. A handler that returns
nothing now sends the JSON document `null` rather than an empty body, also
matching the interpreter. Handlers wanting another type say so with
`text(v)`, `html(v)`, or `response(v, "image/svg+xml")`.

**`select … first` results read back as null.** `jwc_get_field` handled
`V::Object` and `V::Record` and sent everything else to `_ => V::Null`, but a
row from the database arrives as `V::RawJson` and a dynamic object as
`V::Str`. Every field read off a query result was empty under `--native`;
jwc-shortener's `/api/links/{code}` served nulls in production. `datetime`
columns were read as `String` in the same path — `TIMESTAMPTZ` has no
`FromSql for String`, so the read could only fail, and `unwrap_or_default`
turned the failure into `""`.

**`redis_eval` re-uploaded the script on every call.** It issued a plain
`EVAL`, so a rate limiter running one script per request put the script's
bytes on the wire forever. Both backends now use `redis::Script`, which
sends `EVALSHA` and falls back once on `NOSCRIPT`.

**`cargo run -- serve` aborted on a stack overflow.** tokio's default
worker stack is 2 MiB, and the `#[async_recursion]` evaluator nests one
boxed future per expression node — which fits in an optimised build and
does not fit in a debug one. jwc-shortener's redirect route overflowed and
killed the process on the *first* request under a debug build while serving
normally from a release build, which reads as a broken application rather
than a profile artefact. Server workers now get 8 MiB, which on Linux is
address space rather than committed memory.

### Added

**Redis pool gauges on native `/metrics`.** `jwc_redis_pool_size` /
`_available` / `_max_size` / `_waiting`, alongside the Postgres pool series
the endpoint already published. Absent rather than zeroed when Redis is not
configured, matching the interpreter.

## [0.9.2] — Request logging off the critical path, and three native-parity fixes

### Fixed

**`pattern(...)` was not enforced under `--native`.** `emit_validate_body`
compiled the rule to an is-it-a-string check and discarded the regex, because
the generated crate had no regex dependency. Every other rule was emitted
faithfully, so the gap was invisible: `jwc check` accepted it, the interpreter
honoured it, only the shipped binary ignored it. A program using `pattern` as
a security boundary had none — jwc-shortener's native build accepted
`javascript:` URLs and redirected to them. `regex` is now a conditional
dependency gated on the program using `pattern`, compiled once per call site
behind a `OnceLock`. Semantics now match `runner::validation` exactly,
including a null field passing (that is `required`'s job) — which the old
codegen also got wrong, in the other direction.

**Middleware `after { }` blocks never ran under `--native`.** A route body's
`return` lowers to a real Rust `return`, and the body was emitted inline into
`route_N_inner`, so it exited past the response-status capture and the whole
after-chain. Nearly every route ends in `return`, so the response phase simply
did not happen. Route bodies with an after-chain are now lifted into their own
`async fn`.

**Source discovery walked into nested projects.** A vendored dependency —
a subdirectory with its own manifest — was loaded twice: once as a plain
project source, landing in `<root>` because package files declare no namespace
of their own, and once through dependency resolution. The duplicate broke
visibility, so the package's own call to a `private` helper failed with
`E021`. jwc-shortener had been unbuildable with its own compiler.

**Feature detection was blind to two places.** `program_calls_any` did not walk
middleware `after_body`, and did not recurse into `savepoint { }`. Harmless
while every AOT prelude shipped unconditionally; a real fault now that they are
gated, since codegen would emit a call to a `jwc_b_*` that was never included.

### Added

**`GET /metrics` on native builds.** Serves the buffered-writer series and the
Postgres pool. Registered before the user's routes — the router returns the
first match, so a catch-all like `route GET "/{code}"` otherwise swallows it —
and skipped entirely when the program declares its own `/metrics`, matching
`server.rs::route_owned_by_user`. Narrower than the interpreter's endpoint:
request counters live in `ServerMetrics` and have no native counterpart.

**`log_insert(Entity, record)` — buffered, batched telemetry writes.** A
request-logging middleware that calls `insert` puts a database round-trip on
the critical path: `runner/dispatch.rs` awaits middleware `after { }` blocks
*before* `dispatch_route` returns, so the client waits for its own log row.
`log_insert` hands the row to a bounded channel and a single background
consumer writes it in batches.

One consumer, not a task per row — spawning per row fixes latency and nothing
else: the same number of `INSERT`s still run, they compete for the pool that
real requests need, and a traffic spike spawns unbounded background work.
A bounded channel gives batching, one connection, and an explicit policy when
the writer falls behind.

Durability is the trade: rows are lost on crash (at most `JWC_LOG_FLUSH_MS`
worth) and dropped under sustained overload. That is why this is a separate
built-in rather than a mode of `insert` — the call site states which
semantics it wants. Drops are counted, not silent.

- New env vars `JWC_LOG_QUEUE` / `JWC_LOG_BATCH` / `JWC_LOG_FLUSH_MS`.
- New `/metrics` series: `jwc_log_queue_depth`, `jwc_log_queue_capacity`,
  `jwc_log_dropped_total`, `jwc_log_written_total`, `jwc_log_failed_total`,
  `jwc_log_batches_total` — absent entirely until the writer runs, so "not
  buffering" reads differently from "buffering nothing".
- New `error[E023]`: the entity argument must be a string literal, because
  both backends resolve its schema at build time.
- Works identically under `jwc run` and `jwc build --native`.

### Changed

**`http_get` / `fetch_json` moved into their own AOT prelude block.** They
were emitted unconditionally, so `reqwest` was a dependency of every
generated crate and a hello-world compiled reqwest → hyper → h2 → tower →
rustls before the linker discarded it. LTO recovered the binary size; nothing
recovered the compile time. `needs_http_client` had been computed and then
thrown away with `let _ =` — it is now honoured, and `url` follows the same
gate.

Crypto still pulls the block in: the JWKS fetch calls `jwc_http_client` and
`jwc_check_outbound_url`, so `needs_crypto` implies HTTP whether or not the
program calls `http_get` itself.

**Generated crates enumerate their tokio features** instead of taking
`features = ["full"]`. The prelude genuinely uses most of it — `fs` for the
file built-ins, `io-std` for `console.*`, `signal` for graceful shutdown,
`macros` for the emitted `#[tokio::main]` — so only `process` and
`parking_lot` fall out. A small win next to dropping reqwest, but the
manifest now says what the crate actually uses.

## [0.9.0] — Redis, as a core-tier driver

### Added

**Redis, as a core-tier driver** (`docs/spec/ecosystem.md` Faza 1). Nine
built-ins — `redis_get`, `redis_set`, `redis_del`, `redis_exists`,
`redis_incr`, `redis_expire`, `redis_eval`, `redis_ping`,
`redis_enabled` — in both the interpreter and `jwc build --native`.

This is the shared-state counterpart to the in-process `cache_*` family.
Same key/value shape and the same `ttl_secs == 0 means no expiry` contract,
so code can move between them, but the state lives in Redis and every
replica sees it. A rate limit that read 100/min per pod now reads 100/min
across the deployment.

Redis is behind a **`redis` Cargo feature, off by default**, so the default
build pulls in neither `redis` nor `deadpool-redis`. The built-in *rows* are
not gated — gating them would make `jwc check` accept or reject the same
program depending on how the binary was compiled. A binary built without
the feature warns at boot when `JWC_REDIS_URL` is set, then fails
`redis_*` calls with a message naming the missing flag.

- `rediss://` TLS via rustls with bundled webpki roots, so it works in a
  scratch/distroless container.
- Transient failures (dropped connection, timeout, `LOADING`, cluster
  `MOVED`/`ASK`) retry with exponential backoff; permanent ones don't.
- New error kinds: `RedisError`, `RedisError.ConnectionFailure`,
  `RedisError.TimedOut`, `RedisError.NoScript`, `RedisError.LoadingError`.
- `/readyz` probes Redis **only when configured**, so already-deployed apps
  keep their existing readiness behaviour on upgrade.
- `/metrics` gains `jwc_redis_pool_{size,available,max_size,waiting}`,
  emitted only when Redis is configured.
- New env vars: `JWC_REDIS_URL`, `JWC_REDIS_POOL_SIZE`,
  `JWC_REDIS_RETRY_MAX_ATTEMPTS`, `JWC_REDIS_RETRY_BACKOFF_MS`.
  `JWC_REDIS_URL` is redacted by `jwc config` — its userinfo carries a
  password.

`redis_lpush` / `redis_brpop` are deliberately absent: `BRPOP` blocks,
holding a pool connection for its whole timeout and starving the pool it
came from. Both belong with the durable queue's Redis backend.

See [`docs/archive-0.9/deployment/redis.md`](docs/archive-0.9/deployment/redis.md).

### Changed

- **`JWC_REDIS_URL` joins the `jwc config` redaction list.** It was
  previously possible for a connection string with an inline password to
  print in full, because the redaction needles matched `DATABASE_URL` but
  had no entry for Redis.

### Fixed

- **A package's `tests/` no longer breaks its consumers.** `ecosystem.md`
  §3.7 tells package authors to ship conformance cases as
  `tests/case_*.jwc`, each with its own `main()` so it can be run — but
  source discovery merged those into whatever depended on the package,
  failing the load with `E015: Duplicate function name: main`. Via a path
  dependency and via the registry alike, since `jwc publish` includes
  `tests/` in the tarball. A dependency's top-level `tests/` is now
  skipped, as is a `type: "pkg"` project's own when it loads itself, so
  `jwc lint` / `jwc test` work in a package root. An app's `tests/` is
  untouched.

- **W001 no longer reports a library's public API as dead code.**
  `public` is an export; no walk of the package's own sources can see the
  consumers that call it, so every spec-shaped package emitted one
  "defined but never called" warning per exported function. `private` and
  unmarked functions are still checked.

- **`/readyz` names the subsystem that failed.** The 503 body reported
  every failure under a `"db"` key, so a Redis outage read as a database
  one. It now emits `{"status":"not_ready","redis":"..."}` or `"db"` as
  appropriate.

### Internal

- The three copies of the "does this program call built-in X?" AST walk in
  `native_build.rs` are now one `CallScan` over a name list. The copies
  differed only in their name lists, so every new `Expr` variant had to be
  remembered in each of them — and a missed one silently under-reports a
  dependency, leaving a prelude fragment out of a generated crate that then
  fails to compile.

## [0.8.8] — int() stops lying, and console.writeln

### BREAKING

**`int(v)` no longer answers `0` for input that isn't a number.** An
unparseable string now raises, with a message carrying `type error` so it
classifies as `ValidationError` and `catch (e: ValidationError)` reaches
it. Previously `int("abc")` and `int("0")` were indistinguishable, so bad
input travelled on looking like a real number.

Two softenings ship with it, both aimed at the case that surfaced this:

- **Strings are trimmed before parsing.** `console.read()` hands back
  whatever the terminal gave, and a single trailing space was enough to
  make `int` answer `0`. `query_param` / `header` values pick up stray
  whitespace the same way. `int(" 42 ")` is now `42`.
- **`null` propagates instead of becoming `0`.** `int(null)` is `null`, so
  `int(query_param("page"))` stays usable when the parameter is absent.

Call sites that may receive absent or non-numeric input need a guard. The
shipped examples already had one (`if (port_env != "")`); the one that did
not is `int(query_param("count", "10"))` in `examples/csv-export`, which
now returns a catchable error for `?count=abc` instead of silently
computing on zero.

### Added

**`console.writeln(v)`** — `console.write` plus a trailing newline. The
common case, and without it the newline tempts you back to `print`, whose
output goes to the buffer rather than the terminal.

### Fixed

**The `env` default pattern in the stdlib docs never worked.**
`int(env("MAX_ITEMS") || "20")` fails with `'or' expects bool, got
string` — `||` is boolean-only in JWC, and `env()` returns the empty
string for an unset variable rather than `null`, so neither half of that
line was right. Replaced with an explicit `!= ""` guard.

## [0.8.7] — the filesystem and the terminal

### Added

**Console and filesystem built-ins.** Sixteen new functions under three
namespaces, working on both backends:

- `console.write(v)` / `console.error(v)` — write to stdout / stderr
  immediately, no trailing newline. `console.read()` — one line from
  stdin, `null` at EOF.
- `file.read`, `file.write`, `file.append`, `file.exists`, `file.delete`,
  `file.copy`, `file.move`, `file.size`, `file.lines`.
- `directory.list`, `directory.create`, `directory.exists`,
  `directory.delete`.

These are the first builtins with dotted names. That works because the
parser already flattens `a.b(...)` into a single call name for `dome`
namespaces; codegen maps the dot to an underscore
(`console.write` → `jwc_b_console_write`). Each one defers to a
same-named user function, so a project that already declares
`dome file { ... }` keeps its own.

The file and directory operations are `tokio::fs`-backed rather than
`std::fs`, because they are reachable from a route handler and a slow
mount must not park a runtime worker.

`console.write` is not a second spelling of `print`. `print` appends to a
buffer the interpreter flushes after `main()` returns, and a fall-through
route body returns that buffer as the HTTP response; `console.write` goes
straight to the process stdout and never becomes the response. That makes
it the correct way to log from a handler — and means mixing the two
reorders output differently under `jwc run` than in a native binary.
Documented in `docs/docs/stdlib/io.md` and
`docs/spec/aot-scope.md` § Known interpreter / native divergences.

**`IoError` error kind, with `.NotFound`, `.PermissionDenied` and
`.AlreadyExists` subtypes.** Classification comes from a typed
`std::io::Error` downcast, never from the message text — the fallback
substring scan reads `sql` as `DbError` and `url` / `http` as `HttpError`,
so `file.read("/var/backups/app.sql")` failing would otherwise be
reported as a database error.

**Lint `W007`** — `console.read()` in a route or middleware body. stdin is
not request input; the fix is `body()` / `query_param()` / `header()`. The
same call in `main()` is the intended CLI use and does not warn.

### Fixed

**Native `catch (e: Parent)` now matches dotted subtypes.**
`jwc_catch_type_matches` in the AOT prelude compared the catch type to the
error kind with `==`, so `catch (e: DbError)` silently missed every
`DbError.*` in a native binary while catching them fine under `jwc run`.
Existing native builds that catch `DbError` / `HttpError` / `JwtError`
now catch strictly more than before.

**`serve` and `random_int` were missing from the generated builtins
reference.** Neither matched any group predicate in the doc generator, and
a def matching no predicate is dropped silently — the sync test still
passes because generator and checked-in file agree on the omission.

**The builtin-shadowing lint reported the wrong code.** It emitted `W006`,
which is registered as "unreachable statement after top-level `return`", so
`jwc lint --explain` printed an unrelated description and the registered
`W005` was never emitted by anything. Its message also asserted that calls
resolve to the user function, which is true only for `substring` / `take`
and the new `console.*` / `file.*` / `directory.*` families — every other
builtin wins over a same-named user function.

**The documented regenerate command didn't work.** `gen_builtins_doc.rs`
printed `cargo run --bin gen-builtins-doc` (hyphens) in its module docs
and into the generated markdown itself; there is no `[[bin]]` entry, so
cargo resolves the target by filename and only the underscore form runs.
The same file also claimed CI verifies the doc, which it does not —
`builtins_doc_sync` is not in the workflow's test list.

### Security

The `file.*` / `directory.*` builtins pass paths to the OS unchanged —
no jail, no allowlist, no root setting. A path built from request data is
a local-file-include or an arbitrary write. Recorded as an accepted risk
in `docs/spec/threat-model.md` row 6, with the corresponding claim in row
1 corrected. `directory.delete` is non-recursive specifically to avoid a
one-call `rm -rf`.

## [0.8.5] — SQL params bound by column type, and a brand that isn't a placeholder

### BREAKING

**Wrong-arity builtin calls are rejected at `jwc check` (E022).** Four
variadic codegen branches used to pad the missing slots with `V::Null`, so
`raw_sql(sql, a, b)` compiled to a no-op that answered 200 with an empty
body. `min_args` / `max_args` were documented as informational and nothing
enforced them. `typecheck` now checks arity for both backends before
anything is emitted.

A program that passed the wrong number of arguments to a builtin used to
compile and now fails. That is the pre-1.0 minor bump this release carries
(see `SEMVER.md` — "a program that used to compile now fails" is breaking
even when the program was broken). Fixing the arity table first turned up
15 rows that disagreed with the interpreter in both directions;
enforcing those as written would have rejected working programs, so the
table was corrected against the interpreter before the check was turned on.

`serve(host, port)` with the arguments swapped — `serve("0.0.0.0", 8081)`
— took the host as the port and bound `:0`. That is also E022 now instead
of a server nobody can reach.

### Fixed — 500 on the most common route in any application

**`where <int column> == @id` never worked in the interpreter.**
`path_param()` and `query_param()` always return a string, so

```jwc
let id = path_param("id");
select User from AppDb.User where User.id == @id first;
```

bound the text `"1"` against an `integer` column and answered 500 every
time:

```text
cannot convert between the Rust type `alloc::string::String`
and the Postgres type `int4`
```

`build_where_sql` picked the bind type from the value's Rust shape and
never consulted the schema. The native backend has always resolved it from
the entity field (`WhereBuilder::col_kind`); the interpreter now does the
same through `value_to_sql_param_typed`, across `where`, `between`,
`in (...)`, and the atomic `update ... set` RHS. A column that doesn't
resolve — a joined entity's, an ad-hoc table's — falls back to the old
shape-based binding, which is what native does too.

### Fixed — native builds

- **`update ... set` bound the SET value as TEXT for a variable and int8
  for a literal**, so no form of writing an `int` column worked.
  `build_set_rhs_sql_native` now takes the target column's `PgKind`.
- **SET column names were lower-cased** while `gen-sql` quotes the declared
  casing, and the case of a column on the RHS of the same statement was
  kept — so the two halves of one statement disagreed.
- **A DB error unwound into axum and the client got no response at all.**
  The route-level panic guard was emitted only for programs that declare an
  `error_handler`. Every route gets it now, and without a handler it answers
  the same 500 envelope the interpreter does. `route_N_inner` is no longer
  separately boxed, so the guard costs no extra allocation.
- **`[::]` was bound without clearing `IPV6_V6ONLY`**, which Windows
  defaults to on, so `127.0.0.1` was unreachable. Adds `JWC_BIND_HOST`.
- **`setConnectionString(url)` failed to compile** — the native prelude took
  no arguments.
- **`not_found`, `unauthorized` and `forbidden` discarded their message**,
  which the native prelude honoured; two shipped examples pass one.

### Fixed — the DB integration suite has never run

`testcontainers`' `SyncRunner::start` calls `block_on` inside the
`#[tokio::test]` runtime, so a host without Docker got "Cannot start a
runtime from within a runtime" — a panic, not the `Err` the skip path was
written against. All six tests failed everywhere, `continue-on-error` hid it
in CI, and two fixtures had rotted unnoticed (`dependencies: []` against a
map, `integer` where the JWC type is `int`). The suite now takes
`JWC_TEST_DATABASE_URL` like the differential suite, catches the boot panic
so a host without Docker really skips, and is required in CI.

### Added

- **`random_int(end)` / `random_int(start, end)`** in both backends,
  half-open to match `range()`.
- **`unix_timestamp`** reaches native.
- **`JWC_BIND_HOST`** to override the native server's bind address.

### Changed — brand

The hummingbird is teal, and it is the same bird everywhere. `icon.png`,
the docs favicon, the navbar mark and the social card are all generated
from one master (`vscode-extension/logo-source.png`) by
`tools/gen-logo-assets.py`, so the set can't drift the way it did when the
marketplace listing shipped a blank square for two months. The Docusaurus
site drops the last of the scaffolding artwork — default logo, default
social card — and its Infima ramp is built from the two teals sampled off
the artwork.

## [0.8.0] — Query layer: a silent filter bug, `having` aggregates, `distinct`

### Fixed — wrong rows, silently

**`and` / `or` could vanish from a `where` clause.** A comparison's
right-hand side was parsed at the top of the precedence ladder, so it
consumed the `and` belonging to the surrounding WHERE tree:

```text
where Sale.amount > 2 and Sale.amount < 9
  ->  SELECT * FROM "sale" WHERE "amount" > $1
      $1 = (2 and (Sale.amount < 9))
```

The second filter didn't fail — it was folded into the first term's bound
value and disappeared. A query that should have returned one row returned
two, with nothing logged and no error raised. The RHS now parses at
additive precedence, which is what a comparison's right side actually is.

Only literal right-hand sides could reach it. `where col == @param` — the
overwhelmingly common form — returns before that code path, which is why
it went unnoticed. **This bug is present in 0.7.0 and every release before
it.** If you have a `where` with a literal RHS followed by `and` / `or`,
that query has been returning wrong rows; upgrading fixes it with no
source change.

Both backends are fixed by the one parser change — the interpreter and the
AOT codegen build their SQL from the same tree.

### Added

**Aggregates in `having`.** `having count(*) > 5` was a parse error, so the
thing `having` exists for could not be written:

```jwc
select Task { status, total: count(*), effort: sum(hours) }
    from AppDb.Task
    group by status
    having count(*) > 2 and sum(Task.hours) >= 40;
```

An aggregate alias from the projection works too — Postgres rejects an
output alias in `HAVING`, so `having total > 2` is resolved to the
aggregate it names before any SQL is built. Previously that form compiled
and then died at the database with `column "total" does not exist`.

A `having` term that is neither a group key, an aggregate, nor an alias is
now `error[E010]` at `jwc check`. It used to reach Postgres as *"column
must appear in the GROUP BY clause or be used in an aggregate function"*,
at runtime.

**`select distinct`.**

```jwc
let countries = select distinct Sale { country } from AppDb.Sale;
```

Composes with `where`, `orderby`, `limit`, `group by` and `join`, and is
part of the prepared-statement shape key so the distinct and non-distinct
forms can't share a cached plan. `select distinct count(*)` is rejected at
parse time — de-duplicating a one-row result is always a no-op, and SQL's
`count(distinct col)` is a different construct that isn't emitted yet.

### Changed

`having_with_group_by_validates` asserted `having Sale.amount > @min` while
grouping by `country`. Postgres rejects that program, so the test was
replaced rather than kept: E010 now catches it, and a new case covers
`having` on a real group key.

## [0.7.0] — Field feedback: the DSL, the editor, and the HTTP contract

Two real applications — MyWallet and jwc-shortener — were written against
0.6.x and their authors wrote down every place the language got in the way.
This release works through both lists. Nothing here is speculative; each
item below started as a workaround somebody had already shipped.

### BREAKING

**One error envelope.** The runtime returned three different error shapes
and a client had to handle all of them:

```text
{"errors":{"email":"pattern(...)","password":"minLength(8)"}}   // validate
{"status":404,"error":"Not Found","method":"GET","path":"/x"}   // router
{"error":"category has transactions; delete them first"}        // handler
```

They now share one shape, with `code` as the stable key to branch on
(`validation_failed`, `not_found`, `method_not_allowed`, `timeout`,
`internal_error`):

```text
{ "error": "…", "status": 400, "code": "validation_failed", "details": {…} }
```

Per-field validation detail moved from a top-level `errors` object to
`details`, and every body gained `status` and `code`. A client reading
`.error` for the message keeps working — that key is now present on all of
them, where before it was missing from the validation response.

**A 500 no longer echoes server internals.** The raw error used to go
straight to the caller, putting internal Rust type names and SQL text in
front of anyone who could make a request. The response is now a generic
message pointing at the `x-request-id`; the full error is still logged
server-side, and `JWC_DEBUG_ERRORS=1` restores it locally.

### Language

- **`unique(a, b);`** — table-level composite unique constraints, checked
  against the entity at `jwc check`. A join table's `(taskId, labelId)`
  pair previously had to be enforced by a select-then-insert in
  application code, which is a TOCTOU race.
- **`col int index;`** — index declarations. `gen-sql` emitted no
  `CREATE INDEX` at all, so every foreign-key column was unindexed and
  `where user_id == @u` was a sequential scan.
- **`null`** is accepted as a spelling of `nullable`.
- **`&&` and `||`** as aliases for `and` / `or`; `and` / `or` keep working.
- **`+=` / `-=` / `*=` / `/=`** on plain variables and object fields.
- **`?:` and `??`**, both short-circuiting, so `x ?? expensive()` is safe
  to write. `?:` requires a bool condition; `??` tests for null
  specifically, so `0` and `""` pass through as themselves.
- **`async` / `public` / `private` on dome members.** Domes hold the
  business logic, so `async` was available everywhere except where domain
  code is written.

### HTTP runtime

- **CORS.** There was none — an `OPTIONS` preflight fell through to the
  route table and came back 404, so a browser frontend on another origin
  needed a reverse proxy in front of the server. `JWC_CORS_ORIGINS` turns
  it on; it stays off unless configured, and `*` plus credentials is
  refused at boot rather than silently ignored by the browser.
- **405 for a wrong verb.** A path that existed under another method was
  indistinguishable from one that didn't exist — both 404. Wrong-verb
  requests now get 405 with an `Allow` header.
- **Dual-stack bind.** The listener bound `0.0.0.0`, so a Node dev proxy
  resolving `localhost` to `::1` got `ECONNREFUSED`. It now binds `[::]`
  and falls back to IPv4 where there is no IPv6 stack.

### Database

- **Numeric parameters bind by column type, not value magnitude.** An
  `i64` was passed for every integer regardless of the column, so an
  `int4` column raised `cannot convert between the Rust type i64 and the
  Postgres type int4`. Binding now resolves against the target type;
  `decimal` goes through the value's shortest text form so money doesn't
  pick up `f64` drift.
- **`raw_sql` routes on the statement's result shape.** Anything not
  starting with `select` / `with` went down the exec path, so
  `UPDATE … RETURNING url` returned the affected-row count and discarded
  the column it asked for. A prepared statement with no result columns is
  now an exec, anything else is a query.

### Native AOT

- `jwt_sign` / `jwt_verify` are supported, so a project using Bearer auth
  can be built with `--native` at all.
- Handled errors stop printing panic noise. `try { jwt_verify(…) } catch
  { unauthorized() }` is every auth middleware, and each unauthenticated
  request logged a panic message and a full backtrace for what the
  interpreter reports as nothing. `RUST_BACKTRACE=1` restores the trace.
- `decimal` / `numeric` columns work end to end. They mapped to
  `PgKind::Float`, so writes hit `WrongType` and reads fell through to
  `V::Null` — a money column came back empty with no error raised.
- 404 / 405 use the same JSON envelope as the interpreter.

### Editor

- **`jwc check` and the language server validate against the whole
  project.** Both parsed a single file in isolation, but a JWC project is
  one flat namespace — so on a project `jwc lint`, `jwc test` and
  `jwc run` all accept, the editor showed 12 diagnostics, including the
  same middleware reported as both "declared but never attached" and
  "unknown". Warnings are now published on the file that declares the
  symbol, and validation errors are anchored at their real line.
- **Go-to-definition and rename work across files.** Rename previously
  resolved a sibling file's symbol and then edited only the current one.
- **`textDocument/formatting` is implemented and advertised.** The
  capability was never declared, so format-on-save came back `-32601
  Method not found` and silently did nothing.
- **The extension warns when `jwc-lsp` is older than the extension.** The
  two update through different channels, and a stale binary flags valid
  code as an error in the Problems panel.

### `jwc fmt`

Four ways the formatter produced source the parser then rejected:
`min_length` / `max_length` instead of `minLength` / `maxLength`,
`dbcontext AppDb Postgres;` without the `:`, `pub function` (the lexer
only knows `public`), and `function Dome.member()` — which deleted the
`dome` wrapper, so formatting any comment-free file containing a dome
corrupted it. Files carrying comments took the line-based fallback, which
is the only reason this wasn't constant.

The durable fix is a round-trip test over every declaration form; it is
what found the last three.

### Documentation

A sweep of every fenced example found 37 of 136 unparseable, including the
README's headline example — the first code anyone sees. It used a
`dbcontext AppDb { Notes: Note }` block form and colon-separated entity
fields, neither of which the parser has ever accepted. The surrounding
claim was wrong too: one entity and one dbcontext do not yield CRUD
routes; there is no route generation.

`tests/docs_parse.rs` and `tests/snippets_parse.rs` keep docs and shipped
snippets at zero parse failures. Deliberate excerpts are marked
` ```jwc no-compile `.

### Fixed

- `jwc check main.jwc` failed on a bare relative filename —
  `Path::parent("main.jwc")` is `""`, not `"."`, so the project root came
  back empty and every path built from it was unreadable. `./main.jwc`
  worked. Introduced by the project-aware `check` above, and caught before
  release.

## [0.6.3] — Hotfix: native redirect with `V::Record` header object

`statusCode(3xx, { Location: url })` stopped redirecting on the `--native`
build — it returned `{"Location":"..."}` as a JSON body with no `Location`
header, so browsers never followed it. Object literals lower to `V::Record`
(the shape-deduped fast layout) on the native path, but `jwc_b_status_code`
only special-cased `V::Object` for the 3xx-as-headers branch, so the record
fell through to the JSON-body arm. It now accepts both `V::Object` and
`V::Record`. The interpreter was unaffected (it builds `V::Object`).

Verified end-to-end: a native jwc-shortener binary against Postgres now
returns `HTTP/1.1 302` + `location:` for `GET /:code`.

## [0.6.2] — Hotfix: native AOT Cargo.toml dependency emission

The `--native` build produced a non-compiling crate for any DB-touching app
(`error[E0433]: unresolved module or unlinked crate`), surfaced by the
jwc-shortener Linux CI build. Two bugs in `render_cargo_toml`
(`src/native_build.rs`):

- **`tokio-postgres` / `deadpool-postgres` (and the crypto crates) were
  emitted *after* the `[target.'cfg(windows)'.dependencies]` table header**,
  so they landed under the Windows-only target and silently vanished on
  Linux/musl. The `[target.'cfg(windows)']` block is now the last thing
  written, after the conditional `needs_db` / `needs_crypto` deps.
- **`serde_json` and `url` were never declared** even though the prelude uses
  them unconditionally (JSON body validation, SSRF host-allowlist parse in
  `http_get`). Both are now direct `[dependencies]`.

Native builds on Windows masked the first bug (the crates resolved via the
`cfg(windows)` table) — only the Linux release path failed.

## [0.6.1] — Hotfix: atomic update-set column case

- **`update CTX.Table set col = expr where …` no longer lowercases the SET
  column name** (or an RHS column self-reference). It previously emitted
  `"columnid"` for a `columnId` column and failed to prepare against camelCase
  schemas (`Failed to prepare SQL statement`); the `hits = hits + 1` example
  never hit it because the column was already lowercase. Columns are now quoted
  as-declared, matching `where` / `insert` / `update`. Surfaced by a task-tracker
  `move` (reorder) endpoint.

## [0.6.0] — Query Layer complete + native query-layer parity

Closes ROADMAP **Phase 11 (Query Layer)** — the last 1.0-blocker. `raw_sql` is
no longer the default escape hatch for cross-table reads. Re-dogfooded on
task-tracker: **0 raw_sql, 0 read-path N+1**.

**Cross-entity queries**

- **Explicit `join Entity on a == b`** (inner equi-join, chainable) with
  table-alias qualification, **aliased columns** (`columnName: Column.name`),
  and **grouped aggregation over a join** — bringing cross-table stats to 0
  raw_sql.
- **`group by` + `having`** with aliased aggregate projection
  (`select Task { status, total: count(*) } group by status`).

**Filters**

- **Optional predicate `op?`** (`status ==? @s`) — a null/empty bound value
  drops the term, so one static query serves every filter combination.
- **Dynamic in-list** — `where col in (@arr)` binds a runtime array as
  `= ANY($1)`.

**Eager loading**

- `with` now covers every nav kind — belongs-to, has-many/one, many-to-many
  (link table) — plus nav projection (hides columns) and nav ordering.
- **Two-level nested `with`** (`select Project with boards.columns`) loads an
  aggregate root and two levels of children in one query.

**Mutations**

- **Atomic `update CTX.Table set col = expr where …`** (no read-modify-write):
  counters, status transitions, and position-shift reorders. RHS supports
  column arithmetic (`position = position + 1`).

**API docs**

- Built-in **`/openapi.json`** (OpenAPI 3.0.3, generated at request time from
  the live routes) and **`/docs`** (Swagger UI). Off via `JWC_DISABLE_OPENAPI`.
  Also offline from the CLI: `jwc openapi` (3.0.3) / `jwc swagger` (3.1).

**Native AOT**

- **Query-layer parity**: nav eager-load (all kinds + nested), grouped
  aggregation, explicit join, and `==?` all codegen the same SQL the
  interpreter emits.
- **Fixed** a call-resolution bug where a camelCase root function call
  (`byStatus()`) wasn't rewritten to its FQN and was rejected as "unknown
  function" — this blocked native builds of any camelCase-named app.
- Still interpreter-only on the native path: `jwt_sign` / `jwt_verify`,
  dynamic in-list (`= ANY`), and a `where` on a joined entity's column.

## [0.5.1] — Release pipeline fixes

No language or runtime changes from v0.5.0 — this release just gets the
publish pipeline green.

- **Docker image build is amd64-only.** The multi-arch build compiled the Rust
  release for arm64 under QEMU emulation and effectively hung (30+ min). arm64
  can return later via a native ARM runner + manifest merge.
- **VS Code extension renamed** `jwc-lang` → `jwc-language`. The Marketplace
  name `jwc-lang` is taken by another publisher, which failed the v0.5.0
  Marketplace publish; the bundled `.vsix` and the publish now use the new id
  `Nodirbek-Abdulaxadov.jwc-language`.

## [0.5.0] — Query Layer: relation loading + grouped aggregation

The first slice of the Query Layer (ROADMAP Phase 11). Navigations now
materialise related rows in a single query, and single-entity grouped
aggregation projects typed result rows. The dogfooding app (task-tracker) was
rewritten on top: read-path N+1 dropped to **zero** and the stats `raw_sql` for
status counts is gone.

**Eager loading via `with`** — a navigation pulls related rows into the result
as a nested JSON value, in one correlated query:

- `posts: List<Post> via Post.userId orderby createdAt desc;` — one-to-many,
  optionally ordered (`json_agg(... ORDER BY ...)`).
- `author: User { id, name } via authorId;` — belongs-to (this entity holds the
  FK; distinguished by a bare, undotted `via` column), with an optional column
  projection so an eager-loaded relation can hide sensitive columns
  (e.g. `passwordHash`).
- `labels: List<Label> via TaskLabel(taskId, labelId);` — many-to-many through a
  join table.

`select Entity with rel1, rel2 from Ctx.Table` returns each row with the
relations nested.

**Grouped aggregation** — an aliased aggregate projection drives the SELECT list,
so `select Task { status, total: count(*) } from Ctx.Task group by status`
returns typed `{ status, total }` rows. `count(*)` / `sum` / `avg` / `min` /
`max`.

**Migrations** — `jwc migrate new` now emits `ALTER TABLE … ADD/DROP CONSTRAINT
… UNIQUE` when a `unique` modifier is added to (or removed from) an existing
column; previously only a fresh `CREATE TABLE` honoured it.

**Release & CI** — the `x86_64-unknown-linux-musl` release build vendors OpenSSL
for that target (it had failed at `openssl-sys` since v0.4.8); the VS Code
extension lockfile is back in sync (`npm ci`); and the runner code is
rustfmt/clippy-clean, so `main` CI is green again.

**Docs** — README/docs corrected to the real implementation: `unix_timestamp()`
(not `now_epoch()`), `query_param` returns `""` when absent, `jwt_verify` strips
an optional `Bearer ` prefix, and the `group by` / `having` section reflects what
actually runs.

**Interpreter-only** — the new nav/aggregate query forms run under `jwc run` /
`serve`; `jwc build --native` rejects them with a clear compile error for now.

## [0.4.9] — Runtime correctness fixes (pain-log root causes)

Fixes a cluster of dogfooding-surfaced bugs at their root, each guarded by a
regression test (341 unit tests green).

**Response model**: a body key named `status` is no longer swallowed — the HTTP
status now travels through an internal `__jwc_status__` sentinel (mirroring
`__jwc_content_type__`/`__jwc_body__`), so `json({ status: ... })` and entities
with a `status` column round-trip intact.

**Value model unified**: a row from `select ... first` (a `Record`) is now
accepted by `update <var> in`, `insert`/`delete <var>`, entity-typed function
returns, and entity-typed parameters. The canonical
`let x = select…; x.f = …; update x in T;` pattern — including across a function
boundary — works.

**Schema-aware parameter binding**: `insert`/`update` bind by the column's
declared type instead of guessing from value shape. An ISO-date *string* into a
`varchar` column stays text; a JSON *object* into a `jsonb` column binds as real
`jsonb`.

**Partial / PATCH**: a typed `class` parameter no longer requires every declared
field to be present (presence stays the job of `validate body { … required }`),
so partial PATCH payloads validate.

**Auth**: `jwt_verify` strips an optional `Bearer ` scheme prefix, so handlers
can pass `header("authorization")` straight through.

**Entities**: `unique` column modifier is now honoured end-to-end (DDL +
migration-diff round-trip).

**Pagination**: dynamic `limit`/`offset` values are bound parameters, fixing a
SQL-compile-cache collision that made `offset` silently no-op.

**Ergonomics**: `query_param(name)` returns `""` (not `null`) when absent,
matching `path_param`/`env`. Docs corrected (`for x in xs` has no parentheses;
entity columns use `<name> <type> <modifiers>` with `nullable`/`autoincrement`,
not colon/`?`/`auto`).

## [0.4.8] — Phase 8 developer experience + ecosystem close-out

Bundles the full Phase 8 dev-experience surface from
PRODUCTION_READINESS_PLAN.md across eight parallel sprint deliverables
in two batches.

**Deploy**: official multi-arch Docker images on GHCR
(`jwc:0.4.8` + `jwc-runtime:0.4.8`, distroless cc-debian12:nonroot for
the runtime variant), `x86_64-unknown-linux-musl` static binary in
every release with `JWC_MUSL=1` install opt-in, k8s
migrate-as-init-container deployment guide.

**Onboarding**: `jwc new <name> --template <empty|api|auth|jobs>`
ships three starter projects baked into the binary; "Zero to deployed
CRUD in 15 minutes" tutorial walks scaffold → Postgres + migrations →
native build → Docker → k8s rollout.

**Editor**: LSP gains go-to-definition, rename, context-aware completion
(`catch (e: ?)` / `use ?` / default keywords + builtins + user fns).
VS Code Marketplace publish pipeline wired (Marketplace + OpenVSX,
GitHub Release artefact fallback when secrets are missing).

**Formatter**: `jwc fmt` finished via hybrid AST + line-based dispatch
(line-based when source contains comments, AST canonical output
otherwise, line-based fallback on parse error). CLI:
`jwc fmt [paths] [--check] [--stdout]`. Idempotency test walks every
`.jwc` under `examples/`, `templates/`, `tests/conformance/cases/`.

**Codemod scaffold**: `jwc upgrade [paths] [--dry-run]` lands the
deprecation migration runner. Registry is empty at v0.4.8; first
scheduled rule is `no-typecheck-removed` in v0.6.0
(per `DEPRECATION.md`).

**Autogen docs**: `src/bin/gen_builtins_doc.rs` walks `BUILTIN_DEFS`
into `docs/docs/reference/builtins.md` grouped by 15 categories.
`tests/builtins_doc_sync.rs` fails CI when the checked-in doc
diverges from the generator output.

Tests: 336 lib (+6), 8 jwc-runtime, 35 conformance, 3 native_parity,
21 imports, 1 fmt_idempotency, 1 builtins_doc_sync, 1 lsp_smoke (3
ignored), 1 chaos (ignored), 1 lib ignored. Builds clean default +
`--features otlp`.

Phase 8 [1.0-blocker] developer experience closed. Long-form docs
site finalization + registry stable-contract write-up remain as
follow-up content work.

## [0.4.7] — Sprint 1-5 chala ishlar yopildi: Phase 2/6/7 close-outs

Closing every remaining partial-state item from Phases 2, 6, and 7 so
the 1.0 ship gate has nothing dangling above the line. v0.4.7 ships:

**Phase 2 #11 — unwrap budget audit**

The plan listed ~340 `.unwrap()` calls to convert; the actual audit found
the inflation came from counting `tests.rs` modules + double-counting
mod.rs+tests.rs. After this commit there is exactly **one** production
`.unwrap()` in `src/`, converted to `.expect("INVARIANT: ...")` with a
precise reason.

- `src/runner/types.rs:168` — `.unwrap()` → `.expect("INVARIANT: ...")`.
- `Cargo.toml` `[lints.clippy]` comment block rewritten: the right flip
  is per-module `#![cfg_attr(not(test), warn(clippy::unwrap_used))]`,
  not a global `warn`. Both lints stay `allow` with a documented
  TODO[unwrap-budget] for the per-module pass.
- `CONTRIBUTING.md` extended: three categories (A INVARIANT / B Result?
  / C Mutex), marker conventions, lint roadmap.

**Phase 6 — Security program close-out**

A. cargo audit blocking flip:

- Bumped `tokio-postgres = "0.7.18"` (from 0.7.16) — closes
  RUSTSEC-2026-0178 / -0179 / -0180.
- `.github/workflows/security.yml` confirmed blocking (no
  continue-on-error). Ignore list reviewed; remaining 8 IDs justified.
- `SECURITY.md` gains "Dependency hygiene" section pointing at the new
  threat-model doc.

B. Threat-model pass — `docs/spec/threat-model.md` (new):

- **Path traversal in `{param}` capture** — `match_route_pattern`
  rejects `..`, `.`, `/`, `\`, NUL via new `is_traversal_segment`
  helper. +4 regression tests.
- **Header injection** — interpreter path was already safe via
  `axum::HeaderValue::parse()`. Native AOT now also rejects
  `\r`/`\n`/NUL in header values (was only checking names).
- **SSRF allowlist** — new `JWC_HTTP_ALLOWLIST` env var (CSV hosts);
  empty/unset = no restriction (backwards compat). Helper
  `check_url_allowlisted` wired into `http_get`/`http_post`/`fetch_json`
  in the interpreter AND `jwc_check_url_allowlisted` in native AOT.
  Registered in `src/config.rs::REGISTRY`. +3 tests.
- **JWT `exp` enforcement** — `jwt::verify_hs256` now checks `exp`
  after signature verify. Absent → accept (don't break old tokens);
  past → reject with `"token expired"`. Closes the Sprint 3A
  `JwtError.Expired` deferral; classifier branch added; the kind sits
  in `JWC_ERROR_KINDS`. +3 tests.
- **SQL interpolation audit** — clean: every `format!`-built SQL site
  uses compiler-resolved table/column names; user values flow through
  `$N` placeholders + `boxed_params`. Documented with file:line
  citations.

C. Secrets redaction:

- `src/engine.rs::scrub_database_url` masks `://user:password@` →
  `://user:***@`; called wherever connection-string strings flow into
  error context. +4 tests.
- `src/error_report.rs::scrub_secrets` is the last-pass scrubber for
  the CLI error printer + runtime error logs. Strips
  `scheme://user:password@` AND `password=...` (stops at
  `&`/whitespace/quote). Wired into `print_cli_error`,
  `log_runtime_error_text`, `log_runtime_error_json`, `to_single_line`.
  +3 tests including `database_url_with_password_redacted_in_connection_error`
  and `smtp_password_not_leaked_in_error_chain`.

**Phase 7 — Performance with receipts (partial)**

A. Bench DB tier added to `bench` repo (`_my/jwc-app`):

- `entity World of BenchDb` (`@id int`, `randomNumber int`).
- Migration `1781373067_init-bench.{up,down}.sql` — `world` table
  + 10,000-row seed via `generate_series` with `ON CONFLICT DO NOTHING`
  (idempotent).
- Three new TechEmpower-shape routes:
  * `GET /db` — single random SELECT.
  * `GET /queries?queries=N` — N selects, N clamped 1..500.
  * `GET /updates?queries=N` — N update+select pairs.
- `bench/.dist/bench.sh` + `bench.ps1` extended with the three new
  endpoints at `c=64 d=15s`; URL builder appends `?queries=20`.
- `bench/.dist/setup-linux.sh` gains an idempotent `psql` seed block
  guarded by `JWC_BENCH_SKIP_DB` + `DATABASE_URL` presence.

B. README "Performance" section (`jwc-lang/README.md`):

Top-of-file 3-bullet headline + bench-repo link. The strongest
positioning asset the project has is now visible above the fold.

C. AOT scope contract (`docs/spec/aot-scope.md` + native_build header):

Explicitly scopes 1.0 native AOT as the **stateless route tier**.
Documents: what works end-to-end on `--native` (stateless routes,
V::Record, response helpers, simple select/update/insert, cache,
sleep_ms, http_get, JWT, hashing), what panics in the native build
(`savepoint`, the Postgres queue worker loop), what falls back to
`jwc run` (long-running queue workers, mid-tx savepoints, OTLP traces).
`src/native_build.rs:30` header comment updated to point at the new doc.

**Error kinds catalog:**

- `JwtError.Expired` lands (closes Sprint 3A deferral).

**Env vars added:**

- `JWC_HTTP_ALLOWLIST` (CSV hosts; empty = no restriction).

Tests: 324 lib (was 306, +18 across security + redaction + path
traversal + SSRF + JWT exp), 8 jwc-runtime, 35 conformance,
3 native_parity, 21 imports, 1 chaos (ignored), 1 lib ignored.
Builds clean default + `--features otlp`.

Sprint 1-5 + every chala ish closed. Phase 6 done; Phase 7 partially
(bench DB tier + scope docs + README — Linux session execution +
GitHub Actions regression gate + 72h soak run remain as ops-side
work).

## [0.4.6] — Sprints 2–5: code health + Phase 3/4/5 [1.0-blocker] close-outs

The big Sprint 1-5 wrap. v0.4.5 shipped the Phase 1 unified value model;
v0.4.6 closes every remaining [1.0-blocker] across Phases 2, 3, 4, and 5
of `PRODUCTION_READINESS_PLAN.md`.

**Sprint 2 — code health & diagnostics**

- `src/runner/mod.rs` (5,647 lines) decomposed into 9 sub-modules:
  `dispatch.rs`, `eval.rs`, `exec.rs`, `sql.rs`, `types.rs`, `util.rs`,
  `validation.rs`, plus the pre-existing `builtins.rs` and a `tests.rs`
  harness. Every production sub-file under 1,200 lines; `mod.rs` 787.
- `src/parser.rs` (5,197 lines) decomposed into 7 sub-modules:
  `decl.rs`, `expr.rs`, `stmt.rs`, `validate.rs`, `validate_walk.rs`,
  plus a `tests.rs` harness. All under 1,200 lines.
- `fuzz/` standalone crate with `lex` + `parse` libFuzzer targets +
  `.github/workflows/fuzz.yml` nightly 8h-per-target CI.

**Sprint 3 — typed catch + dotted subtypes + gradual type checker**

- `JWC_ERROR_KINDS` grows from 5 to 18 entries with hierarchical
  dot-paths (DbError.UniqueViolation, HttpError.NotFound, etc.).
- Classifier downcasts `tokio_postgres::Error` (SQLSTATE matrix) and
  `reqwest::Error` (HTTP status family). Parent matches all
  children; "Error" still catches everything.
- Parser accepts `catch (e: A.B.C)` dotted form. Validator does
  prefix lookup (`closest_known_kind` hint on unknown root).
- **Gradual static type checker (`src/typecheck.rs`)**:
  E018 return type, E019 call-site arity, E020 arg type. Wired
  via `project::load_project_from_root_with` so every loader path
  runs it. `--no-typecheck` escape hatch on `jwc check / run / build`.
- `docs/spec/semantics.md` covers integer overflow, float format,
  UTF-8 strings, `==` cross-type rules.

**Sprint 3 #16 — AOT visibility re-check**

- New `parser::validate::check_visibility` walks every call site in
  functions / routes / middlewares / errorHandler. Emits E021 with a
  did-you-mean hint when a private function is referenced across
  namespaces.
- `src/native_build.rs` codegen header updated: "NOT re-checked here"
  → precise reference to the validator section + `docs/spec/visibility.md`.

**Sprint 4 — data layer hardening**

- **Migration safety**: `_jwc_migrations` gains a `checksum text`
  column (idempotent ALTER). `migrate up` recomputes the SHA-256 of
  every already-applied `.up.sql` and refuses to run on a mismatch.
  Each migration is wrapped in `BEGIN; ... COMMIT;` UNLESS the file
  opens with `BEGIN` itself (CREATE INDEX CONCURRENTLY etc.).
  `jwc migrate status` prints the applied / pending / sha-mismatch /
  orphan matrix; `--dry-run` on `up` and `down`.
- **Savepoints**: new `savepoint <name> { ... }` syntax inside
  `transaction { }`. Engine helper issues `SAVEPOINT/RELEASE/
  ROLLBACK TO SAVEPOINT`. Naked `transaction { transaction {} }` is
  rejected with **E016**; savepoint outside transaction with **E017**.
- **`json()` validates strings, `json_unchecked()` escape hatch**.
  Interpreter: unconditional validation. Native AOT:
  `#[cfg(debug_assertions)]` validation. The old footgun (passing
  malformed JSON as a 200 body) is closed by default.
- **Pool resilience**: retry-with-backoff on transient errors
  (SQLSTATE 08* / 40001, `tokio_postgres::Error::is_closed()`,
  `PoolError::Backend`/`Timeout`). Skipped inside `transaction {}`
  to avoid silent re-execution. `JWC_DB_RETRY_MAX_ATTEMPTS` (3) +
  `JWC_DB_RETRY_BACKOFF_MS` (100, exponential). New
  `engine::ping()` wired into `/readyz`. Four `jwc_db_pool_*` gauges
  added to `/metrics`. Chaos test recipe at
  `tests/integration_chaos.rs` (ignored; documents the testcontainers
  setup).

**Sprint 5 — Phase 5 close-out**

- **`src/config.rs`**: 29-entry registry of every JWC_* env var.
  Boot-time `validate_or_bail()` + rendered ASCII config table
  (gated by `JWC_PRINT_CONFIG`, default on). Redaction of
  PASSWORD / SECRET / TOKEN / KEY / JWT / DATABASE_URL in
  the rendered output.
- **OTLP optional tracing** (`src/observability/otlp.rs`) behind
  Cargo feature `otlp`. `JWC_OTLP_ENDPOINT` runtime gate;
  `JWC_SERVICE_NAME` resource attribute. W3C
  `TraceContextPropagator` global. `OtlpGuard` flushes the batch
  span processor on `Drop`.
- **Postgres-backed persistent job queue**: pluggable `JobDriver`
  trait + `enum Driver { InMemory, Postgres }` behind a `OnceLock`.
  In-memory stays the default. `JWC_QUEUE_DRIVER=postgres` switches
  to the durable driver — own multi-thread runtime + mpsc bridge to
  avoid nested-runtime panics. DDL: `_jwc_jobs` + dispatch index +
  `_jwc_jobs_dlq`. Dequeue uses `SELECT ... FOR UPDATE SKIP LOCKED`
  with a 30-second lease; `nack` moves to DLQ when
  `attempts >= max_attempts`.
- **72h soak harness** (`soak/`): `run-soak.sh` cycle driver,
  `analyze.py` PASS/FAIL gate (RSS drift ≤ 10%, lost responses == 0),
  `chaos-script.sh` SIGTERM sidecar, `.github/workflows/soak.yml`
  manual-dispatch self-hosted job.

**Error codes added (catalog @ `src/error_codes.rs`):**

- E016 nested transaction; E017 savepoint outside transaction
- E018 return type mismatch; E019 arity mismatch; E020 arg type
- E021 private function called across namespace

**Env vars added:**

- Phase 3: (none — error code only)
- Phase 4: `JWC_DB_RETRY_MAX_ATTEMPTS`, `JWC_DB_RETRY_BACKOFF_MS`
- Phase 5: `JWC_PRINT_CONFIG`, `JWC_OTLP_ENDPOINT`,
  `JWC_SERVICE_NAME`, `JWC_QUEUE_DRIVER`

**CLI additions:**

- `jwc check --no-typecheck`, `jwc run --no-typecheck`,
  `jwc build --no-typecheck`
- `jwc migrate up --dry-run`, `jwc migrate down --dry-run`
- `jwc migrate status`
- `jwc --version` long form now includes target / profile / rustc /
  git hash (carried over from v0.4.4)

**New Cargo feature:** `otlp` (gated opentelemetry / tracing /
tracing-opentelemetry deps).

Tests: 306 lib (was 251 at sprint 1, +55), 8 jwc-runtime,
35 conformance (was 25), 3 native_parity (was 1), 21 imports
(was 17, +4 visibility), 1 chaos (ignored), 1 lib ignored
(Postgres-driver smoke). All green.

Sprint 1–5 [1.0-blocker] punch list closed. Phase 6 (security
program close-out) and Phase 7+ (perf-with-receipts, DX, release
engineering) remain.

## [0.4.5] — Phase 1 unified value model: Value::Record everywhere

Performance + architectural release. Closes the Phase 1 [1.0-blocker]
Sprint 1 punch-list from `PRODUCTION_READINESS_PLAN.md`: the
interpreter and AOT both flow object-shaped values through a single
typed-shape Record carrier, shape names are deduplicated across rows,
and the value model now lives in a sibling `jwc-runtime` crate so a
future interpreter ⇄ AOT unification has somewhere to land.

Highlights:

- **`Value::Record { field_names: Arc<Vec<Arc<str>>>, values: Arc<Vec<Value>> }`**
  — the interpreter's typed-shape object variant. Object literals,
  `select` rows, `json_parse(s)` of any object, and `set_json_field`
  on a known shape all materialise as Record. Field access is O(N)
  linear scan over the shared `field_names` Arc — no JSON parse
  round-trip on `obj.field`, no per-row Vec<String> allocation. The
  `Value::Str(json_string)` fallback stays for computed-key literals
  + non-JSON `json_parse` payloads.

- **DB rows go straight to Record.** `Expr::DbSelect` eagerly parses
  the engine's JSON result via the new `materialize_select_result`
  helper: one `field_names` Arc per query, one `Vec<Value>` per row,
  N rows share the schema layout via Arc refcount. The headline
  /json-large win the production-readiness plan targets.

- **AOT mirror.** `src/native_prelude.rs.in` gains a `V::Record`
  variant with the same shape (`field_names: Arc<Vec<JwcStr>>`,
  `values: Arc<Vec<V>>`). `native_build.rs` interns each
  declaration-order key list into `CodegenCtx.shapes` and emits one
  `fn __jwc_shape_N() -> &'static Arc<Vec<JwcStr>>` getter (wrapping
  a `std::sync::OnceLock`) per distinct shape. Object literals
  become `v_record(Arc::clone(__jwc_shape_N()), vec![...])` — no
  per-construction `JwcObj::default()` + 3-7 FxHashMap inserts.

- **`crates/jwc-runtime/` sibling crate.** Extracted `Value`,
  `format_float`, `value_to_json`, `value_to_json_smart`,
  `json_to_value`, `materialize_select_result`, and the
  matching unit tests into `crates/jwc-runtime/src/lib.rs`. The
  main crate keeps a `pub use jwc_runtime::{...}` re-export so
  call sites compile unchanged. Path dep, no `[workspace]` mode
  (kept simple deliberately — the AOT-uses-runtime-as-crate
  follow-up is a separate sprint).

- **Per-request micro-fixes** (carried over from v0.4.4 close):
  `Request.response_status` is now `AtomicU16` instead of
  `Mutex<Option<u16>>`; `jwc_set_response_status()` is only
  emitted on routes whose middleware chain has at least one
  `after { ... }` block (stateless routes emit zero Phase-5
  instrumentation now).

Bench against the http-framework-benchmark suite on the same
machine (bombardier 15s @ warmup 3s):

  /json-large:  14,643 -> 15,378  (+5.0%, the targeted V::Record win)
  /async-delay: 31,108 -> 33,014  (+6.1%, reduced alloc pressure)
  /ping:        129,227 -> 129,382 (noise)
  /json-small:  125,918 -> 128,017 (+1.7%)
  /cpu:         127 -> 120        (noise on the SHA-256 bound path)

jwc-app now ~6% clear of go-fiber on /json-large (15,378 vs 14,516).
Other stacks unchanged from the v0.4.0 cross-stack snapshot.

Tests: 251 lib (8 moved out to the sub-crate), 8 jwc-runtime,
30 conformance (5 new Record cases), 3 native_parity (2 new V::Record
+ shape-dedup codegen cases). All green.

Sprint 1 of the production-readiness plan closed. Sprint 2
(decompose `runner/mod.rs` + `parser.rs`, unwrap budget walk,
cargo-fuzz CI) is next.

## [0.4.4] — Phase 5 close-out + observability bundle

Second large bundle on top of v0.4.3. Folds 30+ commits shipped in
this session that close the rest of the Phase 5 server-reliability
gate, finish the Phase 1 write-side monomorphization wiring through
the AOT codegen, and add the observability surface (Prometheus
`/metrics`, JSON access logs, `request_id` + W3C `traceparent`
propagation, response-phase `after { ... }` middleware in interpreter
*and* native).

Highlights:

- **Phase 5** — built-in `/healthz` + `/readyz` + `/metrics`,
  SIGTERM handler, `JWC_MAX_BODY_BYTES`, `JWC_REQUEST_TIMEOUT`
  watchdog with 504 envelope, `JWC_LOG_FORMAT=json` structured
  logs, `JWC_TRUSTED_PROXIES`-aware `client_ip()`, `request_id()`
  + `x-request-id` propagation, W3C `traceparent` reuse-as-id +
  `traceparent`/`tracestate` echo on response, queue drain on
  shutdown, response-phase `after { ... }` middleware (interpreter
  + native AOT), `response_status()` / `response_duration_ms()` /
  `request_id()` builtins.
- **Phase 1** — `V::RawJson` write-side fragment carrier;
  `emit_db_select` simple-select path now produces
  `JwcEnt_<Name>::jwc_from_row(r)` → `jwc_write_json(&mut buf)` →
  `V::RawJson(buf.into())`, fully skipping `V::Object` on both the
  read and the write side.
- **Phase 2** — spanned validator errors with per-file `<label>:line:col`
  + rustc snippet (single + multi-file), lint enforcement in
  `jwc build` / `jwc test`, `--deny-warnings` CI gate, did-you-mean
  hints on every `Unknown column` site, did-you-mean on native
  unknown-function errors, E011 / E012 / E013 / E014 / E015 codes.
- **Phase 4** — atomic `update CTX.Table set col = expr where ...`
  closes the lost-update race on the jwc-shortener `hits` counter.
- **Phase 3** — `substring(s, start, len)` + `take(s, n)` builtins.

CLI / DX: `jwc --version` long form prints target + profile + rustc +
git short hash. Conformance suite grew from 16 → 25 cases, each
running in an isolated 8 MiB-stack thread with its own tokio
current_thread runtime so `case_functions`-style recursive fixtures
don't flap under parallel `#[tokio::test]` pressure.

Docs: deployment env-vars reference page, k8s probes / scrape /
trusted-proxy snippet, security supply-chain section, monomorphization
wins note on the native-build page, response-phase `after { ... }`
section on the README + middleware doc, seven-step "shipping a new
builtin" recipe in CONTRIBUTING.md.

## [Unreleased]

### Added
- **W3C `traceparent` propagation.** When an upstream service sends
  a well-formed `traceparent: 00-<32-hex>-<16-hex>-<flags>` header,
  the server reuses the trace-id as `request_id()` instead of
  generating a local one. Distributed tracing across hops just
  works: a Tempo / Jaeger / Honeycomb query for the trace-id
  surfaces every JWC service it passed through. Malformed
  traceparents fall back to the local counter id (never refuse a
  request over a broken upstream header).
- **Native AOT codegen for response-phase `after { ... }` blocks.**
  Each middleware with an after-body now emits a separate
  `mw_<name>_after()` fn alongside `mw_<name>()`; the route
  dispatcher calls them in reverse middleware order after the
  handler. Interpreter shipped in v0.4.3; this slice closes the
  follow-up.
- **Native AOT `response_status()` is fully wired.** Previously a
  V::Null stub. The `Request` task-local now carries a
  `Mutex<Option<u16>>` slot that the route dispatcher populates
  between handler return and after-chain dispatch, so
  `response_status()` inside `after { ... }` blocks reads the wire
  status. Tied to a new `after_block_sees_response_status` parity
  case.
- **`jwc --version` is operator-friendly.** The long flag now also
  prints the cargo target triple, build profile, git short commit,
  and rustc version line. Short `-V` keeps emitting just `jwc 0.4.3`
  for script-friendly probes.
- **Three new diagnostic codes:** E013 (bulk `delete from CTX.Table`
  without `where`), E014 (route handler references undefined fn),
  E015 (duplicate function name in the project namespace).
- **Two new conformance cases:** `case_array_helpers` pins `range`
  edge semantics + `join` separator corners; `case_json_helpers`
  pins `json_stringify` -> `json_parse` round-trip + mixed-type
  array serialization. Conformance suite is now 25 cases.

### Changed
- **Docs:** `docs/spec/semantics.md` now pins after-block dispatch
  order (reverse), error isolation, and the timeout-skip rule.
  `docs/spec/builtins.md` pins the hash builtin family
  (sha256/sha1/md5/hmac_sha256) with output length, casing,
  null-prop, and the "not for passwords" warning.
  `docs/docs/backend/middleware.md` documents `after { ... }` with
  a runnable Telemetry example.
  `docs/docs/backend/queue.md` adds a backoff schedule table.
  `docs/docs/data/select.md` cross-links to atomic `update ... set`.
  `docs/docs/deployment/native-build.md` explains the
  monomorphization wins.
- **CONTRIBUTING.md:** a seven-step recipe for shipping a new
  builtin (interpreter, validator, native codegen, spec, user docs,
  conformance, changelog) so the v1.0 freeze can't catch a builtin
  with no test or no spec entry.

### Tests
- Lib unit tests: 243 -> 249 (six new server.rs tests covering the
  access-line JSON envelope shape, path escaping rules, text-form
  layout, and three new traceparent boundary cases).
- Conformance: 23 -> 25.

## [0.4.3] — Phase 1/2/4/5 1.0-blockers, dogfooding bundle

Twenty-six commits land together as a single release because each is
incremental and the shipping cadence in this session was per-commit
green builds. The bundle closes six 1.0-blockers across four phases:

- Phase 1 — Struct monomorphization (read + write), `V::RawJson`
  fragment carrier, `emit_db_select` now skips `V::Object` on simple
  selects. /json-large gap closed at the codegen level.
- Phase 2 — Spanned validator errors (single + multi-file),
  rustc-style snippets, lint enforcement in `jwc build` / `jwc test`,
  `--deny-warnings` CI gate, unwrap-budget policy + `[lints.clippy]`
  slot.
- Phase 4 — Atomic `update CTX.Table set col = expr where ...`
  closes the lost-update race observed live on jwc-shortener's
  hits counter.
- Phase 5 — SIGTERM handler, request body cap, /healthz + /readyz +
  /metrics built-in endpoints, client_ip() with JWC_TRUSTED_PROXIES,
  request_id() + x-request-id, JWC_LOG_FORMAT=json, queue drain on
  shutdown, response-phase `after { ... }` middleware.
- Phase 3 — `substring(s, start, len)` + `take(s, n)` builtins close
  the dogfooded `split(s, "")` workaround.

Conformance suite grew from 16 → 21 cases. Each runs in an
8 MiB-stack thread so `case_functions` and friends don't flap under
parallel `#[tokio::test]` pressure.

### Added
- **Graceful shutdown drains the background queue.** The kubelet
  TERM path used to log `draining N inflight requests` and return
  immediately. Any pending job (welcome email, sync ping) was lost on
  exit. The shutdown signal now also polls `queue::pending_count()`
  in a `spawn_blocking` task until it hits zero or `JWC_SHUTDOWN_TIMEOUT`
  fires — workers stay alive in the meantime so they keep draining.
  A leftover count is logged so operators can spot a queue that
  never drains cleanly.
- **`client_ip()` honours `JWC_TRUSTED_PROXIES`.** Walks the
  `JWC_REAL_IP_HEADER` chain RIGHT to LEFT, peeling off any entries
  whose prefix matches the comma-separated `JWC_TRUSTED_PROXIES`
  list, and returns the first untrusted entry — the original client.
  Empty / unset trust list means "trust no proxy in the chain" and
  the rightmost entry wins. Mirrors nginx + Go's `net/http`
  semantics. **Behaviour change** from the prior slice (which always
  returned the leftmost entry — spoofable when the LB doesn't
  overwrite the slot); set `JWC_TRUSTED_PROXIES` to your LB / k8s
  ingress prefix (e.g. `10.,127.0.0.1,::1`) to opt back into
  client-IP semantics. Native AOT + interpreter both updated.
- **`/metrics` exports queue depth + DLQ size.** Two more
  Prometheus gauges (`jwc_queue_pending`, `jwc_queue_dlq`) join the
  HTTP counters / gauges so operators can chart a backlog before it
  becomes an SLO breach.
- **Response-phase middleware: `middleware Name { … } after { … }`.**
  Closes the biggest jwc-shortener dogfooding gap: pre-handler
  middleware couldn't read the response, so `latency_ms` and `status`
  in their request-log table were hardcoded to 0 / 200. The optional
  `after { ... }` block now runs after the route handler, in reverse
  middleware order (mirroring Express / koa / ring), with two new
  builtins exposed:
    - `response_status()` — HTTP status the handler produced.
    - `response_duration_ms()` — ms since dispatch began.
  Errors thrown inside an `after` block are logged but don't override
  the response — by the time it runs the response has already been
  committed. Native AOT covers the parser and the dispatch side via
  the interpreter; native-codegen for `after` bodies is the follow-up.
- **Phase 1.6 — write-side monomorphization through `V::RawJson`.**
  The native runtime gains a new V variant: `V::RawJson(JwcStr)` carries
  an opaque, already-encoded JSON fragment. `jwc_write_json` writes
  the bytes verbatim; every other match arm (truthy, Display)
  treats it like a `V::Str`. `emit_db_select` for simple entity
  selects now generates `JwcEnt_<Name>::jwc_from_row(r)` →
  `jwc_write_json(&mut buf)` → `V::RawJson(buf.into())` per row,
  wrapped in a `V::Array`. The dynamic `V::Object` / FxHashMap
  allocation is GONE from the hot path — neither the read nor the
  write side touches it. This is the slice
  PRODUCTION_READINESS_PLAN.md called out as the Phase 1 1.0-blocker
  ("close the /json-large axum gap"); the benchmark run lands in the
  follow-up commit alongside the bench.sh harness update.
- **`request_id()` builtin + `x-request-id` response header.** The
  server stamps a unique id on every HTTP request (16 hex chars,
  `<wall_secs><counter>`), threads it into the runtime so middleware
  / handler / `errorHandler` all read the same value via
  `request_id()`, includes it on every response as `x-request-id`,
  and adds it to both text and JSON access log shapes (text: `(rid=…)`
  suffix; JSON: top-level `"request_id"` field). The plain
  `run_request_with_headers` entry point keeps its old shape — the
  new `run_request_with_headers_and_id(...)` is what the server uses;
  tests that don't stamp see `request_id()` as `null`.
- **Built-in `/metrics` endpoint in Prometheus text format.** The
  bundled launcher's existing `ServerMetrics` (request counts,
  in-flight gauge, running mean / peak latency) now scrapes natively
  via `/metrics`. Each metric carries `# HELP` and `# TYPE` so
  Grafana's metric explorer surfaces a description and the
  aggregator picks the right query semantics (counter vs gauge).
  Latency is exposed as seconds (Prometheus convention) — a running
  mean and a peak; bucketed histograms land alongside the tracing
  / OTel work. User precedence applies: `route GET "metrics"` in
  the program takes the slot. Closes the Phase 5 dogfooding gap
  where every project had to roll its own counters / scrape route.
- **`JWC_LOG_FORMAT=json` for structured logs.** When set, both the
  per-request access line (`jwc serve --request-logging`) and the
  runtime error log (caught by `error_report::log_runtime_error`)
  switch from the legacy `[JWC] …` / `[JWC-ERROR] …` text shape to
  newline-delimited JSON: `{"level":"info","kind":"access","method":...,
  "path":...,"status":...,"latency_us":...}` and
  `{"level":"error","context":...,"message":...,"causes":[...]}`.
  k8s log aggregators (Loki, Datadog, CloudWatch) parse this natively
  — no regex extraction, level field is first-class, the anyhow error
  chain stays addressable per index. Default stays text so existing
  scrapers and interactive `jwc run` output don't break.
- **`client_ip()` builtin with proxy-header override.** Reads
  `JWC_REAL_IP_HEADER` (default `x-forwarded-for`) from request
  headers and returns the FIRST entry of the comma-separated chain —
  the original client, not the closest proxy. Returns `null` when the
  header is absent. Closes the jwc-shortener dogfooding gap where
  rate-limit code had to hand-roll `header("x-forwarded-for")` per
  app and got Cloudflare's `cf-connecting-ip` precedence wrong;
  flipping the builtin to a Cloudflare deploy is now an env-var
  change (`JWC_REAL_IP_HEADER=cf-connecting-ip`). Native AOT and
  interpreter both ship the builtin; spec entry follows.
- **Built-in `/healthz` + `/readyz` endpoints with DB probe.** The
  bundled launcher's server now registers both routes by default:
  `/healthz` is the liveness probe (always 200 — if axum can answer,
  the process is alive); `/readyz` round-trips a `SELECT 1` against
  the configured pool and returns 503 with a short `{"db":"..."}`
  body if the DB is unreachable. The user can ship their own handler
  for either path — `route GET "healthz"` registered in the program
  takes precedence and the built-in yields. Closes the dogfooding
  gap where jwc-shortener's hand-rolled `/healthz` had no DB check,
  so kubelet probes stayed green through a database outage. No
  `DATABASE_URL` configured means `/readyz` falls back to liveness-only.
- **String builtins `substring(s, start, len)` + `take(s, n)`** — char-based
  slicing that closes the gap surfaced by jwc-shortener (where the only
  workaround was a `split(s, "")` for-loop). UTF-8 safe, out-of-range
  inputs clamp to `""`, null threads through. Native AOT covered; spec
  entry pinned in `docs/spec/builtins.md`. Both names defer to a
  user-declared function of the same name when one exists.
- **`jwc build --deny-warnings` / `jwc test --deny-warnings`** — promotes
  lint warnings to errors for CI gates.
- **Atomic `update CTX.Table set col = expr where ...`** — partial-row
  update that compiles to a single SQL `UPDATE` (no preceding read).
  Closes the lost-update race the whole-row form `update var in CTX.Table`
  has under concurrency — observed live on jwc-shortener's `hits`
  counter. Column refs (`hits`) and column arithmetic (`hits + 1`) stay
  inline in the SQL so the increment is genuinely atomic; everything
  else is evaluated host-side once and bound as `$N`. Both interpreter
  and native AOT codegen. `where` clause required; column validation
  happens at compile time.
- **Spanned validator errors** — top-level decls (DbContext, Model,
  Route, Function, Middleware, Const) now carry a byte `offset` of their
  opening keyword, and `Program` carries the original source string.
  Validator errors render as `<msg> at line X, col Y` + rustc-style
  snippet for thirteen of the most-hit sites (duplicate name/route,
  unsupported method, missing handler, …). Multi-file projects fall
  back to the bare-message shape — per-file source tracking is next.
- **`Token::end_offset`** + **`SourceMap::snippet(offset)`** — building
  blocks for span-carrying AST nodes. Parser errors already use this
  to render an in-source caret under the failing token.
- **Per-file source tracking in validator diagnostics.** `Program` now
  carries a `Vec<SourceFile>` (label + text) instead of a single
  source string, and every top-level decl records the `file_idx` of
  the file it came from. Multi-file projects now render validator
  errors as `at <relative-path>:<line>:<col>` + snippet — the previous
  slice cleared `program.source` on merge, so multi-file projects
  fell back to the bare message shape. `parse_program(src)` keeps the
  short single-file shape; `parse_program_with_label(src, label)` is
  the new entry point the project loader uses to flow file paths in.
  Single-file output is byte-identical so the LSP regex resolves.
  Ctrl+C path stays — but kubelet's rolling-deploy TERM signal no
  longer waits for the `terminationGracePeriodSeconds` ceiling to
  SIGKILL the process. The shutdown log line names which signal
  fired (`SIGINT` vs `SIGTERM`) so operators can distinguish a
  k8s deploy from an interactive Ctrl+C. Windows behaviour is
  unchanged.
- **Request body size cap.** New `JWC_MAX_BODY_BYTES` env var (default
  2 MiB) hard-caps inbound request bodies via axum's `DefaultBodyLimit`.
  Setting the var to `0` disables the cap for projects that already
  enforce a size at the proxy (nginx, Cloudflare). Without this a
  single client streaming an unbounded body could OOM the worker —
  exactly the kind of footgun the Phase 5 plan flags as a 1.0-blocker.
- **Phase 1 struct monomorphization — codegen foundation.** Every
  `entity` declared in a project now produces a concrete Rust struct
  (`JwcEnt_<Name>`) in the emitted source, alongside a `jwc_to_v`
  serializer that lifts it into the dynamic `V` enum the rest of the
  runtime speaks. Field types map column-for-column (Smallint → i16,
  Int → i32, Bigint → i64, Float → f64, Bool → bool, Timestamp/Str →
  String); nullable columns wrap in `Option<T>`. The struct is not
  yet wired onto the hot path — the next slice replaces `V::Object`
  on `select` results with these structs so JSON serialisation skips
  the FxHashMap that `/json-large` round-trips through (closes the
  axum gap documented in PRODUCTION_READINESS_PLAN.md Phase 1).
- **Phase 1.5c — `emit_db_select` wired to the typed read path.**
  "Simple" entity selects (no projection, no `with` relations) now
  generate `jwc_db_query_rows(sql, params)` →
  `JwcEnt_<Name>::jwc_from_row(row)` → `jwc_to_v()` instead of the
  dynamic `jwc_row_to_v` FxHashMap roundtrip. The Vec<V> shape
  downstream is identical, so JSON serialisation and route returns
  stay the same — this slice closes the read-side allocation, the
  write-side switchover (skip V::Object entirely) is the next slice.
  Complex paths (projection / eager-load) keep the dynamic codepath
  because the monomorphized struct has a fixed shape that doesn't
  match a partial projection.
- **Phase 1.5b — `jwc_db_query_rows` raw-row helper on the DB
  prelude.** Returns `Vec<tokio_postgres::Row>` so generated code can
  feed each row straight into a monomorphized `JwcEnt_<Name>` via
  the struct's `jwc_from_row` ctor without going through the dynamic
  `V::Object` detour. `jwc_db_query` keeps its `Vec<V>` signature for
  callers that still want the FxHashMap shape — it's a one-line
  wrapper now. Per-callsite switchover is the next slice.
- **Phase 1.5 — typed row reader + direct JSON writer on every
  monomorphized struct.** `JwcEnt_<Name>` now ships with
  `jwc_from_row(row: &tokio_postgres::Row) -> Self` (reads columns by
  declared-order index, skipping the per-row column-name lookup the
  dynamic `jwc_row_to_v` does) and `jwc_write_json(&self, out: &mut
  String)` (appends `{"col":value, ...}` straight into a String — no
  `V::Object` allocation, no `serde_json::Value` round-trip, no
  FxHashMap on the hot path). Methods are emitted on every entity
  unconditionally and marked `#[allow(dead_code)]`; the next slice
  rewires `emit_db_select` to use them and closes the `/json-large`
  RPS gap.

### Changed
- **`jwc build` and `jwc test` now run the lint pass by default** and
  surface warnings on stderr before continuing. Closes the dogfooding gap
  where jwc-shortener shipped with a declared-but-unused `RateLimit`
  middleware and nothing in the build path said a word — the warning
  existed, but only `jwc lint` (opt-in) ran it. Warnings stay advisory
  unless `--deny-warnings` is set.

## [0.4.2] — Spec scaffold, SemVer policy, release hardening

Docs + supply-chain release. No language-level behaviour change; user
`.jwc` source compiles without modification. Closes the Phase 0 and the
remaining Phase 6 quick-wins from `PRODUCTION_READINESS_PLAN.md`.

### Added
- **`docs/spec/`** — language specification scaffold. `grammar.ebnf`
  covers the top-level grammar (declarations, statements, expressions,
  routes, SQL `select`) with `TODO` markers on incomplete productions;
  `semantics.md` pins evaluation order, scope, async suspension,
  coercion, integer/float behaviour, DB and HTTP semantics, and an
  explicit "what is NOT specified yet" section; `builtins.md` defines
  the contract template (Signature / Errors / Notes / Tests) and lands
  the first batch of entries (length, replace, split, hashes, time,
  body / response / serve).
- **`SEMVER.md`** — what counts as a breaking change, what does not,
  release cadence target, pre-release suffix contract, yank policy.
- **`DEPRECATION.md`** — minimum warning window (pre-1.0 ≥ 1 minor,
  post-1.0 full minor cycle), what can/cannot be deprecated, lifecycle,
  authoring checklist (W#### code + CHANGELOG + test + spec update +
  `jwc upgrade` rule).
- **`SECURITY.md`** — private vulnerability disclosure via GitHub
  Security Advisories, 72h ack / 14d high-severity fix SLA, explicit
  in-scope/out-of-scope list, hardening notes for users.
- **`.github/dependabot.yml`** — weekly updates for cargo, GitHub
  Actions, and both npm trees (`docs/`, `vscode-extension/`), with
  minor/patch grouped.
- **README — Performance section** linking the
  [http-framework-benchmark](https://github.com/just-web-code/http-framework-benchmark)
  repo with the v0.4.x headline numbers.

### Changed
- **Release artifacts now carry `.sha256` checksums.**
  `.github/workflows/release.yml` runs `sha256sum` (Linux) and
  `Get-FileHash -Algorithm SHA256` (Windows) over each tarball/zip and
  attaches the sidecar `.sha256` to both the CI artifact and the GitHub
  Release.
- **`install.sh` / `install.ps1`** now download the `.sha256` next to
  the archive and verify it before extracting. Releases without a
  checksum (older than 0.4.2) warn and continue, so old tags remain
  installable.

### Deprecated
- None.

### Removed
- None.

### Internal
- Phase 0 conformance suite (16 cases across both interpreter and
  native AOT) shipped in `13a3cad` is now reachable from the spec docs;
  each spec entry references its conformance case.
- `PRODUCTION_READINESS_PLAN.md` Phase 0 + Phase 6 status updated to
  reflect landed vs remaining items.

## [0.4.1] — Native AOT Phase A perf

Performance-only release. No public API changes; user `.jwc` source compiles
without modification. Phase A of `PERF_PLAN.md` — closes a large chunk of the
gap to rust-axum reported in v0.4.0.

### Changed
- **`V::Object` payload** now uses `FxHashMap<String, V>` instead of
  `BTreeMap` — O(1) lookup, ~3× faster hashing on short keys. `jwc_write_json`
  sorts keys at serialisation time so JSON output stays byte-for-byte
  deterministic, and `raw_sql` keeps its alphabetic first-column semantics.
- **`V::Array` / `V::Object` payloads** are now `Arc<Vec<V>>` / `Arc<JwcObj>`.
  `Clone V` becomes a refcount bump instead of a deep copy of the whole
  subtree; mutating sites use `Arc::make_mut` (copy-on-write), consuming sites
  use `Arc::unwrap_or_clone`. `Arc` (not `Rc`) because axum tasks are `Send`.
- **`V::Str` payload** is `Cow<'static, str>`. Source literals codegen to
  `Cow::Borrowed(&'static str)` — zero per-request allocation; dynamic strings
  continue to flow through `Cow::Owned(String)`.
- **Release profile** — `opt-level = 3` (was `"z"`), `lto = "fat"` written
  explicitly. Release builds pass `RUSTFLAGS="-C target-cpu=native"` so LLVM
  emits instructions for the host's exact micro-architecture (skipped for
  cross-target builds and debug). `panic = "abort"` is intentionally NOT set —
  it would break `try {} catch {}` and `transaction {}` which depend on
  `catch_unwind`.
- **`mimalloc` global allocator** on Windows targets (replaces `HeapAlloc`,
  the dominant source of allocator churn). Linux / macOS keep the system
  allocator.
- **Pre-sized buffers** — `jwc_to_json` seeds the output `String` with
  `String::with_capacity(256)`, `jwc_write_json_string` reserves
  `s.len() + 2` up front.

### Fixed
- **Allocator-free hex encoding** in `jwc_hash_to_hex` — replaces the
  per-byte `format!("{:02x}", b)` (32 tiny `String` allocs per SHA-256) with
  a direct table lookup. Hot enough on chained-hash workloads to dominate
  per-request time on the `/cpu` benchmark.

### Performance

Measured on Intel i5-10400 / 32GB / Win11 with `_my/jwc-app` from
`http-framework-benchmark`, release native, bombardier 15s @ warmup 3s:

| Endpoint | v0.4.0 baseline | v0.4.1 | Δ |
| --- | ---: | ---: | ---: |
| `/ping` | 123,256 | 133,024 | **+7.9%** |
| `/json-small` | 117,729 | 129,032 | **+9.6%** |
| `/json-large` | 13,064 | 13,900 | **+6.4%** |
| `/cpu` | 68 | 123 | **+81%** |

`/cpu` closes the rust-axum gap from 2.80× to 1.55× — already exceeds the
`B5` target of "68 → 110+ RPS" listed for the next phase. `/async-delay` is
dominated by TCP-accept-queue saturation at `c=1000` and the 32-bit
bombardier client; at `c=100` it runs cleanly with zero errors.

## [0.4.0] — Array + Builtin Parity

### Added
- **Array literals** — `[1, 2, 3]`, the empty form `[]`, and heterogeneous
  elements (`[1, "two", true]`). Iterable with `for x in xs`. Works in both the
  interpreter and native AOT.
- **Array builtins** — `range(n)` / `range(start, end)` / `range(start, end,
  step)`, `push(arr, x)` / `append(arr, x)` (in-place), and `join(arr, sep)`
  (O(n)). `length`/`first`/`last`/`contains` now accept arrays directly.
- **Hash builtins** — `sha256`, `sha1`, `md5`, and `hmac_sha256` (lowercase
  hex), backed by a new `src/hash.rs` with known-vector tests (incl. RFC 4231).
- **Custom MIME responses** — `response(body, mime)` (alias `raw`) ships a body
  verbatim under an explicit Content-Type (`; charset=utf-8` appended to
  `text/*`). `text(body)` now works in the interpreter too.
- **Module-level `const`** — top-level `const NAME = expr;` visible read-only in
  routes, functions, middlewares, and main; compile-time rejection of
  non-constant expressions, undeclared references, duplicates, and cycles.
- **Graceful shutdown** — `serve()` drains inflight requests on Ctrl+C with a
  `JWC_SHUTDOWN_TIMEOUT` (default 5s) watchdog; open WebSockets get a `1001`
  close frame (interpreter).

### Changed
- Built-in metadata consolidated into a single source of truth
  (`src/builtins.rs` `BUILTIN_DEFS`); the native-AOT whitelist and lint pass
  derive from it. The interpreter's built-in evaluators were split into
  `src/runner/builtins.rs`.

### Fixed
- Native AOT now accepts `hash_password` / `verify_password` (argon2id) — they
  were previously interpreter-only and rejected at native-build time.
- `ok`, `not_found`, `no_content`, `bad_request`, and `internal_error` no longer
  error with "Unknown function" in the interpreter; they are dispatched in both
  runtimes. (Remaining error-body shape differences are tracked in
  `docs/parity-notes.md`, deferred to v0.4.1.)

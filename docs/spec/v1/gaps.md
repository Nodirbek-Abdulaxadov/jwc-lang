# JWC redesign — tasdiqlangan kamchiliklar

138 topilmadan 44 tasi adversarial tekshiruvdan o'tdi.

## BLOCKER

### No join alias — the same table cannot be joined twice, so self-joins are inexpressible
**Soha:** joins

Join predicates are written against the declared table name (`Members.org_id == Orgs.id`) while the projection key comes from `as one X` / `as many X`. The predicate side has exactly one namespace — the table name — so the moment a query touches the same table twice, both bindings are spelled identically and the `on` clause is unresolvable. DESIGN.md never mentions aliasing and the sample never self-joins, so the hole is invisible. Related and also unstated: the pre-`from` slot is inconsistent (`select Accounts from App.auth.Accounts` for tables, `select from App.billing.SubscriptionDetail` for views) — its purpose is never defined.

**Taklif:** Make the alias the binding rather than the table name: `left join App.auth.Accounts inviter on inviter.id == Invites.invited_by as one inviter`, or let `as one inviter` introduce the name and require predicates to use it. Then `Accounts.` is never a binding and the flat-namespace collision disappears. Settle the pre-`from` slot in the same change: either it is the root binding's alias (mandatory, including for views, so `where` can qualify) or it is deleted.

### `where col == param` with matching names is ambiguous, and it silently drops tenant scoping
**Soha:** queries/scoping

View queries use unqualified column names, and JWC's naming rules force an id-carrying parameter to be named exactly like the column, so both sides of a comparison are spelled identically. If the resolver prefers columns, this is `WHERE org_id = org_id` — a tautology for non-null rows, and every tenant boundary in the app evaporates. If it prefers locals, it is `$1 = $1`. There is no disambiguating syntax: view queries have no root binding to qualify with, and no sigil marks a local. This appears three times in ~1100 lines of sample, twice on security-critical paths.

**Taklif:** Pick one and enforce it at compile time. Cleanest: a bare identifier in `where` is always a column, and locals/params require a sigil (reuse `@`, or add one). Second-best: make a `where` comparison whose two sides resolve to the same column a hard error, and give view queries a root binding name so `where SubscriptionDetail.org_id == org_id` is writable.

### `left join ... as one` with no match: null object vs object of nulls is undefined, and there is no null-safe field access
**Soha:** joins/projections

`as one` over a `left join` must produce something when the right side misses. `json_build_object('id', p.id, ...)` yields `{"id":null,...}`; `CASE WHEN p.id IS NULL THEN NULL ELSE json_build_object(...) END` yields `null`. Every API consumer branches on the difference and DESIGN.md says nothing. The JWC-side type is equally unspecified: if a projected record can carry a null nested record, `invoice.subscription.status` can fault, and the language has no `?.` and no null-propagation rule through a projection.

**Taklif:** Specify that `left join ... as one` produces SQL NULL when the join key is null or no row matches (emit the `CASE WHEN <right pk> IS NULL` guard), and type the field as a nullable record. Then require null-safe access or a null check before any field read, matching how the sample already gates `first` results. Also state whether an `inner join` exists at all — the sample uses `left join` for 100% of joins, including semantically-inner ones over NOT NULL FKs.

### Path parameters have no declared type, and `@id` coercion/failure behaviour is undefined
**Soha:** path params / routing

`@org_id` and `@id` are used in routes, middleware and service calls, but nothing declares their type anywhere. A URL segment is a string; it is passed through an untyped service parameter into `where id == invoice_id` against a `bigint` column, so no inference is possible even in principle. Three behaviours are undefined: what SQL type the value binds as, what happens on a non-numeric segment (`/invoices/not-a-number`), and whether the failure surfaces as 400, 404, or a Postgres `operator does not exist: bigint = text` 500. It also leaves the router unable to know that `{org_id}` should only match digits, which feeds the route-collision problem.

**Taklif:** Type the parameter at its only declaration site — the path pattern — and make coercion failure a routing miss rather than a handler error: `routes "/api/v1/orgs/{org_id: bigint}/invoices" { route GET "{id: bigint}" { ... } }`. Allowed types are the scalar column types already in the language plus declared enums; an untyped `{id}` is a compile error, not a `varchar` default.

### No way to set a response header — `Location`, `Retry-After`, `X-Request-Id` are all unexpressible
**Soha:** response construction

The enumerated response verbs (`json`, `created`, `notFound`, `badRequest`, `unauthorized`, `forbidden`, `noContent`, `statusCode`) take a body and a status and nothing else. There is no header setter on any of them and none on the response object visible in `after` blocks. So 201-with-`Location`, 429-with-`Retry-After` (which the sample's own StrictRateLimit needs), `Content-Disposition`, `X-Request-Id`, and every cache header are unreachable — and `after` middleware, which can read `response.status()`, cannot decorate the response at all, which is what after-middleware normally exists to do.

**Taklif:** Add a `with { }` header suffix valid on every response builder (`return created(json(org)) with { "Location": "/api/v1/orgs/" + org.id };`) plus a `response.set_header(...)` usable inside `after` blocks. Keep repeating headers such as `Set-Cookie` out of the map shape.

### Raw result forwarding makes envelope pagination unexpressible, and there is no offset/cursor/total
**Soha:** pagination

Two designed rules collide. Raw is the default result and 'reading a field of a raw value is a COMPILE ERROR', while the query language offers `limit` and `first` but no `offset`, no keyset helper, and no count. A list endpoint therefore returns a bare array with no way for a client to reach page 2 or know that more exist, and the standard fix — `json({ data: invoices, total: total, next_cursor: cursor })` — requires placing a raw value inside an object literal, which the spec never says is legal. If it is illegal, envelope pagination cannot be written; if it is legal, the compiler must splice unparsed JSON into a literal, which is a substantial unspecified feature. Separately, `limit` comes straight from `int(request.query("limit") ?? "50")` with no cap.

**Taklif:** Answer the composition question explicitly in the spec — whether a raw value may appear as a field value in an object literal — since it is load-bearing well beyond pagination. Then make pagination a query form so the envelope is produced by Postgres rather than hand-assembled: `... orderby issued_at desc page after @cursor size page_size max 200;` emitting `{data, next_cursor, has_more}` as one raw payload with the cap enforced at compile time.

### Route conflicts and duplicates across `routes` blocks are undetectable, and precedence is unspecified
**Soha:** routing

`routes` cannot nest, so the same prefix is repeated verbatim across many blocks — `/api/v1/orgs/{org_id}` appears in five blocks across two files in the sample — and there is no module system, so every file contributes to one flat namespace. DESIGN.md states no precedence rule at all, so nothing decides: two blocks in different files declaring the same method+path, a literal segment competing with a parameter segment (`/api/v1/orgs/current` vs `/api/v1/orgs/{org_id}`), or the same URL slot being spelled `{org_id}` in one file and `{orgId}` in another, which silently breaks the middleware that reads `@org_id`.

**Taklif:** Three rules enforced by `jwc check`: duplicate `(method, resolved_path)` anywhere in the program is a hard error naming both sites, never last-wins; a literal segment beats a parameter segment at the same depth and a fully shadowed route is an error; and a parameter occupying the same URL slot must use the same name and type in every block that spans it.

### Middleware silently depends on path parameters it never declares
**Soha:** middleware

`RequireOrgMember` reads `@org_id`, but its declaration says nothing about requiring a path parameter of that name or type. Middleware is attached by name in a `use` list on a `routes` block whose path the middleware author never sees, and nothing checks that the path actually contains `{org_id}`. Attaching `RequireOrgMember` to `routes "/api/v1/me"` has undefined behaviour — compile error, null, or a 500 deep inside a SQL bind. The sample reuses this middleware across five route groups in two files, so the invisible coupling is real.

**Taklif:** Put the requirement in the middleware signature and check it at every attach site: `middleware RequireOrgMember(@org_id: bigint) { ... }`, making attachment to a path without a matching typed parameter a compile error naming both sites.

### Middleware ordering, group-vs-route `use` composition, and after-block behaviour on short-circuit are all undefined
**Soha:** middleware

Three unstated semantics. (1) When a group has `use A, B, C` and a route inside has `use D`, is the chain `A,B,C,D` or does the route list replace the group list? The sample expresses the same intent both ways — `RequireOrgAdmin` route-level in orgs.jwc, inline in the group list in the invites block — so the answer is load-bearing and never given. (2) Do `after` blocks run in declaration or reverse order? (3) Does `Audit`'s `after` block run when `RequireAuth` short-circuits with 401, which its `status >= 400` branch clearly assumes? The first is a security question: `RequireOrgAdmin` reads `role` from context written by `RequireOrgMember`, so a replace-semantics answer produces an unauthenticated admin PATCH rather than an error.

**Taklif:** Specify all three and make the dangerous one impossible: route-level `use` always appends to the group list and a name repeated from the group list is a compile error; `after` blocks run in reverse attach order; `after` runs for every response the chain produces including short-circuits, with `response.status()` reflecting what will be sent. Add a declared dependency (`middleware RequireOrgAdmin requires RequireOrgMember`) so the ordering mistake is caught at compile time.

### `request.client_ip()` has no proxy-trust rule, making the rate limiter either bypassable or self-DoSing
**Soha:** request introspection / security

`request.client_ip()` keys both rate limiters and is written to the audit log, but DESIGN.md never says whether it reads the socket peer address or an `X-Forwarded-For`/`Forwarded` header, and there is no configuration surface anywhere where a trusted-proxy list could live (the only config block is `database App`). Both readings are broken: unconditional XFF trust defeats the 5-per-300s login limiter with a single spoofed header, while ignoring XFF means every deployment behind a load balancer sees one IP, so the limiter locks out all users behind a shared egress and the audit log records the balancer for every event.

**Taklif:** Split the concepts and require the deployment to declare its trust boundary before the header is honoured: `request.peer_ip()` always returns the socket address; `request.client_ip()` returns the rightmost forwarded entry outside a declared `trusted_proxies` set and falls back to the peer address, returning the peer address verbatim when no trusted proxies are configured.

### The raw-vs-record rule is defined only for table projections, so field access on view selects and builtin returns is undecidable — the sample's auth gate violates the rule
**Soha:** raw/record classification, field access

DESIGN.md makes its strongest static claim — "Default result is RAW (row_to_json ... forwarded to the response with zero parsing). Reading a field of a raw value is a COMPILE ERROR" and "`as { ... }` is the ONLY way to get a record" — but classifies only `select <Alias> from App.x.Table ... as { }`. Two other major producers of values that are then field-read have no stated classification: (a) `select from App.x.SomeView` with no `as` clause, and (b) builtin returns such as `jwt.verify(...)`. With no classification the checker cannot decide whether `x.field` is legal, so the flagship guarantee is undecidable for most real field reads in the canonical sample. Note `context.get` is separately excused by the Known-invented section; the view and builtin cases are not.

**Taklif:** Make the classification total: give every value a position in a two-point lattice `Raw | Record{fields}` and state the producer rules. `select from <view>` yields `Record` typed by the view's declared projection — a view IS a named `as { }`, so this is consistent with "projection is the only way to get a record" without a special case, and it also makes a view rename a compile error at every reader. Give every builtin a declared result shape in the builtin table (`jwt.verify -> Record{sub, exp, iat}?`, or an opaque `Claims` with named accessors). Leave `jsonb` columns and `context.get` as `Raw` so field access on them is the compile error the spec advertises, requiring an explicit coercion. Then re-check the sample: every `x.field` must type-check or be rewritten.

### View queries have no alias binder, so bare column names and locals share one namespace — `where org_id == org_id` is ambiguous between a tenancy filter and a tautology
**Soha:** name resolution in queries

Table queries bind an alias (`select Accounts from App.auth.Accounts`) and therefore qualify columns (`where Accounts.email == req.email`). View queries bind nothing (`select from App.billing.InvoiceDetail`), so their `where` and `orderby` clauses use bare column names — and bare identifiers are simultaneously the local/parameter namespace. When a parameter is named after the column it filters on (the natural naming, and what the sample does at every site), `where org_id == org_id` has two readings — `col = col` (a tautology matching every row in the table) or `col = $param` — and the design provides neither a disambiguating syntax nor a type rule that could pick, since both sides have the same type. The tautology reading returns another tenant's rows with a 200.

**Taklif:** Require an alias binder on view queries exactly as on table queries (`select Sub from App.billing.SubscriptionDetail where Sub.org_id == org_id`), then make bare identifiers inside a query resolve only to the local scope — one namespace per side, no shadowing rule needed. As a transition, make an unqualified identifier that resolves to both a column and a local a hard error naming both (`E: 'org_id' is ambiguous between column SubscriptionDetail.org_id and parameter org_id`), which catches every affected site.

### `?` is a DDL marker only — nullability introduced by `first`, LEFT JOIN and aggregates never propagates, so the sample serves 200 `null` where it means 404 and types a NULL aggregate as `int`
**Soha:** nullability propagation, flow typing

DESIGN.md defines `?` solely as a column-declaration marker ("`?` = nullable; NOT NULL is default"). Nothing makes it a type constructor that propagates, yet three constructs manufacture nullability that no column carries: `first` turns `T` into 0-or-1 rows; `left join ... as one x` makes the nested object itself absent; and SQL aggregates diverge per function (`count` returns 0 on an empty group, `sum`/`min`/`max` return NULL even over a NOT NULL column). With no propagation there is no rule forcing a null check before a field read and no rule forcing a route to handle the empty case — the sample checks by hand in some places and forgets in others, and nothing distinguishes them.

**Taklif:** Make `T?` first-class with stated propagation: `first : T -> T?`; `left join ... as one a` makes the projected object `?`; `inner join` and `as many` stay non-null (empty array); `count -> int`, `sum|min|max|avg -> T?` regardless of source column nullability. Add flow narrowing so `if (x == null) { throw|return }` narrows `x` in the continuation (requires typing `throw`/`return` as diverging). Reading a field of a `T?` without narrowing is an error, and — the half that catches the route side — `json(x)` where `x : T?` must also be an error, discharged by promoting the spec's proposed `... first or throw NotFound("...")` from proposal to the required mechanism.

### NOT NULL-by-default makes zero-value backfill the common path, and the generated DDL either aborts or writes corrupt data
**Soha:** schema-evolution/nullability

DESIGN.md:43 inverts the usual default: `?` is nullable, NOT NULL is implicit. So every added column is NOT NULL unless the author remembers a `?`, which turns the engine's last-resort backfill (`zero_value_for`, src/schema_diff.rs:904 — its own doc comment calls it a rare-case escape) into the default behaviour of the most common migration there is. It collides destructively with three other things the design puts on the same column: `unique`, `pattern(...)`/`minLength(...)` CHECKs, and `foreign key`. It also has a silent hole: `zero_value_for` falls through to `"NULL"` for any unrecognised type, including array types.

**Taklif:** Refuse to generate a NOT NULL ADD COLUMN unless the column declares an explicit `default` or the table is provably empty. Otherwise emit the two-migration expand form (add nullable now; `SET NOT NULL` in a later migration once the source drops the `?`) with a comment naming the backfill the developer owes. Never guess a zero for a column carrying unique/check/FK. Make the unknown-type fallback a hard generation error instead of `DEFAULT NULL`.

### Views are first-class DB objects with no snapshot and no dependency phases, so they veto ordinary ALTERs on the tables beneath them
**Soha:** schema-evolution/views

DESIGN.md:20 makes `view X of App.billing { select ... }` a real database object and the sample declares five of them. Postgres refuses to alter a column a view depends on, so a migration touching such a column is not a flat statement list at all — it is a three-phase plan: DROP dependent views in reverse dependency order, ALTER the tables, re-CREATE the views in topological order. Neither DESIGN.md nor the engine has any notion of phases, view snapshots, or a dependency DAG (`read_latest_snapshot`, src/schema_diff.rs:217, parses only CREATE TABLE/ALTER/CREATE INDEX). `CREATE OR REPLACE VIEW` is not an escape: it only works when the output column list and types are unchanged, so any projection edit forces DROP/CREATE, cascading to views built on views.

**Taklif:** Model views as snapshot objects carrying compiled SQL text plus their referenced-table set, build a tables->views->views DAG, and make emission phased: CREATE SCHEMA, DROP VIEW in reverse topological order for every view whose transitive dependencies are touched, table DDL, then CREATE VIEW in topological order. Always drop-and-recreate rather than trying to prove CREATE OR REPLACE is safe.

### Partial unique/index predicates are not in the diff model, so adding or removing a `where` clause silently ships nothing
**Soha:** schema-evolution/indexes

DESIGN.md:52-55 defines four related forms that map onto two different Postgres object classes — a partial unique cannot be a table CONSTRAINT, it must be `CREATE UNIQUE INDEX ... WHERE`. The diff carries only `AddCompositeUnique`/`DropCompositeUnique` comparing column lists (`same_cols`, src/schema_diff.rs:680) with no predicate anywhere in the snapshot, so adding or removing a `where` on an existing `unique` produces an empty diff. A second, harder half: diffing predicates means canonicalizing expressions — `where revoked_at == null` must lower to `IS NULL` (a naive `= NULL` yields a live but permanently empty index) and `where status == InvoiceStatus.open` lowers differently depending on whether the enum is CHECK-backed or a real type.

**Taklif:** Replace the column-list-only model with `UniqueSnapshot { columns, predicate: Option<CanonicalExpr> }` and `IndexSnapshot { columns, predicate, unique }`; choose CONSTRAINT vs UNIQUE INDEX on `predicate.is_some()` and treat a predicate change as drop-and-recreate. Canonicalize predicates at compile time (null comparisons to IS NULL, enum literals to their physical form) so comparison is meaningful, and validate at compile time that composite FK targets resolve to a non-partial unique or PK.

### Typed-enum evolution is inexpressible: ADD VALUE cannot run in the transaction every migration file is wrapped in, and DROP VALUE does not exist
**Soha:** schema-evolution/enums

DESIGN.md:22 gives `enum X of App.billing { ... }` a real `CREATE TYPE`, and the sample declares five. Three Postgres facts collide with the runner: (a) `ALTER TYPE ... ADD VALUE` cannot be used in the transaction that added it and is illegal inside a transaction block on older servers, but `run_migration_file` (src/migrate.rs:710) wraps every file in BEGIN/COMMIT and the only opt-out, `file_opens_transaction` (src/migrate.rs:164), is inverted — it skips the wrapper only when the file opens its *own* transaction, so un-transacted DDL cannot be requested at all; (b) `ALTER TYPE ... DROP VALUE` does not exist, so removing a label needs create-new-type/ALTER-every-column-USING/drop-old plus a cross-schema map of every column of that type; (c) label order is ORDER BY order, so reordering the source list is a semantic change a set-based diff cannot see.

**Taklif:** Add an `EnumSnapshot { name, schema, ordered_values }` class and classify enum diffs into three named ops: append-only (ADD VALUE emitted into its own file marked `-- jwc:no-transaction`), rename (ALTER TYPE RENAME VALUE, gated on an explicit source-level `was` marker so it is not read as remove+add), and reorder/remove (the four-statement rebuild plus a guard `SELECT count(*)` that aborts when rows still hold the removed label). Teach `run_migration_file` a `-- jwc:no-transaction` header directive.

### Renames are inferred, never declared, so every rename becomes DROP COLUMN + ADD COLUMN — silent total data loss that exits 0
**Soha:** schema-evolution/renames

DESIGN.md:34-36 makes the physical name a function of the declared name plus an optional `as "physical"` override, and the diff keys on the physical name only. A rename and a drop+add are therefore indistinguishable, and the generated migration is always the destructive interpretation — for columns, for tables, and (via `schema` declarations) for whole schemas. The override makes this worse than in a conventional tool, because it lets a developer change a string that reads as purely cosmetic. A secondary hazard rides along: `"createdAt"` is a mixed-case quoted identifier while the snapshot path lowercases through `normalize_sql_type`, so case-only distinctions collapse.

**Taklif:** Renames must be declared, not inferred: either a source-level `created_at timestamptz was "createdAt"` marker consumed once and then removable, or a `jwc migrate rename <table>.<col> <new>` command that writes the `ALTER ... RENAME` and records it in the snapshot. Independently, make the diff refuse to emit a paired DropColumn+AddColumn of compatible types on one table without `--allow-destructive`, printing the rename it suspects.

### Bare identifiers in `where` have no specified resolution rule — column vs. local shadowing makes the sample's own tenancy and auth filters tautologies
**Soha:** queries / name resolution

DESIGN.md shows two query spellings — aliased (`where Accounts.email == req.email`) and bare (`where org_id == org_id`) — and never states how an unqualified identifier resolves when it matches both a column of the queried relation and an in-scope local or parameter. The design's own naming rule makes columns, params and locals all snake_case, and services are written to take params named after the columns they filter on, so the collision is the default shape rather than an accident. Under either resolution (column wins → `WHERE org_id = org_id`; local wins → `WHERE $1 = $1`) the predicate is a tautology. Views compound it: no view query in the sample uses an alias form, so the unambiguous spelling is not even available for the relations carrying the tenant key.

**Taklif:** Make the collision a compile error rather than picking a winner: inside a query body, unqualified identifiers resolve to columns of the queried relation only, and referring to an outer local requires a sigil or qualifier (e.g. `$org_id`, mirroring the existing `@org_id` path-param sigil). Give views an alias form so `select MemberAccess from App.org.MemberAccess where MemberAccess.account_id == $account_id` is writable. Until that lands, reject any `where` in which both operands resolve to the same column. Then fix the six sites in the sample, since it is the ground-truth artifact.

### `private` is contradicted by the projection syntax and by `view` — the sample's own login query names a private column
**Soha:** schema / visibility

DESIGN.md defines `private` as 'never in responses, never from body', but neither half is enforced by the marker. The write half is done by the class whitelist, not by `private`. The read half is defeated by `as { }`, which the design calls the ONLY way to get a record: naming `password_hash` in a projection yields an ordinary value that `json()` will serialise, and the sample's `AuthService.login` already does exactly this (`as { id, password_hash }`), so making it an error is not open either. `view` launders the marker further — a view is stated to be a real database object, so a view projecting a private column produces a physical column with no marker, which the RAW default path then forwards untouched. The RAW default itself is unresolved: `row_to_json` either includes private columns (leak on every unprojected query) or the compiler must emit an explicit column list, which changes what the DBA test predicts from the schema file.

**Taklif:** Redefine `private` as 'excluded from the RAW/`row_to_json` default and from any `view` projection; naming it in a view is a compile error', and add an explicit greppable read escape for the auth path (`as { id, password_hash unsafe }`, or a `secret` type whose only legal consumers are `hash.*`/`jwt.*` and which `json()` refuses). Then specify the RAW path normatively — state that the default query emits an explicit column list omitting private columns, and show the emitted SQL so the DBA test still holds.

### `...req` spread's whitelist claim rests on three unstated rules: unknown-key retention, non-class spread sources, and silent field/column intersection
**Soha:** input / mass assignment

DESIGN.md asserts 'The class is the whitelist, so spread cannot reach `private`/`server` columns.' That holds only under three preconditions the design never commits to. (1) Whether `request.body() as Register` is strict or lenient, and if lenient whether the resulting value is projected down to the declared fields or retains the original keys — mainstream binders are lenient-and-retaining, under which `...req` splats unknown keys and the whitelist claim is false. (2) Spread is defined syntactically with no restriction to class-typed operands, so nothing forbids `insert into T { ...request.body() }`. (3) The sample REQUIRES spread to be a silent intersection: `Register` declares `password`, `Accounts` has no `password` column, and `insert into App.auth.Accounts { ...req, password_hash = ... }` is the canonical example — which means the write set of every insert and update is invisible at the call site and silently re-opens whenever a column name and a class field name later coincide.

**Taklif:** State all three rules normatively. Make `as <Class>` a projection as well as a validation (unknown keys dropped, or 400 — pick one and write it down). Require the operand of `...` to be statically class-typed, making `...request.body()` and `...context.get(...)` compile errors. Make the intersection explicit: a class field with no matching column in the spread target is a compile error, with `password` handled by a non-persisted/`transient` field marker or an explicit `...req except password`. That turns every future column/field collision into a build failure instead of a privilege escalation.

### Composition order of block-level and route-level `use` is unspecified, and the sample's admin gate depends entirely on it
**Soha:** routes / middleware

DESIGN.md introduces both block-level `routes "..." use A, B` and route-level `route PATCH "" use C`, and never states whether route-level middleware runs before, after, or interleaved with block-level. The sample's authorization depends on the answer: `RequireOrgAdmin` is a pure consumer of `context.get("role")`, which `RequireOrgMember` writes, and the two are attached at different levels (orgs.jwc:20/28, 35; billing.jwc:18/26, 33). Under 'route-level first' ordering, `role` is null, both `is_owner` and `is_admin` are false, and every admin route 403s forever; under 'block-level first' it works. Nothing declares the dependency — it exists only as a matching string key — so `use RequireAuth, RequireOrgAdmin` without `RequireOrgMember` is also writable and currently fails closed only by luck.

**Taklif:** Define middleware order normatively in DESIGN.md, not in the implementation: block-level list in written order, then route-level list in written order, then `after` blocks in reverse. Let a middleware declare its prerequisites (`middleware RequireOrgAdmin requires RequireOrgMember { ... }`) and check the chain statically at each route, which also removes the untyped `context` string key from the security-critical path.

### Nothing lets a developer see the SQL a query or route produces, and raw-by-default removes the printf fallback
**Soha:** Debugging / observability

The north star is that 'the SQL produced is SQL they could have written by hand', and the DBA test is an acceptance criterion, yet no surface in the design exposes the generated SQL: no hover-SQL, no explain command, no dev query log, no per-request trace. The raw-by-default rule makes this worse rather than neutral - reading a field of a raw value is a compile error, so a developer cannot inspect an unprojected result in-process either. A wrong response body from a view-backed read (e.g. BillingService.invoices reading InvoiceDetail, which carries two many-joins) is therefore uninspectable by any means the language provides, and BillingService.subscribe returns a raw insert with no as { } so there is literally no expression that can look at it.

**Taklif:** Add three cheap surfaces: (1) LSP hover over a select/insert/update rendering the generated SQL with $n placeholders and the join strategy - this makes the DBA test verifiable in the editor; (2) 'jwc explain <Service.function>' and 'jwc explain --route "GET /api/v1/orgs/{org_id}/invoices"' printing SQL plus EXPLAIN against the dev DB; (3) JWC_LOG_SQL=1 under 'jwc serve' logging SQL, bound params, duration and row count per request, plus a dev-only /__jwc/queries endpoint. Add a dev-only debug.dump(x) permitted on raw values (rejected outside 'jwc serve --dev') so the raw optimisation stays traceable.

### Constraint-message to 400 mapping is undefined for message-less constraints and for every foreign key
**Soha:** Error model / lint

DESIGN.md states only that 'constraint violations with a message become 400 automatically'. It says nothing about constraints without a message - which is most of them in the sample: Plans.code, Invoices.number, Sessions.token_hash, ApiKeys.key_hash, Invites.token_hash - and foreign keys have no message syntax anywhere in the grammar, so FK violations can never carry one. Those fall through to the catch-all arm, i.e. a blank 500 for what is nearly always a client error (a stale plan_id on insert into App.billing.Subscriptions raises 23503 and matches no catch arm). The mechanism also requires mapping a Postgres constraint name back to a declaration, and constraint naming is nowhere specified - which additionally makes migration diffs unstable, since an unstable generated name means every 'jwc migrate new' drops and recreates the index. Partial indexes such as unique (org_id) where status != SubscriptionStatus.canceled must be attributed to that specific index, not the table.

**Taklif:** Publish the constraint-naming function as part of the DBA-facing contract (e.g. <table>_<cols>_uniq, <table>_<cols>_fkey, <table>_<n>_check) so names are predictable and stable across diffs, and compile a name-to-message table into the binary. Add lint rules that surface the gap: warn when a unique or FK constraint reachable from a write has no message and will surface as a 500, with 'jwc lint --constraints' printing every constraint reachable from a route and its resulting status code. Decide FK message syntax (foreign key (plan_id) references ... : "tarif topilmadi") or state explicitly that FK violations are always 500 and services must pre-check.

### No pagination primitive — `limit` truncates, and there is no way to reach page 2
**Soha:** query language / runtime API contract

The clause vocabulary is exactly `first`, `limit`, `orderby`, `group by`. There is no `offset`, no keyset/cursor form, and no way to return a total or a next-cursor alongside rows. A `limit` therefore returns the first page and permanently hides the rest — the 51st invoice in `BillingService.invoices(org_id, status, limit)` (`limit page_size` where `page_size = limit ?? 50`) is unreachable through the HTTP API. Every collection endpoint in the sample has this property, and the ones that omit `limit` entirely (`OrgService.members`, `OrgService.invites`, `BillingService.plans`, `AuthService.orgs_of`) are simply unbounded instead. Known-invented names `??`, `context.get/set`, aggregate spelling and transaction semantics — it does not name pagination.

**Taklif:** Make keyset pagination a first-class clause rather than reaching for `offset`, which is O(skipped) in Postgres and degrades exactly as a tenant's data grows: `orderby issued_at desc, id desc after (@cur_issued_at, @cur_id) limit 50`. Answering this forces the design to also state how a cursor gets back to the caller, which is the envelope question the raw-path finding raises — the two should be decided together.

## MAJOR

### Bare joins feeding aggregates are an undocumented third mode, and their fan-out has no `distinct` repair
**Soha:** aggregates

The sample uses three join modes — `as one`, `as many`, and a *bare* join with no cardinality keyword feeding aggregates under `group by` (src/views/billing.jwc:43). The bare mode appears nowhere in DESIGN.md. Mixing modes is undefined: `as many` compiles to a lateral producing one row per parent, while a bare join produces fan-out rows collapsed by grouping — putting both in one query means the aggregate counts over the lateral or the lateral survives grouping, and neither is specified. Worse, two bare joins under one `group by` multiply the aggregates, and the standard repair `count(distinct x)` has no spelling: DESIGN.md lists only `count`, `sum`, `min`, `max`.

**Taklif:** Document the bare join as a first-class third mode with its own keyword so the cardinality is readable off the page. Add `count(distinct x)` and `avg`. Then either forbid mixing bare-join aggregation with `as many` in one query with a clear diagnostic, or define `as many` as always-lateral and let aggregates reference the lateral by alias (`count(lines)`), which is the shape users will guess.

### No ordering or limiting inside `as many`
**Soha:** joins

A `many` child collection has no ordering guarantee — `json_agg` over a lateral returns whatever order the subplan produced — and no way to cap size. Every nested collection in every response therefore has nondeterministic order, and any unbounded collection is a latent response-size bomb. Invoice line order is semantically meaningful (it is what the customer sees) and is currently whatever Postgres feels like, potentially differing between two calls after a row update.

**Taklif:** Allow `orderby` and `limit` as suffixes on an `as many` join, compiling into the lateral's ORDER BY / LIMIT (not `json_agg(... ORDER BY ...)`, so the limit works too): `left join Payments on ... as many payments orderby Payments.created_at desc limit 5`. Consider making `orderby` mandatory on `as many` — an unordered collection in a JSON response is a bug the language can refuse to emit.

### `delete` returns nothing — no RETURNING, no row count, so "404 if it did not exist" is unwritable
**Soha:** writes

`insert` and `update` both accept `as { }`, and `update` supports `first` so a null result signals no-match. `delete from ... where ...` has neither, in DESIGN.md or the sample. There is no way to know whether a delete affected any rows, so every DELETE endpoint returns success for a nonexistent resource — including one belonging to a different tenant — and "return the deleted row" or "audit what was deleted" is impossible.

**Taklif:** Give `delete` the same tail as `update`: `as { ... }` for RETURNING and `first` for a one-row result, with null meaning nothing matched. The 404 idiom then becomes identical to `update`'s and a whole class of silently-wrong endpoints disappears.

### `update set ...req` with an all-absent body emits an empty SET clause
**Soha:** writes

`...req` spread into an `update` sets the fields present in the request. DESIGN.md specifies `set col =? maybe_null` for skipping an individual absent field but says nothing about what the *spread* does when every field is absent — which for a PATCH DTO whose fields are all optional is a body of `{}`, entirely legal input that passes validation. The generated SQL is `UPDATE t SET WHERE ...`, a syntax error: a 500 on a well-formed request.

**Taklif:** Define the empty-spread case explicitly. Best: skip the statement and return the current row via the `as { }` projection, since a no-op PATCH is a legitimate request that should return 200. Alternatively reject at validation time with a class-level "at least one field" rule. Either way it must be compile-time-known behaviour, not a runtime SQL error.

### A parent cannot be filtered by its children — no `exists`, and a `where` on a joined child under `as many` is meaningless
**Soha:** queries/joins

With `as many` compiling to a lateral, a top-level `where` on the child table has no coherent meaning — the child is not in the outer FROM. With a bare fan-out join it would filter the parents (a semi-join), but the same join also feeds the projection, so filtering it changes the returned collection too. `select Orgs ... left join Members ... as many members where Members.role == admin` can mean all orgs with only their admins, only orgs having an admin with all their members, or only orgs having an admin with only the admins. All three are things people want; the design picks none, and the semi-join reading has no other spelling.

**Taklif:** Split the two concerns syntactically. Put child-collection filtering on the join, alongside the ordering suffix: `left join Members on ... where Members.role == admin as many admins`. Add a separate `where exists (...)` / `where not exists (...)` for parent filtering, which also covers "orgs with no active subscription".

### Reading the body twice (raw for signature, parsed for the handler) has no defined buffering or consistency guarantee
**Soha:** request body

The sample's webhook flow reads the body twice: `VerifySignature` calls `request.raw_body()`, then the route calls `request.body() as WebhookPayment`. DESIGN.md defines neither builtin's interaction with the other. Unspecified: whether the body is buffered and replayable at all or the second read returns empty; whether the parsed value is guaranteed to derive from the exact bytes the signature covered rather than a re-read (the classic signature-bypass shape); and whether `raw_body()` is bounded at all, given it runs in middleware on a public unauthenticated endpoint and no size limit appears anywhere in the design.

**Taklif:** Specify that the body is read once into a bounded buffer and that both `raw_body()` and `body() as T` are views over that same buffer, so signed bytes and parsed value provably match. Put the bound in configuration with a per-group override (`routes "/api/v1/webhooks" max_body "256KB"`), with exceeding it producing a 413 before any middleware runs.

### Path parameters `@x` are untyped strings fed straight into `bigint` predicates and INSERT columns; Postgres does the parsing and malformed input is a 500
**Soha:** path parameters, coercion

`routes "/api/v1/orgs/{org_id}"` declares no type for the segment, and `@org_id` is whatever the URL contained — a string. It flows through untyped service parameters into predicates against `bigint` columns and into INSERT column values. Nothing coerces it, nothing validates it, and the spec never says whether a cast is inserted. Two consequences: malformed input reaches Postgres as a type error (a 500, not a 400), and the same untyped value can flow into a `bigint` column and a `varchar` column with no diagnostic. There is also no stated check that every `@name` used in a route body is bound by a `{name}` in the path — and the binder set spans two declarations (the `routes` prefix plus the `route` suffix), even though DESIGN.md insists "Full path is written literally; no prefix rewriting."

**Taklif:** Type the segments in the path literal — `routes "/api/v1/orgs/{org_id: bigint}"`, `route GET "{id: bigint}"` — defaulting to `string` when omitted. The router parses and returns 400 before any middleware runs, `@org_id : bigint` becomes a real type at every use site, and the merged binder set (prefix ∪ suffix) becomes the checked scope, so an unbound `@name` or a duplicate binder across the two pieces is a compile error. This is also the smallest change that gives untyped service parameters an inferable type at the call site.

### Spread has no absent-vs-null rule, so `set ...req` with an optional class field either erases data or no-ops — the spec provides `=?` for the explicit case and nothing for spread
**Soha:** spread semantics, PATCH

`?` on a class field means "not required" (absent from the request body); `?` on a column means "nullable". Spread joins the two with no stated rule for the absent case, and the omission is visible in the spec's own asymmetry: `update App.x.T set col =? maybe_null` exists precisely to skip a SET when the value is absent, but `set ...req` is given no field-by-field equivalent. In an INSERT, an absent optional field must omit the column so the DDL `default` fires — emitting NULL defeats the default or violates NOT NULL. In an UPDATE, an absent field must skip the SET — emitting NULL erases data. Relatedly, JSON distinguishes `{}` from `{"name": null}` and `?` cannot express which one a PATCH body means.

**Taklif:** State the rule and make it uniform: spread emits a column only when the source field is present; absent fields are omitted from the INSERT column list and from the UPDATE SET list, so `...req` behaves as `=?` field by field. That is the only reading under which the sample is correct. Then, because `?` can no longer express "explicitly set to null", either add a distinct spelling for clear-to-NULL or state that JSON `null` in a body means clear-to-NULL and is rejected at validation time when the target column is NOT NULL (`E: OrgEdit.name is optional but Orgs.name is NOT NULL; explicit null cannot be accepted`).

### `sum(req.lines, line => ...)` is a lambda over an in-memory array and a third overload of a SQL aggregate the spec restricts to queries — closures appear nowhere in the design
**Soha:** function types, builtins

DESIGN.md lists `count`, `sum`, `min`, `max` as "SQL aggregates, only valid inside a query", and separately lists a builtin `array.sum`. The sample calls a bare `sum` outside any query, over an in-memory array, with a lambda as its second argument — a third `sum`, ill-formed by the spec's own restriction. Beyond the collision, the language has no function type, no lambda syntax anywhere in DESIGN.md, and no typing rule for closures, so `line => line.quantity * line.unit_cents` has no inferable parameter type unless array element types propagate — which in turn requires `InvoiceLineInput[]` on a class field to be a real parametric type, also unspecified. This is the only higher-order construct in 1100 lines and it is entirely undesigned; it is not covered by the Known-invented list.

**Taklif:** Pick one spelling: either drop the bare-`sum`-over-array form and require `array.sum(req.lines, ...)`, matching the builtin list and removing the collision, or drop `array.sum`. Then decide whether the language has lambdas at all — the alternative that fits the "one operation per line, do not nest calls" style is no closures plus an explicit loop with an accumulator, or a dedicated fold form (`sum over req.lines as line: line.quantity * line.unit_cents`). If lambdas stay they need a spec section: parameter type inference from the receiver's element type, capture rules, and where they may appear. Either way, state that `T[]` is a real type constructor usable in parameters and returns, not only on class fields.

### Constraint messages are compiled into the binary and matched by constraint name at runtime, with nothing keeping the two in sync across migrations
**Soha:** constraints/runtime-coupling

DESIGN.md:96 promises that a constraint violation carrying a message becomes an automatic 400. That only works if the runtime maps Postgres's `constraint_name` error field to the message compiled from the current source — a hidden contract between a deployed binary and a schema built by a migration possibly written months earlier — and the design never states the naming scheme. The current generator makes it worse by emitting unnamed constraints (src/sql.rs:98-126 writes bare `UNIQUE ("col")`), leaving Postgres to auto-name. Three ways it breaks: a naming scheme that changes between jwc versions turns every 400 into a 500 against an older database; deriving names from message text makes a copy edit into a DROP/ADD CONSTRAINT migration with an ACCESS EXCLUSIVE full-table scan; deriving names from columns alone collides for two partial uniques on the same columns with different predicates.

**Taklif:** Fix a versioned, message-independent naming scheme now (`uq_<table>__<cols>`, `uq_<table>__<cols>__<8-hex-of-predicate>`, `ck_<table>__<cols>__<n>`) and always emit names explicitly in DDL. Add a `jwc migrate verify` that reads `pg_constraint`/`pg_indexes` and fails when a name the binary expects is absent, so a mismatch surfaces at deploy rather than as a 500. Make the test harness assert on the returned message, not merely that the write failed.

### Hashed-token lookup is unimplementable: three tables declare `unique` token_hash columns while the only hashing builtins are a salted KDF
**Soha:** builtins / schema

`Sessions.token_hash`, `ApiKeys.key_hash` and `Invites.token_hash` are all declared `private, unique`, but the design's only hashing builtins are `hash.password` / `hash.verify`, i.e. a salted password KDF that produces a different digest per call. That makes the `unique` constraints meaningless (two rows for the same token never collide) and makes lookup-by-token impossible: `where Invites.token_hash == hash.password(token)` can never match, and the only expressible alternative is scanning all pending invites and running `hash.verify` on each — a cross-tenant read plus an unauthenticated CPU-exhaustion endpoint. The sample never writes accept-invite, logout, or API-key auth, which is exactly why the gap is invisible; the first developer to need one will store the raw token, since that is the only thing the language makes possible.

**Taklif:** Add a deterministic keyed digest alongside the KDF — `hash.sha256(value)` and preferably `hash.hmac_sha256(value, key)` — so `where Invites.token_hash == hash.hmac_sha256(@token, env("TOKEN_PEPPER"))` is an index seek and the `unique` constraints become real. Document the split rule explicitly (KDF for user-chosen secrets, verified only; HMAC for high-entropy tokens, looked up by digest) and write an accept-invite endpoint into the sample so the pattern is demonstrated. Add the constant-time comparison primitive that `verify_signature` also needs.

### `request.client_ip()` has no trusted-proxy semantics, and the redesign has no server-runtime configuration surface at all
**Soha:** middleware / runtime config

`client_ip()` keys both rate limiters and the audit `Events.ip` column, and its behaviour behind a proxy is unspecified. Both possible behaviours are broken without configuration: trusting `X-Forwarded-For` makes the limiter bypassable with a random header per request and makes the compliance log attacker-authored; ignoring it collapses every user onto one IP, turning `StrictRateLimit`'s 5-per-300s into a global lockout of login and register. The current implementation has `JWC_TRUSTED_PROXIES` for exactly this and the redesigned builtin list drops it with nothing in its place. More broadly, `database App : Postgres { init() { ... } }` configures the connection pool and `main()` calls `serve(port)` — there is nowhere in the redesigned language to configure the HTTP server: no trusted proxies, no max body size, no request or header timeouts, no CORS, no TLS. Every one of those is a security control. Separately, `RateLimit` keys on `"rl:" + request.path() + ":" + ip`, so walking `{org_id}` values yields a fresh bucket per id and mints unbounded Redis keys.

**Taklif:** Add a runtime config block that is to the server what `init()` is to the pool — `server { trusted_proxies = [...]; max_body_bytes = ...; request_timeout = "30s"; }` — and define `client_ip()` against it, documenting that with no list configured it returns the peer address and ignores XFF. Add `request.route()` returning the declared pattern rather than the concrete path so rate-limit keys have bounded cardinality, and key auth-endpoint limits on the identity being attacked as well as the IP.

### Service function parameters are untyped and return shapes are inferred across files, so nothing cross-file is checkable
**Soha:** LSP / codegen

Services are the module boundary and the only thing routes call, yet only req: Register-style params carry annotations - everything else is bare (function invoices(org_id, status, limit), function profile(account_id)). The compiler therefore cannot check arity-plus-types at a call site, the LSP cannot offer signature help or complete a '.' on a parameter, and parameter-order inconsistency across the codebase (OrgService.create(req, owner_id) vs AuthService.update_profile(account_id, req)) is unenforceable. Return shapes are worse: a route's response body lives two or three hops away inside an as { } in a service or a view (route GET "" -> BillingService.subscription -> SubscriptionDetail's projection), and some functions return different shapes on different paths (WebhookService.record_payment returns { status: "duplicate" } or { status: "ok" }), which kills hover, typed client generation, and OpenAPI output.

**Taklif:** Require type annotations on service function parameters - they are the cross-file boundary; locals stay inferred - e.g. function invoices(org_id: bigint, status: InvoiceStatus?, limit: int?). That alone gives signature help, arity and type checking, and completion. For returns, add an optional annotation checked against the inferred projection rather than replacing it (function subscription(org_id: bigint) -> SubscriptionDetail), and make it mandatory when a function has multiple returns with differing shapes, turning the webhook case from silently untypable into a compile error naming both returns. 'jwc openapi' then becomes derivable.

### Validation 400 responses have no defined body shape, and minLength is overloaded across scalars and arrays
**Soha:** Error messages / HTTP contract

'request.body() as Register' is specified to '400 on failure' with no statement of what the 400 body contains. Class fields carry multiple rules and classes nest arrays of classes, so a client needs per-field paths and per-rule identifiers to render a form - none of which is specified. A POST of InvoiceCreate with three lines where the third has quantity = 0 must produce something like lines[2].quantity with rule min and limit 1, and nothing says it does. The design also never says whether validation stops at the first failure or collects all of them, which determines whether a form can highlight every bad field in one round trip. Separately minLength is overloaded: on 'description varchar(200) required, minLength(1)' it means one character, on 'lines InvoiceLineInput[] required, minLength(1)' seven lines later it means one element - the same token with two meanings sharing one message template.

**Taklif:** Specify the 400 body as a stable contract, e.g. {"error": "validation_failed", "fields": [{"path": "lines[2].quantity", "rule": "min", "limit": 1, "message": "..."}]}, and specify collect-all rather than fail-fast. Give the array form a distinct spelling (minItems) so the message is unambiguous and the compiler can reject minItems on a scalar and minLength on an array. Make the default messages localisable the same way constraint messages already are, since the sample is written in Uzbek and built-in English defaults would be the only English strings in its API surface.

### Cross-schema FK cycles make per-file DDL non-emittable, and gen-sql has no defined ordering
**Soha:** Migrations / gen-sql

The one-file-per-schema layout hides a dependency graph that spans schemas in both directions. In the sample, billing depends on org (Subscriptions.org_id references App.org.Orgs), org depends on auth (Members.account_id references App.auth.Accounts), and auth depends back on org (ApiKeys.org_id references App.org.Orgs) - a genuine schema-level cycle that cannot be resolved by ordering files, only by emitting all FKs as a pass after all tables. A view compounds it: 'view OrgBillingSummary of App.billing' selects from App.org.Orgs, so a billing-schema object's base table lives in another schema and the dependency is invisible from src/db/billing.jwc. Nothing in the design states the emission order, which means the DBA test - 'reads a schema file and can state exactly what DDL it produces' - is not answerable per file.

**Taklif:** Define the gen-sql emission contract explicitly: schemas, then enum types, then tables, then all foreign keys as a separate pass, then views topologically ordered by base-table dependency, with a printed plan and a readable trace when a genuine non-FK cycle exists. State in the DBA-test wording that FK DDL is emitted globally rather than per file, so a reader knows what a single schema file does and does not produce. Add 'jwc migrate status' showing applied, pending and drift, and have dev-mode 'jwc serve' diff information_schema against the program on boot so a schema change surfaces as a startup error naming the missing column rather than a 500 wrapping PG 42703.

### The raw/record boundary is undefined at composition, and the sample app already violates its own compile-error rule
**Soha:** runtime / raw fast path

DESIGN.md states raw is `row_to_json` forwarded with zero parsing, that reading a field of a raw value is a COMPILE ERROR, and that `as { ... }` is the ONLY way to get a record. The sample contradicts this directly: `middleware/auth.jwc` does `let access = select from App.org.MemberAccess ... first;` with no `as` — therefore raw — and then reads `access.role` and stores it in context. Either the rule is wrong or the sample does not compile, and the design gives no third option (no `first`-implies-record rule, no `as`-less record form). Separately, nothing defines what happens when a raw value is embedded in a constructed object (`json({ items: raw, next: cursor })`): text splice, still zero-parse, or full parse-and-reserialize. Since the answer determines whether the headline performance property survives any response that carries metadata, it cannot be left implicit.

**Taklif:** State the raw-loss rules as language rules and make them visible: define raw splicing as string concatenation into the surrounding object (never a parse), and emit a per-query compiler diagnostic naming whether the result stayed raw and, when it did not, the construct that lost it (`raw lost here: field read access.role`). Then fix the sample — `MemberAccess` needs an `as { role, org_id }` projection, or `first` must be specified as producing a record.

### `bigint` ids have different numeric fidelity on the raw path than on the record path
**Soha:** runtime / serialization

Ten of eleven tables declare `id bigint primary key identity` and every FK is `bigint`, but DESIGN.md never states the wire representation for `bigint` — while defining two result representations that reach it differently. Raw forwards `row_to_json` output verbatim, so the integer text is exact. The record path parses the value into a language numeric and reserializes it; the current runtime routes numeric coercions through `f64`, so above 2^53 the digits change. `AuthService.register` returns `... as { id, email, display_name, created_at }` (record) while `BillingService.subscription` returns `select from App.billing.SubscriptionDetail ... first` (raw): two endpoints can print different digits for the same id. A third disagreement point exists at input — `@org_id` arrives from the path as text and is compared against a `bigint` column.

**Taklif:** Pick one wire representation for `bigint` and enforce it identically across raw, record, interpreter and native backends. String is the defensible choice given JavaScript consumers, and the design already accepts explicit physical-representation overrides (`created_at timestamptz as "createdAt"`), so a representation annotation is idiomatic. Add a differential test asserting `9007199254740993` round-trips byte-identically on both paths and both backends.

### `update ... first` has no stated locking behaviour, and `first` without `orderby` is nondeterministic
**Soha:** runtime / generated SQL semantics

Postgres has no `UPDATE ... LIMIT`, so `update ... where ... first` must lower to something like `UPDATE ... WHERE ctid IN (SELECT ctid FROM ... LIMIT 1)`, and whether that subquery takes `FOR UPDATE` is the entire difference between a lost update and a serialization. DESIGN.md says only that the form 'returns 0..N rows' — which does not even agree with `first`. `BillingService.cancel(org_id)` is the live case: `update App.billing.Subscriptions set status = canceled, canceled_at = ... where org_id == org_id and status != canceled first`. Two concurrent cancels both select the same row and both write, and the `unique (org_id) where status != SubscriptionStatus.canceled` partial index that would normally arbitrate is inactive for exactly the canceled state, so the database does not catch it. Separately, `select ... first` over a non-unique predicate (`BillingService.subscription`) returns an arbitrary row whose identity can change with a plan flip, and no rule requires `orderby` alongside `first`.

**Taklif:** Specify the lowering in the design: `update ... first` / `delete ... first` emit `FOR UPDATE` in the row-selection subquery (with `SKIP LOCKED` reserved for an explicit work-claiming form), so a DBA reading the source can state the locking behaviour — the DBA test applied to DML. Require `orderby` with `first` unless the compiler can prove the WHERE hits a declared unique or primary key; the schema already declares those keys, so the check costs nothing.

### `as many` aggregation is computed before the outer `orderby`/`limit`, so list endpoints are O(table), not O(page)
**Soha:** runtime / view compilation

README states `of <database>` means a real database object, so a `view` is a real `CREATE VIEW` whose body already contains the `as many` LATERAL + json_agg. A `select ... orderby ... limit N` against that view is a qual+sort+limit layered on top; Postgres will not push the LIMIT below the join, so it aggregates children for every row surviving the WHERE, then sorts, then discards. `BillingService.invoices` selects from `InvoiceDetail`, which carries `as many lines` AND `as many payments`: for an org with 100k invoices that is 200k json_agg subresults built to return 50 rows. The identical view is fine under `BillingService.invoice(org_id, invoice_id)`, so the trap fires only on the list variant — the one under load. Nobody hand-writing this SQL would aggregate children before paging; they would filter+sort+limit the driving table in a CTE and join children to the surviving keys. DESIGN.md also never states whether `view` is a real `CREATE VIEW` or an inlined macro, which changes every plan in the app.

**Taklif:** State the view materialization question in DESIGN.md, then make the compiler emit a two-stage form whenever a `many` child meets `orderby`/`limit`: CTE 1 selects driving-table keys with the WHERE + ORDER BY + LIMIT, CTE 2 LATERALs children over only those keys. Where the pushdown cannot be proven (e.g. ordering on a child-derived value), fail at compile time with the rewrite spelled out rather than silently emitting the slow plan. The same mechanism is where a bound on the child collection itself (`as many payments limit 20 orderby created_at desc`, `as count payments`) belongs.

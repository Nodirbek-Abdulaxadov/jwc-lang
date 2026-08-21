# DEFERRED.md — decisions taken as "not in 1.0", with what happens instead

ROADMAP §10 sets the rule this file exists to satisfy: every gap gets **an
answer or a `DEFERRED` verdict**. "We'll think about it later" is not an
answer; `DEFERRED` is, because each row below states what 1.0 does instead.

Nothing here is a non-goal. Non-goals are ROADMAP §8 and those PRs get
closed; these are dated omissions.

| id | Deferred | What 1.0 does instead | Why |
|---|---|---|---|
| `DEFERRED-1` | Explicit nullable coercions (`int?(x)`) | Source-based classification: coercion failure on a client-derived value is `BadRequest` 400, elsewhere a fault (types §7.2) | Taxes 100% of call sites to fix the 1%; a developer who forgets the `?` gets the 500 anyway |
| ~~`DEFERRED-2`~~ | ~~`--native` AOT backend~~ | **Withdrawn in 0.9.903.** `jwc build` produces a native binary and covers the language; `view`, and nothing else, is refused by name. There is no `--native` flag and there was never an `E0910` | The stated reason — a second backend doubling every query-compiler change — does not hold against the 1.0 front-end: `query_sql` lowers a query to a SQL string at compile time, so codegen *calls* the query compiler rather than reimplementing it. The two backends are held to byte-identical responses over jwc-shortener, MyWallet and task-tracker |
| `DEFERRED-3` | Enum `DROP VALUE` / reorder rebuild | `E1102` plus the printed five-statement recipe and a guard `SELECT count(*)` (migrations §5.3) | The rebuild needs a cross-schema column map. Refusal loses no data; wrong automation does |
| `DEFERRED-4` | Per-foreign-key messages | FK violation → `BadRequest` 400, `"referenced row does not exist"`; `jwc lint --constraints` lists them (errors §6.3) | The right status varies by case (400/404/409) and the data to choose does not exist yet |
| `DEFERRED-5` | General subqueries, CTEs, window functions, recursive queries, full-text | `where exists`/`not exists` in the query language; everything else through the parameterised `raw(…)` escape hatch, banned inside `view`, counted by `jwc lint` (writes §6) | The query compiler is already 28% of the work. The hatch is a valve and its usage count measures which feature to add next |
| `DEFERRED-6` | Navigating into `jsonb` | A `jsonb` column is `Raw` — it splices into a response and cannot be read field-wise (types §5.6) | Path navigation needs a type for "unknown shape", which is the one thing the `Raw`/`Record` lattice exists to avoid |
| `DEFERRED-7` | A dev-only `/__jwc/queries` endpoint | `JWC_LOG_SQL=1`, `jwc explain`, and LSP hover-SQL (queries §7.4) | Three mechanisms already cover the DBA and Developer tests. The fourth is convenience |
| `DEFERRED-8` | Multi-row `insert` | `for (x in xs) { insert into … }` inside a `transaction` (writes §2.1) | One statement shape, one RETURNING shape. Batching is a performance change with no new semantics |
| `DEFERRED-9` | `SKIP LOCKED` / work claiming | `update … first` always emits `FOR UPDATE` (writes §4) | Work-claiming is a queue feature and queues are ROADMAP §7 |
| `DEFERRED-10` | `send_email` and outbound I/O builtins | A package: `import mail; mail.send(…)` (builtins §10) | Provider shape is not language shape. Core stays Postgres + HTTP |
| `DEFERRED-11` | Test isolation, `seed.*`, fixtures | `test "…" { }` blocks exist and run; each creates its own data and the suite runs serially (ROADMAP v0.28.0) | N9 showed the sample's own tests corrupting each other. Getting isolation wrong is worse than running serially |
| `DEFERRED-12` | Bare-join aggregation and `as many` in one query | `E0532` with the two-query rewrite printed (queries §6.2) | Whether the lateral survives grouping is a real design question; a silently multiplied `count` is not an answer to it |
| `DEFERRED-13` | A real module/visibility system | Flat declaration space; `import` is a **checked, mandatory dependency declaration** that does not scope (names §6.3) | Flat + enforced imports is enough for 1.0. Visibility is a 2.0 redesign |
| `DEFERRED-14` | Typed client SDK generation (TS/Go/Python) | `jwc openapi` | OpenAPI is the boundary; per-language SDKs are separate projects |
| `DEFERRED-15` | Automatic inverse of destructive migrations | `migrate down` exists; destructive statements emit `-- irreversible` and stop (migrations §9.2) | After a column is dropped the data is gone. Promising reversibility is a lie |
| `DEFERRED-16` | Background jobs, durable queue, DLQ, WebSocket, SSE, in-process cache | Not declarable in the 1.0 vocabulary. **The runtime code is not all retained** — see the note below | design.md never touched these areas. Guessing a vocabulary means writing it twice |
| `DEFERRED-17` | Sequences as a declared object class | A counter table plus `update … first` (which emits `FOR UPDATE`) — shown in the sample's `next_invoice_number` | A sequence is a sixth DDL object class with its own diff rules, for one use in the sample. The counter-table form is correct and already specified |
| `DEFERRED-18` | Generated columns (`GENERATED ALWAYS AS … STORED`) | Compute in application code, or a counter table | The expression would be raw SQL text inside a declaration — a hole in the DBA test, not a feature |

---

## A correction to `DEFERRED-16`

This row used to read "the 0.9.x runtime code is retained but unreachable".
That was not true of all of it. The v0.25.0 cutover deleted 73 source files
along with the 0.9.x front-end, and the queue was among them:

| | State |
|---|---|
| durable queue, DLQ, `dispatch` | **deleted** at v0.25.0. `queue.rs` (1,352 lines) is at `60cc971^` and nowhere else |
| WebSocket / SSE | the runtime half is back (`src/native/prelude/ws.rs.in`), and unreachable: the vocabulary has no way to declare one, and the native dispatcher answers 501 |
| in-process cache | the runtime half is back (`jwc_cache_store`), and unreachable: `cache.*` is not a 1.0 built-in |
| native AOT backend | **restored** in 0.9.901–0.9.903, and covered — see `DEFERRED-2` |

"Retained but unreachable" and "deleted" are different facts, and a reader
deciding whether to depend on 1.0 needs the second one. Restoring the queue
is a decision that has not been taken; what has been taken is the decision
to stop saying it is already there.

---

## Gap → verdict index

Every gap in [`gaps.md`](./gaps.md) (44) and ROADMAP §5 (12) resolves to a
normative clause or to a row above. `spec-coverage.json` carries the machine
-readable form; this is the index.

| Gap | Verdict |
|---|---|
| 1 join alias / self-join | queries §4.2 |
| 2 `where col == param` ambiguity | names §5.3 |
| 3 `as one` null shape | queries §4.5, types §6.3 |
| 4 bare-join aggregates, `count(distinct)` | queries §6.2 + `DEFERRED-12` |
| 5 order/limit inside `as many` | queries §4.6 |
| 6 `delete` returns nothing | writes §5 |
| 7 empty `set ...req` | types §9.5 |
| 8 filter parent by children | queries §3.5 |
| 9 path params untyped | routing §3.1 |
| 10 no response headers | routing §6.2 |
| 11 envelope pagination / raw composition | queries §9.3, types §5.4 |
| 12 route conflicts / precedence | routing §4 |
| 13 middleware undeclared path-param dep | middleware §2, §3 |
| 14 middleware order / `use` composition / after | middleware §4, §5 |
| 15 `client_ip()` proxy trust | routing §5.4, config §3.3 |
| 16 double body read | routing §5.1 |
| 17 raw-vs-record not total | types §5.3 |
| 18 view queries have no alias binder | names §5.4, queries §2.2 |
| 19 `?` never propagates | types §6 |
| 20 path param coercion → 500 | routing §3.2 |
| 21 spread absent-vs-null | types §9.2 |
| 22 `sum(xs, lambda)` | builtins §5 (non-goal for closures: ROADMAP §8) |
| 23 NOT NULL backfill | schema §10, migrations §7 |
| 24 views veto ALTERs / phases | migrations §4 |
| 25 partial index predicates in diff | schema §4.3, migrations §8 |
| 26 enum evolution | migrations §5 + `DEFERRED-3` |
| 27 renames → data loss | migrations §6 |
| 28 constraint name ↔ message coupling | schema §8.3 |
| 29 generated SQL invisible | queries §7.4 + `DEFERRED-7` |
| 30 message-less constraints & FK → 500 | errors §6 + `DEFERRED-4` |
| 31 untyped service params / returns | types §10 |
| 32 validation 400 body / `minLength` overload | types §11 |
| 33 cross-schema FK cycles / gen-sql order | schema §9 |
| 34 bare identifiers in `where` | names §5.3 |
| 35 `private` contradicted by projection/view | schema §3 |
| 36 spread whitelist preconditions | types §9.1, §9.3, §11.5 |
| 37 block vs route `use` order | middleware §4.1 |
| 38 hashed-token lookup impossible | builtins §6 |
| 39 no server config surface | config §3 |
| 40 no pagination primitive | queries §9 |
| 41 raw boundary at composition | types §5.4, §5.5 |
| 42 `bigint` fidelity raw vs record | types §2.3 |
| 43 `update … first` locking / `first` order | writes §4, queries §5.2 |
| 44 `as many` aggregation before limit | queries §8.3 |
| N1 `on update now()` | schema §6 |
| N2 scalar type dictionary | types §2.1 |
| N3 expression core | types §12 |
| N4 coercion on client input | types §7.2 + `DEFERRED-1` |
| N5 `import` semantics / free functions | names §6 |
| N6 `having` / `in` / `like` / nested orderby | queries §3.3, §3.4, §5.4; `having` in the clause list §1 |
| N7 bare `return;` in `after` | middleware §5.3 |
| N8 package content model | `DEFERRED-13` for visibility; package `table` declarations are ROADMAP v0.28.0 |
| N9 test isolation | `DEFERRED-11` |
| N10 doc comments / `identity` physical form | schema §7, §2.3 |
| N11 `now()` vs `date.now()` | types §2.4 |
| N12 join attachment tree | queries §4.4 |

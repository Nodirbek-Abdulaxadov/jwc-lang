# Changelog

All notable changes to JWC are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

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
  [http-framework-benchmark](https://github.com/Nodirbek-Abdulaxadov/http-framework-benchmark)
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

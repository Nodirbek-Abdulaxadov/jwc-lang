# Native AOT scope (`jwc build --native`)

Status: **DRAFT** · Target: stable at v1.0 · Reflects: **v0.4.8**.

> **Non-goals (declared, won't ship pre-1.0):** LLVM IR backend, cross-target
> native build matrix (Windows-ARM, macOS-ARM, FreeBSD, …), WASM target.
> Native AOT scope is intentionally **Linux x86_64 (glibc + musl) + Docker
> amd64/arm64**. See [`ROADMAP.md` Non-goals](../../ROADMAP.md#non-goals-10-ga-qadar-va-undan-keyin-ham--qatiy-yoq) for the policy rationale.

**Related spec docs**:
[index](index.md) ·
[visibility](visibility.md) (the AOT path trusts the validator's
visibility pass — `pub fn` / `fn` modifiers in the emitted Rust crate
do NOT correspond to JWC `Visibility`) ·
[semantics](semantics.md) (value model, savepoints, error kinds — the
constructs this doc says lower or panic) ·
[threat-model](threat-model.md) (every mitigation is mirrored in
`src/native_prelude.rs.in` — header byte loop, SSRF allowlist) ·
[builtins](builtins.md) (the surface the AOT codegen promises to lower).

This file documents what the v1.0 native AOT build (`src/native_build.rs`
→ Rust codegen → `cargo build`) supports, what it deliberately defers, and
what the user falls back to `jwc run` (the interpreter) for. The roadmap
positions native AOT as the **stateless route tier**: low-latency, no
nested runtime, no rebound semantics. Stateful work — long-running
workers, mid-transaction rollback boundaries, distributed traces — runs
under the interpreter where the async runtime and DB engine are already
re-entrant.

## What works end-to-end on `--native` today

Lowered cleanly to Rust + tokio + a thin axum-shaped HTTP layer:

- **Routes.** `route GET|POST|PUT|DELETE|PATCH "path" { ... }` block-body
  routes, including `{path}` params and middlewares. Mount/group expansion
  happens before codegen (see `flatten_namespaces`).
- **Values.** Object literals (`{ a: 1, b: "x" }`) compile to `V::Record`
  with shape-deduped `Arc<Vec<JwcStr>>` field layouts; arrays of records
  (`/json-large` 1000-object array) share one schema allocation.
- **Response helpers.** `json`, `text`, `html`, `ok`, `created`,
  `not_found`, `unauthorized`, `forbidden`, `internal_error`,
  `status_code`.
- **Request introspection.** `path_param`, `query_param(name[, default])`,
  `header`, `body`, `request_path`, `request_method`.
- **Crypto / id / time.** `sha256`, `uuid`, `now`, `unix_timestamp`,
  `hash_password`, `verify_password`, `jwt_sign`, `jwt_verify`.
- **Env / coercion.** `env(name)`, `int(v)`.
- **Cache / sleep / http.** `cache_get`, `cache_set`, `cache_delete`,
  `sleep_ms`, `http_get` (minimal — no streaming).
- **DB selects (simple).** `select Entity from CTX.Table where … first`
  on the v0.4.4 `V::RawJson` fast path. Projections (`{ col1, col2 }`),
  `where Entity.col == @var`, `orderby`, `limit/offset`, `count(*)`,
  `select first` are all lowered.
- **DB writes (simple).** `insert var into CTX.Table` and
  `update CTX.Table set col = expr where …` (the v0.4.5 atomic form,
  emitted as a single prepared `UPDATE`).
- **Control flow / locals.** `let`, assignment, `if`/`while`/`for in`,
  `break`/`continue`/`return`, `print`, `push`, string ops, JSON
  parse/stringify.

The bench repo's `_my/jwc-app/main.jwc` exercises this entire surface in
one program — if the bench suite's `jwc check` is green on a target
revision, the surface above is intact.

## What raises a runtime panic in the native build (deferred to interpreter)

`src/native_build.rs::emit_stmt` (and its expression sibling) emit
`panic!(…)` for forms whose semantics need the interpreter's value model
or DB engine state machine:

- **Savepoints** — `savepoint <name> { ... }` lowers to
  `panic!("savepoint not supported in --native build yet; use \`jwc run\`")`
  at `src/native_build.rs:2101`. Mid-transaction nested rollback boundaries
  need the interpreter's connection-bound savepoint stack.
- **Persistent job queue (Postgres-backed worker loop).** The Postgres
  driver in `src/queue.rs` is built around a single tokio runtime; an AOT
  binary that re-enters the queue's pop loop from inside a route handler
  would nest runtimes. `queue.rs:35` documents the contract; the AOT path
  panics if you try to drive the queue from a native binary.
- **Codegen-level unknowns.** Every `emit_*` dispatch in
  `native_build.rs` that hits an unknown entity in
  `insert`/`update`/`delete`/`select`/aggregate forms panics with
  `panic!("<op> on unknown entity {table}")`. These are reachable only on
  internal codegen bugs (the validator catches user-level cases first),
  but the panic surface is documented here so a future contributor
  doesn't read them as user-facing.

## What the user falls back to `jwc run` for

These work only under the interpreter. The native build will either panic
(see above) or silently lack the wiring:

- **Long-running queue workers.** Anything that calls `queue_pop`,
  spawns a background worker loop, or relies on the queue's lease/retry
  state machine.
- **Mid-transaction nested rollback.** `savepoint <name> { ... }` inside
  a `transaction { ... }`. The flat-transaction case (one
  `transaction { ... }` per request, no nested savepoints) is fine on
  AOT.
- **OTLP / distributed tracing.** The `observability/` OTLP exporter only
  wires the interpreter's request span. A native binary won't emit OTLP
  spans even with `OTEL_EXPORTER_OTLP_ENDPOINT` set.

If a feature is needed in AOT, the work item is "lift it from
`runner/builtins.rs` into `native_build.rs` BUILTINS + emit a
real lowering" — see the CLAUDE.md note at the top of `native_build.rs`
for the contract.

## Future — items the native mirror could close next

Concrete follow-ups that would shrink the "panics in --native" surface.
Each is a self-contained codegen task; none requires a language-level
change.

- **Savepoint codegen.** `Stmt::Savepoint` currently lowers to
  `panic!("savepoint not supported in --native build yet; use \`jwc run\`")`
  (`src/native_build.rs:2101`). The interpreter path
  (`engine::with_savepoint`) is already self-contained — the codegen
  mirror is "emit `SAVEPOINT <n>` / `RELEASE` / `ROLLBACK TO` against the
  same `tokio_postgres::Transaction` handle the enclosing
  `transaction { ... }` block holds". See
  [`semantics.md`](semantics.md) §6.1 for the contract the codegen
  needs to honour.
- **Full queue driver.** The interpreter wires both the in-process
  driver AND the `JWC_QUEUE_DRIVER=postgres` worker loop; the AOT path
  panics if you try to drive the queue from a native binary because
  the pop loop would nest tokio runtimes. The fix is to refactor
  `queue.rs` so the pop loop can be spawned onto the AOT binary's
  primary runtime (one runtime per process) instead of starting its
  own.
- **OTLP wiring.** The `observability/` OTLP exporter is only
  initialised by the interpreter's request span. Lifting the
  initialisation into the AOT prelude (`src/native_prelude.rs.in`)
  would let `OTEL_EXPORTER_OTLP_ENDPOINT` work out of the box for
  native binaries.
- **Interpreter-only builtins on the parity list.** `set_json_field`,
  `request_body`, `http_post`, `db_query`, `jwt_sign`, `jwt_verify`,
  `unix_timestamp`, the `register_job_handler` family, `send_email`,
  `setContext` / `context`, and `dispatch` are all flagged as
  *interpreter* in [`docs/builtins.md`](../builtins.md). Each is a
  candidate for native lowering on its own merit — the shape of the
  work is "add the name to `BUILTINS`, emit a body in
  `native_prelude.rs.in`, pin a conformance case without the
  `// CONFORMANCE: interpreter-only` header".

# JWC Production Readiness Plan

Status: PROPOSED · Target: v1.0.0
Scope: everything between today's v0.4.1 and a version teams can bet a real backend on.

This plan is grounded in a code audit of the current tree (v0.4.1, ~26.5k LOC Rust):
it names the actual files and gaps, not generic advice. Phases are ordered by
dependency — each unblocks the next. Items marked **[1.0-blocker]** gate the
1.0 release; everything else can ship in 1.x.

---

## Guiding definition of "production ready"

A team can:

1. Write a non-trivial API against a stable, documented language spec.
2. Upgrade `jwc` minor versions without source changes (SemVer honored).
3. Get a precise diagnostic (file:line:col) for every compile-time error.
4. Run the server for weeks: graceful shutdown, no leaks, observable.
5. Trust the data layer: transactions, migrations, and pooling behave
   predictably under failure.
6. Get security fixes through a documented disclosure + patch process.

---

## Production dogfooding — findings from `jwc-shortener` (June 2026)

The first production JWC app —
[`jwc-shortener`](https://github.com/Nodirbek-Abdulaxadov/jwc-shortener),
live at 1kb.uz (native AOT image, k8s + ArgoCD, migrations via init
container) — validates the deploy story end-to-end and surfaced concrete
gaps. Each is folded into its phase below; the map:

| Finding in production | Plan item |
|---|---|
| Whole-row `update` after a read lost-updates the `hits` counter under concurrency | Phase 4 — atomic `update ... set` |
| PK collision (short slug) surfaces as a generic 500 — can't catch unique-violation distinctly | Phase 3 — typed `catch` |
| No `substring`/`take` builtin → `split(s, "")` loop workaround | Phase 3 — builtin gaps |
| Middleware can't see the response → `latency_ms` always 0, status hardcoded | Phase 5 — response-phase middleware |
| Hand-rolled `/healthz` has no DB check — probes stay green during a DB outage | Phase 5 — built-in `/readyz` |
| Per-process rate limit + spoofable `x-forwarded-for` behind Cloudflare | Phase 5 — trusted-proxy `client_ip()` |
| Declared-but-unused middleware left an endpoint unthrottled, no warning in the build path | Phase 2 — lint enforcement |
| Dockerfile curls a release tarball; glibc 2.36→2.40 mismatch broke older bases | Phase 8 — official image + musl builds |

This is the feedback loop working as intended — keep shipping real apps and
folding findings back here.

---

## Phase 0 — Specification & compatibility contract

> You cannot stabilize what is defined only by the implementation.

- **[1.0-blocker] Language specification** (`docs/spec/`) — ✅ scaffold
  landed (this PR): `docs/spec/README.md`, `grammar.ebnf` (top-level
  productions, statements, expressions, types, routes, SQL-shaped forms
  with `TODO` markers for incomplete sections), `semantics.md` (evaluation
  order, scope, coercion, integer/float behaviour, DB and HTTP semantics,
  what is implementation-defined), `builtins.md` (contract template +
  first batch of entries). Remaining: fill the `TODO` productions and
  pin every entry with a conformance case.
- **[1.0-blocker] SemVer policy doc** — ✅ landed (this PR):
  [`SEMVER.md`](SEMVER.md). Enumerates breaking-change rules, release
  cadence, and the pre-release suffix contract.
- **Conformance suite**: skeleton landed in `13a3cad` (16 cases, harness
  runs interpreter + native AOT); expansion gated on the `docs/spec/`
  extraction so each new case pins a spec rule.
- **Deprecation policy** — ✅ landed (this PR):
  [`DEPRECATION.md`](DEPRECATION.md). Pre-1.0: ≥ 1 minor cycle of warning.
  Post-1.0: full minor cycle of warning before removal in the next major.

Exit criteria: every construct in the README has a spec section and at least
one conformance test.

---

## Phase 1 — One semantic model, two execution strategies

> The single highest-leverage refactor in the codebase.

Current state: the interpreter `Value` enum (`src/runner/mod.rs:3845`) has
`Int/Float/Str/Bool/Null/Void/Array` — **no structured object**; objects and
DB rows travel as JSON strings. The native AOT runtime has a dynamic
`V::Object` (FxHashMap + Arc/CoW since v0.4.1, `native_build.rs:3065`) — and
the benchmark repo's own analysis attributes the `/json-large` gap to
rust-axum/dotnet (13.0k vs 22–23k RPS) to exactly this dynamic value model.

The fix is NOT "make everything a hashmap object." It is a **two-path
design**, leaning into JWC's compile-time-known shapes:

- **[1.0-blocker] Static path — struct monomorphization in AOT.** Every
  `entity`/`class` has a shape known at compile time → codegen a concrete
  Rust struct (`struct Brand { id: i64, name: String }`) with a generated
  serializer. No hashing, no dynamic dispatch on the hot path. This is what
  closes the `/json-large` gap; target: within 15% of rust-axum.
- **[1.0-blocker] Dynamic fallback stays.** `json_parse` of arbitrary
  payloads, object literals with computed keys, and middleware context
  cannot be monomorphized — they keep `V::Object`. The compiler decides
  static vs dynamic per expression; more type annotations ⇒ more code on
  the struct path (gradual typing pays for itself here).
- **[1.0-blocker] Interpreter — shape-based records, not JSON strings and
  not hashmaps.** For known shapes, represent rows/instances as
  `Value::Record { shape_id, fields: Arc<Vec<Value>> }` with field access
  resolved to an index at compile time (O(1), no hashing, no re-parsing).
  Dynamic values use the same fallback representation as AOT. This removes
  every "objects are JSON strings" code path (`split()` returning a JSON
  string, string round-trips on field access) while keeping `jwc run` fast.
- **[1.0-blocker] Shared `jwc-runtime` crate**: one shape table, one dynamic
  `V`, one set of builtin implementations, one JSON serializer — consumed by
  both the interpreter and AOT-generated code. Interpreter and AOT then
  differ only in *dispatch strategy*, never in semantics.
- Performance gates (enforced by the benchmark suite, see Phase 7):
  `/json-large` AOT ≥ 90% of current after struct path lands, then climbing
  toward the axum/dotnet tier; interpreter hot select path regression ≤ 5%.

Exit criteria: conformance/parity suite green; no internal JSON-string
round-trip for field access in `src/runner`; `/json-large` benchmark shows
the struct path beating today's 13.0k RPS.

---

## Phase 2 — Code health & diagnostics

- **[1.0-blocker] Decompose `src/runner/mod.rs` (5,152 lines)** into
  `runner/{eval,http,db,middleware,jobs,ws}.rs`; same for `parser.rs`
  (4,650 lines) into per-declaration modules. Rule of thumb: no file > 1,200
  lines.
- **[1.0-blocker] Spanned diagnostics everywhere.** `Token` carries only a
  byte `offset`; many `bail!`s have no location. Introduce `Span {file, range}`
  on AST nodes, render via the existing `diag::SourceMap` as
  `error[E0123]: ... --> app.jwc:14:9` with a source snippet. Wire the same
  spans into `jwc-lsp` diagnostics.
- **Error-code registry**: `src/error_codes.rs` exists — make every
  user-facing error reference a stable `E####` code with a docs page.
- **`unwrap()` budget**: 340 `.unwrap()` calls in `src/` today. Policy:
  forbidden outside tests except with a `// INVARIANT:` comment;
  enforce via clippy lint config in CI.
- **Lint that actually gates.** jwc-shortener shipped with a declared-but-
  unused `RateLimit` middleware — leaving its write endpoint unthrottled in
  production — and nothing in the build path said a word, because `jwc lint`
  is a separate opt-in command. Encode this as a lint fixture test, surface
  high-signal warnings (unused middleware, unused function) in `jwc build` /
  `jwc test` output by default, and add `--deny-warnings` for CI use.
- **Fuzzing**: `cargo-fuzz` targets for lexer + parser (untrusted `.jwc`
  input must never panic the CLI or LSP). Run in nightly CI.

Exit criteria: clippy `unwrap_used` warning clean; fuzzer 24h run with zero
panics; every parse/validate error shows file:line:col.

---

## Phase 3 — Type system & language completeness

- **[1.0-blocker] Real typed `catch`.** Today `catch (e: DbError)` parses but
  matches everything (documented in README). Implement first-class error
  types: builtin hierarchy (`DbError` with `UniqueViolation` etc.,
  `HttpError`, `ValidationError`, `JwtError`) + user-declared
  `error Name { fields }`. Production motivation: jwc-shortener cannot
  distinguish a slug PK collision from any other DB failure — the correct
  retry-on-conflict pattern is unwritable today; every collision is a
  user-facing 500.
- **Builtin gaps from production use**: `substring(s, start, len)` /
  `take(s, n)` (today: a `split(s, "")` for-loop), `client_ip()` (see
  Phase 5 trusted-proxy work), and a reviewed pass over `docs/builtins.md`
  for similar everyday holes before the surface freezes at 1.0.
- **Static type checker (gradual).** Function signatures already carry types;
  add a check pass: declared return type vs. actual returns, call-site arity
  + argument types, entity field types in expressions. Unannotated code stays
  dynamic — no breakage.
- **Module hygiene**: re-verify `public/private` enforcement in the AOT path
  (the codegen header in `src/native_build.rs` notes visibility is "NOT
  re-checked here" — make `validate_program` provably sufficient or add the
  check).
- Decide and document: integer overflow behavior, float formatting (the
  custom `format_float`), string encoding guarantees, `==` semantics across
  types.

Exit criteria: conformance tests for every error type; type checker on by
default with `--no-typecheck` escape hatch for one transition release.

---

## Phase 4 — Data layer hardening

- **[1.0-blocker] Migration safety**: `migrate up` inside a transaction per
  migration; checksum recorded migrations and refuse to run if an applied
  file was edited; `jwc migrate status`; dry-run mode printing SQL.
- **[1.0-blocker] Transaction semantics doc + tests**: nested `transaction`
  rejection is documented — add savepoint support or a clear compile error;
  test rollback under panic, mid-statement DB disconnect, and pool
  exhaustion.
- **[1.0-blocker] Atomic partial updates.** The only update form today is
  whole-row `update x in Db.Table` after a read — a read-modify-write that
  silently loses writes under concurrency (observed live: jwc-shortener's
  `hits` counter undercounts on parallel redirects; the workaround is
  `raw_sql`). Add `update Link set hits = hits + 1 where Link.code == @c;`
  — column expressions compile to a single SQL `UPDATE`, validated against
  entity fields like `where`/`orderby` already are. Document the
  read-modify-write hazard on the whole-row form.
- **Connection pool resilience**: retry-with-backoff on transient errors,
  health-check query option, pool metrics (in-use/idle/wait time) exposed via
  the existing `JWC_SERVER_METRICS`.
- **Prepared-statement cache bounds**: `prepare_cached` per connection —
  verify eviction for apps with many dynamic query shapes.
- **`json()` passthrough fix**: keep the fast-path, but (a) validate strings
  in debug/`jwc run`, (b) add `json_unchecked()` for the hot path, so the
  insecure-by-default footgun documented in the README is closed by 1.0.
  **[1.0-blocker]**
- Defer multi-driver (Redis/ClickHouse) to post-1.0 — one excellent driver
  beats three half ones. State this in ROADMAP.

Exit criteria: chaos test (kill Postgres mid-request burst) leaves no
poisoned pool, no stuck transactions, clean 503s.

---

## Phase 5 — Server reliability & operations

- **[1.0-blocker] Graceful shutdown**: SIGTERM/SIGINT → stop accepting,
  drain in-flight requests (deadline), flush job queue, close pool. Required
  for Kubernetes/rolling deploys.
- **[1.0-blocker] Request hardening defaults**: body size limit, header
  limits, read/write timeouts, WS max frame size — all env-tunable, all
  documented with defaults.
- **Persistent job queue option**: current in-process queue loses jobs on
  restart (documented). Add a Postgres-backed driver
  (`JWC_QUEUE_DRIVER=postgres`) with at-least-once delivery + dead-letter
  table; keep in-memory as the default for dev.
- **Observability**: structured JSON logs (`JWC_LOG_FORMAT=json`), request
  IDs propagated to logs + error handler, Prometheus `/metrics` endpoint
  (latency histograms, pool stats, queue depth), optional OTLP traces.
- **Response-phase middleware (`after` hooks).** Middleware today runs only
  before the handler — it cannot observe status or duration. jwc-shortener
  hand-rolls a metrics table with `latency_ms` hardcoded to 0 and
  `status` hardcoded to 200 because nothing better is expressible. Add
  `after { ... }` blocks (or a second middleware phase) exposing
  `response_status()` / `response_duration_ms()` — and note that the
  built-in `/metrics` endpoint below removes the need for hand-rolled
  request-logging tables entirely.
- **Trusted-proxy client IP.** `header("x-forwarded-for")` is spoofable and
  wrong behind Cloudflare (`CF-Connecting-IP`). Add a `client_ip()` builtin
  driven by `JWC_TRUSTED_PROXIES` / `JWC_REAL_IP_HEADER` config, so
  rate-limit and audit code stops re-deriving this per app.
- **Health endpoints**: `/healthz` (process) and `/readyz` (DB reachable),
  built-in, documented for load balancers. Validated need: the hand-rolled
  `/healthz` in jwc-shortener has no DB check, so k8s probes stay green
  through a database outage.
- **Config validation at boot**: fail fast with a table of every JWC_* env
  var, its parsed value, and its source.

Exit criteria: 72h soak test under load with restarts — zero lost responses
on graceful restart, memory flat.

---

## Phase 6 — Security program

- **[1.0-blocker] `SECURITY.md`** — ✅ landed (this PR): private disclosure
  via GitHub advisories, 72h ack / 14d high-severity fix SLA, scope defined.
- **Dependency hygiene** — ✅ partially landed: `cargo audit` + `cargo deny`
  run in CI (`security.yml` + `deny.toml`, currently `continue-on-error`)
  and Dependabot covers cargo, GitHub Actions, and both npm trees (this PR).
  Remaining: triage the current advisory set, then flip both jobs to
  blocking.
- **Release integrity** — ✅ landed (this PR): release workflow publishes
  `.sha256` checksums for every artifact; `install.sh` / `install.ps1`
  verify them, warning (not failing) on pre-checksum releases. Remaining:
  consider Sigstore/`cosign` signing post-1.0.
- **Threat-model pass** on: route parsing (path traversal in `{param}`),
  header injection via `response()`, SSRF surface of `http_get`/`fetch_json`
  (document; optional allowlist env), JWT (alg confusion already prevented —
  add `exp` enforcement test), template strings in SQL contexts (verify no
  interpolation path reaches SQL text).
- **Secrets**: never log connection strings; redact `JWC_SMTP_PASSWORD` and
  friends from error chains and boot config output.

Exit criteria: external or community security review of the HTTP + DB
surface; all findings ≥ medium fixed.

---

## Phase 7 — Performance with receipts

Largely underway: [`http-framework-benchmark`](https://github.com/Nodirbek-Abdulaxadov/http-framework-benchmark)
already exists with equal-workload methodology (bombardier, sequential
isolation, raw JSON archived, reproduce scripts) and strong results — 2nd on
`/async-delay` ahead of rust-axum, 0 errors across 4.48M requests. Remaining
gaps:

- **[1.0-blocker] Add DB workloads.** JWC is a database-focused language, yet
  the suite has no Postgres endpoint. Add TechEmpower-style `/db` (single
  query), `/queries?n=20` (multi query), `/updates`, and a `select ... with`
  relation endpoint — this is the benchmark that actually sells JWC, and it
  exercises the pool/pipeline code Phase 4 hardens.
- **Linux runs + CI automation.** Current numbers are Windows-only,
  hand-run. Add a Linux session and a GitHub Actions job that re-runs the
  suite per jwc release and commits the summary JSON; gate releases on a
  regression budget for the conformance workload.
- **Link it.** The jwc README should link the benchmark repo prominently —
  it is the strongest positioning asset the project has, currently invisible.
- Track p50/p99 + RSS per release alongside RPS.
- Native AOT: close the documented feature gap (DB, try/catch, await,
  middleware in `src/native_build.rs` header) **or** explicitly scope 1.0 AOT
  as "stateless route tier" — either is fine; ambiguity is not. The struct
  monomorphization from Phase 1 lands here as the headline `/json-large`
  improvement.

Exit criteria: benchmark repo linked from README, DB workloads measured on
Linux + Windows, per-release CI regression gate active.

---

## Phase 8 — Developer experience & ecosystem

- **Docs site completion** (docusaurus tree already exists in `docs/`):
  spec, tutorial ("zero to deployed CRUD in 15 min"), builtin reference
  (generated from code, replacing hand-maintained `docs/builtins.md`),
  deployment guides (Docker, systemd, k8s) — a `jwc`-official Dockerfile +
  example compose with Postgres.
- **Official Docker image + musl static builds.** jwc-shortener's Dockerfile
  curls a release tarball and pins `rust:1.90-slim` purely to match the
  build host's glibc (2.36 vs 2.40 broke older bases). Publish
  `ghcr.io/.../jwc:<version>` (builder + slim runtime variants) and a
  `x86_64-unknown-linux-musl` static binary so app Dockerfiles stop caring
  about glibc at all. Document the proven migrate-as-init-container k8s
  pattern in the deployment guide.
- **`jwc fmt`** (formatter exists at `src/fmt.rs` — finish + document) and
  **`jwc upgrade`** (codemod for deprecations).
- **LSP parity**: go-to-definition, rename, completion for entity fields and
  route paths; ship the VS Code extension to the Marketplace.
- **Package registry**: the resolver/lockfile work is done; stand up the
  registry service (MinIO mirror env var already hints at infra) with
  namespaced packages and yank support — or document path/git deps as the
  1.0 story and defer the registry.
- **Project templates**: `jwc new --template api|auth|jobs`.

---

## Phase 9 — Release engineering & 1.0

- Release-candidate process: `1.0.0-rc.N` for ≥ 4 weeks with at least 2
  external pilot projects in production-like use.
- Supported-platforms matrix in CI (linux x86_64/aarch64, macOS, Windows) —
  build + conformance on each.
- LTS statement: how long 1.x receives security fixes.
- 1.0 ship gate = every **[1.0-blocker]** above closed + conformance suite
  green on all platforms + zero open `severity:high` issues.

---

## Sequencing & effort (single maintainer + AI-assisted, rough)

| Phase | Depends on | Est. effort |
|---|---|---|
| 0 Spec & contract | — | 3–4 wk |
| 1 Unified value model | 0 (conformance suite) | 4–6 wk |
| 2 Code health & diagnostics | 1 | 3–4 wk |
| 3 Type system | 1, 2 | 4–6 wk |
| 4 Data layer | 1 | 3–4 wk |
| 5 Server reliability | 1 | 3–4 wk |
| 6 Security | 2, 4, 5 | 2–3 wk + ongoing |
| 7 Performance | 1, 4, 5 | 2 wk + ongoing |
| 8 DX & ecosystem | parallel from Phase 2 | ongoing |
| 9 Release | all blockers | 4+ wk RC bake |

Realistic horizon to 1.0: **6–9 months** of focused work. The biggest risk is
scope creep — new surface area (drivers, registry, AOT features) before
Phases 1–2 land. Recommendation: feature freeze on new language syntax until
the unified value model ships.

---

## Immediate next sprint (2 weeks, replaces `next-sprint.md` candidates)

1. Land the conformance-suite skeleton (Phase 0) — it de-risks everything
   after it.
2. Spike struct monomorphization for ONE entity in AOT (`/json-large`
   endpoint of the benchmark as the measuring stick) + `Value::Record`
   shape table design doc for the interpreter.
3. Add `Span` to `Token`/AST and convert the top-10 most-hit parser errors
   to file:line:col output.
4. Cheap, high-signal wins shipped alongside this plan: `cargo audit` +
   `cargo deny` in CI (see `.github/workflows/security.yml`), Postgres `/db`
   endpoint added to the benchmark repo, benchmark link in README.
5. File the jwc-shortener findings as `dogfooding`-labelled issues so they
   don't live only in this document: atomic `update ... set` (Phase 4),
   typed `UniqueViolation` catch (Phase 3), `substring`/`take` +
   `client_ip()` builtins (Phase 3/5), response-phase middleware (Phase 5),
   lint warnings in the default build path (Phase 2), official Docker
   image + musl build (Phase 8). The two smallest — `substring` builtin and
   the unused-middleware lint fixture — are good first issues to close
   within this sprint.

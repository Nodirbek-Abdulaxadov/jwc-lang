# Changelog

All notable changes to JWC are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased] — dogfooding patches

Phase 2 and Phase 3 follow-ups to `PRODUCTION_READINESS_PLAN.md`,
shipped together since each is small.

### Added
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
- **Graceful shutdown listens for SIGTERM on Unix.** The pre-existing
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

# Contributing to JWC

Thanks for taking the time to dig in. This document covers what you need to
get the compiler/runtime building locally, where the moving parts live, and
the conventions we follow for tests, style, and commits.

If you only read one other file, read [`CLAUDE.md`](./CLAUDE.md) — it is the
deep architectural reference and is kept in sync with the code.

## Getting set up

Requirements:

- **Rust stable**, edition 2021 (any recent stable toolchain works; the
  workspace uses `edition = "2021"` and `Cargo.lock` is committed).
- **Docker** *optional*, only needed if you want to run the Postgres-backed
  integration suite locally.

```bash
git clone https://github.com/<org>/jwc-lang
cd jwc-lang

cargo build                          # debug build of jwc + jwc-lsp
cargo build --release                # release build
cargo build --bin jwc-lsp            # LSP binary only
cargo test                           # unit tests (integration_db auto-skips without Docker)
```

For fast iteration on a `.jwc` program, run via cargo rather than the
installed binary:

```bash
cargo run -- check examples/testapp/main.jwc
cargo run -- run   examples/testapp
cargo run -- serve examples/testapp --port 8080 --watch
```

Do **not** run `install-from-source.{sh,ps1}` to test your changes — those
overwrite the user's installed `jwc`.

## Project layout

Two binaries (`jwc`, `jwc-lsp`) live in a single Rust crate. Key files
under `src/`:

- `lexer.rs` — hand-written tokenizer (raw strings, template strings, comments).
- `parser.rs` — recursive-descent parser. Also hosts `validate_program`,
  which re-walks the AST for compile-time dbcontext/entity/column checks.
- `ast.rs` — every AST node lives here.
- `runner.rs` — the async tree-walking interpreter `Vm` (`#[async_recursion]`).
  All built-ins live in `call_builtin` / `call_function`.
- `engine.rs` — `deadpool-postgres` + `tokio-postgres` pool, prepared
  statement cache, optional TTL result cache, TLS via `tokio-postgres-rustls`.
- `server.rs` — axum + tokio HTTP server; each request is `tokio::spawn`'d.
- `native_build.rs` — AST → Rust source for `jwc build --native` (the AOT path).
- `queue.rs` — in-process background job queue.
- `migrate.rs` + `schema_diff.rs` — `jwc migrate new/up/down` with diff-based generation.
- `sql.rs` — Postgres DDL generator for `jwc gen-sql`.
- `lint.rs` — AST walk for `jwc lint` (W001/W002 warnings).
- `diag.rs` — byte offset → `(line, col)` mapping.
- `src/bin/jwc_lsp.rs` — the LSP server (tower-lsp, stdio).

For the full architectural picture (async stack, ENGINE singleton,
project loading, migration locking), read [`CLAUDE.md`](./CLAUDE.md).

## Compilation pipeline

`lexer.rs` → `parser::parse_program` → `parser::validate_program` →
optionally `lint::lint_program` → `runner::run_main` (or
`sql::generate_postgres_schema` for `gen-sql`, or `native_build` for
`--native`).

**Rule when adding a new syntactic form:** wire it through every layer it
touches. Skipping one of these is the most common source of "works in
interpreter, breaks on `jwc build --native`" regressions.

- **New token / keyword?** `lexer.rs` (`Token`, `TokenKind`, keyword table).
- **Always:** `ast.rs` (new enum variant) + `parser.rs` (parsing rule).
- **Always:** `parser::validate_program` (compile-time invariants — column
  existence, type membership, "did you mean" hints).
- **Always:** `runner.rs` (interpreter behaviour, async if it can suspend).
- **New built-in function?** Also add it to the `BUILTINS` list in
  `src/native_build.rs`, otherwise the AOT codegen will reject the call.

See [`ROADMAP.md`](./ROADMAP.md) for which phases each subsystem belongs to
and which gaps are intentional deferrals (LLVM IR backend, cross-target
native builds, multi-catch dispatch, etc.).

## Tests

```bash
cargo test                                       # unit + cheap integration tests
cargo test --test integration_db                 # Postgres suite (needs Docker)
cargo test --test integration_db -- some_name    # single test by name
cargo test -- --nocapture                        # show eprintln! output
```

`tests/integration_db.rs` boots Postgres via `testcontainers`. Without
Docker it prints `SKIPPED` via `eprintln!` and returns `Ok(())` — do not
treat skipped output as a pass when verifying DB-touching work; rerun on a
host with Docker before claiming the change is done. Tests are serialised
behind a global `Mutex` because `engine::ENGINE` is a process-wide
`OnceLock`.

Adding tests:

- **Small smoke test for a validator or parser rule** —
  see [`tests/typed_catch.rs`](./tests/typed_catch.rs) for a minimal
  pattern: parse a snippet, assert on the resulting error / AST.
- **End-to-end through `project::load`** —
  see [`tests/imports.rs`](./tests/imports.rs) for the larger pattern:
  set up a temp project tree, exercise the full loader + validator + runner.

## Style

Rustfmt is configured in [`rustfmt.toml`](./rustfmt.toml)
(`max_width = 100`, `edition = "2021"`). Clippy thresholds are in
[`clippy.toml`](./clippy.toml).

CI (`.github/workflows/ci.yml`) gates every PR on:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Run both locally before pushing — a 30-second `cargo fmt && cargo clippy`
loop saves a CI round-trip.

## Commit and PR conventions

Commit messages use a short prefix matching the existing log:

- `feat:` — user-visible new capability
- `fix:` — bug fix
- `perf:` — performance change with no behaviour change
- `refactor:` — internal restructuring, no behaviour change
- `docs:` — README / ROADMAP / CONTRIBUTING / inline doc updates
- `test:` — tests only
- `ci:` — workflow / build infrastructure

Keep commits small and focused. One logical change per commit is much
easier to review (and to revert) than a giant "various improvements" blob.

For PRs:

- Title: same prefix style as commits.
- Body: one paragraph of *why*, plus a short bullet list of *what*.
- If the change overlaps a `ROADMAP.md` phase (e.g. Phase 10.2 tracing,
  Phase 3.1 LSP), reference the phase number so we can update the roadmap
  in the same PR or the next one.
- If you touched `parser.rs`, mention whether you also updated `validate_program`.
- If you added a built-in, confirm it is also in the `native_build.rs` `BUILTINS` list.

## Where to start

Open issues labelled **`good-first-issue`** are the easiest ramp.
If none are open right now, these are good self-directed picks that almost
always need work:

- Documentation typos and clarifications in `README.md` / `ROADMAP.md` /
  `CLAUDE.md`.
- A new example program under `examples/` (something more interesting than
  `testapp` — e.g. a small auth + paginated list service).
- Smoke tests in the [`tests/typed_catch.rs`](./tests/typed_catch.rs) style
  for any new validator or parser rule that currently lacks coverage.
- Polishing error messages in `runner.rs` — "did you mean" hints, missing
  context in panics, error JSON shape consistency.

For larger work, see the **Priority Timeline** at the bottom of
[`ROADMAP.md`](./ROADMAP.md): the current focus is Phase 10.1 (real
benchmark numbers), then Phase 10.2 (`tracing` + OpenTelemetry), then
LSP completeness (Phase 3.1+).

## Shipping a new builtin

A new builtin function (`my_helper(...)`) is one of the most common
contributor PRs. It also touches the most layers — skipping any one
of them produces a "works in `jwc run` but breaks under `jwc build
--native`" regression, or a builtin that has no spec entry and no
test pinning its contract. The recipe below is the full checklist:

1. **Interpreter** — `src/runner/builtins.rs` (or `src/runner/mod.rs::Vm::call_builtin`).
   Wire the new name to a function returning a `Value`. Async if it
   suspends (DB, HTTP, sleep). Null-propagation rule: if every other
   string builtin returns `null` on `null` input, yours should too.

2. **Validator (optional, recommended)** — `src/parser.rs::validate_program`.
   If the builtin has type-checkable invariants (arg count, declared
   arg types), assert them at compile time and bail with a numbered
   E-code so the message is grep-friendly. See E011 / E012 / E013 for
   the prefix shape.

3. **Native AOT codegen** — `src/native_build.rs`.
   Add the builtin name to the `BUILTINS` allow-list so
   `jwc build --native` accepts the call instead of rejecting it as
   unknown. If the call needs a non-trivial body, emit the helper
   into `src/native_prelude.rs.in` (or its `_db.rs.in` sibling for
   DB-touching code) as `fn jwc_b_<name>(...)`. Don't add a new
   `V::Variant` here — the codegen has 25+ V match arms in the
   prelude that need updating for each new variant; reach for an
   existing variant (e.g. `V::Str(JwcStr::from(...))`) instead.

4. **Spec entry** — `docs/spec/builtins.md`.
   Use the entry template (signature, errors, notes, tests). The
   `Tests:` field names the conformance case(s) that pin the
   contract — write the test in step 5 and back-fill the name here.

5. **User-facing docs** — `docs/builtins.md` (the existing
   hand-maintained reference) AND `docs/docs/...` (docusaurus tree)
   if the builtin belongs to a documented surface (HTTP / DB /
   queue / observability). At v1.0 the generated reference replaces
   the hand-maintained one; until then, keep both in sync.

6. **Conformance case** — `tests/conformance/cases/case_<name>.jwc`
   + `case_<name>.stdout.txt`. Register the case name in
   `tests/conformance.rs::REGISTERED_CASES` AND add a
   `conformance_test!(case_<name>);` line. The discovery test will
   yell if you forget the registry, the macro will yell if you
   forget the test list. Add `// CONFORMANCE: interpreter-only` as
   the first line of the `.jwc` file when the builtin isn't yet
   supported in the AOT path.

7. **CHANGELOG** — under the next unreleased version's "Added"
   section, naming the builtin and pointing at the spec entry.

A small builtin (string helper, hash, env access) is ~50 LOC across
the seven files; a larger one (HTTP, DB) is mostly the codegen step
3.

## `unwrap()` policy

`PRODUCTION_READINESS_PLAN.md` Phase 2 tracks the open `unwrap()`
budget — 1.0 forbids them in non-test code unless the unwrap is
provably safe and the proof is captured in the code.

### Audit finding (Sprint 1-5 close-out)

The plan originally listed ~340 unwraps as the open budget. The
actual count is ~120 distinct `.unwrap()` call sites; the inflated
number came from sites appearing in both `mod.rs` and the matching
`tests.rs` sub-module being counted separately. Of the 120, **119
live inside `#[cfg(test)]` modules** (allowed by policy) and **1
lives in production code** (now converted to `.expect(...)`). The
post-audit production unwrap count is **0**.

### Categories

Every unwrap belongs to one of three categories — pick the right
one before reaching for `.unwrap()`:

- **A — init / lazy / "just checked" patterns**: a `get()` after a
  matching `is_some()` check, a `Mutex::new(...)` that can't fail,
  a `OnceLock` you populated three lines above. Use
  `.expect("INVARIANT: <reason>")`. The `INVARIANT:` prefix is the
  proof obligation: if you can't name the invariant in one line,
  the unwrap isn't actually safe.
- **B — user input / parse / I/O**: anything where the failure is
  a real runtime condition (bad JSON, missing env var, network
  blip). Use `?` with `anyhow::Context` (`.context("parsing X")?`)
  so the error carries the call site forward.
- **C — Mutex poisoning**: a poisoned mutex is a panic in another
  thread, not recoverable in the current one. Use
  `.expect("Mutex poisoned: <name>")` — the prefix lets the audit
  script distinguish these from category A.

### Marker conventions

The audit script greps for these prefixes:

- `INVARIANT: ...` — category A.
- `Mutex poisoned: ...` — category C.
- `// SAFETY: ... [unwrap budget exempt: <reason>]` — escape hatch
  for the rare case the message itself doesn't fit the
  `expect()` (e.g. expansion inside a macro). Reserve for cases
  where A/B/C genuinely don't apply.

### Lint roadmap

- **Today**: `[lints.clippy] unwrap_used = "allow"`, `expect_used =
  "allow"` workspace-wide. Production count is 0, but the lint
  stays `allow` because test code legitimately uses both and we
  haven't yet drawn the per-module cfg boundary.
- **Next**: per-module
  `#![cfg_attr(not(test), warn(clippy::unwrap_used))]` on every
  `src/*.rs` and `src/**/mod.rs`. This warns in production paths
  while leaving `#[cfg(test)]` modules untouched.
- **1.0 gate**: per-module `deny(clippy::unwrap_used)` for
  non-test code, `allow` for tests. The CI check becomes
  `cargo clippy --lib -- -D clippy::unwrap_used`.

Every unwrap → expect conversion is still welcome as a small PR;
the audit script counts down from 119 (test) + 0 (prod).

## License and Code of Conduct

This project is distributed under the **MIT** license unless a
`LICENSE` file in the repository root says otherwise. By submitting a
contribution you agree it is licensed under the same terms.

We don't have a dedicated `CODE_OF_CONDUCT.md` yet; the default is the
[Contributor Covenant](https://www.contributor-covenant.org/) —
be respectful, assume good faith, keep discussion focused on the code.

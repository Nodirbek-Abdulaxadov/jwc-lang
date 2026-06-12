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

## `unwrap()` policy

`PRODUCTION_READINESS_PLAN.md` Phase 2 tracks the open `unwrap()`
budget — 1.0 forbids them in non-test code unless the unwrap is
provably safe and the proof is captured in the code.

Practical rule for new code:

- **Tests** (`#[cfg(test)]`, `#[test]` modules, integration suites):
  `unwrap()` is fine — a panic is a test failure.
- **Production code paths**: prefer `?` to propagate; when that
  doesn't apply, use `.expect("INVARIANT: <why this can't fail>")`
  instead of `.unwrap()`. The message is the proof obligation: if
  you can't write it, the unwrap is unsafe and needs a real error
  path.
- **Recipe for unwrap conversions**: search for `.unwrap()`,
  identify the static invariant that guarantees `Some`/`Ok`, write
  it as the `expect()` message starting with `INVARIANT:`. Anything
  else gets a real error return.

The 1.0 gate is `cargo clippy -- -D clippy::unwrap_used` over the
whole tree. Until then, every unwrap → expect conversion is welcome
as a small PR.

## License and Code of Conduct

This project is distributed under the **MIT** license unless a
`LICENSE` file in the repository root says otherwise. By submitting a
contribution you agree it is licensed under the same terms.

We don't have a dedicated `CODE_OF_CONDUCT.md` yet; the default is the
[Contributor Covenant](https://www.contributor-covenant.org/) —
be respectful, assume good faith, keep discussion focused on the code.

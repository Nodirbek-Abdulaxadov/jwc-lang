# Contributing to JWC

Thanks for taking the time to dig in. This document covers getting the
compiler building locally, where the moving parts live, and the
conventions for tests, style and commits.

**Read [`docs/spec/v1/`](docs/spec/v1/) first.** It is normative: where
this document, the README, or the code disagree with it, the spec is
right. [`README.md`](README.md) is the shortest tour of the language.

> **The language changed.** v0.25.0 replaced the 0.9.x grammar with the
> one in `docs/spec/v1/` and deleted the old front-end. If you are
> reading a guide that mentions `entity`, `dbcontext`, `with`, `via`,
> `validate body` or `jwc build --native`, you are reading
> [`docs/archive-0.9/`](docs/archive-0.9/) — kept because 0.9.x binaries
> are deployed, not because it describes this compiler.

## Getting set up

Requirements:

- **Rust stable**, edition 2021. `Cargo.lock` is committed. CI currently
  runs 1.98; a local toolchain older than CI's will miss clippy lints
  that then fail the PR, so `rustup update stable` before you start.
- **Postgres**, for the suites that need one. Not optional if you are
  touching the query, schema, or migration layers — see Tests.
- **Redis**, only for the `redis` feature's suites.

```bash
git clone https://github.com/just-web-code/jwc-lang
cd jwc-lang

cargo build                     # debug
cargo build --release           # release
cargo build --features redis    # the redis.* surface; off by default
cargo test                      # everything that needs no server
```

For fast iteration on a `.jwc` program, run through cargo rather than an
installed binary:

```bash
cargo run -- check docs/spec/v1/sample
cargo run -- explain docs/spec/v1/sample     # every query, with its SQL
cargo run -- serve docs/spec/v1/sample --port 8080
```

Do **not** run `install-from-source.{sh,ps1}` to test a change — they
overwrite the user's installed `jwc`.

## Project layout

One crate, one binary (`jwc`). The language server is `jwc lsp`, not a
separate binary. Under `src/`:

**Front end**
- `lexer.rs`, `token.rs` — hand-written tokenizer.
- `parser.rs`, `ast.rs` — recursive-descent parser and every AST node.
- `fmt.rs` — canonical form, for `jwc fmt`.
- `diag.rs` — diagnostics: codes, spans, byte offset → `(line, col)`.

**Middle**
- `symbols.rs` — the program-wide symbol table.
- `check.rs` — the type checker; `types.rs` — the value lattice.
- `model.rs` — the resolved schema model; `naming.rs` — physical names
  and the versioned constraint-naming function.
- `wiring.rs` — routing, middleware composition, and the error model.
- `imports.rs`, `packages.rs`, `registry.rs` — package resolution.

**Queries and schema**
- `query.rs` — the query plan: bindings, the join attachment tree.
- `query_sql.rs`, `sql.rs` — SELECT emission, and the write statements.
- `cursor.rs` — keyset cursors.
- `ddl.rs` — DDL emission for `jwc gen-sql`.

**Migrations**
- `snapshot.rs` — the schema as a database holds it.
- `diff.rs` — two snapshots in, typed operations out.
- `apply.rs`, `migrate.rs` — `up` / `down` / `status` / `verify`.

**Runtime**
- `exec.rs` — the interpreter; `exec_call.rs` — call dispatch for
  builtins, free functions and service methods.
- `serve.rs` — the request pipeline and the server driving it: a manual
  hyper-util accept loop (needed for both `header_timeout` and TLS),
  graceful shutdown, and the operational endpoints.
- `engine.rs`, `db.rs` — the Postgres pool; `redis_engine.rs` — the
  Redis pool, behind the `redis` feature.
- `config.rs` — env-driven runtime config.

**Tooling**
- `lsp.rs`, `openapi.rs`, `hash.rs`, `jwt.rs`, `jwks.rs`,
  `password.rs`, `locks.rs`.

There is no native/AOT backend. `jwc serve` is the only execution path.

## Adding a syntactic form

Wire it through every layer it touches — skipping one is the most common
source of "parses but does nothing":

1. **New token or keyword?** `lexer.rs` and its keyword table.
2. **Always:** `ast.rs` (the node) and `parser.rs` (the rule).
3. **Always:** `fmt.rs`. A form the formatter doesn't know will be
   mangled or dropped by `jwc fmt`, and `tests/fmt.rs` checks
   idempotency.
4. **Always:** `check.rs` / `wiring.rs` — the compile-time invariants,
   raised as a numbered diagnostic.
5. **Always:** the interpreter, in `exec.rs` / `exec_call.rs`.
6. **The grammar** — `docs/spec/v1/grammar.ebnf` plus the normative
   prose in the matching `docs/spec/v1/*.md`.
7. **The sample**, if the form is one a real application would use.
   `docs/spec/v1/sample/` is what the compiler is graded against, and
   `spec-coverage.json` maps each construct to the clause defining it.

## Adding a builtin

Builtins are namespaced (`string.*`, `request.*`, `hash.*`, …) and
dispatched by name in `src/exec_call.rs`.

1. **Dispatch + body** in `exec_call.rs`. Match the null-propagation
   convention of its neighbours: if every other `string.*` builtin
   returns null on null input, yours does too.
2. **Arity and types** in `check.rs`, so a wrong call is a diagnostic
   rather than a runtime surprise.
3. **Spec entry** in `docs/spec/v1/builtins.md` — signature, errors,
   notes. This is the normative definition; the implementation follows
   it, not the other way round.
4. **A test** pinning the contract, including the failure modes.
5. **CHANGELOG** under the unreleased version's "Added", naming the
   builtin and pointing at the spec entry.

If the builtin can fail in a way a program should be able to catch, it
needs an error type in the model of `docs/spec/v1/errors.md`, not a bare
runtime panic.

## Reviewer cross-references

- **Security surfaces.** When a change touches the HTTP server, the SQL
  layer, the JWT helpers, or a log path that handles a connection
  string, re-read [`docs/spec/v1/security.md`](docs/spec/v1/security.md)
  and update the relevant section. The two language promises — every
  value is a bind parameter, and a result is `Raw` until projected — are
  load-bearing; a change that weakens either needs to say so out loud.
- **Configuration.** `server { }` keys are specified in
  [`docs/spec/v1/config.md`](docs/spec/v1/config.md) and validated in
  `wiring.rs`. A new key must be *rejected when misspelled* (`E1206`) —
  the check is not optional, because a silently-ignored key is a
  security default that quietly didn't apply.
- **Diagnostic codes.** `E####` / `W####` are append-only: never reuse a
  code for a different condition. The spec names them; `src/diag.rs` and
  its callers raise them.
- **Fuzzing.** When a change touches the tokenizer or the parser, run
  the harness before pushing — see [`fuzz/README.md`](fuzz/README.md).

## Tests

```bash
cargo test                                       # no server needed
cargo test -- --nocapture                        # show eprintln! output
```

Several suites are opt-in on a real dependency, and **a SKIPPED line is
not a pass** — a suite that skips has verified nothing. Do not claim a
DB-touching change is done off a skipped run:

```bash
export JWC_V1_DATABASE_URL=postgres://…          # a DB it may drop schemas in
export JWC_V1_PG=postgres://…                    # same server, psql goldens
export JWC_TEST_REDIS_URL=redis://127.0.0.1:6379 # flushed between tests
export CURSOR_SECRET=ci-cursor-secret

cargo test --features redis --test http_golden --test hardening
cargo test --test migrate_apply --test migrate_golden --test migrate_roundtrip
cargo test --test jwc_test --test sql_golden --test ddl_golden --test raw_hatch
cargo test --features redis --test integration_redis
cargo test --test serve_listener                 # needs a socket and openssl
```

Two guards exist because the failure they catch is invisible:

- `hardening.rs::every_test_suite_is_named_in_ci` fails when a suite
  exists that no CI job runs. It was added after seven suites were found
  running nowhere for months.
- `docs_parse.rs::the_spec_coverage_map_is_current` re-runs
  `check_sample.py` and diffs, so `spec-coverage.json` cannot drift from
  the sample.

If you add a suite, add it to `.github/workflows/ci.yml` in the same
change — the first guard will fail the PR otherwise, which is the point.

Writing tests: the suites in `tests/` are the pattern to copy.
`parse_corpus.rs` / `type_corpus.rs` / `wiring_corpus.rs` are
table-driven over a directory of cases and are the cheapest place to pin
a front-end rule. `http_golden.rs` drives real requests. `hardening.rs`
is where behaviour that has no natural home goes — including assertions
about the repo itself.

## Style

Rustfmt is configured in [`rustfmt.toml`](rustfmt.toml), clippy in
[`clippy.toml`](clippy.toml). CI gates every PR on:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Run both locally first. Note clippy is version-sensitive: a lint your
older toolchain doesn't know will still fail CI.

### `unwrap()` policy

**The budget is closed.** `src/lib.rs` carries
`#![cfg_attr(not(test), deny(clippy::unwrap_used))]`, so production code
is unwrap-free and the compiler enforces it. `#[cfg(test)]` modules
unwrap freely and that is fine.

In production code:

- **A "just checked" value** — use `.expect("INVARIANT: <reason>")`. The
  prefix is a proof obligation: if you cannot state the invariant in one
  line, it is not actually safe.
- **User input, parsing, I/O** — use `?` with context
  (`.context("parsing X")?`) so the error carries its call site.
- **A poisoned mutex** — see `src/locks.rs`. `lock_recover` /
  `wait_recover` exist because a poisoned lock turns one panic into a
  permanently dead subsystem; the module documents when *not* to use
  them.

## Commit and PR conventions

Keep commits small and focused — one logical change each. The log uses
short prefixes (`feat:`, `fix:`, `perf:`, `refactor:`, `docs:`, `test:`,
`ci:`) but a clear sentence beats a prefix on a change that doesn't fit
one.

A commit message should say **why**, and what you did to be sure. The
most useful line in a fix is the one describing how the bug was
reproduced before it was fixed, and how you confirmed the fix — "checked
that reverting the change fails the test" is worth more than a summary
of the diff, which the diff already contains.

For PRs:

- Body: one paragraph of *why*, then what changed.
- If you touched `parser.rs`, say whether `fmt.rs` and the checker were
  updated too.
- If you added a builtin or a syntactic form, say where its spec entry
  is.
- If a claim in the PR body is not covered by a test, say so plainly
  rather than implying it is.

## Where to start

Issues labelled **`good-first-issue`** are the easiest ramp. Failing
that, work that reliably needs doing:

- **The four documents this one belongs to.** `SEMVER.md`,
  `DEPRECATION.md`, `SECURITY.md` and this file were rewritten for v1
  late; if you find a claim here that the code contradicts, that is a
  bug in the document and a fix is welcome.
- **Diagnostics.** A confusing message with a correct code is a good,
  self-contained change: better spans, a "did you mean", the clause of
  the spec that explains the rule.
- **Corpus cases.** A front-end rule with no case in
  `parse_corpus` / `type_corpus` / `wiring_corpus` is a rule that can
  regress silently.
- **`docs/archive-0.9/`** is frozen; do not fix things there beyond
  broken links.

Larger work is tracked in [`ROADMAP.md`](ROADMAP.md). The next milestone
is **v1.0.0-rc.1**: the conformance corpus blocking in CI, an external
review, and a migrated pilot application.

## Licence and Code of Conduct

**The licence is undecided.** There is no `LICENSE` file in the
repository root, and that is deliberate rather than an oversight —
`Cargo.toml` and `deny.toml` both record the crate as workspace-private
until a licence decision lands, and the crate is `publish = false`.

Do not assume a licence from the sibling components: the VS Code
extension under `vscode-extension/` and the `redis` package repository
each ship their own MIT `LICENSE`, and those cover only themselves.

Practically, this means a contribution cannot yet be accepted under
stated terms. If you want to contribute something substantial, open an
issue first so the licence question can be settled before you spend the
effort.

There is no `CODE_OF_CONDUCT.md`; the default is the
[Contributor Covenant](https://www.contributor-covenant.org/) — be
respectful, assume good faith, keep discussion on the code.

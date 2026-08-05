# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

JWC is a backend-focused programming language with first-class HTTP routes,
entities, SQL generation, and Postgres execution. The repository ships the
Rust compiler/interpreter (`jwc`), the language server (`jwc-lsp`), a VS Code
extension, and example projects under `examples/`.

`.jwc` source is read end-to-end at process start (no separate IR file); the
default runtime is an interpreter. `jwc build` (alias `jwc bundle`) without
flags still does launcher + runtime bundling. `jwc build --native` is the
real AOT path: `src/native_build.rs` emits Rust source from the AST, shells
out to `cargo` and produces a standalone tokio binary. **LLVM IR backend
and cross-target native build matrix are declared Non-goals** (see
`ROADMAP.md` Non-goals). `--native` is the Rust codegen path on Linux
x86_64 (+ musl) only; that's intentional, not a deferral.

## Build / run commands

```bash
cargo build                          # debug build of jwc + jwc-lsp
cargo build --release
cargo build --bin jwc-lsp            # LSP only (used by editors)

# fast iteration: don't shell out to the installed binary, run via cargo
cargo run -- check examples/testapp/main.jwc
cargo run -- run examples/testapp
cargo run -- serve examples/testapp --port 8080 --watch
cargo run -- lint                    # validate + dead-code warnings
cargo run -- gen-sql examples/testapp/src/data/AppDbContext.jwc
```

`build.ps1` / `build.sh` / `build.cmd` are convenience wrappers around
`cargo build` for end-user installs — prefer `cargo` directly during dev.

`install-from-source.{sh,ps1}` install the compiled binary to the user
profile (`~/.jwc/bin` or `%LOCALAPPDATA%\jwc\bin`). Never run these to test
local changes; they overwrite the user's installed `jwc`.

## Tests

```bash
cargo test                           # unit tests
cargo test --test integration_db     # Postgres integration suite
cargo test --test integration_db -- some_test_name   # single test
cargo test -- --nocapture            # show eprintln! output (skips, etc.)
```

`tests/integration_db.rs` boots a Postgres container via `testcontainers` and
serialises tests behind a global `Mutex` because `jwc::engine::ENGINE` is a
process-wide `OnceLock`. Tests print `SKIPPED` via `eprintln!` and return
`Ok(())` when Docker is unreachable — do **not** treat skipped output as a
pass when verifying a change. Run the suite on a machine with Docker before
claiming DB-touching work is done.

There is no `cargo fmt` config and no clippy lint config beyond defaults;
match the surrounding style.

## CLI surface (defined in `src/main.rs`)

The Clap subcommands map 1:1 to functions in the library crate:

| Subcommand                   | Library entry point                           |
|------------------------------|-----------------------------------------------|
| `new <name>`                 | `project::create_new_project`                 |
| `check <file>`               | `parser::parse_program` + `validate_program`  |
| `gen-sql <file>`             | `sql::generate_postgres_schema`               |
| `run [path]`                 | `project::load` → `runner::run_main` → `server::serve` |
| `serve [path] --watch`       | same as `run`, but always starts the server   |
| `test` / `lint`              | `project::load` + `lint::lint_program`        |
| `build [--release]`          | runtime bundler in `main.rs`                  |
| `migrate new/up/down`        | `migrate::*`                                  |

When extending the CLI, add the subcommand in `main.rs` and the implementation
behind a function in the matching `src/*.rs` module — keep `main.rs` thin.

## Architecture

### Compilation pipeline (always in this order)

`lexer.rs` → `parser.rs::parse_program` → `parser.rs::validate_program` →
optionally `lint::lint_program` → `runner::run_main` (or `sql::generate_*`
for `gen-sql`).

- `lexer.rs` — hand-written tokeniser. Adds a token type? Update `Token`,
  `TokenKind`, and the keyword table.
- `parser.rs` — recursive-descent, builds nodes from `ast.rs`. The same file
  also hosts `validate_program`, which is **not** a separate pass — it
  re-walks the AST to enforce dbcontext/entity/column compile-time checks.
  Many invariants (e.g. column existence on `where Entity.col`) are enforced
  here, not in the runtime.
- `ast.rs` — all AST node definitions live here. Every new syntactic form
  needs an enum variant plus parser support plus runner support.
- `lint.rs` — pure AST walk; only emits warnings (unused functions, unused
  middleware). Never produces hard errors.
- `runner/` — the interpreter `Vm`, split across `mod.rs`, `eval.rs`,
  `builtins.rs`, `dispatch.rs`, `exec.rs`, `sql.rs`, `types.rs`, `util.rs`,
  `validation.rs`. Everything user code can do (built-ins, control flow,
  route dispatch glue, validation, JSON coercion) lives here. There is **no
  `call_builtin` function**: builtin dispatch is a chain of
  `if name.eq_ignore_ascii_case(...)` in the `Expr::Call` arm of
  `Vm::eval_expr` (`eval.rs`), and the bodies are `eval_*_call` methods in
  `builtins.rs`. The arity/native table is `src/builtins.rs::BUILTIN_DEFS`.
  The Vm is **async**: the recursive evaluator methods carry
  `#[async_recursion]`, so `eval_expr` / `exec_block` / `call_function` are
  all `async fn` and must be `.await`-ed.
- `engine.rs` — the singleton DB layer (`ENGINE: OnceLock<JwcEngine>`).
  Wraps a `deadpool-postgres` async pool backed by `tokio-postgres` (TLS via
  `tokio-postgres-rustls` when `JWC_DB_TLS` is set), a prepared statement
  cache, and an optional TTL result cache. Same `OnceLock<JwcEngine>`
  singleton — tests reset the `public` schema between runs but **do not**
  reset `ENGINE`; design new DB code so it tolerates being called against a
  fresh schema on the same pool.

### Async stack: Vm, server, DB, native AOT

This is the single most important architectural fact:

- `runner/` is fully async (`#[async_recursion]` on the recursive
  evaluator methods). `Vm::eval_expr` is an `async fn`; SQL calls await on
  the `deadpool-postgres` pool.
- `server.rs` is built on axum + tokio. Every request is a `tokio::spawn`'d
  task — no more `spawn_blocking`. WebSocket frame I/O is direct async I/O
  via `tokio::io::{AsyncReadExt, AsyncWriteExt}` against a
  `tokio::task_local!` `Arc<Mutex<TcpStream>>` (no mpsc bridge thread).
- `async function` / `await` are real now (Phase 9). Suspending across an
  `.await` yields to the scheduler — concurrent requests no longer
  serialise on a worker thread.
- When adding a new async builtin (HTTP, filesystem, anything that
  suspends), put the impl in `runner/builtins.rs`, wire the dispatch arm in
  `runner/eval.rs`, and add the `BuiltinDef` row in `src/builtins.rs` with
  `native: true` — that flag **is** the AOT whitelist, via
  `native_builtin_names()`. There is no separate BUILTINS list in
  `native_build.rs` any more.
- **The one that bites:** an async builtin must ALSO be named in the
  `is_async_builtin` `matches!` in `src/native_build.rs`. That list is not
  derived from anything. Miss it and codegen emits the call without
  `.await`, so the *generated* crate fails to compile — and no test in this
  repo catches it, because neither `conformance.rs` nor `native_parity.rs`
  builds emitted source. Verify with a real `jwc build --native`, or extend
  the await-guard in `tests/native_emit.rs`.

### Migrations

`src/migrate.rs` owns both the file generator (`jwc migrate new`) and the
applier (`up`/`down`). Apply uses a Postgres session-level advisory lock
(`pg_advisory_lock` keyed by `MIGRATION_LOCK_KEY` = ASCII `"jwc-mig"`) so
concurrent processes serialise without deadlocking. The CLI honours
`DATABASE_URL` / `JWC_DATABASE_URL` and the same `JWC_DB_TLS*` flags as the
runtime pool — keep that contract when extending the migrate command.

`schema_diff.rs` is wired into `migrate new`: `create_migration` reads the
latest `.up.sql` snapshot from `migrations/`, parses it back into entity
snapshots, diffs it against the current program's entities, and emits only
the resulting `ALTER` / `CREATE TABLE` statements (or `-- no schema changes`
if the diff is empty). Don't re-emit the full schema from a generator —
extend `schema_diff::compute_diff` / `diff_to_sql` instead.

### Project layout

A JWC project is `jwcproj.json` + one or more `.jwc` files (the loader walks
upward from `cwd` to find the manifest). `project::load` discovers source
files, parses each, and merges them into a single `Program`. There is no
module/import system — every `.jwc` file in the project contributes to one
flat namespace, so a parse error in any file fails the whole load.

`examples/testapp` is the canonical end-to-end test project; `microblog` is
a second working example. When prototyping a language change, run both
against it (`cargo run -- lint --manifest-path examples/testapp/jwcproj.json`
equivalent: `cd examples/testapp && cargo run --manifest-path ../../Cargo.toml -- lint`).

## VS Code extension

`vscode-extension/` is a self-contained TypeScript project that bundles
syntax highlighting and points at the `jwc-lsp` binary. Iterating on it:

```bash
cd vscode-extension
npm install
npm run compile
```

The extension's `package.json` declares the LSP launch command — if you
rename or move the `jwc-lsp` binary in Rust, update the extension config in
the same change.

## Roadmap awareness

`ROADMAP.md` is the source of truth for what counts as "done" vs.
"partial" vs. **"non-goal"**. Before adding a feature that overlaps a
Phase item, re-read the relevant section. The **Non-goals** block at the
top of ROADMAP.md is policy — those items (LLVM IR, cross-target matrix,
WASM, self-hosting, multi-database driver, SSE v2, OTLP-as-core,
job-priority/DLQ ML, rich-domain object graphs) won't ship pre-1.0 and
PRs that add them get closed. The north star — "Web backend yoz —
CRUD'ni qo'lda yozmasdan, ORM bilan kurashmasdan, native-tez" — gates
every new feature.

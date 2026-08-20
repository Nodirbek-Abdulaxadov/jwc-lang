# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working
with code in this repository.

## What this repo is

JWC is a backend-focused programming language with first-class HTTP routes,
tables, views, generated SQL and Postgres execution. The repository ships
the Rust compiler and interpreter (`jwc`), the normative specification under
`docs/spec/v1/`, and a VS Code extension.

**The language changed at v0.25.0.** The pre-1.0 grammar — `entity`,
`dbcontext`, `with`, `via`, `validate body`, `new … from`, `patch`, `group`,
`mount`, `dome` — was removed along with its front-end, its runtime, the
native AOT backend, the package manager and the language server. What used
to live under `src/v1/` is now the whole of `src/`. The old documentation is
archived under `docs/archive-0.9/`; it describes what deployed 0.9.x
binaries implement, not this compiler.

`ROADMAP.md` is the source of truth for what counts as done, partial, and
**non-goal**. The Non-goals block is policy: LLVM IR, a cross-target native
matrix, WASM, self-hosting, a multi-database driver, SSE v2, OTLP-as-core,
job-priority/DLQ ML and rich-domain object graphs will not ship pre-1.0.

## Build / run

```bash
cargo build                       # debug
cargo build --release
cargo run -- check docs/spec/v1/sample
cargo run -- explain docs/spec/v1/sample     # every query, with its SQL
cargo run -- explain docs/spec/v1/sample --route "GET /api/v1/orgs/{org_id}"
cargo run -- gen-sql docs/spec/v1/sample     # the schema as DDL
cargo run -- serve docs/spec/v1/sample --port 8080
cargo run -- lint docs/spec/v1/sample --constraints
cargo run -- openapi docs/spec/v1/sample --out openapi.json
cargo run -- migrate new add_region docs/spec/v1/sample --explain
```

`install-from-source.{sh,ps1}` install to the user profile. Never run them
to test local changes; they overwrite the user's installed `jwc`.

## Tests

```bash
cargo test                        # everything that needs no database
```

Three suites need Postgres and **print SKIPPED without it**. A SKIPPED line
is not a pass — the variables below are how you actually run them:

```bash
# psql connection string. Drops and creates databases.
JWC_V1_PG='-h 127.0.0.1 -p 5432 -U postgres' cargo test --test sql_golden
JWC_V1_PG='-h 127.0.0.1 -p 5432 -U postgres' cargo test --test ddl_golden

# A database the suite may drop and recreate schemas in.
JWC_V1_DATABASE_URL=postgres://…  CURSOR_SECRET=x cargo test --test http_golden
JWC_V1_DATABASE_URL=postgres://…  cargo test --test raw_hatch

# Applies each generated migration on top of the schema it migrates from.
JWC_V1_PG='-h 127.0.0.1 -p 5432 -U postgres' cargo test --test migrate_golden

# up / down / status / verify. Serial: they share one database.
JWC_V1_DATABASE_URL=postgres://…  cargo test --test migrate_apply -- --test-threads=1

# v0.26.0's acceptance test. 20 random walks by default, 200 for the full run.
JWC_V1_DATABASE_URL=postgres://…  JWC_ROUNDTRIP_SEQUENCES=200 \
  cargo test --test migrate_roundtrip -- --test-threads=1
```

What each suite is for:

| Suite | Pins |
|---|---|
| `parse_corpus` | the grammar, snippet by snippet |
| `fmt` | canonical printing, and that it is a fixed point |
| `removed_keywords` | every removed keyword names its replacement |
| `schema_diagnostics` | one case per schema rule, message included |
| `type_corpus` | the type layer, `-- expect:` annotated, exact both ways |
| `wiring_corpus` | routes, middleware, `context`, the error model, imports |
| `ddl_golden` | emitted DDL, byte for byte, and that it applies |
| `sql_golden` | emitted SQL, byte for byte, and that it runs |
| `http_golden` | request/response pairs through the real pipeline |
| `docs_parse` | every ```jwc block in the README and the spec |
| `snapshot_sample` | the sample's migration snapshot, field by field |
| `diff_corpus` | two schemas in, the migration's operations and phases out |
| `migrate_golden` | emitted migrations, byte for byte, and that they apply |
| `migrate_apply` | `up`, `down`, `status`, `verify` against a real database |
| `migrate_roundtrip` | a migrated database *is* a created database |
| `tooling` | the CLI contract: which flag selects what, and what a wrong name prints |
| `lsp` | a scripted session against the real stdio protocol |

`tests/tooling.rs` validates the emitted OpenAPI against
`openapi-spec-validator` when it is importable, and prints SKIPPED when it is
not. `pip install openapi-spec-validator` to actually run it.

The corpora are **exact in both directions**: a missing diagnostic and an
unannotated one both fail. That is what makes them a specification rather
than a smoke test.

There is no `cargo fmt` config and no clippy config beyond defaults; match
the surrounding style. CI runs `clippy --all-targets -- -D warnings`.

## Architecture

### The pipeline, always in this order

`lexer` → `parser` → `model` (schema) → `symbols` → `check` (types) →
`wiring` (routes, middleware, errors) + `imports` → `query`/`query_sql` →
`exec`.

- **`token.rs` / `lexer.rs`** — hand-written. `KEYWORDS` is the keyword
  table; `REMOVED_KEYWORDS` is what makes a 0.9.x file say what to write
  instead. Keywords are **contextual**: there are no reserved words, because
  the sample uses `route`, `max`, `check`, `key`, `text` and `date` as
  ordinary identifiers.
- **`parser.rs`** — recursive descent with per-declaration recovery, so one
  bad declaration does not swallow the file.
- **`ast.rs`** — every node. A new syntactic form needs a variant here, a
  parser arm, a `fmt.rs` arm, and a `check.rs` arm.
- **`fmt.rs`** — the canonical printer. Formatting is a fixed point by
  construction, and `tests/fmt.rs` asserts it.
- **`model.rs`** — the resolved schema: physical names, types, constraints.
  DDL reads this, never the AST.
- **`views.rs`** — views as *relations*, with their columns worked out. A
  view is a real `CREATE VIEW`, not a macro.
- **`ddl.rs`** — seven-phase emission (schema, enum type, table, **every FK
  in its own pass**, index, trigger, view, comment). The separate FK pass is
  what makes cross-schema cycles emittable. Every statement is rendered from
  the *snapshot* form of its object, so `gen-sql` and `jwc migrate` cannot
  emit different DDL for the same thing.
- **`snapshot.rs`** — the schema as a database holds it, as checked-in JSON.
  What it leaves out is deliberate: `private`, a constraint's message and
  `was` are all absent, which is the statement "editing this produces no
  migration".
- **`diff.rs`** — two snapshots in, typed operations out, in the ten phases
  of migrations.md §4. Constraints and indexes match on their *bodies*, not
  their names, so renaming a column renames its constraints instead of
  rebuilding them.
- **`migrate.rs`** — the operations, lowered to files. `down` is generated
  by diffing the other way, and refused outright when the migration is
  irreversible.
- **`apply.rs`** — `up`, `down`, `status`, `verify`, all under a session
  advisory lock. The bookkeeping row goes in the **same transaction** as the
  statements, which is why the applier strips the file's own
  `BEGIN`/`COMMIT` rather than running it as written.
- **`check.rs`** — the type pass. `Raw` vs `Record`, `T?` and narrowing,
  class validation, aggregates.
- **`wiring.rs`** — whole-program checks: the route table, middleware
  chains, typed `context`, raise sets over the static call graph.
- **`query.rs`** — the join *tree*: which join attaches to which binding.
- **`query_sql.rs`** — the join tree as SQL. This is where the shapes that
  matter live; see below.
- **`exec.rs` / `exec_call.rs` / `serve.rs`** — the interpreter and the axum
  server. `serve::handle` is a plain async function over an owned request,
  so tests drive the real pipeline and only the socket is unexercised.

### Four things in the emitter that are easy to get wrong

1. **`as one` under `left join`** emits `CASE WHEN <child pk> IS NULL THEN
   NULL ELSE json_build_object(…) END`. Without the guard an unmatched row
   projects an object of nulls instead of null.
2. **`as many`** is a `LEFT JOIN LATERAL`, never `json_agg(… ORDER BY …)`.
   That form can order a collection but cannot bound one, and two
   collections side by side multiply each other's rows.
3. **A bounded page over a collection** takes its keys first
   (`WITH page AS MATERIALIZED`), scanning the **base table** — selecting
   keys from a view still evaluates its laterals. If the pushdown cannot be
   proven it is `E0542`, never a silent O(table) plan.
4. **Every value is a bind parameter, bound as text and cast in SQL**:
   `($1::text)::bigint`, never `$1::bigint`, which makes Postgres infer the
   column's type for the parameter and refuse the text the runtime sends.

`json`, not `jsonb`: `jsonb` sorts object keys, and the projection order
**is** the JSON key order.

### Diagnostics

Every diagnostic carries a code, a message, a `= help:` note naming the fix,
and a `= spec:` clause. A code that appears in a spec table and not in the
source is debt; the audit is `comm -23` over the two lists.

## The specification

`docs/spec/v1/` is normative — thirteen documents plus `sample/`, a complete
application (13 tables, 5 views, 25 endpoints). Before changing behaviour,
read the clause. If the implementation and the spec disagree, one of them is
wrong and the commit says which.

`check_sample.py` classifies every construct in the sample against the spec
and fails if one is unspecified.

## VS Code extension

`vscode-extension/` is self-contained TypeScript: syntax highlighting and
snippets. Its keyword list is generated from `token.rs::KEYWORDS` — if you
add a keyword, update the TextMate grammar in the same change. The language
server is `jwc lsp` (`src/lsp.rs`), a subcommand of the compiler rather than
a second binary, so the server and `jwc check` cannot be different builds.

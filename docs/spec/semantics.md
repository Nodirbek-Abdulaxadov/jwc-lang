# JWC Evaluation Semantics

Status: **DRAFT** — extracted from `src/runner/mod.rs` (v0.4.1, ~5.2 kloc).

This document pins the observable runtime behaviour. Where prose
disagrees with the conformance suite, the conformance suite wins and
this document is the bug.

---

## 1. Evaluation order

- Statements within a block run top-to-bottom.
- Within an expression, sub-expressions evaluate **left-to-right**.
- Function arguments evaluate left-to-right **before** the call.
- Short-circuit operators (`&&`, `||`) stop at the first decisive operand.
- `if`/`while` conditions evaluate exactly once per branch test.

## 2. Scope and bindings

- `let` and `var` introduce a binding in the **enclosing block**. There
  is no separate "function scope" — a `let` inside an `if` block dies at
  the `}`. Same rule for `for` loop variables.
- Shadowing is allowed: a `let x` inside a nested block hides any outer
  `x` for the duration of that block.
- Top-level `const` declarations are in scope for the whole program and
  must be initialised by a constant expression.
- Functions and routes share a single top-level namespace; route names
  must be unique across HTTP method + path; function names must be
  unique across the program.

## 3. Async, await, suspension

- A function declared `async fn f()` returns a future-like value. Inside
  another `async fn` callers can `await` it.
- `await` suspends the current task; concurrent requests progress while
  it waits. Suspension never observably reorders effects of a single
  request relative to its own `await`s.
- `await` on a non-future value is a runtime error.
- Top-level `main()` is allowed to be `async`; the runtime drives it on
  the tokio scheduler.

## 4. Types and the value model (current)

JWC values today are tagged as one of:

`Int`, `Float`, `Str`, `Bool`, `Null`, `Void`, `Array`.

Objects (entity instances, JSON objects, DB rows) are currently
represented as `Str` carrying a JSON document. **This is a known issue
tracked in Phase 1** of the production-readiness plan — `Value::Record`
will replace the JSON-string round-trip without changing observable
semantics for code that uses field access.

### Type coercion (`==`, arithmetic)

- `Int` ↔ `Float`: arithmetic on a mixed pair widens to `Float`.
- `Int == Float` compares by widening to `Float` (exact representable
  values compare equal; otherwise the comparison reflects IEEE-754).
- `Str == Int` / `Str == Float` is **never true** — different types
  never compare equal across the type boundary except for the numeric
  pair above.
- `Null` compares equal only to `Null`.
- `Bool` does NOT auto-coerce to `Int` in arithmetic; mixing is a runtime
  error.

### Integer behaviour

- All `int` values are 64-bit signed.
- Overflow is **wrapping** in the interpreter today (matches Rust release
  semantics on `i64`); a checked/saturating variant is a Phase 3 item.
- Division by zero (`int` or `float`) raises a runtime error rather
  than producing `Inf`/`NaN`.

### Float formatting

- The default string conversion of a float uses JWC's `format_float`
  (see `runner::format_float`): trailing zeros stripped, integer-valued
  floats render without a `.0`, NaN renders as `NaN`, infinities render
  as `Infinity`/`-Infinity`. This is observable through
  `string(3.0) == "3"` and is conformance-pinned.

### String semantics

- Strings are UTF-8. `length(s)` returns `s.chars().count()`, not the
  byte length. (`length()` on a JSON-array string returns the array
  cardinality — see Phase 1 for the cleanup plan.)
- `==` on strings is byte-wise equality after UTF-8 normalization is
  **not** applied — the strings must be byte-identical.

## 5. Control flow

- `return` from a function unwinds the function. `return` outside a
  function is a compile-time error.
- `break` and `continue` are supported inside `for`/`while`; targeting an
  outer loop is not supported.
- `try`/`catch` today: the parser accepts `catch (e: DbError)` but the
  type filter does NOT discriminate — every error reaches every catch
  clause. **Phase 3 [1.0-blocker]** changes this to typed dispatch.

## 6. Database semantics

- `select ... from Entity` constructs a typed SQL query at compile time;
  unknown columns/entities are rejected by `validate_program`, not at
  runtime.
- A query result is iterable (`for row in select ...`) and indexable
  (`xs[0]`), and the value model is the JSON-string fallback noted in §4.
- Writes go through the `deadpool-postgres` pool; reads may hit the
  optional TTL result cache (config-driven).
- `transaction { ... }` opens a serializable transaction; nested
  transactions are rejected at compile time (savepoints are a Phase 4
  item).
- **Whole-row `update`** today reads, modifies, and writes back the row
  — under concurrency it loses writes. Atomic `update Entity set col = expr
  where ...` is the [1.0-blocker] Phase 4 fix.

## 7. HTTP serving

- Routes are mounted with `get "/path"`, `post "/path"`, etc., with
  `{param}` placeholders matching one path segment.
- Handler return values are JSON-encoded via the same JSON serializer as
  `json_stringify`.
- Middleware runs **before** the handler today; response-phase
  middleware (Phase 5) will add `after { ... }` blocks.
- `response(status, body)` allows manual override of both the HTTP
  status code and the body; further mutation after `response()` is a
  runtime error.

## 8. Background jobs

- `job` declarations register a worker handler keyed by name.
- The default queue is **in-process** and **loses jobs on restart** —
  this is intentional for dev. A Postgres-backed driver
  (`JWC_QUEUE_DRIVER=postgres`) is a Phase 5 item.

## 9. Errors

- Compile-time errors abort the load with `file:line:col` (after the
  Phase 2 span migration completes — today many errors lack location).
- Runtime errors unwind through `try`/`catch`; uncaught errors at the
  top level abort the request with HTTP 500 (response phase logs the
  trace via the structured logger when enabled).
- The error message text and JSON shape are NOT a stable API today; the
  error-code registry (`src/error_codes.rs`) will become the contract
  surface at v1.0.

## 10. What is NOT specified yet

The following are observable in v0.4.x but intentionally NOT spec
commitments until they're explicitly added here:

- Exact iteration order of object literals / DB rows.
- Floating-point rounding mode beyond IEEE-754 default.
- Garbage-collection / memory-reclamation timing.
- Concurrent modification of shared arrays under tokio.
- Network-layer error → JWC error mapping at the byte level.

These will be either pinned in this document by v1.0 or explicitly
declared "implementation defined" in the SemVer policy.

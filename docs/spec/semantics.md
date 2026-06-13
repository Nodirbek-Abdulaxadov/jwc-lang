# JWC Evaluation Semantics

Status: **DRAFT** · Reflects: **v0.4.7** — extracted from `src/runner/eval.rs`,
`crates/jwc-runtime/src/lib.rs`, `src/runner/exec.rs`, and `src/runner/mod.rs`
(post Sprint 2 decomposition; Sprint 3C / 4B updates).

**Related spec docs**:
[index](index.md) ·
[visibility](visibility.md) (cross-namespace call gate) ·
[threat-model](threat-model.md) (depends on §4.3 string encoding) ·
[aot-scope](aot-scope.md) (which constructs lower cleanly) ·
[builtins](builtins.md) (per-builtin contracts).

This document pins the observable runtime behaviour. Where prose
disagrees with the conformance suite, the conformance suite wins and
this document is the bug. Every claim in §4.1 – §4.4 ("the Phase 3
quartet") is cross-referenced to a conformance fixture under
`tests/conformance/cases/` so the spec cannot drift past the suite
silently.

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

### 4.1 Integer overflow

JWC `int` is a 64-bit signed integer (`i64`). All integer arithmetic in
the interpreter is plain Rust arithmetic on `i64`:

```rust
// src/runner/eval.rs, Expr::Add / Expr::Sub / Expr::Mul
(Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
```

There is **no** explicit `checked_*` / `wrapping_*` / `saturating_*`
wrapper around `+`, `-`, `*`, `Neg`. The observable overflow policy is
therefore the Rust compiler's default, which is intentionally
build-profile-dependent:

| Build profile | Overflow of `a + b`, `a - b`, `a * b`, `-a` |
|---------------|----------------------------------------------|
| `cargo build` (debug) | Panic — `attempt to add with overflow`. The panic is caught by `try`/`catch` (Rust unwind through anyhow); without a `catch` the request returns 500 and the structured logger emits a `panic` line. |
| `cargo build --release` | Two's-complement **wrap**. `i64::MAX + 1` evaluates to `i64::MIN`. No diagnostic — the wrap is silent. |

This is the contract through v1.0. We deliberately do NOT promise:

- a single behaviour across debug and release (matching Rust default
  keeps the compile-time cost of arithmetic at zero — the alternative
  is a hot-path `checked_add` on every integer op),
- a fault-on-overflow guarantee in release builds — production users
  who need that should validate inputs ahead of arithmetic,
- promotion to a bigint type — JWC is statically `i64`-shaped and
  promotion would force every integer to allocate.

**Division and modulo are different.** `/` and `%` are *not* native
Rust `/` / `%`; the interpreter explicitly checks for a zero divisor
and raises a typed error (`bail!("division by zero")` /
`bail!("modulo by zero")`) — these paths are catchable. See
`tests/conformance/cases/case_try_catch.jwc` for the divide-by-zero
catch fixture. Int division otherwise truncates toward zero (Rust
`/` semantics on `i64`); `%` matches Rust truncated division
(`-7 % 3 == -1`, not the Python `2`). Both pinned in
`case_int_overflow.jwc` lines 21-29.

**Mixed-type arithmetic widens to `Float` and uses IEEE-754** —
`Int(a) op Float(b)` evaluates as `(a as f64) op b`, so the overflow
discussion above applies only to the pure-`Int` lattice. The float
side cannot overflow in the panic sense; it produces `Infinity` or
`NaN` per IEEE-754.

### 4.2 Float formatting (`format_float`)

Every render of a `Value::Float` to a user-visible string — `print(x)`,
string concatenation, JSON serialization of a top-level scalar — goes
through one function:

```rust
// crates/jwc-runtime/src/lib.rs::format_float
pub fn format_float(value: f64) -> String {
    let mut s = format!("{value:.15}");
    while s.contains('.') && s.ends_with('0') { s.pop(); }
    if s.ends_with('.') { s.pop(); }
    if s == "-0" { "0".to_string() } else { s }
}
```

The contract:

1. **Fixed precision-15.** The base render is `"{:.15}"` — fifteen
   fractional digits, always present, regardless of the float's
   "natural" Rust display. This intentionally hides the IEEE-754
   precision boundary: `0.1 + 0.2` renders as `0.300000000000000`
   before trimming, not `0.30000000000000004`.
2. **Strip trailing fractional zeros.** After the precision-15 render,
   trailing `'0'` characters are popped one at a time *as long as the
   string still contains a `'.'`*. The `contains('.')` guard prevents
   the loop from eating zeros in the integer part of a value that
   already lost its decimal point.
3. **Strip the now-orphan decimal point.** If after step 2 the string
   ends in `.`, that point is removed — integer-valued floats render
   without a `.0` suffix.
4. **Collapse negative zero.** The literal string `"-0"` (the post-trim
   result of `-0.0`, `-0.000000000000000` minus zeros and dot)
   becomes `"0"`. This is a string-level rule — runtime IEEE-754
   identity (`-0.0`'s bit pattern) is preserved in the `Value::Float`
   itself, only its printed form normalizes.

**Five worked examples** (cross-checked against
`tests/conformance/cases/case_format_float.{jwc,stdout.txt}`):

| Input `f64`           | After `"{:.15}"`           | After trim | Final  |
|-----------------------|----------------------------|------------|--------|
| `1.0`                 | `1.000000000000000`        | `1.`       | `1`    |
| `0.1 + 0.2`           | `0.300000000000000`        | `0.3`      | `0.3`  |
| `-0.0`                | `-0.000000000000000`       | `-0.`→`-0` | `0`    |
| `100.0`               | `100.000000000000000`      | `100.`     | `100`  |
| `-3.14`               | `-3.140000000000000`       | `-3.14`    | `-3.14`|

**Not specified yet**: NaN and infinity rendering. Today `format!("{:.15}", f64::NAN)`
produces `NaN` and `format!("{:.15}", f64::INFINITY)` produces `inf`,
both of which fall through the trim/dot/negzero passes unchanged. The
JSON serializer rejects NaN/Inf entirely. These edges are not
conformance-pinned and may change before 1.0.

### 4.3 String encoding

JWC strings are **UTF-8** — a `Value::Str` is a Rust `String`, so the
"valid UTF-8" invariant is enforced by the type system itself. There
is no `Value::Bytes`, no `Value::AsciiStr`, no separately-typed binary
buffer. Every place that materialises a string — literals, JSON
deserialization, DB column reads, HTTP body decode, `to_string`,
arithmetic-style `+` concatenation — produces UTF-8.

**Length is char-count, not byte-count.** `length(s)` returns
`s.chars().count() as i64` (see `eval_length_call` in
`src/runner/builtins.rs`). This is the *Unicode scalar value* count
under Rust's `char` model; a `"café".length()` is `4`, not `5` (which
is the UTF-8 byte length).

**Take is char-count, not byte-count.** `take(s, n)` is implemented as
`s.chars().take(n)` (via `slice_chars`); it cannot split a multibyte
sequence in the middle. `take("café", 3)` returns `"caf"`, not the
three leading UTF-8 bytes of `"café"`.

**Substring is char-count, not byte-count.** Same model — see
`slice_chars` in `src/runner/builtins.rs::1426`.

**String equality is byte-identical Unicode-scalar comparison.** No
Unicode normalization is applied: `"\u{00E9}" == "\u{0065}\u{0301}"`
(precomposed `é` vs `e` + combining acute) is **false**. If a future
release adds NFC normalization at any boundary (DB read, HTTP body),
this section needs an update.

**There are intentionally no byte-level string operations.** Code that
needs to inspect bytes — for example, validating a base64 payload's
exact byte length — must explicitly route through `bytes_length(s)` /
`bytes_take(s, n)` builtins, which do not yet exist. They are tracked
under Phase 3 "builtin gaps".

### 4.4 `==` across types

The `==` operator is implemented as a **derived `PartialEq` on the
`Value` enum** (see `crates/jwc-runtime/src/lib.rs::Value`):

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Value { Int(i64), Float(f64), Str(String), Bool(bool), Null, Void, Array(...), Record {...} }
```

`Expr::Eq` in `src/runner/eval.rs` evaluates both sides to a `Value`
and returns `Value::Bool(l == r)` — no cross-type promotion, no
coercion. This gives the following table; every row is conformance-
pinned by `tests/conformance/cases/case_equality.jwc`:

| Comparison                  | Result   | Reason                                                       |
|-----------------------------|----------|--------------------------------------------------------------|
| `1 == 1`                    | `true`   | same variant, same payload                                   |
| `"a" == "a"`                | `true`   | same variant, byte-equal payload                             |
| `true == true`              | `true`   | same variant                                                 |
| `null == null`              | `true`   | both `Value::Null` (no payload)                              |
| `1 == 1.0`                  | `false`  | `Value::Int(1) != Value::Float(1.0)` — different discriminants |
| `2.5 == 2.5`                | `true`   | float-vs-float identity, IEEE-754 `==`                       |
| `1 == "1"`                  | `false`  | `Value::Int` vs `Value::Str` — different discriminants       |
| `0 == false`                | `false`  | `Value::Int` vs `Value::Bool` — different discriminants       |
| `null == 0` / `null == ""`  | `false`  | `Value::Null` only equals `Value::Null`                       |
| `null == void`              | `false`  | distinct discriminants                                       |

**`!=` is the negation of `==`** at the AST level (`Expr::Neq`
evaluates `Value::Bool(l != r)`).

**Ordering comparisons (`<`, `<=`, `>`, `>=`) ARE numeric-promoting**,
unlike `==`. `eval_numeric_cmp` widens both sides to `f64` and runs
the comparison on doubles. So `1 < 2.5` is `true` and `1 <= 1.0` is
`true` — even though `1 == 1.0` is `false`. This asymmetry is
intentional: `==` carries an identity contract (think
hash-key equality) where mixing `Int` and `Float` would surprise
caller code that relies on `if x == 1 { ... }` not matching a float
column; ordering carries no such contract.

The ordering operators error out on non-numeric operands (`"a" < "b"`
is a runtime error — `bail!("Unsupported comparison for ...")`).
String / bool / null comparison is not in scope through v1.0.

**Floats follow IEEE-754.** `NaN == NaN` is `false`,
`Float(0.0) == Float(-0.0)` is `true` (matches IEEE-754 even though
`format_float` collapses the *printed* form). No silent epsilon
tolerance is applied — code that needs approximate equality must
write it explicitly.

**`Array` and `Record` use structural equality** by derived
`PartialEq`. Two `Array(vec![Int(1), Int(2)])` values are `==`. Two
`Record { field_names, values }` are `==` when the field-name layouts
and the value vectors are pairwise `==`. There is currently no
cross-shape equality between `Record { ... }` and an equivalent
`Str(json_string)` carrier — Phase 1 is migrating dynamic JSON into
`Record`, after which both forms will compare structurally.

## 5. Control flow

- `return` from a function unwinds the function. `return` outside a
  function is a compile-time error.
- `break` and `continue` are supported inside `for`/`while`; targeting an
  outer loop is not supported.
- `try`/`catch` dispatches by error kind. A bare `catch (e)` and an
  explicit `catch (e: Error)` both catch every error. A typed clause
  `catch (e: T)` matches when the error's classified kind is `T` or a
  dotted child of `T` (e.g. `catch (e: DbError)` catches
  `DbError.UniqueViolation`). Non-matching errors propagate to the
  surrounding scope. The canonical list of kinds is described in §5.1
  below.

### 5.1 Error kinds and typed `catch`

The dispatch table for `catch (e: T)` is the constant
`JWC_ERROR_KINDS` in `src/runner/mod.rs`. The current set
(v0.4.7) is hierarchical via dot-separated paths — a parent kind
catches every dotted child:

| Kind | Catches |
|---|---|
| `Error` | every runtime error (root) |
| `DbError` | every `DbError.*` subtype |
| `DbError.UniqueViolation` | PG SQLSTATE `23505` |
| `DbError.ForeignKeyViolation` | PG SQLSTATE `23503` |
| `DbError.NotNullViolation` | PG SQLSTATE `23502` |
| `DbError.CheckViolation` | PG SQLSTATE `23514` |
| `DbError.SerializationFailure` | PG SQLSTATE `40001` |
| `DbError.DeadlockDetected` | PG SQLSTATE `40P01` |
| `DbError.ConnectionFailure` | `tokio_postgres::Error::is_closed()` |
| `HttpError` | every `HttpError.*` subtype |
| `HttpError.NotFound` | upstream 404 |
| `HttpError.Unauthorized` | upstream 401 |
| `HttpError.Forbidden` | upstream 403 |
| `HttpError.BadGateway` | upstream 502 / 503 / 504 |
| `ValidationError` | request-body / type-coercion validation failures |
| `TimeoutError` | watchdog / `tokio::time::error::Elapsed` |
| `JwtError` | every `JwtError.*` subtype |
| `JwtError.InvalidSignature` | HS256 signature mismatch / bad base64 |
| `JwtError.Expired` | `exp` claim is in the past (Phase 6) |

The classification rules (`classify_jwc_error` in the same module)
walk the `anyhow::Error::chain()`, first trying typed downcasts onto
`tokio_postgres::Error` / `reqwest::Error` for authoritative
SQLSTATE / HTTP status signals, then falling back to a substring
scan of the rendered chain. The string is the single source of
truth; downstream docs should NOT re-list the kinds — link to
`src/runner/mod.rs::JWC_ERROR_KINDS` instead.

Tracked by `tests/conformance/cases/case_typed_catch_*` and
`case_catch_falls_through_when_type_mismatches`.

## 6. Database semantics

- `select ... from Entity` constructs a typed SQL query at compile time;
  unknown columns/entities are rejected by `validate_program`, not at
  runtime.
- A query result is iterable (`for row in select ...`) and indexable
  (`xs[0]`), and the value model is the JSON-string fallback noted in §4.
- Writes go through the `deadpool-postgres` pool; reads may hit the
  optional TTL result cache (config-driven).
- `transaction { ... }` opens a serializable transaction; nested
  transactions are rejected at compile time.
- **Whole-row `update`** today reads, modifies, and writes back the row
  — under concurrency it loses writes. Atomic `update Entity set col = expr
  where ...` is the [1.0-blocker] Phase 4 fix.

### 6.1 Transactions and savepoints

`transaction { ... }` (Sprint 4) opens a single Postgres transaction
scoped to the block. The block runs at the engine's default
isolation level; commit happens on clean exit, rollback on any
error (including a panic that unwinds through `anyhow`).

`savepoint <name> { ... }` (Sprint 4B) nests a SAVEPOINT inside the
enclosing transaction. The semantics:

- The savepoint is named exactly as in the source — the name reaches
  Postgres as a SQL identifier and must be a valid one (the parser
  rejects invalid identifiers at compile time).
- On clean block exit, the runner issues `RELEASE SAVEPOINT <name>`.
- On any error thrown from the block, the runner issues
  `ROLLBACK TO SAVEPOINT <name>` and then `RELEASE` — the outer
  transaction is **not** poisoned and continues. The error still
  propagates up the JWC stack (so a surrounding `try`/`catch` may
  intercept it).
- `savepoint` outside a `transaction { ... }` is rejected at runtime
  with error code `E017` (see `src/error_codes.rs`).
- The codegen path in `--native` does NOT yet support savepoints —
  see [`aot-scope.md`](aot-scope.md) "What raises a runtime panic".

Implementation: `src/runner/exec.rs::Stmt::Savepoint` →
`engine::with_savepoint`. The flat-transaction case (one
`transaction { ... }` per request, no nested savepoints) works on
both the interpreter AND `jwc build --native`.

## 7. HTTP serving

- Routes are mounted with `get "/path"`, `post "/path"`, etc., with
  `{param}` placeholders matching one path segment.
- Handler return values are JSON-encoded via the same JSON serializer as
  `json_stringify`.
- Middleware runs **before** the handler by default. Response-phase
  middleware lives inside `after { ... }` blocks (see §7.1).
- `response(status, body)` allows manual override of both the HTTP
  status code and the body; further mutation after `response()` is a
  runtime error.

### 7.1 Response-phase middleware (`after { ... }`)

A middleware declaration may declare one optional `after { ... }` block
in addition to its main body. The main body is the *request phase* and
runs before the handler; the `after` block is the *response phase* and
runs after the handler has produced a status + body.

**Dispatch order**

- Request-phase middleware bodies run **in declaration order** — the
  first declared `middleware` runs first, then the next, etc., before
  the route handler executes.
- After the handler returns (or errors), response-phase `after` blocks
  run **in reverse declaration order** — the last middleware's `after`
  runs first, walking outward. This mirrors the conventional onion
  model (the middleware that started last finishes first).

**What is visible inside `after`**

- `response_status()` — the HTTP status the handler emitted, including
  values set by an explicit `response(status, body)` call. Always
  non-null inside an `after` block.
- `response_duration_ms()` — milliseconds since the dispatcher first
  saw the request. Monotonically non-decreasing within a single
  request.
- `request_id()` — the same per-request id visible in the request
  phase, so log lines from `after` blocks correlate to the handler.
- All other request inspection builtins (`header()`, `body()`,
  `client_ip()`) — same values they had during the handler.

**Error handling**

- An exception raised inside an `after` block does **not** alter the
  response that already reached the client buffer in a streamed
  response. For a buffered response, an `after` block that raises is
  treated like any other handler error: the configured error handler
  runs and the original status/body is replaced with the error
  envelope.
- A failing `after` block does NOT skip the remaining `after` blocks —
  each block is invoked independently and exceptions are isolated to
  the offending block. Log lines from earlier (in reverse-order terms,
  inner) blocks still flush.
- `response()` inside an `after` block is rejected with the same
  "response already sent" runtime error that double-`response()` raises
  from the handler, so observers cannot retroactively change the wire
  status.

**When `after` does not run**

- The connection drops or times out before the handler returns — the
  `JWC_REQUEST_TIMEOUT` watchdog short-circuits to a 504 envelope and
  no `after` blocks fire for that request. This is intentional: the
  upstream client has already given up, and running response-phase
  logging on an aborted task would race the watchdog.

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

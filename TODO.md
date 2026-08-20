# TODO

Defects found in the field, not scheduled into `ROADMAP.md`. Historical phase
numbering lives in `docs/spec/roadmap-0.9.x.md`. Entries marked
**RESOLVED** are kept for the field report — the reproduction and the reasoning
are worth more than the strikethrough — with a pointer to the regression cover
that now holds them down.

## RESOLVED (0.9.6) — Native: field access on a `select` result — panic on write, `null` on read (2026-08-06)

**Both halves fixed.** The read half landed earlier: `jwc_get_field` grew a
`V::RawJson(s) | V::Str(s)` arm that parses on access. The write half landed in
0.9.6 — `jwc_set_field` had the same two arms and still `panic!`d on everything
else, so read-modify-write kept failing after reads started working. On 0.9.x
the panic is caught and answers HTTP 500 rather than dropping the connection as
reported below, but the handler never ran either way.

Regression cover: `tests/differential/cases/field_write.*` — read-modify-write
over a JSON-bearing `V::Str`, the same match arm, no Postgres needed. Verified
to fail without the fix (native 500, interpreter 200).

Original report follows.

Found on 0.8.7 building a CRUD demo (Postgres 17, Windows). The update path
every REST handler is written with — read a row, assign a field, write it
back — kills the worker thread under the native backend:

```jwc
route PUT "api/todos/{id}" {
    let existing = select Todo from AppDb.Todo where Todo.id == @id first;
    existing.title = req.title;          // ← panics natively, fine under `jwc run`
    update existing in AppDb.Todo;
    return ok(existing);
}
```

```
thread 'tokio-rt-worker' panicked at src\main.rs:894:
field assignment on non-object value:
RawJson("{\"id\":7,\"title\":\"sss\",\"done\":false,\"created_at\":\"\"}")
```

`select … first` yields `V::RawJson` — that's the whole point of the variant
(`native_prelude.rs.in:149`, skip the `V::Object` FxHashMap roundtrip). But
neither accessor knows about it:

- `jwc_set_field` (`native_prelude.rs.in:881`) matches `Object` / `Record` and
  `panic!`s on everything else — so a write is a hard crash, one per request,
  and the client just loses the connection.
- `jwc_get_field` (`native_prelude.rs.in:866`) has the same two arms with
  `_ => V::Null`, so a *read* off a select result is silently null rather than
  a crash. That one is worse: no diagnostic anywhere.

The interpreter handles both (it falls back to the JSON-string path), so
`jwc test` / `jwc lint` / `jwc run` are all clean and the failure only shows up
after `--native`.

Fix: give both helpers a `V::RawJson` arm that parses to `V::Object` (in
`set_field`, materialise then write back through `*v`). A parity test should
cover read-modify-write on a `select … first` result — `native_parity.rs`
currently never assigns to a field of a query result.

## RESOLVED (0.9.5) — Native `validate body` — two bugs, one of them a live security hole (2026-08-06)

**Both fixed**, and both now covered by `tests/differential/cases/validate_body.*`,
which asserts the status line *and* the envelope shape on both backends for
`pattern`, `minLength` and `required` failures plus the accepted case — the
differential test this entry asked for. A present, non-matching value is
rejected natively (400, `code: validation_failed`), so the `javascript:` relay
is closed.

Original report follows.

Found while benchmarking jwc-shortener on 0.8.7. Same source file, same
request, two backends:

```
POST /api/links  {"url":"javascript:alert(1)"}

jwc run          400  {"code":"validation_failed",
                       "details":{"url":"pattern(^https?://)"},
                       "error":"Request body failed validation","status":400}

--native         201  {"code":"9e73bef", ...}       ← link created and stored
```

### A. `pattern(...)` is not enforced against a present, non-matching value

The route declares `url: required, minLength(8), maxLength(2048),
pattern(r"^https?://")` precisely so the service can't be turned into a
phishing relay. `minLength` works natively (`{"url":"ab"}` is rejected) and
`pattern` fires when the field is *absent* — but a value that is present and
does not match sails through. `javascript:` and `data:` URLs are shortened and
stored, and the redirect then emits them as a `Location:` header. This is live
on the deployed app, because production is the `--native` build.

### B. A validation failure answers HTTP 200

Native rejections come back with the right JSON but the wrong status line:

```
HTTP/1.1 200 OK
{"error":"Validation failed","fields":{"url":"minLength 8"},"status":400}
```

Two problems in one response. The status is 200, not 400 — so `res.ok` is true
in a browser and jwc-shortener's landing page renders `undefined` instead of
the error. And the envelope is the pre-0.7.0 shape (`fields`, no `code`), so
native never got the "one error envelope" work that 0.7.0 shipped for the
interpreter — a client cannot branch on `code` the way the changelog promises.

Worth a differential test: `tests/native_parity.rs` should assert status line
*and* body shape for each `validate` failure mode, not just the body.

**This also silently corrupts load tests.** Every `POST` bombardier sent to
that route counted as a 2xx while inserting nothing, which is how a "118,000
rps" write-path measurement happened — it was measuring rejections.

## Found while moving jwc-shortener from 0.6.3 → 0.8.8 (2026-08-06)

> **Re-checked against v1 (2026-08-20).** Every file path below — `src/runner/`,
> `src/native_build.rs`, `src/project.rs`, `src/server.rs` — was deleted at the
> v0.25.0 cutover, so none of these entries could be closed by pointing at a
> line. Each was re-derived against the current tree instead:
>
> | | v1 verdict |
> |---|---|
> | 1 `raw_sql` first column must be text | **Moot by construction.** Every statement the runtime sends projects one text column: the query compiler wraps in `json_agg(…)::text` / `row_to_json(…)::text`, and so does the `raw()` hatch (`exec_call.rs`). There is no path left that returns a bare `int8` to read. The adjacent bug was real, though — `db::run_on` reached the same column with `.unwrap_or(None)`, which turned a projection that *was* wrong into "no rows": 404 under `Shape::First`, `[]` under `Shape::Rows`, indistinguishable from an empty table. It now surfaces as a fault. |
> | 2 unqualified call into a dependency namespace | **Resolved by the flat declaration space** (names §5.1, §6.4.1): a free function is callable unqualified from anywhere, and `import` is checked without scoping, so there is no resolver step left to be missing. The hyphen half went with it — an `import` names a `dependencies` key, and the `redis` package is named without one for exactly this reason. |
> | 3 Windows `[::]` bound without clearing `IPV6_V6ONLY` | **Moot:** v1 binds `0.0.0.0` explicitly, so the dual-stack ambiguity never arises. It left a successor, now closed: the address was not configurable at all, so a development machine had no way to keep the listener off the LAN. `server { bind = … }` (config §3.2). |
> | 4 `return { status: N, … }` silently answers 200 | **Resolved, and harder than this entry asked for.** It proposed a `W0xx` lint; v1 makes it `E0732` at check time — a route body that returns a non-response does not compile. |

All four were reproduced against jwc 0.8.7 on Windows, with the
[jwc-shortener](https://github.com/just-web-code/jwc-shortener) app running on a
throwaway Postgres 17 container. The first three are interpreter/native
divergences: the same source is correct under `jwc build --native` and broken
under `jwc run`, so `jwc test` + `jwc lint` pass clean and the failure only
appears at request time.

### 1. Interpreter `raw_sql` reads only a *text* first column

`engine::rows_first_column_text` (`src/engine.rs:630`) does
`try_get::<_, Option<String>>(0)`, so any non-text first column errors:

```
raw_sql("SELECT 1", "[]")
→ Expected query to return text in first column
  | caused by: error deserializing column 0
  | caused by: cannot convert between the Rust type
    `core::option::Option<alloc::string::String>` and the Postgres type `int8`
```

Native has no such limit — `jwc_db_query` → `jwc_row_to_v`
(`src/native_prelude_db.rs.in:89`) maps whatever type the column has. In the
shortener this took out `/readyz` (`SELECT 1`) and `/api/v1/stats`
(`COUNT(*)`, `COALESCE(SUM(hits), 0)`) under `jwc run` while production was
fine. Worked around app-side with explicit `::text` casts.

Fix: make the interpreter mirror the native conversion (int / float / bool /
numeric → their `Value` variants) instead of demanding text.

### 2. Interpreter can't resolve an unqualified call into a dependency namespace

`Vm::resolve_function` (`src/runner/mod.rs:806`) tries FQN → caller namespace →
caller's imports → root, then gives up. The native resolver
`Resolver::resolve_fn` (`src/native_build.rs:258`) has one more step: **any
single unique match across all namespaces** (`native_build.rs:291`).

`project::merge_dep_package` (`src/project.rs:518`) stamps every dependency
declaration with the package name as its default namespace, so a package
function is never in root. Result: an app that calls a dependency function
unqualified compiles and runs natively but dies under `jwc run` with
`Unknown function: qr_img_tag`.

Compounding it: the package is named `qr-lite`, so its namespace is `qr-lite`
— and a hyphen isn't a legal identifier, so `import qr-lite;` and
`qr-lite.qr_img_tag(...)` can't be written either. There is no interpreter-side
workaround for a hyphenated package name today.

Fix: either give the interpreter the same unique-match fallback, or normalise
hyphens in the default namespace (`qr-lite` → `qr_lite`) so an `import` is
expressible — and decide which, since they differ in what a name clash does.

### 3. Windows: interpreter binds `[::]` without clearing `IPV6_V6ONLY`

`bind_listener` (`src/server.rs:461`) does a plain
`TcpListener::bind("[::]:{port}")`. Windows defaults `IPV6_V6ONLY` to on, so
the bind *succeeds* (the `0.0.0.0` fallback never triggers) and IPv4 is
silently unreachable:

```
http://localhost:8080/healthz  → 200
http://[::1]:8080/healthz      → 200
http://127.0.0.1:8080/healthz  → Unable to connect to the remote server
```

0.8.5 fixed exactly this for the native backend — see the `set_only_v6(false)`
call in `src/native_prelude.rs.in:692` — and added `JWC_BIND_HOST`, but the
interpreter's listener never got either. Port the same two changes.

### 4. `return { status: N, … }` silently answers 200

An HTTP status only reaches the response through the `__jwc_status__` envelope
that the response builtins stamp (`statusCode`, `ok`, `created`, `notFound`, …
in `src/runner/eval.rs`). A handler or middleware returning a bare object
literal with a `status` key gets a `200 OK` whose body happens to contain
`"status": 429`, on **both** backends, with no diagnostic anywhere.

In jwc-shortener that meant the rate limiter never actually returned 429, and
`/readyz` answered 200 while reporting `{"status":503,"error":"db_unreachable"}`
— so the k8s readiness probe passed with Postgres down and the pod stayed in
rotation. Both had been shipped and looked correct in review.

Fix: a lint (new `W0xx`) on a route/middleware `return` of an object literal
carrying a top-level `status` (and maybe `error`) key, pointing at
`statusCode(n, body)`. Not a hard error — the shape is legal as a plain body.

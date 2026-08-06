# TODO

Open defects found in the field, not yet scheduled into `ROADMAP.md`.

## Found while moving jwc-shortener from 0.6.3 → 0.8.8 (2026-08-06)

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

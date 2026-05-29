# Interpreter ↔ Native AOT parity notes

Tracking of known behavioural differences between `jwc run` (interpreter) and
`jwc build --native` (AOT). Audited during the v0.4.0 sprint (Day 8).

## Resolved in v0.4.0

- **Array builtins** (`[..]`, `range`, `push`/`append`, `join`) — byte-identical
  in both modes.
- **Hash builtins** (`sha256`, `sha1`, `md5`, `hmac_sha256`, `hash_password`,
  `verify_password`) — identical; `hash_password`/`verify_password` are now
  accepted by native AOT (were interpreter-only).
- **Module-level `const`** — identical; native emits `jwc_const_<name>()`.
- **Custom MIME** (`response`/`raw`) and **`text()`** — identical Content-Type
  and body. `text()` was previously native-only and errored in the interpreter.
- **Response helper names** (`ok`, `not_found`, `no_content`, `bad_request`,
  `internal_error`) — previously errored with "Unknown function" in the
  interpreter while working in native. Now dispatched in both.

## Known divergences — deferred to v0.4.1 (non-blocking)

- **Error/status helper body shape.** For `not_found`/`bad_request`/
  `internal_error`/`unauthorized`/`forbidden`, the interpreter returns a JSON
  envelope (e.g. `{"status":404,"error":"Not Found"}` → body `{"error":"Not
  Found"}` with `application/json`), whereas native `make_response` emits the
  plain reason string as `text/plain` (body `Not Found`). Status codes match;
  the body representation and Content-Type differ. Reconciling requires picking
  one convention across both runtimes — a v0.4.1 design decision.

- **`ok(value)`.** Interpreter bakes `status:200` into an object body (or wraps
  non-objects as `{"status":200,"data":...}`); native classifies `Str` →
  `text/plain` and everything else → `application/json` at 200. Status matches;
  body framing differs in the same way as above.

## Environment caveats during this audit

- `examples/testapp` and `examples/microblog` parse + validate (`jwc check`) in
  both modes, but their DB-backed routes require a live Postgres
  (`testcontainers`/Docker) to exercise end-to-end. Route-by-route output
  diffing of the DB paths was not run in the sprint's headless environment.
- Live `Ctrl+C` graceful-shutdown draining and the WebSocket `1001` close frame
  (Day 7) compile and serve in both modes but were not exercised under a real
  SIGINT headless.

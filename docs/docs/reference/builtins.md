---
sidebar_position: 1
---

# Built-ins reference

Cross-cutting catalog of every built-in the runtime exposes. The
per-topic docs under [stdlib/](../stdlib/strings.md) are still the
primary tutorials — this page is the alphabetical reference + the few
HTTP-server-side helpers that don't fit cleanly under stdlib.

The canonical source is
[`src/builtins.rs`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/src/builtins.rs).

## Request / response — the `after` triad

These three return the per-request identity / outcome. Designed for
the middleware response phase (`after { ... }`) but can also be read
mid-handler.

| Built-in | Returns | Notes |
|---|---|---|
| `request_id()` | `string` | Stable per-request id stamped at dispatch; same id echoes back as the `x-request-id` response header and appears in every access-log line. Works in request and response phases. |
| `response_status()` | `int?` | The wire status the handler emitted (200, 201, 500, etc.), including any explicit `response(status, body)` call. Returns `null` outside an `after` block — the value isn't known until the handler returns. |
| `response_duration_ms()` | `int` | Milliseconds since the dispatcher first saw the request. Works in both phases; in the request phase it's "ms so far". |

See [backend/middleware](../backend/middleware.md#response-phase-after--) for the response-phase pattern.

## Request inspection

| Built-in | Returns | Notes |
|---|---|---|
| `header(name)` | `string?` | Case-insensitive header lookup. |
| `path_param(name)` | `string?` | Route placeholder value (`:id` etc.). |
| `query_param(name)` | `string?` | `?key=value` lookup. |
| `body()` | `any` | Parsed JSON body — coerced into the declared type when the binding is annotated. |
| `request_path()` | `string` | The URL path (without query). |
| `request_method()` | `string` | `GET`, `POST`, etc. |
| `client_ip()` | `string` | Peer address, peeled through `JWC_TRUSTED_PROXIES`. See below. |

### `client_ip()` and trusted proxies

`client_ip()` reads the `JWC_REAL_IP_HEADER` header (default
`x-forwarded-for`) and walks the chain right-to-left, peeling off each
entry while it matches an IP / prefix in `JWC_TRUSTED_PROXIES`. The
first entry that is **not** in the trusted set wins. Empty
`JWC_TRUSTED_PROXIES` ⇒ "trust no proxy" ⇒ the rightmost (peer) entry
is returned, which is the safest default for a server exposed directly.

Pair this with `JWC_HTTP_ALLOWLIST` (v0.4.7) on outbound requests so a
handler can't be tricked into hitting an internal address via
`http_get(body().url)` — see [stdlib/http](../stdlib/http.md) and
[security/](../security/index.md).

## Responses

| Built-in | Returns | Notes |
|---|---|---|
| `json(v)` | `Response` | Serialise + send as `application/json`. Validates that string input is valid JSON. |
| `json_unchecked(v)` | `Response` | Same as `json(v)` but skips the string-validation arm — use for performance-critical paths where you've already produced trusted JSON (e.g. a `json_stringify` round-trip or a `raw_sql` text result). |
| `text(s)` | `Response` | `text/plain`. |
| `html(s)` | `Response` | `text/html`. |
| `response(status, body)` | `Response` | Explicit status + body. |
| `ok(v)` / `created(v)` / `not_found(v)` / `bad_request(v)` / `unauthorized()` / `forbidden()` / `internalError(v)` | `Response` | Conventional status helpers. |

## Stdlib reference (cross-link)

The per-topic stdlib pages are the tutorial home for everything below;
this list is just a pointer.

- [strings](../stdlib/strings.md) — `length`, `upper`, `lower`, `trim`, `replace`, `split`, `substring`, `take`, `contains`, `startswith`, `endswith`.
- [arrays-json](../stdlib/arrays-json.md) — `length`, `first`, `last`, `contains`, `range`, `push`, `append`, `join`, `json_parse`, `json_stringify`.
- [http](../stdlib/http.md) — `await http_get`, `await http_post`, `await fetch_json`, `await sleep_ms` (subject to `JWC_HTTP_ALLOWLIST`).
- [jwt-passwords](../stdlib/jwt-passwords.md) — `jwt_sign`, `jwt_verify`, `password_hash`, `password_verify`, `sha256_hex`.
- [email](../stdlib/email.md) — `send_email`.
- [cache](../stdlib/cache.md) — `cache_get`, `cache_put`, `cache_delete`.
- [misc](../stdlib/misc.md) — `env`, `int`, `uuid`, `now`, `now_epoch`, `print`, `serve`, `setConnectionString`.

## Verification

If a builtin is documented here but not in `src/builtins.rs`, the
reference page is wrong — please open an issue. The list is generated
by hand for now; a future doc-test will diff this page against the
runtime registry.

<!-- TODO[docs]: wire a build-time check that diffs this page against src/builtins.rs::BUILTINS to catch drift -->

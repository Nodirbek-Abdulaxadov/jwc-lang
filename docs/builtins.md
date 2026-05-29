# JWC Built-in Functions

Reference for every built-in, derived from the single source of truth
`src/builtins.rs` (`BUILTIN_DEFS`). The **Native** column shows whether
`jwc build --native` (AOT) accepts the call; built-ins marked *interpreter*
run under `jwc run` but are rejected at native-build time.

## Strings

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `length(x)` | 1 | ✅ | Length of a string, array, or object (chars/elements/keys). |
| `len(x)` | 1 | interpreter | Alias of `length`. |
| `lower(s)` | 1 | ✅ | Lowercase a string. |
| `upper(s)` | 1 | ✅ | Uppercase a string. |
| `trim(s)` | 1 | ✅ | Strip leading/trailing whitespace. |
| `contains(haystack, needle)` | 2 | ✅ | Substring / array-element / object-key membership. |
| `starts_with(s, p)` | 2 | ✅ | Prefix test. |
| `ends_with(s, p)` | 2 | ✅ | Suffix test. |
| `replace(s, from, to)` | 3 | ✅ | Replace all occurrences. |
| `split(s, sep)` | 2 | ✅ | Split into a JSON array. |
| `first(xs)` / `last(xs)` | 1 | ✅ | First/last element of an array (or char of a string). |

## Arrays (v0.4.0)

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `range(n)` / `range(start, end)` / `range(start, end, step)` | 1–3 | ✅ | Integer array `[start, …, end-1]`; `step` must be positive. |
| `push(arr, x)` / `append(arr, x)` | 2 | ✅ | Append `x` to the array variable in place; returns the array. |
| `join(arr, sep)` | 2 | ✅ | Stringify each element and concatenate with `sep` (O(n)). |

## JSON

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `json_parse(s)` | 1 | ✅ | Parse a JSON string into a value. |
| `json_stringify(v)` | 1 | ✅ | Serialise a value to a JSON string. |
| `set_json_field(obj, key, value)` | 3 | interpreter | Set a field on a JSON-object string. |

## HTTP request

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `path_param(name)` | 1 | ✅ | Route path parameter (`/users/{id}`). |
| `query_param(name[, default])` | 1–2 | ✅ | Query-string parameter. |
| `header(name)` | 1 | ✅ | Request header (case-insensitive). |
| `body()` / `request_body()` | 0 | ✅ / interpreter | Raw request body string. |
| `request_path()` | 0 | ✅ | Request path (query stripped). |
| `request_method()` | 0 | ✅ | HTTP method. |

## HTTP response

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `json(v)` | 1 | ✅ | JSON response. A **string** argument is passed through verbatim (no re-encode). |
| `text(body)` | 1 | ✅ | `text/plain; charset=utf-8` response. |
| `html(body)` | 1 | ✅ | `text/html; charset=utf-8` response. |
| `response(body, mime)` / `raw(body, mime)` | 2 | ✅ | Custom Content-Type; `text/*` gets `; charset=utf-8`. (v0.4.0) |
| `ok(value?)` | 0–1 | ✅ | 200 response. |
| `created(value)` | 1 | ✅ | 201 response. |
| `not_found()` / `notFound()` | 0 | ✅ | 404 response. |
| `no_content()` / `noContent()` | 0 | ✅ | 204 response. |
| `bad_request(msg?)` / `badRequest(msg?)` | 0–1 | ✅ | 400 response. |
| `unauthorized()` | 0 | ✅ | 401 response. |
| `forbidden()` | 0 | ✅ | 403 response. |
| `internal_error(msg?)` / `internalError(msg?)` | 0–1 | ✅ | 500 response. |
| `status_code(code, body_or_headers?)` / `statusCode(...)` | 1–2 | ✅ | Arbitrary status; object body on a 3xx becomes response headers. |

> Note: error/status helper *body shapes* differ slightly between the
> interpreter (JSON envelope) and native (`text/plain` reason). Status codes
> match. See `docs/parity-notes.md` (deferred to v0.4.1).

## Hashing & crypto (v0.4.0)

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `sha256(s)` / `sha1(s)` / `md5(s)` | 1 | ✅ | Lowercase hex digest. |
| `hmac_sha256(key, msg)` | 2 | ✅ | Lowercase hex HMAC-SHA256. |
| `hash_password(pwd)` | 1 | ✅ | Argon2id PHC hash with a random salt. |
| `verify_password(pwd, stored_hash)` | 2 | ✅ | Verify a password against a stored hash. |
| `jwt_sign(payload_json, secret)` | 2 | interpreter | HS256 JWT. |
| `jwt_verify(token, secret)` | 2 | interpreter | Verify an HS256 JWT, returns the payload. |

## Time, identifiers, env

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `now()` | 0 | ✅ | Current UTC time as an RFC 3339 string. |
| `unix_timestamp()` | 0 | interpreter | Current Unix time (seconds). |
| `uuid()` | 0 | ✅ | Random UUID v4. |
| `env(name)` | 1 | ✅ | Environment variable (empty string if unset). |
| `int(v)` | 1 | ✅ | Coerce to integer (parse strings, truncate floats). |

## Cache (in-memory, TTL)

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `cache_get(key)` | 1 | ✅ | Read a cached value (or null). |
| `cache_set(key, value[, ttl_secs])` | 2–3 | ✅ | Store a value with an optional TTL. |
| `cache_del(key)` | 1 | ✅ | Remove a key. |
| `cache_clear()` | 0 | ✅ | Clear the whole cache. |

## Database & async I/O

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `raw_sql(sql[, params_json])` | 1+ | ✅ | Execute raw SQL; returns JSON. |
| `db_query(sql, …)` | 1+ | interpreter | Parameterised query helper. |
| `setConnectionString(url_or_object?)` | 0–1 | ✅ | Pin the DB connection for the process. |
| `sleep_ms(n)` | 1 | ✅ | Async sleep. |
| `http_get(url)` | 1 | ✅ | HTTP GET, returns the body string. |
| `http_post(url[, body])` | 1–2 | interpreter | HTTP POST. |
| `fetch_json(url)` | 1 | ✅ | `http_get` + `json_parse`. |

## WebSocket

| Built-in | Args | Native | Description |
|----------|------|--------|-------------|
| `ws_send(msg)` | 1 | ✅ | Send a text frame (inside a WS route). |
| `ws_recv()` | 0 | ✅ | Receive the next text frame (or null on close). |
| `ws_close()` | 0 | ✅ | Close the connection. |

## Jobs, email, context (interpreter-only)

| Built-in | Args | Description |
|----------|------|-------------|
| `register_job_handler(name, fn)` | 2 | Register a background job handler. |
| `enqueue(name, …)` / `enqueue_urgent(name, …)` | 1+ | Enqueue a job. |
| `job_count()` / `dlq_count()` / `dlq_drain()` | 0+ | Queue / dead-letter introspection. |
| `send_email(…)` | 1+ | Send email via SMTP. |
| `setContext(key, value)` / `context(key)` | 2 / 1 | Per-request key/value bag. |
| `dispatch(…)` | 1+ | Internal route dispatch. |

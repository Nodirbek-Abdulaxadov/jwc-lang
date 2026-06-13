# JWC Builtin Functions — Contract

Status: **DRAFT** · Reflects: **v0.4.8** — extraction in progress from
`src/runner/builtins.rs` and `src/runner/mod.rs`. Phase 0 deliverable.

**Related spec docs**:
[index](index.md) ·
[semantics](semantics.md) (§4 value model the signatures below
operate on) ·
[aot-scope](aot-scope.md) (which builtins lower to `--native`) ·
[threat-model](threat-model.md) (the HTTP / JWT / SQL builtins gated
here have runtime defences documented there).

This document is the contract; the implementation (`runner/builtins.rs`)
is the reference until each builtin lands here with a pinned conformance
case. Until then, see [`docs/builtins.md`](../builtins.md) (the
user-facing reference) for current behaviour.

---

## How to read an entry

Each builtin has a single canonical entry:

```
NAME — one-line purpose

Signature: name(arg: Type, ...) -> ReturnType
Errors:    list of conditions that raise at runtime
Notes:     edge-cases, null-handling, locale rules
Tests:     names of conformance cases pinning the behaviour
```

`Type` references match `docs/spec/grammar.ebnf::type_ref`. A trailing
`?` on a parameter type marks it as optional.

---

## String functions

### `length` (alias `len`)

Returns the count of characters in a string, the cardinality of an
array, or the field count of an object literal.

```
Signature: length(x: string | array | object) -> int
Errors:    none — null returns 0
Notes:     character count uses `s.chars().count()`, NOT byte length.
Tests:     case_strings, case_arrays, case_object_len  (TODO add)
```

### `upper`, `lower`

Case-fold the string per ASCII rules. Non-ASCII characters pass through
unchanged (this is a known limitation tracked for Phase 3).

```
Signature: upper(s: string) -> string
Signature: lower(s: string) -> string
Errors:    none — null returns null
Tests:     case_strings
```

### `trim`, `trim_start`, `trim_end`

Strip Unicode whitespace from the ends of the string.

### `starts_with`, `ends_with`

Boolean prefix/suffix test.

```
Signature: starts_with(s: string, p: string) -> bool
Signature: ends_with(s: string, p: string)   -> bool
Errors:    none — null inputs return false
```

### `replace`

Substring replacement, all occurrences.

```
Signature: replace(s: string, from: string, to: string) -> string
Errors:    none — null source returns null
Tests:     case_strings
```

### `substring`, `take`

Character-based string slicing. `substring(s, start, len)` returns up to
`len` characters starting at the 0-based char index `start`. `take(s, n)`
is shorthand for `substring(s, 0, n)`.

Out-of-range indices clamp to empty: `start < 0`, `len <= 0`, or `n <= 0`
yields `""`. Running past the end of `s` is not an error — the trailing
slice is returned as-is. Iteration is char-based, not byte-based, so
UTF-8 input (Uzbek, Cyrillic, emoji) is sliced correctly. Both
short-circuit through `null` (null input → null output) for consistency
with the other string builtins.

```
Signature: substring(s: string, start: int, len: int) -> string
Signature: take(s: string, n: int) -> string
Errors:    s/start/len/n must match declared type when present (TypeError)
Tests:     substring_slices_chars_with_clamping, take_returns_prefix_of_string,
           substring_basic, take_basic
```

Defers to user-declared functions of the same name when one exists in
the program — neither identifier is reserved.

### `split`

Splits on a substring separator; returns the pieces as a JSON-array string
in the legacy value model (Phase 1 will lift this to a real array).

```
Signature: split(s: string, sep: string) -> string   (* JSON array *)
Errors:    none — null source returns "[]"
Tests:     case_strings
```

## HTTP request inspection

### `client_ip`

Original client IP. Reads the header named by `JWC_REAL_IP_HEADER`
(default `x-forwarded-for`), walks the comma-separated chain RIGHT to
LEFT, peels off entries whose prefix matches `JWC_TRUSTED_PROXIES`
(comma-separated list, empty default), and returns the first untrusted
entry. Returns `null` when the header is absent or every entry was
trusted (degenerate case).

```
Signature: client_ip() -> string | null
Errors:    none — degrades to null
Tests:     client_ip_returns_rightmost_untrusted_entry,
           client_ip_peels_trusted_proxies_off_the_chain,
           client_ip_returns_null_when_header_absent
```

### `request_id`

The stable per-request identifier the server stamps. Reused on every
log line and echoed back as the `x-request-id` response header.
Incoming W3C `traceparent` upstream IDs are honoured — if the upstream
service already started a trace, `request_id()` returns the inbound
`trace-id` so distributed tracing tools can correlate hops.

```
Signature: request_id() -> string | null
Errors:    none — null outside a server request
Tests:     request_id_is_visible_when_server_stamps_one,
           request_id_returns_null_when_unstamped
```

### `response_status`

HTTP status the handler emitted. Read inside a middleware `after { ... }`
block; `null` elsewhere (the value isn't known until after the handler
returns).

```
Signature: response_status() -> int | null
Errors:    none — null outside an after-block
Tests:     after_middleware_block_sees_response_status
```

### `response_duration_ms`

Milliseconds since the dispatcher saw the request. Valid in any
request-scoped block — middleware (before / after), handler,
errorHandler. `null` outside requests.

```
Signature: response_duration_ms() -> int | null
Errors:    none — null outside requests
Tests:     after_middleware_response_duration_reads_back
```

## Array functions

### `first`, `last`

Returns the first or last element of an array, or `null` for empty.

```
Signature: first(xs: array) -> any | null
Signature: last(xs: array)  -> any | null
```

### `push`, `append`, `join`, `range`

Standard array surgery — entries to be filled in alongside the array
value-model cleanup in Phase 1.

## JSON

### `json_parse`, `json_stringify`

Round-trip between JSON text and JWC values. `json_parse` of malformed
input raises a runtime error.

### `set_json_field`

Mutates a JSON-string value's field, returning the new JSON string.

## Hashing

All four hashing builtins share the same contract surface:

- Input is the UTF-8 byte representation of the string argument; passing
  a non-string value is a `TypeError` at runtime (the type-checker
  catches it at compile time when the input has a declared type).
- `null` input passes through as `null` — useful for hashing optional
  audit columns without nesting `if x != null { sha256(x) }`.
- Output is **lower-case hexadecimal**, never base64, never raw bytes.
  Length is fixed by the algorithm: 64 chars for `sha256`, 40 for
  `sha1`, 32 for `md5`, 64 for `hmac_sha256`.
- The output is byte-stable across releases — these hashes are routinely
  used as cache keys and password verifiers, so the value model freeze
  applies to them at v1.0.
- None of these are cryptographically suitable for password storage on
  their own. JWC ships `password_hash()` / `password_verify()` for that
  case (argon2). The hash builtins below are documented as
  general-purpose digests, NOT credential primitives.

### `sha256(s: string) -> string`

```
Signature: sha256(s: string) -> string
Errors:    TypeError if s is not a string or null
Notes:     SHA-256 of UTF-8 bytes of s. Lower-case hex, length 64.
           null -> null.
Tests:     case_hash_helpers (sha256_hex_of_known_input)
```

### `sha1(s: string) -> string`

```
Signature: sha1(s: string) -> string
Errors:    TypeError if s is not a string or null
Notes:     SHA-1 of UTF-8 bytes of s. Lower-case hex, length 40.
           null -> null. Documented as "not collision-resistant" — use
           sha256 for anything new.
Tests:     case_hash_helpers (sha1_hex_of_known_input)
```

### `md5(s: string) -> string`

```
Signature: md5(s: string) -> string
Errors:    TypeError if s is not a string or null
Notes:     MD5 of UTF-8 bytes of s. Lower-case hex, length 32.
           null -> null. Provided for legacy integration (S3 ETags,
           gravatar) only.
Tests:     case_hash_helpers (md5_hex_of_known_input)
```

### `hmac_sha256(payload: string, secret: string) -> string`

```
Signature: hmac_sha256(payload: string, secret: string) -> string
Errors:    TypeError on non-string arguments
Notes:     HMAC-SHA256 of payload with secret (RFC 2104, key padded /
           hashed per the spec). Lower-case hex, length 64.
           Either argument null -> null. Constant-time compare is NOT
           applied here — callers comparing two HMACs MUST use
           constant_time_eq() to avoid timing oracles.
Tests:     case_hash_helpers (hmac_sha256_hex_of_known_input,
           hmac_sha256_matches_rfc4231_vector)
```

## Time

### `now`

Returns the current UTC time as an ISO-8601 string.

### `unix_timestamp`

Returns the current UTC time as an `int` (seconds since the epoch).

> **Conformance note**: `unix_timestamp` is excluded from the native
> AOT conformance run by the case header
> `// CONFORMANCE: interpreter-only`. See `tests/conformance.rs`.

## HTTP / server

### `body`

Returns the raw request body string for the current handler.

### `response`

Manually set the response status + body.

### `serve`

Starts the HTTP server from `main()`. Optional port argument; defaults
to the value of `JWC_SERVER_PORT` or `8080`.

> **TODO Phase 5**: `client_ip()` — trusted-proxy-aware client IP from
> the configured forwarded header.

## Database

DB surgery is exposed through SQL-shaped expressions
(`select`/`update`/`delete`), not standalone builtins. The builtin shape
that *does* exist (`raw_sql`, parameter binding helpers) will be folded
in as the Phase 1 value model lands so the contract reads against the
new types.

---

## Removal / deprecation registry

None yet. Deprecations will be recorded here AND in `CHANGELOG.md` per
[`DEPRECATION.md`](../../DEPRECATION.md).

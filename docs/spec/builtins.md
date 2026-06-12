# JWC Builtin Functions — Contract

Status: **DRAFT** — extraction in progress from `src/runner/builtins.rs`
and `src/runner/mod.rs`. Phase 0 deliverable.

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

> **TODO Phase 3**: `substring(s: string, start: int, len: int) -> string`
> and `take(s: string, n: int) -> string` (production gap from
> jwc-shortener — see PRODUCTION_READINESS_PLAN Phase 3).

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

### `sha256`, `sha1`, `md5`

Hex-encoded digest of the UTF-8 bytes of the input.

### `hmac_sha256`

HMAC of a payload with a shared secret, hex-encoded.

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

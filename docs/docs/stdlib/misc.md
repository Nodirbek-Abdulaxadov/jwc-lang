---
sidebar_position: 7
description: "The remaining JWC built-ins: environment variables with defaults, time and timestamps, UUIDs, random numbers, hashing and encoding helpers."
---

# Misc helpers

| Built-in | Returns | Notes |
|---|---|---|
| `env(key)` | `string?` | process env var, `null` if unset |
| `int(v)` | `int?` | coerces to int. Strings are **trimmed** then parsed, and an unparseable one **raises** `ValidationError` — it does not answer `0`. Truncates floats; `true`/`false` → `1`/`0`; `null` passes through as `null` |
| `uuid()` | `string` | new UUID v4 |
| `now()` | `string` | ISO 8601 UTC, e.g. `2026-05-23T20:34:00Z` |
| `unix_timestamp()` | `int` | Unix seconds (UTC) |
| `print(v)` | `void` | **not plain stdout** — see the note below. For logging use [`console.write`](io.md) |
| `serve(port)` | `void` | called from `main()` to start the HTTP server; on Ctrl+C (SIGINT) it drains in-flight requests (timeout `JWC_SHUTDOWN_TIMEOUT` seconds, default 5) before exiting |
| `setConnectionString(url)` | `void` | overrides `JWC_DATABASE_URL` at runtime (rare; mostly for tests) |

## `print` is not a logging function

Under `jwc run`, `print` appends to an internal buffer that is flushed only
after `main()` returns — so a prompt printed before reading input appears
*after* you were meant to answer it. Inside a route body it is worse: when
the handler falls through without an explicit `return`, whatever it
`print`-ed becomes the HTTP response body.

`console.write(v)` writes to the real stdout immediately and never becomes
part of a response. That is the one to reach for when logging from a
handler. See [Console + files](io.md), including why you should not mix the
two in one program.

In a `jwc build --native` binary `print` *is* an immediate `println!`,
which is exactly why mixing them orders differently on the two backends.

## Patterns

```jwc no-compile
// optional env with default.
// `||` is boolean-only in JWC, so compare explicitly — `env()` returns the
// empty string when the variable is unset, not null.
let raw = env("MAX_ITEMS");
let max = 20;
if (raw != "") { max = int(raw); }

// jwt with absolute exp
let exp = unix_timestamp() + 24 * 3600;
let token = jwt_sign(
    json_stringify({ sub: user.id, exp: exp }),
    env("JWT_SECRET")
);
```

Two things that trip people up here: `||` takes booleans only, so it cannot
be used to supply a default value; and `env()` returns the **empty string**
for an unset variable, never `null`. Guard with `!= ""`.

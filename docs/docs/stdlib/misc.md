---
sidebar_position: 7
description: "The remaining JWC built-ins: environment variables with defaults, time and timestamps, UUIDs, random numbers, hashing and encoding helpers."
---

# Misc helpers

| Built-in | Returns | Notes |
|---|---|---|
| `env(key)` | `string?` | process env var, `null` if unset |
| `int(v)` | `int` | coerces to int: string→int parse (`0` on bad input, never throws); truncates floats; `true`/`false` → `1`/`0` |
| `uuid()` | `string` | new UUID v4 |
| `now()` | `string` | ISO 8601 UTC, e.g. `2026-05-23T20:34:00Z` |
| `unix_timestamp()` | `int` | Unix seconds (UTC) |
| `print(v)` | `void` | stdout — debug only |
| `serve(port)` | `void` | called from `main()` to start the HTTP server; on Ctrl+C (SIGINT) it drains in-flight requests (timeout `JWC_SHUTDOWN_TIMEOUT` seconds, default 5) before exiting |
| `setConnectionString(url)` | `void` | overrides `JWC_DATABASE_URL` at runtime (rare; mostly for tests) |

## Patterns

```jwc no-compile
// optional env with default
let max = int(env("MAX_ITEMS") || "20");

// jwt with absolute exp
let exp = unix_timestamp() + 24 * 3600;
let token = jwt_sign(
    json_stringify({ sub: user.id, exp: exp }),
    env("JWT_SECRET")
);
```

(`||` is logical-or; `env(...)` returning `null` falls through to the default string.)

---
sidebar_position: 7
---

# Misc helpers

| Built-in | Returns | Notes |
|---|---|---|
| `env(key)` | `string?` | process env var, `null` if unset |
| `int(s)` | `int` | string→int parse; throws on bad input |
| `uuid()` | `string` | new UUID v4 |
| `now()` | `string` | ISO 8601 UTC, e.g. `2026-05-23T20:34:00Z` |
| `now_epoch()` | `int` | Unix seconds |
| `print(v)` | `void` | stdout — debug only |
| `serve(port)` | `void` | called from `main()` to start the HTTP server |
| `setConnectionString(url)` | `void` | overrides `JWC_DATABASE_URL` at runtime (rare; mostly for tests) |

## Patterns

```jwc
// optional env with default
let max = int(env("MAX_ITEMS") || "20");

// jwt with absolute exp
let exp = now_epoch() + 24 * 3600;
let token = jwt_sign(
    json_stringify({ sub: user.id, exp: exp }),
    env("JWT_SECRET")
);
```

(`||` is logical-or; `env(...)` returning `null` falls through to the default string.)

---
sidebar_position: 6
---

# In-memory cache

> **Scope note.** Intentionally a process-local cache only. Distributed
> caches (Redis, Memcached) are not a core JWC concern — they belong in a
> separate `jwc-cache` package post-1.0. If you outgrow the in-process
> cache, you're already past JWC's CRUD ergonomics niche; reach for
> Redis directly.

Per-process, TTL-bounded. Good for hot lookups (config, user-permissions); not a Redis replacement.

| Built-in | Returns | Notes |
|---|---|---|
| `cache_set(key, value, ttl_secs)` | `void` | `ttl_secs = 0` → never expires |
| `cache_get(key)` | `string?` | `null` if expired or missing |
| `cache_del(key)` | `void` | |
| `cache_clear()` | `void` | wipe everything |

```jwc
async function getUser(id: int): User? {
    let key = "user:" + id;
    let cached = cache_get(key);
    if (cached != null) { return json_parse(cached); }
    let fresh = first(select User from AppDb.User where User.id == @id);
    if (fresh != null) { cache_set(key, json_stringify(fresh), 300); }
    return fresh;
}
```

## Invalidation

Manual — call `cache_del(key)` on every mutation. There's no auto-purge tied to `update` / `delete`.

## Multi-instance

The cache lives **inside the process**. If you run two pods, each has its own. For shared cache use Redis (lands with the Redis-cache sprint, same surface).

## Memory bound

Unbounded today. If you cache millions of entries, the process grows. Use short TTLs + cache-set sparingly.

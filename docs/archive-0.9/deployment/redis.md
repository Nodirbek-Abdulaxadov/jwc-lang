---
sidebar_position: 9
description: "Wire a JWC app to Redis for cache and rate-limit state that is shared across replicas, including TLS, health probes, metrics and the native AOT path."
---

# Redis

JWC ships an in-process cache (`cache_get` / `cache_set` / `cache_del` /
`cache_clear`) that is fast and needs no infrastructure — and is **per
process**. Run two replicas and each keeps its own copy: a rate limit of
100/min becomes 200/min across two pods, and a cached value invalidated on
one pod stays stale on the other.

The `redis_*` built-ins are the shared-state counterpart. Same key/value
shape, same `ttl_secs == 0 means no expiry` contract — but the state lives
in Redis, so every replica sees the same thing.

## Enabling it

Redis is behind a Cargo feature so the default build pulls in neither the
`redis` nor the `deadpool-redis` crate:

```bash
cargo build --release --features redis
```

Then point the runtime at a server:

```bash
export JWC_REDIS_URL=redis://127.0.0.1:6379
jwc run
```

That is the whole setup. With `JWC_REDIS_URL` unset the built-ins are still
*defined* — programs compile and `jwc check` behaves identically — they just
report "not configured" when called, and `redis_enabled()` answers `false`.

:::note Why the built-ins exist even without the feature

`BUILTIN_DEFS` is not feature-gated. If it were, `jwc check` would accept or
reject the same program depending on how the binary was compiled, and the
generated [built-ins reference](../reference/builtins.md) would change with
build flags. Only the implementation is gated.

A binary built without `--features redis` prints a warning at boot if
`JWC_REDIS_URL` is set, so a misconfiguration doesn't silently degrade to
per-process caching.
:::

## Built-ins

| Call | Returns | Notes |
|---|---|---|
| `redis_get(key)` | string / `null` | `null` for a missing key — never `""` |
| `redis_set(key, value, ttl_secs)` | — | `ttl_secs == 0` means no expiry |
| `redis_del(key)` | int | Number of keys removed (`0` if absent) |
| `redis_exists(key)` | bool | |
| `redis_incr(key)` | int | New value; creates the key at `1` |
| `redis_expire(key, ttl_secs)` | bool | `false` when the key doesn't exist |
| `redis_eval(script, keys_json, args_json)` | string / `null` | Lua; see below |
| `redis_ping()` | bool | Reachability — never raises |
| `redis_enabled()` | bool | Is Redis configured? No round-trip |

Cache-aside, written out — JWC has no first-class functions, so there is no
`remember(key, ttl, fn)` helper to hide the shape:

```jwc
function cached_profile(id: string) {
    let hit = redis_get("profile:" + id);
    if (hit != null) {
        return json_parse(hit);
    }
    let row = load_profile(id);
    redis_set("profile:" + id, json_stringify(row), 300);
    return row;
}
```

### `redis_eval` — when two round-trips aren't enough

`KEYS` and `ARGV` cross as JSON arrays, because JWC has no list-of-string
value to pass. Numbers and booleans are accepted and stringified (Redis is
typeless on the wire, so `ARGV[1]` is always a string in Lua anyway).

The reason to reach for it is atomicity. A rate limiter written as
`redis_incr` followed by `redis_expire` has a window: if the process dies
between the two calls, the counter is left with no TTL and never resets —
that user is blocked forever. One script closes it:

```jwc
const RATE_SCRIPT = "local n = redis.call('INCR', KEYS[1]) " +
                    "if n == 1 then redis.call('EXPIRE', KEYS[1], ARGV[1]) end " +
                    "return n";

function rate_limit(key: string, limit: int, window_secs: int) {
    let n = redis_eval(RATE_SCRIPT, "[\"" + key + "\"]", "[" + window_secs + "]");
    return int(n) <= limit;
}
```

Replies are flattened to a string (`null` for nil). A script returning a
table reads back as its first element, so return a scalar — or
`cjson.encode(...)` the structure and `json_parse` it on the JWC side.

## TLS

Managed Redis (Upstash, ElastiCache with in-transit encryption, Redis
Cloud) requires TLS. Use the `rediss://` scheme — note the double `s`:

```bash
export JWC_REDIS_URL="rediss://default:PASSWORD@eu1-example.upstash.io:6379"
```

Verification uses bundled webpki roots, so this works in a `scratch` /
distroless container with no system trust store.

## Health and metrics

`/readyz` probes Redis **only when it is configured**, so an app without
`JWC_REDIS_URL` keeps exactly the readiness behaviour it had before. A
configured-but-unreachable Redis fails the probe: the reason to configure
it is shared state, and an instance that silently degrades to per-process
behaviour is worse than one pulled out of rotation.

`/metrics` gains four gauges, present only when Redis is configured — a
missing gauge distinguishes "no Redis" from "Redis with an empty pool":

```
jwc_redis_pool_size       # connections the pool currently holds
jwc_redis_pool_available  # idle, checkout-able right now
jwc_redis_pool_max_size   # ceiling (JWC_REDIS_POOL_SIZE)
jwc_redis_pool_waiting    # tasks queued for a slot — non-zero means contention
```

## Error handling

Failures raise `RedisError`, with subtypes for the cases worth branching on:

```jwc
function record_visit(key: string) {
    try {
        redis_incr(key);
    } catch (e: RedisError) {
        // A cache write is not worth failing the request over. `e.type`
        // carries the specific kind — `try` takes a single catch clause,
        // so branch on it here rather than with a second clause.
        if (e.type == "RedisError.ConnectionFailure") {
            print("redis down, continuing without shared counters");
        }
    }
}
```

Catching the parent kind catches the subtypes too, so
`catch (e: RedisError)` above sees a `RedisError.TimedOut` as well.

| Kind | Cause |
|---|---|
| `RedisError.ConnectionFailure` | Socket dropped or an IO error |
| `RedisError.TimedOut` | Command exceeded its deadline |
| `RedisError.LoadingError` | Server still loading its dataset (restart / replica sync) |
| `RedisError.NoScript` | `EVALSHA` for a script the server doesn't have |
| `RedisError` | Everything else, including "not configured" and "built without `--features redis`" |

Transient failures — dropped connection, timeout, `LOADING`, cluster
`MOVED` / `ASK` — are retried automatically with exponential backoff
(`JWC_REDIS_RETRY_MAX_ATTEMPTS`, `JWC_REDIS_RETRY_BACKOFF_MS`). Permanent
ones (`WRONGTYPE`, a Lua syntax error) are not, so bugs surface instead of
livelocking.

:::caution `redis_incr` and retries

A retry after a *timeout* can double-count, because the first attempt may
have landed. For a rate-limit counter that means occasionally counting one
request twice during a network blip — it fails closed, which is the safe
direction. Don't use `redis_incr` where an exact count is load-bearing.
:::

## Native AOT

`jwc build --native` supports every `redis_*` built-in. The generated crate
gets `redis` + `deadpool-redis` only when the program actually calls one,
so a Redis-free program doesn't pay for the dependency.

The AOT binary reads the same `JWC_REDIS_*` variables and produces the same
output as `jwc run` — that parity is what `tests/native_emit.rs` guards.

## Values are UTF-8

Values cross the boundary as JWC strings, which are UTF-8 by construction.
A non-UTF-8 payload written by some other client reads back as an error
rather than as mojibake. Store binary as base64.

## Not included

`LPUSH` / `BRPOP` are deliberately absent. `BRPOP` blocks, holding a pool
connection for its whole timeout, which starves the pool it was checked out
of; both belong to the job-queue tier rather than to a KV cache. They are
tracked against the durable queue's Redis backend, not here.

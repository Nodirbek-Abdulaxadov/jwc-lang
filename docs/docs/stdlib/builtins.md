---
sidebar_position: 1
title: "Built-ins"
description: "Every built-in function JWC has, by group: coercions, dates, text, arrays, hashing, the request, responses and Redis."
---

# Built-ins

Everything here is a function the runtime provides. There is no import,
no namespace to open, and no way to shadow one.

The normative list is
[`docs/spec/v1/builtins.md`](https://github.com/just-web-code/jwc-lang/blob/main/docs/spec/v1/builtins.md);
this page is the same set with the reasons.

## Coercions

| | |
|---|---|
| `int(v)`, `bigint(v)` | parse. A value that is not a number is a **400**, not a plausible-looking `0`. |
| `numeric(v)` | exact decimal, kept as text — money never touches a float |
| `boolean(v)` | `"true"` and `"1"` are true |
| `uuid(v)`, `timestamptz(v)` | assertions the checker already made |
| `enum(E, v)` | the type name is not a value, so it is a separate argument |
| `env(name)` | the variable, or **null** when it is unset |

`env` answering null rather than `""` is what makes the standard shape
work:

```jwc no-compile
serve(int(env("PORT") ?? "8080"));
```

`??` only fires on null.

## Dates

| | |
|---|---|
| `date.now()` | RFC 3339 UTC, microsecond precision |
| `date.today()` | the date, no time |
| `date.days(n)`, `date.hours(n)`, `date.minutes(n)`, `date.seconds(n)` | an interval |
| `date.parse(s)`, `date.format(v, f)` | |

Intervals compose with timestamps in a query:

```jwc no-compile
where created_at > date.now() - date.hours(24)
```

## Text

`string.of`, `string.len`, `string.lower`, `string.upper`, `string.trim`,
`string.replace`, `string.slice`, `string.split`, `string.split_csv`,
`string.join`, `string.contains`, `string.starts_with`,
`string.ends_with`, `string.strip_prefix`, `string.pad_left`,
`string.pad_right`, `string.matches`.

`string.strip_prefix($h, "Bearer ")` is the one to reach for over
`string.replace`, which also strips the literal from the middle of a
token.

## Arrays

`array.len`, `array.is_empty`, `array.first`, `array.last`,
`array.contains`, `array.pluck`, `array.sum`, `array.sum_product`,
`array.min`, `array.max`, `array.sorted`.

The field-taking ones — `array.sum($rows, "amount")` — exist because JWC
has no lambdas. A function is not a first-class value here, so the
alternative to `array.sum(rows, "amount")` would be a `for` loop and an
accumulator.

Most of the time the answer is that the aggregate belongs in the query.

## Hashing, tokens, JWT

| | |
|---|---|
| `hash.password(p)` | Argon2id, salted |
| `hash.verify(p, stored)` | against the stored PHC string |
| `hash.sha256(s)` | hex |
| `hash.hmac_sha256(msg, key)` | hex |
| `hash.hmac_verify(msg, sig, key)` | constant-time |
| `crypto.token(n)` | `n` CSPRNG bytes, base64url |
| `crypto.constant_time_eq(a, b)` | |
| `jwt.sign(claims, secret, ttl_minutes)` | HS256 |
| `jwt.verify(token, secret)` | `Record?`; strips an optional `Bearer ` |

`jwt.sign` fixes the claim set: `sub` from the record, `iat` now, `exp`
`ttl_minutes` later. A caller cannot set `exp` itself, which is what
stops a token outliving the policy that issued it.

`jwt.verify` answering `Record?` is what makes the standard shape a
one-liner:

```jwc no-compile
let claims = jwt.verify($token, $secret) or throw Unauthorized("token yaroqsiz");
```

## The request

`request.body() as C`, `request.header`, `request.query`,
`request.query_all`, `request.method`, `request.path`, `request.route`,
`request.id`, `request.client_ip`, `request.peer_ip`,
`request.raw_body`.

## The response

`json`, `created`, `accepted`, `noContent`, `badRequest`, `unauthorized`,
`forbidden`, `notFound`, `conflict`, `tooManyRequests`, `internalError`,
`statusCode`, `redirect`, `content`, and the suffixes `with { … }` and
`cookie(name, value)`.

From inside an `after` block: `response.status()`,
`response.duration_ms()`, `response.duration_us()`,
`response.set_header(k, v)`, `response.add_header(k, v)`.

## Redis

Available when `JWC_REDIS_URL` is set:

| | |
|---|---|
| `redis.get(k)`, `redis.set(k, v, ttl)`, `redis.del(k)` | `ttl = 0` is no expiry |
| `redis.incr(k)`, `redis.expire(k, ttl)` | |
| `redis.rate_limit(key, limit, window_secs)` | `INCR` + `EXPIRE` in one script |
| `redis.enabled()` | what to branch on when the call is optional |

`rate_limit` is one script rather than two round-trips because `INCR`
then `EXPIRE` races: the loser gets a key that never expires, and the
bucket never resets.

Every other `redis.*` call **raises** when no server is configured. A
rate limiter built on a call that quietly answered null would allow
everything.

## Debug

`debug.dump(v)` returns its argument unchanged, so wrapping a
subexpression in it changes nothing but what is printed. Outside `JWC_DEV`
it prints nothing at all — a debug statement that survived review should
not be what takes an endpoint down.

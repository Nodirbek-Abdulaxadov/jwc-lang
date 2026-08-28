---
sidebar_position: 1
title: "Built-ins"
description: "Every built-in function JWC has, by group: coercions, dates, text, arrays, hashing, the request, responses, Redis, the cache, mail and debug."
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

`string.strip_prefix(h, "Bearer ")` is the one to reach for over
`string.replace`, which also strips the literal from the middle of a
token.

## Arrays

`array.len`, `array.is_empty`, `array.first`, `array.last`,
`array.contains`, `array.pluck`, `array.sum`, `array.sum_product`,
`array.min`, `array.max`, `array.sorted`.

The field-taking ones — `array.sum(rows, "amount")` — exist because JWC
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
| `hash.sha1(s)` | hex — for reading a checksum someone else produced, not for passwords |
| `hash.md5(s)` | hex — same caveat |
| `hash.hmac_sha256(msg, key)` | hex |
| `hash.hmac_verify(msg, sig, key)` | constant-time |
| `crypto.token(n)` | `n` CSPRNG bytes, base64url |
| `crypto.constant_time_eq(a, b)` | |
| `jwt.sign(claims, secret, ttl_minutes)` | HS256 |
| `jwt.verify(token, secret)` | HS256. `Record?`; strips an optional `Bearer ` |
| `jwt.verify_jwks(token, jwks_url)` | RS256 against an OIDC provider's key set. Same `Record?` |

`jwt.sign` fixes the claim set: `sub` from the record, `iat` now, `exp`
`ttl_minutes` later. A caller cannot set `exp` itself, which is what
stops a token outliving the policy that issued it.

`jwt.verify` answering `Record?` is what makes the standard shape a
one-liner:

```jwc no-compile
let claims = jwt.verify(token, secret) or throw Unauthorized("token yaroqsiz");
```

`jwt.verify_jwks` answers the same record, against an OIDC provider's
published key set instead of a secret you hold — so a service that starts
with its own tokens and later moves behind an identity provider changes
one line, not the code that reads the claims:

```jwc no-compile
let claims = jwt.verify_jwks(token, env("JWKS_URL"))
    or throw Unauthorized("token yaroqsiz");
```

It picks the signing key by the token header's `kid` and caches the key
set. A token that does not verify is null; a `jwks_url` that cannot be
reached **raises** — the provider being down is not the same fact as the
token being wrong, and reporting it as one turns an outage into a
site-wide "your credentials are invalid".

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

## HTTP

Calling another service. Restored in 0.9.921 — the cutover deleted it, and
a language for HTTP backends could not make an HTTP request.

| | |
|---|---|
| `http.get(url)` | the response body, as `text` |
| `http.post(url, body)` | same |
| `http.json(url)` | the body as `Raw`, spliced like a `jsonb` column |
| `http.status(url)` | the status code |

```jwc no-compile
let body = http.get("https://api.example.com/rates");
let code = http.status("https://api.example.com/health");
```

**A non-2xx is not an error.** A 404 from a remote service is an answer;
`http.status` is how to ask what it was. What raises is the request never
happening — a refused URL, DNS failing, a timeout — as `BadRequest`, so
`catch BadRequest` recovers it.

### Outbound requests are gated

Both checks run before anything is dispatched:

| | |
|---|---|
| `JWC_HTTP_ALLOWLIST` | comma-separated hosts; empty means no restriction |
| `JWC_HTTP_BLOCK_PRIVATE` | refuses loopback, private and link-local addresses, resolving the host first — `169.254.169.254` is the reason it exists |

Redirects are **not** followed: a redirect is how an allowlisted host walks
you to one that is not. `JWC_HTTP_TIMEOUT_SECS` bounds the request,
default 10.

If a route takes a URL from the request and fetches it, set both.

## JSON

| | |
|---|---|
| `json.parse(text)` | `Raw` — raises `BadRequest` when it is not JSON |
| `json.stringify(v)` | `text` |

`json.parse` answers `Raw`, the same as a `jsonb` column: it splices into a
response and is not read field-wise. To get typed fields out of JSON,
declare a `class` and let validation do it.

## Console

For a program that talks to a person rather than over HTTP. `jwc run` is
what executes one.

| | |
|---|---|
| `console.write(v)` | stdout, **no** trailing newline — for a prompt |
| `console.writeln(v)` | stdout, with one |
| `console.error(v)` | stderr |
| `console.read()` | one line from stdin, `null` at EOF |

```jwc no-compile
function main() {
    console.write("Your name: ");
    let who = console.read();
    console.writeln("Hello, " + (who ?? "stranger"));
}
```

```bash
jwc run app.jwc
```

Any value works — `console.write(42)` is fine. Text prints as its
characters; anything else prints the way `debug.dump` renders it. Both
write paths flush, so a prompt reaches the screen before the read.

`console.read()` returns the line as typed, minus the terminator, without
trimming. `int()` trims before parsing, so `int(console.read())` handles
`" 42 "`.

## Redis

`redis` is a **package**, not part of the language, so a file that uses it
needs `import redis;` and `jwcproj.json` needs `redis` in `dependencies`.
Without the import the names do not resolve (`E0202`).

The calls answer when `JWC_REDIS_URL` is set and the binary was built with
the driver; every name except `redis.enabled()` raises when it is not:

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

## Cache

Process-local, always available, no configuration:

| | |
|---|---|
| `cache.get(k)`, `cache.set(k, v, ttl)`, `cache.del(k)` | `ttl = 0` is no expiry |
| `cache.clear()` | drops every entry |

The four shapes match `redis.*` on purpose — swapping one for the other
is a rename. The scope does not: this store is **one process**. Two
replicas do not share it, a restart empties it, and a rate limiter keyed
here counts per pod, so the real limit is `limit × replicas`. Use
`redis.rate_limit` for that.

Entries are bounded by `JWC_CACHE_MAX_ENTRIES` (default 10 000): at the
cap a write sweeps expired entries, then evicts the oldest. Watch
`jwc_cache_evicted_total` in `/metrics` — a cache that has quietly turned
into a no-op looks the same from outside as one that is working.

## Mail

Available when `JWC_SMTP_HOST`, `JWC_SMTP_USER`, `JWC_SMTP_PASSWORD` and
`JWC_SMTP_FROM` are all set:

| | |
|---|---|
| `mail.send(to, subject, body_html)` | HTML body; raises on a delivery failure |
| `mail.enabled()` | what to branch on when the send is optional |

`JWC_SMTP_PORT` defaults to `587` and `JWC_SMTP_TLS` to `starttls`
(`tls` for implicit TLS on 465, `none` for a local relay).

`mail.send` raises when no relay is configured, for the same reason
`redis.*` does: it used to answer `null`, so a password-reset route
returned 200 and sent nothing.

## Debug

`debug.dump(v)` returns its argument unchanged, so wrapping a
subexpression in it changes nothing but what is printed. Outside `JWC_DEV`
it prints nothing at all — a debug statement that survived review should
not be what takes an endpoint down.

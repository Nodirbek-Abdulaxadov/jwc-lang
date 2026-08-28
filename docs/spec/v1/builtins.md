# builtins.md — the builtin surface

Normative. Builtins are namespaced by **where they run**. Bare names are
language verbs; everything else is qualified.

Closes the declaration-site half of **N5** and the `sum(xs, lambda)` half of
**#22**.

---

## 1. Rules

1.1 A builtin name is not a keyword. `date` and `string` are namespaces, not
reserved words; a local named `date` shadows nothing because builtin
namespaces are only reachable through a call (`date.now()`), and a bare
`date` is `E0301`.

1.2 Every builtin has a fixed arity and a declared type. Calling one with the
wrong arity or types is `E0204`/`E0301`, the same as any other call.

1.3 Builtins that can fail on client-derived input follow types §7.2: they
raise `BadRequest` there and are faults elsewhere.

1.4 A builtin marked **query-only** is legal only inside a query clause
(names §5.3). A builtin marked **route-only** is legal only inside a route,
middleware or `after` block.

---

## 2. Bare — language verbs

| Name | Type | Notes |
|---|---|---|
| `json(v)` | `Response` | 200 |
| `created(v)` | `Response` | 201 |
| `accepted(v)` | `Response` | 202 |
| `noContent()` | `Response` | 204 |
| `redirect(n, url)` | `Response` | sets `Location` |
| `badRequest(v)` | `Response` | 400 |
| `unauthorized(m)` / `forbidden(m)` / `notFound(m)` / `conflict(m)` / `tooManyRequests(m)` | `Response` | `{"error": m}` |
| `internalError()` | `Response` | 500 |
| `statusCode(n, v)` | `Response` | explicit status |
| `content(mime, body)` | `Response` | 200; `body` verbatim as `mime`, routing §6.5 |
| `cookie(name, value, opts)` | `Response` suffix | routing §6.2 |
| `env(k)` | `text?` | process environment; read once at boot |
| `raw(sql, …)` | `Raw[]` | writes §6 |
| `serve(port)` | `Void` | only in `main()`; the port the listener binds (config §3.2.2) |

Coercions — types §7.2 decides their failure class:

| Name | Type |
|---|---|
| `int(x)` | `int` |
| `bigint(x)` | `bigint` |
| `numeric(x)` | `numeric` |
| `boolean(x)` | `boolean` |
| `uuid(x)` | `uuid` |
| `timestamptz(x)` | `timestamptz` — RFC 3339 only |
| `enum(E, x)` | `E?` — `null` in gives `null` out; a non-member raises |

`enum(E, x)` takes an enum **type name** as its first argument, the way
`request.body() as C` takes a class name. It accepts `text?` and returns
`E?`, so `enum(InvoiceStatus, request.query("status"))` is one line and
`?status=bogus` is a 400 rather than a silently dropped filter.

---

## 3. `date.*` — the application clock

| Name | Type |
|---|---|
| `date.now()` | `timestamptz` — UTC, application clock (types §2.4) |
| `date.today()` | `date` |
| `date.days(n)` / `date.hours(n)` / `date.minutes(n)` / `date.seconds(n)` | `interval` |
| `date.add(t, i)` | `timestamptz` — same as `t + i` |
| `date.parse(s)` | `timestamptz?` |
| `date.format(t, fmt)` | `text` — `fmt` is a literal, checked at compile time |

Bare `now()` is `E0302`.

---

## 4. `string.*`

| Name | Type |
|---|---|
| `string.of(v)` | `text` — canonical wire form of any scalar (types §2.1) |
| `string.len(s)` | `int` — characters |
| `string.lower(s)` / `string.upper(s)` / `string.trim(s)` | `text` |
| `string.replace(s, from, to)` | `text` — literal, all occurrences |
| `string.starts_with(s, p)` / `string.ends_with(s, p)` / `string.contains(s, p)` | `boolean` |
| `string.split(s, sep)` | `text[]` |
| `string.split_csv(s)` | `text[]` — comma-separated, trimmed, empties dropped |
| `string.join(xs, sep)` | `text` |
| `string.pad_left(s, n, pad)` / `string.pad_right(s, n, pad)` | `text` |
| `string.slice(s, from, len)` | `text` |
| `string.matches(s, r"…")` | `boolean` |
| `string.strip_prefix(s, p)` | `text` — unchanged when absent |

`string.strip_prefix($header, "Bearer ")` is the correct spelling of the
sample's `string.replace(header, "Bearer ", "")`, which would also have
stripped the literal from the middle of a token.

---

## 5. `array.*` — the lambda replacement (#22)

Field names are passed as **string literals**, checked against the element
type at compile time (`E0301` on a bad name).

| Name | Type |
|---|---|
| `array.len(xs)` | `int` |
| `array.is_empty(xs)` | `boolean` |
| `array.sum(xs, "field")` | `numeric` |
| `array.sum_product(xs, "a", "b")` | `numeric` — Σ aᵢ·bᵢ |
| `array.min(xs, "field")` / `array.max(xs, "field")` | `T?` |
| `array.pluck(xs, "field")` | `T[]` |
| `array.contains(xs, v)` | `boolean` |
| `array.first(xs)` / `array.last(xs)` | `T?` |
| `array.sorted(xs, "field")` | `T[]` — ascending, stable |

All return `numeric` where a sum is involved, which answers the overflow
question of types §12.3 at the builtin rather than at the call site.

Anything these do not cover is written as `for` plus an accumulator.

---

## 6. `hash.*`, `jwt.*`, `crypto.*`

| Name | Type | Notes |
|---|---|---|
| `hash.password(p)` | `text` | Argon2id, salted; output includes params |
| `hash.verify(p, stored)` | `boolean` | constant time |
| `hash.sha256(s)` | `text` | lowercase hex — **deterministic**, for lookup keys |
| `hash.hmac_sha256(payload, secret)` | `text` | lowercase hex |
| `hash.hmac_verify(payload, sig, secret)` | `boolean` | constant time |
| `crypto.token(n)` | `text` | `n` bytes from the CSPRNG, base64url |
| `crypto.constant_time_eq(a, b)` | `boolean` | no early exit on the first differing byte |
| `jwt.sign(claims, secret, ttl_minutes)` | `text` | HS256 |
| `jwt.verify(token, secret)` | `Record{sub: text, exp: bigint, iat: bigint}?` | HS256; null on invalid/expired |
| `jwt.verify_jwks(token, jwks_url)` | `Record{sub: text, exp: bigint, iat: bigint}?` | RS256 against a published key set; null on invalid/expired |

**`jwt.verify_jwks` answers the same record as `jwt.verify`**, so moving
from a shared secret to an identity provider does not change the code that
reads the claims. It selects the key by the token header's `kid`, fetches
`jwks_url` and caches the result.

The `kid` comes from the token header, which is *unauthenticated*. An
implementation that refetched on every unknown `kid` would be a
denial-of-service amplifier pointed at the identity provider, so the fetch
is rate-limited and unknown `kid`s are negatively cached.

A token that does not verify is null. An **unreachable** `jwks_url` is
not — that is the provider being down, and answering null would report an
outage as "every credential is wrong". It raises, and the outbound URL
gate (§7c) applies: a provider on a private network needs
`JWC_HTTP_BLOCK_PRIVATE` left off.

**`hash.sha256` exists to make hashed-token lookup possible (#38).** Three
sample tables declare `token_hash varchar(255) private, unique` and then need
`where token_hash == $h` — which a salted KDF cannot serve, because every
call produces a different string. The rule is: **`hash.password` for secrets
a human chose, `hash.sha256` for high-entropy tokens the server generated.**
A `crypto.token(32)` value has 256 bits of entropy, so an unsalted digest is
not brute-forceable and is the standard construction.

`W1201` warns when a column named `*_hash` is compared with `==` against a
`hash.password` result — that comparison can never match.

---

## 7. `request.*` / `response.*` / `context.*` — route-only

| Name | Type | Notes |
|---|---|---|
| `request.body() as C` | `C` | routing §5.2 |
| `request.raw_body()` | `text` | same buffer |
| `request.header(k)` | `text?` | case-insensitive |
| `request.query(k)` | `text?` | first occurrence |
| `request.query_all(k)` | `text[]` | |
| `request.method()` | `text` | |
| `request.path()` | `text` | the concrete path |
| `request.route()` | `text` | the declared pattern (routing §5.4) |
| `request.peer_ip()` | `inet` | socket address |
| `request.client_ip()` | `inet` | proxy-aware only with `trusted_proxies` |
| `request.id()` | `text` | per-request id, also in every log line |
| `response.status()` | `int` | `after` only |
| `response.duration_ms()` / `response.duration_us()` | `bigint` | `after` only; whole request, middleware included |
| `response.set_header(k, v)` | `Void` | `after` only |
| `response.add_header(k, v)` | `Void` | `after` only |
| `context.<key>` | declared type | middleware §6 |
| `context.<key>?` | `T?` | middleware §6.3 |

---

## 7a. `debug` — development only

| Name | Type | Notes |
|---|---|---|
| `debug.dump(x)` | the type of `x` | writes `x` to stderr and returns it |

The only builtin that accepts a **`Raw`** (types §5.1): the one place a raw
result's shape can be inspected is where the shape is in question. It prints
only under `jwc serve --dev` and is otherwise a no-op that returns its
argument, and a program containing it warns (`W1301`). Full rules:
tooling §3.

---

## 7b. `console.*` — the terminal

| Name | Type |
|---|---|
| `console.write(v)` | `void` — stdout, **no** trailing newline |
| `console.writeln(v)` | `void` — stdout, with one |
| `console.error(v)` | `void` — stderr, no trailing newline |
| `console.read()` | `text?` — one line from stdin, line terminator stripped; `null` at EOF |

Any value goes in, not only text: `console.write(42)` is not an error. A
text value prints as its characters; anything else prints the way
`debug.dump` renders it.

Both write paths **flush**. 0.9 also had `print`, which appended to a
buffer flushed after `main` returned — so a prompt written before a read
appeared after the answer was due, and inside a route body whatever it
printed became the response. `print` is not back, and this family is why.

`console.read()` does not trim: it returns the line as typed, minus the
terminator. `int()` trims before parsing, so `int(console.read())` is safe
on `" 42 "`.

These are legal anywhere, including a route body — a handler that logs to
stderr is ordinary. They are the surface `jwc run` exists for
(tooling.md §2.1), and they work identically under `jwc build`.

---

## 7c. `http.*` — outbound HTTP

| Name | Type |
|---|---|
| `http.get(url)` | `text` — the response body |
| `http.post(url, body)` | `text` |
| `http.json(url)` | `Raw` — the body, spliced like a `jsonb` column |
| `http.status(url)` | `int` |

A **non-2xx is not a raise**. A 404 from a remote service is an answer, and
a language that turns it into a fault makes every caller wrap the call to
find that out. `http.status` is how to ask.

What raises is the request never happening: a refused URL, DNS failing, the
timeout expiring. `BadRequest`, so `catch BadRequest` recovers it — a
remote service being unreachable is not the program being wrong.

The body is `text`, not a shape. What a remote service returns is not
something this compiler can know, and inventing a shape for it turns a
runtime surprise into a type error in the wrong place.

### 7c.1 The SSRF gates

Both run **before** the request is dispatched, so a refused URL never
touches the network.

| | |
|---|---|
| scheme | only `http` and `https`. `file:` is refused by name |
| `JWC_HTTP_ALLOWLIST` | comma-separated hosts. Empty means no restriction |
| `JWC_HTTP_BLOCK_PRIVATE` | resolves the host and refuses loopback, private, link-local and unspecified addresses — including `169.254.169.254`, which is the reason it exists. Off by default, because talking to a sibling container by name is ordinary |
| redirects | **not followed**. A redirect is how an allowlisted host walks you to one that is not |

`JWC_HTTP_TIMEOUT_SECS` bounds the whole request; default 10.

---

## 7d. `json.*`

| Name | Type |
|---|---|
| `json.parse(text)` | `Raw` — raises `BadRequest` when it is not JSON |
| `json.stringify(v)` | `text` |

`parse` answers `Raw`, the same thing a `jsonb` column reads as: it splices
into a response verbatim and is not read field-wise. That is the honest
shape for text whose structure the compiler cannot know — reading a field
off it is `E0310`, and the way to get a typed value is a `class`.

---

## 7e. `file.*` / `directory.*` — the filesystem

| Name | Type |
|---|---|
| `file.read(path)` | `text?` — `null` when it is not there |
| `file.write(path, body)` | `boolean` |
| `file.append(path, body)` | `boolean` |
| `file.exists(path)` | `boolean` |
| `file.size(path)` | `bigint?` |
| `file.delete(path)` | `boolean` |
| `directory.exists(path)` | `boolean` |
| `directory.create(path)` | `boolean` — creates parents |
| `directory.list(path)` | `text[]` — names only, **sorted** |

A missing file reads as `null` rather than raising: "is it there" is what
`file.exists` answers, and making `read` raise puts a `catch` around the
ordinary case. The write paths answer `boolean` for the same reason.

`directory.list` sorts. The order a filesystem hands entries back in is not
stable, and a program that iterates one should not depend on it.

### 7e.1 Only inside a plain `function` (E0230)

These are refused in a `route`, `middleware`, `after`, `errorHandler`,
`service`, `view`, `job` or socket handler.

A route that reads or writes a path derived from the request is one line
from path traversal:

```jwc no-compile
route GET "leak" {
    let secret = file.read(request.query("path") ?? "/etc/passwd");
}
```

0.9 placed no restriction here at all. A script needs files; an HTTP
handler almost never does, and the rare one that does should reach for
something that bounds the path rather than for the raw call.

**What the check is, exactly**: the body being compiled, not a call graph.
A helper `function` reached from both `main` and a route still passes. That
is a smaller hole than the one it closes, and stating it is better than
implying a guarantee the compiler does not make.

---

## 7f. The rest of 0.9's registry

Restored in 0.9.922, after a name-by-name diff of the two registries.

| Name | Type | |
|---|---|---|
| `redis.eval(script, keys_json, args_json)` | `text?` | what `rate_limit` is built on; keys and args are JSON arrays because the language has no varargs |
| `redis.exists(key)` | `boolean` | |
| `redis.ping()` | `boolean` | |
| `unix_timestamp()` | `bigint` | the integer clock. `date.now()` is the application one and answers `timestamptz` |
| `random_int(lo, hi)` | `int` | inclusive low, exclusive high, so `random_int(0, len)` is an index. **Not a secret** — `crypto.token(n)` is |
| `sleep_ms(n)` | `void` | plain `function` only (§7e.1). A handler that sleeps holds a connection open to do nothing |
| `array.take(xs, n)` | same as `xs` | |
| `array.push(xs, v)` | same as `xs` | answers a **new** array. A JWC value is not a reference, and a `push` that appeared to mutate one would be the only place in the language where it did |
| `array.range(lo, hi)` | `int[]` | |

---

## 8. Package namespaces

`import redis;` makes `redis.*` resolvable. A package's exported surface is
declared by the package (ROADMAP v0.28.0). The redis package provides:

| Name | Type |
|---|---|
| `redis.get(k)` / `redis.set(k, v, ttl)` / `redis.del(k)` | `text?` / `boolean` / `int` |
| `redis.incr(k)` / `redis.expire(k, ttl)` | `bigint` / `boolean` |
| `redis.rate_limit(key, limit, window_secs)` | `boolean` — atomic |
| `redis.enabled()` | `boolean` |

`redis.enabled()` answers whether this process is actually talking to a
server: the binary was built with the driver *and* `JWC_REDIS_URL` is set.
It is the only name in the table that answers without one.

Every other name **raises** when there is no server. That is deliberate and
it is mostly about `rate_limit`: a limiter that reads "no Redis" as
"allowed" admits every request, and nothing in the response says so. Branch
on `redis.enabled()` where the call is genuinely optional, and let it raise
where it is not.

`rate_limit(key, limit, window)` is `INCR` plus `EXPIRE` in one script, so
the count and its deadline cannot come apart — the two-call form leaves a
counter with no TTL if the process dies between them, and that key is
blocked for good. The window is fixed: the TTL is set by the request that
creates the key and not pushed forward by later ones.

`cache.*` is the process-local store:

| Name | Type |
|---|---|
| `cache.get(k)` / `cache.set(k, v, ttl)` / `cache.del(k)` | `text?` / `boolean` / `int` |
| `cache.clear()` | `void` |

Deliberately the same four shapes as their `redis.*` counterparts, so
moving a call from one to the other is a rename. What is **not** the same
is the scope: this store lives in one process, so two replicas do not share
it and a restart empties it. It is right for what a single process can own
— a parsed JWKS document, a config row read on every request. It is wrong
for anything whose correctness spans replicas, and a rate limiter is the
standard mistake: per-process counters make the real limit `limit ×
replicas`, and nothing in the response says so. `redis.rate_limit` is for
that.

Entries are capped by `JWC_CACHE_MAX_ENTRIES` (default 10 000). At the cap
a write sweeps what has expired and then evicts the oldest write; the
evictions are counted in `/metrics` as `jwc_cache_evicted_total`, because a
cache that has quietly become a no-op looks exactly like one that works.

`import mail;` makes `mail.*` resolvable:

| Name | Type |
|---|---|
| `mail.send(to, subject, body_html)` | `void` |
| `mail.enabled()` | `boolean` |

The relay is `JWC_SMTP_HOST` / `_PORT` / `_USER` / `_PASSWORD` / `_FROM` /
`_TLS`, and `mail.enabled()` answers whether the four required ones are
set. The rule is the one `redis.*` follows and for the same reason:
`mail.send` **raises** when no relay is configured. It used to answer
`null` — a password-reset route typechecked, ran, returned 200 and
delivered nothing.

---

## 9. Query-only

The SQL aggregates: `count(x)`, `count.distinct(x)`, `sum(x)`, `min(x)`,
`max(x)`, `avg(x)`, each optionally with an aggregate filter
(`count(x where pred)`, queries §6.3). Legal only inside a projection of a
grouped query (`E0530`).

---

## 10. Deliberately absent

| Not a builtin | Why, and what to write instead |
|---|---|
| `now()` | two clocks; write `date.now()` or `default now()` (types §2.4) |
| `send_email` | I/O with a provider shape the language does not model. The package is §8's `mail.send(to, subject, body)`. `DEFERRED-10` |
| `log_insert` | not a built-in, but the capability is not absent: it is `insert into … buffered` (writes §7). This row used to read "overlapped `insert into` for no benefit" — the benefit is the round trip an `after` block otherwise puts in front of every response, and it is measurable |
| `random_token` | ambiguous strength. `crypto.token(n)` |
| `verify_signature` | ambiguous algorithm. `hash.hmac_verify(payload, sig, secret)` |
| `days(n)` | `date.days(n)` |
| `next_invoice_number` | application logic, not a language feature. The sample shows a counter table |
| `seed.*` | test fixtures; the isolation model is `DEFERRED-11` (ROADMAP v0.28.0) |
| SSE | `DEFERRED-19`. Use a `socket` (routing §9) or long-polling. 0.9 parsed `route SSE "…"` end to end and dispatched it to a stub, so a program could declare one, pass every check and serve nothing. This row used to also list `dispatch`, the job queue and WebSocket; all three are declarations now (jobs.md, routing §9) |

---

## 11. Reference generation

There is no generator and no inventory file today. This file is the
specification, and `docs/docs/stdlib/builtins.md` is written by hand
against it.

An earlier version of this section described `docs/docs/reference/builtins.md`
as generated and checked by `tests/builtins_doc_sync.rs`. Neither exists — a
section about keeping the documentation honest that was itself wrong.

---

## 12. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0205` | wrong number of arguments to a builtin |
| `E0206` | a field name in `json.get`-style access is not a string literal |
| `E0230` | `file.*` / `directory.*` outside a plain `function` (§7e.1) |
| `W1301` | `debug.dump` in the program (tooling §3.4) |

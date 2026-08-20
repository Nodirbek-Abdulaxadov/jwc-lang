# routing.md — routes, path parameters, responses

Normative. Closes gaps **#9**, **#10**, **#12**, **#16**, **#20**, and the
routing half of **#37**.

---

## 1. Shape

```jwc
routes "/api/v1/orgs/{org_id: bigint}" use RequireAuth, RequireOrgMember, Audit {

    route GET "" {
        let org = OrgService.detail(@org_id);
        return json($org);
    }

    route PATCH "" use RequireOrgAdmin {
        let req = request.body() as OrgEdit;
        let org = OrgService.update(@org_id, $req);
        return json($org);
    }
}
```

1.1 A path is written in **exactly two pieces**: the `routes` prefix and the
`route` suffix. There is no third. `routes` blocks do not nest
(ROADMAP §8).

1.2 The resolved path is `prefix + "/" + suffix`, with an empty suffix
meaning the prefix itself, and duplicate `/` collapsed. Both pieces are
literal; no rewriting, no interpolation.

1.3 A route body is a block. By convention it is 3–4 lines: read input, call
a service, return. Nothing enforces that, but everything else in the language
is arranged so that longer bodies are a signal.

---

## 2. Methods

`GET POST PUT PATCH DELETE HEAD OPTIONS`. `HEAD` is answered automatically
for every `GET` unless declared explicitly. `OPTIONS` is answered by the CORS
layer unless declared explicitly (config §3.4).

---

## 3. Path parameters (#9, #20)

### 3.1 Declaration

A path segment of the form `{name}` or `{name: T}` declares a parameter.
`T` is a scalar type (types §2.1); the default when omitted is `text`.

```
routes "/api/v1/orgs/{org_id: bigint}/invoices"
route  GET "{id: bigint}"
```

An untyped `{name}` compared against a non-`text` column is `E0376`, not a
warning: this language does not coerce, so the comparison has no meaning
rather than a slow one. The error names the missing word — `{org_id: bigint}`
— because "text and bigint cannot be compared" describes the symptom and
not the fix.

*(`W0601` was specified here for a design where the comparison was legal
and merely slow. It is not, so the warning would only ever appear beside
the error that already rejects the program.)*

### 3.2 Parsing happens before middleware

The router parses every declared parameter **before any middleware runs**.
A value that does not parse produces:

```
400 {"error":"bad_path_parameter","parameter":"org_id","expected":"bigint"}
```

This is the fix for #20: malformed input reached Postgres and became a 500.
It is also why a middleware binder (`middleware M(@org_id: bigint)`) can be
typed at all — by the time middleware runs, the value is already a `bigint`.

### 3.3 Scope

`@name` resolves to a parameter declared in the enclosing `routes` prefix or
the `route` suffix (names §5.2). The union of the two is the binder set for
that route.

### 3.4 One slot, one name, one type

If two `routes` blocks declare a parameter in the same positional slot of the
same path shape, the name and type must match (`E0701`). Otherwise
`/orgs/{org_id}` and `/orgs/{slug: text}` would make `RequireOrgMember`
mean two different things depending on which block invoked it.

---

## 4. Route conflicts (#12)

### 4.1 Duplicates are a hard error

Two routes with the same `(method, resolved path)` is `E0710`, naming both
declaration sites. Last-wins does not exist. Registration order is a file
ordering, and file ordering is not a language feature.

### 4.2 Precedence

A literal segment beats a parameter segment at the same position:
`/orgs/settings` wins over `/orgs/{org_id}` for the request `/orgs/settings`.

This is fixed precedence, not registration order — but it is also a trap, so:

### 4.3 Total shadowing is an error

A route that can never match because another route shadows it entirely is
`E0711`. `/orgs/{a}` declared after `/orgs/{b}` is `E0710` (same shape).

**`E0711` is unreachable in 1.0 and is not implemented.** Two things close
it: there is no wildcard segment, so a route cannot swallow a longer path;
and the router picks the candidate with the **most literal segments**
rather than the first declared, so `/orgs/new` beats `/orgs/{id}` whichever
order they appear in. A shadowing check would be dead code, and dead checks
are read as coverage.

The specificity rule is what makes that true, so it is pinned by a test
rather than left as a property of the current implementation. `E0711` stays
reserved: a wildcard segment would bring it back.

### 4.4 Trailing slash

`/x` and `/x/` are the same route. The router redirects `/x/` to `/x` with
308 unless `server { strict_slash = false }` (config §3.2), in which case
both serve.

---

## 5. Request input

### 5.1 One buffer (#16)

The request body is read **once**, into a bounded buffer, before middleware
runs. `request.raw_body()` and `request.body() as C` are two views of that
same buffer; both may be called, in either order, any number of times, and
they always agree.

A body larger than `server { max_body_bytes }` (default 1 MiB) is answered
`413` **before** middleware. A webhook signature check therefore never sees a
truncated body.

### 5.2 `request.body() as C`

Parses the buffer as JSON and validates against class `C` (types §11).
Failure is a `BadRequest` with the fixed `validation_failed` body. There is
no unvalidated body accessor other than `raw_body()`.

`request.body()` without `as C` is `E0720` — it would produce a value with
no declared shape, which spread rejects anyway (types §9.1).

### 5.3 Query and headers

`request.query(k)` and `request.header(k)` return `text?`. Header lookup is
case-insensitive. Repeated query keys: `request.query(k)` returns the first;
`request.query_all(k)` returns `text[]`.

### 5.4 Client address (#15)

| Call | Meaning |
|---|---|
| `request.peer_ip()` | the socket peer address. Always. `inet` |
| `request.client_ip()` | the originating client address. `inet` |
| `request.route()` | the **declared** path pattern, e.g. `/api/v1/orgs/{org_id}` |

`client_ip()` returns the peer address **unless** `server { trusted_proxies }`
is declared, in which case it walks `X-Forwarded-For` from the right, past
addresses inside the trusted set, and returns the first outside it. With no
`trusted_proxies` declared, `X-Forwarded-For` is **ignored entirely** — a
rate limiter keyed on `client_ip()` is then unspoofable by default, and
becomes proxy-aware only when an operator says which proxies to trust.

`request.route()` exists so rate-limit keys have bounded cardinality;
`request.path()` on a parameterised route gives each id its own bucket, which
is a self-DoS. `W0602` warns on `request.path()` in a rate-limit key.

---

## 6. Responses

### 6.1 Builders

| Call | Status | Body |
|---|---|---|
| `json(v)` | 200 | `v` |
| `created(v)` | 201 | `v` |
| `accepted(v)` | 202 | `v` |
| `noContent()` | 204 | none |
| `redirect(n, url)` | `n` (301/302/303/307/308) | none, `Location` set |
| `badRequest(v)` | 400 | `v` |
| `unauthorized(m)` | 401 | `{"error": m}` |
| `forbidden(m)` | 403 | `{"error": m}` |
| `notFound(m)` | 404 | `{"error": m}` |
| `conflict(m)` | 409 | `{"error": m}` |
| `tooManyRequests(m)` | 429 | `{"error": m}` |
| `internalError()` | 500 | `{"error":"internal_error"}` |
| `statusCode(n, v)` | `n` | `v` |

A builder taking a message always produces `{"error": <message>}` with
`application/json`. There is no bare-string response body; the content type
and the shape are the same on every path.

### 6.2 `with { }` headers (#10)

```jwc
return created(json($invoice)) with { "Location": $url, "X-Request-Id": $rid };
```

`with { }` is a suffix on any response expression. Keys are literal strings;
values are `text`. A key given twice in one `with` is `E0730`.

Repeated headers (`Set-Cookie`) use a separate call, because a JSON object
cannot carry a duplicate key:

```jwc
return json($x) with { "Cache-Control": "no-store" } cookie("sid", $sid, { http_only: true, max_age: 3600 });
```

`cookie(name, value, opts)` may be chained; each occurrence appends one
`Set-Cookie`.

### 6.3 Headers from `after`

`response.set_header(k, v)`, `response.add_header(k, v)` and
`response.status()` are legal only inside an `after` block
(middleware §5).

### 6.4 What a route may return

A route body must end every path in `return <Response>` (`E0731`).
Returning a non-`Response` from a route is `E0732` — `return $account;` is
the mistake this catches, and the fix is `return json($account);`.

---

## 7. Content negotiation

7.1 Every JSON response is `application/json; charset=utf-8`.

7.2 A request body that is not `application/json` where a body is read is
`415`. A body read on a method that carries none (`GET`, `HEAD`, `DELETE`
with no body) yields an empty buffer and fails validation with `400`, not
`500`.

---

## 8. Route registration and startup

8.1 All routes are known at compile time. There is no dynamic registration.

8.2 `jwc routes` prints the resolved table: method, path, middleware chain in
execution order, and the source location of each. This is the artefact
against which `E0710`/`E0711` are read.

---

## 9. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0701` | path parameter slot disagrees on name or type |
| `E0710` | duplicate `(method, path)` |
| `E0711` | route is fully shadowed |
| `E0720` | `request.body()` without `as C` |
| `E0730` | duplicate key in `with { }` |
| `E0731` | route path does not end in a response |
| `E0732` | route returns a non-`Response` |
| `E0733` | a header value is not text |
| `E0734` | `response.status()` outside an `after` block |
| `E0900` | removed keyword (§11 below) |
| `W0602` | `request.path()` in a rate-limit key |

---

## 10. `E0900` — the removed vocabulary

Encountering a pre-1.0 keyword produces a dedicated diagnostic naming its
replacement. There is no migration path and no compatibility flag; the old
language had no users (ROADMAP §0).

| Old | Message |
|---|---|
| `entity` | `'entity' was removed in 1.0 — write 'table Accounts of App.auth { … }'` |
| `dbcontext` | `'dbcontext' was removed in 1.0 — write 'database App : Postgres' + 'schema auth of App;'` |
| `dbset` | `'dbset' was removed in 1.0 — a table is declared with 'table T of App.s { … }'` |
| `via` | `'via' was removed in 1.0 — write the join's 'on' clause` |
| `nav` | `'nav' was removed in 1.0 — joins are written in the query, never declared on the table` |
| `validate` | `'validate body' was removed in 1.0 — write 'request.body() as ClassName'` |
| `new` | `'new X from Y' was removed in 1.0 — write 'insert into App.s.X { ...y }'` |
| `patch` | `'patch' was removed in 1.0 — write 'update App.s.X set …'` |
| `mount` | `'mount' was removed in 1.0 — every route declares its full path` |
| `dome` | `'dome' was removed in 1.0` |

Two words from the old vocabulary are **not** on this list, because they are
live 1.0 keywords with different meanings and a dedicated diagnostic would
fire on correct code:

- **`with`** — was a query clause (`select … with Category`); in 1.0 it is
  the response-header suffix (§6.2).
- **`group`** — was a route grouping keyword; in 1.0 it is `group by`
  (queries §1).

Both produce an ordinary parse error at the position where the old syntax
would have continued. That is the honest trade: `E0900` is for words that
can only ever be the old language.

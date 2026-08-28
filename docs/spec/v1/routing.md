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
| `content(mime, body)` | 200 | `body`, verbatim, as `mime` (§6.5) |
| `text(body)` | 200 | `body`, verbatim, as `text/plain; charset=utf-8` |
| `html(body)` | 200 | `body`, verbatim, as `text/html; charset=utf-8` |

A builder taking a message always produces `{"error": <message>}` with
`application/json`. Every builder in this table except `content` JSON-encodes
its payload, so the content type and the shape are the same on every path
that carries data.

### 6.2 `with { }` headers (#10)

```jwc
return created(json($invoice)) with { "Location": $url, "X-Request-Id": $rid };
```

`with { }` is a suffix on any response expression. Keys are literal strings;
values are `text`. A key given twice in one `with` is `E0730`.

A key that the builder already set — `Content-Type` on any JSON response,
`Location` on a `redirect` — is **replaced**, matched case-insensitively.
Appending instead would send the header twice, which is a malformed message
(RFC 9110 §8.3) and is resolved differently by different clients.

`Set-Cookie` is the exception: `with { }` **appends** on that one name
rather than replacing, because it is the header HTTP expects to repeat,
so replacing on it would delete a cookie instead of overriding a value.
Every other header replaces.

`__Host-` and `__Secure-` cookie names are checked against the rule the
prefix promises (`E0746`): both need `secure: true`, and `__Host-` also
needs `path: "/"` and no `domain`. A browser refuses a cookie that breaks
the rule and says nothing, which is the same silent failure `E0739` exists
for.

A cookie is set with `cookie(...)` rather than through `with { }`, because
a JSON object cannot carry a duplicate key:

```jwc
return json($x) with { "Cache-Control": "no-store" } cookie("sid", $sid, { http_only: true, max_age: 3600 });
```

`cookie(name, value, opts)` may be chained; each occurrence appends one
`Set-Cookie`. `opts` is optional.

#### The attributes

| Key | Default | |
|---|---|---|
| `http_only` | **`true`** | the cookie is not readable from a script |
| `secure` | `false` | sent over HTTPS only |
| `same_site` | **`"Lax"`** | `Strict`, `Lax` or `None` |
| `max_age` | — | seconds; absent is a session cookie |
| `path` | `"/"` | |
| `domain` | — | absent is host-only, which is the narrower scope |

`http_only` and `same_site` default to the strict values, and an author who
needs a cookie a script can read writes `http_only: false`. A default that
is wrong is a defect in every program that did not think about it; an
opt-out that is wrong is a defect in the one program that asked.

`expires` is deliberately absent: `Max-Age` says the same thing in seconds,
and an absolute date has one exact spelling (RFC 7231 §7.1.1.1) that a
caller has no way to produce from JWC.

An unknown key is `E0737` — a misspelled `httponly` must not be a cookie
that quietly lost its `HttpOnly`. A `same_site` that is not one of the three
is `E0738`. `same_site: "None"` without `secure: true` is `E0739`: every
current browser refuses to store that cookie, and says nothing, so the
failure has no layer that would otherwise report it.

`same_site: "None"` implies `Secure` in the emitted header even when
`secure` was not written, because the pair is what the browser requires.

#### What cannot be sent

A cookie name is a `token` and a value is `cookie-octet*` (RFC 6265
§4.1.1). A name or value carrying a space, a comma, a semicolon, a quote, a
backslash or a control character is a **fault** — 500, with a sentence in
the log naming the cookie — rather than a header that splits the response.
Nothing is encoded on the author's behalf: a value silently percent-encoded
here is one the reader has to guess the encoding of.

### 6.4 `redirect` and `redirectExternal`

```jwc no-compile
return redirect(302, "/dashboard");                 -- a path on this service
return redirectExternal(302, $link.url);            -- anywhere
```

`redirect` sends a caller to a path on **this** service. A target that can
leave — a scheme, an authority, a protocol-relative `//host` or `/\host`,
or anything not rooted at `/` — is refused. A literal one is `E0745`; a
value discovered at run time is a **fault**, 500 with the target named in
the log.

`redirectExternal` is the same builder without the restriction.

Two builders because the language cannot tell the two cases apart and the
author can. An open redirect is the primitive behind a phishing link that
starts on your domain, and behind stealing an OAuth code from a
`redirect_uri`. It is also, for a URL shortener, the entire product. What
the split buys is that "where can this service send someone off-site" is a
question `grep` answers in one line, which reading every `redirect(` could
not settle.

The classification is syntactic and does not need to know this service's
own host — behind a proxy it does not reliably have one. `//evil.example`,
`/\evil.example` and `\\evil.example` are all external: browsers
normalise the backslash, and a check that only looked for `http:` lets
every one of them through. A target carrying a control character or a
newline is refused by **both** builders — it cannot be a header value.

### 6.3 Headers from `after`

`response.set_header(k, v)`, `response.add_header(k, v)` and
`response.status()` are legal only inside an `after` block
(middleware §5).

### 6.5 `content(mime, body)`, `text(body)`, `html(body)` — the non-JSON body

```jwc
namespace pages;

function landing_page() -> text {
    return "<!doctype html><title>1kb.uz</title>";
}

routes "/" {
    route GET "" {
        return content("text/html", landing_page());
    }
}
```

6.5.1 `body` is `text` and is sent **verbatim** — not JSON-encoded. A body
of any other type is `E0736`; build the string first.

6.5.2 `mime` is a **string literal**, so the framing of a response can never
depend on a runtime value and `jwc openapi` can name the media type. A
non-literal is `E0735`.

6.5.3 A `text/*` media type with no `charset` gets `; charset=utf-8`
appended. Everything else is sent exactly as written.

6.5.4 `content` produces 200. Other statuses compose, because a response is
a value (§6.1): `statusCode(404, content("text/html", not_found_page()))`.

6.5.5 This is the only builder that does not answer `application/json`, and
it exists for the endpoints a browser or a crawler reads directly — a
landing page, `robots.txt`, `sitemap.xml`, an SVG card. An API payload is
`json`, and `content` is not a way to hand-roll one: the compiler checks
nothing about the bytes inside the string.

### 6.4 What a route may return

A route body must end every path in `return <Response>` (`E0731`).
Returning a non-`Response` from a route is `E0732` — `return $account;` is
the mistake this catches, and the fix is `return json($account);`.

---

## 7. Content negotiation

7.1 Every JSON response is `application/json; charset=utf-8`. A
`content(mime, body)` response (§6.5) carries the media type it declared,
and is the only exception.

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

## 9. Sockets

### 9.1 Declaration

A `socket` is a member of a `routes` block, beside its HTTP siblings. It
shares the prefix and the `use` chain.

```jwc no-compile
routes "/live" use RequireAuth {
    socket "rooms/{room: text}" use RequireMember {
        on open {
            socket.send("joined " + @room);
        }

        on message (text) {
            socket.send("echo: " + $text);
        }

        on close {
            -- released here, whatever ended the connection
        }
    }
}
```

The three blocks are optional and each may appear at most once (`E0012`).
A `socket` with none of them is `E0013`: the endpoint would accept the
upgrade and then do nothing.

Three blocks rather than one body looping on a receive call: the
language has no unbounded loop — `for` over a collection is the only
iteration — and adding `while` to serve sockets would be a worse trade
than naming what a socket handler is, which is three moments in a
connection's life. The runtime owns the loop, which also removes the
failure mode a hand-written one has, where forgetting to break holds a
task for the process's lifetime.

### 9.2 Semantics

| | |
|---|---|
| Method | the upgrade is a `GET`, so `route GET` on the same path is a duplicate (`E0710`) |
| Middleware | runs **before** the upgrade, on the HTTP request |
| A middleware that answers | the client gets that response; no upgrade happens |
| `@param` | as on any route — a socket has a path |
| `context.*` | set by the chain, readable in all three handlers, and it persists for the connection |
| `request.route()` | the declared pattern, so §5.4's bounded cardinality still holds |
| Locals | do not persist between handlers; `context` is what does |
| `on message (m)` | `m : text` |
| A binary frame | closes the connection — there is no text to bind, and `from_utf8_lossy` would hand the handler a string the peer never sent |
| No `on message` | a text frame is dropped; a peer that speaks first is not an error |
| A raise in a handler | ends that handler and closes the connection — there is no response to put an error in |
| `after` blocks, on a successful upgrade | do not run: an `after` block observes a response, and the response was the 101 |
| `after` blocks, when the chain answers | run, in reverse order, for every middleware that started — the response is an ordinary one (middleware §4.3), and a rejected upgrade is exactly what an access log is for |
| A plain `GET` at a socket path | `400`, not `404` — the path exists, the request is wrong |

Middleware running before the upgrade is the whole value of `use` on a
socket: a rejected client reads a `401`, rather than getting a `101`
followed by an immediate close it has to guess about.

### 9.3 `socket.*`

`socket.send(text)` and `socket.close()`, and only inside one of the three
handlers (`E0225`).

Both **queue**. The connection task writes what a handler produced once
that handler returns, which is why `socket.close()` followed by
`socket.send(...)` drops the send: the close came first. A handler that
panics therefore cannot leave a half-written frame on the wire either.

### 9.4 Message size

A socket message and a single frame are both capped at
`server { max_body_bytes }` (config §3.1). Over the cap the connection
closes; the handler does not run.

The cap is the body cap because a frame is a thing a peer sends, and
`max_body_bytes` is the knob that says how large that may be. An upgrade
carrying no limit of its own would leave the real ceiling at the WebSocket
library's 64 MiB default whatever the config said — measured against a
server configured for 1024 bytes, a **5,000,000** byte text frame goes
through, about 5000x the number in the file, and the author who set the
knob has not bought what it says.

The cap is per connection, so N peers still cost N x the cap — this is a
bound on one message, not a memory budget. `0`, the escape hatch for a
deployment behind a proxy that enforces its own size, leaves the library
default in place here too.

### 9.5 How many connections

`server { max_sockets }` bounds how many WebSocket connections may be open
at once. Past it the upgrade is **503**, answered before the handshake, so
the descriptor is never spent and the client gets a status it can read
rather than a 101 followed by a close.

The default is half the process's own descriptor limit, clamped to
[64, 4096], rather than a fixed number. A fixed number is wrong at both
ends: 1024 is the common Linux soft limit, so defaulting to 1024 would let
sockets take every descriptor the process has — the exact failure this cap
exists to stop — while on a host tuned to 65536 the same number is
needlessly small.

`max_sockets = 0` means **no limit**, the same thing `0` means for
`max_body_bytes` — the escape hatch for a deployment whose load balancer
already bounds connections. It restores exactly the behaviour described
below, so set it deliberately.

Why the cap exists, measured on a server whose descriptor limit was 200:
without one, an attacker opens **190** connections, sends nothing on any
of them, and every ordinary HTTP request then fails to connect at all.
`/healthz` and `/readyz` are HTTP too, so an orchestrator sees a dead pod
and restarts it — handing the attacker a fresh one to refill. Each
connection costs about 14.7 kB and exactly one descriptor, and nothing
closes them.

With the cap, the same 190 attempts fill it, the rest get 503, and HTTP
keeps answering 200.

What the cap does **not** do: it does not reclaim a connection that is
open but dead. `socket.recv()` waits with no timeout, so a peer that has
gone away silently holds its slot until the TCP connection breaks. A
server-initiated ping with a pong deadline is what distinguishes "quiet"
from "gone", and 1.0 does not send one — an attacker who holds slots open
still denies *sockets* to everyone else, and the cap's guarantee is only
that **HTTP survives it**. Set `max_sockets` with that in mind.

### 9.6 What is not here

`OpenAPI` cannot describe a WebSocket, so `jwc openapi` lists sockets
under `x-jwc-sockets` rather than emitting the upgrade as a `GET` that
answers 200 — a lie a client generator would act on.

Server-Sent Events are `DEFERRED-17`: absent rather than
half-implemented. A transport a program can declare and pass every check
against, and that then serves nothing, is worse than one that is simply
not there.

## 10. Static assets

### 10.1 Declaration

```jwc
static "/assets" from "public";
static "/" from "dist" cache 31536000;
```

A **mount**, not a route: no body, no `use` chain, no path parameters. The
prefix is a literal beginning with `/`; a `{slot}`, a `?` or a `#` in it is
`E0740`. `"/assets/"` and `"/assets"` are the same mount.

`from` names a directory **relative to the project**. A mount is not a
handler and takes no middleware — a tree of files has no `context` to
populate and nothing to authorise per file. Anything that needs a decision
per request is a `route`, which can read the file itself.

### 10.2 Precedence

A request is answered by the first of these that matches:

1. a declared `route` or `socket` **whose segments are all literal**;
2. `/healthz`, `/readyz`, `/metrics` (config.md §4.0.2);
3. a `static` mount, in source order;
4. a declared `route` or `socket` that bound a path parameter;
5. 404.

A mount is therefore never able to take a declared path away, and a mount
at `"/"` cannot capture the operational paths — a file that happens to be
named `healthz` does not answer the probe. Two mounts on one prefix is
`E0742`: which directory answered would otherwise depend on the order the
files happened to load in.

Rows 3 and 4 are the same rule as §4.2, extended to mounts: §4.2 ranks
candidates by **literal segments**, a file under a mount is all-literal and
a `{slot}` route has none, so the mount is the more specific candidate.
Putting every route ahead of every mount instead would let a `/{code}`
catch-all take `/robots.txt` and `/favicon.ico` away from the mount sitting
next to `index.html`, and answer 404 in the shape of "no such link" — a
wrong answer rather than a missing one, told to a crawler asking for
`/robots.txt`.

The mount only wins when it **has** the file: a miss under the mount falls
through to the parameterised route, so `/abc123` still reaches `/{code}`.
A route with no parameters is unaffected and stays ahead of a file of the
same name (row 1), which is what keeps `/docs` a route even when
`public/docs` exists.

### 10.3 What a mount will not serve

The URL under the prefix is split on `/`, and each segment is
percent-decoded on its own. A segment is **refused** — never repaired —
when it is `..`, when it begins with `.`, when it decodes to something
containing `/`, `\`, `:` or a NUL, or when its escape is not `%` followed
by two hex digits. A refusal is a 404: that the traversal was *understood*
is more than the caller needs to know.

Nothing is normalised. Normalising `a/../b` to `b` is a repair, and a
repair has to be exactly as clever as the caller — it has to agree with the
operating system about every encoding, separator and case fold, or the path
that was checked and the path that is opened are different strings.

What survives is joined to the root and **canonicalised**, and the result
must still be under the canonical root: a symlink leaving the tree is
caught even though every segment of the URL was an ordinary name.

The root itself must exist, be a directory, and be inside the project
(`E0741`, `E0744`) — checked when the program is checked, not at the first
request that misses.

A path that resolves to a directory answers that directory's `index.html`,
and nothing else. There is no listing.

### 10.4 What a mount sends

| Header | Value |
|---|---|
| `Content-Type` | by extension; an unknown one is `application/octet-stream`, never a guess |
| `ETag` | the sha256 of the bytes, quoted |
| `Cache-Control` | `public, max-age=<cache>`, or `public, max-age=0, must-revalidate` without one |
| `X-Content-Type-Options` | `nosniff` |

`cache <n>` is a whole number of seconds, at most 31536000 (a year);
anything else is `E0743`.

An `If-None-Match` naming the ETag — exactly, weakly, or as `*` — is a 304
with the same headers and no body. `HEAD` is those headers plus the
`Content-Length` the `GET` would have sent. Any other method on a path the
mount covers is **405** with `Allow: GET, HEAD`, not a 404: the path exists
and the request is wrong.

### 10.5 The filesystem is still out of reach

`E0230` stands: a route may not read a path the caller chose. A mount is
not that. Its root is written in the source and fixed when the program is
compiled, and the only thing the caller supplies is a name inside it that
§10.3 has already refused unless it is an ordinary file name.

### 10.6 One implementation, two backends

`jwc serve` reads the directory per request, so an edit shows on the next
refresh. `jwc build` walks the tree at compile time, copies it into the
crate it generates and `include_bytes!`s it: the binary carries its assets
and needs no directory beside it. The build applies §10.3's rules to the
walk as well, so a dotfile or an escaping symlink is not merely unreachable
in the artifact — it is not in it.

Every *decision* in §10.3 and §10.4 is one file, `src/assets_core.rs.in`,
which the interpreter includes and codegen pastes verbatim into the
generated crate. The two backends do not implement this section twice; they
run the same text.

## 11. Diagnostics introduced here

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
| `E0735` | `content(...)` media type is not a string literal |
| `E0736` | `content(...)` body is not `text` |
| `E0737` | unknown key in a `cookie(...)` options record |
| `E0738` | a cookie attribute of the wrong type, or a `same_site` that is not `Strict` / `Lax` / `None` |
| `E0739` | `same_site: "None"` without `secure: true` |
| `E0745` | `redirect` given a literal target that leaves this service |
| `E0740` | a `static` prefix is not a literal path beginning with `/` |
| `E0741` | a `static` root is missing, or is not a directory |
| `E0746` | a `__Host-` / `__Secure-` cookie whose attributes break the prefix's rule |
| `E0742` | two `static` mounts on one prefix |
| `E0743` | a `static` `cache` value is not a number of seconds within the ceiling |
| `E0744` | a `static` root is outside the project |
| `E0019` | a `socket` member that is not `on open` / `on message (m)` / `on close` |
| `E0020` | the same `on` handler declared twice |
| `E0021` | a `socket` with no handlers at all |
| `E0225` | `socket.*` outside a socket handler |
| `E0814` | `return <value>` inside a socket handler |
| `E0900` | removed keyword (§11 below) |
| `W0602` | `request.path()` in a rate-limit key |

---

## 12. `E0900` — the removed vocabulary

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

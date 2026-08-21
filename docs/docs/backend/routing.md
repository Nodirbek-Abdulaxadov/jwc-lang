---
sidebar_position: 1
title: "Routing"
description: "Routes, typed path parameters, responses. What the router matches, and why a malformed parameter is a 400 before any middleware runs."
---

# Routing

```jwc
namespace routes.notes;

routes "/api/notes" {
    route GET "" {
        return json({ ok: true });
    }

    route GET "{id: bigint}" {
        return json({ id: @id });
    }
}

function main() {
    serve(8080);
}
```

A `routes` block declares a prefix; a `route` inside it declares a method
and a suffix. Blocks do not nest.

## Path parameters are typed

```jwc no-compile
route GET "{id: bigint}" { … }
```

`@id` is the parameter, and it arrives as a `bigint` — not as text that
the query layer will try to cast. A segment that does not parse as its
declared type is a **400 before any middleware runs**:

```json
{"error":"bad_path_parameter","parameter":"id","expected":"bigint"}
```

which is the honest answer, and keeps malformed input out of Postgres
where it would have been a 500.

`@name` is a path parameter and `$name` is a local. They are different
sigils because they come from different places, and one of them is
client-controlled.

## Which route wins

The route with the most **literal** segments. `/notes/{code}` and
`/notes/docs` both match `/notes/docs`; `docs` is a literal, so it wins.
Registration order does not enter into it.

Two routes with the same shape — same literals in the same places,
parameters wherever the other has one — are a duplicate, and a hard error
at startup rather than a coin flip at run time.

## Responses

| Builder | Status |
|---|---|
| `json(v)` | 200 |
| `created(v)` | 201 |
| `accepted(v)` | 202 |
| `noContent()` | 204, no body and no content-type |
| `badRequest(v)` | 400 — takes a **value**, which becomes the body |
| `unauthorized(msg)` | 401 |
| `forbidden(msg)` | 403 |
| `notFound(msg)` | 404 |
| `conflict(msg)` | 409 |
| `tooManyRequests(msg)` | 429 |
| `internalError()` | 500, one fixed message |
| `statusCode(n, v)` | any |
| `redirect(n, url)` | any 3xx, with `Location` |
| `content(mime, body)` | 200, body verbatim |

A builder applied to something that is **already** a response replaces its
status and keeps its body — so `created(json($row))` is 201 with that
body, not 201 wrapping a response object.

`content` is the one that does not JSON-encode. An HTML page through
`json()` reaches the browser as a quoted string.

```jwc no-compile
return content("text/html", $page);
```

## Headers

```jwc no-compile
return created(json($w)) with { "Location": "/wallets/" + string.of($w.id) };
```

`with { … }` **replaces** a header of the same name rather than appending
one. A builder has already stamped `content-type`, and two of them is a
malformed message that clients resolve inconsistently — so
`with { "Content-Type": … }` has to win, or it does nothing an author can
rely on.

`cookie(name, value)` is the append form, because `Set-Cookie` legitimately
repeats.

## Request input

| | |
|---|---|
| `request.body() as <Class>` | the validated body. The cast is what validates — see [Validation](./validation.md). |
| `request.header(name)` | `text?` |
| `request.query(name)` | `text?` |
| `request.query_all(name)` | every value, in order |
| `request.method()`, `request.path()` | as sent |
| `request.route()` | the **declared** pattern, `/orgs/{org_id}` |
| `request.client_ip()` | the forwarded chain, walked against `trusted_proxies` |
| `request.raw_body()` | the bytes, unparsed |

`request.route()` is the one to key a rate limit on: `request.path()`
buckets by every distinct id.

## Where the port stops

`main` runs at boot, and `serve(port)` inside it is where the program says
where it listens:

```jwc no-compile
function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

The argument is an expression, evaluated at startup. The environment does
not override what the program declared — the program reads the environment
if it wants to.

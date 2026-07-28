---
sidebar_position: 1
description: "Declare HTTP endpoints with route GET/POST/PUT/DELETE. Path and query parameters, request bodies, response helpers and status codes."
---

# Routes

```jwc no-compile
route GET    "/users"           { ... }
route POST   "/users"           { ... }
route GET    "/users/{id}"      { ... }
route PUT    "/users/{id}"      { ... }
route DELETE "/users/{id}"      { ... }
route PATCH  "/users/{id}"      { ... }
```

Wildcard segments (`{id}`) bind path parameters. Query string is automatic.

## Handler binding

Two equivalent shapes:

```jwc
// inline body
route GET "/users/{id}" {
    let id_int = int(path_param("id"));
    let u = first(select User from AppDb.User where User.id == @id_int);
    return ok(u);
}

// named handler
function getUser(id: int): User? {
    return first(select User from AppDb.User where User.id == @id);
}
route GET "/users/{id}" -> getUser;
```

When you use the `-> handler` form and the handler has typed params, the server auto-binds path / query params by name and coerces types (`int(path_param("id"))` is implicit).

## Path params

```jwc
let id  = path_param("id");                  // string
let n   = int(path_param("page"));           // coerced
```

## Query string

```jwc
let q     = query_param("q");                // "" (empty string) if missing
let limit = int(query_param("limit", "20")); // default "20" → coerced to int
```

## Request body

```jwc
let raw    = body();          // parsed request body
let parsed = json_parse(raw); // when you were handed a JSON string
```

`let` bindings are untyped — there is no `let x: Type = ...` form. To check
the shape of an incoming body, declare the rules with `validate body`
(see [Validation](./validation.md)); the handler runs only if they all pass.

```jwc
route POST "/users" {
    validate body {
        name:  required, minLength(1), maxLength(120);
        email: required, pattern("^[^@]+@[^@]+$");
    }
    let req = body();
    return created(json({ name: req.name }));
}
```

## Response builders

| Built-in | Status | Body |
|---|---|---|
| `text(s)` | 200 | plain text |
| `html(s)` | 200 | HTML — `text/html` |
| `response(body, mime)` / `raw(body, mime)` | 200 | raw body under a custom Content-Type; `text/*` gets `; charset=utf-8` |
| `json(v)` | 200 | JSON-serialised value |
| `ok(v)` | 200 | JSON |
| `created(v)` | 201 | JSON |
| `noContent()` | 204 | empty |
| `badRequest(v)` | 400 | JSON |
| `unauthorized()` | 401 | `{"error":"Unauthorized"}` |
| `forbidden()` | 403 | `{"error":"Forbidden"}` |
| `notFound(v)` | 404 | JSON |
| `internalError(v)` | 500 | JSON |
| `statusCode(n, v)` | n | JSON |

camelCase and snake_case aliases both work (`notFound` ≡ `not_found`).

## Headers

```jwc
let auth = header("authorization");   // null if missing
```

There's no general setter for response headers in v1 — `content-type` is implied by the builder you choose. For a custom Content-Type, use `response(body, mime)` / `raw(body, mime)`, which ship the body verbatim (e.g. `return response(csv, "text/csv");`). `text/*` MIME types get `; charset=utf-8` appended automatically; others (e.g. `image/png`) pass through unchanged.

## WebSocket routes

```jwc
route WS "/chat/{room}" {
    while (true) {
        let msg = ws_recv();
        if (msg == null) { break; }
        ws_send("got: " + msg);
    }
}
```

See [WebSockets](./websockets) for details.

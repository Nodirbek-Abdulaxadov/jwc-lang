---
title: Routes & handlers
sidebar_position: 1
---

# Routes & handlers

## Inline route

```jwc
route GET "users/{id}" {
    let id = path_param("id");
    let u = select User from AppDb.User where User.id == @id first;
    if (u == null) { return notFound(); }
    return json(u);
}
```

- `{name}` placeholders are exposed via `path_param(name)`.
- The handler body must `return` a response or call one of the helpers
  (`json`, `created`, `notFound`, `noContent`, `internalError`,
  `unauthorized`, `forbidden`).

## Typed handler function

```jwc
function getUser(id: uuid): User? {
    return select User from AppDb.User where User.id == @id first;
}

route GET "users/{id}" -> getUser;
```

JWC binds the handler's typed params against the route's path placeholders
(then the query string, as fallback) and coerces each value to the
declared type. Misspelled params fail at compile time.

## HTTP verbs

`GET`, `POST`, `PUT`, `DELETE`, `PATCH` — duplicate routes are rejected by
the validator.

## WebSocket routes

```jwc
route WS "/chat/{room}" {
    while (true) {
        let msg = ws_recv();
        if (msg == null) { break; }
        ws_send(json_stringify({ room: path_param("room"), echo: msg }));
    }
}
```

See [Standard library → WebSocket](../stdlib#websocket).

## Query string

```jwc
route GET "posts" {
    let q      = query_param("q");
    let limit  = query_param("limit", "20");
    let offset = query_param("offset", "0");
    return json(
        select Post from AppDb.Post
            where Post.title like @q
            limit @limit offset @offset
    );
}
```

- `query_param(name)` returns the raw string, or `null` if missing.
- `query_param(name, default)` falls back to the second argument.

## Body validation

```jwc
route POST "users" {
    validate body {
        email: required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
        name:  required, minLength(2), maxLength(60);
        age:   min(0), max(120);
    }
    // ... handler ...
}
```

On failure, JWC short-circuits with HTTP **400** and a body of
`{ "errors": { "<field>": "<rule>" } }`. Supported rules:
`required`, `minLength(n)`, `maxLength(n)`, `min(n)`, `max(n)`,
`pattern("regex")`.

## Middleware

```jwc
middleware AuthMw {
    let token = header("authorization");
    if (token == null) { return unauthorized(); }
    try {
        let claims = jwt_verify(token, env("JWT_SECRET"));
        setContext("userId", claims.sub);
    } catch (e) {
        return unauthorized();
    }
}

route GET "me" use AuthMw {
    return json(context("userId"));
}
```

- Middleware that `return`s a value short-circuits the request.
- Per-route `use M1, M2` runs middlewares in declaration order.
- `header(name)` (case-insensitive), `setContext(key, val)`,
  `context(key)` share state between middleware and the handler.

## Global error handler

```jwc
errorHandler (e) {
    return internalError(e.message);
}
```

Catches any uncaught error from route bodies. `e` is bound to a JSON
envelope `{"message": "...", "causes": [...]}`. Only one
`errorHandler` may be declared per project.

---
sidebar_position: 2
---

# Middleware

```jwc
middleware Auth {
    let token = header("authorization");
    if (token == null) { return unauthorized(); }
    let claims = jwt_verify(token, env("JWT_SECRET"));
    if (claims == null) { return unauthorized(); }
    setContext("user_id", claims.sub);
    // falling off the end = pass through to the next middleware / route
}
```

Middleware is declared at the top level (not inside a route). Attach to routes with `use`:

```jwc
route GET "/me" use Auth {
    let uid = context("user_id");
    let me  = first(select User from AppDb.User where User.id == @uid);
    return ok(me);
}
```

Chain multiple:

```jwc
route POST "/admin/posts" use Auth, RequireAdmin, RateLimit {
    ...
}
```

## Short-circuit

If a middleware **returns a value** (e.g. `unauthorized()`), the whole route is short-circuited — the route body never runs, the response is what the middleware returned. Falling off the end (no `return`) hands control to the next middleware / route body.

## Context

```jwc
middleware Auth {
    setContext("user_id", claims.sub);
    setContext("roles",   claims.roles);
}
route GET "/me" use Auth {
    let uid: string = context("user_id");
}
```

Context is a per-request key/value bag — gone when the response is sent.

## Built-ins inside middleware

All HTTP built-ins (`header`, `query_param`, `path_param`, `body`) work. Database calls and `await` work. There's no special async marker on middleware; it inherits the route's task.

## Groups (prefix + middleware)

```jwc
group "/api" use ApiAuth {
    route GET  "/users"  { ... }   // → GET /api/users
    route POST "/users"  { ... }
}
```

Groups nest. The inner middleware list **adds to** the outer:

```jwc
group "/api" use ApiAuth {
    group "/admin" use RequireAdmin {
        route GET "/users" { ... }   // → GET /api/admin/users, ApiAuth + RequireAdmin
    }
}
```

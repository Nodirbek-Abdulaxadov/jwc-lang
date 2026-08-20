---
sidebar_position: 2
description: "Cross-cutting request handling in JWC: declaring middleware, attaching it to routes and groups, and the request and response phases."
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

```jwc no-compile
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
    let uid = context("user_id");
}
```

Context is a per-request key/value bag — gone when the response is sent.

## Built-ins inside middleware

All HTTP built-ins (`header`, `query_param`, `path_param`, `body`) work. Database calls and `await` work. There's no special async marker on middleware; it inherits the route's task.

## Response-phase: `after { ... }`

A middleware can declare a second body that runs **after** the route handler returns. Use it for access logs, metric counters, audit trails — anything that needs to observe the response, not the request.

```jwc no-compile
middleware Telemetry {
    // request phase: runs before the handler
    setContext("started_at", unix_timestamp());

    after {
        // response phase: runs after the handler returns
        let line = json_stringify({
            "request_id":  request_id(),
            "status":      response_status(),
            "duration_ms": response_duration_ms(),
            "client":      client_ip(),
        });
        log_info(line);
    }
}
```

**Ordering.** Request-phase bodies run in the order middleware are listed on the route. `after` blocks run in **reverse order** — the last middleware finishes first. This is the conventional onion pattern: the outermost middleware sees the response last.

**Three builtins are designed for `after`:**

| Builtin | Returns | Useful for |
|---|---|---|
| `response_status()` | `int` — the wire status (200, 201, 500, etc.), including any explicit `response(status, body)` call | metric labels, structured-log status field |
| `response_duration_ms()` | `int` — milliseconds since the dispatcher first saw the request | latency histograms, slow-request warnings |
| `request_id()` | `string` — same id as the request phase, echoed back as `x-request-id` | correlating `after` log lines to the request log |

`response_status()` returns `null` outside of an `after` block (the value isn't known until the handler returns). `response_duration_ms()` and `request_id()` work in both phases.

**Error isolation.** An exception inside one `after` block does NOT skip the others — each `after` block is run independently. Errors are surfaced through the regular error handler; the response that already went out to the wire is not rewritten.

**Skipped on timeout.** If `JWC_REQUEST_TIMEOUT` fires and the watchdog short-circuits to a `504`, `after` blocks do not run for that request — the upstream client has already given up.

## Groups (prefix + middleware)

```jwc no-compile
group "/api" use ApiAuth {
    route GET  "/users"  { ... }   // → GET /api/users
    route POST "/users"  { ... }
}
```

Groups nest. The inner middleware list **adds to** the outer:

```jwc no-compile
group "/api" use ApiAuth {
    group "/admin" use RequireAdmin {
        route GET "/users" { ... }   // → GET /api/admin/users, ApiAuth + RequireAdmin
    }
}
```

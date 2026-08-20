---
sidebar_position: 4
description: "Error handling in JWC: try / catch with typed error kinds (DbError, HttpError, ValidationError) and a project-wide error_handler for uncaught failures."
---

# Error handling

## try / catch

In-handler:

```jwc no-compile
try {
    let u = first(select User from AppDb.User where User.id == @id);
    return ok(u);
} catch (e: DbError) {
    return internalError({ error: "database down" });
} catch (e) {
    return internalError({ error: e.message });
}
```

## Known error kinds

| Kind | When |
|---|---|
| `DbError` | Database driver / connection / query error |
| `HttpError` | `http_get` / `fetch_json` failure |
| `ValidationError` | JSON parsing / type check at boundary |
| `TimeoutError` | `await sleep_ms` cancelled, futures timed out |
| `Error` | Catch-all (`throw` inside user code, division by zero, …) |

The catch type filter is a runtime classifier (`runner::classify_jwc_error`). Unknown catch kinds fail at compile time with a "did you mean?" hint.

## Catch binding

`e` is bound to a JSON value:

```jwc no-compile
{ "type": "DbError", "message": "connection closed", "causes": ["..."] }
```

So `e.type` / `e.message` work without parsing.

## Global error handler

Top-level declaration that wraps **every** route that doesn't catch internally:

```jwc
errorHandler (e) {
    return internalError({
        error: e.message,
        code:  e.type,
        ref:   uuid()    // useful for log-correlation
    });
}
```

Only one `errorHandler` per program. It does not catch:

- Middleware short-circuit returns (those are normal responses)
- Errors after the response stream has started (e.g. mid-stream WS send)

For those, log inside the handler/middleware itself.

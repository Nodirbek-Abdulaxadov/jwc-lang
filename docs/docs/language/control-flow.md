---
sidebar_position: 4
---

# Control flow

## if / else

```jwc
if (count > 100) {
    return ok({ tier: "gold" });
} else if (count > 10) {
    return ok({ tier: "silver" });
} else {
    return ok({ tier: "bronze" });
}
```

There's no `unless` / no ternary expression. Use plain `if`.

## while

```jwc
let i = 0;
while (i < 10) {
    print(i);
    i = i + 1;
}
```

`break` exits the innermost loop; `continue` jumps to the next iteration.

## for-in

Iterates a JSON array:

```jwc
let items = ["a", "b", "c"];
for item in items {
    print(item);
}
```

The loop header has **no parentheses** — `for <var> in <iterable> { ... }`.
`break` / `continue` / `return` all work inside the body. The iterable is evaluated **once** at loop start; items round-trip through JSON.

## try / catch

```jwc no-compile
try {
    let user = first(select User from AppDb.User where User.id == @id);
    return ok(user);
} catch (e: DbError) {
    return internalError({ error: "database problem" });
} catch (e: ValidationError) {
    return badRequest({ error: e.message });
} catch (e) {
    // catch-all
    return internalError({ error: e.message });
}
```

Known error kinds: `Error`, `DbError`, `HttpError`, `ValidationError`, `TimeoutError`. The catch binding (`e`) is a JSON object `{ type, message, causes }`. See [Error handler](../backend/error-handler).

## Global error handler

Top-level `errorHandler` catches anything an uncaught route throws:

```jwc
errorHandler (e) {
    return internalError({ error: e.message, code: e.type });
}
```

Only one handler per program.

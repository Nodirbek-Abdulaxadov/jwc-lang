---
sidebar_position: 3
---

# Functions & classes

## Function declaration

```jwc
function add(a: int, b: int): int {
    return a + b;
}

function greet(name: string) {    // no return type → returns void
    print("hello, " + name);
}
```

Parameters are typed; the return type is optional (`void` if omitted).

## Async functions

```jwc
async function fetchUser(id: int): User? {
    let raw = await http_get("https://api.example.com/users/" + id);
    return json_parse(raw);
}
```

`async`/`await` are real — the Vm and HTTP server use tokio. `await` only inside `async` bodies. See [Async helpers](../stdlib/http) for built-ins.

## Classes (DTOs)

`class` declares a typed object shape — no methods, just a struct. Used for typed request/response bodies:

```jwc no-compile
class CreateUserRequest {
    name: string;
    email: string;
    age: int?;
}

route POST "/users" {
    let req = body();
    return created({ id: 1, name: req.name });
}
```

Field accesses on a typed param are checked at compile time:

```
Type error: field 'agee' is not declared on CreateUserRequest
```

## Closures

JWC does not have anonymous functions today. Use named `function` declarations.

## Recursion

Recursion is supported — the interpreter is fully async, so deep recursion in an async function is bounded by tokio task stack (~2 MB default), not the OS thread stack.

```jwc
async function fib(n: int): int {
    if (n < 2) { return n; }
    return await fib(n - 1) + await fib(n - 2);
}
```

---
title: Control flow & expressions
sidebar_position: 3
---

# Control flow & expressions

## Variables

```jwc
let name = "JWC";    // immutable-style declaration (rebind needs `=`)
name = "JWC 2";      // rebind to a new value
```

`let` declares; subsequent bare assignment rebinds. Shadowing within the
same scope is rejected at runtime.

## Literals

```jwc
let n   = 42;
let f   = 0.25;
let s   = "hi";
let raw = r"\d+\.\d+";              // raw string — no escape processing
let tpl = `user=${user.name}`;     // template string
let b   = true;
let z   = null;
let obj = { name: "Najim", age: 25 };
```

JSON object literals (`{ k: v }`) evaluate to a JSON-string `Value`.
Nested arrays / objects already carried as JSON embed raw, so
`return json({ items: posts, total: count });` produces
`{"items": [...], "total": 5}` rather than the double-encoded form.

## Operators

| | |
|---|---|
| Arithmetic | `+ - * / %`, unary `-` |
| Comparison | `== != < <= > >=` |
| Logical | `and`, `or`, unary `!` |
| String concat | `+` (right-hand side coerced to string) |

## Control flow

```jwc
if (x > 0) { print("positive"); }
else if (x == 0) { print("zero"); }
else { print("negative"); }

while (i < 10) {
    if (i == 3) { continue; }
    if (i == 8) { break; }
    i = i + 1;
}

for item in items {
    if (item == "stop") { break; }
    print(item);
}

try {
    riskyOp();
} catch (e) {
    return internalError(e.message);
}
```

`for VAR in EXPR { ... }` iterates over a JSON array (e.g. `select`
results, `body()` payloads, literals like `"[1,2,3]"`). `break`,
`continue`, and `return` work as expected.

## Functions

```jwc
function add(a: int, b: int): int {
    return a + b;
}

async function fetchUser(id: uuid): User? {
    return await getUser(id);
}
```

`async` / `await` parse cleanly — the runtime is currently synchronous, so
`await` is a transparent pass-through, but the syntax is forward
compatible with the upcoming async runtime.

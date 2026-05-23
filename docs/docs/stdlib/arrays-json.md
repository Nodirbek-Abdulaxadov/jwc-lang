---
sidebar_position: 2
---

# Arrays & JSON

JSON is the native data shape — arrays and objects are first-class. There's no separate `Array<T>` type; a JSON-array string round-trips through `json_parse` / `json_stringify`.

## Built-ins

| Built-in | Returns | Notes |
|---|---|---|
| `length(xs)` | `int` | element count for arrays, key count for objects, 0 for null |
| `first(xs)` | `any` | element 0, or `null` if empty |
| `last(xs)` | `any` | last element, or `null` if empty |
| `contains(xs, x)` | `bool` | strict equality |
| `json_parse(s)` | `any` | parses a JSON string into a structured value |
| `json_stringify(v)` | `string` | inverse |

## Object literals

```jwc
let user = {
    id:    1,
    name:  "ali",
    roles: ["admin", "editor"],
    profile: { age: 30 }
};
return json(user);
```

Nested objects, arrays, and primitives mix freely.

## Iterating

```jwc
let nums = [1, 2, 3];
for (n in nums) {
    print(n);
}
```

`for ... in` accepts both arrays and JSON-array-strings (auto-parsed). `break` / `continue` / `return` all work.

## When to reach for `json`

The `json` type in entity columns or function signatures means "arbitrary JSON value":

```jwc
entity Event of AppDb {
    id: uuid pk;
    payload: json;
}
```

The DB column is `jsonb`; reads and writes round-trip through `json_parse` / `json_stringify` automatically.

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
| `range(n)` / `range(start, end)` / `range(start, end, step)` | `int[]` | `[0..n-1]`, `[start..end-1]`, or stepped; step must be positive |
| `push(arr, x)` / `append(arr, x)` | `any[]` | appends `x` to the array variable in place (first arg must be a variable); returns the array |
| `join(arr, sep)` | `string` | stringifies each element and concatenates with `sep`; O(n) |
| `json_parse(s)` | `any` | parses a JSON string into a structured value |
| `json_stringify(v)` | `string` | inverse |

## Array literals

```jwc
let nums  = [1, 2, 3];
let empty = [];
let mixed = [1, "two", true];   // heterogeneous elements are fine
```

An array literal evaluates to a real array value, iterable with `for ... in`.

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
for n in nums {
    print(n);
}
```

`for ... in` (no parentheses) accepts both arrays and JSON-array-strings (auto-parsed). `break` / `continue` / `return` all work.

## Building arrays

```jwc
let squares = [];
for n in range(5) {             // 0, 1, 2, 3, 4
    push(squares, n * n);       // mutates `squares` in place
}
let csv = join(squares, ",");   // "0,1,4,9,16"
```

## When to reach for `json`

The `json` type in entity columns or function signatures means "arbitrary JSON value":

```jwc
entity Event of AppDb {
    id: uuid pk;
    payload: json;
}
```

The DB column is `jsonb`; reads and writes round-trip through `json_parse` / `json_stringify` automatically.

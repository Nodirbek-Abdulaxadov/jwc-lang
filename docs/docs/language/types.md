---
sidebar_position: 2
---

# Types

JWC has a small fixed type set. Types appear on function parameters, return types, and entity columns. Local variables are untyped today (inferred at the runtime); compile-time inference is on the [roadmap](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/ROADMAP.md).

## Primitives

| Type | Notes |
|---|---|
| `string` / `str` | UTF-8 text. |
| `int` / `integer` / `number` | 64-bit signed. `int(min,max)` constrains an entity column. |
| `bigint` | Alias of `int` at runtime; distinct in SQL DDL. |
| `double` / `float` | 64-bit float. |
| `decimal` | Fixed-point. `decimal(precision, scale)` in DDL. |
| `bool` / `boolean` | `true` / `false`. |
| `uuid` | ISO RFC-4122 format. `where User.id == @id` works. |
| `datetime` / `timestamp` | ISO 8601 UTC string. `now()` returns one. |
| `json` | Arbitrary JSON value (object / array). |
| `bytes` / `byte[]` | Base64-encoded string on the wire. |

## Nullable

Append `?` to make a type nullable; `Optional<T>` is an alias:

```jwc no-compile
function find(id: int): User? { ... }   // may return null
function find(id: int): Optional<User> { ... }
```

## Lists

```jwc no-compile
function tags(): List<string> { ... }
```

A `List<T>` is JSON-array-shaped; element types are checked at the boundary.

## Type validation

Typed parameters and return values are validated at the runtime boundary (entering a function from HTTP, leaving a function to the client). Type errors surface as:

```
Type error: parameter 'id' expects int, got string "abc"
```

## Coercion

JWC tolerates a handful of safe coercions so HTTP path/query params don't need manual parsing:

- `string → int` when the string parses as an integer
- `int → string`
- `int ↔ double`
- `Value::Null ↔ T?` (nullable)

Anything else is a `Type error` — no silent truncation.

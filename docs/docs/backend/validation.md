---
sidebar_position: 4
title: "Validation"
description: "A class is a whitelist with rules attached. The cast is what validates, the 400 body is fixed, and user code cannot produce a different one."
---

# Validation

A `class` describes a request body. The rules live with the shape, and
the cast is what runs them.

```jwc
namespace dto.auth;

class Register {
    email        varchar(255) required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
    display_name varchar(80) required, minLength(2);
    password     varchar(200) required, minLength(10);
}
```

```jwc no-compile
route POST "register" {
    let req = request.body() as Register;
    …
}
```

`request.body()` on its own is an error: the body has no declared shape
until it is validated, so there is nothing to read a field off.

## A class is a whitelist

Keys the class does not name are **dropped silently**. Rejecting extras
breaks every client that adds a field, and accepting them into a write is
how a request sets a column it had no business setting.

Which is also why `server` columns exist: a column declared `server` is
one a request body can never write, whatever the client sends.

## Rules

| Rule | Applies to |
|---|---|
| `required` | the key must be present and non-null |
| `minLength(n)` / `maxLength(n)` | text length, in characters |
| `minItems(n)` / `maxItems(n)` | array length |
| `min(n)` / `max(n)` | numeric bound, compared as a decimal — `min(0)` rejects `-1.00` |
| `pattern(r"…")` | regular expression |
| `transient` | validated, never written to a column |

Rules are **collected**, not fail-fast: one request produces every failure
it has, so a form can show all of them at once.

## The 400 body is fixed

```json
{
  "error": "validation_failed",
  "fields": [
    {"path": "email", "rule": "pattern", "message": "email shakli mos emas"},
    {"path": "password", "rule": "minLength", "limit": 10,
     "message": "password kamida 10 belgidan iborat bo'lishi kerak"}
  ]
}
```

User code cannot produce a different one, because validation is not
reachable from user code. A client can be written against this shape once.

`path` is dotted for a nested class and indexed for an array element, so
`items[2].sku` names exactly one field.

## Optional fields

A field without `required` may be absent. Give it a `?` if the service is
going to supply a default:

```jwc no-compile
class CreateTask {
    title    varchar(300) required, minLength(1);
    status   varchar(30)?;
    priority varchar(20)?;
}
```

Without the `?`, the class type says the field is always present, `??` is
dead code, and the insert binds null into a `NOT NULL` column.

With it:

```jwc no-compile
status = $req.status ?? "todo",
```

## Absent is not null

`types.md §6.5`: a key the body omitted and a key it sent as `null` are
different. That is what `=?` and `...$req` rest on — an omitted field
leaves the column alone, an explicit null clears it. A validated class
carries the distinction through, so the write can act on it.

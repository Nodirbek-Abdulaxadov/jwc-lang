---
sidebar_position: 3
description: "The validate body block declares per-field rules and returns 400 with a structured errors object before your handler runs. All built-in validators."
---

# Request validation

The `validate body { ... }` block declares per-field rules. On failure the route returns 400 with `{"errors":{...}}` before your handler runs.

```jwc
route POST "/users" {
    validate body {
        name:     required, minLength(2), maxLength(64);
        email:    required, pattern(r"^[^@]+@[^@]+\.[^@]+$");
        age:      min(0), max(150);
    }
    let req = body();
    // … handler runs only if all rules passed
}
```

## Built-in rules

| Rule | Effect |
|---|---|
| `required` | field must be present and non-null |
| `minLength(n)` / `maxLength(n)` | string length |
| `min(n)` / `max(n)` | numeric range |
| `pattern("regex")` | string matches the regex |

The `r"..."` raw string form is recommended for patterns so `\d` doesn't need double-escaping.

## Failure response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/json

{"errors":{"email":"pattern","age":"min"}}
```

One entry per failing field; value names the first failing rule.

## Custom validation

For cross-field rules (`endsAt > startsAt`), do it manually in the handler and return `badRequest({...})`. The declarative block is intentionally limited — there's no escape into arbitrary code.

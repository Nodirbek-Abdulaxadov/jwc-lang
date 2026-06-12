---
sidebar_position: 1
---

# Strings

| Built-in | Returns | Notes |
|---|---|---|
| `length(s)` | `int` | char count (not byte count) |
| `lower(s)` | `string` | |
| `upper(s)` | `string` | |
| `trim(s)` | `string` | strips leading/trailing whitespace |
| `contains(s, needle)` | `bool` | |
| `starts_with(s, p)` | `bool` | |
| `ends_with(s, p)` | `bool` | |
| `replace(s, from, to)` | `string` | global replace |
| `split(s, sep)` | `string` | JSON-array-string of pieces |
| `substring(s, start, len)` | `string` | char-based slice, clamps to empty out of range |
| `take(s, n)` | `string` | first `n` chars — shorthand for `substring(s, 0, n)` |

```jwc
let lower_email = lower(trim(req.email));
if (ends_with(lower_email, "@example.com")) {
    return badRequest({ error: "internal addresses not allowed" });
}
let parts = json_parse(split(lower_email, "@"));   // ["ali", "example.com"]
```

`split` returns a JSON-array string so the result can flow through `for ... in` without an explicit parse:

```jwc
for (part in json_parse(split(s, ","))) {
    print(part);
}
```

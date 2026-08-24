---
sidebar_position: 1
title: Syntax
description: "Comments, literals, sigils, and the fact that JWC has no reserved words."
---

# Syntax

## Comments

```jwc no-compile
-- a line comment
--- a doc comment: it attaches to the next declaration and reaches the
--- database as a COMMENT ON, and `jwc migrate` diffs it
```

`//` is **not** a comment. It is division, twice, and the first `//` line in
a 0.9.x file is where the port stops.

## Literals

| Kind | Form |
|---|---|
| Integer | `42` — no underscores, no hex, no exponent |
| Decimal | `9.99` — always `numeric`, never a float |
| Text | `"…"` with `\"` `\\` `\n` `\r` `\t` `\0` `\u{…}` |
| Raw text | `r"…"` — no escapes except `\"`, for regular expressions |
| Boolean | `true`, `false` |
| Null | `null` |

**A literal newline inside a string is an error, and `r"…"` ends at the
line.** There is no multi-line string. Build long text by concatenating:

```jwc no-compile
function page() -> text {
    return "<!doctype html>\n"
        + "<title>hello</title>\n"
        + "<h1>hello</h1>\n";
}
```

## Sigils

Two characters carry meaning where a bare name would be ambiguous:

| Sigil | Means |
|---|---|
| `$name` | a local variable, read inside an expression or a query |
| `@name` | a path parameter, from the route's pattern |

```jwc no-compile
route GET "" {
    let account = AccountService.one(@id);
    return json($account);
}
```

The sigil is part of the token — `@ id` is an error. Inside a query, the
distinction is load-bearing: a bare `email` is the *column*, `$email` is
your variable, and without the sigil `where email == email` would be a
tautology no one meant to write.

## There are no reserved words

Every word the grammar gives meaning to is also a legal identifier; position
decides. `route`, `key`, `max`, `check`, `text` and `date` all appear as
ordinary column names in the specification's own sample. A reserved-word
list would forbid the language's own examples.

## Declarations

At the top level a file may declare:

```
namespace   import      database    schema      table       view
enum        class       error       errorHandler
service     function    middleware  routes      test
server
```

Statements — `let`, assignment, `if`, `for`, `return`, `throw`,
`transaction`, `break`, `continue` — live inside bodies. See
[control flow](./control-flow).

## Operators

```
or  and  not          -- logical (`!x` is also `not`)
==  !=  <  <=  >  >=  -- comparison
+  -  *  /  %         -- arithmetic; `+` also concatenates text
??                    -- null coalescing
? :                   -- conditional
...                   -- spread, in an object or a `set`
```

`+` does not coerce: `text + int` is `E0370`, and the fix is
`string.of(…)`. `/` on two integers is integer division.

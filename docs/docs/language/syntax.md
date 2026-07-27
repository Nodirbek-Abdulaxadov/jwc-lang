---
sidebar_position: 1
---

# Syntax basics

JWC reads like a hybrid of TypeScript and SQL. Statements end with `;`, blocks use `{ }`, comments are `//` and `/* */`.

## Comments

```jwc no-compile
// single line
/* multi
   line */
```

## Variables

```jwc
let name  = "ali";        // type inferred
let count = 42;
let n     = 7;            // bindings are untyped; no `let x: T = ...` form
count = count + 1;        // re-assignable
```

`let` is for re-assignable variables. For read-only values use `const` (see below).

### Module-level `const`

```jwc
const PI  = 3.14159;       // top-level, read-only, frozen
const TAU = PI * 2;        // a const may reference another const
```

Declared at the top level with `const NAME = expr;`. The value is read-only and visible everywhere — inside routes, functions, middlewares, and `main`. The right-hand side must be a **constant expression**: only literals, operators, array/object literals, and references to other consts. No function calls, DB access, field access, or `await`. The compiler rejects non-const expressions, undeclared names, duplicate names, and circular references (e.g. `const X = X + 1;`).

## String literals

```jwc
let plain  = "hello";
let escaped = "line\nbreak";
let raw    = r"^[^@]+@[^@]+\.[^@]+$";  // raw — no escape processing
let templ  = "user " + name + " has " + count + " items";
let xs     = [1, 2, 3];                 // array literal
```

Concatenation is `+`. There's no template-literal interpolation today; use `+` or `json({...})`.

## Operators

```jwc no-compile
+  -  *  /  %      // arithmetic
== != <  <= > >=   // comparison
&& || !            // logical (short-circuit && and ||)
```

`!=` is preferred over `not equal`. Unary `!` flips a boolean.

## Function calls

```jwc
print("hi");
let n = length(items);
let v = json_stringify({ ok: true });
```

Built-ins are documented in [Standard library](../stdlib/strings).

## Statements vs expressions

JWC is statement-oriented: `if`, `while`, `for`, `return` are statements. Expressions are values: literals, identifiers, calls, operators, object/array literals, SQL constructs.

```jwc
let total = sum(items.prices);     // expression
if (total > 100) {                  // statement
    return ok({ discount: 0.1 });
}
```

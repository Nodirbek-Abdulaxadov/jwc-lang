---
sidebar_position: 1
---

# Syntax basics

JWC reads like a hybrid of TypeScript and SQL. Statements end with `;`, blocks use `{ }`, comments are `//` and `/* */`.

## Comments

```jwc
// single line
/* multi
   line */
```

## Variables

```jwc
let name  = "ali";        // type inferred
let count = 42;
let typed: int = 7;       // explicit type (compile-time check)
count = count + 1;        // re-assignable
```

`let` is the only declaration keyword. There is no `const`.

## String literals

```jwc
let plain  = "hello";
let escaped = "line\nbreak";
let raw    = r"^[^@]+@[^@]+\.[^@]+$";  // raw — no escape processing
let templ  = "user " + name + " has " + count + " items";
```

Concatenation is `+`. There's no template-literal interpolation today; use `+` or `json({...})`.

## Operators

```jwc
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

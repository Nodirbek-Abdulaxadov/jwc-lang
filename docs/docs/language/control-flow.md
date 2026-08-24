---
sidebar_position: 3
title: Control flow
description: "let, assignment, if, for, break, continue, and the two ways out of a function."
---

# Control flow

## Binding and assignment

```jwc no-compile
let total = 0;
$total = $total + 1;      -- assignment reads the sigil
```

There is no `+=`. A binding is declared with `let` and written with `$`.

## `if`

```jwc no-compile
if ($role != MemberRole.owner) {
    throw Forbidden("faqat egasi o'chira oladi");
}
```

The condition must be a boolean — there is no truthiness. `else if` chains
as usual.

## `for`

```jwc no-compile
for (line in $invoice.lines) {
    $total = $total + $line.amount;
}
```

The binder is a bare name and references use `$`. The iterable is an array:
a `T[]` field, a query result, or an array literal. Iterating a `Raw` is
`E0311` — add a projection.

## `break` and `continue`

Both leave the enclosing `for`; outside one they are `E0813`.

They matter most inside a postfix `catch`, which must diverge. Without them
a retry loop cannot be written, because `return` and `throw` leave the whole
function:

```jwc no-compile
for (attempt in [1, 2, 3, 4, 5]) {
    let code = fresh_code();
    insert into App.public.Links {
        code = $code,
        url  = $req.url
    } catch Conflict (err) {
        continue;
    };
    return $code;
}
throw CodeExhausted("qisqa kod ajratib bo'lmadi");
```

## Leaving a function

`return <expr>` or `throw <Error>(…)`. A route body must end every path in
`return <Response>` (`E0731`); returning anything else is `E0732`.

Inside an `after` block a bare `return;` ends that block only, and
`return <expr>;` is `E0810` — an `after` block cannot produce a response.

## `transaction`

```jwc no-compile
transaction {
    let org = insert into App.org.Orgs { ...$req } as { id, slug };
    insert into App.org.Members {
        org_id     = $org.id,
        account_id = $owner_id,
        role       = MemberRole.owner
    };
    return $org;
}
```

Every statement in the block runs in one Postgres transaction. A `throw`
rolls it back. A postfix `catch` inside one compiles to a savepoint, because
without it the connection would be in an aborted transaction and every later
statement would fail.

## No `while`, no early `else`, no `switch`

`for` over an array is the only loop. A bounded retry is a `for` over a
literal array, which makes the bound visible at the call site rather than in
a counter someone has to read.

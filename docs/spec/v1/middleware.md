# middleware.md — ordering, composition, `after`, and `context`

Normative. Closes gaps **#13**, **#14**, **#37**, and **G5**/**N7** from
error-model.md.

---

## 1. Shape

```jwc
middleware RequireOrgMember(@org_id: bigint)
    requires RequireAuth
    provides org_id: bigint, role: MemberRole
{
    let account_id = context.account_id;

    let access = select MA from App.org.MemberAccess
        where org_id == @org_id and account_id == $account_id
        first
        or throw Forbidden("siz bu tashkilot a'zosi emassiz");

    context.org_id = @org_id;
    context.role   = $access.role;
}
```

Three declaration clauses, all optional, all checked:

| Clause | Meaning |
|---|---|
| `(@name: T, …)` | path parameters this middleware reads (#13) |
| `requires A, B` | middleware that must run before this one (#13) |
| `provides k: T, …` | context keys this middleware sets (#17-adjacent) |

---

## 2. Path-parameter binders (#13)

2.1 A middleware that reads `@org_id` must declare it. Reading an undeclared
`@name` is `E0801`.

2.2 At **every attachment site**, the route's binder set (routing §3.3) must
contain every declared binder with a matching type (`E0802`), reported at the
`use` site, not at the middleware.

This is the gap the sample walked into: `RequireOrgMember` read `@org_id`
from whatever route happened to have one, and attaching it to a route without
one was a runtime surprise.

---

## 3. `requires` (#13, #37)

3.1 `requires A` asserts that `A` runs before this middleware in the resolved
chain of every route it is attached to. Violation is `E0803`, naming the
route and printing the resolved chain.

3.2 `requires` does **not** insert `A` into the chain. Declared dependencies
are checked, not satisfied automatically; inserting middleware the reader
cannot see at the `use` site is exactly the invisible-order problem.

3.3 This is what makes `RequireOrgAdmin` honest. It reads `context.role`,
which only `RequireOrgMember` sets, so it declares
`requires RequireOrgMember`, and a route that lists `RequireOrgAdmin` without
it fails to compile.

---

## 4. Composition and order (#14, #37)

### 4.1 The resolved chain

For a route, the chain is:

1. the enclosing `routes` block's `use` list, **in written order**;
2. then the `route`'s own `use` list, **in written order**.

Route-level `use` **appends**; it never replaces. A name appearing in both
is `E0804` (duplicate in chain), because the intended meaning — run twice,
or reorder — is not guessable.

### 4.2 Execution

Each middleware body runs to completion in chain order. A middleware
completes in one of three ways:

| Outcome | Effect |
|---|---|
| falls off the end | the chain continues |
| `return <Response>` | **short-circuit** — the chain stops, that response is used |
| `throw E(...)` | the chain stops, `errorHandler` produces the response |

`return` from a middleware is reserved for deliberately **non-error**
responses: a redirect, a `304`, a `202`. Errors use `throw` (errors §2.2,
E14). A middleware that `return`s a 4xx/5xx builder is `W0801` pointing at
the `throw` equivalent.

### 4.3 `after` blocks run in reverse

Every middleware that **started** — including the one that short-circuited —
runs its `after` block, in reverse chain order. Middleware that never started
does not.

---

## 5. `after` (#14, G5, N7)

```jwc
middleware Audit {
    after {
        let method = request.method();
        let status = response.status();

        if ($method == "GET" or $status >= 400) { return; }

        let path = request.route();
        insert into App.audit.Events {
            org_id   = context.org_id?,
            actor_id = context.account_id?,
            action   = $method,
            entity   = $path,
            ip       = request.client_ip()
        };
    }
}
```

5.1 An `after` block runs for **every** outcome: normal response,
short-circuit, and error — including the response the `errorHandler`
produced. `response.status()` is therefore the status actually being sent.

5.2 `after` runs **after** the `errorHandler`, and after any transaction has
been committed or rolled back (writes §7.2).

5.3 A bare `return;` inside `after` ends **that block only** (N7). It cannot
produce a response; `return <expr>;` in an `after` block is `E0810`.

5.4 An `after` block may modify the response through
`response.set_header(k, v)` / `response.add_header(k, v)`. It may not change
the status or the body: an audit hook that can rewrite a 200 into a 500 is
not a hook.

5.5 **The raise set of an `after` block must be empty** (errors §3, E7).
An `after` block that can throw is `E0811`, because there is no outer handler
left — the response has already been decided. Writes inside `after` must
therefore be infallible-by-construction or wrapped in a postfix `catch`.

---

## 6. `context`

### 6.1 Typed, declared, dotted

Context is **not** an untyped map. `context.get("account_id")` does not
exist. A middleware declares what it provides:

```
provides account_id: bigint
```

and every reader writes `context.account_id`, which has type `bigint`.

design.md called `context.get/set` "the weakest point" of the invented
surface. This clause is the replacement: the keys are declared, the types are
declared, and the compiler can tell you that nobody set the one you read.

### 6.2 Resolution

`context.k` in a route, middleware or `after` block resolves to the `provides`
declaration of some middleware **that provably runs earlier** in every chain
that reaches this code. If no such middleware exists: `E0820`, naming the
readers and the `requires` clause that would fix it.

### 6.3 `context.k?`

`context.k?` is the nullable read, of type `T?`, for a key that is set only
on some paths (the `Audit` middleware above reads `org_id` on routes that
have no org). Reading `context.k` where the key is not set on every path is
`E0820`; `context.k?` is always legal for a declared key.

### 6.4 Writing

`context.k = v` is legal only inside a middleware that declares `k` in its
`provides` clause (`E0821`). Routes and `after` blocks read only.

---

## 7. Middleware and the error model

7.1 A middleware's raise set participates in exhaustiveness exactly like a
route's (errors §3).

7.2 A middleware may not open a `transaction` (writes §7.4).

7.3 A middleware body may not contain a `route` or another `middleware`.

---

## 8. Ordering summary — worked example

For

```jwc no-compile
routes "/api/v1/orgs/{org_id: bigint}" use RequireAuth, RequireOrgMember, Audit {
    route PATCH "" use RequireOrgAdmin { … }
}
```

the resolved chain of `PATCH /api/v1/orgs/{org_id}` is

```
before:  RequireAuth → RequireOrgMember → Audit → RequireOrgAdmin → handler
after:   Audit.after                                    (reverse order, only Audit has one)
```

`jwc routes` prints exactly this. If `RequireOrgAdmin` had been listed
without `RequireOrgMember` anywhere in the chain, `E0803` would name this
route and print this table.

---

## 9. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0801` | undeclared `@name` in a middleware |
| `E0802` | attachment site lacks a declared binder |
| `E0803` | `requires` not satisfied by the resolved chain |
| `E0804` | middleware appears twice in one chain |
| `E0805` | `uses` names something that is not a declared middleware |
| `E0810` | `return <expr>` inside `after` |
| `E0811` | `after` block can raise |
| `E0820` | `context.k` is not provided on every path |
| `E0821` | `context.k = …` without a `provides` declaration |
| `W0801` | middleware returns an error response instead of throwing |

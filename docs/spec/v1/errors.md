# errors.md — the error model, normative

This is the normative form of the decision analysed in
[`error-model.md`](./error-model.md) and recorded in ROADMAP §4. Where the
two differ, this file wins.

Closes **G1**–**G8**, **N4**, and gaps **#28**, **#30**, **#32**.

**The decision:** keep automatic propagation (`throw` + one `errorHandler`),
but make error types *declared*, make each function's raise set *inferred*
over the static call graph, and check exhaustiveness *once* at the app
boundary. Add exactly one local recovery form, savepoint-scoped inside
transactions.

---

## 1. Error types

### 1.1 Declaration (E1, G2)

```jwc
error PaymentDeclined(code: text) = 402 : "to'lov rad etildi";
error QuotaExceeded = 429;
```

- `= <status>` is the **default HTTP status**. It is what makes an
  `errorHandler` arm optional (§4.3).
- `: "message"` is the default message.
- Parameters are typed; `err.code` inside a `catch` arm has that type.
- An error name is in the flat declaration space (names §5.1).

### 1.2 Predeclared

| Name | Status |
|---|---|
| `BadRequest(message: text)` | 400 |
| `Unauthorized(message: text)` | 401 |
| `Forbidden(message: text)` | 403 |
| `NotFound(message: text)` | 404 |
| `Conflict(message: text)` | 409 |
| `Gone(message: text)` | 410 |
| `TooManyRequests(message: text)` | 429 |
| `ConstraintViolation(message: text, constraint: text)` | 400 |

`Unauthorized` exists precisely so `AuthService.login` can answer 401 for bad
credentials. 403 means *authenticated but not allowed*; the sample had it
wrong and had no other name to reach for (G4).

### 1.3 Unknown names are errors (E1)

`throw NotFund("…")` is `E1001: unknown error type`. It was a valid program
before; it is the single most common way a typed-arm design silently
degrades to 500.

### 1.4 Faults

A **fault** is a failure with no declared type: a division by zero, an
overflow, a coercion failure on non-client-derived input (types §7.2), a
message-less constraint violation (§6.2), a driver error, a panic.

Faults are not values, cannot be named in `throw`, and are caught only by an
untyped `catch (err)` arm (E4). They always log with the request id and
answer `500 {"error":"internal_error"}`.

---

## 2. Where errors come from

### 2.1 Services throw; they never return responses (G1)

A `service` function signals failure with `throw`. Returning a `Response`
from a service is `E0330` (types §8). Services do not know HTTP.

### 2.2 Middleware throws too (E14)

A middleware signals failure with `throw`. `return <Response>` from a
middleware is reserved for deliberately non-error responses
(middleware §4.2). This removes the two-spellings-for-one-outcome problem
G1 named.

### 2.3 Routes contain no error handling

A route reads input, calls a service, and returns a response. It has no
`catch`. This is a style rule the language enforces only through §7.2:
postfix `catch` is legal in a route but its block must diverge, so it cannot
be used to build a second error channel.

---

## 3. Raise sets (E2)

### 3.1 Definition

The raise set of a function, route, middleware or `after` block is the least
fixed point of:

```
raises(f) =  { E | `throw E(…)` in f }
          ∪  { E | `… or throw E(…)` in f }
          ∪  ⋃ { raises(g) | f calls g }
          ∪  { E | E is promoted from a constraint of a table f writes }   (§6)
          −  { E | E is caught by a postfix `catch` whose block diverges }
```

Computation starts from the empty set and iterates to a fixed point over the
static call graph. The graph is exact because there are no function values
(types §1).

### 3.2 Faults are not in the raise set

Faults propagate but are not tracked. Tracking them would put `catch (err)`
on every signature and produce nothing checkable.

### 3.3 `raises (…)` (E12)

A `service` function **exported by a package** may declare
`raises (NotFound, Conflict)`. The declaration is checked to be a superset of
the inferred set (`E1002`) and becomes that function's public contract.

Application code may not write `raises` (`E1003`) — the compiler already
knows, and a hand-written set drifts.

---

## 4. Exhaustiveness (E3–E6)

### 4.1 The boundary

Exactly one `errorHandler` per program (`E1010` on a second). It is where
every raise set is discharged:

```jwc
errorHandler (e) {
    catch PaymentDeclined (err) { return statusCode(402, { error: err.message, code: err.code }); }
    catch (err)                 { return internalError(); }
}
```

### 4.2 The rule (E3)

The union of all route, middleware and `after` raise sets must be covered by
`errorHandler` arms **or** by an error type's declared default status.

### 4.3 Defaults make the handler optional

An error with a declared status (§1.1, §1.2) needs no arm: the runtime
answers `<status> {"error": <message>}`. An error declared with no default —
not possible in 1.0's grammar, since `= <status>` is mandatory — would
require one.

The consequence is worth stating plainly: **the sample's eight-arm
`errorHandler` can be deleted entirely and the app behaves identically.**
The handler is for the cases where you want something other than the default,
like adding a field to the body.

### 4.4 Untyped `catch` catches only faults (E4)

`catch (err)` matches faults, never declared errors. It therefore never
satisfies §4.2 for a declared type, and it can no longer swallow a
`NotFund`-style typo — that is `E1001` at compile time now.

### 4.5 Unreachable arms (E5)

An arm for a type nobody raises is `W1001: unreachable arm`. A warning, not
an error: a package upgrade can legitimately remove a raise.

### 4.6 Arms must answer (E6)

Every `errorHandler` arm must end every path in `return <Response>`
(`E1011`).

---

## 5. `or throw` (E2, types §6.6)

```jwc
let account = select A from App.auth.Accounts where email == $req.email as { id, password_hash } first
    or throw Unauthorized("email yoki parol xato");
```

5.1 `x or throw E(…)` has type `T` when `x : T?`. It is the bind /
null-check / throw ritual as one expression, and it is why narrowing rule 4
exists.

5.2 `or throw` on a non-nullable operand is `W1002` (dead).

5.3 It contributes `E` to the enclosing raise set.

---

## 6. Constraint promotion (E10, #28, #30)

### 6.1 With a message

A constraint carrying `: "message"` raises a declared error when violated:

| Constraint | Error | Status |
|---|---|---|
| `unique` / partial unique | `Conflict` | 409 |
| `check` | `BadRequest` | 400 |
| a column rule — `minLength(2)`, `pattern(r"…")`, `min(0)`, … | `BadRequest` | 400 |

The marker goes after the rule, so a column can carry several:

```jwc no-compile
email varchar(255) unique : "bu email band", pattern(r"^[^@]+@[^@]+$") : "email yaroqsiz";
```

On a **class** field the same marker replaces the generated sentence in
the `validation_failed` body, so the two places a value is checked can say
the same thing.

The rule form was added because `W1302` pointed at a message-less
`pattern(...)` and advised "add `: \"…\"`" — advice that did not parse,
since only `unique` and the table-level forms took one. The specification's
own sample tripped the warning eleven times and could not act on it.

The message is the declared string. The generated constraint name
(schema §8) is **not** carried on the raised error: `Conflict` and
`BadRequest` take a message and nothing else, and widening them so that one
raise path can pass a second argument would put a schema identifier one
`err.constraint` away from a response body — which §6.2 refuses for exactly
that reason.

`ConstraintViolation(message, constraint)` is therefore a name an **author**
throws, with a constraint name they chose to expose. Nothing in the runtime
raises it. `jwc test` made that visible: the sample had a test asserting
`assert fails ConstraintViolation`, which nothing could ever satisfy.

409 for unique is a change from design.md's "constraint violations become
400": a duplicate is a conflict with existing state, and the sample's own
webhook case shows why the distinction is load-bearing — Stripe retries 4xx,
and a 409 with a stable body is the honest answer while a 400 reads as
"malformed, resend".

### 6.2 Without a message

A message-less constraint violation is a **fault** → 500 + log. This is
deliberate: the language cannot invent a client-facing sentence, and a
generic "constraint violated" leaks schema names to the client.

`jwc lint --constraints` lists every message-less constraint with its table
and columns, so the 500-producing set is enumerable rather than discovered in
production.

### 6.3 Foreign keys

An FK violation raises `BadRequest` 400 with the message
`"referenced row does not exist"`. FKs carry no per-constraint message in
1.0 (`DEFERRED-4`): the correct status varies by case (400 vs 404 vs 409) and
the data to choose does not exist yet.

### 6.4 Promotion feeds the raise set

Because §3.1 adds the constraints of every written table to the raise set, a
route that inserts into `Payments` statically raises the `provider_ref`
`Conflict`, and §4.2 covers it. Nothing is discovered at runtime.

### 6.5 Matching is by generated name

The runtime maps a Postgres `SQLSTATE 23xxx` to its message through the
constraint name (schema §8.1). Names are deterministic and independent of the
message text (schema §8.3), so editing a message never desynchronises the
map and never produces a migration.

---

## 7. Postfix `catch` — the one local recovery form (E8, E9, G6)

### 7.1 Shape

```jwc
let payment = insert into App.billing.Payments { ...$req } as { id }
    catch Conflict (err) { return { status: "duplicate" }; };
```

`<expr> catch E (err) <block>`. The block runs when evaluating `<expr>`
raises `E`.

### 7.2 The block must diverge (E8)

Every path through the block must `return`, `throw`, `break` or `continue`
(`E1020`). It cannot produce a substitute value.

`break` and `continue` are what make a retry loop expressible: a conflict on
a generated key is handled by going round again, and with only `return` and
`throw` available the handler could not stay inside the loop. Both are
`E0813` outside a `for` body.

This is the clause that keeps the language from growing a second error
channel: postfix `catch` handles-and-leaves, so a route cannot use it to
build result-plumbing, and the "routes contain no logic" rule survives.

### 7.3 Inside a transaction (E9)

A postfix `catch` inside a `transaction` block compiles to
`SAVEPOINT` before the guarded expression, `RELEASE SAVEPOINT` on success,
`ROLLBACK TO SAVEPOINT` before running the block. Mandatory: without it the
connection is in `25P02` and every subsequent statement in the transaction
fails.

### 7.4 It removes the caught type from the raise set

Per §3.1. That is how a service that handles its own duplicate stops
requiring an `errorHandler` arm for it.

---

## 8. The transaction boundary (G7)

`errorHandler` runs **after** rollback, outside the transaction, on a fresh
connection. `after` blocks run after `errorHandler`. Full ordering:

```
route/middleware raises
  → transaction ROLLBACK (if inside one), connection released
  → errorHandler arm runs, produces a Response
  → after blocks run in reverse order, seeing that Response's status
  → response written
```

---

## 9. Client input errors (N4)

Coercion failure on a client-derived value raises `BadRequest`
(types §7.2). Body validation raises `BadRequest` with a `fields` payload
(types §11.3) — E11: there is no other 400 path, and user code cannot
construct a `validation_failed` body.

Path-parameter parse failure is answered by the router **before** any user
code runs (routing §3.2) and is therefore not in any raise set.

---

## 10. Nested transactions (E13)

Detected at compile time over the call graph (writes §7.3, `E0620`). The
pre-1.0 runtime discovered this with a `bail!` at request time.

---

## 11. Logging

Every fault logs at `error` with: request id, method, resolved route
pattern (routing §5.4), the error chain, and the source location of the
`throw` or the failing statement. Declared errors log at `info` with the
same fields, without a stack.

Neither log ever contains a value from a `private` column (schema §3.1), a
request body field whose class field is `private`, or the `Authorization`
header.

---

## 12. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1001` | unknown error type in `throw` / `catch` |
| `E1002` | `raises` is not a superset of the inferred set |
| `E1003` | `raises` in application code |
| `E1004` | wrong number of arguments to an error constructor |
| `E1005` | error constructor argument is not assignable to the declared type |
| `E1010` | more than one `errorHandler` |
| `E1011` | `errorHandler` arm does not return a response |
| `E1020` | postfix `catch` block does not diverge |
| `E0813` | `break` or `continue` outside a `for` loop |
| `W1001` | unreachable `errorHandler` arm |
| `W1002` | `or throw` on a non-nullable operand |

## 13. Conformance

Each of E1–E14 carries at least two tests in
`tests/error_corpus/`. The mapping:

| # | Rule | Clause |
|---|---|---|
| E1 | declared names only | §1.1, §1.3 |
| E2 | raise set inference | §3.1 |
| E3 | exhaustiveness at the boundary | §4.2 |
| E4 | untyped catch = faults only | §4.4 |
| E5 | unreachable arm warning | §4.5 |
| E6 | arms must answer | §4.6 |
| E7 | `after` raise set is empty | middleware §5.5 |
| E8 | postfix catch diverges | §7.2 |
| E9 | savepoints in transactions | §7.3 |
| E10 | constraint promotion | §6 |
| E11 | validation is the only 400 body path | §9 |
| E12 | `raises` at the package boundary | §3.3 |
| E13 | nested transaction is static | §10 |
| E14 | middleware throws | §2.2 |

# types.md — the value lattice, nullability, and the input layer

Normative. Closes **N2** (scalar dictionary), **N3** (expression core),
**N4** (coercion on client input), **N11** (`now()` vs `date.now()`),
and gaps **#17**, **#19**, **#21**, **#22**, **#32**, **#35**, **#36**,
**#41**, **#42**.

---

## 1. The type universe

```
Type  ::= Scalar                       -- §2
        | Enum                         -- §3
        | Class                        -- §4
        | Record { field: Type, ... }  -- §5
        | Raw                          -- §5
        | Type '[]'                    -- array
        | Type '?'                     -- nullable, §6
        | Response                     -- §8
        | Void
```

A `Record` type is written inline where a signature needs one:

```jwc
function record_payment(req: WebhookPayment) -> { status: text } { … }
```

A `class` may not be used as a return annotation and a `view` may not be
used as a parameter annotation: classes are input shapes and views are
output shapes (§4.1, queries §8.1).

There is no function type, no type variable, and no user-defined generic.
All three are declared non-goals (ROADMAP §8).

---

## 2. Scalars

### 2.1 The dictionary

Every scalar usable in a `table`, `class`, `view` projection or `function`
signature. There are no others; a type name not in this table and not an
`enum`/`class` is `E0301: unknown type`.

| JWC | Postgres | JSON wire form | Notes |
|---|---|---|---|
| `smallint` | `smallint` | number | −2^15 … 2^15−1 |
| `int` | `integer` | number | −2^31 … 2^31−1 |
| `bigint` | `bigint` | **string** | §2.3 |
| `numeric(p,s)` | `numeric(p,s)` | **string** | exact decimal; money |
| `numeric` | `numeric` | **string** | unconstrained precision |
| `boolean` | `boolean` | `true` / `false` | |
| `varchar(n)` | `varchar(n)` | string | `n` is characters, not bytes |
| `text` | `text` | string | |
| `timestamptz` | `timestamptz` | RFC 3339, UTC, `Z` suffix, microseconds | `2026-08-19T07:21:44.120031Z` |
| `date` | `date` | `YYYY-MM-DD` | |
| `time` | `time` | `HH:MM:SS[.ffffff]` | no zone; `timetz` is not offered |
| `interval` | `interval` | ISO 8601 duration string | `P30D`, `PT10S` |
| `uuid` | `uuid` | canonical lowercase hyphenated | |
| `jsonb` | `jsonb` | the JSON value itself | §5.6 |
| `inet` | `inet` | string | `192.0.2.1`, `2001:db8::1/32` |
| `bytea` | `bytea` | **base64 string** (RFC 4648 §4) | |
| `T[]` | `T[]` | JSON array of `T`'s wire form | one dimension only |

`string` is **not** a type name. Use `text` or `varchar(n)`. Accepting both
spellings would put two names on one Postgres type and break the DBA test.

### 2.2 Literals

- An integer literal has type `int` if it fits, else `bigint`.
- A fractional literal has type `numeric`. It is never a binary float.
- A string literal has type `text`.
- `true`/`false` are `boolean`. `null` has type `Null` (§6.2).

### 2.3 `bigint` on the wire is a string — both paths (#42)

`bigint` and `numeric` serialise as JSON **strings**, on the raw path and
the record path alike, in every direction.

Rationale: ten of eleven sample tables key on `bigint identity`, and
JavaScript consumers silently corrupt anything above 2^53. Raw
(`row_to_json`) would emit exact digits, the record path would round; two
endpoints would print different ids for the same row. One representation,
chosen for the consumer that cannot handle the other.

Implementation obligations:

1. On the raw path the generated SQL casts: `to_jsonb(id::text)`, never
   `to_jsonb(id)`. This is why raw and record agree — raw is not "whatever
   Postgres does", it is a compiled projection (queries §7.2).
2. On input, a JSON string **or** a JSON number is accepted for a `bigint`
   field; a number that is not integral, or is outside `bigint`, is a
   validation failure (§11).
3. `9007199254740993` must round-trip byte-identically through both paths.
   This is a required conformance test (`tests/type_corpus/bigint_fidelity`).

`int` and `smallint` stay JSON numbers: they cannot exceed 2^53.

### 2.4 Time and the two clocks (N11)

There are two clocks and they are different values:

| Expression | Clock | Where it is legal |
|---|---|---|
| `default now()` on a column | **Postgres** `now()` — transaction start | schema only |
| `date.now()` | **application** — the process's wall clock, UTC | code only |

`now()` as a bare call in application code does **not** exist. It is
`E0302: 'now()' is a column default; in code write 'date.now()'`. The sample
called bare `now()` six times and computed billing periods from it; this
clause is why that is a compile error rather than a drift bug.

`date.now()` returns `timestamptz`. `date.days(n)`, `date.hours(n)`,
`date.minutes(n)`, `date.seconds(n)` return `interval` (builtins §3).

---

## 3. Enums

3.1 `enum E { a, b }` — no `of` clause. Physically a `varchar` column plus a
`CHECK (col IN ('a','b'))` constraint (schema §5.1).

3.2 `enum E of App.s { a, b }` — a real `CREATE TYPE App.s.e AS ENUM`
(schema §5.2).

3.3 Both are the same *language* type. Members are always qualified:
`MemberRole.owner`. A bare member name is `E0303`.

3.4 Wire form is the member name as a string, for both physical forms.

3.5 An enum value compares only against the same enum (`==`, `!=`) and
against `null` when nullable. Ordering comparisons (`<`) are `E0304`:
declaration order is not a documented total order and `of`-less enums do not
have one in the database at all.

---

## 4. Classes — the input layer

4.1 A `class` describes **request input only**. It is never a query result
type and never a response type. `as <Class>` on a query is not grammar.

4.2 Fields carry a scalar/enum/class type plus zero or more rules (§11).

4.3 `T?` on a class field means *may be absent or null* (§6.5).

4.4 `transient` marks a field with no corresponding column. Without it, a
class field that is spread into a table with no matching column is
`E0305` (§9.3).

4.5 A class field may be another class or an array of classes
(`lines InvoiceLineInput[]`). Nesting is validated recursively (§11.4).

---

## 5. `Raw` and `Record` — the total rule (#17, #41)

### 5.1 The two representations

- **`Raw`** — a JSON fragment produced by Postgres and forwarded to the
  response without being parsed. This is the performance promise.
- **`Record { f: T, ... }`** — a value with statically known fields.

### 5.2 Reading

Reading a field of a `Raw` value is `E0310: cannot read a field of a raw
result`, with a fix-it naming the projection to add. Indexing a `Raw` is the
same error.

### 5.3 Every producer, classified

This table is total. If a construct is not here, it does not produce a value.

| Producer | Type |
|---|---|
| `select B from T ... ` with no `as { }` | `Raw[]` |
| `select B from T ... first` with no `as { }` | `Raw?` |
| `select B from T ... as { … }` | `Record{…}[]` |
| `select B from T ... as { … } first` | `Record{…}?` |
| `insert into T { … }` with no `as { }` | `Void` |
| `insert into T { … } as { … }` | `Record{…}` |
| `update T … as { … }` | `Record{…}[]`, or `Record{…}?` with `first` |
| `update T …` with no `as { }` | `Void` |
| `delete from T … as { … }` | `Record{…}[]`, or `Record{…}?` with `first` |
| `delete from T …` with no `as { }` | `Void` |
| a `view` referenced as a source | as its declared `as { }` — always a `Record` shape |
| object literal `{ a: e }` | `Record{a: typeof e}` |
| array literal | `T[]` |
| `jsonb` column read in a projection | `Raw` |
| `context.get(k)` | the type declared by the `provides` clause that set it (middleware §6) |
| `request.body() as C` | `C` |
| `request.raw_body()` | `text` |
| `request.query(k)` / `request.header(k)` | `text?`, client-derived (§7) |
| `jwt.verify(t, s)` | `Record{sub: text, exp: bigint, iat: bigint}?` |
| every other builtin | as declared in builtins.md |

**A `view` is a named projection.** Because a view's body must carry
`as { }` (queries §8.1), selecting from a view yields a `Record`, not `Raw`.
That is the clause that made the sample's `MemberAccess` gate legal; it was
illegal under the old rule, and gaps.md #17 found it.

### 5.4 Raw composition is a text splice (#41)

A `Raw` value may appear as a **field value in an object literal**:

```jwc
return json({ items: $rows, next: $cursor });   -- $rows : Raw[]
```

The compiler emits the surrounding object by string concatenation and splices
the raw bytes in. It does **not** parse and re-serialise. `Raw` therefore
survives envelope construction, which is what makes keyset pagination
(queries §9) compatible with the raw fast path.

`Raw` in any other position — an operand of `+`, a comparison, a function
argument other than `json(...)`, a `for` subject, an array element — is
`E0311: raw value used where a parsed value is required`.

### 5.5 The `raw lost here` diagnostic

`jwc explain --raw` prints, per query, either `raw preserved` or
`raw lost here: <construct> at file:line`. Losing raw is legal and common;
being unable to see that you lost it is the gap. This is a report, not an
error.

### 5.6 `jsonb`

A `jsonb` column read through a projection is `Raw` (§5.3) — it splices. A
`jsonb` value written from code takes any `Record`, array, scalar or `Raw`.
There is no navigation into `jsonb` in 1.0 (`DEFERRED-6`).

---

## 6. Nullability (#19)

### 6.1 `T?`

`?` is a type constructor, not a DDL marker. `T` and `T?` are different
types everywhere: columns, class fields, parameters, locals, projections,
returns.

### 6.2 `null`

`null` has type `Null`, which is a subtype of every `T?` and of no `T`.

### 6.3 Where `?` is introduced

| Construct | Result |
|---|---|
| column declared `col T?` | `T?` |
| `first` | `T` → `T?` |
| `left join … as one a` | `a : Record{…}?` |
| `inner join … as one a` | `a : Record{…}` |
| `… as many xs` | `xs : Record{…}[]` — empty array, never null |
| `count(x)` | `int` — never null |
| `sum` / `min` / `max` / `avg` | `T?` — null over an empty group |
| `request.query(k)` etc. | `text?` |
| `?? ` right operand non-null | strips `?` (§6.6) |

`right join` and `full join` are not grammar (grammar.ebnf `join_kind`):
they would make the *driving* binding nullable, which inverts the projection
tree for no expressiveness the sample or DESIGN.md asks for. Use `left join`
with the sides swapped.

### 6.4 Using a `T?`

Reading a field of, calling a method on, or passing a `T?` where `T` is
required is `E0320: value may be null`. `json(x)` with `x : T?` is also
`E0320` — a route that answers `200 null` where it means 404 is the exact
bug #19 names.

### 6.5 Absent vs null on class fields

A class field `f T?` has three input states, and they are distinguishable:

| Input | State |
|---|---|
| key not present in the JSON body | **absent** |
| `"f": null` | **null** |
| `"f": v` | **present** |

A field `f T` (no `?`) that is absent or null is a validation failure
(§11). The distinction matters only for spread (§9.2).

### 6.6 Flow narrowing

Inside a block, a local of type `T?` narrows to `T` after a guard that
**diverges** on the null branch:

```jwc
let account = select A from App.auth.Accounts … first;   -- Record{…}?
if ($account == null) { throw NotFound("akkaunt topilmadi"); }
-- account : Record{…} from here to end of block
```

The narrowing rules, exhaustively:

1. `if (x == null) { <divergent> }` — narrows `x` after the `if`.
2. `if (x != null) { … }` — narrows `x` inside the then-branch.
3. `x ?? d` — the whole expression is non-null when `d` is non-null.
4. `x or throw E(...)` — the expression is `T` (errors §5).
5. Assignment to `x` resets narrowing to `x`'s declared type.
6. Narrowing does not cross a function boundary or a `for` body.

A guard is **divergent** if every path through it ends in `return`, `throw`,
`break` or `continue`.

`??` is defined only for `T? ?? T` and `T? ?? T?`. `T ?? …` on a non-null
left operand is `W0301` (dead coalesce).

---

## 7. Client-derived values and coercion (N4)

### 7.1 The taint

A value is **client-derived** if it originates from `request.body()`,
`request.query()`, `request.header()`, `request.raw_body()`, `request.path()`,
a path parameter `@x`, or any expression with a client-derived operand. The
property propagates through `let`, through object literals, and through
function calls (a parameter is client-derived if any reachable call site
passes a client-derived argument — a call-graph fixpoint, computed with the
same machinery as the raise set in errors §3).

### 7.2 The rule

The coercion builtins `int(x)`, `bigint(x)`, `numeric(x)`, `boolean(x)`,
`uuid(x)`, `timestamptz(x)` fail differently by source:

- on a **client-derived** operand, failure raises `BadRequest` → **400**;
- otherwise, failure is a **fault** → 500 + log.

```jwc
let limit = int(request.query("limit") ?? "50");   -- ?limit=abc → 400
let port  = int(env("PORT") ?? "8080");            -- bad env → 500 at boot
```

No new syntax. The rejected alternative — an explicit `int?()` form — taxes
100% of call sites to fix the 1% that read client input, and a developer who
forgets it gets the 500 anyway. The rejected alternative is recorded, not
silently dropped: `DEFERRED-1`.

### 7.3 Signature verification clears the taint

The result of `jwt.verify(token, secret)` is **not** client-derived, in
whole or in part: a value that passed signature verification was produced by
this server. `bigint($claims.sub)` on a verified token therefore fails as a
fault, which is correct — a non-numeric `sub` in a token we signed is our
bug, not the caller's.

The same applies to `hash.hmac_verify`-gated bodies only when the body is
re-read *after* the check; the language does not track that, so webhook
payloads stay client-derived and their coercion failures stay 400. That is
the safe direction.

### 7.4 What taint does *not* do

It does not restrict where a value may be used, it does not appear in
signatures, and it is not surfaced in the type. It selects an error class and
nothing else.

---

## 8. `Response`

`json(v)`, `created(v)`, `noContent()`, `notFound(m)`, `badRequest(v)`,
`unauthorized(m)`, `forbidden(m)`, `statusCode(n, v)` and `redirect(n, url)`
produce `Response`. `Response with { … }` is still `Response` (routing §7).

A `Response` may only be the operand of `return` in a route, middleware or
`errorHandler` arm. Returning a `Response` from a `service` function is
`E0330` — services do not know HTTP (errors §2.1).

---

## 9. Spread (#21, #36, #7)

### 9.1 Preconditions

`...x` requires `x` to have a **statically known field set**: a `class`
value, a `Record`, or a class-typed parameter. `...request.body()` without
`as C` is `E0340: spread source has no declared shape`. `...raw` is `E0311`.

`...x except a, b` removes named fields. Naming a field that does not exist
is `E0341`.

### 9.2 Absent, null, present

For each field of the spread source, in an `insert` object literal or a
`set` clause:

| State | `insert` | `update set` |
|---|---|---|
| absent | column omitted → its `default` applies | `SET` item omitted |
| null | column set to `NULL` | `SET col = NULL` |
| present | column set to the value | `SET col = value` |

This is what `=?` does for a single column, applied field-wise. There is now
no case where spread erases data it was not given.

### 9.3 Field/column intersection is checked, not silent

Every non-`transient` field of the spread source must have a column in the
target table with a **compatible type** (§10.3). A field with no column is
`E0305`, fixed by `transient` on the field or `except` at the site.

The reverse is not required: a table column with no field simply keeps its
default.

### 9.4 `private` and `server` are unreachable through spread (#35)

A column marked `private` or `server` (schema §3) is removed from the
INSERT/UPDATE column list produced by a spread, always, and a class field
that names one is `E0342`. Mass assignment is closed at the language level,
not by discipline.

### 9.5 Empty spread (#7)

If every field of the spread source is absent:

- `insert into T { ...req }` inserts a row of defaults. If any non-defaulted
  `NOT NULL` column is left unset, that is a compile error (`E0343`), not a
  runtime failure.
- `update T set ...req where … as { … } first` **skips the UPDATE** and
  performs the equivalent `select … as { … } first` instead. The route
  answers 200 with the current row. No empty `SET` clause is ever emitted.
- `update T set ...req where …` with no projection is a no-op returning
  `Void`.

---

## 10. Signatures

### 10.1 Parameters

Service and free-function parameters **must** be annotated (`E0350`). This
is what makes cross-file checking possible at all (#31).

### 10.2 Returns

A return annotation (`-> T`) is optional and inferred from the function's
`return` statements when omitted, **unless**:

- two `return` statements produce incompatible shapes — then the annotation
  is mandatory (`E0351`). `WebhookService.record_payment` returning
  `{status: "duplicate"}` on one path and `{status: "ok"}` on another is
  compatible; returning a `Record{id, …}` on one path and `{status}` on
  another is not.
- the function is exported by a package (`E0352`).

### 10.3 Assignability

`S` is assignable to `T` when:

1. `S == T`; or
2. `S` is `Null` and `T` is `U?`; or
3. `S` is `U` and `T` is `U?`; or
4. both are `Record` and `S` has every field of `T` with an assignable type
   (width subtyping; extra fields in `S` are dropped at the boundary); or
5. `S` and `T` are numeric and `S` widens without loss:
   `smallint → int → bigint → numeric`.

Narrowing (`bigint → int`) is never implicit. `Raw` is assignable only to
`Raw`.

---

## 11. Class validation and the 400 contract (#32)

### 11.1 Rules

| Rule | Applies to | Meaning |
|---|---|---|
| `required` | any | field must be present and non-null |
| `minLength(n)` / `maxLength(n)` | `varchar`, `text` | characters |
| `minItems(n)` / `maxItems(n)` | `T[]` | elements |
| `min(n)` / `max(n)` | numeric, `timestamptz`, `date` | inclusive bound |
| `pattern(r"…")` | `varchar`, `text` | anchored implicitly? **no** — the regex is used as written |
| `oneOf(a, b, …)` | scalars | membership |

`minLength` on an array is `E0360`. It was the overload gaps.md #32 named;
arrays use `minItems`. A `T?` field with no `required` is optional; adding
`required` to a `T?` field is `E0361` (contradiction).

### 11.2 All errors are collected

Validation does not stop at the first failure. Every failing field of every
element is reported.

### 11.3 The response body is fixed

```json
{
  "error": "validation_failed",
  "fields": [
    { "path": "lines[2].quantity", "rule": "min", "limit": 1,
      "message": "quantity kamida 1 bo'lishi kerak" }
  ]
}
```

- `path` is a JSON-pointer-like dotted path with `[i]` for array indices.
- `rule` is the rule name verbatim from §11.1.
- `limit` is present for rules that carry a bound; absent otherwise.
- `message` is the localised default, or the rule's `: "…"` override.
- Status is **400**. Content type is `application/json`.

This shape is normative; a route cannot produce a different validation body,
because validation is not reachable from user code (errors §4, E11).

### 11.4 Nesting

`lines InvoiceLineInput[] required, minItems(1)` validates the array bound
first, then every element, accumulating into the same `fields` array with
indexed paths.

### 11.5 Unknown keys

Keys in the JSON body with no matching class field are **dropped silently**.
The class is the whitelist (design.md); rejecting unknown keys would break
every client that adds a field. This is the answer to the first of #36's
three unstated rules.

---

## 12. Expression core (N3)

### 12.1 `+`

| Left | Right | Result |
|---|---|---|
| numeric | numeric | numeric, widened per §10.3 |
| `text`/`varchar` | `text`/`varchar` | `text` |
| `timestamptz` | `interval` | `timestamptz` |
| `date` | `interval` | `timestamptz` |
| `interval` | `interval` | `interval` |

Any other combination is `E0370`. In particular `text + int` does **not**
coerce; write `string.concat(a, string.of(b))`.

### 12.2 `-`, `*`, `/`, `%`

Numeric only, plus `timestamptz - timestamptz → interval` and
`timestamptz - interval → timestamptz`. `/` on two integers is **integer
division**; `/` with a `numeric` operand is exact division. `%` is integer
only. Division by zero is a fault.

### 12.3 Integer width and overflow

`int op int → int`. `smallint op smallint → int`. Any `bigint` operand
makes the result `bigint`. Any `numeric` operand makes it `numeric`.

Overflow of the result type is a **fault** (500), never a wrap. Money is
`numeric`; the sample's `quantity * unit_cents` summed over lines is
`int * int → int` and can overflow, which is why §12.5 exists.

### 12.4 There is no truthiness

The condition of `if`, `and`, `or`, `!` and the ternary must be `boolean`.
`if ($x)` where `x : text?` is `E0371`. Write `if ($x != null)`.

### 12.5 Aggregating an array in code (#22)

Lambdas do not exist. The replacement is a fixed set of array builtins that
take **field names as strings**:

```jwc
let total = array.sum_product($req.lines, "quantity", "unit_cents");
let n     = array.len($req.lines);
let ids   = array.pluck($rows, "id");
```

`array.sum_product` returns `numeric` — the width question of §12.3 is
answered by the builtin, not by the call site. Full list: builtins §5.

### 12.6 Comparison

`==` and `!=` are defined for two values of the same type, or `T` vs `Null`.
`<`, `<=`, `>`, `>=` are defined for numerics, `text` (collation
`C` — byte order, stated so the DBA test holds), `timestamptz`, `date`,
`time`, `interval`, `uuid`. Not for `boolean`, `enum` (§3.5), `jsonb`,
`Record` or `Raw`.

Comparing `T?` with `<` is `E0320`. Comparing with `==` against `null` is
how you test nullity and is always allowed.

---

## 13. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0301` | unknown type |
| `E0302` | bare `now()` in code |
| `E0303` | unqualified enum member |
| `E0304` | ordering comparison on an enum |
| `E0305` | class field has no matching column |
| `E0310` | field read on a raw value |
| `E0311` | raw value in a non-splice position |
| `E0320` | value may be null |
| `E0330` | `Response` returned from a service |
| `E0340` | spread source has no declared shape |
| `E0341` | `except` names a field that does not exist |
| `E0342` | class field names a `private`/`server` column |
| `E0343` | empty spread leaves a `NOT NULL` column unset |
| `E0350` | missing parameter annotation |
| `E0351` | incompatible return shapes without an annotation |
| `E0352` | package-exported function without a return annotation |
| `E0360` | `minLength` on an array |
| `E0361` | `required` on a `T?` field |
| `E0370` | no `+` overload for these operands |
| `E0371` | non-boolean condition |
| `W0301` | dead `??` |

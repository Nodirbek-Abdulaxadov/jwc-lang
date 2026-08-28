---
sidebar_position: 2
title: "Diagnostic codes"
description: "Every E and W code the compiler can produce, what each means, and which spec file defines it."
---

# Diagnostic codes

Every diagnostic carries a code. The code is stable: it is what you search
for, what an editor keys off, and what `--explain` looks up.

```bash
jwc lint --explain E0211     # one code
jwc lint --list-codes        # all of them
```

Both read no sources and answer outside a project — you look a code up
when you have one in front of you, not when you have a checkout.

## The bands

| Band | What it is about | Spec |
|---|---|---|
| `E00xx` | the parser: a token where another was expected | `names.md` |
| `E01xx` | the lexer: characters, literals, escapes, sigils | `names.md` |
| `E02xx` | names — imports, bindings, locals, `const` | `names.md` |
| `E03xx` | types | `types.md` |
| `E04xx` | schema: columns, keys, indexes, constraints | `schema.md` |
| `E05xx` | queries | `queries.md` |
| `E06xx` | writes | `writes.md` |
| `E07xx` | routing | `routing.md` |
| `E08xx` | middleware and statements | `middleware.md` |
| `E09xx` | words from the pre-1.0 language | `names.md` |
| `E10xx` | the error model | `errors.md` |
| `E11xx` | migrations | `migrations.md` |
| `E12xx` | `server { }`, `init()` and configuration | `config.md` |
| `E14xx` | `test` blocks | `testing.md` |
| `E15xx` | packages | `packages.md` |
| `W…` | warnings — true, and not fatal | across all of them |

A code you mistyped still lands you in the right band: `jwc lint --explain
E0299` lists every `E02` code rather than saying nothing.

## The table

The rows below are extracted from the normative specification under
`docs/spec/v1/`. That is the definition; this page and `--explain` read
the same extraction, so neither can drift from it.

<!-- generated:diagnostic-table -->
| Code | Meaning | Defined in |
|---|---|---|
| `E0001` | any construct — expected a specific token, name, integer or string literal | `names.md` |
| `E0002` | the top level — expected a declaration | `names.md` |
| `E0003` | the top level — `route` outside a `routes` block | `names.md` |
| `E0004` | a foreign key — expected `delete`/`update` after `on`, or a referential action | `names.md` |
| `E0005` | an `error` declaration — expected an HTTP status code, or one outside `100..=599` | `names.md` |
| `E0006` | a `service` body — expected `function` — a service holds nothing else | `names.md` |
| `E0007` | a `middleware` binder — expected `@name` | `names.md` |
| `E0008` | a `routes` body — expected `route` or `socket` — blocks do not nest | `names.md` |
| `E0009` | a `route` header — not an HTTP method | `names.md` |
| `E0010` | an `errorHandler` body — expected `catch` — a handler holds nothing else | `names.md` |
| `E0011` | a type — expected a type argument, as in `varchar(120)` | `names.md` |
| `E0012` | an object literal — expected `$name` after `...` | `names.md` |
| `E0013` | an object literal — expected an object key, or `:`/`=` after one | `names.md` |
| `E0014` | an expression — expected an expression | `names.md` |
| `E0015` | a query — `select` with no binder before `from` | `names.md` |
| `E0016` | a join — expected `left` or `inner` — `right`/`full`/`cross` are not grammar | `names.md` |
| `E0017` | a join result — expected `one`, `many` or `group` after `as` | `names.md` |
| `E0018` | a spread — `except` without a parenthesised list | `names.md` |
| `E0019` | a `socket` member that is not `on open` / `on message (m)` / `on close` | `routing.md` |
| `E0020` | the same `on` handler declared twice | `routing.md` |
| `E0021` | a `socket` with no handlers at all | `routing.md` |
| `E0022` | `retries N` outside `1..=100` | `jobs.md` |
| `E0100` | unexpected character | `names.md` |
| `E0102` | unterminated string | `names.md` |
| `E0103` | literal newline inside a string literal | `names.md` |
| `E0104` | doc comment attaches to nothing | `names.md` |
| `E0105` | identifier starts with `_` | `names.md` |
| `E0106` | `$` or `@` not followed immediately by a name | `names.md` |
| `E0107` | integer literal out of `bigint` range | `names.md` |
| `E0108` | `\u` not followed by `{XXXX}` | `names.md` |
| `E0109` | unknown string escape | `names.md` |
| `E0110` | physical name collision | `names.md` |
| `E0111` | duplicate declared name | `names.md` |
| `E0201` | unknown import | `names.md` |
| `E0202` | reference to a name in an un-imported namespace | `names.md` |
| `E0203` | import is both a namespace and a package | `names.md` |
| `E0204` | unknown function | `names.md` |
| `E0205` | wrong number of arguments to a builtin | `builtins.md` |
| `E0206` | a field name in `json.get`-style access is not a string literal | `builtins.md` |
| `E0210` | bare identifier in a query clause is not a column but matches a local | `names.md` |
| `E0211` | unknown name: not a column here, and not a local or declaration | `names.md` |
| `E0212` | duplicate binding name in one query | `names.md` |
| `E0213` | unqualified column is ambiguous across bindings | `names.md` |
| `E0214` | `let` shadows an existing binding | `names.md` |
| `E0215` | two `const` declarations with one name | `names.md` |
| `E0216` | a `const` right-hand side is not a constant expression | `names.md` |
| `E0220` | `@name` outside a route or middleware | `names.md` |
| `E0225` | `socket.*` outside a socket handler | `routing.md` |
| `E0230` | `file.*` / `directory.*` outside a plain `function` (§7e.1) | `builtins.md` |
| `E0301` | unknown type | `types.md` |
| `E0302` | bare `now()` in code | `types.md` |
| `E0303` | unqualified enum member | `types.md` |
| `E0304` | ordering comparison on an enum | `types.md` |
| `E0305` | class field has no matching column | `types.md` |
| `E0310` | field read on a raw value | `types.md` |
| `E0311` | raw value in a non-splice position | `types.md` |
| `E0312` | field read on a value that has no such field | `types.md` |
| `E0320` | value may be null | `types.md` |
| `E0330` | `Response` returned from a service | `types.md` |
| `E0340` | spread source has no declared shape | `types.md` |
| `E0341` | `except` names a field that does not exist | `types.md` |
| `E0342` | class field names a `private`/`server` column | `types.md` |
| `E0343` | empty spread leaves a `NOT NULL` column unset | `types.md` |
| `E0351` | incompatible return shapes without an annotation | `types.md` |
| `E0352` | package-exported function without a return annotation | `types.md` |
| `E0353` | wrong number of arguments to a declared function | `types.md` |
| `E0354` | argument is not assignable to the declared parameter type | `types.md` |
| `E0360` | `minLength` on an array | `types.md` |
| `E0361` | `required` on a `T?` field | `types.md` |
| `E0362` | a job parameter that is not a scalar or an array of scalars | `jobs.md` |
| `E0363` | two `job`s with one name | `jobs.md` |
| `E0364` | `dispatch` of an undeclared job | `jobs.md` |
| `E0365` | `dispatch` inside a job body | `jobs.md` |
| `E0366` | a parameter given twice at a dispatch site | `jobs.md` |
| `E0367` | a dispatch argument of the wrong type | `jobs.md` |
| `E0368` | a dispatch argument the job does not declare | `jobs.md` |
| `E0369` | a non-optional parameter left out | `jobs.md` |
| `E0370` | no `+` overload for these operands | `types.md` |
| `E0371` | non-boolean condition | `types.md` |
| `E0372` | `for` over something that is not an array | `types.md` |
| `E0373` | index is not a number | `types.md` |
| `E0374` | the two branches of a conditional produce unrelated shapes | `types.md` |
| `E0375` | `in` over an array whose element type does not match | `types.md` |
| `E0376` | operands cannot be compared or ordered | `types.md` |
| `E0401` | `identity` on a non-integer column | `schema.md` |
| `E0402` | non-constant `default` | `schema.md` |
| `E0403` | `default now()` on a non-temporal column | `schema.md` |
| `E0410` | `private` column in a projection or view | `schema.md` |
| `E0420` | both column-level and table-level primary key | `schema.md` |
| `E0421` | FK column count mismatch | `schema.md` |
| `E0422` | FK target is not a PK or unique | `schema.md` |
| `E0423` | `on delete set null` on a `NOT NULL` column | `schema.md` |
| `E0424` | unsupported function in a `check` | `schema.md` |
| `E0430` | `on update` expression other than `now()` | `schema.md` |
| `E0431` | unknown index access method, or GIN on a scalar | `schema.md` |
| `E0440` | `NOT NULL` column added with no default | `schema.md` |
| `E0450` | unknown schema | `schema.md` |
| `E0451` | index or constraint names a column the table does not have | `schema.md` |
| `E0452` | `required` used as a column modifier | `schema.md` |
| `E0453` | unknown column rule | `schema.md` |
| `E0501` | query clause out of order | `queries.md` |
| `E0502` | source is not a table or view | `queries.md` |
| `E0503` | `==?` on a non-nullable operand | `queries.md` |
| `E0510` | ambiguous join attachment — add `under <binding>` | `queries.md` |
| `E0511` | `under` names no binding in this query | `queries.md` |
| `E0520` | `first` without `orderby` on a non-unique predicate | `queries.md` |
| `E0521` | `orderby` on an `as many` field | `queries.md` |
| `E0530` | aggregate outside a grouped projection | `queries.md` |
| `E0531` | non-aggregate projection field missing from `group by` | `queries.md` |
| `E0532` | bare-join aggregation combined with `as many` | `queries.md` |
| `E0533` | aggregate filter on a non-aggregate call | `queries.md` |
| `E0534` | field read on a binding that is not a join result | `queries.md` |
| `E0535` | join does not say what it produces | `queries.md` |
| `E0536` | `as many` without `orderby` | `queries.md` |
| `E0540` | view body has no projection | `queries.md` |
| `E0541` | view body carries a per-query clause | `queries.md` |
| `E0542` | pagination pushdown cannot be proven | `queries.md` |
| `E0550` | `page` without a total order | `queries.md` |
| `E0601` | write targets a view | `writes.md` |
| `E0602` | unknown column in a write | `writes.md` |
| `E0603` | `on conflict` columns are not a unique constraint | `writes.md` |
| `E0604` | `on conflict` without columns on a multi-unique table | `writes.md` |
| `E0605` | `update`/`delete` with no `where` | `writes.md` |
| `E0606` | value is not assignable to the column it is written to | `writes.md` |
| `E0610` | `raw` placeholder/argument count mismatch | `writes.md` |
| `E0611` | `raw` inside a view | `writes.md` |
| `E0612` | `buffered` inside a `transaction { }` | `writes.md` |
| `E0613` | `on conflict` on a buffered insert | `writes.md` |
| `E0614` | `as { … }` on a buffered insert | `writes.md` |
| `E0620` | nested transaction | `writes.md` |
| `E0621` | `transaction` outside a service | `writes.md` |
| `E0701` | path parameter slot disagrees on name or type | `routing.md` |
| `E0710` | duplicate `(method, path)` | `routing.md` |
| `E0711` | route is fully shadowed | `routing.md` |
| `E0720` | `request.body()` without `as C` | `routing.md` |
| `E0730` | duplicate key in `with { }` | `routing.md` |
| `E0731` | route path does not end in a response | `routing.md` |
| `E0732` | route returns a non-`Response` | `routing.md` |
| `E0733` | a header value is not text | `routing.md` |
| `E0734` | `response.status()` outside an `after` block | `routing.md` |
| `E0735` | `content(...)` media type is not a string literal | `routing.md` |
| `E0736` | `content(...)` body is not `text` | `routing.md` |
| `E0737` | unknown key in a `cookie(...)` options record | `routing.md` |
| `E0738` | a cookie attribute of the wrong type, or a `same_site` that is not `Strict` / `Lax` / `None` | `routing.md` |
| `E0739` | `same_site: "None"` without `secure: true` | `routing.md` |
| `E0740` | a `static` prefix is not a literal path beginning with `/` | `routing.md` |
| `E0741` | a `static` root is missing, or is not a directory | `routing.md` |
| `E0742` | two `static` mounts on one prefix | `routing.md` |
| `E0743` | a `static` `cache` value is not a number of seconds within the ceiling | `routing.md` |
| `E0744` | a `static` root is outside the project | `routing.md` |
| `E0745` | `redirect` given a literal target that leaves this service | `routing.md` |
| `E0801` | undeclared `@name` in a middleware | `middleware.md` |
| `E0802` | attachment site lacks a declared binder | `middleware.md` |
| `E0803` | `requires` not satisfied by the resolved chain | `middleware.md` |
| `E0804` | middleware appears twice in one chain | `middleware.md` |
| `E0805` | `uses` names something that is not a declared middleware | `middleware.md` |
| `E0810` | `return <expr>` inside `after` | `middleware.md` |
| `E0811` | `after` block can raise | `middleware.md` |
| `E0812` | bare `return;` in a middleware — it answers 204 | `middleware.md` |
| `E0813` | `break` or `continue` outside a `for` loop | `errors.md` |
| `E0814` | `return <value>` inside a socket handler | `routing.md` |
| `E0820` | `context.k` is not provided on every path | `middleware.md` |
| `E0821` | `context.k = …` without a `provides` declaration | `middleware.md` |
| `E0900` | removed keyword from the pre-1.0 language | `names.md` |
| `E1001` | unknown error type in `throw` / `catch` | `errors.md` |
| `E1002` | `raises` is not a superset of the inferred set | `errors.md` |
| `E1003` | `raises` in application code | `errors.md` |
| `E1004` | wrong number of arguments to an error constructor | `errors.md` |
| `E1005` | error constructor argument is not assignable to the declared type | `errors.md` |
| `E1010` | more than one `errorHandler` | `errors.md` |
| `E1011` | `errorHandler` arm does not return a response | `errors.md` |
| `E1020` | postfix `catch` block does not diverge | `errors.md` |
| `E1101` | `no-transaction` migration contains other statements | `migrations.md` |
| `E1102` | enum value removal or reorder | `migrations.md` |
| `E1103` | `was` names something not in the snapshot | `migrations.md` |
| `E1104` | rename combined with a type change | `migrations.md` |
| `E1201` | I/O or query inside `init()` | `config.md` |
| `E1202` | unknown `init()` key | `config.md` |
| `E1203` | more than one `database` | `config.md` |
| `E1204` | more than one `server` block | `config.md` |
| `E1205` | `page` used with no `cursor_secret` | `config.md` |
| `E1206` | unknown `server { }` key, or unknown key inside its `cors` / `tls` / `headers` block | `config.md` |
| `E1207` | `cors { origins = ["*"] }` together with `credentials = true` | `config.md` |
| `E1401` | `assert fails` without an error type | `testing.md` |
| `E1402` | `with` on an `assert fails` whose message cannot be a literal | `testing.md` |
| `E1501` | a package declares a schema object | `packages.md` |
| `E1502` | a package declares `routes` or an `errorHandler` | `packages.md` |
| `W0101` | case convention | `names.md` |
| `W0102` | namespace does not match file path | `names.md` |
| `W0103` | unused import | `names.md` |
| `W0104` | comparison is always true | `names.md` |
| `W0301` | dead `??` | `types.md` |
| `W0401` | table has no primary key | `schema.md` |
| `W0501` | unbounded `as many` | `queries.md` |
| `W0502` | `count` under fan-out — did you mean `count.distinct`? | `queries.md` |
| `W0602` | `request.path()` in a rate-limit key | `routing.md` |
| `W0801` | middleware returns an error response instead of throwing | `middleware.md` |
| `W1001` | unreachable `errorHandler` arm | `errors.md` |
| `W1002` | `or throw` on a non-nullable operand | `errors.md` |
| `W1101` | stale `was` marker | `migrations.md` |
| `W1301` | `debug.dump` in the program (tooling §3.4) | `builtins.md` |
| `W1302` | a `unique` or foreign key reachable from a route carries no message | `tooling.md` |
| `W1501` | an exported function can raise and declares no `raises` | `packages.md` |
<!-- /generated:diagnostic-table -->

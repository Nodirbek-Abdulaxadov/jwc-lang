# packages.md — what a package is, and what it may contain

Normative. Closes gap **N8**.

---

## 1. The manifest

A project is described by `jwcproj.json` at its root:

```json
{
  "name": "redis",
  "version": "0.1.0",
  "type": "pkg",
  "dependencies": { "redis": "^0.1.0" }
}
```

1.1 `type` is `"app"` (the default) or `"pkg"`. An app is deployed; a
package is imported.

1.2 A package name matches `^[a-z][a-z0-9_-]{0,63}$` **and must also be a
legal identifier**, because `import redis;` puts the name in the source.
A hyphen is therefore accepted by the registry and unusable in a program;
`jwc publish` refuses one.

---

## 2. What a package may declare

| Declaration | In a package |
|---|---|
| `service` | **yes** — its exported surface |
| `middleware` | **yes** |
| `class` | **yes** — request and response shapes |
| `error` | **yes** |
| `enum` without `of` | **yes** — a `varchar` plus a check, no type to create |
| `function` (free) | **yes** — internal |
| `test` | **yes** |
| `database`, `schema`, `table`, `view`, `enum … of` | **no** — `E1501` |
| `routes`, `errorHandler` | **no** — `E1502` |

2.1 The line is **migrations** (`E1501`). A package that declares a table
brings DDL with it, and installing a dependency would mean applying someone
else's schema change to your database. There is no version of that which is
safe: two packages can want the same table name, a package upgrade becomes a
migration you did not write, and `jwc migrate new` would have to diff
against sources you do not control. A package that needs storage takes a
table name as a parameter, or asks the application to declare it.

2.2 `routes` and `errorHandler` are the application's (`E1502`). Mounting is
a decision about a URL space the package cannot see, and errors §4.1 allows
exactly one `errorHandler` per program — a package carrying one would make
importing two packages a compile error about a construct neither author
wrote.

2.3 An `enum` **without** `of` is a `varchar` plus a check constraint
(schema §5) and creates no type, so it is allowed. The `of` form creates a
Postgres type and is not.

---

## 3. The export boundary

3.1 Everything in a package's `service` blocks is exported. There is no
`public` marker: a service *is* the boundary (types §10).

3.2 An exported function's `raises` clause is its error contract.
Application code may not write `raises` (`E1003`) — the compiler infers it
there — but a package must, because a consumer compiles against the
declaration and not against the body.

3.3 The declared set must be a **superset** of the inferred one (`E1002`).
Narrowing is refused: a caller who handles exactly what the declaration
names would otherwise meet an error nothing told them about.

Widening is allowed. A package may declare an error it does not raise yet,
which is how a raise set stays stable across a minor version.

3.4 An exported function that can raise and declares nothing is `W1501`.
It compiles — the compiler still knows the set — but the package's consumers
read the declaration, and an absent one silently becomes "raises nothing".

---

## 4. Imports

4.1 `import <name>;` resolves to a namespace declared in this program, or
to a dependency in the manifest (names §6.2.1). Both, or neither, is an
error (`E0203` / `E0201`).

4.2 A package's exports are reached through its name: `redis.get(k)`. There
is no `use`-style unqualified import — a bare name in a program should
always be resolvable without knowing which packages are installed.

---

## 5. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1501` | a package declares a schema object |
| `E1502` | a package declares `routes` or an `errorHandler` |
| `W1501` | an exported function can raise and declares no `raises` |

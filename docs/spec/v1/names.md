# names.md — lexical structure, modules, name resolution

Normative. Clause references elsewhere in the spec use the form `names §3.2`.

Closes gaps **#2**, **#18**, **#34** (bare identifiers in `where`), **N5**
(`import` semantics, undeclared free functions), and the naming half of
**#35**.

---

## 1. Source text

1.1 A source file is UTF-8. A BOM, if present, is skipped.

1.2 Line terminators are LF or CRLF; both count as one line for diagnostics.

1.3 Whitespace separates tokens and is otherwise insignificant.

1.4 A **line comment** starts with `--` and runs to end of line.

1.5 A **doc comment** starts with `---` and runs to end of line. Consecutive
doc-comment lines form one doc block. A doc block attaches to the immediately
following declaration, column definition or class field. A doc block followed
by anything else is an error (`E0104: doc comment attaches to nothing`).
Doc blocks on tables and columns become `COMMENT ON` (schema §7).

1.6 `--` inside a string literal is text, not a comment.

---

## 2. Tokens

2.1 **Identifier** — `[A-Za-z][A-Za-z0-9_]*`. Identifiers are
case-sensitive. A leading `_` is not permitted (it is reserved for compiler
temporaries in generated SQL).

2.2 **Number** — decimal integer or decimal fraction: `[0-9]+(\.[0-9]+)?`.
No underscores, no hex, no exponent. An integer literal that does not fit
`bigint` is an error (`E0107`). A fractional literal is `numeric`, never a
binary float (types §2.4).

2.3 **String** — `"..."` with the escapes `\"` `\\` `\n` `\r` `\t` `\0`
`\u{XXXX}`. A literal newline inside a string is an error.

2.4 **Raw string** — `r"..."` with no escape processing except `\"`. Used for
regular expressions: `pattern(r"^[a-z0-9-]{3,40}$")`.

2.5 **Sigils** — `@name` (path parameter, §5.2) and `$name` (local reference
inside a query, §5.3). The sigil is part of the token; `@ name` is an error.

2.6 **There are no reserved words.** Every word the grammar gives meaning to
is also a legal identifier; the parser decides by position.

This is not laxity. `route`, `server`, `size`, `max`, `check`, `key`, `text`,
`date` and `int` all appear as ordinary column names, rule names or builtin
namespaces in this specification's own sample — `route varchar(200)` in the
audit table, `max(90)` on a class field, `int(…)` and `date.now()` as calls.
A reserved-word list would forbid the language's own examples.

The words with grammatical meaning are:

```
after     and       as        asc       by        cascade   catch     check
class     conflict  cross     database  default   delete    desc      do
else      enum      error     errorHandler         except   exists    false
first     for       foreign   from      full      function  group     having
identity  if        ilike     import    in        index     inner     insert
into      join      key       left      let       like      limit     max
middleware          namespace no        not       nothing   null      nulls
of        on        or        orderby   page      primary   private   provides
raises    references          requires  restrict  return    right     route
routes    schema    select    server    service   set       size      table
test      throw     transaction         transient true      under     unique
update    use       using     view      was       where     with      assert
```

A word in a position where the grammar expects that keyword is that keyword.
Everywhere else it is an identifier. In practice this bites in exactly one
place — a *statement-leading* word is effectively reserved at the start of a
statement — and that is the position where a column name never appears.

2.7 The HTTP method names `GET POST PUT PATCH DELETE HEAD OPTIONS` are
contextual: they are keywords only immediately after `route`.

2.8 **Removed keywords.** `entity`, `dbcontext`, `with` *(as a query
clause)*, `via`, `nav`, `validate`, `new`, `patch`, `group` *(as a route
grouping keyword)*, `mount`, `dome`. Encountering one in a declaration
position produces `E0900` naming the replacement (routing §11).

---

## 3. Case convention

3.1 **PascalCase** — things whose shape you declare: `database`, `table`,
`view`, `class`, `enum`, `service`, `middleware`, `error`.

3.2 **snake_case** — columns, functions, locals, parameters, enum members,
JSON keys, `namespace` segments, `schema` names, `server { }` keys.

3.3 A name that violates §3.1 or §3.2 produces `W0101`, a warning. It is not
an error: casing carries no semantics, and physical naming (§4) is derived
mechanically either way. `jwc lint --deny-warnings` promotes it.

3.4 `jwc fmt` never renames. Renaming is a schema-visible act (migrations §6).

---

## 4. Physical names

4.1 The physical name of a `schema`, `table`, `view`, `enum` or column is the
**snake_case transform** of its declared name. No pluralisation, no
singularisation, no prefix, no suffix.

The transform inserts `_` before each uppercase letter that follows a
lowercase letter or digit, or that is followed by a lowercase letter, then
lowercases everything:

| Declared | Physical |
|---|---|
| `Accounts` | `accounts` |
| `InvoiceLines` | `invoice_lines` |
| `ApiKeys` | `api_keys` |
| `OrgWithMembers` | `org_with_members` |
| `created_at` | `created_at` |

4.2 `as "literal"` overrides the transform and is used verbatim, quoted in
DDL. It is the only escape hatch and it is visible at the declaration site:

```
table Accounts of App.auth as "tbl_user_accounts" { ... }
created_at timestamptz as "createdAt";
```

4.3 Two objects in one schema whose physical names collide is an error
(`E0110`), naming both declaration sites. This can only happen through §4.2.

4.4 The physical name of an *enum member* is its declared name verbatim.
Enum members are already snake_case by §3.2.

4.5 **The database name is never declared.** It comes from `DATABASE_URL`.
`database App : Postgres` declares a *connection*, and `App` is the name that
qualifies schemas in source. See config §2.

4.6 The database name is therefore **not** part of any physical name.
`schema billing of App;` creates the schema `billing`, not `app_billing`;
`App.billing.Invoices` is `billing.invoices` in DDL. The `App.` prefix exists
to make source references unambiguous, and it stops at the source.

---

## 5. Scopes and resolution

There are exactly four name spaces, distinguished lexically. This is the
whole rule, and §5.3 is the clause that makes `where org_id == org_id`
impossible.

### 5.1 Declaration space (bare PascalCase / dotted)

Types, tables, views, classes, services, middleware, errors and enums live in
one **flat, global** space keyed by their declared name. `import` does not
scope it (§6.3). A duplicate declared name anywhere in the program is an
error (`E0111`), naming both sites.

Database objects are additionally addressable by their qualified path:
`App.auth.Accounts`. The qualified form is **required** in `from`, `into`,
`references` and `join` (queries §2.1). The bare form is used everywhere
else, including enum member access (`MemberRole.owner`).

### 5.2 Path parameters — `@name`

`@name` refers to a path parameter of the enclosing route or middleware. It
is legal only inside a `route` block, a `middleware` body/`after` block, or
a `routes` block's `use` arguments. Elsewhere: `E0220`.

Path parameters are typed at their binding site (routing §4) and are **never**
strings by default. `@org_id` in a route under
`routes "/api/v1/orgs/{org_id: bigint}"` has type `bigint`.

### 5.3 Locals — `$name`, everywhere

**Every reference to a local, a function parameter, or a `for` binding
carries `$`.** Not only inside queries — everywhere, including route bodies
and service code:

```jwc
let account = AuthService.profile(context.account_id);
return created(json($account)) with { "Location": "/orgs/" + string.of($account.id) };
```

The *declaration* site is bare (`let account`, `function f(org_id: bigint)`,
`for (line in $xs)`); the *reference* carries the sigil. One rule, no
context-sensitivity, and a reader can always tell a value that came from code
from a name that came from the database.

Consequently, inside a **query clause** — a `join ... on` expression,
`where`, `having`, `group by`, `orderby`, a projection field expression, an
aggregate filter, an `insert` object literal, a `set` clause, or a
`page after` expression — an unqualified identifier can only be a column:

- an **unqualified identifier resolves to a column** of exactly one binding;
- **`Binding.name`** resolves to a column of that binding;
- **`$name`** is a local, **`@name`** is a path parameter.

A bare identifier that is neither a column nor a declaration name is
`E0211: unknown identifier — did you mean '$name'?`.

Consequences, all of them intended:

```jwc no-compile
where org_id == $org_id          -- tenancy filter: column vs. local
where org_id == org_id           -- W0104: both sides are the same column
where code == $req.plan_code
where accepted_at == null
```

Because the sigil is **required everywhere**, a bare identifier in a query clause is
unambiguously a column and `where org_id == org_id` can only ever mean the
tautology it looks like. That is the whole fix for #2/#34: the ambiguity is
removed by construction rather than reported. The compiler still warns —
`W0104: comparison is always true` — because a tautology in a `where` is
never intentional.

`E0210` covers the opposite slip: a bare identifier in a query clause that
resolves to **no** column but does match a local in scope. The message is
`E0210: 'account_id' is not a column of any binding here; did you mean
'$account_id'?`. Without it the diagnostic would be a bare "unknown column",
which points away from the fix.

The declaration space (§5.1) is *not* sigiled: `MemberRole.owner`,
`AuthService.login(...)`, `context.account_id`, `request.header(...)` and
`App.auth.Accounts` are names, not locals.

### 5.4 Bindings inside a query

Every `select` binds exactly one name for its source, written after `select`:

```jwc no-compile
select Accounts from App.auth.Accounts     -- binds `Accounts`
select a from App.auth.Accounts            -- binds `a`
```

The binder is **mandatory**. `select from App.org.MemberAccess` does not
parse. This closes #18: a view query has a binder like every other query.

Each `join` may bind its own name, defaulting to the joined table's declared
name:

```jwc no-compile
left join App.auth.Accounts inviter on inviter.id == Invites.invited_by
left join App.auth.Accounts on Accounts.id == Members.account_id
```

Two bindings with the same name in one query is an error (`E0212`), which is
what makes self-joins expressible (#1): the second occurrence must be
aliased.

An unqualified column name that exists in more than one binding is
`E0213: 'x' is ambiguous`, naming every binding that has it.

### 5.5 Locals

`let name = …;` introduces a local; every reference to it is `$name` (§5.3).
A `let` may not shadow a local, parameter or path parameter that is already
in scope (`E0214`). Blocks nest; a local goes out of scope at the end of its
block.

Assignment to a local (`$x = expr;`) is permitted and does not change its
type. Assignment to a field (`x.y = expr;`) does not parse: load-modify-save
is a declared non-goal.

---

## 6. Namespaces, imports, packages

### 6.1 `namespace`

6.1.1 A file **may** declare `namespace a.b.c;` as its first declaration.
At most one per file.

6.1.2 A file without a `namespace` belongs to the root namespace.

6.1.3 The namespace does **not** scope names (§5.1). It exists to (a) name
the file's contents in diagnostics and `jwc openapi` tags, (b) be the target
of `import`, and (c) give `jwc fmt` and the LSP a module identity.

6.1.4 Convention, enforced as `W0102`: a file's namespace matches its path
under `src/` with `/` replaced by `.` and the extension dropped.

### 6.2 `import`

6.2.1 `import x.y;` declares a dependency. Its target is resolved in this
order:

1. a namespace declared by some file in this project → **namespace import**;
2. a key in `jwcproj.json`'s `dependencies` → **package import**;
3. neither → `E0201: unknown import 'x.y'`.

6.2.2 If the name resolves under *both* rule 1 and rule 2, that is
`E0203: 'x' is both a local namespace and a package dependency` — rename one.
There is no precedence rule, deliberately.

6.2.3 A **package import** additionally brings the package's builtin
namespace into the declaration space: `import redis;` is what makes
`redis.rate_limit(...)` resolvable (builtins §8).

### 6.3 What `import` does and does not do

6.3.1 `import` does **not** restrict visibility. Because the declaration
space is flat (§5.1), a name is reachable whether or not you import it.

6.3.2 `import` **is** checked. Referring to a name declared in another
namespace without importing that namespace is `E0202`, naming the missing
import line. This is the enforcement half of N5, and it is what makes the
import list a truthful dependency graph without building a visibility system
(deferred; ROADMAP §7).

6.3.3 An `import` whose namespace contributes no referenced name is `W0103`
(unused import).

6.3.4 Import cycles between namespaces are permitted. Namespaces are not
compilation units.

### 6.4 Free functions

6.4.1 A `function` declared at top level is a **free function**. It is
callable unqualified from anywhere in the program, subject to §6.3.2.

6.4.2 A call to a name that is neither a free function, a builtin
(builtins §1), nor `Service.method` is `E0204: unknown function`. There is
no implicit declaration. This closes the second half of N5: the sample's
`invite_body`, `random_token`, `next_invoice_number`, `verify_signature` and
`send_email` each need a declaration site or a builtin (builtins §7).

6.4.3 A free function's parameters must be annotated; its return type is
inferred unless the function is exported by a package (types §10.2).

---

## 7. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E0104` | doc comment attaches to nothing |
| `E0107` | integer literal out of `bigint` range |
| `E0110` | physical name collision |
| `E0111` | duplicate declared name |
| `E0201` | unknown import |
| `E0202` | reference to a name in an un-imported namespace |
| `E0203` | import is both a namespace and a package |
| `E0204` | unknown function |
| `E0210` | bare identifier in a query clause is not a column but matches a local |
| `E0211` | unknown identifier — likely a missing `$` |
| `E0212` | duplicate binding name in one query |
| `E0213` | unqualified column is ambiguous across bindings |
| `E0214` | `let` shadows an existing binding |
| `E0220` | `@name` outside a route or middleware |
| `E0900` | removed keyword from the pre-1.0 language |
| `W0101` | case convention |
| `W0102` | namespace does not match file path |
| `W0103` | unused import |
| `W0104` | comparison is always true |

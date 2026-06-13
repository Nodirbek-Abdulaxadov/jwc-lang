# Visibility — `public` and `private` declarations

Status: **DRAFT** · Target: stable at v1.0 · Reflects: **v0.4.8**

**Related spec docs**:
[index](index.md) ·
[semantics](semantics.md) (the call sites this gate inspects) ·
[aot-scope](aot-scope.md) (the native AOT path that trusts this gate) ·
[threat-model](threat-model.md) (runtime gates that sit on top of the
static one here).

This file specifies the public/private surface of JWC top-level declarations
and the validator invariant that downstream stages (interpreter, native AOT
codegen) depend on.

## Surface

Every top-level declaration carries a `Visibility` marker
(`src/ast.rs::Visibility`):

| Variant   | Default? | Source syntax                                       |
|-----------|----------|-----------------------------------------------------|
| `Public`  | no       | `public function …`, `public entity …`, `public middleware …` |
| `Private` | yes      | bare `function …`, or explicit `private function …` |

Declarations that carry a visibility marker:

| Decl          | Carries `Visibility`? | Source                          |
|---------------|------------------------|----------------------------------|
| `function`    | yes                    | `ast.rs::FunctionDecl::visibility` |
| `entity`/`class` | yes                 | `ast.rs::ModelDecl::visibility`    |
| `middleware`  | yes                    | `ast.rs::MiddlewareDecl::visibility` |
| `route`       | parsed, ignored        | per `src/parser/mod.rs:197` comment — routes are activated by `mount`, not invoked by name |
| `dbcontext`   | no marker              | always reachable to any entity that names it |
| `const`       | parser rejects the marker | `src/parser/mod.rs:222`         |
| `errorHandler`| parser rejects the marker | `src/parser/mod.rs:242`         |

## Rule

> A reference from inside namespace **A** to a declaration whose declaring
> namespace is **B** is allowed only when either (a) **A == B** or (b) the
> declaration is `Public`.

For top-level functions, "reference" means any of:

1. an `Expr::Call { name, … }` expression evaluated anywhere inside a
   function body, route inline body, middleware body, or `errorHandler`
   body — including nested expressions (`if cond`, `for in iter`, RHS of
   assignments, arguments of other calls, …);
2. a `route ... -> handler;` handler reference (the implicit call site that
   dispatch invokes per request);
3. a middleware `after { ... }` block, which is part of the same chain as
   the pre-handler body.

For entities and middlewares the rule still applies — entities are
referenced by `new EntityName()`, DB statements, and type annotations;
middlewares are referenced by `route ... use Mw1, Mw2`. The current
validator does not yet gate those edges because the AOT codegen flattens
both before lowering (entities → DDL emission, middlewares → inline
expansion at the route activation site, `runner/mod.rs::flatten_namespaces`).
The function-call gate is what the AOT path actually needs.

## Validator check sites

The single canonical check is `parser::validate::check_visibility`
(`src/parser/validate.rs`), called as the last pass of `validate_program`.
It walks:

| Caller                         | `caller_ns`           | File:line                                  |
|--------------------------------|------------------------|---------------------------------------------|
| `program.functions[*].body`    | `function.namespace`   | `src/parser/validate.rs::check_visibility` (function loop) |
| `program.routes[*].body`       | `route.namespace`      | `src/parser/validate.rs::check_visibility` (route loop, inline body branch) |
| `program.routes[*].handler`    | `route.namespace`      | `src/parser/validate.rs::check_handler_visibility` |
| `program.middlewares[*].body`  | `mw.namespace`         | `src/parser/validate.rs::check_visibility` (middleware loop) |
| `program.middlewares[*].after_body` | `mw.namespace`    | same                                       |
| `program.error_handler.body`   | root (`[]`)            | `src/parser/validate.rs::check_visibility` (final block) |

At each call site `check_visibility_in_expr` resolves the callee with
`resolve_callee`, which mirrors `runner::Vm::resolve_function`
(`src/runner/mod.rs::resolve_function`): exact-FQN lookup, then caller's
own namespace, then each `import` declared in the caller's namespace.
The resolved callee's `(namespace, visibility)` pair is then checked
against the caller's namespace; mismatch + `Private` → fatal `error[E021]`.

Built-in calls (`is_builtin` in `src/builtins.rs`) are skipped — builtins
have no `Visibility` and live in the prelude, not a user namespace.

## Invariant the AOT path trusts

`src/native_build.rs` (the codegen header at the top of the file) calls
`validate_program` upstream. After validation succeeds, the codegen
flattens namespaces and lowers every `Expr::Call` to a plain Rust function
call against `user_fn_name(callee)`. The lowered Rust source does NOT
re-emit a visibility check, and `pub fn` / `fn` modifiers in the emitted
crate don't correspond to JWC's `Visibility` — every emitted helper is
crate-public. The validator's `check_visibility` is therefore the SOLE
static gate against a cross-namespace private leak in the AOT path. If
this gate fails, AOT must too; if it passes, AOT is safe.

The interpreter independently re-checks at call time via
`Vm::check_visibility` (`src/runner/mod.rs:803`). That's defensive — a
hand-built `Program` constructed outside the parser would skip the
validator. The two checks are intentionally redundant; if they diverge,
the validator wins.

## Test fixtures

Positive (acceptance) and negative (rejection) cases both live as Rust
tests in `tests/imports.rs`. Conformance-suite fixtures (`tests/conformance/`)
are stdout-matching only; rejection cases don't fit that format. The
relevant cases are:

| Test                                                                  | Asserts                                                       |
|-----------------------------------------------------------------------|----------------------------------------------------------------|
| `case_private_function_referenced_across_namespace_rejected`          | cross-namespace bare call into private → E021                  |
| `case_public_function_referenced_across_namespace_ok`                 | cross-namespace call into `public` is accepted                 |
| `case_private_function_called_inside_same_namespace_ok`               | same-namespace private call is accepted                        |
| `case_private_route_handler_from_other_namespace_rejected`            | `route … -> ns.private_handler` is rejected                    |
| `public_function_in_namespaced_file_is_tagged_correctly`              | parser stamps `Public` on `pub function`                       |
| `function_without_modifier_defaults_to_private`                       | default visibility is `Private`                                |
| `explicit_private_modifier_is_accepted`                               | explicit `private function` parses and tags correctly          |
| `duplicate_visibility_modifier_rejected`                              | `public public function` is a parse error                      |

Any change to the validator's visibility rule MUST update both this spec
and the matching tests in the same change — the conformance precedence
note in `docs/spec/README.md` applies.

## Follow-ups

These adjacent surfaces will need a parallel cross-namespace check once
they land in the language. They are NOT gated by the current
`check_visibility` pass; if you ship one, extend the validator in the
same change.

- **User-declared error types.** A future `error Name { fields }`
  declaration (currently tracked under the typed-catch follow-ups in
  `docs/spec/semantics.md` §5.1) will live in a namespace just like a
  function or entity. The `throw Ns.MyError(...)` call site needs the
  same `Public` / same-namespace check applied here for functions, and
  the matching `catch (e: Ns.MyError)` clause will need the same
  resolution path that `resolve_callee` runs today.
- **Entity / middleware cross-namespace edges.** The current validator
  intentionally only gates function calls because the AOT codegen
  flattens both before lowering (see "Invariant the AOT path trusts"
  above). If a future change emits per-namespace lowered output for
  entities or middlewares, the same `(namespace, visibility)` check
  needs to extend to `new EntityName(...)` and `use Mw1, Mw2` edges.
- **Imports.** `resolve_callee` already walks the caller's `import`
  declarations; the rule above ("same namespace OR `Public`") still
  applies. If a future `import as` alias form lands, the visibility
  check operates on the resolved (post-alias) declaration, not the
  alias name.

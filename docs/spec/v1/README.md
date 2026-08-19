# JWC v1 — the specification

This directory is the **normative** description of the language JWC 1.0 will
be. Nothing here compiles with the 0.9.x compiler and nothing here is
expected to: v1 replaces that language rather than extending it
(ROADMAP §0).

## The normative documents

| File | What it fixes |
|---|---|
| [`grammar.ebnf`](./grammar.ebnf) | the syntax, complete |
| [`names.md`](./names.md) | lexical structure, `namespace`/`import`, name resolution, the `$`/`@` sigils |
| [`types.md`](./types.md) | scalar dictionary, `Raw \| Record` lattice, `T?` and narrowing, spread, class validation, expression core |
| [`schema.md`](./schema.md) | tables, enums, constraints, indexes, triggers, comments, constraint naming, DDL emission order |
| [`queries.md`](./queries.md) | `select`, joins, projections, aggregates, `first`, views, keyset pagination |
| [`writes.md`](./writes.md) | `insert`/`update`/`delete`, `on conflict`, locking, `raw`, `transaction` |
| [`routing.md`](./routing.md) | `routes`/`route`, typed path parameters, conflicts, response construction, `E0900` |
| [`middleware.md`](./middleware.md) | chain composition, `after`, `requires`/`provides`, typed `context` |
| [`errors.md`](./errors.md) | E1–E14: declared errors, inferred raise sets, exhaustiveness, constraint promotion, postfix `catch` |
| [`migrations.md`](./migrations.md) | snapshots, diff phases, declared renames, enum evolution |
| [`builtins.md`](./builtins.md) | the builtin surface, namespaced by where it runs |
| [`config.md`](./config.md) | `database init()`, `server { }`, environment |
| [`DEFERRED.md`](./DEFERRED.md) | 18 dated omissions + the gap → verdict index for all 56 gaps |

## The inputs

| File | What it is |
|---|---|
| [`design.md`](./design.md) | the design session's decisions — the source the spec formalises |
| [`gaps.md`](./gaps.md) | 44 confirmed gaps (from 138 findings, after adversarial review) |
| [`error-model.md`](./error-model.md) | the error-model analysis; [`errors.md`](./errors.md) is its normative form |

## The sample

[`sample/`](./sample) is a ~1200-line SaaS billing app: 4 schemas, 12 tables,
5 views, 4 services, 26 endpoints. It is **ground truth** — ROADMAP §2 rule
4 says that if the specification cannot express the sample, the specification
is wrong.

As of v0.20.0 the sample conforms to the spec. The four defects `gaps.md`
was written from are fixed, and each fix is a clause:

| Was | Now |
|---|---|
| `AuthService.login` answered **403** for bad credentials | `Unauthorized` → 401 (errors §1.2) |
| `RequireOrgAdmin` read a `context` key it never declared | `requires RequireOrgMember` + `provides role: MemberRole` (middleware §3, §6) |
| `WebhookService.record_payment` was select-then-insert — a TOCTOU that turned redelivery into a retry loop | `on conflict (provider_ref) do nothing` (writes §2.3) |
| six `where` sites were ambiguous between a column and a local | `$` is mandatory on every local (names §5.3) |

Money moved from `int` cents to `numeric(14,2)` (types §2.1), path
parameters are typed (routing §3.1), the invoice list is keyset-paginated
(queries §9), and the eight-arm `errorHandler` shrank to one arm plus a
fault arm — because a declared error's default status makes an arm optional
(errors §4.3).

## Checking the sample against the spec

```bash
python3 docs/spec/v1/check_sample.py
```

Classifies every construct the sample uses, maps it to the clause that
defines it, verifies that clause exists, and fails on any construct from the
removed vocabulary. Output is [`spec-coverage.json`](./spec-coverage.json).
ROADMAP's done-criterion for v0.20.0 is **0 `unspecified`**.

This is a lexical scan, not a parser. v0.21.0 brings a real front-end and
`tests/parse_corpus/` takes over the syntax half; the clause map stays.

## Plan

[`ROADMAP.md`](../../../ROADMAP.md) — 12 releases, v0.20.0 → v1.0.0.

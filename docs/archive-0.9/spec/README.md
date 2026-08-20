# JWC Language Specification

Status: **DRAFT** · Target: stable at v1.0 · Reflects: **v0.4.8**

This directory is the language specification — the contract between the
JWC compiler and any code written in it. Until v1.0 the parser
(`src/parser.rs`) is still the de-facto reference; the spec is being
extracted from it under Phase 0 of
[`../../PRODUCTION_READINESS_PLAN.md`](../../PRODUCTION_READINESS_PLAN.md).

> **Looking for a one-page overview?** See [`index.md`](index.md) — the
> entry point that lists every spec doc with one-line summaries plus
> cross-links.

## Layout

| File | Scope |
|---|---|
| [`index.md`](index.md) | Entry-point index with cross-links and the "future placeholders" map |
| [`grammar.ebnf`](grammar.ebnf) | Concrete syntax — token grammar + production rules in EBNF |
| [`semantics.md`](semantics.md) | Evaluation order, scope, coercion, equality, integer/float behaviour, transactions/savepoints, error kinds |
| [`visibility.md`](visibility.md) | `public` / `private` declarations, `E021`, the AOT-trusted invariant |
| [`threat-model.md`](threat-model.md) | Runtime security surfaces and their mitigations |
| [`aot-scope.md`](aot-scope.md) | What `jwc build --native` lowers cleanly; deferrals |
| [`builtins.md`](builtins.md) | Per-builtin contract: signature, type rules, error modes, examples |
| [`SEMVER.md`](../../SEMVER.md) | What counts as a breaking change |
| [`DEPRECATION.md`](../../DEPRECATION.md) | Minimum warning window before removal |

## How the spec relates to the rest of the repo

- **Conformance suite** (`tests/conformance/`) — every observable rule
  in this spec should have at least one case. Cases live as paired
  `case_*.jwc` + `case_*.stdout.txt` files; the harness runs each
  through both the interpreter and the native AOT pipeline.
- **README.md** — product-facing guide. When the spec disagrees with
  the README, the spec wins, but file an issue so the README is fixed.
- **Source of truth precedence (during extraction)**:
  1. Conformance suite (executable)
  2. This spec (prose + EBNF)
  3. `parser.rs` / `runner.rs` (current implementation)
  4. README.md (user-facing summary)

## Contributing

When adding a language construct:

1. Add EBNF for the new production in `grammar.ebnf`.
2. Describe its evaluation semantics in `semantics.md`.
3. If it's a builtin, document it in `builtins.md`.
4. Add at least one `tests/conformance/cases/case_*.jwc` + `.stdout.txt`
   pinning the observable behaviour.

A construct without all four is considered unstable and may change in a
minor release until they land.

# JWC Language Specification — Index

Status: **DRAFT** · Target: stable at v1.0 · Reflects: **v0.4.8**

> **North star.** "Write web backends without hand-coding CRUD, without
> fighting an ORM, native-fast." Surface that doesn't serve this goal is
> declared Non-goal — see [`ROADMAP.md` Non-goals](../../../ROADMAP.md#non-goals-10-ga-qadar-va-undan-keyin-ham--qatiy-yoq).
> LLVM IR, cross-target native matrix, WASM, self-hosting, multi-DB
> driver, SSE v2 won't ship pre-1.0.

This page is the entry point for the JWC language specification. Each file
below is a free-standing spec document; together they are the contract
between the compiler and any code written in it. Until v1.0 the parser
(`src/parser.rs`) and runner (`src/runner/`) remain the de-facto reference
where the spec is silent — see the precedence note in
[`README.md`](README.md).

## Spec documents

| Document | One-line summary |
|---|---|
| [`semantics.md`](semantics.md) | Evaluation order, scope, integer overflow, float formatting, UTF-8 strings, `==` cross-type rules, transactions/savepoints, error kinds. |
| [`visibility.md`](visibility.md) | `public` / `private` declarations, the cross-namespace rule, `E021`, and the invariant the AOT path trusts. |
| [`threat-model.md`](threat-model.md) | Runtime security surfaces: path traversal, header injection, SSRF allowlist, JWT `exp`, SQL interpolation audit, secrets redaction, boot-time config validation. |
| [`aot-scope.md`](aot-scope.md) | What `jwc build --native` lowers cleanly today, what panics (use `jwc run`), and what the native mirror could close next. |
| [`ecosystem.md`](ecosystem.md) | **draft** — architectural split between core-tier hot-path infra (Redis, MySQL, Kafka, ...) and pure-JWC registry packages (S3, Stripe, OpenAI, ...). Implementation roadmap toward 1.0. |
| [`builtins.md`](builtins.md) | Per-builtin contract: signature, error modes, tests. |
| [`grammar.ebnf`](grammar.ebnf) | Concrete syntax — token grammar + production rules in EBNF. |
| [`README.md`](README.md) | How the spec relates to the conformance suite, precedence rules, contributor checklist. |

**Future placeholders** (not yet authored):

- `grammar.md` — prose companion to `grammar.ebnf` with worked examples.
- `evaluation.md` — formal small-step rules backing the prose in `semantics.md`.

## How the spec hangs together

- `semantics.md` pins the value model and operator behaviour.
- `visibility.md` adds the namespace-level static check that gates calls
  before the runner sees them; the AOT path in
  [`aot-scope.md`](aot-scope.md) trusts that gate.
- `threat-model.md` documents the runtime defences (path matcher, header
  parser, SSRF allowlist, JWT `exp`, secret scrubber) and links back to
  `semantics.md` for the string-encoding rules those defences depend on.
- `builtins.md` is the per-function contract; the surface listed there is
  what `aot-scope.md` promises to lower.

## Source-of-truth precedence (during extraction)

1. Conformance suite (`tests/conformance/`) — executable.
2. This spec — prose + EBNF.
3. `parser.rs` / `runner/` — current implementation.
4. [`../../README.md`](../../../README.md) — user-facing summary.

When the spec disagrees with the implementation, the spec wins; file the
implementation as a bug. When the spec disagrees with the conformance
suite, the suite wins and the spec is the bug.

## Meta documents (repo root)

- [`../../SEMVER.md`](../../../SEMVER.md) — what counts as a breaking change.
- [`../../DEPRECATION.md`](../../../DEPRECATION.md) — minimum warning window.
- [`../../SECURITY.md`](../../../SECURITY.md) — reporting, hardening notes.
- [`../../CONTRIBUTING.md`](../../../CONTRIBUTING.md) — contributor handbook.

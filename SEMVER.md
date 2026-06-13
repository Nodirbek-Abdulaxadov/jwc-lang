# JWC SemVer Policy

JWC versions follow [Semantic Versioning 2.0.0](https://semver.org).
Until v1.0 this document is the contract; at v1.0 it becomes binding.

> Stable surface as of **v0.4.8** (Phase 8 dev-experience close-out).

> Released as part of Phase 0 of
> [`PRODUCTION_READINESS_PLAN.md`](PRODUCTION_READINESS_PLAN.md).

---

## Version meaning

A release `X.Y.Z` of JWC carries:

- **X — major.** Incremented for breaking changes (see below). Before
  v1.0, breaking changes may land on a minor bump; after v1.0 they
  require a major bump.
- **Y — minor.** Backwards-compatible feature additions, new builtins,
  new syntax that doesn't invalidate existing programs.
- **Z — patch.** Bug fixes, performance work, doc fixes — nothing a
  user-written program can observe a behavioural change from, beyond
  "the bug is gone."

A pre-1.0 minor (`0.Y.Z`) is allowed to break things. Each such break
ships with a `BREAKING:` line in `CHANGELOG.md` and, when feasible, a
`jwc upgrade` codemod.

## What counts as a breaking change

A change is **breaking** if any of the following could happen to a
previously-working program after upgrade:

1. **Syntax that used to parse now fails** — keyword stolen, reserved
   word added, grammar tightened.
2. **A program that used to compile now fails `validate_program`** —
   stricter entity / DB / type checks that weren't there before.
3. **A builtin function changes signature, return type, or argument
   types.** Adding *new* optional builtins or *new* optional arguments
   to existing builtins (defaulted) is non-breaking.
4. **The JSON shape of an HTTP response changes** — field renamed,
   removed, type changed, ordering becoming significant where it
   wasn't.
5. **An environment-variable default changes** in a way that alters
   observable runtime behaviour (timeouts that drop in-flight requests,
   pool sizes that change connection budgets, etc.).
6. **An error code (`E####`) is reused for a different condition.** New
   codes may be added freely; existing ones are append-only.
7. **The CLI surface changes** — subcommand renamed, removed, exit code
   for a documented condition changed, flag removed.
8. **A previously-documented invariant is dropped.** Anything in
   `docs/spec/` is part of the contract.

## What is explicitly NOT breaking

- Performance changes (faster or slower) — tracked separately under
  Phase 7 regression budgets, not SemVer.
- Internal refactors visible only through source-level inspection of the
  `jwc` crate (this repo's Rust code is **not** a public API; consumers
  are users of the language, not the crate).
- Adding new error codes, new builtins, new optional arguments, new env
  vars (with non-breaking defaults).
- Changing wording of an error message text, as long as the `E####` code
  and the conditions that produce it are unchanged.
- Implementation-defined behaviour declared in
  [`docs/spec/semantics.md`](docs/spec/semantics.md) §10.

## How deprecations work

Anything that will eventually be removed first becomes **deprecated**.
The lifecycle:

1. **Land deprecation in a minor release.** Emit a `jwc` warning at
   compile time (`W####` code, surfaced in `jwc build`/`jwc lint` by
   default) and document it in `CHANGELOG.md` under "Deprecated".
2. **Keep working for at least one minor.** Removal cannot happen in
   the same minor it was deprecated in.
3. **Remove in the next major** (post-1.0) or call out the break loudly
   in `0.Y` (pre-1.0).

Pre-1.0 the warning window is effectively "best effort"; post-1.0 it is
contractually one full minor cycle.

## Release cadence (target)

- **Patch**: as needed for fixes; aim for ≤ 2 weeks between releases
  during active sprints.
- **Minor**: roughly every 4–6 weeks, batching shipped Phase work.
- **Major**: rare; pre-1.0 the 0.Y bump carries the breaking changes
  themselves.
- A release is tagged `vX.Y.Z` against `main`, with the GitHub Release
  containing changelog + `.sha256`-verified artifacts (see
  `.github/workflows/release.yml`).

## Pre-release suffixes

- `vX.Y.Z-rc.N` — release candidate; production-supported on a
  best-effort basis. The 1.0 series will spend ≥ 4 weeks in RC with at
  least two external pilot projects before promotion.
- `vX.Y.Z-alpha.N` / `-beta.N` — feature previews; **no SemVer
  guarantees** between alpha/beta builds.

## Yanking and rollbacks

A release may be **yanked** (the GitHub Release marked as such, the tag
left in place) if a regression makes it dangerous to install. Yanked
versions never receive backported fixes; users are directed to the next
patch. Yanks are recorded in `CHANGELOG.md`.

## Stable surface as of v0.4.7

A snapshot of where the contract line sits today. Bumped each minor
release; the next refresh lands with v0.4.8.

### Stable — covered by SemVer guarantees above

- **Language syntax.** Everything documented in
  [`docs/spec/grammar.ebnf`](docs/spec/grammar.ebnf) and the
  evaluation rules in [`docs/spec/semantics.md`](docs/spec/semantics.md).
  Adding new keywords is breaking; we do that under the pre-1.0 minor
  rule with a `BREAKING:` line in `CHANGELOG.md`.
- **Builtin set.** Every entry in
  [`docs/builtins.md`](docs/builtins.md) (the user-facing reference) is
  on the stable surface — signature, argument types, return type. New
  *optional* builtins or new *optional* arguments to existing builtins
  (with defaults) are non-breaking.
- **Public CLI flags + their semantics.** Every `--flag` documented in
  [`docs/docs/cli/overview.md`](docs/docs/cli/overview.md). Renaming /
  removing a documented flag is breaking; adding new flags is not.
  Exit codes for documented conditions (success = 0, validation
  failure = 1, etc.) are part of the contract.
- **Error code identity.** Every `E####` code is append-only — the
  code itself never changes meaning, even if the wording does. The
  catalog lives at
  [`src/error_codes.rs`](src/error_codes.rs).
- **Visibility rule.** The `public` / `private` semantics in
  [`docs/spec/visibility.md`](docs/spec/visibility.md) — namespace
  isolation behaviour and `E021` raising conditions.

### Pre-stable — subject to break before 1.0

These surfaces may change in a minor release before 1.0; each break
ships with `BREAKING:` notes.

- **Native AOT scope details.** Track
  [`docs/spec/aot-scope.md`](docs/spec/aot-scope.md). The set of
  builtins that work under `jwc build --native` grows minor-over-minor;
  the set of constructs that panic shrinks. A program that runs under
  `jwc run` is the stable surface; the AOT-acceptance subset is not.
- **`JWC_QUEUE_DRIVER=postgres` schema.** The Postgres-backed job
  queue tables and column layout will likely gain columns
  (retry-after, priority, lease-owner) before 1.0. Treat the schema as
  managed-by-jwc — do not write app SQL against the queue tables.
- **`JWC_HTTP_ALLOWLIST` format.** Today: CSV of bare hostnames,
  exact-match. May grow to support CIDR ranges and / or port-scoped
  entries. A program that sets `JWC_HTTP_ALLOWLIST=api.example.com`
  today will keep working — but the grammar may expand in a backward-
  compatible way that the spec text needs to follow.
- **OTLP wiring.** The observability exporter set / span names / span
  attributes are not yet pinned; expect renames before 1.0.

### Unstable — anything not listed above

Anything not on the stable list above is fair game to change in any
direction without a deprecation cycle.

Explicit examples worth calling out:

- **`--no-typecheck` escape hatch.** Documented as **temporary**. It
  will be removed once the gradual type checker matures; the
  deprecation lifecycle is tracked in
  [`DEPRECATION.md`](DEPRECATION.md).
- **Internal Rust crate API** of `jwc-lang` itself. This repo's Rust
  source is NOT a public API; consumers are users of the language, not
  the crate. Per-module refactors that don't change the language
  surface are non-breaking even when they rename public Rust items.
- **Error message text.** The `E####` code is stable; the rendered
  human-readable text alongside it is not.

## Reach-out

If a SemVer-relevant change is ambiguous, file an issue tagged
`semver-question`; the maintainer makes the call and documents it here.

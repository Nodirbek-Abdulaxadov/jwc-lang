# JWC SemVer Policy

JWC versions follow [Semantic Versioning 2.0.0](https://semver.org).
Until v1.0 this document is the contract; at v1.0 it becomes binding.

> **Stable surface as of 0.9.7** — the v1 language, specified in
> [`docs/spec/v1/`](docs/spec/v1/). The 0.9.x grammar and its front-end
> were removed at v0.25.0; the surface described here is the one this
> compiler implements, not the one 0.9.x binaries implement.

---

## Version meaning

A release `X.Y.Z` of JWC carries:

- **X — major.** Incremented for breaking changes (see below). Before
  v1.0, breaking changes may land on a minor bump; after v1.0 they
  require a major bump.
- **Y — minor.** Backwards-compatible feature additions: new builtins,
  new syntax that doesn't invalidate existing programs.
- **Z — patch.** Bug fixes, performance work, doc fixes — nothing a
  user-written program can observe a behavioural change from, beyond
  "the bug is gone."

A pre-1.0 minor (`0.Y.Z`) is allowed to break things. Each such break
ships with a `BREAKING:` line in `CHANGELOG.md`. v0.25.0 is the extreme
case and the reason this paragraph exists: it replaced the grammar
outright, and every 0.9.x program stopped compiling.

## What counts as a breaking change

A change is **breaking** if any of the following could happen to a
previously-working program after upgrade:

1. **Syntax that used to parse now fails** — keyword stolen, reserved
   word added, grammar tightened.
2. **A program that used to pass `jwc check` now fails it** — stricter
   name resolution, type checking, or wiring checks that weren't there
   before.
3. **A builtin changes signature, argument types, or return type.**
   Adding *new* builtins, or new optional arguments to existing ones, is
   non-breaking.
4. **The JSON shape of an HTTP response changes** — field renamed,
   removed, type changed, ordering becoming significant where it wasn't.
   This includes the error envelope.
5. **The default HTTP status for a condition changes** — an error that
   answered 404 now answering 400 is breaking even though the body is
   the same shape.
6. **A `server { }` key changes meaning or default** in a way that
   alters observable runtime behaviour: timeouts that drop in-flight
   requests, a body limit that starts rejecting, a `bind` that stops
   answering on an interface it used to.
7. **A diagnostic code (`E####` / `W####`) is reused for a different
   condition.** New codes may be added freely; existing ones are
   append-only.
8. **The CLI surface changes** — subcommand renamed or removed, a
   documented flag removed, or the exit code for a documented condition
   changed.
9. **Emitted DDL changes for an unchanged schema.** `jwc gen-sql` is
   deterministic and offline; a program whose `table` declarations did
   not change must keep producing byte-identical DDL, or every
   downstream migration diff is wrong.
10. **A previously-documented invariant is dropped.** Anything normative
    in [`docs/spec/v1/`](docs/spec/v1/) is part of the contract.

## What is explicitly NOT breaking

- Performance changes, faster or slower. Tracked against the soak
  exit criteria in [`soak/`](soak/), not against SemVer.
- Internal refactors of the `jwc` crate. This repo's Rust code is **not**
  a public API — the crate is `publish = false`, and consumers are users
  of the language, not of the crate. Renaming a public Rust item is not
  a language change.
- Adding new diagnostic codes, new builtins, new optional arguments, new
  `server { }` keys with non-breaking defaults.
- Changing the wording of a diagnostic, as long as the `E####` code and
  the conditions that produce it are unchanged.
- The output of `jwc ast`, which is a debugging aid and is documented as
  an unstable format.
- Implementation-defined behaviour, where
  [`docs/spec/v1/types.md`](docs/spec/v1/types.md) says the result is
  implementation-defined rather than specified.

## How deprecations work

See [`DEPRECATION.md`](DEPRECATION.md) for the full lifecycle. In short:
anything that will eventually be removed first becomes deprecated, warns
for at least one full minor, and only then goes.

## Release cadence (target)

- **Patch**: as needed for fixes.
- **Minor**: batches shipped roadmap work; pre-1.0, this is also where
  breaking changes land.
- **Major**: rare; pre-1.0 the `0.Y` bump carries the breaks itself.
- A release is tagged `vX.Y.Z` against `main`. The tag drives
  [`release.yml`](.github/workflows/release.yml) (five target archives
  plus `.sha256` sidecars, attached to the GitHub Release),
  [`docker.yml`](.github/workflows/docker.yml) (multi-arch `jwc` and
  `jwc-runtime` images) and
  [`vscode-marketplace.yml`](.github/workflows/vscode-marketplace.yml).

## Pre-release suffixes

- `vX.Y.Z-rc.N` — release candidate; production-supported on a
  best-effort basis. Per [`ROADMAP.md`](ROADMAP.md), 1.0.0-rc.1 is gated
  on the conformance corpus blocking in CI, an external review, and a
  migrated pilot application.
- `vX.Y.Z-alpha.N` / `-beta.N` — feature previews; **no SemVer
  guarantees** between alpha/beta builds.

## Yanking and rollbacks

A release may be **yanked** (the GitHub Release marked as such, the tag
left in place) if a regression makes it dangerous to install. Yanked
versions never receive backported fixes; users are directed to the next
patch. Yanks are recorded in `CHANGELOG.md`.

---

## The surface, as of 0.9.7

### Stable — covered by the guarantees above

- **Language syntax.** The grammar in
  [`docs/spec/v1/grammar.ebnf`](docs/spec/v1/grammar.ebnf), and the name
  resolution, type and evaluation rules in
  [`names.md`](docs/spec/v1/names.md),
  [`types.md`](docs/spec/v1/types.md),
  [`queries.md`](docs/spec/v1/queries.md) and
  [`writes.md`](docs/spec/v1/writes.md).
- **Two properties the language promises.** Every value reaching SQL is
  a bind parameter, never interpolated; and a query result is `Raw`
  until an `as { }` projection opts into a `Record`. Both are checkable
  with `jwc explain`, and breaking either is breaking the language.
- **Schema emission.** The DDL
  [`schema.md`](docs/spec/v1/schema.md) specifies, and its determinism.
- **Builtin set.** Every entry in
  [`docs/spec/v1/builtins.md`](docs/spec/v1/builtins.md) — signature,
  argument types, return type. The surface is namespaced (`array.*`,
  `crypto.*`, `date.*`, `hash.*`, `jwt.*`, `mail.*`, `request.*`,
  `response.*`, `string.*`).
- **The error model.** Declared `error` types, inferred raise sets, the
  exhaustiveness check at the app boundary, and the response envelope —
  [`errors.md`](docs/spec/v1/errors.md) and
  [`error-model.md`](docs/spec/v1/error-model.md).
- **Routing and middleware.** Path and query parameter binding, the
  middleware chain order, and typed `context` —
  [`routing.md`](docs/spec/v1/routing.md),
  [`middleware.md`](docs/spec/v1/middleware.md).
- **Public CLI surface.** Every subcommand and documented flag in
  [`tooling.md`](docs/spec/v1/tooling.md): `check`, `fmt`, `gen-sql`,
  `explain`, `login`, `publish`, `add`, `test`, `lsp`, `openapi`,
  `lint`, `routes`, `migrate {new,up,down,status,verify}`, `serve`.
  Exit codes for documented conditions are part of the contract: 0 on
  success, 1 on an expected failure (a diagnostic, a migration that
  cannot be reversed).
- **Diagnostic code identity.** Every `E####` and `W####` is
  append-only — the code never changes meaning, even when the wording
  does. Codes are named by the spec and raised from `src/diag.rs` and
  its callers.
- **`server { }` configuration.** The keys in
  [`config.md`](docs/spec/v1/config.md) — `max_body_bytes`,
  `request_timeout`, `header_timeout`, `bind`, `trusted_proxies`,
  `cors { }`, `tls { }` — their defaults, and the rule that an unknown
  key is `E1206` rather than a silent no-op.
- **Operational endpoints.** `/healthz`, `/readyz` and `/metrics` at
  those fixed paths, with a declared route at the same path winning
  (config.md §4). The gauge *names* on `/metrics` are stable; the set of
  gauges is append-only.
- **Migrations.** The snapshot format, the diff, the emission ordering,
  and `up` / `down` / `status` / `verify` —
  [`migrations.md`](docs/spec/v1/migrations.md). A snapshot written by
  one version must be readable by the next.
- **Packages.** The manifest fields and the closed list of what a
  package may declare — [`packages.md`](docs/spec/v1/packages.md).

### Pre-stable — subject to break before 1.0

Each break ships with `BREAKING:` notes.

- **The `redis.*` surface**, behind the `redis` Cargo feature. The
  feature is off by default, so a standard build pulls in no Redis
  dependency; `redis.enabled()` is how a program asks. The command set
  will grow, and `rate_limit`'s window semantics may be refined.
- **`jwc openapi` output.** The document is derived from the route table
  and the typed signatures. It tracks the spec, so the emitted JSON will
  move as the type lattice does.
- **The LSP wire surface.** Which capabilities `jwc lsp` advertises —
  hover-to-SQL, go-to-definition, completion, signature help — is not
  pinned; the extension and the binary ship on one version line and the
  extension warns on major/minor skew.
- **`jwc test` isolation details.** Each `test` block runs in its own
  transaction and is rolled back. That much is stable; the ordering
  guarantees between blocks are not.

### Not in the language

These are absent by decision, not by omission, and their absence is not
a bug to be reported:

- `dispatch`, a job queue, WebSocket and SSE — ROADMAP §7 and
  [`builtins.md`](docs/spec/v1/builtins.md): the v1 vocabulary cannot
  declare them yet.
- Ahead-of-time native compilation. The 0.9.x `jwc build --native`
  backend was deleted at the v0.25.0 cutover. `jwc serve` runs an
  interpreter on hyper and tokio, and that is the only execution path.

### Unstable — anything not listed above

Fair game to change in any direction without a deprecation cycle.
Explicitly:

- **`jwc ast` output.** A debugging aid, documented as not a stable
  format.
- **`debug.dump`.** Enabled only under `jwc serve --dev`.
- **Diagnostic message text.** The code is stable; the rendered human
  text is not.
- **Internal Rust crate API.** Per-module refactors that don't change
  the language surface are non-breaking even when they rename public
  Rust items.

## Reach-out

If a SemVer-relevant change is ambiguous, file an issue tagged
`semver-question`; the maintainer makes the call and documents it here.

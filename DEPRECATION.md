# JWC Deprecation Policy

Companion to [`SEMVER.md`](SEMVER.md). The version-bump rules say
**when** a break can ship; this document says **how to land softly**
before that break.

> Phase 0 deliverable from
> [`PRODUCTION_READINESS_PLAN.md`](PRODUCTION_READINESS_PLAN.md).

---

## Goals

1. A team upgrading `jwc` always sees the break coming **before** their
   code stops working.
2. The warning is precise enough that a `jwc upgrade` codemod is
   feasible — there's a stable name, a stable error code, a stable
   replacement.
3. The maintainer can clean up old surface without surveying every
   downstream user first.

## Minimum warning window

| Track | Rule |
|---|---|
| **Pre-1.0 (0.Y)** | Deprecation in `0.Y` → earliest removal in `0.(Y+1)`. Best effort; flagged in `CHANGELOG.md` under `BREAKING:` if shorter. |
| **Post-1.0** | Deprecation in `X.Y` → earliest removal in `(X+1).0`. The full minor cycle of `X` keeps the deprecated surface alive with a warning. |

A deprecation may be **withdrawn** before removal if usage data or
community feedback shows the cost is higher than the cleanup is worth.
Withdrawal is announced in `CHANGELOG.md` under "Reverted".

## What can be deprecated

- **Builtin functions** — flagged with a `W####` lint warning at every
  call site.
- **Syntax forms** — older form parses, warns, and the parser suggests
  the replacement form in the diagnostic.
- **Environment variables** — read at boot, log a startup warning
  pointing at the replacement.
- **CLI flags / subcommands** — print a stderr warning on use, suggest
  replacement, exit normally.
- **Configuration keys** — when read from `jwcproj.json` or other config,
  emit a warning at boot.
- **Documented behaviour** in `docs/spec/` — pinned spec rules need a
  spec PR plus the same minor-cycle warning before they change.

## What CANNOT be deprecated this way

These require a true breaking change (major bump post-1.0):

- Wire-format changes on HTTP responses for existing routes (use a
  versioned route instead — `/v2/...`).
- Database schema invariants users rely on for migrations.
- Error-code (`E####`) reuse — the code itself is immutable; only its
  text and context can evolve.

## Lifecycle of a deprecation

```
   land deprecation (minor)
        │
        ▼
   warnings shipped              ←─ users see them in build/lint output
        │
        ▼
   ≥ 1 full minor cycle          ←─ codemod prepared (when feasible)
        │
        ▼
   removal (next major, or 0.(Y+1) pre-1.0)
        │
        ▼
   warning removed, old name reusable after a further full minor
```

The "reusable after a further full minor" tail prevents a name being
deprecated, removed, and re-introduced with a different meaning back to
back — too easy to silently break a downstream that pinned to the old
name's behaviour.

## Authoring a deprecation

When deprecating, the same PR should include:

1. **A `W####` warning code** added to `src/error_codes.rs` with a
   short description.
2. **A `CHANGELOG.md` entry** under "Deprecated" naming the old name,
   the replacement, and the targeted removal version.
3. **A test** — usually a conformance case asserting the warning is
   emitted on the deprecated form, paired with the silent-success case
   on the replacement form.
4. **A `docs/spec/` update** if the old form was a spec-pinned construct.
5. **A `jwc upgrade` rule** when the deprecation has a mechanical
   rewrite. (If a codemod is not feasible, say so in the deprecation
   notice so users know they need to hand-port.)

Removal lands in a follow-up release; the removal PR deletes the old
parser branch / builtin entry / config reader and adds the removal note
under "Removed" in `CHANGELOG.md`.

## Open registry

A running list of deprecations and their target removals lives in
`CHANGELOG.md` rather than a separate file, so every release's
deprecation list is alongside the rest of that release's notes. The
first entries land alongside Phase 0 spec extraction work, as legacy
surface is identified.

## Currently flagged (heads-up, not yet `W####`)

These surfaces are documented as **temporary** in
[`SEMVER.md`](SEMVER.md) and are on track to enter the
deprecation lifecycle above as the underlying replacement matures.
None has a `W####` warning code yet — that lands together with the
deprecation PR.

| Surface | Replacement / blocker | Notional removal |
|---|---|---|
| `--no-typecheck` CLI flag | The gradual type checker stabilising (currently soft-fail; needs to become source-of-truth before the escape hatch can go). | Flip to "deprecated" once the checker covers the parser-asserted invariants; targeted removal in **v0.6.0**. |

The "notional removal" column is a planning hint, not a contract —
the actual removal still lands through the lifecycle above (warning
in a minor, removal in the next minor pre-1.0 or the next major
post-1.0).

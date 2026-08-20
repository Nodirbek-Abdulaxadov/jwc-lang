# JWC Deprecation Policy

Companion to [`SEMVER.md`](SEMVER.md). The version-bump rules say
**when** a break can ship; this document says **how to land softly**
before that break.

---

## Goals

1. A team upgrading `jwc` always sees the break coming **before** their
   code stops working.
2. The warning is precise enough to act on mechanically — a stable name,
   a stable code, a named replacement.
3. The maintainer can retire old surface without surveying every
   downstream user first.

## Minimum warning window

| Track | Rule |
|---|---|
| **Pre-1.0 (0.Y)** | Deprecation in `0.Y` → earliest removal in `0.(Y+1)`. Best effort; flagged in `CHANGELOG.md` under `BREAKING:` if shorter. |
| **Post-1.0** | Deprecation in `X.Y` → earliest removal in `(X+1).0`. The full minor cycle of `X` keeps the deprecated surface alive with a warning. |

A deprecation may be **withdrawn** before removal if the cost turns out
higher than the cleanup is worth. Withdrawal is announced in
`CHANGELOG.md` under "Reverted".

### The v0.25.0 exception, and why it is not a precedent

v0.25.0 replaced the grammar and deleted the 0.9.x front-end in one
release. Nothing warned first, because there was no shared vocabulary to
warn in: `entity`, `dbcontext`, `with`, `via`, `validate body`,
`new … from`, `patch`, `group` and `mount` were not deprecated spellings
of v1 constructs, they were a different language.

What that release owed users instead, and paid:

- The compiler **names the replacement** rather than failing to parse.
  `removed_keywords.rs` pins that behaviour, so a 0.9.x program gets a
  diagnostic pointing at the v1 construct, not a syntax error.
- The 0.9.x documentation is archived intact under
  [`docs/archive-0.9/`](docs/archive-0.9/), because 0.9.x binaries are
  deployed and it is what they implement.

A whole-language replacement is a one-time event on the way to 1.0. Post
1.0 it is simply not available: the table above is the rule.

## What can be deprecated

- **Builtins** — a `W####` warning at every call site, naming the
  replacement.
- **Syntax forms** — the old form keeps parsing, warns, and the
  diagnostic suggests the replacement.
- **`server { }` keys** — read at boot, warn at boot, keep working.
  Note that an *unknown* key is `E1206` and refuses the boot; a
  *deprecated* key is a warning. The distinction matters: a typo must
  never be mistaken for a deprecation.
- **CLI flags and subcommands** — warn on stderr, suggest the
  replacement, exit normally.
- **Environment variables** — warn at boot, pointing at the `server { }`
  key or the `env()` call that replaces them.
- **Documented behaviour** in [`docs/spec/v1/`](docs/spec/v1/) — a
  pinned spec rule needs a spec change plus the same warning window.

## What CANNOT be deprecated this way

These require a true breaking change (a major bump post-1.0):

- Wire-format changes to HTTP responses on existing routes — use a
  versioned route instead.
- The two language-level promises in `SEMVER.md`: every value is a bind
  parameter, and a result is `Raw` until projected. There is no soft
  landing from either; a program's security properties depend on them.
- Diagnostic code (`E####` / `W####`) reuse. The code is immutable; only
  its text and context can evolve.
- Schema invariants users have already migrated against.

## Lifecycle of a deprecation

```
   land deprecation (minor)
        │
        ▼
   warnings shipped              ←─ users see them in check / lint output,
        │                          and CI sees them under --deny-warnings
        ▼
   ≥ 1 full minor cycle
        │
        ▼
   removal (next major, or 0.(Y+1) pre-1.0)
        │
        ▼
   warning removed, old name reusable after a further full minor
```

The "reusable after a further full minor" tail stops a name being
deprecated, removed, and reintroduced with a different meaning back to
back — too easy to silently break a downstream that pinned to the old
name's behaviour.

`--deny-warnings` on `jwc check` and `jwc lint` is what makes the middle
of that diagram bite. A team that runs it in CI turns a deprecation into
a build failure on their own schedule, which is the point: they choose
when to care, rather than discovering it at the removal.

## Authoring a deprecation

The same change should carry all five:

1. **A `W####` code**, raised where the deprecated form is accepted.
   Codes are append-only — take the next free one, never reuse.
2. **A `CHANGELOG.md` entry** under "Deprecated" naming the old form,
   the replacement, and the targeted removal version.
3. **A test** asserting the warning is emitted on the deprecated form
   *and* that the form still works — a deprecation that accidentally
   breaks what it deprecates is the failure mode worth pinning.
4. **A spec update** in `docs/spec/v1/` if the old form was normative
   there.
5. **A note in `SEMVER.md`** if the surface was listed as stable, moving
   it to pre-stable.

Removal lands in a follow-up release: the removal change deletes the
parser branch / builtin entry / config reader, and adds a "Removed"
entry naming what replaced it.

## Open registry

The running list of deprecations and their target removals lives in
`CHANGELOG.md`, so each release's deprecations sit alongside the rest of
that release's notes rather than in a file that drifts.

## Currently deprecated

**Nothing.** The v1 surface is one release old — 0.9.7 is its first
tagged release — so nothing in it has been superseded yet.

This is a real "none", not an unmaintained list: the 0.9.x surface that
would otherwise be listed here was removed outright at v0.25.0 rather
than deprecated, for the reason given above, and the v1 surface has not
yet had time to accumulate any.

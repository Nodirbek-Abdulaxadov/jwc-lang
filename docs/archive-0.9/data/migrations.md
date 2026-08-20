---
sidebar_position: 6
description: "Schema changes are explicit in JWC. jwc migrate new diffs your entities against the last snapshot and emits only the ALTER and CREATE TABLE statements needed."
---

# Migrations

Schema is **not** auto-applied from entity definitions — that would
surprise you in production. Migrations are explicit, append-only SQL
files under `migrations/`, tracked in `_jwc_migrations`, and
checksummed so editing an already-applied file is a hard error.

## Create

```bash
jwc migrate new add_users
```

Scaffolds a timestamped pair using the diff between current entities
and the last applied snapshot:

```
migrations/
├── 20260101_120000_add_users.up.sql
└── 20260101_120000_add_users.down.sql
```

The `up.sql` contains only the **diff** (CREATE TABLE for a new
entity, ALTER TABLE for a column add, etc). Diff-free regen prints
`-- no schema changes`.

## Apply

```bash
jwc migrate up                  # apply every pending migration
jwc migrate up --dry-run        # print SQL that *would* run, no commit
```

- Honours `JWC_DATABASE_URL` / `DATABASE_URL` / `--database-url` in that order.
- Uses a Postgres session-level advisory lock (`pg_advisory_lock` key = ASCII `"jwc-mig"`) so two parallel `migrate up` invocations serialise instead of corrupting the schema.
- Records every successful apply in `_jwc_migrations(name, checksum, applied_at)`.
- `--dry-run` prints the SQL that would execute, in order, without acquiring transaction locks or writing to `_jwc_migrations` — ideal for a PR-time CI step.

## Checksum protection

Each row in `_jwc_migrations` carries the SHA-256 of the `.up.sql`
content at apply time. Every subsequent `migrate up` re-hashes the
file on disk and compares; a mismatch fails fast:

```
Error: migration 20260101_120000_add_users.up.sql was modified after
it was applied (stored sha=ab12…, on-disk sha=cd34…) — restore the
original or revert and re-apply
```

This catches the classic "I tweaked the SQL after running it locally"
foot-gun, where the dev box and production schema silently diverge.
Fix: either restore the file to match production, or write a new
migration that brings production to where you want it.

Rows applied before checksums landed (Sprint 4A) are tagged
`legacy_unstamped`; the first successful `migrate up` after upgrading
backfills the checksum for them — no manual intervention needed.

## Status

```bash
jwc migrate status
```

Renders one row per known migration:

```
NAME                                  APPLIED  APPLIED_AT            CHECKSUM
20260101_120000_add_users.up.sql      yes      2026-01-01T12:00:01Z  ok
20260114_090000_add_orders.up.sql     yes      2026-01-14T09:00:02Z  ok (legacy, no checksum stored)
20260201_140000_add_invoices.up.sql   no       —                     pending
20251215_000000_dropped.up.sql        yes      2025-12-15T00:00:00Z  orphan (.up.sql missing)
```

- **`ok`** — checksum matches.
- **`mismatch (stored=…, on-disk=…)`** — the file was edited after apply; `migrate up` will refuse to run until resolved.
- **`pending`** — on disk but not yet applied.
- **`orphan`** — applied row exists but the file is gone (someone deleted the migration).
- **`ok (legacy, no checksum stored)`** — applied before the checksum column existed; the first subsequent apply backfills.

## Rollback

```bash
jwc migrate down --steps 1     # roll back one
jwc migrate down --steps 3     # roll back three
```

Each `down.sql` runs in its own transaction; on failure the rest are skipped.

`--dry-run` works here too — prints what would roll back without
touching the DB.

## List

```bash
jwc migrate list               # offline — reads migrations/ only
```

Useful in CI for sanity-checking; doesn't need a DB connection.

## What the diff generator can do (today)

| Change | Auto-detected |
|---|---|
| New entity | yes — `CREATE TABLE` |
| New column | yes — `ALTER TABLE ADD COLUMN` |
| Type change | yes — `ALTER COLUMN TYPE` (no safety check) |
| New index / unique | partial |
| Drop column / table | no — write by hand |
| Rename | no — write by hand |

For unsupported changes, edit the `.up.sql` and `.down.sql` directly.
The runner runs whatever's in the file — but remember the checksum
guard: don't edit a file that's already been applied to production.

## See also

- [deployment/migrations](../deployment/migrations.md) — deploy-time checklist (CI step, advisory locking, drift recovery).

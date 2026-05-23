---
sidebar_position: 6
---

# Migrations

Schema is **not** auto-applied from entity definitions — that would surprise you in production. Migrations are explicit, append-only SQL files under `migrations/`.

## Create

```bash
jwc migrate new add_users
```

Scaffolds a timestamped pair using the diff between current entities and the last applied snapshot:

```
migrations/
├── 20260101_120000_add_users.up.sql
└── 20260101_120000_add_users.down.sql
```

The `up.sql` contains only the **diff** (CREATE TABLE for a new entity, ALTER TABLE for a column add, etc). Diff-free regen prints `-- no schema changes`.

## Apply

```bash
jwc migrate up
```

- Honours `JWC_DATABASE_URL` / `DATABASE_URL` / `--database-url` in that order.
- Uses a Postgres session-level advisory lock (`pg_advisory_lock` key = ASCII `"jwc-mig"`) so two parallel `migrate up` invocations serialise instead of corrupting the schema.
- Records every successful apply in `_jwc_migrations(name, applied_at)`.

## Rollback

```bash
jwc migrate down --steps 1     # roll back one
jwc migrate down --steps 3     # roll back three
```

Each `down.sql` runs in its own transaction; on failure the rest are skipped.

## List

```bash
jwc migrate list               # offline — reads migrations/ only
```

Useful in CI for sanity-checking; doesn't need a DB connection.

## What the diff generator can do (today)

| Change | Auto-detected |
|---|---|
| New entity | ✅ → `CREATE TABLE` |
| New column | ✅ → `ALTER TABLE ADD COLUMN` |
| Type change | ✅ → `ALTER COLUMN TYPE` (no safety check) |
| New index / unique | ⏳ partial |
| Drop column / table | ⬜ — write by hand |
| Rename | ⬜ — write by hand |

For unsupported changes, edit the `.up.sql` and `.down.sql` directly. The runner runs whatever's in the file.

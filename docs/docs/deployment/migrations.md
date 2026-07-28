---
sidebar_position: 6
description: "Running schema migrations as part of a deployment: ordering against rollout, the advisory lock that serialises concurrent appliers, and rollback."
---

# Migrations (deployment)

The schema-migration workflow lives in
[data/migrations](../data/migrations.md) — that page covers `migrate
new` / `up` / `down` / `status`, the SHA-256 checksum protection, and
`--dry-run`.

This page is a deployment-side checklist on top of those primitives.

## CI step (recommended)

```bash
jwc migrate up --dry-run     # PR-time: print SQL that would run
jwc migrate up               # deploy-time: actually apply
jwc migrate status           # sanity-check after deploy
```

`--dry-run` doesn't acquire transaction locks or write to
`_jwc_migrations` — safe to run on read replicas or against a
production DB without risk.

## Advisory locking

Two parallel `migrate up` invocations (e.g. a rolling deploy where two
pods boot at once) serialise on a Postgres session-level advisory lock
keyed by ASCII `"jwc-mig"`. Whichever pod gets the lock first applies;
the others wait, then re-check and see "0 pending" once they acquire.
No schema corruption, no manual coordination needed.

## Checksum drift

If `migrate up` fails with:

```
migration X was modified after it was applied (stored sha=…, on-disk sha=…)
```

…the most common cause is "someone tweaked the SQL after running it
locally and committed the edit." Fix:

1. `git log -p migrations/<file>.up.sql` to find the post-apply edit.
2. Restore the original content, OR
3. Write a new migration that brings production to the desired state and leave the old file untouched.

Full reference: [data/migrations#checksum-protection](../data/migrations.md#checksum-protection).

## See also

- [data/migrations](../data/migrations.md) — the primary reference.
- [deployment/ci-cd](./ci-cd.md) — wiring `migrate up --dry-run` into a pipeline.

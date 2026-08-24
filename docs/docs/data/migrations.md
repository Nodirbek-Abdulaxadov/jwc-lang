---
sidebar_position: 4
title: "Migrations"
description: "jwc migrate diffs the schema you wrote against the last snapshot and writes the SQL. Offline, forward-only, and verifiable against a live database."
---

# Migrations

The schema in source is the truth. `jwc migrate new` diffs it against the
last snapshot and writes the SQL that closes the gap.

```bash
jwc migrate new add-published .     # write the next up/down pair
jwc migrate up .                    # apply what is pending
jwc migrate status .                # what is applied, pending, or drifted
jwc migrate verify .                # every constraint and index, by name
jwc migrate down .                  # roll back, newest first
```

## Offline by design

`migrate new` never touches a database. The previous state comes from the
last `.snapshot.json` under `migrations/`, so generating a migration works
on a laptop with no Postgres running, in CI, and on a branch whose
database does not exist yet.

Each migration is three files:

```
1781605087_add_published.up.sql
1781605087_add_published.down.sql
1781605087_add_published.snapshot.json
```

The snapshot is what the *next* `migrate new` diffs against. It is part of
the commit, and it is why two people generating migrations on two branches
get a conflict in git rather than a silent divergence in the database.

## Phases

A generated migration runs in a fixed order:

1. drop views
2. schemas and types
3. tables and columns
4. data
5. constraints
6. indexes
7. functions and triggers
8. comments
9. create views
10. destructive

Everything is in one transaction. A column added and a constraint on it
land together or not at all.

`jwc migrate new --explain` prints the plan with the source line each step
came from, before writing anything.

## Renames

A column that disappears and another that appears look identical to a
diff — the difference is that one is a rename and the other loses data. Say
which:

```jwc no-compile
display_name varchar(80) was "name";
```

`was` makes it `ALTER TABLE … RENAME COLUMN`. Without it, the diff drops
one column and adds another, and the rows in it are gone.

## Verify

```bash
jwc migrate verify .
```

Compares the constraint and index names the binary expects against the
ones the database holds, and names each mismatch:

```
public.task: constraint `fk_task__columnId` is missing
public.task: index `ix_task__projectId_position` is missing
```

This is the check that catches a database changed by hand, and the one to
run in a deploy's readiness gate. It is also how a schema ported from an
older version tells you exactly what the port added.

## Applying

`jwc migrate up` takes an advisory lock, so two pods starting at once do
not both apply the same migration. Applied migrations are recorded in
`_jwc_migrations` with a checksum: editing a migration that has already
run is drift, and `status` says so rather than re-running it.

Forward-only. A `down` file is written for every migration and `migrate
down` runs it, but rolling back a deployed migration is a decision, not a
routine — a down that drops a column drops the data in it.

# migrations.md — snapshots, diffing, phases, renames

Normative. Closes gaps **#23**, **#24**, **#25**, **#26**, **#27**, **#28**,
**#33**, and **N1**/**N10**'s diff half. Implemented in v0.26.0; specified
here so v0.22.0's DDL and v0.21.0's `was` marker mean something fixed.

---

## 1. Model

`jwc migrate new <name>` is **offline**. It never connects to a database. It
reads the last snapshot, computes the current one from source, diffs, and
writes a migration pair:

```
migrations/
  0007_add_region.up.sql
  0007_add_region.down.sql
  0007_add_region.snapshot.json
```

`jwc migrate up` / `down` apply files in order under a session advisory lock,
recording applied names in `_jwc_migrations`.

---

## 2. The snapshot

The snapshot is the authoritative previous state. It is JSON, checked in,
and covers **seven** object classes — all of them, because anything not
snapshotted silently never drifts:

1. schemas
2. enum types (`of` form) and their member order
3. tables: columns (type, nullability, default, identity, physical name),
   primary key, unique constraints, check constraints, foreign keys
4. indexes, including `WHERE` predicates in canonical form (schema §4.3)
5. touch functions and triggers (`on update now()`, schema §6)
6. comments (`COMMENT ON`, schema §7)
7. views, with their dependency edges

Each snapshot records the **constraint-naming scheme version** (schema §8.2),
so a future `v2` scheme can be adopted without renaming live constraints.

Reading the previous state from a `.snapshot.json` rather than by re-parsing
the last `.up.sql` removes an entire class of round-trip bug: the parser only
has to understand JSON it wrote.

---

## 3. Diff output

The diff produces a list of typed operations, each carrying its source
location, and lowers them to SQL in the phase order of §4. Operations:

`create_schema`, `create_enum`, `add_enum_value`, `create_table`,
`add_column`, `alter_column_type`, `set_not_null`, `drop_not_null`,
`set_default`, `drop_default`, `set_identity`, `drop_identity`,
`rename_column`, `rename_table`, `add_constraint`, `drop_constraint`,
`rename_constraint`, `create_index`, `drop_index`, `rename_index`,
`create_function`, `drop_function`, `create_trigger`, `drop_trigger`,
`comment_on`, `create_view`, `drop_view`, `drop_column`, `drop_table`.

`jwc migrate new --explain` prints each with the declaration that caused it.
A drop has no declaration — its cause is an absence — and prints without one
rather than borrowing a line number from somewhere else.

The three rename operations exist because constraint and index names are
*generated* from table + columns + canonical predicate (schema §8). Matching
them by name would turn renaming a column into a drop and rebuild of every
constraint on it — a long lock on a large table for a cosmetic change — so
the diff matches them on their **bodies** and renames when only the name
moved. The same match is what lets §2's scheme version mean something: when
the snapshot was written under an older scheme, the match still succeeds and
the rename is suppressed, so adopting a `v2` scheme renames nothing live.

---

## 4. Phases (#24, #33)

A migration file emits in this order, and each phase is internally sorted:

| Phase | Contents |
|---|---|
| 0 | `DROP VIEW` for every view whose dependencies this migration touches |
| 1 | `CREATE SCHEMA`, `CREATE TYPE` |
| 2 | `CREATE TABLE`, `ADD COLUMN`, type changes, defaults, nullability |
| 3 | data movement (backfills declared by the author, §7) |
| 4 | constraints: `ADD CONSTRAINT` for PK, unique, check, **then all FKs** |
| 5 | indexes |
| 6 | functions and triggers |
| 7 | comments on tables and columns |
| 8 | `CREATE VIEW` for everything dropped in phase 0, in dependency order, each followed by its own `COMMENT ON VIEW` |
| 9 | destructive: `DROP CONSTRAINT`, `DROP INDEX`, `DROP COLUMN`, `DROP TABLE` |

Phase 0/8 is the answer to #24: views are real objects that block ordinary
`ALTER`s, so they are dropped and rebuilt around the change rather than
making the change unrunnable. Phase 4's separate FK pass is the same
mechanism that resolves cross-schema cycles in `gen-sql` (schema §9).

A view's comment travels with the view rather than sitting in phase 7:
`DROP VIEW` takes the comment with it, and a `COMMENT ON VIEW` in phase 7
would name an object phase 8 has not created yet.

The drop set is a **fixed point**, not one pass. A view that reads an
altered relation comes down; that makes the view itself an alteration, so
anything reading *it* comes down too — and `DROP VIEW` without `CASCADE`
refuses while a dependent still stands. Declaration order is not dependency
order, so a single sweep leaves the outer view in place and the migration
fails to apply.

Two of the ten phases contain drops that are not destructive. An object can
share a name with the one replacing it — a table `check` is numbered by
ordinal, so editing its expression keeps the name, and so does an index
whose `nulls` order changed. A drop that exists only to make room for an add
travels **with** the add, in phase 4 or 5; only a drop with nothing
replacing it waits for phase 9.

Phase 9 is last so that a failure anywhere earlier leaves data intact.

---

## 5. Transactions and `ADD VALUE` (#26)

5.1 Every migration file is wrapped in `BEGIN`/`COMMIT` by default.

5.2 `ALTER TYPE … ADD VALUE` cannot run inside a transaction block. A
migration containing one is emitted as its **own file**, marked:

```sql
-- jwc:no-transaction
ALTER TYPE billing.invoice_status ADD VALUE IF NOT EXISTS 'refunded';
```

The applier honours the marker and runs that file outside a transaction. A
`no-transaction` file may contain nothing else (`E1101`), so a partial
failure cannot leave half a schema change applied.

5.3 Removing a member is **refused** (`E1102`) with the manual recipe
printed:

```
E1102: enum App.billing.InvoiceStatus removes value 'void'
  Postgres cannot drop an enum value. The safe sequence is:

    1. CREATE TYPE billing.invoice_status_v2 AS ENUM ('draft','open','paid');
    2. SELECT count(*) FROM billing.invoices WHERE status = 'void';  -- must be 0
    3. ALTER TABLE billing.invoices
         ALTER COLUMN status TYPE billing.invoice_status_v2
         USING status::text::billing.invoice_status_v2;
    4. DROP TYPE billing.invoice_status;
    5. ALTER TYPE billing.invoice_status_v2 RENAME TO invoice_status;

  Columns using this type: billing.invoices.status
```

Refusal plus a recipe loses no data; automated rebuild across an unknown
cross-schema column map does. `DEFERRED-3`.

Member **order** in an `of` enum is not semantically significant to JWC
(types §3.5 forbids ordering comparisons), so reordering the declaration
produces no operation at all. Postgres cannot move a member, so the snapshot
records the order the *database* will have — otherwise the next
`migrate new` would diff the same permutation again, forever.

A value written into the middle of the declaration is a different matter: it
is new, so `ADD VALUE … BEFORE` puts it where the source says, and `\dT+`
shows what was written.

---

## 6. Renames are declared, never inferred (#27)

6.1 There is no rename inference. A column that disappears and a column that
appears are `drop_column` + `add_column` — total data loss, and the generator
will not guess otherwise.

6.2 A rename is declared with `was`:

```jwc no-compile
table Accounts of App.auth {
    display_name varchar(80) was "full_name";
}

table Orgs of App.org was "tenants" { … }
```

which produces `ALTER TABLE … RENAME COLUMN full_name TO display_name`.

6.3 A `was` whose old name is not in the previous snapshot is `E1103`
(nothing to rename) — this catches a `was` left behind after the migration
that used it, which would otherwise be silent.

6.4 `was` is removed from the source in the migration **after** the one that
applied it. `jwc migrate new` reports stale `was` markers as `W1101` — not
`jwc check`, which has no snapshot to judge staleness against. A marker is
stale exactly when the new name is already in the previous snapshot.

6.5 Rename plus type change in one migration is refused (`E1104`): the two
are separable and the combined failure mode is not diagnosable from the
resulting error.

---

## 7. Backfills and `NOT NULL` (#23)

7.1 `jwc migrate new` refuses to add a `NOT NULL` column with no default
(schema §10, `E0440`).

7.2 The expand/contract path is two migrations, and the author writes the
backfill:

```
0008_add_region.up.sql        -- ADD COLUMN region varchar(20)   (nullable)
0008_add_region.data.sql      -- UPDATE org.orgs SET region = 'us' WHERE region IS NULL
0009_region_required.up.sql   -- ALTER COLUMN region SET NOT NULL
```

A `.data.sql` sidecar, if present, runs in phase 3 of its migration. It is
hand-written and never generated: the generator has no way to know the right
value.

7.3 `ALTER COLUMN … SET NOT NULL` on a table with nulls fails loudly at apply
time. That is the correct outcome and the reason for the two-step shape.

---

## 8. Partial index predicates in the diff (#25)

Predicates are canonicalised at compile time (schema §4.3) and stored in the
snapshot in that form. Adding, removing or editing a `where` on a `unique`
or `index` therefore produces `drop_index` + `create_index` with a new name
(the hash segment changed), not silence.

Two logically identical predicates written differently (`a and b` vs
`b and a`) canonicalise to the same text and produce no migration.

---

## 9. `down`

9.1 A reversible operation gets a real inverse.

9.2 A destructive operation — `drop_column`, `drop_table`, `add_enum_value`,
a narrowing `alter_column_type` — emits:

```sql
-- irreversible: dropping org.orgs.region loses data
-- (no down migration is generated for this statement)
```

`migrate down` on such a file stops with the marker's text. Promising
reversibility that cannot exist is worse than refusing it (ROADMAP §7).

---

## 10. Determinism and testing

10.1 Two runs of `jwc migrate new` on the same source and snapshot produce
byte-identical output.

10.2 The round-trip property — *a database you migrated into a shape is the
same database as one created in that shape* — is a property test: a random
walk of schema edits applied one migration at a time, against `gen-sql` of
the final source applied to an empty database, compared with
`pg_dump --schema-only`. 20 random sequences per change, 200 for a release
(ROADMAP §10).

Four things are normalised away, and only these four, each because no
migration can control it:

| Normalised | Why |
|---|---|
| `\restrict` / `\unrestrict` | a random nonce per `pg_dump` run |
| column order | Postgres appends; a column written into the middle of a declaration sits at the end of a migrated table. No `ALTER` moves it and nothing depends on it |
| statement order | `pg_dump` emits by OID, which is creation order — migration order on one side, declaration order on the other |
| an identity column's sequence name | Postgres does not rename it when the table is renamed |

Types, nullability, defaults, identity, every constraint and index with its
generated name and canonical predicate, enum members *in order*, view
bodies, triggers and comments are all compared literally.

The walk's first two edits are pinned by the sequence number so that a run
covers the whole edit vocabulary rather than leaving the tail to luck, and
the test asserts afterwards that every operation class actually occurred. A
generator that stopped generating would otherwise read as a pass.

---

## 11. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1101` | `no-transaction` migration contains other statements |
| `E1102` | enum value removal or reorder |
| `E1103` | `was` names something not in the snapshot |
| `E1104` | rename combined with a type change |
| `W1101` | stale `was` marker |

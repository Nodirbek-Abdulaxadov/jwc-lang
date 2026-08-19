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
recording applied names in `_jwc_migrations`. The applier is unchanged from
0.9.x (ROADMAP §0).

---

## 2. The snapshot

The snapshot is the authoritative previous state. It is JSON, checked in,
and covers **six** object classes — all of them, because anything not
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
`set_default`, `drop_default`, `rename_column`, `rename_table`,
`add_constraint`, `drop_constraint`, `create_index`, `drop_index`,
`create_function`, `drop_function`, `create_trigger`, `drop_trigger`,
`comment_on`, `create_view`, `drop_view`, `drop_column`, `drop_table`.

`jwc migrate new --explain` prints each with the declaration that caused it.

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
| 7 | comments |
| 8 | `CREATE VIEW` for everything dropped in phase 0, in dependency order |
| 9 | destructive: `DROP CONSTRAINT`, `DROP INDEX`, `DROP COLUMN`, `DROP TABLE` |

Phase 0/8 is the answer to #24: views are real objects that block ordinary
`ALTER`s, so they are dropped and rebuilt around the change rather than
making the change unrunnable. Phase 4's separate FK pass is the same
mechanism that resolves cross-schema cycles in `gen-sql` (schema §9).

Phase 9 is last so that a failure anywhere earlier leaves data intact.

---

## 5. Transactions and `ADD VALUE` (#26)

5.1 Every migration file is wrapped in `BEGIN`/`COMMIT` by default.

5.2 `ALTER TYPE … ADD VALUE` cannot run inside a transaction block. A
migration containing one is emitted as its **own file**, marked:

```sql
-- jwc:no-transaction
ALTER TYPE app_billing.invoice_status ADD VALUE IF NOT EXISTS 'refunded';
```

The applier honours the marker and runs that file outside a transaction. A
`no-transaction` file may contain nothing else (`E1101`), so a partial
failure cannot leave half a schema change applied.

5.3 Reordering members, or removing one, is **refused** (`E1102`) with the
manual recipe printed:

```
E1102: enum App.billing.InvoiceStatus removes value 'void'
  Postgres cannot drop an enum value. The safe sequence is:

    1. CREATE TYPE app_billing.invoice_status_v2 AS ENUM ('draft','open','paid');
    2. SELECT count(*) FROM app_billing.invoices WHERE status = 'void';  -- must be 0
    3. ALTER TABLE app_billing.invoices
         ALTER COLUMN status TYPE app_billing.invoice_status_v2
         USING status::text::app_billing.invoice_status_v2;
    4. DROP TYPE app_billing.invoice_status;
    5. ALTER TYPE app_billing.invoice_status_v2 RENAME TO invoice_status;

  Columns using this type: app_billing.invoices.status
```

Refusal plus a recipe loses no data; automated rebuild across an unknown
cross-schema column map does. `DEFERRED-3`.

Member **order** in an `of` enum is not semantically significant to JWC
(types §3.5 forbids ordering comparisons), so reordering the declaration
produces no operation at all.

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
applied it. `jwc lint` reports stale `was` markers as `W1101`.

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
0008_add_region.data.sql      -- UPDATE app_org.orgs SET region = 'us' WHERE region IS NULL
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
-- irreversible: dropping app_org.orgs.region loses data
-- (no down migration is generated for this statement)
```

`migrate down` on such a file stops with the marker's text. Promising
reversibility that cannot exist is worse than refusing it (ROADMAP §7).

---

## 10. Determinism and testing

10.1 Two runs of `jwc migrate new` on the same source and snapshot produce
byte-identical output.

10.2 The round-trip property — *apply every migration in order to an empty
database, snapshot the result, and it equals the source's snapshot* — is a
property test: 20 random operation sequences per PR, 200 nightly
(ROADMAP §10).

---

## 11. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1101` | `no-transaction` migration contains other statements |
| `E1102` | enum value removal or reorder |
| `E1103` | `was` names something not in the snapshot |
| `E1104` | rename combined with a type change |
| `W1101` | stale `was` marker |

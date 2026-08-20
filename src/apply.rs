//! Applying migrations — `up`, `down`, `status`, `verify`.
//!
//! The generator half is `migrate.rs` and never touches a database. This
//! half does nothing else.
//!
//! ## The lock
//!
//! Every command takes a **session-level advisory lock** before it looks at
//! anything. Two deploys rolling out at once then serialise instead of
//! interleaving half a migration each, and a session lock (rather than a
//! transaction one) also covers the `no-transaction` files, which have no
//! transaction to hang a lock on.
//!
//! ## The bookkeeping table
//!
//! `_jwc_migrations` records what ran, when, and the checksum of the file
//! that ran. The checksum is what turns "someone edited an applied
//! migration" from a silent divergence into a line in `jwc migrate status`.

use anyhow::{bail, Context, Result};
use tokio_postgres::Client;

use crate::migrate::{self, DATA_MARKER, IRREVERSIBLE, NO_TRANSACTION};
use crate::snapshot::{self, MigrationFile, Snapshot};
use std::path::Path;

/// ASCII `jwc-mig`, big-endian, as a `bigint`. Any constant would do; this
/// one is greppable in `pg_locks`.
pub const LOCK_KEY: i64 = 0x006a77632d6d6967;

pub const TABLE: &str = "public._jwc_migrations";

/// migrations.md §11. Unlike every other code in the language this one is
/// not a `Diagnostic`: it names a fault in a checked-in SQL file, which has
/// no source span to point a caret at. It surfaces as an apply-time error
/// with the file named instead.
pub const E1101: &str = "E1101";

const CREATE_TABLE: &str = "\
CREATE TABLE IF NOT EXISTS public._jwc_migrations (
    name       text PRIMARY KEY,
    checksum   text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub checksum: String,
}

/// What `jwc migrate status` prints.
pub struct Status {
    pub applied: Vec<Record>,
    pub pending: Vec<String>,
    /// An applied migration whose file changed, or vanished. Either is a
    /// database that is not the shape the repository says it is.
    pub drift: Vec<String>,
}

pub async fn lock(client: &Client) -> Result<()> {
    client
        .execute("SELECT pg_advisory_lock($1)", &[&LOCK_KEY])
        .await
        .context("could not take the migration advisory lock")?;
    Ok(())
}

pub async fn unlock(client: &Client) -> Result<()> {
    client
        .execute("SELECT pg_advisory_unlock($1)", &[&LOCK_KEY])
        .await?;
    Ok(())
}

pub async fn ensure_table(client: &Client) -> Result<()> {
    client.batch_execute(CREATE_TABLE).await?;
    Ok(())
}

pub async fn applied(client: &Client) -> Result<Vec<Record>> {
    let rows = client
        .query(
            &format!("SELECT name, checksum FROM {TABLE} ORDER BY name"),
            &[],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Record {
            name: r.get(0),
            checksum: r.get(1),
        })
        .collect())
}

fn checksum(text: &str) -> String {
    crate::hash::sha256_hex(text)
}

/// The up file with its own transaction control removed and the `.data.sql`
/// sidecar spliced in at phase 3.
///
/// The file carries `BEGIN`/`COMMIT` because it is also meant to be
/// readable and runnable by hand through `psql`. The applier strips them so
/// it can put the bookkeeping row *inside* the same transaction — otherwise
/// a crash between the last statement and the insert would leave a
/// migration applied and unrecorded, and the next `up` would run it twice.
fn body(text: &str, sidecar: Option<&str>) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("BEGIN;") || t.eq_ignore_ascii_case("COMMIT;") {
            continue;
        }
        if t.starts_with(DATA_MARKER) {
            if let Some(data) = sidecar {
                out.push_str(data);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Apply every pending migration, in order.
///
/// `to` stops after that ordinal, which is what a staged rollout wants.
pub async fn up(client: &Client, dir: &Path, to: Option<u32>) -> Result<Vec<String>> {
    lock(client).await?;
    let result = up_locked(client, dir, to).await;
    unlock(client).await?;
    result
}

async fn up_locked(client: &Client, dir: &Path, to: Option<u32>) -> Result<Vec<String>> {
    ensure_table(client).await?;
    let done: Vec<String> = applied(client).await?.into_iter().map(|r| r.name).collect();
    let mut ran = Vec::new();

    for m in snapshot::list(dir) {
        if to.is_some_and(|t| m.ordinal > t) {
            break;
        }
        if done.contains(&m.stem) {
            continue;
        }
        let text =
            std::fs::read_to_string(m.up()).with_context(|| format!("{}", m.up().display()))?;
        let sum = checksum(&text);

        if text.starts_with(NO_TRANSACTION) {
            // §5.2 / E1101. The check is here and not only in the generator
            // because the files are checked in and editable, and this one
            // has no transaction to roll a stray statement back.
            migrate::check_no_transaction(&text)
                .map_err(|e| anyhow::anyhow!("{E1101}: {}: {e}", m.up().display()))?;
            for stmt in migrate::statements(&text) {
                client
                    .batch_execute(&format!("{stmt};"))
                    .await
                    .with_context(|| format!("{}: {stmt}", m.up().display()))?;
            }
            client
                .execute(
                    &format!("INSERT INTO {TABLE} (name, checksum) VALUES ($1, $2)"),
                    &[&m.stem, &sum],
                )
                .await?;
        } else {
            let sidecar = std::fs::read_to_string(m.data()).ok();
            let sql = format!(
                "BEGIN;\n{}\nINSERT INTO {TABLE} (name, checksum) VALUES ('{}', '{}');\nCOMMIT;",
                body(&text, sidecar.as_deref()),
                m.stem.replace('\'', "''"),
                sum
            );
            client
                .batch_execute(&sql)
                .await
                .with_context(|| format!("{} failed to apply", m.up().display()))?;
        }
        ran.push(m.stem);
    }
    Ok(ran)
}

/// Roll back the last `count` applied migrations, newest first.
pub async fn down(client: &Client, dir: &Path, count: usize) -> Result<Vec<String>> {
    lock(client).await?;
    let result = down_locked(client, dir, count).await;
    unlock(client).await?;
    result
}

async fn down_locked(client: &Client, dir: &Path, count: usize) -> Result<Vec<String>> {
    ensure_table(client).await?;
    let done = applied(client).await?;
    let files: Vec<MigrationFile> = snapshot::list(dir);
    let mut undone = Vec::new();

    for r in done.iter().rev().take(count) {
        let Some(m) = files.iter().find(|m| m.stem == r.name) else {
            bail!(
                "{} is applied but its files are gone — nothing to roll back with",
                r.name
            );
        };
        let text =
            std::fs::read_to_string(m.down()).with_context(|| format!("{}", m.down().display()))?;
        if let Some(line) = text.lines().find(|l| l.starts_with(IRREVERSIBLE)) {
            // §9.2. Refusing here is the whole point of the marker: the file
            // says what it cannot undo, and stopping is the honest outcome.
            bail!(
                "{} cannot be rolled back\n  {}",
                r.name,
                line.trim_start_matches("-- ")
            );
        }
        let sql = format!(
            "BEGIN;\n{}\nDELETE FROM {TABLE} WHERE name = '{}';\nCOMMIT;",
            body(&text, None),
            r.name.replace('\'', "''")
        );
        client
            .batch_execute(&sql)
            .await
            .with_context(|| format!("{} failed to roll back", m.down().display()))?;
        undone.push(r.name.clone());
    }
    Ok(undone)
}

pub async fn status(client: &Client, dir: &Path) -> Result<Status> {
    ensure_table(client).await?;
    let applied = applied(client).await?;
    let files = snapshot::list(dir);

    let mut pending = Vec::new();
    let mut drift = Vec::new();
    for m in &files {
        let text = std::fs::read_to_string(m.up()).unwrap_or_default();
        match applied.iter().find(|r| r.name == m.stem) {
            None => pending.push(m.stem.clone()),
            Some(r) if r.checksum != checksum(&text) => drift.push(format!(
                "{} was edited after it was applied — the database is not the shape \
                 this file describes",
                m.stem
            )),
            Some(_) => {}
        }
    }
    for r in &applied {
        if !files.iter().any(|m| m.stem == r.name) {
            drift.push(format!(
                "{} is applied but has no file here — this database has run a \
                 migration this checkout does not have",
                r.name
            ));
        }
    }
    Ok(Status {
        applied,
        pending,
        drift,
    })
}

/// Compare the names the binary expects against the ones Postgres holds
/// (#28).
///
/// Names are generated, deterministically, from table + columns + canonical
/// predicate (schema.md §8) — which is exactly what makes this checkable.
/// A constraint that is missing under its expected name is what turns a
/// unique violation into an unmapped 500 rather than the sentence the
/// author wrote.
pub async fn verify(client: &Client, snap: &Snapshot) -> Result<Vec<String>> {
    let mut problems = Vec::new();

    let rows = client
        .query(
            "SELECT n.nspname, c.relname, con.conname
               FROM pg_constraint con
               JOIN pg_class c ON c.oid = con.conrelid
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname <> ALL ($1::text[])",
            &[&vec![
                "pg_catalog".to_string(),
                "information_schema".to_string(),
            ]],
        )
        .await?;
    let live: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();

    for t in &snap.tables {
        let mut want: Vec<&str> = Vec::new();
        if let Some(pk) = &t.primary_key {
            want.push(&pk.name);
        }
        for u in &t.uniques {
            if u.predicate.is_none() {
                want.push(&u.name);
            }
        }
        for c in &t.checks {
            want.push(&c.name);
        }
        for f in &t.foreign_keys {
            want.push(&f.name);
        }
        for name in want {
            if !live
                .iter()
                .any(|(s, r, c)| s == &t.schema && r == &t.name && c == name)
            {
                problems.push(format!(
                    "{}.{}: constraint `{name}` is missing",
                    t.schema, t.name
                ));
            }
        }
    }

    let rows = client
        .query(
            "SELECT schemaname, tablename, indexname FROM pg_indexes
              WHERE schemaname <> ALL ($1::text[])",
            &[&vec![
                "pg_catalog".to_string(),
                "information_schema".to_string(),
            ]],
        )
        .await?;
    let live_ix: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();

    for t in &snap.tables {
        for u in &t.uniques {
            if u.predicate.is_none() {
                continue;
            }
            if !live_ix
                .iter()
                .any(|(s, r, i)| s == &t.schema && r == &t.name && i == &u.name)
            {
                problems.push(format!(
                    "{}.{}: unique index `{}` is missing",
                    t.schema, t.name, u.name
                ));
            }
        }
        for ix in &t.indexes {
            if !live_ix
                .iter()
                .any(|(s, r, i)| s == &t.schema && r == &t.name && i == &ix.name)
            {
                problems.push(format!(
                    "{}.{}: index `{}` is missing",
                    t.schema, t.name, ix.name
                ));
            }
        }
    }

    for v in &snap.views {
        let found = client
            .query_one(
                "SELECT count(*) FROM pg_views WHERE schemaname = $1 AND viewname = $2",
                &[&v.schema, &v.name],
            )
            .await?;
        if found.get::<_, i64>(0) == 0 {
            problems.push(format!("view `{}.{}` is missing", v.schema, v.name));
        }
    }

    problems.sort();
    Ok(problems)
}

/// The boot check (#33): every column the program reads has to exist.
///
/// Wrapping PG's `42703` in a 500 at request time tells an operator that
/// something broke; naming the missing column at startup tells them what.
/// It reads `information_schema`, which every role can, so it costs one
/// query and no privileges.
pub async fn check_live_schema(client: &Client, snap: &Snapshot) -> Result<Vec<String>> {
    let rows = client
        .query(
            "SELECT table_schema, table_name, column_name FROM information_schema.columns",
            &[],
        )
        .await?;
    let live: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();

    let mut missing = Vec::new();
    for t in &snap.tables {
        if !live.iter().any(|(s, r, _)| s == &t.schema && r == &t.name) {
            missing.push(format!("table {}.{} does not exist", t.schema, t.name));
            continue;
        }
        for c in &t.columns {
            if !live
                .iter()
                .any(|(s, r, col)| s == &t.schema && r == &t.name && col == &c.name)
            {
                missing.push(format!(
                    "column {}.{}.{} does not exist",
                    t.schema, t.name, c.name
                ));
            }
        }
    }
    missing.sort();
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_key_spells_its_own_name() {
        // Greppable in `pg_locks`: `SELECT objid FROM pg_locks WHERE
        // locktype = 'advisory'` gives a number a reader can decode.
        let bytes = LOCK_KEY.to_be_bytes();
        let text: String = bytes
            .iter()
            .filter(|b| **b != 0)
            .map(|b| *b as char)
            .collect();
        assert_eq!(text, "jwc-mig");
    }

    #[test]
    fn the_body_loses_its_transaction_and_gains_the_sidecar() {
        let up = format!(
            "BEGIN;\n\
             ALTER TABLE org.orgs ADD COLUMN region varchar(20);\n\
             {DATA_MARKER} 0008_add_region\n\
             -- a comment about the sidecar\n\
             COMMIT;\n"
        );
        let out = body(&up, Some("UPDATE org.orgs SET region = 'us';"));
        assert!(!out.contains("BEGIN"), "{out}");
        assert!(!out.contains("COMMIT"), "{out}");
        assert!(out.contains("UPDATE org.orgs SET region = 'us';"), "{out}");
        // The marker itself is replaced, not kept alongside.
        assert!(!out.contains(DATA_MARKER), "{out}");

        // With no sidecar the marker simply disappears.
        let out = body(&up, None);
        assert!(!out.contains(DATA_MARKER), "{out}");
        assert!(out.contains("ADD COLUMN region"), "{out}");
    }
}

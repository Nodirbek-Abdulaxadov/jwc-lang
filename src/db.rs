//! The v1 database layer.
//!
//! Thin over `crate::engine` (the deadpool-postgres pool, which is
//! language-independent infrastructure — ROADMAP §0). Its one real job is
//! turning a Postgres integrity error into the shape errors.md §6 needs:
//! the violated constraint's **generated name**, which is how a violation
//! finds its message.

use crate::model::SchemaModel;
use crate::sql::Shape;
use anyhow::anyhow;
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio_postgres::types::ToSql;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConstraintKind {
    Unique,
    Check,
    NotNull,
}

#[derive(Debug)]
pub enum DbError {
    Constraint {
        name: String,
        /// The declared message, when the constraint carried one
        /// (schema.md §4.3, §4.4).
        message: Option<String>,
        kind: ConstraintKind,
    },
    ForeignKey,
    Other(anyhow::Error),
}

/// constraint name -> (message, kind), built once from the schema model.
/// The name is the only stable link between a runtime violation and the
/// sentence the author wrote (schema.md §8.3).
static MESSAGES: OnceLock<HashMap<String, (Option<String>, ConstraintKind)>> = OnceLock::new();

pub fn install_messages(model: &SchemaModel) {
    let mut map = HashMap::new();
    for t in &model.tables {
        for u in &t.uniques {
            map.insert(u.name.clone(), (u.message.clone(), ConstraintKind::Unique));
        }
        for c in &t.checks {
            map.insert(c.name.clone(), (c.message.clone(), ConstraintKind::Check));
        }
    }
    let _ = MESSAGES.set(map);
}

pub async fn run(
    sql: &str,
    binds: &[Option<String>],
    shape: Shape,
) -> Result<Option<String>, DbError> {
    // Inside a `transaction { }` the task is pinned to the connection the
    // `BEGIN` was issued on. Taking a fresh one from the pool here would
    // run the statement outside the transaction it is supposed to be part
    // of — the block would commit nothing and roll back nothing.
    if let Some(cell) = crate::engine::pinned_connection() {
        let mut held = cell.lock().await;
        if let Some(conn) = held.as_mut() {
            return run_on(conn, sql, binds, shape).await;
        }
    }
    let conn = crate::engine::get_connection()
        .await
        .map_err(DbError::Other)?;
    run_on(&conn, sql, binds, shape).await
}

async fn run_on(
    conn: &tokio_postgres::Client,
    sql: &str,
    binds: &[Option<String>],
    shape: Shape,
) -> Result<Option<String>, DbError> {
    let params: Vec<&(dyn ToSql + Sync)> = binds.iter().map(|b| b as &(dyn ToSql + Sync)).collect();

    let started = std::time::Instant::now();
    match shape {
        Shape::None => {
            let n = conn.execute(sql, &params).await.map_err(classify)?;
            log_sql(sql, binds, started, n as usize);
            Ok(None)
        }
        Shape::First | Shape::Rows => {
            let rows = conn.query(sql, &params).await.map_err(classify)?;
            let text = match rows.first() {
                None => None,
                // Not `unwrap_or(None)`. Every statement this layer sends
                // projects a single text column — the query compiler wraps
                // in `json_agg(...)::text` / `row_to_json(...)::text`, and
                // so does `raw()`. A non-text first column therefore means
                // the generator emitted something it should not have, and
                // swallowing it reports the far more damaging lie that the
                // query matched nothing: `Shape::First` answers 404 and
                // `Shape::Rows` answers `[]`, both indistinguishable from
                // an empty table.
                Some(r) => r.try_get::<_, Option<String>>(0).map_err(|e| {
                    DbError::Other(anyhow!(
                        "statement projected a first column that is not text: {e}"
                    ))
                })?,
            };
            if log_enabled() {
                let n = match shape {
                    Shape::Rows => json_len(text.as_deref().unwrap_or("[]")),
                    _ => usize::from(text.is_some()),
                };
                log_sql(sql, binds, started, n);
            }
            Ok(text)
        }
    }
}

/// `JWC_LOG_SQL=1`, read once. A per-statement `std::env::var` in the hot
/// path would cost more than the query on a cached plan.
fn log_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("JWC_LOG_SQL").as_deref() == Ok("1"))
}

/// One line per statement: duration, row count, the statement, then every
/// bound parameter (tooling.md §2.1).
///
/// All four, because each answers a different question and three of them are
/// useless alone — a slow statement with no parameters cannot be reproduced,
/// and a statement with no row count cannot be told from one that matched
/// nothing.
fn log_sql(sql: &str, binds: &[Option<String>], started: std::time::Instant, rows: usize) {
    if !log_enabled() {
        return;
    }
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let params = binds
        .iter()
        .enumerate()
        .map(|(i, b)| match b {
            // `null` never as an empty string: the difference between the
            // two is the whole subject of `==?` (queries.md §4.4).
            None => format!("${}=null", i + 1),
            Some(v) => format!("${}='{}'", i + 1, v.replace('\'', "''")),
        })
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "[sql] {ms:.2}ms {rows} row{}  {flat}  {params}",
        if rows == 1 { "" } else { "s" }
    );
}

/// Elements in a top-level JSON array, without building the values.
fn json_len(text: &str) -> usize {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut count = 0usize;
    let mut any = false;
    for c in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => {
                depth += 1;
                if depth == 2 {
                    any = true;
                }
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 1 => count += 1,
            _ => {}
        }
    }
    if any || count > 0 {
        count + 1
    } else {
        0
    }
}

/// A page: the rows, their ordering tuples, and whether one more exists.
///
/// Three columns rather than one JSON object, because `items` must reach
/// the response as the text Postgres produced. Parsing it here to take the
/// envelope apart would re-serialise it with the keys sorted, and the
/// projection order **is** the key order (queries.md §7.2).
pub async fn run_page(
    sql: &str,
    binds: &[Option<String>],
) -> Result<(String, String, bool), DbError> {
    if let Some(cell) = crate::engine::pinned_connection() {
        let mut held = cell.lock().await;
        if let Some(conn) = held.as_mut() {
            return page_on(conn, sql, binds).await;
        }
    }
    let conn = crate::engine::get_connection()
        .await
        .map_err(DbError::Other)?;
    page_on(&conn, sql, binds).await
}

async fn page_on(
    conn: &tokio_postgres::Client,
    sql: &str,
    binds: &[Option<String>],
) -> Result<(String, String, bool), DbError> {
    let params: Vec<&(dyn ToSql + Sync)> = binds.iter().map(|b| b as &(dyn ToSql + Sync)).collect();
    let started = std::time::Instant::now();
    let rows = conn.query(sql, &params).await.map_err(classify)?;
    let Some(r) = rows.first() else {
        log_sql(sql, binds, started, 0);
        return Ok(("[]".into(), "[]".into(), false));
    };
    let items = r
        .try_get::<_, Option<String>>(0)
        .unwrap_or(None)
        .unwrap_or_else(|| "[]".into());
    if log_enabled() {
        log_sql(sql, binds, started, json_len(&items));
    }
    let _ = &items;
    Ok((
        items,
        r.try_get::<_, Option<String>>(1)
            .unwrap_or(None)
            .unwrap_or_else(|| "[]".into()),
        r.try_get::<_, Option<bool>>(2)
            .unwrap_or(None)
            .unwrap_or(false),
    ))
}

fn classify(e: tokio_postgres::Error) -> DbError {
    let Some(db) = e.as_db_error() else {
        return DbError::Other(anyhow!(e.to_string()));
    };
    let code = db.code().code();
    let name = db.constraint().unwrap_or_default().to_string();
    match code {
        // 23503 foreign_key_violation
        "23503" => DbError::ForeignKey,
        // 23505 unique_violation, 23514 check_violation, 23502 not_null
        "23505" | "23514" | "23502" => {
            let lookup = MESSAGES
                .get()
                .and_then(|m| m.get(&name))
                .cloned()
                .unwrap_or((
                    None,
                    match code {
                        "23505" => ConstraintKind::Unique,
                        "23502" => ConstraintKind::NotNull,
                        _ => ConstraintKind::Check,
                    },
                ));
            DbError::Constraint {
                name,
                message: lookup.0,
                kind: lookup.1,
            }
        }
        _ => DbError::Other(anyhow!(db.message().to_string())),
    }
}

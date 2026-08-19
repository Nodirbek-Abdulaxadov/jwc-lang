//! The v1 database layer.
//!
//! Thin over `crate::engine` (the deadpool-postgres pool, which is
//! language-independent infrastructure — ROADMAP §0). Its one real job is
//! turning a Postgres integrity error into the shape errors.md §6 needs:
//! the violated constraint's **generated name**, which is how a violation
//! finds its message.

use super::model::SchemaModel;
use super::sql::Shape;
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
            map.insert(
                u.name.clone(),
                (u.message.clone(), ConstraintKind::Unique),
            );
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
    let conn = crate::engine::get_connection()
        .await
        .map_err(DbError::Other)?;

    let params: Vec<&(dyn ToSql + Sync)> = binds
        .iter()
        .map(|b| b as &(dyn ToSql + Sync))
        .collect();

    match shape {
        Shape::None => {
            conn.execute(sql, &params).await.map_err(classify)?;
            Ok(None)
        }
        Shape::First | Shape::Rows => {
            let rows = conn.query(sql, &params).await.map_err(classify)?;
            match rows.first() {
                None => Ok(None),
                Some(r) => Ok(r.try_get::<_, Option<String>>(0).unwrap_or(None)),
            }
        }
    }
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

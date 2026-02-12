use std::env;

use anyhow::{anyhow, bail, Context, Result};
use postgres::{Client, NoTls};
use postgres::types::{ToSql, Type};
use sha2::{Digest, Sha256};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::ast::Program;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub name: String,
    pub sql_sha256: String,
    pub applied_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    AlreadyApplied,
}

pub fn resolve_db_url(
    program: &Program,
    cli_db_url: Option<String>,
    dbcontext: Option<String>,
) -> Result<String> {
    if let Some(url) = cli_db_url {
        return Ok(normalize_db_url(&url));
    }

    if let Ok(url) = env::var("JWC_DATABASE_URL") {
        if !url.trim().is_empty() {
            return Ok(normalize_db_url(&url));
        }
    }

    let ctx_url = match dbcontext {
        Some(name) => {
            let ctx = program
                .dbcontexts
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&name))
                .ok_or_else(|| anyhow!("Unknown dbcontext: {name}"))?;
            ctx.url.clone().ok_or_else(|| {
                anyhow!(
                    "dbcontext '{}' has no url. Use `--db-url`, set `JWC_DATABASE_URL`, or add a url string after the driver",
                    ctx.name
                )
            })?
        }
        None => {
            let mut with_url = program.dbcontexts.iter().filter(|c| c.url.is_some());
            let first = with_url.next();
            let second = with_url.next();
            match (first, second) {
                (Some(one), None) => one.url.clone().unwrap(),
                (Some(_), Some(_)) => {
                    bail!(
                        "Multiple dbcontexts have urls; specify which one with `--dbcontext <name>`"
                    )
                }
                (None, _) => {
                    bail!(
                        "No database url provided. Use `--db-url`, set `JWC_DATABASE_URL`, or add a url to a dbcontext (e.g. `dbcontext AppDb : Postgres \"postgres://...\";`)"
                    )
                }
            }
        }
    };

    Ok(normalize_db_url(&ctx_url))
}

fn normalize_db_url(s: &str) -> String {
    let raw = s.trim();
    if raw.is_empty() {
        return raw.to_string();
    }

    // Already a URL.
    if raw.contains("://") {
        return raw.to_string();
    }

    // Convert common Npgsql-style `Host=...;Port=...;Database=...;Username=...;Password=...;`
    // into libpq style `host=... port=... dbname=... user=... password=...`.
    if raw.contains(';') && raw.contains('=') {
        let mut parts: Vec<String> = Vec::new();
        for seg in raw.split(';') {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            let mut it = seg.splitn(2, '=');
            let key = it.next().unwrap_or("").trim();
            let val = it.next().unwrap_or("").trim();
            if key.is_empty() || val.is_empty() {
                continue;
            }

            let k = key
                .to_ascii_lowercase()
                .replace(' ', "")
                .replace('_', "");
            let mapped = match k.as_str() {
                "host" | "server" => "host",
                "port" => "port",
                "database" | "dbname" | "initialcatalog" => "dbname",
                "username" | "userid" | "user" => "user",
                "password" | "pwd" => "password",
                "sslmode" => "sslmode",
                _ => continue,
            };

            let mut v = val.to_string();
            if v.contains(char::is_whitespace) || v.contains('"') || v.contains('\\') {
                v = v.replace('\\', "\\\\");
                v = v.replace('"', "\\\"");
                v = format!("\"{}\"", v);
            }
            parts.push(format!("{}={}", mapped, v));
        }

        if !parts.is_empty() {
            return parts.join(" ");
        }
    }

    // Already looks like libpq format: `key=value key=value` (space-separated, not semicolon-separated).
    if raw.contains(' ') && raw.contains('=') {
        return raw.to_string();
    }

    raw.to_string()
}

pub fn default_migration_name(old_source: &str, new_source: &str) -> String {
    let old_hash = sha256_hex(old_source);
    let new_hash = sha256_hex(new_source);
    format!("{}_to_{}", &old_hash[..8], &new_hash[..8])
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

pub fn apply_postgres_migration(db_url: &str, name: &str, sql: &str) -> Result<()> {
    let _ = apply_postgres_migration_outcome(db_url, name, sql)?;
    Ok(())
}

pub fn apply_postgres_migration_outcome(
    db_url: &str,
    name: &str,
    sql: &str,
) -> Result<ApplyOutcome> {
    let sql_hash = sha256_hex(sql);

    let mut client = Client::connect(db_url, NoTls).with_context(|| {
        "Failed to connect to Postgres. Check connection string (host/port/db/user/password). For local dev you may need `?sslmode=disable`."
    })?;

    client.batch_execute(
        "CREATE TABLE IF NOT EXISTS __jwc_migrations (\
            name TEXT PRIMARY KEY,\
            sql_sha256 TEXT NOT NULL,\
            applied_at TIMESTAMPTZ NOT NULL DEFAULT now()\
        );",
    )?;

    if let Some(row) = client
        .query_opt(
            "SELECT sql_sha256 FROM __jwc_migrations WHERE name = $1",
            &[&name],
        )?
    {
        let prev: String = row.get(0);
        if prev == sql_hash {
            return Ok(ApplyOutcome::AlreadyApplied);
        }
        bail!(
            "Migration '{name}' already applied with different SQL hash (db={prev}, local={sql_hash})"
        );
    }

    let mut tx = client.transaction()?;
    tx.batch_execute(sql)?;
    tx.execute(
        "INSERT INTO __jwc_migrations(name, sql_sha256) VALUES ($1,$2)",
        &[&name, &sql_hash],
    )?;
    tx.commit()?;

    Ok(ApplyOutcome::Applied)
}

pub fn list_postgres_migrations(db_url: &str, limit: usize) -> Result<Vec<AppliedMigration>> {
    let mut client = Client::connect(db_url, NoTls).with_context(|| {
        "Failed to connect to Postgres. Check connection string (host/port/db/user/password). For local dev you may need `?sslmode=disable`."
    })?;

    client.batch_execute(
        "CREATE TABLE IF NOT EXISTS __jwc_migrations (\
            name TEXT PRIMARY KEY,\
            sql_sha256 TEXT NOT NULL,\
            applied_at TIMESTAMPTZ NOT NULL DEFAULT now()\
        );",
    )?;

    let lim: i64 = limit.try_into().unwrap_or(50);
    let rows = client.query(
        "SELECT name, sql_sha256, applied_at::text \
         FROM __jwc_migrations \
         ORDER BY applied_at DESC \
         LIMIT $1",
        &[&lim],
    )?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(AppliedMigration {
            name: r.get(0),
            sql_sha256: r.get(1),
            applied_at: r.get(2),
        });
    }
    Ok(out)
}

pub fn exec_postgres(db_url: &str, sql: &str, params: &[crate::ast::Literal]) -> Result<i64> {
    let mut client = Client::connect(db_url, NoTls).with_context(|| {
        "Failed to connect to Postgres. Check connection string (host/port/db/user/password). For local dev you may need `?sslmode=disable`."
    })?;

    let boxed = literals_to_params(params)?;
    let refs: Vec<&(dyn ToSql + Sync)> = boxed
        .iter()
        .map(|b| &**b as &(dyn ToSql + Sync))
        .collect();
    let n = client.execute(sql, &refs)?;
    Ok(n as i64)
}

pub fn query_postgres_json(
    db_url: &str,
    sql: &str,
    params: &[crate::ast::Literal],
) -> Result<String> {
    let mut client = Client::connect(db_url, NoTls).with_context(|| {
        "Failed to connect to Postgres. Check connection string (host/port/db/user/password). For local dev you may need `?sslmode=disable`."
    })?;

    let boxed = literals_to_params(params)?;
    let refs: Vec<&(dyn ToSql + Sync)> = boxed
        .iter()
        .map(|b| &**b as &(dyn ToSql + Sync))
        .collect();
    let rows = client.query(sql, &refs)?;

    let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(row_to_json(&r)?);
    }
    Ok(JsonValue::Array(out).to_string())
}

pub fn query_postgres_one_json(
    db_url: &str,
    sql: &str,
    params: &[crate::ast::Literal],
) -> Result<Option<String>> {
    let mut client = Client::connect(db_url, NoTls).with_context(|| {
        "Failed to connect to Postgres. Check connection string (host/port/db/user/password). For local dev you may need `?sslmode=disable`."
    })?;

    let boxed = literals_to_params(params)?;
    let refs: Vec<&(dyn ToSql + Sync)> = boxed
        .iter()
        .map(|b| &**b as &(dyn ToSql + Sync))
        .collect();
    let row = client.query_opt(sql, &refs)?;

    match row {
        Some(r) => Ok(Some(row_to_json(&r)?.to_string())),
        None => Ok(None),
    }
}

fn literals_to_params(params: &[crate::ast::Literal]) -> Result<Vec<Box<dyn ToSql + Sync>>> {
    let mut boxed: Vec<Box<dyn ToSql + Sync>> = Vec::with_capacity(params.len());
    for p in params {
        match p {
            crate::ast::Literal::Int(i) => {
                if let Ok(v32) = i32::try_from(*i) {
                    boxed.push(Box::new(v32));
                } else {
                    boxed.push(Box::new(*i));
                }
            }
            crate::ast::Literal::Bool(b) => boxed.push(Box::new(*b)),
            crate::ast::Literal::Str(s) => boxed.push(Box::new(s.clone())),
            other => bail!(
                "Unsupported Postgres param type: {} (supported: int, bool, text)",
                crate::runner::literal_type_name(other)
            ),
        }
    }

    Ok(boxed)
}

fn row_to_json(row: &postgres::Row) -> Result<JsonValue> {
    let mut obj = JsonMap::new();
    for (i, col) in row.columns().iter().enumerate() {
        let name = col.name().to_string();
        let ty = col.type_();

        let v = match *ty {
            Type::BOOL => match row.try_get::<usize, Option<bool>>(i)? {
                Some(x) => JsonValue::Bool(x),
                None => JsonValue::Null,
            },
            Type::INT2 => match row.try_get::<usize, Option<i16>>(i)? {
                Some(x) => JsonValue::Number((x as i64).into()),
                None => JsonValue::Null,
            },
            Type::INT4 => match row.try_get::<usize, Option<i32>>(i)? {
                Some(x) => JsonValue::Number((x as i64).into()),
                None => JsonValue::Null,
            },
            Type::INT8 => match row.try_get::<usize, Option<i64>>(i)? {
                Some(x) => JsonValue::Number(x.into()),
                None => JsonValue::Null,
            },
            Type::TEXT | Type::VARCHAR | Type::BPCHAR => match row.try_get::<usize, Option<String>>(i)? {
                Some(x) => JsonValue::String(x),
                None => JsonValue::Null,
            },
            _ => {
                bail!(
                    "Unsupported Postgres column type '{}' for column '{}' (try selecting only int/bool/text columns)",
                    ty.name(),
                    name
                )
            }
        };

        obj.insert(name, v);
    }
    Ok(JsonValue::Object(obj))
}

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use deadpool_postgres::{Config as DpConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio::sync::Mutex;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client as TokioClient, Config as PgConfig, NoTls};

/// Pooled Postgres connection — async version backed by deadpool-postgres.
pub type PgConn = deadpool_postgres::Object;

tokio::task_local! {
    /// Holds a pooled connection while the current task is inside a
    /// `transaction { ... }` block. All `query_text` / `exec` calls
    /// transparently route through this connection so they are part of the
    /// same SQL transaction (started with a `BEGIN` statement).
    pub static TX_CONN: Arc<Mutex<Option<PgConn>>>;
}

struct CachedResult {
    value: String,
    expires_at: Instant,
}

pub struct JwcEngine {
    pool: Pool,
    query_cache: RwLock<HashMap<String, String>>,
    result_cache: RwLock<HashMap<String, CachedResult>>,
    result_ttl: Option<Duration>,
}

static ENGINE: OnceLock<JwcEngine> = OnceLock::new();

fn read_database_url() -> Result<String> {
    std::env::var("DATABASE_URL")
        .or_else(|_| std::env::var("JWC_DATABASE_URL"))
        .map_err(|_| anyhow!("DATABASE_URL (or JWC_DATABASE_URL) is required for db access"))
}

fn parse_pool_size() -> usize {
    std::env::var("JWC_DB_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(64)
}

fn parse_bool_flag(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Returns `true` when `JWC_DB_TLS` is set to a truthy value
/// (`1` / `true` / `yes` / `on`, case-insensitive). Otherwise the engine
/// keeps its historical `NoTls` behaviour.
pub fn should_use_tls() -> bool {
    std::env::var("JWC_DB_TLS")
        .map(|v| parse_bool_flag(&v))
        .unwrap_or(false)
}

/// Returns `true` when `JWC_DB_TLS_INSECURE_SKIP_VERIFY` is truthy. Only
/// meant for local development against self-signed certificates — production
/// deployments should keep verification on.
pub fn should_skip_tls_verify() -> bool {
    std::env::var("JWC_DB_TLS_INSECURE_SKIP_VERIFY")
        .map(|v| parse_bool_flag(&v))
        .unwrap_or(false)
}

fn build_tls_connector() -> Result<MakeTlsConnector> {
    let mut builder = TlsConnector::builder();
    if should_skip_tls_verify() {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    }
    let connector = builder
        .build()
        .with_context(|| "Failed to build native-tls connector")?;
    Ok(MakeTlsConnector::new(connector))
}

fn parse_result_ttl() -> Option<Duration> {
    std::env::var("JWC_QUERY_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
}

fn build_pool(database_url: &str) -> Result<Pool> {
    let pg_cfg: PgConfig = database_url
        .parse()
        .with_context(|| "Invalid DATABASE_URL")?;

    let mut cfg = DpConfig::new();
    if let Some(user) = pg_cfg.get_user() {
        cfg.user = Some(user.to_string());
    }
    if let Some(pw) = pg_cfg.get_password() {
        if let Ok(s) = std::str::from_utf8(pw) {
            cfg.password = Some(s.to_string());
        }
    }
    if let Some(dbname) = pg_cfg.get_dbname() {
        cfg.dbname = Some(dbname.to_string());
    }
    // hosts: use first host
    if let Some(host) = pg_cfg.get_hosts().first() {
        match host {
            tokio_postgres::config::Host::Tcp(s) => {
                cfg.host = Some(s.clone());
            }
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(_) => {}
        }
    }
    if let Some(port) = pg_cfg.get_ports().first() {
        cfg.port = Some(*port);
    }

    cfg.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    });

    let max_size = parse_pool_size();
    cfg.pool = Some(deadpool_postgres::PoolConfig::new(max_size));

    let pool = if should_use_tls() {
        let connector = build_tls_connector()?;
        cfg.create_pool(Some(Runtime::Tokio1), connector)
            .with_context(|| "Failed to create Postgres TLS pool")?
    } else {
        cfg.create_pool(Some(Runtime::Tokio1), NoTls)
            .with_context(|| "Failed to create Postgres pool")?
    };

    Ok(pool)
}

pub fn init_engine(database_url: &str) -> Result<()> {
    if ENGINE.get().is_some() {
        return Ok(());
    }

    let pool = build_pool(database_url)?;

    let engine = JwcEngine {
        pool,
        query_cache: RwLock::new(HashMap::new()),
        result_cache: RwLock::new(HashMap::new()),
        result_ttl: parse_result_ttl(),
    };

    let _ = ENGINE.set(engine);
    Ok(())
}

pub fn init_engine_from_env() -> Result<()> {
    if ENGINE.get().is_some() {
        return Ok(());
    }
    let database_url = read_database_url()?;
    init_engine(&database_url)
}

fn engine() -> Result<&'static JwcEngine> {
    if let Some(engine) = ENGINE.get() {
        return Ok(engine);
    }

    let database_url = read_database_url()?;
    init_engine(&database_url)?;
    ENGINE
        .get()
        .ok_or_else(|| anyhow!("DB engine initialization failed"))
}

pub async fn get_connection() -> Result<PgConn> {
    let pool = &engine()?.pool;
    pool.get()
        .await
        .with_context(|| "Failed to checkout DB connection from pool")
}

/// Build a single (non-pooled) `tokio_postgres::Client` for migration runs.
///
/// Migrations only need one connection for a short period, so this skips the
/// connection pool entirely. TLS settings are re-read from the env each call so
/// the migrate CLI behaves consistently with the runtime engine.
pub async fn connect_for_migrations(url: &str) -> Result<TokioClient> {
    if should_use_tls() {
        let connector = build_tls_connector()?;
        let (client, connection) = tokio_postgres::connect(url, connector)
            .await
            .with_context(|| "Failed to connect to database (TLS) for migrations")?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("postgres connection error: {}", e);
            }
        });
        Ok(client)
    } else {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .with_context(|| "Failed to connect to database for migrations")?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("postgres connection error: {}", e);
            }
        });
        Ok(client)
    }
}

/// Async transaction helper. Begins a transaction, runs `body` inside a
/// scope that exposes `TX_CONN` to nested queries, then commits on success
/// or rolls back on error.
pub async fn with_tx<F, Fut, T>(body: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    // Try to peek at an in-progress tx (nested) — but since TX_CONN may not be
    // set in the current task, only check via try_with.
    let already_open = TX_CONN
        .try_with(|cell| {
            let guard = cell.try_lock();
            match guard {
                Ok(g) => g.is_some(),
                Err(_) => true,
            }
        })
        .unwrap_or(false);
    if already_open {
        bail!("transaction already in progress on this task (nested transactions are not supported)");
    }

    let mut conn = get_connection().await?;
    conn.batch_execute("BEGIN;")
        .await
        .with_context(|| "Failed to BEGIN transaction")?;
    let cell = Arc::new(Mutex::new(Some(conn)));

    let cell_for_scope = cell.clone();
    let result = TX_CONN.scope(cell_for_scope, body()).await;

    // Take the (possibly already taken) conn out
    let mut held = cell.lock().await;
    if let Some(mut conn) = held.take() {
        drop(held);
        match &result {
            Ok(_) => {
                conn.batch_execute("COMMIT;")
                    .await
                    .with_context(|| "Failed to COMMIT transaction")?;
            }
            Err(_) => {
                let _ = conn.batch_execute("ROLLBACK;").await;
            }
        }
    }
    result
}

pub fn get_or_compile_sql<F>(cache_key: &str, compiler: F) -> Result<String>
where
    F: FnOnce() -> Result<String>,
{
    let engine = engine()?;

    if let Some(found) = engine
        .query_cache
        .read()
        .map_err(|_| anyhow!("Query cache lock poisoned"))?
        .get(cache_key)
        .cloned()
    {
        return Ok(found);
    }

    let compiled = compiler()?;

    let mut write_guard = engine
        .query_cache
        .write()
        .map_err(|_| anyhow!("Query cache lock poisoned"))?;
    let entry = write_guard
        .entry(cache_key.to_string())
        .or_insert_with(|| compiled.clone());

    Ok(entry.clone())
}

/// Try to grab the current tx connection if a transaction is active in the
/// current task. Returns the inner `PgConn` if present — caller must put it
/// back when done.
async fn take_tx_conn() -> Option<Arc<Mutex<Option<PgConn>>>> {
    TX_CONN.try_with(|cell| cell.clone()).ok()
}

pub async fn query_text(sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<String> {
    if let Some(cell) = take_tx_conn().await {
        let mut held = cell.lock().await;
        if let Some(conn) = held.as_mut() {
            return query_text_on_conn(conn, sql, params).await;
        }
    }

    let conn = get_connection().await?;
    query_text_on_conn(&conn, sql, params).await
}

async fn query_text_on_conn(
    conn: &PgConn,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<String> {
    let stmt = conn
        .prepare_cached(sql)
        .await
        .with_context(|| "Failed to prepare SQL statement")?;
    let rows = conn
        .query(&stmt, params)
        .await
        .with_context(|| "Failed to execute SQL query")?;

    let mut parts = Vec::new();
    for row in rows {
        let value: Option<String> = row
            .try_get(0)
            .with_context(|| "Expected query to return text in first column")?;
        if let Some(v) = value {
            parts.push(v);
        }
    }

    Ok(parts.join("\n").trim().to_string())
}

pub async fn query_text_with_optional_cache(
    result_cache_key: &str,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<String> {
    let engine = engine()?;

    if let Some(ttl) = engine.result_ttl {
        let now = Instant::now();

        if let Some(found) = engine
            .result_cache
            .read()
            .map_err(|_| anyhow!("Result cache lock poisoned"))?
            .get(result_cache_key)
            .filter(|cached| cached.expires_at > now)
            .map(|cached| cached.value.clone())
        {
            return Ok(found);
        }

        let result = query_text(sql, params).await?;

        engine
            .result_cache
            .write()
            .map_err(|_| anyhow!("Result cache lock poisoned"))?
            .insert(
                result_cache_key.to_string(),
                CachedResult {
                    value: result.clone(),
                    expires_at: now + ttl,
                },
            );

        return Ok(result);
    }

    query_text(sql, params).await
}

pub async fn exec(sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
    if let Some(cell) = take_tx_conn().await {
        let mut held = cell.lock().await;
        if let Some(conn) = held.as_mut() {
            return exec_on_conn(conn, sql, params).await;
        }
    }

    let conn = get_connection().await?;
    exec_on_conn(&conn, sql, params).await
}

async fn exec_on_conn(
    conn: &PgConn,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<u64> {
    let stmt = conn
        .prepare_cached(sql)
        .await
        .with_context(|| "Failed to prepare SQL statement")?;
    let affected = conn
        .execute(&stmt, params)
        .await
        .with_context(|| "Failed to execute SQL statement")?;
    Ok(affected)
}

pub fn invalidate_result_cache() -> Result<()> {
    let engine = engine()?;
    engine
        .result_cache
        .write()
        .map_err(|_| anyhow!("Result cache lock poisoned"))?
        .clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // `std::env::set_var` is process-global; serialise tests that mutate it so
    // they don't fight each other when run in parallel.
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn should_use_tls_defaults_off() {
        with_env("JWC_DB_TLS", None, || {
            assert!(!should_use_tls());
        });
    }

    #[test]
    fn should_use_tls_accepts_truthy_values() {
        for raw in ["1", "true", "TRUE", "True", "yes", "YES", "on", "On"] {
            with_env("JWC_DB_TLS", Some(raw), || {
                assert!(
                    should_use_tls(),
                    "expected JWC_DB_TLS={} to enable TLS",
                    raw
                );
            });
        }
    }

    #[test]
    fn should_use_tls_rejects_other_values() {
        for raw in ["0", "false", "no", "off", "", "maybe", "2"] {
            with_env("JWC_DB_TLS", Some(raw), || {
                assert!(
                    !should_use_tls(),
                    "expected JWC_DB_TLS={} to leave TLS disabled",
                    raw
                );
            });
        }
    }

    #[test]
    fn should_skip_tls_verify_defaults_off() {
        with_env("JWC_DB_TLS_INSECURE_SKIP_VERIFY", None, || {
            assert!(!should_skip_tls_verify());
        });
    }

    #[test]
    fn should_skip_tls_verify_accepts_truthy() {
        with_env("JWC_DB_TLS_INSECURE_SKIP_VERIFY", Some("true"), || {
            assert!(should_skip_tls_verify());
        });
    }
}

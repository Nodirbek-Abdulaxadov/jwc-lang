use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use native_tls::TlsConnector;
use postgres::types::ToSql;
use postgres::{Client, Config, NoTls, Row, Statement};
use postgres_native_tls::MakeTlsConnector;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;

/// Pooled Postgres connection that abstracts over the `NoTls` and
/// `MakeTlsConnector` variants of the underlying r2d2 manager.
///
/// The two pool types are not interchangeable at the type level, but the
/// `postgres::Client` API they hand out is identical — so we wrap them in
/// an enum and forward every operation we actually use to the inner client.
pub enum PgConn {
    NoTls(PooledConnection<PostgresConnectionManager<NoTls>>),
    Tls(PooledConnection<PostgresConnectionManager<MakeTlsConnector>>),
}

impl PgConn {
    fn client_mut(&mut self) -> &mut Client {
        match self {
            PgConn::NoTls(c) => &mut **c,
            PgConn::Tls(c) => &mut **c,
        }
    }

    pub fn prepare(&mut self, sql: &str) -> Result<Statement, postgres::Error> {
        self.client_mut().prepare(sql)
    }

    pub fn query(
        &mut self,
        stmt: &Statement,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, postgres::Error> {
        self.client_mut().query(stmt, params)
    }

    pub fn execute(
        &mut self,
        stmt: &Statement,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, postgres::Error> {
        self.client_mut().execute(stmt, params)
    }

    pub fn batch_execute(&mut self, sql: &str) -> Result<(), postgres::Error> {
        self.client_mut().batch_execute(sql)
    }
}

thread_local! {
    /// Holds a pooled connection while the current thread is inside a
    /// `transaction { ... }` block. All `query_text` / `exec` calls
    /// transparently route through this connection so they are part of the
    /// same SQL transaction (started with a `BEGIN` statement).
    static TX_CONN: RefCell<Option<PgConn>> = const { RefCell::new(None) };
}

struct CachedResult {
    value: String,
    expires_at: Instant,
}

/// Inner pool — homogeneous variant chosen once at `init_engine` time based
/// on whether TLS is enabled via env. Each branch carries its own concrete
/// `PostgresConnectionManager<...>` type so r2d2's generics are satisfied.
enum JwcPool {
    NoTls(Pool<PostgresConnectionManager<NoTls>>),
    Tls(Pool<PostgresConnectionManager<MakeTlsConnector>>),
}

impl JwcPool {
    fn get(&self) -> Result<PgConn> {
        match self {
            JwcPool::NoTls(p) => p
                .get()
                .map(PgConn::NoTls)
                .with_context(|| "Failed to checkout DB connection from pool"),
            JwcPool::Tls(p) => p
                .get()
                .map(PgConn::Tls)
                .with_context(|| "Failed to checkout DB connection from pool"),
        }
    }
}

pub struct JwcEngine {
    pool: JwcPool,
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

fn parse_pool_size() -> u32 {
    std::env::var("JWC_DB_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(64)
}

fn parse_pool_min_idle() -> Option<u32> {
    std::env::var("JWC_DB_MIN_IDLE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .or(Some(8))
}

fn parse_optional_secs(env: &str, default_secs: u64) -> Option<Duration> {
    match std::env::var(env) {
        Ok(v) => {
            let parsed = v.parse::<i64>().ok();
            match parsed {
                Some(n) if n < 0 => None, // negative → disable
                Some(0) => None,          // 0 → disable
                Some(n) => Some(Duration::from_secs(n as u64)),
                None => Some(Duration::from_secs(default_secs)),
            }
        }
        Err(_) => Some(Duration::from_secs(default_secs)),
    }
}

fn parse_connection_timeout() -> Duration {
    std::env::var("JWC_DB_CONNECTION_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(5))
}

fn parse_result_ttl() -> Option<Duration> {
    std::env::var("JWC_QUERY_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
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

pub fn init_engine(database_url: &str) -> Result<()> {
    if ENGINE.get().is_some() {
        return Ok(());
    }

    let cfg: Config = database_url
        .parse()
        .with_context(|| "Invalid DATABASE_URL")?;

    // Default `max_lifetime` = 30 min, `idle_timeout` = 10 min — these mitigate
    // stale connections after Postgres restarts or LB-level idle kills. Either
    // can be disabled by setting the matching env var to 0 (or negative).
    let max_lifetime = parse_optional_secs("JWC_DB_MAX_LIFETIME_SECS", 30 * 60);
    let idle_timeout = parse_optional_secs("JWC_DB_IDLE_TIMEOUT_SECS", 10 * 60);
    let connection_timeout = parse_connection_timeout();
    let max_size = parse_pool_size();
    let min_idle = parse_pool_min_idle();

    let pool = if should_use_tls() {
        let manager = PostgresConnectionManager::new(cfg, build_tls_connector()?);
        let mut builder = Pool::builder()
            .max_size(max_size)
            .max_lifetime(max_lifetime)
            .idle_timeout(idle_timeout)
            .connection_timeout(connection_timeout);
        if let Some(min) = min_idle {
            builder = builder.min_idle(Some(min));
        }
        let pool = builder
            .build(manager)
            .with_context(|| "Failed to initialize Postgres TLS connection pool")?;
        JwcPool::Tls(pool)
    } else {
        let manager = PostgresConnectionManager::new(cfg, NoTls);
        let mut builder = Pool::builder()
            .max_size(max_size)
            .max_lifetime(max_lifetime)
            .idle_timeout(idle_timeout)
            .connection_timeout(connection_timeout);
        if let Some(min) = min_idle {
            builder = builder.min_idle(Some(min));
        }
        let pool = builder
            .build(manager)
            .with_context(|| "Failed to initialize Postgres connection pool")?;
        JwcPool::NoTls(pool)
    };

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

pub fn get_connection() -> Result<PgConn> {
    engine()?.pool.get()
}

/// Build a single (non-pooled) `postgres::Client` for migration runs.
///
/// Migrations only need one connection for a short period, so this skips the
/// r2d2 pool entirely. TLS settings are re-read from the env each call so the
/// migrate CLI behaves consistently with the runtime engine.
pub fn connect_for_migrations(url: &str) -> Result<Client> {
    if should_use_tls() {
        let connector = build_tls_connector()?;
        Client::connect(url, connector)
            .with_context(|| "Failed to connect to database (TLS) for migrations")
    } else {
        Client::connect(url, NoTls)
            .with_context(|| "Failed to connect to database for migrations")
    }
}

/// RAII guard that owns the in-progress thread-local transaction.
///
/// Construct with `begin_tx()`. Call `commit()` to commit and release the
/// connection back to the pool. If the guard is dropped without `commit()`
/// (early `return`, `?` error propagation, panic), `Drop` issues a
/// best-effort `ROLLBACK` and clears the thread-local — preventing leaked
/// open transactions across worker-thread reuse.
#[must_use = "TxGuard must be committed or dropped to release the transaction"]
pub struct TxGuard {
    committed: bool,
}

impl TxGuard {
    pub fn commit(mut self) -> Result<()> {
        let res = TX_CONN.with(|cell| {
            let mut held = cell.borrow_mut();
            let Some(mut conn) = held.take() else {
                return Ok(());
            };
            drop(held);
            conn.batch_execute("COMMIT;")
                .with_context(|| "Failed to COMMIT transaction")
        });
        self.committed = true;
        res
    }
}

impl Drop for TxGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        TX_CONN.with(|cell| {
            let mut held = cell.borrow_mut();
            if let Some(mut conn) = held.take() {
                drop(held);
                let _ = conn.batch_execute("ROLLBACK;");
            }
        });
    }
}

/// Begin a SQL transaction on the current thread. All subsequent
/// `query_text` / `exec` calls on this thread run on the held connection
/// until the returned guard is committed or dropped.
pub fn begin_tx() -> Result<TxGuard> {
    let already_open = TX_CONN.with(|cell| cell.borrow().is_some());
    if already_open {
        bail!("transaction already in progress on this thread (nested transactions are not supported)");
    }
    let mut conn = get_connection()?;
    conn.batch_execute("BEGIN;")
        .with_context(|| "Failed to BEGIN transaction")?;
    TX_CONN.with(|cell| {
        *cell.borrow_mut() = Some(conn);
    });
    Ok(TxGuard { committed: false })
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

pub fn query_text(sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<String> {
    // Inside `transaction { ... }` route through the held connection so the
    // query participates in the open SQL transaction; otherwise fall back to
    // the connection pool.
    let routed = TX_CONN.with(|cell| {
        let mut held = cell.borrow_mut();
        if let Some(conn) = held.as_mut() {
            Some(query_text_on_conn(conn, sql, params))
        } else {
            None
        }
    });
    if let Some(result) = routed {
        return result;
    }

    let mut conn = get_connection()?;
    query_text_on_conn(&mut conn, sql, params)
}

fn query_text_on_conn(
    conn: &mut PgConn,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<String> {
    let stmt = conn
        .prepare(sql)
        .with_context(|| "Failed to prepare SQL statement")?;
    let rows = conn
        .query(&stmt, params)
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

pub fn query_text_with_optional_cache(
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

        let result = query_text(sql, params)?;

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

    query_text(sql, params)
}

pub fn exec(sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
    let routed = TX_CONN.with(|cell| {
        let mut held = cell.borrow_mut();
        if let Some(conn) = held.as_mut() {
            Some(exec_on_conn(conn, sql, params))
        } else {
            None
        }
    });
    if let Some(result) = routed {
        return result;
    }

    let mut conn = get_connection()?;
    exec_on_conn(&mut conn, sql, params)
}

fn exec_on_conn(
    conn: &mut PgConn,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<u64> {
    let stmt = conn
        .prepare(sql)
        .with_context(|| "Failed to prepare SQL statement")?;
    let affected = conn
        .execute(&stmt, params)
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
    use std::sync::Mutex;

    // `std::env::set_var` is process-global; serialise tests that mutate it so
    // they don't fight each other when run in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

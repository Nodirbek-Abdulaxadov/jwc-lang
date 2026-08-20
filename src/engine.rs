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

/// `DATABASE_URL`, or `JWC_DATABASE_URL`. The migrate CLI reads it through
/// here so it fails with the same sentence the runtime does.
pub fn database_url_from_env() -> Result<String> {
    read_database_url()
}

fn read_database_url() -> Result<String> {
    std::env::var("DATABASE_URL")
        .or_else(|_| std::env::var("JWC_DATABASE_URL"))
        .map_err(|_| anyhow!("DATABASE_URL (or JWC_DATABASE_URL) is required for db access"))
}

/// Mask the `user:password` segment of a `postgres://` (or any
/// `scheme://`) URL so connection-string strings can be embedded in
/// error context without leaking the password to logs / pagers /
/// crash reports. The host, port, path and query are preserved so an
/// operator can still see WHICH database the call was targeting.
///
/// Examples:
///   `postgres://alice:hunter2@db.internal:5432/app`
/// → `postgres://alice:***@db.internal:5432/app`
///
///   `postgres://localhost/app`  (unchanged — no creds present)
/// → `postgres://localhost/app`
pub fn scrub_database_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let after_scheme = scheme_end + 3;
    let rest = &url[after_scheme..];
    // Userinfo (`user[:password]`) ends at the first `@` BEFORE the first `/`.
    let at_idx = match rest.find('@') {
        Some(i) => i,
        None => return url.to_string(),
    };
    let slash_idx = rest.find('/').unwrap_or(rest.len());
    if at_idx >= slash_idx {
        return url.to_string();
    }
    let userinfo = &rest[..at_idx];
    let after_at = &rest[at_idx..];
    let masked_userinfo = match userinfo.find(':') {
        Some(colon) => format!("{}:***", &userinfo[..colon]),
        None => userinfo.to_string(),
    };
    format!("{}{}{}", &url[..after_scheme], masked_userinfo, after_at)
}

fn parse_pool_size() -> usize {
    std::env::var("JWC_DB_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(64)
}

/// **Sprint 4 #21 / #23** — retry ceiling for transient DB failures.
///
/// Reads `JWC_DB_RETRY_MAX_ATTEMPTS` (a `u32`); falls back to `3` when the
/// var is unset, empty, or unparsable. A value of `1` disables retries
/// (one attempt, no retry); `0` is treated as "no attempt" by the retry
/// loop, which is rarely useful but accepted so the knob is uniform.
pub fn parse_retry_max_attempts() -> u32 {
    std::env::var("JWC_DB_RETRY_MAX_ATTEMPTS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(3)
}

/// **Sprint 4 #21 / #23** — base backoff in ms between retries.
///
/// Reads `JWC_DB_RETRY_BACKOFF_MS` (a `u32`); falls back to `100` ms when
/// unset / unparsable. The retry loop doubles this each attempt
/// (exponential: `base`, `2*base`, `4*base`, ...) so a single transient
/// blip rarely produces more than a few hundred ms of added latency.
pub fn parse_retry_backoff_ms() -> u32 {
    std::env::var("JWC_DB_RETRY_BACKOFF_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(100)
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

/// **Sprint 4 #21** — pool saturation snapshot for `/metrics` and `/readyz`.
///
/// Mirrors `deadpool::Status` (the underlying struct is re-exported by
/// `deadpool-postgres`), copied into our own type so the public surface of
/// `engine` doesn't leak a third-party version. `size` is the number of
/// connections the pool currently holds; `available` is how many of those
/// are idle and checkout-able right now; `max_size` is the configured
/// ceiling (from `JWC_DB_POOL_SIZE`); `waiting` is the number of tasks
/// queued for a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStatusSnapshot {
    pub size: usize,
    pub available: usize,
    pub max_size: usize,
    pub waiting: usize,
}

/// Return a snapshot of the deadpool status, or `None` when the engine
/// hasn't been initialised yet (no DB configured → no pool to inspect).
/// Used by `server::handle_metrics` to emit `jwc_db_pool_*` gauges and by
/// the chaos test to assert the pool reports `available == 0` mid-outage.
pub fn pool_status() -> Option<PoolStatusSnapshot> {
    let engine = ENGINE.get()?;
    let s = engine.pool.status();
    Some(PoolStatusSnapshot {
        size: s.size,
        available: s.available,
        max_size: s.max_size,
        waiting: s.waiting,
    })
}

/// **Sprint 4 #21** — readiness probe round-trip.
///
/// Checks out a connection and runs `SELECT 1`. Used by `/readyz` so the
/// readiness gate goes through one centralised code path; if the retry
/// logic ever needs to wrap this (e.g. swallow a single transient blip
/// during pod start-up), we change it here and `/readyz` picks it up
/// transparently.
pub async fn ping() -> Result<()> {
    let conn = get_connection().await?;
    conn.simple_query("SELECT 1")
        .await
        .with_context(|| "DB ping (SELECT 1) failed")?;
    Ok(())
}

/// **Sprint 4 #21 / #23** — classifies an `anyhow::Error` as transient
/// (worth retrying) by walking the `.chain()`.
///
/// Recognised triggers:
/// - `tokio_postgres::Error::is_closed()` — connection died mid-query;
///   a fresh checkout from the pool will get a healthy socket.
/// - SQLSTATE class `08*` (connection_exception family) — Postgres has
///   told us the link is broken.
/// - SQLSTATE `40001` (`serialization_failure`) — concurrent txn
///   conflict; the standard recommendation is "retry the whole txn".
///   (Note: `with_tx` callers DON'T retry — see `retry_with_backoff`,
///   which skips the retry loop entirely when a transaction is open.)
/// - `deadpool_postgres::PoolError::Backend(_)` / `PoolError::Timeout(_)`
///   — pool reported the underlying create / checkout step blew up;
///   another attempt may land on a healthier slot.
///
/// Anything else (parse errors, bad SQL, NOT NULL violations, ...) is
/// classified as permanent so we don't silently mask user bugs.
pub fn is_transient_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(pg) = cause.downcast_ref::<tokio_postgres::Error>() {
            if pg.is_closed() {
                return true;
            }
            if let Some(code) = pg.code() {
                if sqlstate_is_transient(code.code()) {
                    return true;
                }
            }
        }
        if let Some(pool_err) = cause.downcast_ref::<deadpool_postgres::PoolError>() {
            return matches!(
                pool_err,
                deadpool_postgres::PoolError::Backend(_) | deadpool_postgres::PoolError::Timeout(_)
            );
        }
    }
    false
}

/// Pure SQLSTATE → transient classifier — pulled out of
/// [`is_transient_error`] so the unit tests can pin the recognised codes
/// without needing a real `tokio_postgres::Error` (whose internals are
/// crate-private).
fn sqlstate_is_transient(code: &str) -> bool {
    code.starts_with("08") || code == "40001"
}

/// **Sprint 4 #21 / #23** — exponential-backoff retry wrapper.
///
/// Runs `op`, and if it fails with a transient error (per
/// [`is_transient_error`]) retries up to `parse_retry_max_attempts()`
/// times with `parse_retry_backoff_ms()` doubled each attempt. A
/// non-transient error is returned immediately so user-level SQL bugs
/// don't get retried into a livelock.
///
/// **Critical**: callers MUST guard against re-running mutation SQL
/// inside an open `transaction { ... }`. The `query_text` / `exec`
/// entry points check `TX_CONN.try_with(...)` and skip the retry wrap
/// when a transaction is in flight — silently re-running an INSERT
/// inside a txn would either replay the statement twice on the same
/// session (when the session is healthy) or split the work across two
/// connections (when it isn't), both of which are footguns.
pub async fn retry_with_backoff<F, Fut, T>(op: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let max_attempts = parse_retry_max_attempts();
    let base_ms = parse_retry_backoff_ms();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= max_attempts || !is_transient_error(&e) {
                    return Err(e);
                }
                let delay_ms = (base_ms as u64).saturating_mul(1u64 << (attempt - 1));
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// Returns `true` when the current task is inside a `transaction { ... }`
/// block — exposed primarily for the retry-loop unit test, which asserts
/// the helper sees the same flag the entry points use. Production code
/// doesn't call this directly; `query_text` / `exec` just short-circuit
/// on `take_tx_conn().await` first.
#[allow(dead_code)]
fn inside_transaction() -> bool {
    TX_CONN.try_with(|_| ()).is_ok()
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
        bail!(
            "transaction already in progress on this task (nested transactions are not supported)"
        );
    }

    let conn = get_connection().await?;
    conn.batch_execute("BEGIN;")
        .await
        .with_context(|| "Failed to BEGIN transaction")?;
    let cell = Arc::new(Mutex::new(Some(conn)));

    let cell_for_scope = cell.clone();
    let result = TX_CONN.scope(cell_for_scope, body()).await;

    // Take the (possibly already taken) conn out
    let mut held = cell.lock().await;
    if let Some(conn) = held.take() {
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

/// **Sprint 4B [1.0-blocker]** — `savepoint <name> { ... }` helper.
///
/// Issues `SAVEPOINT <name>` on the in-flight transaction connection,
/// runs `body`, then `RELEASE SAVEPOINT <name>` on success or
/// `ROLLBACK TO SAVEPOINT <name>; RELEASE SAVEPOINT <name>` on error.
/// The error is re-raised so the surrounding code (e.g. an outer
/// `catch`) can react. Refuses to run when called outside an active
/// `transaction { ... }` block — E017 surfaced as a clear runtime
/// error so users get the same diagnostic shape as the validator would.
///
/// `name` MUST already be a valid SQL identifier (the parser checks
/// `[A-Za-z_][A-Za-z0-9_]*`); we re-assert here so any caller bypassing
/// the parser path still can't smuggle SQL through this surface.
pub async fn with_savepoint<F, Fut, T>(name: &str, body: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        bail!(
            "savepoint name '{}' is not a valid SQL identifier (use [A-Za-z_][A-Za-z0-9_]*)",
            name
        );
    }

    let Some(cell) = take_tx_conn().await else {
        bail!(
            "error[E017]: `savepoint` is only valid inside a `transaction` block — wrap the call site in `transaction {{ savepoint {name} {{ ... }} }}`"
        );
    };

    let begin_sql = format!("SAVEPOINT {name}");
    let release_sql = format!("RELEASE SAVEPOINT {name}");
    let rollback_sql = format!("ROLLBACK TO SAVEPOINT {name}");

    // Issue SAVEPOINT then run body. We pull the conn out of the cell
    // briefly to call batch_execute, then drop the guard so the body's
    // inner DB calls (which also lock the cell) can proceed.
    {
        let held = cell.lock().await;
        let Some(conn) = held.as_ref() else {
            bail!(
                "error[E017]: transaction connection unexpectedly closed before SAVEPOINT could run"
            );
        };
        conn.batch_execute(&begin_sql)
            .await
            .with_context(|| format!("Failed to issue SAVEPOINT {name}"))?;
    }

    let result = body().await;

    let held = cell.lock().await;
    if let Some(conn) = held.as_ref() {
        match &result {
            Ok(_) => {
                conn.batch_execute(&release_sql)
                    .await
                    .with_context(|| format!("Failed to RELEASE SAVEPOINT {name}"))?;
            }
            Err(_) => {
                // Best-effort cleanup — if the outer error already poisoned
                // the connection, both calls will fail and we surface the
                // original error from the body.
                let _ = conn.batch_execute(&rollback_sql).await;
                let _ = conn.batch_execute(&release_sql).await;
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

    // Outside a transaction: wrap the checkout-and-query in the retry
    // helper so a single dropped connection (deploy / failover blip)
    // doesn't surface as a user-visible 500.
    retry_with_backoff(|| async {
        let conn = get_connection().await?;
        query_text_on_conn(&conn, sql, params).await
    })
    .await
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

    rows_first_column_text(rows)
}

/// Collapse a result set to its first column as text.
///
/// Hot path: select/insert/update/delete RETURNING always project a single
/// text column (json_agg/row_to_json), so the result has 0 or 1 rows. Skip
/// Vec/join/trim allocations for that case.
fn rows_first_column_text(rows: Vec<tokio_postgres::Row>) -> Result<String> {
    match rows.len() {
        0 => Ok(String::new()),
        1 => {
            let value: Option<String> = rows[0]
                .try_get(0)
                .with_context(|| "Expected query to return text in first column")?;
            Ok(value.unwrap_or_default())
        }
        _ => {
            let mut parts = Vec::with_capacity(rows.len());
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
    }
}

/// What a `raw_sql` statement produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawSqlOutcome {
    /// The statement projects result columns — `SELECT`, `WITH ... SELECT`,
    /// `VALUES`, and any `INSERT` / `UPDATE` / `DELETE ... RETURNING`. Carries
    /// the first column, same shape as [`query_text`].
    Rows(String),
    /// No result columns; the affected-row count is all there is to report.
    Affected(u64),
}

/// Run a statement and route the result by **what it actually returns**,
/// asking the prepared statement rather than guessing from the leading
/// keyword.
///
/// `raw_sql` used to send anything not starting with `select` / `with` down
/// the `exec` path, so `UPDATE ... RETURNING url` reported the affected-row
/// count and the returned column was thrown away. Nothing errored — a
/// redirect built from it just carried `Location: 1`. Prefix matching can't
/// see `RETURNING` at all, and the same trap waits for `INSERT ... RETURNING
/// id`, so the check moved to the one place that knows: a prepared statement
/// with no result columns is an exec, everything else is a query.
pub async fn query_or_exec(sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<RawSqlOutcome> {
    if let Some(cell) = take_tx_conn().await {
        let mut held = cell.lock().await;
        if let Some(conn) = held.as_mut() {
            return query_or_exec_on_conn(conn, sql, params).await;
        }
    }
    retry_with_backoff(|| async {
        let conn = get_connection().await?;
        query_or_exec_on_conn(&conn, sql, params).await
    })
    .await
}

async fn query_or_exec_on_conn(
    conn: &PgConn,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<RawSqlOutcome> {
    let stmt = conn
        .prepare_cached(sql)
        .await
        .with_context(|| "Failed to prepare SQL statement")?;
    if stmt.columns().is_empty() {
        let affected = conn
            .execute(&stmt, params)
            .await
            .with_context(|| "Failed to execute SQL statement")?;
        return Ok(RawSqlOutcome::Affected(affected));
    }
    let rows = conn
        .query(&stmt, params)
        .await
        .with_context(|| "Failed to execute SQL query")?;
    Ok(RawSqlOutcome::Rows(rows_first_column_text(rows)?))
}

/// Returns `Ok(Some(value))` when the result cache is enabled AND has a fresh
/// entry for `cache_key`. Returns `Ok(None)` when caching is off or the key
/// missed. Callers use this to short-circuit SQL building entirely on hit.
pub fn try_cached_result(cache_key: &str) -> Result<Option<String>> {
    let engine = engine()?;
    if engine.result_ttl.is_none() {
        return Ok(None);
    }
    let now = Instant::now();
    let found = engine
        .result_cache
        .read()
        .map_err(|_| anyhow!("Result cache lock poisoned"))?
        .get(cache_key)
        .filter(|cached| cached.expires_at > now)
        .map(|cached| cached.value.clone());
    Ok(found)
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

    // Outside a transaction: same retry wrap as `query_text`. Inside a
    // transaction the early return above means we never reach this path,
    // which is intentional — replaying a mutation on a fresh connection
    // would either commit-twice or split work across sessions.
    retry_with_backoff(|| async {
        let conn = get_connection().await?;
        exec_on_conn(&conn, sql, params).await
    })
    .await
}

async fn exec_on_conn(conn: &PgConn, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
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
    fn scrub_database_url_masks_password() {
        // Standard postgres URL — password collapses to ***, everything
        // else is preserved so an operator can still tell which DB the
        // call was targeting.
        let url = "postgres://alice:hunter2@db.internal:5432/app";
        assert_eq!(
            scrub_database_url(url),
            "postgres://alice:***@db.internal:5432/app"
        );
    }

    #[test]
    fn scrub_database_url_handles_passwordless_url() {
        // No `:` in userinfo, no password to mask. Leave the URL alone.
        let url = "postgres://alice@db.internal/app";
        assert_eq!(scrub_database_url(url), url);
    }

    #[test]
    fn scrub_database_url_passthrough_when_no_userinfo() {
        // Bare host/path means no creds at all.
        let url = "postgres://db.internal/app";
        assert_eq!(scrub_database_url(url), url);
    }

    #[test]
    fn scrub_database_url_passthrough_when_not_a_url() {
        let s = "not a url";
        assert_eq!(scrub_database_url(s), s);
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

    // ---- Sprint 4 #21 / #23 — retry path ---------------------------------

    #[test]
    fn retry_classifier_recognises_08_class() {
        // Connection_exception family (08000..08P01) — server has told
        // us the link is dead; another checkout will pick a fresh one.
        for code in ["08000", "08003", "08006", "08P01"] {
            assert!(
                sqlstate_is_transient(code),
                "SQLSTATE {code} should classify as transient"
            );
        }
    }

    #[test]
    fn retry_classifier_recognises_serialization_failure() {
        // 40001 = serialization_failure — Postgres recommends a full
        // transaction retry. Our outer retry loop refuses to retry
        // inside an open `transaction`, so the recovery path lives in
        // user code, but the classifier still flags it.
        assert!(sqlstate_is_transient("40001"));
        // Sanity: a sibling 40P01 (deadlock_detected) is intentionally
        // NOT in the transient set — handling it the same way would
        // mask a real concurrency bug.
        assert!(!sqlstate_is_transient("40P01"));
        // Permanent — must never retry.
        assert!(!sqlstate_is_transient("23505")); // unique_violation
        assert!(!sqlstate_is_transient("42601")); // syntax_error
    }

    #[test]
    fn retry_classifier_recognises_pool_error() {
        // Walk the `.chain()` via anyhow, asserting both PoolError
        // variants flagged in the doc comment classify as transient.
        let wait_timeout: deadpool_postgres::PoolError =
            deadpool_postgres::PoolError::Timeout(deadpool_postgres::TimeoutType::Wait);
        let err: anyhow::Error = anyhow::Error::new(wait_timeout);
        assert!(
            is_transient_error(&err),
            "PoolError::Timeout(Wait) should be transient"
        );

        // A bare anyhow with no recognised cause is permanent.
        let bare = anyhow!("user-level SQL bug, do not retry");
        assert!(!is_transient_error(&bare));
    }

    #[tokio::test]
    async fn retry_skipped_inside_transaction() {
        // The retry path MUST NOT run inside a `transaction { ... }`
        // block — silently re-running an INSERT inside a txn is a
        // footgun (would either replay or split connections). We
        // assert by entering the same `TX_CONN.scope` shape `with_tx`
        // uses and confirming `inside_transaction()` flips on.
        assert!(
            !inside_transaction(),
            "fresh task must not see a TX_CONN scope"
        );

        let cell: Arc<Mutex<Option<PgConn>>> = Arc::new(Mutex::new(None));
        TX_CONN
            .scope(cell, async {
                assert!(
                    inside_transaction(),
                    "TX_CONN.scope must make inside_transaction() return true"
                );
            })
            .await;

        // Scope dropped: the flag flips back off.
        assert!(!inside_transaction());
    }

    #[tokio::test]
    // The guard is held across the awaited retry loop on purpose: it
    // serialises the process-wide environment these tests mutate, and
    // dropping it early is exactly the race it exists to prevent. Single
    // test thread, no contention, no deadlock to have.
    #[allow(clippy::await_holding_lock)]
    async fn retry_honours_max_attempts_env() {
        // Force the loop to give up after 2 attempts so the test stays
        // fast — and use a 0 ms backoff so we don't sleep at all.
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_max = std::env::var("JWC_DB_RETRY_MAX_ATTEMPTS").ok();
        let prev_backoff = std::env::var("JWC_DB_RETRY_BACKOFF_MS").ok();
        std::env::set_var("JWC_DB_RETRY_MAX_ATTEMPTS", "2");
        std::env::set_var("JWC_DB_RETRY_BACKOFF_MS", "0");

        assert_eq!(parse_retry_max_attempts(), 2);
        assert_eq!(parse_retry_backoff_ms(), 0);

        let counter = std::sync::atomic::AtomicU32::new(0);
        let result: Result<()> = retry_with_backoff(|| async {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Always-transient error: PoolError::Timeout(Wait).
            let pool_err: deadpool_postgres::PoolError =
                deadpool_postgres::PoolError::Timeout(deadpool_postgres::TimeoutType::Wait);
            Err(anyhow::Error::new(pool_err))
        })
        .await;

        assert!(result.is_err(), "always-failing op must surface the error");
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "should attempt exactly JWC_DB_RETRY_MAX_ATTEMPTS times"
        );

        // Restore env.
        match prev_max {
            Some(v) => std::env::set_var("JWC_DB_RETRY_MAX_ATTEMPTS", v),
            None => std::env::remove_var("JWC_DB_RETRY_MAX_ATTEMPTS"),
        }
        match prev_backoff {
            Some(v) => std::env::set_var("JWC_DB_RETRY_BACKOFF_MS", v),
            None => std::env::remove_var("JWC_DB_RETRY_BACKOFF_MS"),
        }
    }

    #[test]
    fn retry_defaults_apply_when_unset() {
        with_env("JWC_DB_RETRY_MAX_ATTEMPTS", None, || {
            assert_eq!(parse_retry_max_attempts(), 3);
        });
        with_env("JWC_DB_RETRY_BACKOFF_MS", None, || {
            assert_eq!(parse_retry_backoff_ms(), 100);
        });
    }

    #[test]
    fn pool_status_is_none_before_engine_init_or_snapshot_shape() {
        // The engine may or may not have been initialised by an earlier
        // test in this process — both outcomes are valid. If a snapshot
        // is returned, it must satisfy `size <= max_size` and
        // `available <= size`, which is what the `/metrics` consumers
        // rely on to chart pool saturation correctly.
        if let Some(s) = pool_status() {
            assert!(
                s.size <= s.max_size,
                "size {} > max_size {}",
                s.size,
                s.max_size
            );
            assert!(
                s.available <= s.size,
                "available {} > size {}",
                s.available,
                s.size
            );
        }
    }
}

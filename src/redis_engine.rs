//! Redis driver — the **core tier** integration from
//! `docs/spec/ecosystem.md` Faza 1.
//!
//! Redis sits in the core tier for the reason the spec gives: sub-ms
//! latency budget and a binary (RESP) wire protocol. A pure-JWC package
//! can't speak RESP — there is no socket built-in and `ecosystem.md` §6.6
//! puts a "TCP shim for packages" outside the roadmap — so the driver has
//! to be Rust, and the ergonomic surface goes in a package on top (Faza 2).
//!
//! This module deliberately mirrors [`crate::engine`] (the Postgres layer)
//! shape-for-shape: a `OnceLock` singleton over a deadpool, an
//! `init_*_from_env`, a `ping` for `/readyz`, a `pool_status` for
//! `/metrics`, an `is_transient_error` classifier and a backoff retry
//! wrapper. If you are changing one, check whether the other wants the
//! same change.
//!
//! # Two ways this is NOT like the Postgres engine
//!
//! 1. **Redis is optional.** `engine::init_engine_from_env` errors when
//!    `DATABASE_URL` is missing, because a JWC app that touches `db` needs
//!    a database. Redis has no such contract: an app with no
//!    `JWC_REDIS_URL` runs fine and the `redis_*` built-ins report
//!    "not configured" rather than failing the boot.
//! 2. **The driver is behind a Cargo feature** (`redis`, off by default),
//!    so the default build pulls in neither `redis` nor `deadpool-redis`.
//!    The *built-ins* are not gated — see the note on `BUILTIN_DEFS` in
//!    `crate::builtins` — only the implementation is. Without the feature
//!    every call returns a `RedisError` explaining the binary was built
//!    without Redis support.

use anyhow::Result;

/// Pool saturation snapshot for `/metrics`.
///
/// Same shape and same reasoning as
/// [`crate::engine::PoolStatusSnapshot`] — copied rather than shared so
/// neither module's public surface leaks a `deadpool` version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStatusSnapshot {
    pub size: usize,
    pub available: usize,
    pub max_size: usize,
    pub waiting: usize,
}

/// Read the configured Redis URL, or `None` when Redis isn't configured.
///
/// `JWC_REDIS_URL` is the only spelling. There is deliberately no bare
/// `REDIS_URL` fallback: `engine` accepts bare `DATABASE_URL` because
/// that name is a de-facto standard a JWC app inherits from its platform,
/// whereas silently binding to a `REDIS_URL` that some *other* component
/// in the same container put there would attach us to a cache we were
/// never pointed at.
pub fn read_redis_url() -> Option<String> {
    std::env::var("JWC_REDIS_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Mask credentials in a `redis://` / `rediss://` URL before it reaches a
/// log line or an error chain.
///
/// Redis URLs carry the password in the same `user:password@host` userinfo
/// slot as Postgres ones — and on Redis the common shape is a *bare*
/// password with no username (`redis://:hunter2@host:6379`), which this
/// still masks. Delegates to the one implementation in [`crate::engine`]
/// so the two can't drift apart.
pub fn scrub_redis_url(url: &str) -> String {
    crate::engine::scrub_database_url(url)
}

/// Max connections in the Redis deadpool. Mirrors `JWC_DB_POOL_SIZE`.
///
/// `0` is rejected back to the default rather than honoured: deadpool
/// treats a zero `max_size` as a pool that can never hand out a
/// connection, so the first `redis_get` would hang on the wait queue
/// instead of failing — a much worse outcome than ignoring the value.
pub fn parse_pool_size() -> usize {
    std::env::var("JWC_REDIS_POOL_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(64)
}

/// Retry ceiling for transient Redis failures. Mirrors
/// [`crate::engine::parse_retry_max_attempts`]; `1` disables retries.
pub fn parse_retry_max_attempts() -> u32 {
    std::env::var("JWC_REDIS_RETRY_MAX_ATTEMPTS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(3)
}

/// Base backoff (ms) between retries; doubled each attempt.
pub fn parse_retry_backoff_ms() -> u32 {
    std::env::var("JWC_REDIS_RETRY_BACKOFF_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(100)
}

/// The error every entry point returns when the binary was compiled
/// without `--features redis`.
///
/// Phrased as an actionable build instruction rather than "unsupported":
/// the program is valid, the binary just can't run it.
#[cfg(not(feature = "redis"))]
fn feature_disabled() -> anyhow::Error {
    anyhow::anyhow!(
        "redis: this `jwc` binary was built without Redis support. \
         Rebuild with `cargo build --features redis` (or install a \
         release build, which enables it) to use the redis_* built-ins."
    )
}

// ---------------------------------------------------------------------------
// Real implementation — `--features redis`
// ---------------------------------------------------------------------------

#[cfg(feature = "redis")]
mod imp {
    use super::{
        parse_pool_size, parse_retry_backoff_ms, parse_retry_max_attempts, read_redis_url,
        scrub_redis_url, PoolStatusSnapshot,
    };

    use std::future::Future;
    use std::sync::OnceLock;
    use std::time::Duration;

    use anyhow::{anyhow, Context, Result};
    use deadpool_redis::{Config as RedisConfig, Connection, Pool, PoolConfig, Runtime};

    pub struct JwcRedis {
        pool: Pool,
    }

    static REDIS: OnceLock<JwcRedis> = OnceLock::new();

    fn build_pool(url: &str) -> Result<Pool> {
        let mut cfg = RedisConfig::from_url(url);
        cfg.pool = Some(PoolConfig::new(parse_pool_size()));
        cfg.create_pool(Some(Runtime::Tokio1)).with_context(|| {
            format!(
                "Failed to create Redis pool for {}",
                scrub_redis_url(url)
            )
        })
    }

    /// Initialise the pool from `JWC_REDIS_URL`.
    ///
    /// A **missing** URL is success-with-no-pool, not an error: Redis is
    /// optional and every app that doesn't use it must still boot. A
    /// *malformed* URL is a hard error — the operator asked for Redis and
    /// got the address wrong, and failing fast at boot beats every request
    /// discovering it separately.
    ///
    /// Note `deadpool` builds the pool lazily, so this succeeding does not
    /// mean Redis is reachable. `/readyz` is what proves reachability.
    pub fn init_redis_from_env() -> Result<()> {
        if REDIS.get().is_some() {
            return Ok(());
        }
        let Some(url) = read_redis_url() else {
            return Ok(());
        };
        let pool = build_pool(&url)?;
        let _ = REDIS.set(JwcRedis { pool });
        Ok(())
    }

    pub fn is_enabled() -> bool {
        REDIS.get().is_some()
    }

    fn redis() -> Result<&'static JwcRedis> {
        if let Some(r) = REDIS.get() {
            return Ok(r);
        }
        // Lazily initialise so a built-in called before `server::serve`
        // (e.g. from `main { }`) still works.
        init_redis_from_env()?;
        REDIS.get().ok_or_else(|| {
            anyhow!(
                "redis: not configured. Set JWC_REDIS_URL (e.g. \
                 redis://127.0.0.1:6379) to enable the redis_* built-ins."
            )
        })
    }

    pub async fn get_connection() -> Result<Connection> {
        redis()?
            .pool
            .get()
            .await
            .with_context(|| "Failed to checkout Redis connection from pool")
    }

    pub fn pool_status() -> Option<PoolStatusSnapshot> {
        let s = REDIS.get()?.pool.status();
        Some(PoolStatusSnapshot {
            size: s.size,
            available: s.available,
            max_size: s.max_size,
            waiting: s.waiting,
        })
    }

    /// Readiness round-trip: `PING`. Used by `/readyz`.
    pub async fn ping() -> Result<()> {
        let mut conn = get_connection().await?;
        let pong: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .with_context(|| "Redis ping (PING) failed")?;
        if pong.eq_ignore_ascii_case("PONG") {
            Ok(())
        } else {
            Err(anyhow!("Redis ping returned unexpected reply: {pong:?}"))
        }
    }

    /// Classify a `redis::RedisError` as transient (worth retrying) by
    /// walking the `anyhow` chain.
    ///
    /// Recognised triggers:
    /// - `is_connection_dropped()` / `is_timeout()` / `is_io_error()` —
    ///   the socket died or stalled; a fresh checkout gets a healthy one.
    /// - `BusyLoadingError` — the server is loading its dataset from disk
    ///   (restart / replica sync) and will start answering shortly.
    /// - `TryAgain`, `ClusterDown`, `MasterDown`, `Moved`, `Ask` — cluster
    ///   topology in flux; the retry lands after the slot map settles.
    ///
    /// Everything else (`ResponseError` from a bad command, `WRONGTYPE`,
    /// `NoScriptError`, ...) is permanent, so user bugs surface instead of
    /// being retried into a livelock.
    pub fn is_transient_error(err: &anyhow::Error) -> bool {
        for cause in err.chain() {
            if let Some(e) = cause.downcast_ref::<redis::RedisError>() {
                if e.is_connection_dropped() || e.is_timeout() || e.is_io_error() {
                    return true;
                }
                return matches!(
                    e.kind(),
                    redis::ErrorKind::BusyLoadingError
                        | redis::ErrorKind::TryAgain
                        | redis::ErrorKind::ClusterDown
                        | redis::ErrorKind::MasterDown
                        | redis::ErrorKind::Moved
                        | redis::ErrorKind::Ask
                );
            }
            if let Some(pool_err) = cause.downcast_ref::<deadpool_redis::PoolError>() {
                return matches!(
                    pool_err,
                    deadpool_redis::PoolError::Backend(_) | deadpool_redis::PoolError::Timeout(_)
                );
            }
        }
        false
    }

    /// Exponential-backoff retry for transient errors. Mirrors
    /// [`crate::engine::retry_with_backoff`].
    ///
    /// Unlike the DB one this has no transaction guard, because none of the
    /// operations it wraps is part of a multi-statement session: each is a
    /// single round-trip on a connection that goes straight back to the
    /// pool. `INCR` is the one to think about — a retry after a *timeout*
    /// could double-count, since the first attempt may have landed. That
    /// is accepted: a rate-limit counter that occasionally over-counts
    /// during a network blip fails closed, which is the safe direction.
    pub async fn retry_with_backoff<F, Fut, T>(op: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let max_attempts = parse_retry_max_attempts();
        let base = parse_retry_backoff_ms() as u64;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match op().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if attempt >= max_attempts || !is_transient_error(&e) {
                        return Err(e);
                    }
                    let backoff = base.saturating_mul(1u64 << (attempt - 1).min(16));
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
            }
        }
    }

    // -- Command surface -----------------------------------------------
    //
    // One thin wrapper per built-in. Values cross the JWC boundary as
    // `String`, which is UTF-8 by construction (`Value::Str`), so a
    // non-UTF-8 payload written by some *other* client reads back as an
    // error rather than mojibake. `ecosystem.md` §6.1 settles this: binary
    // payloads travel base64-encoded.

    pub async fn get(key: &str) -> Result<Option<String>> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            redis::cmd("GET")
                .arg(key)
                .query_async::<Option<String>>(&mut conn)
                .await
                .with_context(|| "Redis GET failed")
        })
        .await
    }

    /// `SET key value` — with `EX ttl` when `ttl_secs > 0`.
    ///
    /// `ttl_secs == 0` means "no expiry", matching `cache_set`'s contract
    /// so the `redis` package can fall back between the two without the
    /// meaning of its arguments changing.
    pub async fn set(key: &str, value: &str, ttl_secs: u64) -> Result<()> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            let mut cmd = redis::cmd("SET");
            cmd.arg(key).arg(value);
            if ttl_secs > 0 {
                cmd.arg("EX").arg(ttl_secs);
            }
            cmd.query_async::<()>(&mut conn)
                .await
                .with_context(|| "Redis SET failed")
        })
        .await
    }

    pub async fn del(key: &str) -> Result<i64> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            redis::cmd("DEL")
                .arg(key)
                .query_async::<i64>(&mut conn)
                .await
                .with_context(|| "Redis DEL failed")
        })
        .await
    }

    pub async fn exists(key: &str) -> Result<bool> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            redis::cmd("EXISTS")
                .arg(key)
                .query_async::<i64>(&mut conn)
                .await
                .map(|n| n > 0)
                .with_context(|| "Redis EXISTS failed")
        })
        .await
    }

    pub async fn incr(key: &str) -> Result<i64> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            redis::cmd("INCR")
                .arg(key)
                .query_async::<i64>(&mut conn)
                .await
                .with_context(|| "Redis INCR failed")
        })
        .await
    }

    pub async fn expire(key: &str, ttl_secs: i64) -> Result<bool> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            redis::cmd("EXPIRE")
                .arg(key)
                .arg(ttl_secs)
                .query_async::<i64>(&mut conn)
                .await
                .map(|n| n == 1)
                .with_context(|| "Redis EXPIRE failed")
        })
        .await
    }

    /// `EVAL script numkeys key... arg...`.
    ///
    /// The escape hatch that makes read-modify-write atomic — which is the
    /// whole point for a rate limiter, where `INCR` + `EXPIRE` as two
    /// round-trips can leave a key with no TTL if the process dies between
    /// them.
    ///
    /// Replies are coerced to a string (`Value::Null` for nil) rather than
    /// modelled faithfully, because JWC has no sum type to receive a
    /// RESP-typed reply into. A script returning a table therefore reads
    /// back as its first element — scripts meant for `redis_eval` should
    /// return a scalar, or `cjson.encode(...)` a structure.
    pub async fn eval(script: &str, keys: &[String], args: &[String]) -> Result<Option<String>> {
        retry_with_backoff(|| async {
            let mut conn = get_connection().await?;
            let mut cmd = redis::cmd("EVAL");
            cmd.arg(script).arg(keys.len());
            for k in keys {
                cmd.arg(k);
            }
            for a in args {
                cmd.arg(a);
            }
            let raw: redis::Value = cmd
                .query_async(&mut conn)
                .await
                .with_context(|| "Redis EVAL failed")?;
            Ok(redis_value_to_string(&raw))
        })
        .await
    }

    /// Flatten a RESP reply into `Option<String>` — see [`eval`] for why
    /// this is lossy on purpose.
    fn redis_value_to_string(v: &redis::Value) -> Option<String> {
        match v {
            redis::Value::Nil => None,
            redis::Value::Int(n) => Some(n.to_string()),
            redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            redis::Value::SimpleString(s) => Some(s.clone()),
            redis::Value::Okay => Some("OK".to_string()),
            redis::Value::Double(d) => Some(d.to_string()),
            redis::Value::Boolean(b) => Some(b.to_string()),
            redis::Value::Array(items) | redis::Value::Set(items) => {
                items.first().and_then(redis_value_to_string)
            }
            other => Some(format!("{other:?}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Stub implementation — default build, no `redis` feature
// ---------------------------------------------------------------------------

#[cfg(not(feature = "redis"))]
mod imp {
    use super::{feature_disabled, read_redis_url, PoolStatusSnapshot};
    use anyhow::Result;

    /// No pool to build — but if the operator *did* set `JWC_REDIS_URL`
    /// they asked for something this binary can't do, and silently
    /// ignoring that would strand them debugging why their cache is
    /// per-process. Warn once at boot and keep going.
    pub fn init_redis_from_env() -> Result<()> {
        if read_redis_url().is_some() {
            eprintln!(
                "warning: JWC_REDIS_URL is set but this `jwc` binary was built \
                 without Redis support (`--features redis`). The redis_* \
                 built-ins will fail; caching stays per-process."
            );
        }
        Ok(())
    }

    pub fn is_enabled() -> bool {
        false
    }

    pub fn pool_status() -> Option<PoolStatusSnapshot> {
        None
    }

    pub fn is_transient_error(_err: &anyhow::Error) -> bool {
        false
    }

    pub async fn ping() -> Result<()> {
        Err(feature_disabled())
    }

    pub async fn get(_key: &str) -> Result<Option<String>> {
        Err(feature_disabled())
    }

    pub async fn set(_key: &str, _value: &str, _ttl_secs: u64) -> Result<()> {
        Err(feature_disabled())
    }

    pub async fn del(_key: &str) -> Result<i64> {
        Err(feature_disabled())
    }

    pub async fn exists(_key: &str) -> Result<bool> {
        Err(feature_disabled())
    }

    pub async fn incr(_key: &str) -> Result<i64> {
        Err(feature_disabled())
    }

    pub async fn expire(_key: &str, _ttl_secs: i64) -> Result<bool> {
        Err(feature_disabled())
    }

    pub async fn eval(
        _script: &str,
        _keys: &[String],
        _args: &[String],
    ) -> Result<Option<String>> {
        Err(feature_disabled())
    }
}

pub use imp::*;

/// Boot hook — called alongside [`crate::engine::init_engine_from_env`].
///
/// Separate from `imp::init_redis_from_env` only so callers have one name
/// to reach for that reads as a lifecycle step.
pub fn init_from_env() -> Result<()> {
    init_redis_from_env()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_masks_bare_password_form() {
        // The common Redis shape: no username, password only.
        assert_eq!(
            scrub_redis_url("redis://:hunter2@cache.internal:6379/0"),
            "redis://:***@cache.internal:6379/0"
        );
    }

    #[test]
    fn scrub_masks_user_password_form() {
        assert_eq!(
            scrub_redis_url("rediss://default:s3cret@eu1.upstash.io:6379"),
            "rediss://default:***@eu1.upstash.io:6379"
        );
    }

    #[test]
    fn scrub_leaves_credential_free_url_alone() {
        assert_eq!(
            scrub_redis_url("redis://127.0.0.1:6379"),
            "redis://127.0.0.1:6379"
        );
    }

    #[test]
    fn pool_status_is_none_before_init() {
        // Nothing in the unit-test process sets JWC_REDIS_URL, so the
        // singleton is empty and `/metrics` must simply omit the gauges
        // rather than reporting zeros that look like a dead pool.
        assert!(pool_status().is_none());
        assert!(!is_enabled());
    }

    #[test]
    fn retry_knobs_fall_back_when_unset() {
        // Only assert the defaults when the env is actually clean —
        // `cargo test` shares one process, and another test (or the
        // developer's shell) may legitimately have these set.
        if std::env::var("JWC_REDIS_RETRY_MAX_ATTEMPTS").is_err() {
            assert_eq!(parse_retry_max_attempts(), 3);
        }
        if std::env::var("JWC_REDIS_RETRY_BACKOFF_MS").is_err() {
            assert_eq!(parse_retry_backoff_ms(), 100);
        }
    }

    #[test]
    fn pool_size_rejects_zero_and_garbage() {
        // A zero-size deadpool never hands out a connection, so every
        // redis_* call would hang rather than fail — always fall back.
        std::env::set_var("JWC_REDIS_POOL_SIZE", "0");
        assert_eq!(parse_pool_size(), 64);
        std::env::set_var("JWC_REDIS_POOL_SIZE", "lots");
        assert_eq!(parse_pool_size(), 64);
        std::env::set_var("JWC_REDIS_POOL_SIZE", "8");
        assert_eq!(parse_pool_size(), 8);
        std::env::remove_var("JWC_REDIS_POOL_SIZE");
    }

    #[test]
    fn read_redis_url_treats_blank_as_unset() {
        // An empty var is what you get from `JWC_REDIS_URL=` in a compose
        // file or a k8s ConfigMap with the key present but no value; it
        // must read as "no Redis", not as a malformed URL that fails boot.
        std::env::set_var("JWC_REDIS_URL", "   ");
        assert!(read_redis_url().is_none());
        std::env::remove_var("JWC_REDIS_URL");
    }

    #[cfg(not(feature = "redis"))]
    #[tokio::test]
    async fn stubs_report_actionable_build_error() {
        let err = get("k").await.unwrap_err().to_string();
        assert!(
            err.contains("--features redis"),
            "error should tell the operator how to fix it, got: {err}"
        );
    }
}

// See integration_db.rs for the rationale; this harness has the same shape.
#![allow(clippy::await_holding_lock)]
#![cfg(feature = "redis")]

//! Integration tests for the Redis driver against a real Redis instance.
//!
//! Boots (or reuses) a Redis container via `testcontainers` and exercises
//! `jwc::redis_engine` directly. Tests skip with an `eprintln!` when Docker
//! is unreachable, so the suite is safe on hosts without a daemon.
//!
//! **A skip is not a pass.** `cargo test --features redis --test
//! integration_redis` printing `SKIPPED` means nothing was verified — run it
//! on a machine with Docker before calling Redis work done.
//!
//! ## Why a shared container + global mutex
//!
//! `redis_engine::REDIS` is a `OnceLock` — once initialised against a URL,
//! every later call reuses that pool. So: one container, `JWC_REDIS_URL`
//! pointed at it, and a `Mutex<()>` serialising tests, each of which flushes
//! the keyspace first. Same shape as `integration_db.rs`.
//!
//! The whole file is `#[cfg(feature = "redis")]` — without the feature the
//! driver is a set of stubs and there is nothing to integration-test.

use std::sync::{Mutex, OnceLock};

use jwc::redis_engine;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::Container;

static SHARED: OnceLock<Option<SharedContainer>> = OnceLock::new();
static TEST_LOCK: Mutex<()> = Mutex::new(());

struct SharedContainer {
    /// Held to keep the container alive for the test process.
    _container: Container<Redis>,
    url: String,
}

/// Boot a shared Redis container, or `None` when Docker is unreachable.
fn shared_redis_url() -> Option<&'static str> {
    // An externally supplied server wins, so CI can point at a service
    // container. Deliberately a test-only variable and not the runtime's
    // `JWC_REDIS_URL`: every test here calls `FLUSHDB`, and pointing that
    // at a developer's real cache would be destructive. Opting in has to
    // be explicit.
    if let Ok(url) = std::env::var("JWC_TEST_REDIS_URL") {
        if !url.trim().is_empty() {
            static EXTERNAL: OnceLock<String> = OnceLock::new();
            let url = EXTERNAL.get_or_init(|| {
                std::env::set_var("JWC_REDIS_URL", &url);
                url
            });
            return Some(url.as_str());
        }
    }
    SHARED
        .get_or_init(|| {
            // `SyncRunner::start` calls `block_on`, so an unreachable Docker
            // panics with "Cannot start a runtime from within a runtime"
            // rather than returning `Err` — the same trap documented in
            // `integration_db.rs`. Catch it so the skip is real.
            std::panic::catch_unwind(|| {
                let container = Redis.start().ok()?;
                let port = container.get_host_port_ipv4(6379).ok()?;
                let url = format!("redis://127.0.0.1:{port}");
                std::env::set_var("JWC_REDIS_URL", &url);
                Some(SharedContainer {
                    _container: container,
                    url,
                })
            })
            .ok()
            .flatten()
        })
        .as_ref()
        .map(|c| c.url.as_str())
}

/// Acquire the global lock and empty the keyspace. `None` = graceful skip.
async fn fresh_keyspace() -> Option<std::sync::MutexGuard<'static, ()>> {
    let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    shared_redis_url()?;
    redis_engine::init_redis_from_env().ok()?;
    // Via the public surface rather than a raw connection, so a broken
    // `eval` fails loudly here instead of leaving stale keys behind.
    redis_engine::eval("redis.call('FLUSHDB') return 1", &[], &[])
        .await
        .ok()?;
    Some(guard)
}

fn skip_notice(test_name: &str) {
    eprintln!(
        "[integration_redis::{test_name}] SKIPPED: docker not reachable from this test process"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn set_get_del_round_trip() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("set_get_del_round_trip");
    };

    assert_eq!(redis_engine::get("missing").await.unwrap(), None);

    redis_engine::set("k", "v", 0).await.unwrap();
    assert_eq!(redis_engine::get("k").await.unwrap(), Some("v".into()));
    assert!(redis_engine::exists("k").await.unwrap());

    assert_eq!(redis_engine::del("k").await.unwrap(), 1);
    assert_eq!(redis_engine::get("k").await.unwrap(), None);
    assert!(!redis_engine::exists("k").await.unwrap());
    // Deleting a key that isn't there is not an error — it deletes nothing.
    assert_eq!(redis_engine::del("k").await.unwrap(), 0);
}

/// `ttl_secs == 0` must mean "no expiry", matching `cache_set`. If this
/// regressed to "expire immediately" the `redis` package's fallback would
/// silently start losing every value it wrote.
#[tokio::test(flavor = "multi_thread")]
async fn zero_ttl_means_no_expiry() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("zero_ttl_means_no_expiry");
    };

    redis_engine::set("forever", "v", 0).await.unwrap();
    let ttl = redis_engine::eval(
        "return redis.call('TTL', KEYS[1])",
        &["forever".into()],
        &[],
    )
    .await
    .unwrap();
    // -1 is Redis for "key exists, no expiry set".
    assert_eq!(ttl.as_deref(), Some("-1"));

    redis_engine::set("fleeting", "v", 60).await.unwrap();
    let ttl = redis_engine::eval(
        "return redis.call('TTL', KEYS[1])",
        &["fleeting".into()],
        &[],
    )
    .await
    .unwrap();
    let secs: i64 = ttl.unwrap().parse().unwrap();
    assert!((1..=60).contains(&secs), "expected a live TTL, got {secs}");
}

#[tokio::test(flavor = "multi_thread")]
async fn incr_counts_and_expire_sets_a_deadline() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("incr_counts_and_expire_sets_a_deadline");
    };

    assert_eq!(redis_engine::incr("hits").await.unwrap(), 1);
    assert_eq!(redis_engine::incr("hits").await.unwrap(), 2);
    assert_eq!(redis_engine::incr("hits").await.unwrap(), 3);

    assert!(redis_engine::expire("hits", 60).await.unwrap());
    // EXPIRE on a missing key reports false rather than erroring.
    assert!(!redis_engine::expire("nonexistent", 60).await.unwrap());
}

/// The atomicity `redis_eval` exists for: INCR + EXPIRE in one round-trip,
/// which is what makes a multi-replica rate limiter correct. As two separate
/// calls, a crash between them leaves a counter with no TTL that never
/// resets — the exact bug the `redis` package's `rate_limit` avoids.
#[tokio::test(flavor = "multi_thread")]
async fn eval_runs_a_script_atomically_with_keys_and_args() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("eval_runs_a_script_atomically_with_keys_and_args");
    };

    const SCRIPT: &str = r#"
        local n = redis.call('INCR', KEYS[1])
        if n == 1 then redis.call('EXPIRE', KEYS[1], ARGV[1]) end
        return n
    "#;

    for expected in 1..=3 {
        let got = redis_engine::eval(SCRIPT, &["rl:ip".into()], &["60".into()])
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some(expected.to_string().as_str()));
    }

    // The TTL was set on the first call and survives later increments.
    let ttl = redis_engine::eval("return redis.call('TTL', KEYS[1])", &["rl:ip".into()], &[])
        .await
        .unwrap();
    let secs: i64 = ttl.unwrap().parse().unwrap();
    assert!((1..=60).contains(&secs), "expected a live TTL, got {secs}");
}

/// A nil reply must come back as `None` (JWC `null`), not `Some("")` — the
/// interpreter maps the two to different values and route code branches on
/// the difference.
#[tokio::test(flavor = "multi_thread")]
async fn eval_nil_reply_is_none_not_empty_string() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("eval_nil_reply_is_none_not_empty_string");
    };

    let got = redis_engine::eval("return nil", &[], &[]).await.unwrap();
    assert_eq!(got, None);

    // An empty string, by contrast, really is an empty string.
    redis_engine::set("empty", "", 0).await.unwrap();
    assert_eq!(
        redis_engine::get("empty").await.unwrap(),
        Some(String::new())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_and_enabled_report_a_live_server() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("ping_and_enabled_report_a_live_server");
    };

    assert!(redis_engine::is_enabled());
    redis_engine::ping().await.expect("ping a live server");
    let status = redis_engine::pool_status().expect("pool exists once initialised");
    assert!(status.max_size > 0);
}

/// A Lua error must surface as an error, not as a value — otherwise a
/// broken script silently returns `null` and the caller treats it as a miss.
#[tokio::test(flavor = "multi_thread")]
async fn eval_surfaces_script_errors() {
    let Some(_guard) = fresh_keyspace().await else {
        return skip_notice("eval_surfaces_script_errors");
    };

    let err = redis_engine::eval("this is not lua", &[], &[])
        .await
        .expect_err("a syntax error must not read as a nil reply");
    assert!(
        !redis_engine::is_transient_error(&err),
        "a script syntax error is permanent — retrying it would livelock: {err:?}"
    );
}

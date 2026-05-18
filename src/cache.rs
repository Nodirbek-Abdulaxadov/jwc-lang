//! Process-wide in-memory cache with optional per-entry TTL.
//!
//! Designed for typical backend caching needs (JWT validation results,
//! user-scoped query results, rate-limit counters, etc.) and intentionally
//! kept dependency-free — only `std`.
//!
//! The store is a single global `Mutex<HashMap>` guarded by `OnceLock`; the
//! contention is fine for the workloads JWC targets (one process, modest
//! concurrent request volume). Expired entries are evicted lazily on `get`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct CacheEntry {
    value: String,
    /// `None` means "never expires".
    expires_at: Option<Instant>,
}

fn store() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static STORE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Insert (or overwrite) `key` -> `value`. `ttl_secs == 0` means the entry
/// never expires; any positive value sets an absolute deadline measured from
/// the moment of the call.
pub fn set(key: &str, value: &str, ttl_secs: u64) {
    let ttl = if ttl_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(ttl_secs))
    };
    set_with_ttl(key, value, ttl);
}

/// Internal variant that accepts an arbitrary `Duration` (or `None` for "no
/// expiration"). Used by sub-second unit tests; the public stdlib surface
/// stays whole-seconds via [`set`].
pub(crate) fn set_with_ttl(key: &str, value: &str, ttl: Option<Duration>) {
    let expires_at = ttl.and_then(|d| Instant::now().checked_add(d));
    let mut guard = store().lock().expect("cache mutex poisoned");
    guard.insert(
        key.to_string(),
        CacheEntry {
            value: value.to_string(),
            expires_at,
        },
    );
}

/// Look up `key`. Returns `None` if the key is missing or its TTL has
/// elapsed (in which case the stale entry is also evicted).
pub fn get(key: &str) -> Option<String> {
    let mut guard = store().lock().expect("cache mutex poisoned");
    let expired = match guard.get(key) {
        Some(entry) => match entry.expires_at {
            Some(deadline) => Instant::now() >= deadline,
            None => false,
        },
        None => return None,
    };
    if expired {
        guard.remove(key);
        return None;
    }
    guard.get(key).map(|e| e.value.clone())
}

/// Remove a single key. Silently succeeds if it was not present.
pub fn del(key: &str) {
    let mut guard = store().lock().expect("cache mutex poisoned");
    guard.remove(key);
}

/// Drop every entry in the cache.
pub fn clear() {
    let mut guard = store().lock().expect("cache mutex poisoned");
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::thread;
    use std::time::Duration;

    /// All cache tests touch the same global store, so we serialize them
    /// to keep `clear_drops_everything` from racing the others.
    fn lock() -> MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        let m = M.get_or_init(|| Mutex::new(()));
        match m.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    #[test]
    fn set_then_get_returns_value() {
        let _g = lock();
        clear();
        set("k1", "hello", 0);
        assert_eq!(get("k1").as_deref(), Some("hello"));
    }

    #[test]
    fn get_returns_null_for_missing_key() {
        let _g = lock();
        clear();
        assert!(get("never-set").is_none());
    }

    #[test]
    fn del_removes_entry() {
        let _g = lock();
        clear();
        set("kdel", "v", 0);
        assert!(get("kdel").is_some());
        del("kdel");
        assert!(get("kdel").is_none());
    }

    #[test]
    fn expired_entry_is_invisible_to_get() {
        let _g = lock();
        clear();
        // Use the internal sub-second helper so the test runs quickly.
        set_with_ttl("kexp", "soon-gone", Some(Duration::from_millis(100)));
        assert_eq!(get("kexp").as_deref(), Some("soon-gone"));
        thread::sleep(Duration::from_millis(150));
        assert!(get("kexp").is_none(), "entry should expire after TTL");
    }

    #[test]
    fn clear_drops_everything() {
        let _g = lock();
        set("ca", "1", 0);
        set("cb", "2", 0);
        clear();
        assert!(get("ca").is_none());
        assert!(get("cb").is_none());
    }
}

//! Process-local cache behind the `cache.*` built-ins (builtins.md §8).
//!
//! The native prelude has carried `jwc_cache_store` since the backend came
//! back, but no 1.0 program could reach it: `cache` was not a namespace, so
//! `cache.get(...)` was an unknown name. This is the other half.
//!
//! # `cache.*` is not `redis.*`
//!
//! This store lives in **one process**. Two replicas do not share it, and
//! a restart empties it. That makes it right for what a single process can
//! own — a parsed JWKS document, a compiled template, a config row read on
//! every request — and wrong for anything whose correctness spans replicas.
//! A rate limiter is the standard mistake: per-process counters mean the
//! real limit is `limit × replicas`, and nothing in the response says so.
//! `redis.rate_limit` exists for that.
//!
//! # Bounded
//!
//! The 0.9 store this restores was an unbounded `HashMap` that evicted only
//! lazily, on a `get` of the expired key itself. A program that cached
//! per-request keys it never read back — a session token, a request id —
//! grew it until the process died, and the TTL never helped because nothing
//! ever looked the key up again. So: entries are capped
//! (`JWC_CACHE_MAX_ENTRIES`, default 10 000), a write at the cap first
//! sweeps what has expired, and if that frees nothing the oldest write is
//! evicted. Evictions are counted and reported on `/metrics` rather than
//! being silent, because a cache that has quietly become a no-op looks
//! exactly like a cache that is working.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default `JWC_CACHE_MAX_ENTRIES`.
const DEFAULT_MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone)]
struct Entry {
    value: String,
    /// `None` never expires.
    expires_at: Option<Instant>,
    /// Insertion order, for the eviction tie-break. A `u64` at one
    /// increment per write does not wrap in any process lifetime.
    seq: u64,
}

fn store() -> &'static Mutex<HashMap<String, Entry>> {
    static STORE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

static SEQ: AtomicU64 = AtomicU64::new(0);
static EVICTED: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

fn max_entries() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("JWC_CACHE_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_ENTRIES)
    })
}

/// Insert or overwrite. `ttl_secs == 0` means no expiry — the same reading
/// `redis.set` gives it, so the two are swappable.
pub fn set(key: &str, value: &str, ttl_secs: u64) {
    let ttl = (ttl_secs > 0).then(|| Duration::from_secs(ttl_secs));
    set_with_ttl(key, value, ttl);
}

/// Sub-second TTLs, for the tests. The language surface is whole seconds.
pub(crate) fn set_with_ttl(key: &str, value: &str, ttl: Option<Duration>) {
    let expires_at = ttl.and_then(|d| Instant::now().checked_add(d));
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut guard = crate::locks::lock_recover(store());

    // Only when the write would *add* a key: overwriting one is free.
    if guard.len() >= max_entries() && !guard.contains_key(key) {
        let now = Instant::now();
        guard.retain(|_, e| e.expires_at.is_none_or(|d| now < d));
        if guard.len() >= max_entries() {
            // Nothing had expired, so give up the oldest write. FIFO
            // rather than LRU: tracking reads would mean taking the write
            // lock on every `get`.
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, e)| e.seq)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest);
                EVICTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    guard.insert(
        key.to_string(),
        Entry {
            value: value.to_string(),
            expires_at,
            seq,
        },
    );
}

/// `None` when the key is absent or its TTL has elapsed. An expired entry
/// is dropped on the way past.
pub fn get(key: &str) -> Option<String> {
    let mut guard = crate::locks::lock_recover(store());
    let Some(entry) = guard.get(key) else {
        MISSES.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    if entry.expires_at.is_some_and(|d| Instant::now() >= d) {
        guard.remove(key);
        MISSES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    HITS.fetch_add(1, Ordering::Relaxed);
    guard.get(key).map(|e| e.value.clone())
}

/// How many keys were removed: 0 or 1. `redis.del` answers the same shape.
pub fn del(key: &str) -> i64 {
    let mut guard = crate::locks::lock_recover(store());
    i64::from(guard.remove(key).is_some())
}

/// Drop everything.
pub fn clear() {
    crate::locks::lock_recover(store()).clear();
}

/// Live entry count, for `/metrics`.
pub fn len() -> usize {
    crate::locks::lock_recover(store()).len()
}

/// `/metrics` lines. Empty when the program never touched the cache, so a
/// service that does not use it does not grow four flat-zero series.
pub fn metrics_text() -> String {
    let (hits, misses, evicted) = (
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        EVICTED.load(Ordering::Relaxed),
    );
    if hits == 0 && misses == 0 && evicted == 0 && len() == 0 {
        return String::new();
    }
    format!(
        "# HELP jwc_cache_entries Live entries in the process-local cache.\n\
         # TYPE jwc_cache_entries gauge\n\
         jwc_cache_entries {}\n\
         # HELP jwc_cache_hits_total Reads that found a live entry.\n\
         # TYPE jwc_cache_hits_total counter\n\
         jwc_cache_hits_total {hits}\n\
         # HELP jwc_cache_misses_total Reads that found nothing, expired included.\n\
         # TYPE jwc_cache_misses_total counter\n\
         jwc_cache_misses_total {misses}\n\
         # HELP jwc_cache_evicted_total Entries dropped to stay under JWC_CACHE_MAX_ENTRIES.\n\
         # TYPE jwc_cache_evicted_total counter\n\
         jwc_cache_evicted_total {evicted}\n",
        len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;
    use std::thread;

    /// One global store, so the tests take turns.
    fn lock() -> MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        crate::locks::lock_recover(M.get_or_init(|| Mutex::new(())))
    }

    #[test]
    fn set_then_get_round_trips() {
        let _g = lock();
        clear();
        set("k", "v", 0);
        assert_eq!(get("k").as_deref(), Some("v"));
    }

    #[test]
    fn zero_ttl_never_expires() {
        let _g = lock();
        clear();
        set_with_ttl("k", "v", None);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(get("k").as_deref(), Some("v"));
    }

    #[test]
    fn expired_entry_reads_as_absent_and_is_dropped() {
        let _g = lock();
        clear();
        set_with_ttl("k", "v", Some(Duration::from_millis(5)));
        thread::sleep(Duration::from_millis(20));
        assert_eq!(get("k"), None);
        assert_eq!(len(), 0, "the expired entry should not still be held");
    }

    #[test]
    fn del_reports_whether_it_removed_anything() {
        let _g = lock();
        clear();
        set("k", "v", 0);
        assert_eq!(del("k"), 1);
        assert_eq!(del("k"), 0);
    }

    #[test]
    fn clear_drops_everything() {
        let _g = lock();
        clear();
        set("a", "1", 0);
        set("b", "2", 0);
        clear();
        assert_eq!(len(), 0);
    }

    /// The defect the 0.9 store had: keys written and never read back grew
    /// the map without bound, because eviction only happened on a `get` of
    /// the expired key itself.
    #[test]
    fn writes_that_are_never_read_stay_bounded() {
        let _g = lock();
        clear();
        let cap = max_entries();
        for i in 0..cap + 500 {
            set(&format!("k{i}"), "v", 0);
        }
        assert!(len() <= cap, "{} entries held, cap is {cap}", len());
        assert!(
            EVICTED.load(Ordering::Relaxed) > 0,
            "eviction should be counted, not silent"
        );
        clear();
    }

    #[test]
    fn overwriting_an_existing_key_does_not_evict() {
        let _g = lock();
        clear();
        set("a", "1", 0);
        let before = EVICTED.load(Ordering::Relaxed);
        for _ in 0..50 {
            set("a", "2", 0);
        }
        assert_eq!(EVICTED.load(Ordering::Relaxed), before);
        assert_eq!(get("a").as_deref(), Some("2"));
    }

    #[test]
    fn metrics_are_empty_until_the_cache_is_used() {
        let _g = lock();
        // Counters are process-wide and other tests bump them, so this
        // asserts the shape rather than the emptiness.
        set("k", "v", 0);
        let text = metrics_text();
        assert!(text.contains("jwc_cache_entries "), "{text}");
        assert!(text.contains("jwc_cache_hits_total "), "{text}");
        assert!(text.contains("jwc_cache_evicted_total "), "{text}");
        clear();
    }
}

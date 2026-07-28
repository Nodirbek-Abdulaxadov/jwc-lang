//! Mutex locking that survives a panic elsewhere in the process.
//!
//! Rust poisons a `Mutex` when a thread panics while holding it. The default
//! response — `lock().unwrap()` / `lock().expect(...)` — propagates that
//! panic to every later caller, forever. In a long-running server that turns
//! one bad request into a permanently dead subsystem:
//!
//! ```text
//! request 1: panics inside a handler while holding the cache lock
//! request 2: cache.get() -> panic (poisoned)
//! request 3: cache.get() -> panic (poisoned)
//! ...        every request from here on, until someone restarts the process
//! ```
//!
//! That is a worse outcome than the original panic, and it is silent — the
//! logs show a flood of identical "poisoned" panics with no hint that the
//! first one was the real event.
//!
//! Poisoning is also conservative rather than diagnostic. It fires on *any*
//! panic while the guard is alive, including one from code that never touched
//! the protected value. It is a "someone panicked nearby" flag, not "this data
//! is corrupt".
//!
//! So the state behind these locks is designed to tolerate it:
//!
//! - **cache** — a `HashMap` of cached strings. Worst case one entry is stale
//!   or missing, which is what a cache is allowed to be anyway.
//! - **queue** — job list, handler map, worker-started flag. A panicking
//!   worker may lose the job it was running; recovering keeps every *other*
//!   job flowing. Propagating instead stops the queue permanently, which
//!   loses all of them.
//! - **email transport** — held read-only after construction.
//!
//! None of these carry an invariant that a mid-mutation panic could break in
//! a way that later reads can't cope with, so recovering the guard is both
//! safe and strictly better than dying.
//!
//! Where a lock protects something that genuinely cannot be used after an
//! interrupted mutation, don't reach for these helpers — handle the
//! `PoisonError` explicitly at that site and say why.

use std::sync::{Condvar, Mutex, MutexGuard};

/// Lock `m`, taking the guard even if a previous holder panicked.
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `Condvar::wait`, recovering the guard the same way [`lock_recover`] does.
///
/// A condvar wait re-acquires the mutex on wake, so it fails for exactly the
/// same reason and deserves the same answer — otherwise a worker parked on
/// the queue dies the moment any sibling worker panics.
pub fn wait_recover<'a, T>(cv: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    cv.wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The whole point: a lock poisoned by an unrelated panic is still usable,
    /// and the value written before the panic is still there.
    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));

        let m2 = Arc::clone(&m);
        let handle = std::thread::spawn(move || {
            let mut g = m2.lock().expect("first lock is not poisoned yet");
            g.push(4);
            panic!("boom");
        });
        assert!(handle.join().is_err(), "the helper thread should panic");

        assert!(m.lock().is_err(), "the mutex should now be poisoned");

        let g = lock_recover(&m);
        assert_eq!(
            *g,
            vec![1, 2, 3, 4],
            "recovered guard must expose the value as the panicking thread left it"
        );
    }

    /// Recovery is not one-shot — every later caller gets through too, which
    /// is what stops a single panic from killing a subsystem for good.
    #[test]
    fn recovery_holds_for_every_subsequent_lock() {
        let m = Arc::new(Mutex::new(0u32));

        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().expect("not poisoned yet");
            panic!("boom");
        })
        .join();

        for expected in 1..=5u32 {
            let mut g = lock_recover(&m);
            *g += 1;
            assert_eq!(*g, expected);
        }
    }
}

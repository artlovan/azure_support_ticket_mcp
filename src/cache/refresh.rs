//! Single-flight refresh coordination.
//!
//! Ensures that for a given `cache_key` only one refresh task is in flight
//! at a time across the process. Callers `acquire()` a guard; concurrent
//! requests for the same key get `None` and should serve stale data.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Clone, Default)]
pub struct SingleFlight {
    inner: Arc<Mutex<HashSet<String>>>,
}

pub struct FlightGuard {
    key: String,
    inner: Arc<Mutex<HashSet<String>>>,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.inner.lock().remove(&self.key);
    }
}

impl SingleFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `Some(guard)` if this caller wins the right to refresh.
    /// Returns `None` if another refresh is already in flight for `key`.
    pub fn acquire(&self, key: &str) -> Option<FlightGuard> {
        let mut g = self.inner.lock();
        if g.contains(key) {
            None
        } else {
            g.insert(key.to_string());
            Some(FlightGuard {
                key: key.to_string(),
                inner: self.inner.clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_wins_second_blocked_until_drop() {
        let sf = SingleFlight::new();
        let g = sf.acquire("k").unwrap();
        assert!(sf.acquire("k").is_none());
        drop(g);
        assert!(sf.acquire("k").is_some());
    }

    #[test]
    fn different_keys_independent() {
        let sf = SingleFlight::new();
        let _a = sf.acquire("a").unwrap();
        let _b = sf.acquire("b").unwrap();
    }
}

use std::time::{SystemTime, UNIX_EPOCH};

use super::CacheStatus;

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Classify cache freshness against a TTL window.
pub fn classify(updated_at: Option<i64>, ttl_secs: i64, now: i64) -> CacheStatus {
    match updated_at {
        None => CacheStatus::Missing,
        Some(ts) if now.saturating_sub(ts) <= ttl_secs => CacheStatus::Fresh,
        Some(_) => CacheStatus::Stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_when_within_ttl() {
        assert_eq!(classify(Some(900), 100, 950), CacheStatus::Fresh);
    }
    #[test]
    fn stale_when_outside_ttl() {
        assert_eq!(classify(Some(800), 100, 1000), CacheStatus::Stale);
    }
    #[test]
    fn missing_when_none() {
        assert_eq!(classify(None, 100, 1000), CacheStatus::Missing);
    }
}

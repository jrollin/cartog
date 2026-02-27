use std::collections::HashMap;
use crate::utils::helpers::get_logger;
use crate::app_errors::AppErrorExt;

/// A sliding-window rate limiter tracking request counts per key.
pub struct RateLimiter {
    /// Maximum requests allowed in the window.
    max_requests: u64,
    /// Window size in seconds.
    window_secs: u64,
    /// Request counts per client key.
    counts: HashMap<String, u64>,
}

impl RateLimiter {
    /// Create a new rate limiter with the given limits.
    pub fn new(max_requests: u64, window_secs: u64) -> Self {
        let logger = get_logger("middleware.rate_limit");
        logger.info(&format!("RateLimiter: max={}, window={}s", max_requests, window_secs));
        Self {
            max_requests,
            window_secs,
            counts: HashMap::new(),
        }
    }

    /// Check if a request from the given key should be allowed.
    pub fn check(&mut self, key: &str) -> Result<(), AppErrorExt> {
        let logger = get_logger("middleware.rate_limit");
        let count = self.counts.entry(key.to_string()).or_insert(0);
        *count += 1;
        if *count > self.max_requests {
            logger.warn(&format!("Rate limit exceeded for {}", key));
            return Err(AppErrorExt::RateLimit {
                retry_after: self.window_secs,
            });
        }
        logger.info(&format!("Rate limit OK for {}: {}/{}", key, count, self.max_requests));
        Ok(())
    }

    /// Reset the counter for a specific key.
    pub fn reset(&mut self, key: &str) {
        let logger = get_logger("middleware.rate_limit");
        self.counts.remove(key);
        logger.info(&format!("Rate limit reset for {}", key));
    }

    /// Reset all counters.
    pub fn reset_all(&mut self) {
        let logger = get_logger("middleware.rate_limit");
        let count = self.counts.len();
        self.counts.clear();
        logger.info(&format!("All rate limits reset: {} keys", count));
    }
}
